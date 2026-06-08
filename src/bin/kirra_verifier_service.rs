// src/bin/kirra_verifier_service.rs
// Kirra Verifier Service — distributed legitimacy fabric entry point.

use axum::{
    extract::{Path, Query, Request, State},
    extract::rejection::JsonRejection,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use tower_http::cors::{CorsLayer, Any};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use kirra_runtime_sdk::verifier::{
    validate_client_identity_headers, AppState, BackupExport, FlapStatus, FleetNodePosture,
    FleetPosture, HealthResponse, NodeTrustState, PostureStreamEvent, RegisteredNode, VerifierOperationMode,
};
use kirra_runtime_sdk::verifier_store::VerifierStore;
use kirra_runtime_sdk::posture_cache::{now_ms, ServiceState, POSTURE_CACHE_TTL_MS};
use kirra_runtime_sdk::posture_engine_v2::{resolve_posture_with_reason, LockoutReason};
use kirra_runtime_sdk::security::admin_token_ok;
use kirra_runtime_sdk::action_filter::{evaluate_action_claim, ActionClaim};
use kirra_runtime_sdk::protocol_adapter::{
    evaluate_unified_industrial_request, UnifiedIndustrialRequest,
};
use kirra_runtime_sdk::adapters::ethernet_ip::{EtherNetIpAdapter, EtherNetIpMessage};
use kirra_runtime_sdk::adapters::canopen::{CanOpenAdapter, CanOpenMessage};
use kirra_runtime_sdk::adapters::dnp3::{Dnp3Adapter, Dnp3Message};
use kirra_runtime_sdk::federation::{
    evaluate_federated_report,
    verify_federated_report_signature,
    FederatedTrustReport,
    RegisterFederationControllerRequest,
    ReportEvaluation,
};
use kirra_runtime_sdk::standby_monitor::{
    instance_id as ha_instance_id, spawn_heartbeat_writer, spawn_promotion_monitor,
    HEARTBEAT_KEY, PROMOTION_TIMEOUT_MS,
};
use kirra_runtime_sdk::gateway::kinematics_contract::ProposedVehicleCommand;
use kirra_runtime_sdk::gateway::policy_layer::{
    enforce_actuator_safety_envelope, enforce_posture_routing, EnforcementOutcome,
};
use kirra_runtime_sdk::recovery_hysteresis::{evaluate_recovery_report, HysteresisDecision};
use kirra_runtime_sdk::fabric::asset::{AssetPosture, AssetType, FabricAsset, KinematicProfileType};
use kirra_runtime_sdk::fabric::router::FabricRouter;
use kirra_runtime_sdk::fabric::telemetry::FabricTelemetry;
use kirra_runtime_sdk::fabric::causal_log::FabricCausalLog;

// --- Auth middleware ---------------------------------------------------------

async fn require_admin_token(request: Request, next: Next) -> Result<Response, StatusCode> {
    let expected = std::env::var("KIRRA_ADMIN_TOKEN")
        .unwrap_or_default();

    // Fail-closed: absent or empty admin token → 503 (CRITICAL INVARIANT #1/#6).
    // Kept distinct from the 401 below so an unconfigured server is never
    // mistaken for a bad credential.
    if expected.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Single constant-time authorization decision (SG-015). `expected` is
    // non-empty here, so this reduces to a constant_time_compare of the two
    // tokens — behavior identical to the prior inline check, never `==`.
    if !admin_token_ok(Some(provided), Some(&expected)) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}

// --- SG-008: process fail-closed startup sentinel ---------------------------
//
// Verifies: SG-008 (ASIL D) — Process Fail-Closed on Startup. The service must
// refuse to bind its listener unless the safety-critical startup invariants
// hold. The checks are factored into a pure predicate so they are
// deterministically testable without `process::exit` (see sg_008_cert_tests):
// `main` builds a `StartupContext` from the real boot facts, and aborts BEFORE
// `TcpListener::bind` on any `Err` — so "the listener never binds before
// invariants pass" holds by construction (bind is strictly after the check).

/// The boot facts the startup sentinel evaluates. Built once in `main` from the
/// real environment/store/wiring; consumed by `check_startup_invariants`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StartupContext {
    /// `KIRRA_ADMIN_TOKEN` is present and non-empty (CRITICAL INVARIANT #6).
    pub admin_token_present: bool,
    /// The SQLite store reports `journal_mode = wal` (CRITICAL INVARIANT #12
    /// ordering depends on the WAL-mode durable seam).
    pub sqlite_wal: bool,
    /// True on the Active path. PassiveStandby is read-only and intentionally
    /// runs neither the watchdog nor the posture engine, so those two
    /// invariants are evaluated ONLY when this is true.
    pub mode_active: bool,
    /// The telemetry watchdog task was spawned (Active path; SG-003 / SG9).
    pub watchdog_spawned: bool,
    /// The serialized posture-engine worker is running (`posture_engine_tx` set).
    pub posture_engine_running: bool,
}

/// The first violated startup invariant, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupInvariant {
    AdminTokenMissing,
    SqliteNotWal,
    WatchdogNotSpawned,
    PostureEngineDown,
}

impl std::fmt::Display for StartupInvariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::AdminTokenMissing => "KIRRA_ADMIN_TOKEN absent or empty",
            Self::SqliteNotWal => "SQLite store is not in WAL journal mode",
            Self::WatchdogNotSpawned => "telemetry watchdog not spawned (Active path)",
            Self::PostureEngineDown => "posture-engine worker not running (Active path)",
        };
        write!(f, "{s}")
    }
}

/// SG-008 (ASIL D) — pure startup-invariant predicate. Returns the first
/// violated invariant, or `Ok(())` when all hold. Fail-closed and order-stable.
/// The watchdog / posture-engine invariants apply only to the Active path
/// (`mode_active`); PassiveStandby is read-only and runs neither, so requiring
/// them there would wrongly abort a valid standby.
//
// Verifies: SG-008
pub(crate) fn check_startup_invariants(ctx: &StartupContext) -> Result<(), StartupInvariant> {
    if !ctx.admin_token_present {
        return Err(StartupInvariant::AdminTokenMissing);
    }
    if !ctx.sqlite_wal {
        return Err(StartupInvariant::SqliteNotWal);
    }
    if ctx.mode_active {
        if !ctx.watchdog_spawned {
            return Err(StartupInvariant::WatchdogNotSpawned);
        }
        if !ctx.posture_engine_running {
            return Err(StartupInvariant::PostureEngineDown);
        }
    }
    Ok(())
}

async fn require_client_identity(
    State(svc): State<Arc<ServiceState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cfg = &svc.app.transport_identity;
    if !validate_client_identity_headers(
        cfg.trusted_ingress_mode,
        &cfg.client_id_header,
        request.headers(),
    ) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

// --- Real-time posture stream -----------------------------------------------

/// Sends an event-driven posture recalc trigger to the worker if the
/// `posture_engine_tx` is initialized (Active path). On PassiveStandby the
/// OnceLock is unset and this is a no-op — correct, since a standby does
/// not maintain a posture cache. A `try_send` failure (channel full or
/// worker gone) is logged; the periodic-refresh loop will fail-close the
/// cache and gate on its own if the worker has truly died.
fn enqueue_recalc(svc: &ServiceState, trigger: kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger) {
    if let Some(tx) = svc.posture_engine_tx.get() {
        if let Err(e) = tx.try_send(trigger) {
            tracing::warn!(error = %e,
                "posture recalc trigger: try_send failed (channel full or worker gone)");
        }
    }
}

/// Fail-closed posture read for action/actuator gating sites.
///
/// Delegates to `resolve_posture_with_reason` so the cache-staleness check
/// (age >= POSTURE_CACHE_TTL_MS), empty-cache check, and poisoned-lock check
/// all collapse into the same `(FleetPosture::LockedOut, Some(LockoutReason))`
/// answer — never serving a stale entry as if current. The returned
/// `LockoutReason` is threaded into the denial-audit payload so operators
/// can distinguish a DAG-derived LockedOut from a posture-cache-derived one.
fn gate_posture(svc: &ServiceState) -> (FleetPosture, Option<LockoutReason>) {
    resolve_posture_with_reason(&svc.posture_cache, POSTURE_CACHE_TTL_MS)
}

fn emit_posture_event(state: &AppState, event_type: &str, node_id: Option<String>) {
    let posture = node_id.as_ref().map(|id| state.calculate_posture(id));
    let _ = state.posture_tx.send(PostureStreamEvent {
        event_type: event_type.to_string(),
        node_id,
        emitted_at_ms: now_ms(),
        posture,
    });
}

async fn system_posture_stream(
    State(svc): State<Arc<ServiceState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = svc.app.posture_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| {
        match msg {
            Ok(event) => serde_json::to_string(&event).ok().map(|data| {
                Ok(Event::default().data(data))
            }),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "posture stream subscriber lagged; frames dropped");
                None
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// --- Request / response types -----------------------------------------------

#[derive(Deserialize)]
struct RegisterNodeRequest {
    node_id: String,
    #[serde(default)]
    ak_public_pem: Option<String>,
    #[serde(default)]
    expected_pcr16_digest_hex: Option<String>,
}

#[derive(Deserialize)]
struct RegisterDependenciesRequest {
    node_id: String,
    depends_on: Vec<String>,
}

#[derive(Deserialize)]
struct VerifyAttestationRequest {
    node_id: String,
    nonce: u64,
    proof_hex: String,
}

#[derive(Serialize)]
struct AttestationStatusResponse {
    node_id: String,
    status: String,
    registered_at_ms: u64,
}

#[derive(Deserialize)]
struct SensorFaultReportRequest {
    source_node_id: String,
    confidence_score: f64,
    hardware_fault_detected: bool,
}

#[derive(Deserialize)]
struct RegisterAvAssetRequest {
    node_id: String,
    subsystem_type: String,
    hardware_id: String,
    #[serde(default)]
    confidence_floor: Option<f64>,
}

// --- Handlers ----------------------------------------------------------------

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok".to_string() })
}

async fn ready(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc.app.store.lock() {
        Ok(store) => match store.health_check() {
            Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ready".to_string() }))
                .into_response(),
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE,
                       Json(HealthResponse { status: "db_unavailable".to_string() }))
                .into_response(),
        },
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE,
                   Json(HealthResponse { status: "store_lock_poisoned".to_string() }))
            .into_response(),
    }
}

async fn export_backup(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    match svc.app.store.lock() {
        Ok(store) => {
            let nodes = match store.load_nodes() {
                Ok(n) => n,
                Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                                  Json(json!({ "error": "failed to load nodes" }))).into_response(),
            };
            let dependencies = match store.load_dependencies() {
                Ok(d) => d,
                Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                                  Json(json!({ "error": "failed to load dependencies" }))).into_response(),
            };
            let posture_events = match store.load_all_posture_events() {
                Ok(e) => e,
                Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                                  Json(json!({ "error": "failed to load posture events" }))).into_response(),
            };
            Json(BackupExport {
                exported_at_ms: now_ms(),
                nodes,
                dependencies,
                posture_events,
            }).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn register_node(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    let now = now_ms();
    let node = RegisteredNode {
        node_id: req.node_id.clone(),
        status: NodeTrustState::Unknown,
        registered_at_ms: now,
        last_trust_update_ms: 0,
        ak_public_pem: req.ak_public_pem,
        expected_pcr16_digest_hex: req.expected_pcr16_digest_hex,
    };

    if svc.app.persist_and_insert_node(node).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist node" }))).into_response();
    }

    (StatusCode::CREATED, Json(json!({ "node_id": req.node_id, "status": "registered" }))).into_response()
}

async fn issue_challenge(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }
    let nonce: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    svc.app.issue_challenge(&node_id, nonce, now_ms());
    (StatusCode::OK, Json(json!({ "node_id": node_id, "nonce": nonce }))).into_response()
}

async fn verify_attestation(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<VerifyAttestationRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    let now = now_ms();

    // SAFETY: SG9 | REQ: attestation-node-proven-identity | TEST: valid_signature_verifies,legacy_admin_token_hmac_proof_is_rejected,absent_registered_key_fails_closed,wrong_key_is_rejected
    // (#73) Node-PROVEN identity: the node must prove possession of the
    // PRIVATE attestation key matching the `ak_public_pem` it registered, by
    // signing the (node_id, nonce) challenge with Ed25519. The prior
    // `HMAC(KIRRA_ADMIN_TOKEN, nonce)` proof was admin-ASSERTED trust —
    // anyone with the admin token could attest any node. Fail-closed: a node
    // with no registered AK, a malformed key, a malformed proof, or a bad
    // signature is rejected here, before the nonce is consumed or any trust
    // state is written. PCR16 (measured-boot) quote verification is a
    // documented follow-up; see src/attestation.rs.
    let ak_public_pem = match svc.app.nodes.get(&req.node_id) {
        Some(node) => node.ak_public_pem.clone(),
        None => return (StatusCode::NOT_FOUND,
                        Json(json!({ "error": "node not registered" }))).into_response(),
    };

    if let Err(reason) = kirra_runtime_sdk::attestation::verify_attestation_proof(
        ak_public_pem.as_deref(),
        &req.node_id,
        req.nonce,
        &req.proof_hex,
    ) {
        // No registered key is a precondition failure (403); a present-but-
        // failing proof is an authentication failure (401). Either way the
        // attestation is REFUSED — never accepted by default.
        let status = match reason {
            kirra_runtime_sdk::attestation::AttestationError::NoRegisteredKey => {
                StatusCode::FORBIDDEN
            }
            _ => StatusCode::UNAUTHORIZED,
        };
        tracing::warn!(node_id = %req.node_id, reason = %reason.as_str(),
            "attestation proof rejected (fail-closed, #73)");
        return (status, Json(json!({ "error": reason.as_str() }))).into_response();
    }

    if !svc.app.consume_challenge(&req.node_id, req.nonce, now) {
        return (StatusCode::CONFLICT,
                Json(json!({ "error": "nonce absent, expired, or already consumed" }))).into_response();
    }

    let updated = match svc.app.nodes.get(&req.node_id) {
        Some(existing) => RegisteredNode {
            node_id: existing.node_id.clone(),
            status: NodeTrustState::Trusted,
            registered_at_ms: existing.registered_at_ms,
            last_trust_update_ms: now,
            ak_public_pem: existing.ak_public_pem.clone(),
            expected_pcr16_digest_hex: existing.expected_pcr16_digest_hex.clone(),
        },
        None => return (StatusCode::NOT_FOUND,
                        Json(json!({ "error": "node not registered" }))).into_response(),
    };

    if svc.app.persist_and_insert_node(updated).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist trust state" }))).into_response();
    }

    let posture = svc.app.calculate_posture(&req.node_id);
    if let Ok(posture_json) = serde_json::to_string(&posture) {
        if let Ok(mut store) = svc.app.store.lock() {
            if let Err(e) = store.save_posture_event_chained(
                &req.node_id, "ATTESTATION_TRUSTED", &posture_json, None, now,
            ) {
                tracing::error!(error=%e, node_id=%req.node_id,
                    "AUDIT-CHAIN WRITE FAILED for ATTESTATION_TRUSTED — event missing from tamper-evident log");
            }
        }
    }
    emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.node_id.clone()));
    enqueue_recalc(&svc, kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
        node_id: req.node_id.clone(),
        reason:  "ATTESTATION_TRUSTED".to_string(),
    });

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "attested": true }))).into_response()
}

async fn get_node_status(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    match svc.app.nodes.get(&node_id) {
        Some(node) => {
            let status = match &node.status {
                NodeTrustState::Trusted => "Trusted",
                NodeTrustState::Untrusted(_) => "Untrusted",
                NodeTrustState::Unknown => "Unknown",
            };
            (StatusCode::OK, Json(AttestationStatusResponse {
                node_id: node_id.clone(),
                status: status.to_string(),
                registered_at_ms: node.registered_at_ms,
            })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

async fn get_fleet_posture(State(svc): State<Arc<ServiceState>>) -> impl IntoResponse {
    let postures: Vec<FleetNodePosture> = svc.app.nodes
        .iter()
        .map(|entry| svc.app.calculate_posture(entry.key()))
        .collect();
    Json(json!({ "fleet": postures }))
}

async fn get_node_posture(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    let posture = svc.app.calculate_posture(&node_id);
    Json(posture)
}

async fn register_dependencies(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterDependenciesRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    if svc.app.persist_and_insert_deps(&req.node_id, req.depends_on).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist dependencies" }))).into_response();
    }

    let posture = svc.app.calculate_posture(&req.node_id);
    let now = now_ms();
    if let Ok(posture_json) = serde_json::to_string(&posture) {
        if let Ok(mut store) = svc.app.store.lock() {
            if let Err(e) = store.save_posture_event_chained(
                &req.node_id, "DEPENDENCY_UPDATED", &posture_json, None, now,
            ) {
                tracing::error!(error=%e, node_id=%req.node_id,
                    "AUDIT-CHAIN WRITE FAILED for DEPENDENCY_UPDATED — event missing from tamper-evident log");
            }
        }
    }
    emit_posture_event(&svc.app, "DEPENDENCY_GRAPH_MUTATED", Some(req.node_id.clone()));
    enqueue_recalc(&svc, kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::DependencyGraphChanged);

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "dependencies_registered": true }))).into_response()
}

async fn get_node_history(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    match svc.app.store.lock() {
        Ok(store) => match store.load_node_history(&node_id) {
            Ok(history) => Json(json!({ "node_id": node_id, "history": history })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to load history" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn get_node_flap_status(
    State(svc): State<Arc<ServiceState>>,
    Path(node_id): Path<String>,
) -> impl IntoResponse {
    let five_minutes_ago = now_ms().saturating_sub(300_000);
    match svc.app.store.lock() {
        Ok(store) => match store.count_recent_posture_events(&node_id, five_minutes_ago) {
            Ok(count) => {
                let status = FlapStatus {
                    node_id: node_id.clone(),
                    flapping: count >= 3,
                    event_count_5m: count,
                };
                Json(status).into_response()
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to query events" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn verify_audit_chain(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let vk = svc.audit_verifying_key.as_ref();
    match svc.app.store.lock() {
        Ok(store) => match store.verify_audit_chain_full(vk) {
            Ok(r) => Json(json!({
                "chain_intact": r.chain_intact,
                "total_entries": r.total_entries,
                "latest_hash": r.latest_hash,
                "signing_enabled": r.signing_enabled,
                "signed_entries": r.signed_entries,
                "unsigned_entries": r.unsigned_entries,
                "signature_valid": r.signature_valid,
                "first_signed_at_ms": r.first_signed_at_ms,
                "public_key_b64": r.public_key_b64,
            })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "audit chain query failed" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

#[derive(Deserialize)]
struct AuditExportQuery {
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn handle_audit_export(
    State(svc): State<Arc<ServiceState>>,
    Query(params): Query<AuditExportQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(100).min(1000);
    let offset = params.offset.unwrap_or(0);
    let vk = svc.audit_verifying_key.as_ref();
    match svc.app.store.lock() {
        Ok(store) => match store.load_audit_chain_page(limit, offset, vk) {
            Ok(page) => Json(page).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "export query failed" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

#[derive(Deserialize)]
struct RotateSigningKeyRequest {
    /// Base64 of the NEW 32-byte Ed25519 signing seed (the private key). The
    /// store must hold the private half to sign subsequent rows under the new
    /// key — a public-key-only rotation can never actually swap signing (#76).
    /// Admin-gated endpoint; transmit over TLS in production.
    new_signing_key_b64: String,
    reason: String,
}

async fn handle_audit_rotate_key(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RotateSigningKeyRequest>,
) -> impl IntoResponse {
    if !svc.app.is_active() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    // Decode the new signing seed → SigningKey (32-byte Ed25519 seed).
    let new_signing_key = {
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        match b64e.decode(req.new_signing_key_b64.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .map(|seed| ed25519_dalek::SigningKey::from_bytes(&seed))
        {
            Some(sk) => sk,
            None => return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "new_signing_key_b64 must be a base64 32-byte ed25519 seed" }))).into_response(),
        }
    };
    let new_key_id = kirra_runtime_sdk::audit_chain::verifying_key_id(&new_signing_key.verifying_key());
    match svc.app.store.lock() {
        Ok(mut store) => match store.record_key_rotation(new_signing_key, &req.reason, now_ms()) {
            Ok(_) => Json(json!({ "recorded": true, "event_type": "KEY_ROTATION", "new_key_id": new_key_id })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to record key rotation" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn evaluate_action_filter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<ActionClaim>, JsonRejection>,
) -> impl IntoResponse {
    let claim = match body {
        Ok(Json(c)) => c,
        Err(rejection) => {
            if let Ok(mut store) = svc.app.store.lock() {
                let _ = store.save_posture_event_chained(
                    "action_filter", "ACTION_FILTER_MALFORMED_REQUEST",
                    &json!({ "error": rejection.body_text() }).to_string(),
                    Some("malformed request body"), now_ms(),
                );
            }
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "MALFORMED_REQUEST",
                "detail": rejection.body_text(),
                "allowed": false,
            }))).into_response();
        }
    };

    let request_id = now_ms().to_string();

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let decision = evaluate_action_claim(claim.clone(), posture);

    let audit_event_type = if !decision.allowed {
        if decision.reason == "UNKNOWN_ACTION_TYPE" {
            "ACTION_FILTER_UNKNOWN_TYPE"
        } else {
            "ACTION_FILTER_DENIED"
        }
    } else {
        "ACTION_FILTER_ALLOWED"
    };

    let event = json!({
        "request_id": request_id,
        "target_node": claim.target_node,
        "action_type": claim.action_type,
        "risk_class": claim.risk_class,
        "allowed": decision.allowed,
        "reason": decision.reason,
        "posture": posture_str,
        "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
    });
    if let Ok(mut store) = svc.app.store.lock() {
        let _ = store.save_posture_event_chained(
            "action_filter", audit_event_type,
            &event.to_string(), None, now_ms(),
        );
    }

    tracing::info!(
        action_type = %claim.action_type,
        target_node = %claim.target_node,
        allowed = decision.allowed,
        posture = %posture_str,
        reason = %decision.reason,
        request_id = %request_id,
        "action_filter evaluated"
    );

    Json(json!({
        "allowed": decision.allowed,
        "reason": decision.reason,
        "posture_at_evaluation": posture_str,
        "request_id": request_id,
    })).into_response()
}

async fn evaluate_industrial_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<UnifiedIndustrialRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rejection) => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "MALFORMED_REQUEST",
                "detail": rejection.body_text(),
                "allowed": false,
            }))).into_response();
        }
    };

    let (posture, lockout_reason) = gate_posture(&svc);

    let audit_ref = now_ms().to_string();
    let protocol_name = format!("{:?}", req.protocol);

    match evaluate_unified_industrial_request(req, posture) {
        Ok(result) => {
            let should_audit = !result.allowed
                || result.adapter_details.get("is_broadcast").and_then(|v| v.as_bool()).unwrap_or(false);

            if should_audit {
                let audit = json!({
                    "protocol": result.protocol,
                    "command": format!("{:?}", result.command),
                    "allowed": result.allowed,
                    "denial_reason": result.denial_reason,
                    "posture": result.posture_at_evaluation,
                    "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
                    "audit_ref": audit_ref,
                });
                let event_type = if result.allowed {
                    "INDUSTRIAL_ACTION_ALLOWED_BROADCAST"
                } else {
                    "INDUSTRIAL_ACTION_DENIED"
                };
                match svc.app.store.lock() {
                    Ok(mut store) => {
                        if let Err(e) = store.save_posture_event_chained(
                            "industrial_adapter", event_type,
                            &audit.to_string(), None, now_ms(),
                        ) {
                            tracing::error!(error = %e, event_type = event_type,
                                "AUDIT-CHAIN WRITE FAILED for industrial adapter event — event missing from tamper-evident log");
                        }
                    }
                    Err(_) => {
                        tracing::error!(event_type = event_type,
                            "industrial adapter: store lock poisoned — audit write SKIPPED for this event");
                    }
                }
            }

            Json(json!({
                "protocol": result.protocol,
                "command": format!("{:?}", result.command),
                "allowed": result.allowed,
                "denial_reason": result.denial_reason,
                "posture_at_evaluation": result.posture_at_evaluation,
                "adapter_details": result.adapter_details,
                "audit_ref": audit_ref,
                "triggers_recalculation": result.triggers_recalculation,
            })).into_response()
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({
                "error": "ADAPTER_PARSE_FAILURE",
                "detail": e,
                "protocol": protocol_name,
                "allowed": false,
            }))).into_response()
        }
    }
}

async fn evaluate_ethernet_ip_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<EtherNetIpMessage>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let msg = match body {
        Ok(Json(m)) => m,
        Err(rejection) => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "MALFORMED_REQUEST",
                "detail": rejection.body_text(),
                "allowed": false,
            }))).into_response();
        }
    };

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = EtherNetIpAdapter::evaluate(&msg);
    let (allowed, denial_reason) = kirra_runtime_sdk::protocol_adapter::command_allowed_for_posture_pub(&eval.command, &posture);
    let audit_ref = now_ms().to_string();

    if !allowed {
        match svc.app.store.lock() {
            Ok(mut store) => {
                if let Err(e) = store.save_posture_event_chained(
                    "ethernet_ip_adapter", "INDUSTRIAL_ACTION_DENIED",
                    &json!({
                        "service_name": eval.service_name,
                        "safety_relevant": eval.safety_relevant,
                        "posture": posture_str,
                        "denial_reason": denial_reason,
                        "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
                    }).to_string(),
                    None, now_ms(),
                ) {
                    tracing::error!(error = %e, event_type = "INDUSTRIAL_ACTION_DENIED",
                        "AUDIT-CHAIN WRITE FAILED for ethernet_ip adapter event — event missing from tamper-evident log");
                }
            }
            Err(_) => {
                tracing::error!(
                    "ethernet_ip adapter: store lock poisoned — audit write SKIPPED for this denial"
                );
            }
        }
    }

    Json(json!({
        "protocol": "ethernet_ip",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        "adapter_details": {
            "service_name": eval.service_name,
            "is_write": eval.is_write,
            "target_description": eval.target_description,
            "safety_relevant": eval.safety_relevant,
        },
        "audit_ref": audit_ref,
    })).into_response()
}

async fn evaluate_canopen_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<CanOpenMessage>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let msg = match body {
        Ok(Json(m)) => m,
        Err(rejection) => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "MALFORMED_REQUEST",
                "detail": rejection.body_text(),
                "allowed": false,
            }))).into_response();
        }
    };

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = CanOpenAdapter::evaluate(&msg);
    let (allowed, denial_reason) = kirra_runtime_sdk::protocol_adapter::command_allowed_for_posture_pub(&eval.command, &posture);
    let audit_ref = now_ms().to_string();

    if !allowed || eval.triggers_recalculation {
        match svc.app.store.lock() {
            Ok(mut store) => {
                let event_type = if eval.triggers_recalculation {
                    "CANOPEN_NMT_NODE_OFFLINE"
                } else {
                    "INDUSTRIAL_ACTION_DENIED"
                };
                if let Err(e) = store.save_posture_event_chained(
                    "canopen_adapter", event_type,
                    &json!({
                        "node_id": eval.node_id,
                        "message_type": format!("{:?}", eval.message_type),
                        "is_emergency": eval.is_emergency,
                        "triggers_recalculation": eval.triggers_recalculation,
                        "posture": posture_str,
                        "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
                    }).to_string(),
                    None, now_ms(),
                ) {
                    tracing::error!(error = %e, event_type = event_type,
                        "AUDIT-CHAIN WRITE FAILED for canopen adapter event — event missing from tamper-evident log");
                }
            }
            Err(_) => {
                tracing::error!(
                    "canopen adapter: store lock poisoned — audit write SKIPPED for this event"
                );
            }
        }
    }

    // CANopen NMT node-offline / reset surfaces an underlying-fleet change,
    // but there is no production CANopen-bus-address → fleet-node-id
    // mapping in this repo today. The honest minimal wiring is to enqueue
    // a `DependencyGraphChanged` so the freshness pipeline runs — but
    // because no underlying state changed, the resulting recalc recomputes
    // the SAME posture. This is a partial fix; tracked as follow-up so the
    // recalc becomes effectful once the mapping exists.
    if eval.triggers_recalculation {
        tracing::warn!(
            canopen_node_id = eval.node_id,
            source_node     = %msg.source_node,
            "CANopen NMT node-offline triggers recalc, but no CANopen→fleet-node \
             mapping exists yet — recalc is a no-op until that mapping is defined"
        );
        enqueue_recalc(
            &svc,
            kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::DependencyGraphChanged,
        );
    }

    Json(json!({
        "protocol": "canopen",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        "adapter_details": {
            "message_type": format!("{:?}", eval.message_type),
            "node_id": eval.node_id,
            "is_emergency": eval.is_emergency,
            "emergency_code": eval.emergency_code,
            "triggers_recalculation": eval.triggers_recalculation,
        },
        "audit_ref": audit_ref,
    })).into_response()
}

async fn evaluate_dnp3_adapter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<Dnp3Message>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let msg = match body {
        Ok(Json(m)) => m,
        Err(rejection) => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "MALFORMED_REQUEST",
                "detail": rejection.body_text(),
                "allowed": false,
            }))).into_response();
        }
    };

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let eval = Dnp3Adapter::evaluate(&msg);
    let (allowed, denial_reason) = kirra_runtime_sdk::protocol_adapter::command_allowed_for_posture_pub(&eval.command, &posture);
    let audit_ref = now_ms().to_string();

    if !allowed || eval.is_broadcast {
        let event_type = if eval.is_broadcast {
            "DNP3_BROADCAST_COMMAND"
        } else {
            "INDUSTRIAL_ACTION_DENIED"
        };
        match svc.app.store.lock() {
            Ok(mut store) => {
                if let Err(e) = store.save_posture_event_chained(
                    "dnp3_adapter", event_type,
                    &json!({
                        "function_name": eval.function_name,
                        "is_broadcast": eval.is_broadcast,
                        "is_control": eval.is_control,
                        "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
                        "dest_address": msg.dest_address,
                        "posture": posture_str,
                        "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
                    }).to_string(),
                    None, now_ms(),
                ) {
                    tracing::error!(error = %e, event_type = event_type,
                        "AUDIT-CHAIN WRITE FAILED for dnp3 adapter event — event missing from tamper-evident log");
                }
            }
            Err(_) => {
                tracing::error!(event_type = event_type,
                    "dnp3 adapter: store lock poisoned — audit write SKIPPED for this event");
            }
        }
    }

    Json(json!({
        "protocol": "dnp3",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        "adapter_details": {
            "function_name": eval.function_name,
            "is_control": eval.is_control,
            "is_broadcast": eval.is_broadcast,
            "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
        },
        "audit_ref": audit_ref,
    })).into_response()
}

async fn register_federation_controller(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterFederationControllerRequest>,
) -> impl IntoResponse {
    if req.controller_id.trim().is_empty() || req.public_key_b64.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "controller_id and public_key_b64 are required" }))).into_response();
    }
    match svc.app.store.lock() {
        Ok(store) => match store.save_trusted_federation_controller(
            &req.controller_id, &req.public_key_b64, now_ms(),
        ) {
            Ok(()) => (StatusCode::CREATED,
                       Json(json!({ "controller_id": req.controller_id, "registered": true }))).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to register controller" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

#[derive(Deserialize)]
struct RegisterIdentityRequest {
    node_id: String,
    ak_public_fingerprint_hex: String,
}

async fn register_node_identity(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterIdentityRequest>,
) -> impl IntoResponse {
    if req.node_id.trim().is_empty() || req.ak_public_fingerprint_hex.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "node_id and ak_public_fingerprint_hex are required" }))).into_response();
    }
    let now = now_ms();
    match svc.app.store.lock() {
        Ok(mut store) => match store.register_attestation_identity(
            &req.node_id, &req.ak_public_fingerprint_hex, "admin", now,
        ) {
            Ok(()) => {
                emit_posture_event(&svc.app, "NODE_IDENTITY_PROVISIONED", Some(req.node_id.clone()));
                (StatusCode::CREATED,
                 Json(json!({ "node_id": req.node_id, "registered": true }))).into_response()
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to register identity" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn submit_federated_report(
    State(svc): State<Arc<ServiceState>>,
    Json(report): Json<FederatedTrustReport>,
) -> impl IntoResponse {
    let received_at_ms = now_ms();

    let evaluation = evaluate_federated_report(&report, received_at_ms);
    if !evaluation.accepted {
        return Json(evaluation).into_response();
    }

    let mut store = match svc.app.store.lock() {
        Ok(s) => s,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "store lock poisoned" }))).into_response(),
    };

    let pk_b64 = match store.load_trusted_federation_controller_key(&report.source_controller_id) {
        Ok(Some(key)) => key,
        Ok(None) => {
            let event = json!({ "source_controller_id": report.source_controller_id,
                                "reason": "UNREGISTERED_FEDERATION_CONTROLLER" });
            let _ = store.save_posture_event_chained(
                "federation_gateway", "FEDERATION_REJECTED",
                &event.to_string(), Some("unregistered source"), received_at_ms,
            );
            return Json(ReportEvaluation {
                accepted: false,
                reason: "UNREGISTERED_FEDERATION_CONTROLLER".to_string(),
            }).into_response();
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "controller lookup failed" }))).into_response(),
    };

    if !verify_federated_report_signature(&report, &pk_b64) {
        let event = json!({ "source_controller_id": report.source_controller_id,
                            "reason": "INVALID_FEDERATION_SIGNATURE" });
        let _ = store.save_posture_event_chained(
            "federation_gateway", "FEDERATION_REJECTED",
            &event.to_string(), Some("signature mismatch"), received_at_ms,
        );
        return Json(ReportEvaluation {
            accepted: false,
            reason: "INVALID_FEDERATION_SIGNATURE".to_string(),
        }).into_response();
    }

    match store.has_seen_federation_nonce(&report.nonce_hex) {
        Ok(true) => {
            let event = json!({ "source_controller_id": report.source_controller_id,
                                "nonce_hex": report.nonce_hex,
                                "reason": "FEDERATION_NONCE_REPLAY" });
            let _ = store.save_posture_event_chained(
                "federation_gateway", "FEDERATION_REJECTED",
                &event.to_string(), Some("nonce replay"), received_at_ms,
            );
            return Json(ReportEvaluation {
                accepted: false,
                reason: "FEDERATED_NONCE_REPLAY".to_string(),
            }).into_response();
        }
        Ok(false) => {}
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "nonce lookup failed" }))).into_response(),
    }

    match store.save_federated_report_chained(&report, received_at_ms) {
        Ok(()) => Json(evaluation).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "failed to persist federated report" }))).into_response(),
    }
}

async fn get_federated_reports(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    match svc.app.store.lock() {
        Ok(store) => match store.load_federated_reports_for_asset(&asset_id) {
            Ok(reports) => Json(json!({ "asset_id": asset_id, "reports": reports })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "failed to load reports" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn handle_actuator_motion_command(
    State(svc): State<Arc<ServiceState>>,
    // Threaded by `enforce_actuator_safety_envelope` (the route always runs it
    // first). It carries the TRUE verdict — `cmd` below is already the enforced
    // (post-clamp) command, but only this tells us WHETHER a clamp happened, so
    // the response can report it instead of always claiming "Allow".
    Extension(outcome): Extension<EnforcementOutcome>,
    Json(cmd): Json<ProposedVehicleCommand>,
) -> impl IntoResponse {
    let now = now_ms();

    tracing::info!(
        action              = %outcome.action.as_str(),
        linear_velocity_mps = %cmd.linear_velocity_mps,
        steering_angle_deg  = %cmd.steering_angle_deg,
        delta_time_s        = %cmd.delta_time_s,
        "Actuator motion command admitted through safety envelope"
    );

    // Record the verdict in the tamper-evident log, including whether a clamp
    // occurred and the original vs enforced values (previously a clamp was
    // logged indistinguishably from a plain admit).
    let audit = serde_json::json!({
        "action":                       outcome.action.as_str(),
        "original_linear_velocity_mps": outcome.original_linear_velocity_mps,
        "original_steering_angle_deg":  outcome.original_steering_angle_deg,
        "enforced_linear_velocity_mps": outcome.enforced_linear_velocity_mps,
        "enforced_steering_angle_deg":  outcome.enforced_steering_angle_deg,
        "current_velocity_mps":         cmd.current_velocity_mps,
        "current_steering_angle_deg":   cmd.current_steering_angle_deg,
        "delta_time_s":                 cmd.delta_time_s,
        "admitted_at_ms":               now,
    });
    if let Ok(mut store) = svc.app.store.lock() {
        if let Err(e) = store.save_posture_event_chained(
            "actuator_motion", "MOTION_COMMAND_ADMITTED",
            &audit.to_string(), None, now,
        ) {
            tracing::error!(error=%e,
                "AUDIT-CHAIN WRITE FAILED for MOTION_COMMAND_ADMITTED — event missing from tamper-evident log");
        }
    }

    // Response speaks the ROS interceptor's schema (action / enforced_*) AND
    // the legacy keys (now accurate). See `EnforcementOutcome::response_body`.
    (StatusCode::OK, Json(outcome.response_body())).into_response()
}

async fn handle_sensor_fault_report(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<SensorFaultReportRequest>,
) -> impl IntoResponse {
    if req.source_node_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "source_node_id is required" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&req.source_node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }

    let now = now_ms();

    let confidence_floor = match svc.app.store.lock() {
        Ok(store) => store.load_av_confidence_floor(&req.source_node_id)
            .unwrap_or(None)
            .unwrap_or(0.70),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "store lock poisoned" }))).into_response(),
    };

    let is_degraded = req.hardware_fault_detected || req.confidence_score < confidence_floor;

    if is_degraded {
        let reason = if req.hardware_fault_detected { "hardware_fault" } else { "low_confidence" };

        if let Ok(store) = svc.app.store.lock() {
            let _ = store.reset_recovery_streak(&req.source_node_id, now);
        }

        let updated = match svc.app.nodes.get(&req.source_node_id) {
            Some(n) => RegisteredNode {
                node_id:              n.node_id.clone(),
                status:               NodeTrustState::Untrusted(reason.to_string()),
                registered_at_ms:     n.registered_at_ms,
                last_trust_update_ms: now,
                ak_public_pem:        n.ak_public_pem.clone(),
                expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
            },
            None => return (StatusCode::NOT_FOUND,
                            Json(json!({ "error": "node not found" }))).into_response(),
        };

        if svc.app.persist_and_insert_node(updated).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "failed to persist node state" }))).into_response();
        }

        let event = json!({
            "source_node_id":          req.source_node_id,
            "confidence_score":        req.confidence_score,
            "hardware_fault_detected": req.hardware_fault_detected,
            "reason":                  reason,
        });
        if let Ok(mut store) = svc.app.store.lock() {
            if let Err(e) = store.save_posture_event_chained(
                &req.source_node_id, "SENSOR_HEALTH_REPORT_FAULT",
                &event.to_string(), None, now,
            ) {
                tracing::error!(error=%e, node_id=%req.source_node_id,
                    "AUDIT-CHAIN WRITE FAILED for SENSOR_HEALTH_REPORT_FAULT — event missing from tamper-evident log");
            }
        }

        emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.source_node_id.clone()));
        enqueue_recalc(&svc, kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
            node_id: req.source_node_id.clone(),
            reason:  format!("SENSOR_FAULT:{reason}"),
        });

        return (StatusCode::OK, Json(json!({
            "source_node_id": req.source_node_id,
            "accepted": true,
            "fault_recorded": true,
        }))).into_response();
    }

    let currently_untrusted = svc.app.nodes.get(&req.source_node_id)
        .map(|n| matches!(n.status, NodeTrustState::Untrusted(_)))
        .unwrap_or(false);

    if !currently_untrusted {
        if let Ok(store) = svc.app.store.lock() {
            let _ = store.touch_av_telemetry_timestamp(&req.source_node_id, now);
        }
        return (StatusCode::OK, Json(json!({
            "source_node_id": req.source_node_id,
            "accepted": true,
            "fault_recorded": false,
        }))).into_response();
    }

    let decision = match svc.app.store.lock() {
        // `&*store` dereferences the MutexGuard so the generic
        // `S: RecoveryStreakStore` bound on `evaluate_recovery_report`
        // resolves to `&VerifierStore` (S3 / #115 — trait seam, behavior
        // unchanged: the trait impl delegates verbatim).
        Ok(store) => evaluate_recovery_report(&*store, &req.source_node_id, now),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "store lock poisoned" }))).into_response(),
    };

    match &decision {
        HysteresisDecision::RecoveryConfirmed { streak } => {
            let updated = match svc.app.nodes.get(&req.source_node_id) {
                Some(n) => RegisteredNode {
                    node_id:              n.node_id.clone(),
                    status:               NodeTrustState::Trusted,
                    registered_at_ms:     n.registered_at_ms,
                    last_trust_update_ms: now,
                    ak_public_pem:        n.ak_public_pem.clone(),
                    expected_pcr16_digest_hex: n.expected_pcr16_digest_hex.clone(),
                },
                None => return (StatusCode::NOT_FOUND,
                                Json(json!({ "error": "node not found" }))).into_response(),
            };

            if svc.app.persist_and_insert_node(updated).is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "failed to persist node state" }))).into_response();
            }

            if let Ok(mut store) = svc.app.store.lock() {
                let _ = store.reset_recovery_streak(&req.source_node_id, now);
                let event = json!({
                    "source_node_id": req.source_node_id,
                    "streak":         streak,
                });
                if let Err(e) = store.save_posture_event_chained(
                    &req.source_node_id, "SENSOR_RECOVERY_CONFIRMED",
                    &event.to_string(), None, now,
                ) {
                    tracing::error!(error=%e, node_id=%req.source_node_id,
                        "AUDIT-CHAIN WRITE FAILED for SENSOR_RECOVERY_CONFIRMED — event missing from tamper-evident log");
                }
            }

            emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.source_node_id.clone()));
            enqueue_recalc(&svc, kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::NodeTrustChanged {
                node_id: req.source_node_id.clone(),
                reason:  "SENSOR_RECOVERY_CONFIRMED".to_string(),
            });
        }
        HysteresisDecision::StreakBuilding { .. } | HysteresisDecision::WindowExpired { .. } => {}
        HysteresisDecision::NotApplicable => {
            if let Ok(store) = svc.app.store.lock() {
                let _ = store.touch_av_telemetry_timestamp(&req.source_node_id, now);
            }
        }
    }

    (StatusCode::OK, Json(json!({
        "source_node_id":      req.source_node_id,
        "accepted":            true,
        "fault_recorded":      false,
        "hysteresis_decision": format!("{:?}", decision),
    }))).into_response()
}

async fn handle_register_av_asset(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterAvAssetRequest>,
) -> impl IntoResponse {
    if req.node_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({ "error": "node_id is required" }))).into_response();
    }
    if !svc.app.nodes.contains_key(&req.node_id) {
        return (StatusCode::NOT_FOUND,
                Json(json!({ "error": "node not registered" }))).into_response();
    }

    let now = now_ms();
    let floor = req.confidence_floor.unwrap_or(0.70);

    match svc.app.store.lock() {
        Ok(mut store) => {
            if let Err(e) = store.register_av_subsystem_meta(
                &req.node_id, &req.subsystem_type, &req.hardware_id, floor, now,
            ) {
                tracing::warn!(
                    error   = %e,
                    node_id = %req.node_id,
                    "Failed to register av_subsystem_meta"
                );
            }
            let meta = json!({
                "subsystem_type":   req.subsystem_type,
                "hardware_id":      req.hardware_id,
                "confidence_floor": floor,
            });
            if let Err(e) = store.save_posture_event_chained(
                &req.node_id, "AV_ASSET_REGISTERED", &meta.to_string(), None, now,
            ) {
                tracing::error!(error=%e, node_id=%req.node_id,
                    "AUDIT-CHAIN WRITE FAILED for AV_ASSET_REGISTERED — event missing from tamper-evident log");
            }
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }

    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "registered": true }))).into_response()
}

// --- Fabric handlers --------------------------------------------------------

#[derive(Deserialize)]
struct RegisterFabricAssetRequest {
    asset_id: String,
    asset_type: AssetType,
    display_name: String,
    kinematic_profile: KinematicProfileType,
    metadata: Option<std::collections::HashMap<String, String>>,
}

async fn handle_register_fabric_asset(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<RegisterFabricAssetRequest>, JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.body_text()}))).into_response(),
    };
    if req.asset_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "asset_id is required"}))).into_response();
    }
    let asset = FabricAsset {
        asset_id: req.asset_id.clone(),
        asset_type: req.asset_type,
        display_name: req.display_name,
        kinematic_profile: req.kinematic_profile,
        registered_at_ms: now_ms(),
        last_seen_ms: now_ms(),
        metadata: req.metadata.unwrap_or_default(),
    };
    svc.fabric_router.register_asset(&asset);
    if let Ok(store) = svc.app.store.lock() {
        let _ = store.save_fabric_asset(&asset);
    }
    (StatusCode::CREATED, Json(json!({"asset_id": req.asset_id, "registered": true}))).into_response()
}

async fn handle_list_fabric_assets(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let assets = svc.fabric_router.list_assets();
    let total = assets.len();
    Json(json!({"assets": assets, "total": total})).into_response()
}

async fn handle_fabric_state(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let changes = svc.fabric_router.propagate_cross_asset_trust();
    for (asset_id, new_posture) in changes {
        let gen = svc.fabric_router.fabric_state().fabric_generation + 1;
        svc.fabric_router.update_asset_posture(&asset_id, AssetPosture {
            asset_id: asset_id.clone(),
            posture: new_posture.clone(),
            generation: gen,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["cross_asset_propagation".to_string()],
        });
    }
    let state = svc.fabric_router.fabric_state();
    Json(state).into_response()
}

async fn handle_fabric_telemetry(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let summary = svc.fabric_telemetry.summary();
    Json(summary).into_response()
}

async fn handle_fabric_telemetry_asset(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    match svc.fabric_telemetry.asset_snapshot(&asset_id) {
        Some(snap) => Json(snap).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "asset not found"}))).into_response(),
    }
}

async fn handle_fabric_command(
    State(svc): State<Arc<ServiceState>>,
    Path(asset_id): Path<String>,
    body: Result<Json<ProposedVehicleCommand>, JsonRejection>,
) -> impl IntoResponse {
    let cmd = match body {
        Ok(Json(c)) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.body_text()}))).into_response(),
    };

    // KIRRA-OCCY-PMON-002: resolve the perception-derate cap O(1) here (the
    // handler holds `svc`/`ServiceState`) and thread it through to the fabric
    // governor's Nominal arm. `None` while the monitor is disabled (default).
    let perception_cap = kirra_runtime_sdk::gateway::perception_monitor::resolve_perception_cap(
        svc.perception_monitor_enabled,
        &svc.perception_cap,
        now_ms(),
    );
    match svc.fabric_router.route_command(&asset_id, &cmd, perception_cap) {
        Ok(action) => {
            let action_str = format!("{:?}", action);
            let allowed = !matches!(action, kirra_runtime_sdk::gateway::kinematics_contract::EnforceAction::DenyBreach(_));

            let now = now_ms();
            if !allowed {
                // `DenyCode -> &'static str` keeps this path alloc-free; the previous
                // `r.clone()` of a `String` allocated per denial (S3 / #115).
                let denial_reason: &'static str = match action {
                    kirra_runtime_sdk::gateway::kinematics_contract::EnforceAction::DenyBreach(c) => c.reason(),
                    _ => "",
                };
                svc.fabric_causal_log.record(
                    &asset_id,
                    "COMMAND_DENIED",
                    &json!({"reason": denial_reason, "command": serde_json::to_value(&cmd).unwrap_or_default()}).to_string(),
                    vec![],
                    vec![],
                    svc.fabric_router.fabric_state().fabric_generation,
                );
                if let Ok(mut store) = svc.app.store.lock() {
                    let _ = store.save_posture_event_chained(
                        &asset_id, "FABRIC_COMMAND_DENIED",
                        &json!({"asset_id": asset_id, "action": action_str}).to_string(),
                        None, now,
                    );
                }
            }

            Json(json!({
                "asset_id": asset_id,
                "action": action_str,
                "allowed": allowed,
            })).into_response()
        }
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

#[derive(Deserialize)]
struct CausalLogQuery {
    from_ms: Option<u64>,
    to_ms: Option<u64>,
}

async fn handle_fabric_causal_log(
    State(svc): State<Arc<ServiceState>>,
    Query(q): Query<CausalLogQuery>,
) -> impl IntoResponse {
    let from = q.from_ms.unwrap_or(0);
    let to = q.to_ms.unwrap_or(u64::MAX);
    let entries = svc.fabric_causal_log.export(from, to);
    let total = entries.len();
    Json(json!({"entries": entries, "total": total})).into_response()
}

async fn handle_fabric_causal_chain(
    State(svc): State<Arc<ServiceState>>,
    Path(entry_id): Path<String>,
) -> impl IntoResponse {
    let chain = svc.fabric_causal_log.causal_chain(&entry_id);
    if chain.is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "entry not found"}))).into_response();
    }
    let depth = chain.len();
    Json(json!({"entry_id": entry_id, "chain": chain, "depth": depth})).into_response()
}

// --- Entry point ------------------------------------------------------------

#[tokio::main]
async fn main() {
    let db_path = std::env::var("KIRRA_DB_PATH")
        .unwrap_or_else(|_| "kirra_verifier.sqlite".to_string());
    let listen_addr = std::env::var("KIRRA_VERIFIER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string());

    let mut store = VerifierStore::new(&db_path)
        .expect("failed to initialize verifier store");

    let mode = VerifierOperationMode::from_env();
    println!("Kirra Verifier starting in {mode:?} mode (db: {db_path})");

    let audit_signing_key: Option<ed25519_dalek::SigningKey> =
        std::env::var("KIRRA_LOG_SIGNING_KEY").ok()
            .filter(|s| !s.is_empty())
            .and_then(|b64_str| {
                use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
                b64e.decode(&b64_str).ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                    .map(|seed| ed25519_dalek::SigningKey::from_bytes(&seed))
            });
    let audit_verifying_key = audit_signing_key.as_ref().map(|sk| sk.verifying_key());

    // #165: admit the env-loaded signing key against the DURABLE trust map
    // (audit_trust_anchor + audit_key_ledger) before any signing happens.
    // First boot backfills the anchor; a matching active key resumes; a retired
    // key (restart-reverted) or a brand-new env key WITHOUT an explicit adopt
    // signal is FAIL-CLOSED — the process refuses to start rather than sign
    // under the wrong key. Adopt is opt-in via KIRRA_LOG_SIGNING_KEY_ADOPT=1;
    // an optional KIRRA_LOG_SIGNING_GENESIS_PIN pins the durable genesis.
    if let Some(ref key) = audit_signing_key {
        let adopt = std::env::var("KIRRA_LOG_SIGNING_KEY_ADOPT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let pinned = std::env::var("KIRRA_LOG_SIGNING_GENESIS_PIN")
            .ok()
            .filter(|s| !s.is_empty());
        let admission = store
            .admit_signing_key(key.clone(), adopt, pinned.as_deref(), now_ms())
            .expect("failed to admit audit signing key against the durable trust map");
        use kirra_runtime_sdk::verifier_store::KeyAdmission;
        match admission {
            KeyAdmission::Resumed
            | KeyAdmission::BackfilledGenesis
            | KeyAdmission::AdoptedReanchor => {
                println!("Audit signing key admitted ({admission:?}).");
            }
            KeyAdmission::RetiredKeyRejected => panic!(
                "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_KEY is a RETIRED audit key \
                 (a later rotation is the durable active key). Refusing to sign under \
                 a retired key. Provide the current active private key, or perform an \
                 explicit rotation."
            ),
            KeyAdmission::UnadoptedNewKeyRejected => panic!(
                "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_KEY is a NEW key not in the durable \
                 ledger and no adopt signal was given. Refusing to silently re-root audit \
                 trust. Set KIRRA_LOG_SIGNING_KEY_ADOPT=1 to consent to adopting it."
            ),
            KeyAdmission::GenesisPinMismatch => panic!(
                "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_GENESIS_PIN does not match the durable \
                 trust anchor's genesis. Refusing to start."
            ),
            KeyAdmission::MigrationReversionRejected { chain_latest_key_id, env_key_id } => panic!(
                "FAIL-CLOSED (#165 migration): the audit chain's latest rotation is to key \
                 {chain_latest_key_id} but KIRRA_LOG_SIGNING_KEY supplied {env_key_id}. The env \
                 key has reverted to a pre-rotation (or foreign) key; anchoring on it would \
                 re-root audit trust. RESOLUTION — supply the correct active key in \
                 KIRRA_LOG_SIGNING_KEY, OR set KIRRA_LOG_SIGNING_KEY_ADOPT=1 to consent to \
                 anchoring on the env key (recorded as a consented reanchor)."
            ),
        }
    }

    let app_state = Arc::new(AppState::new(store, mode));

    // S3 Pass B2 (#115): spawn the audit-writer task and install its Sender
    // into AppState. The deny arm of the actuator-safety-envelope middleware
    // reaches the Sender via `svc.app.audit_writer_tx.get()` to push the
    // kinematic-violation audit record off the verdict path. Done before
    // the listener binds so no request can race the install.
    let audit_tx =
        kirra_runtime_sdk::audit_writer::spawn_audit_writer(Arc::clone(&app_state));
    app_state.install_audit_writer(audit_tx);

    // Learning-loop capture writer (Phase 1, #190) — DEFAULT OFF. Only spawned +
    // installed when KIRRA_CAPTURE_ENABLED is set; unset → no writer, and the
    // gateway emit is a pure no-op (capture_writer_tx stays None). Non-safety
    // side channel; mirrors the audit writer wiring above.
    if kirra_runtime_sdk::capture::capture_enabled() {
        let capture_tx = kirra_runtime_sdk::capture::spawn_capture_writer();
        app_state.install_capture_writer(capture_tx);
        tracing::info!("learning-loop capture ENABLED (KIRRA_CAPTURE_ENABLED) — verdict records → JSONL sink");
    }

    {
        let guard = app_state.store.lock()
            .expect("verifier store lock poisoned during boot hydration");

        for node in guard.load_nodes().expect("failed to load persisted nodes") {
            app_state.nodes.insert(node.node_id.clone(), node);
        }
        for (node_id, deps) in guard.load_dependencies()
            .expect("failed to load persisted dependencies")
        {
            app_state.dependency_graph.insert(node_id, deps);
        }
    }

    let signing_key = audit_signing_key.clone();
    let svc_state = Arc::new(ServiceState {
        app: app_state,
        posture_cache: Arc::new(std::sync::RwLock::new(None)),
        audit_verifying_key,
        fabric_router: Arc::new(FabricRouter::new()),
        fabric_telemetry: Arc::new(FabricTelemetry::new()),
        fabric_causal_log: Arc::new(FabricCausalLog::new(signing_key)),
        posture_engine_tx: std::sync::OnceLock::new(),
        // KIRRA-OCCY-PMON-002: perception-derate composition. DEFAULT OFF —
        // pure no-op (state 1) until #126 wires a real perception ingest and a
        // deployment enables the monitor + starts the publisher worker.
        perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    });

    {
        let assets_loaded;
        if let Ok(store) = svc_state.app.store.lock() {
            if let Ok(assets) = store.load_fabric_assets() {
                assets_loaded = assets.len();
                for asset in assets {
                    svc_state.fabric_router.register_asset(&asset);
                }
            } else {
                assets_loaded = 0;
            }
        } else {
            assets_loaded = 0;
        }
        tracing::info!(count = assets_loaded, "Loaded fabric assets from store");
    }

    // Heartbeat-aware startup arbitration (HA epoch fence).
    //
    // A configured-Active instance must CLAIM the durable epoch before
    // it starts heartbeating, but must NOT steal from a live holder
    // (prevents a restarted old primary from stealing back from a
    // standby that has already promoted). The decision is:
    //
    //   1. Read (epoch E, active_id A) from the singleton ha_state row.
    //   2. Read primary heartbeat age. If A is some OTHER instance and
    //      heartbeat is fresh, stand down to PassiveStandby.
    //   3. Otherwise try_claim_epoch(E, my_id, now). On win, hold the
    //      epoch and proceed Active. On loss, stand down to PassiveStandby
    //      (a concurrent claim landed first — fence held).
    //
    // Even if clock skew makes a live holder LOOK stale, the worst case
    // here is an EXTRA failover: this node claims and bumps the epoch,
    // the real holder gets fenced at its gate (STEP 5) and self-demotes.
    // Still at most one effective writer.
    //
    // A configured-PassiveStandby instance does NOT attempt to claim at
    // startup; it spawns the promotion monitor as before. The monitor
    // will claim via perform_promotion when the primary heartbeat goes
    // stale (the same conditional CAS path).
    let my_id = ha_instance_id();
    let effective_mode = match mode {
        VerifierOperationMode::PassiveStandby => VerifierOperationMode::PassiveStandby,
        VerifierOperationMode::Active => {
            let arbitration = svc_state.app.store.lock().ok().and_then(|store| {
                let (epoch, holder) = store.current_active_holder().ok()?;
                let hb_str = store.load_engine_state(HEARTBEAT_KEY).ok()?;
                let now = now_ms();
                let hb_fresh = hb_str
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|ts| now.saturating_sub(ts) < PROMOTION_TIMEOUT_MS)
                    .unwrap_or(false);
                Some((epoch, holder, hb_fresh))
            });

            match arbitration {
                Some((epoch, Some(holder), true)) if holder != my_id => {
                    tracing::warn!(
                        my_id = %my_id,
                        live_holder = %holder,
                        epoch = epoch,
                        "another Active instance is alive at startup — starting as PassiveStandby instead"
                    );
                    VerifierOperationMode::PassiveStandby
                }
                Some((epoch, _holder, _stale_or_self)) => {
                    let claim = svc_state.app.store.lock().ok().and_then(|mut s| {
                        s.try_claim_epoch(epoch, &my_id, now_ms()).ok().flatten()
                    });
                    match claim {
                        Some(new_epoch) => {
                            svc_state
                                .app
                                .held_epoch
                                .store(new_epoch, std::sync::atomic::Ordering::SeqCst);
                            tracing::info!(
                                my_id = %my_id,
                                epoch = new_epoch,
                                "Active startup: durable epoch claimed"
                            );
                            VerifierOperationMode::Active
                        }
                        None => {
                            tracing::error!(
                                my_id = %my_id,
                                observed_epoch = epoch,
                                "Active startup: epoch claim LOST — another instance won the race; \
                                 starting as PassiveStandby (fail-closed; one writer invariant preserved)"
                            );
                            VerifierOperationMode::PassiveStandby
                        }
                    }
                }
                None => {
                    // Could not read ha_state (store error). Fail closed to
                    // PassiveStandby rather than serve as an unfenced Active.
                    tracing::error!(
                        my_id = %my_id,
                        "Active startup: unable to read ha_state for epoch arbitration; \
                         starting as PassiveStandby (fail-closed)"
                    );
                    VerifierOperationMode::PassiveStandby
                }
            }
        }
    };

    // Bring the per-process mode atomic in line with the arbitrated decision.
    // (AppState::new initialized mode_active from the env-derived `mode`; if
    // arbitration downgraded Active→PassiveStandby, flip it off.)
    if effective_mode == VerifierOperationMode::PassiveStandby {
        svc_state
            .app
            .mode_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    match effective_mode {
        VerifierOperationMode::Active => {
            spawn_heartbeat_writer(Arc::clone(&svc_state.app));
            tracing::info!("Heartbeat writer started (Active mode)");
        }
        VerifierOperationMode::PassiveStandby => {
            spawn_promotion_monitor(
                Arc::clone(&svc_state.app),
                Arc::clone(&svc_state.posture_cache),
            );
            tracing::info!("Promotion monitor started (PassiveStandby mode)");
        }
    }

    // One-time, idempotent: anchor the v1/v2 hash boundary in the audit
    // chain. Active instances only (the anchor is a write; a passive
    // standby is read-only and must not write — a later promotion path
    // will run this and no-op via the idempotency guard). Runs AFTER
    // set_signing_key so the HASH_V2_MIGRATION event itself is signed
    // and therefore tamper-evident. The two info/error log lines below
    // are the OBSERVABLE PROOF the wiring is live — operators can
    // confirm the anchor ran (or was deliberately skipped) from the
    // startup log alone, since the assembled-app self-test that would
    // catch a missing call sits behind the build_app extraction follow-up
    // (#72). Do not remove the log lines.
    if svc_state.app.is_active() {
        match svc_state.app.store.lock() {
            Ok(mut store) => match store.ensure_hash_v2_migration_anchor(now_ms()) {
                Ok(()) => tracing::info!("audit: hash-v2 migration anchor ensured"),
                Err(e) => tracing::error!(
                    error = %e,
                    "audit: hash-v2 migration anchor FAILED at startup"
                ),
            },
            Err(_) => tracing::error!(
                "audit: hash-v2 migration anchor skipped — store lock poisoned at startup"
            ),
        }
        // Key-id backfill (#76): assign existing NULL-key_id rows the genesis
        // key's id so they verify after a future rotation. Idempotent; signed.
        match svc_state.app.store.lock() {
            Ok(mut store) => match store.ensure_key_id_backfill_migration(now_ms()) {
                Ok(()) => tracing::info!("audit: key-id backfill migration ensured"),
                Err(e) => tracing::error!(
                    error = %e,
                    "audit: key-id backfill migration FAILED at startup"
                ),
            },
            Err(_) => tracing::error!(
                "audit: key-id backfill migration skipped — store lock poisoned at startup"
            ),
        }
    } else {
        tracing::info!(
            "audit: hash-v2 + key-id migrations skipped — passive standby (read-only)"
        );
    }

    // ── Posture-cache freshness wiring (Active path only) ────────────────
    //
    // Without this, a fresh Active primary serves 503 for every functional
    // route: the posture cache starts as `None`, the routing gate
    // fail-closes on None or stale, and nothing on the Active path was
    // populating the cache. The three-part fix:
    //
    //   (a) one synchronous initial recalc BEFORE axum::serve so the gate
    //       has a populated cache on first request,
    //   (b) the serialized posture-engine worker spawned so event-driven
    //       triggers (NodeTrustChanged, DependencyGraphChanged, etc.)
    //       refresh the cache,
    //   (c) a periodic recompute-and-restamp loop at POSTURE_REFRESH_INTERVAL_MS
    //       (= TTL/2) — load-bearing: without it the cache goes stale
    //       one TTL after the last event and the gate fails closed
    //       fleet-wide. The same loop is the engine-liveness signal: if
    //       the loop stops (worker dead, channel full repeatedly), the
    //       cache goes stale and the gate fails closed — the desired
    //       fail-safe.
    //
    // PassiveStandby does not run this — its promotion path already calls
    // recalculate_and_broadcast once on transition to Active. Ongoing
    // freshness on the freshly-promoted node is filed as a C2 follow-up.
    // SG-008 startup-invariant fact: set true once the watchdog is spawned on
    // the Active path (PassiveStandby leaves it false — and the sentinel does
    // not require it there).
    let mut watchdog_spawned = false;
    if svc_state.app.is_active() {
        kirra_runtime_sdk::posture_engine::recalculate_and_broadcast(
            &svc_state.app,
            &svc_state.posture_cache,
        );
        tracing::info!("posture: initial recalc complete; cache populated");

        let posture_tx = kirra_runtime_sdk::posture_engine_v2::start_posture_engine_worker(
            Arc::clone(&svc_state.app),
            Arc::clone(&svc_state.posture_cache),
        );
        svc_state
            .posture_engine_tx
            .set(posture_tx.clone())
            .expect("posture_engine_tx must not be set before startup wiring");
        tracing::info!("posture: serialized worker started");

        // SAFETY: SG9 | REQ: sensor-liveness-watchdog | TEST: test_watchdog_dead_mans_switch_fires_after_telemetry_timeout
        // Phase 4 (S131): wire the telemetry watchdog into the
        // production binary. This is the first real consumer of the
        // PostureEngineSender from a sensor-liveness path — gated until
        // now on the cold-refresh deadlock fix that landed on
        // `s3-watchdog-deadlock-fix`. The watchdog runs as a background
        // task; a node going silent past AV_TELEMETRY_TIMEOUT_MS
        // produces a WatchdogTimeout trigger, which the posture engine
        // worker consumes and recomputes the posture (typically
        // collapsing to LockedOut for the affected node, which fails
        // the actuator gate closed).
        kirra_runtime_sdk::telemetry_watchdog::spawn_telemetry_watchdog(
            Arc::clone(&svc_state.app),
            posture_tx.clone(),
        );
        watchdog_spawned = true;
        tracing::info!(
            timeout_ms = kirra_runtime_sdk::telemetry_watchdog::AV_TELEMETRY_TIMEOUT_MS,
            "telemetry watchdog spawned (SG9 sensor-liveness)"
        );

        let refresh_tx = posture_tx;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(
                kirra_runtime_sdk::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
            ));
            // First tick fires immediately; skip it (the synchronous
            // initial recalc above already covered cold start).
            tick.tick().await;
            loop {
                tick.tick().await;
                if refresh_tx
                    .try_send(kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger::PeriodicRefresh)
                    .is_err()
                {
                    tracing::error!(
                        "posture periodic refresh: worker channel unavailable — \
                         cache will go stale (gate will fail-close fleet-wide)"
                    );
                }
            }
        });
        tracing::info!(
            interval_ms = kirra_runtime_sdk::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
            ttl_ms = kirra_runtime_sdk::posture_cache::POSTURE_CACHE_TTL_MS,
            "posture: periodic refresh loop started"
        );
    } else {
        tracing::info!(
            "posture: freshness wiring skipped — passive standby (no recalc/worker/refresh)"
        );
    }

    let identity_gated_routes = Router::new()
        .route("/system/posture/stream", get(system_posture_stream))
        .route("/federation/reports/submit", post(submit_federated_report))
        .route("/action_filter/evaluate", post(evaluate_action_filter))
        .route("/industrial/evaluate", post(evaluate_industrial_adapter))
        .route("/industrial/ethernet-ip/evaluate", post(evaluate_ethernet_ip_adapter))
        .route("/industrial/canopen/evaluate", post(evaluate_canopen_adapter))
        .route("/industrial/dnp3/evaluate", post(evaluate_dnp3_adapter))
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_client_identity))
        .layer(middleware::from_fn(require_admin_token));

    let admin_routes = Router::new()
        .route("/attestation/register", post(register_node))
        .route("/fleet/dependencies", post(register_dependencies))
        .route("/fleet/diagnostics/report", post(handle_sensor_fault_report))
        .route("/fleet/assets/register", post(handle_register_av_asset))
        .route("/system/backup/export", post(export_backup))
        .route("/system/audit/verify", get(verify_audit_chain))
        .route("/system/audit/export", get(handle_audit_export))
        .route("/system/audit/rotate-signing-key", post(handle_audit_rotate_key))
        .route("/federation/controllers/register", post(register_federation_controller))
        .route("/attestation/identity/register", post(register_node_identity))
        .route("/fabric/assets/register", post(handle_register_fabric_asset))
        .route("/fabric/assets", get(handle_list_fabric_assets))
        .route("/fabric/state", get(handle_fabric_state))
        .route("/fabric/telemetry", get(handle_fabric_telemetry))
        .route("/fabric/telemetry/{asset_id}", get(handle_fabric_telemetry_asset))
        .route("/fabric/command/{asset_id}", post(handle_fabric_command))
        .route("/fabric/causal-log", get(handle_fabric_causal_log))
        .route("/fabric/causal-log/{entry_id}", get(handle_fabric_causal_chain))
        .layer(middleware::from_fn(require_admin_token));

    let actuator_routes = Router::new()
        .route("/actuator/motion/command", post(handle_actuator_motion_command))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            enforce_actuator_safety_envelope,
        ))
        .layer(middleware::from_fn(require_admin_token));

    let attestation_routes = Router::new()
        .route("/attestation/challenge/{node_id}", post(issue_challenge))
        .route("/attestation/verify", post(verify_attestation));

    let probe_routes = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready));

    let read_routes = Router::new()
        .route("/attestation/status/{node_id}", get(get_node_status))
        .route("/fleet/posture", get(get_fleet_posture))
        .route("/fleet/posture/{node_id}", get(get_node_posture))
        .route("/fleet/history/{node_id}", get(get_node_history))
        .route("/fleet/flapping/{node_id}", get(get_node_flap_status))
        .route("/federation/reports/{asset_id}", get(get_federated_reports));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .merge(probe_routes)
        .merge(identity_gated_routes)
        .merge(admin_routes)
        .merge(actuator_routes)
        .merge(attestation_routes)
        .merge(read_routes)
        .with_state(svc_state.clone())
        .layer(cors)
        // Outermost layer: command-classification + posture-routing gate.
        // Runs BEFORE auth and the actuator envelope on every request;
        // is_posture_exempt allowlists liveness / observability paths so
        // probes stay reachable regardless of fleet posture. Returns 503
        // SERVICE_UNAVAILABLE on denial (transient server-state condition,
        // retryable once posture recovers).
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            enforce_posture_routing,
        ));

    // SG-008 (ASIL D): fail closed BEFORE binding the listener. Build the boot
    // facts and evaluate the startup-invariant predicate; on any violation, log
    // and abort so no request can reach a half-initialized service. Bind is
    // strictly AFTER this check, so "the listener never binds before invariants
    // pass" holds by construction.
    let startup_ctx = StartupContext {
        admin_token_present: std::env::var("KIRRA_ADMIN_TOKEN")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        sqlite_wal: svc_state.app.store.lock().unwrap().is_wal_mode(),
        mode_active: svc_state.app.is_active(),
        watchdog_spawned,
        posture_engine_running: svc_state.posture_engine_tx.get().is_some(),
    };
    if let Err(violation) = check_startup_invariants(&startup_ctx) {
        tracing::error!(
            invariant = %violation,
            "SG-008: startup invariant violated — aborting before listener bind (fail-closed)"
        );
        std::process::exit(1);
    }
    tracing::info!("SG-008: startup invariants satisfied; binding listener");

    println!("Kirra Verifier Service listening on {listen_addr} (db: {db_path})");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await
        .expect("failed to bind listener");

    // #74: on safe-stop / shutdown, force a durable checkpoint so the audit chain
    // (and any NORMAL-connection writes) are fsync'd to disk — durable at the
    // moment that matters most (the incident preceding the stop). The HA epoch
    // and federation nonce burns are already FULL-synced per-commit.
    let shutdown_state = Arc::clone(&svc_state.app);
    let shutdown = async move {
        shutdown_signal().await;
        match shutdown_state.store.lock() {
            Ok(store) => match store.durable_checkpoint() {
                Ok(()) => tracing::info!("audit: durable checkpoint flushed on shutdown"),
                Err(e) => tracing::error!(error = %e, "audit: durable checkpoint FAILED on shutdown"),
            },
            Err(_) => tracing::error!("audit: durable checkpoint skipped — store lock poisoned at shutdown"),
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("server error");
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM — the safe-stop / shutdown signals.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

// ---------------------------------------------------------------------------
// CERT-003 — SG-008 RTM coverage (ASIL D): Process Fail-Closed on Startup
//
// Verifies: SG-008 — the startup sentinel refuses to bind unless every
// safety-critical startup invariant holds. We test the PURE predicate
// (`check_startup_invariants`) for each individual violation, the all-present
// Ok case, and the mode distinction (watchdog/posture-engine are required only
// on the Active path). The abort + bind ordering is structural: `main` calls
// this predicate immediately before `TcpListener::bind` and `process::exit(1)`s
// on Err, so a failing predicate means the listener is never reached. These
// live in the bin (the predicate is `pub(crate)` and not visible to an external
// integration test).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod sg_008_cert_tests {
    use super::{check_startup_invariants, StartupContext, StartupInvariant};

    /// All invariants satisfied on the Active path.
    fn all_ok_active() -> StartupContext {
        StartupContext {
            admin_token_present: true,
            sqlite_wal: true,
            mode_active: true,
            watchdog_spawned: true,
            posture_engine_running: true,
        }
    }

    #[test]
    fn test_startup_ok_when_all_invariants_hold() {
        assert_eq!(
            check_startup_invariants(&all_ok_active()),
            Ok(()),
            "SG-008: startup must succeed when all invariants hold"
        );
    }

    #[test]
    fn test_startup_aborts_without_admin_token() {
        let ctx = StartupContext { admin_token_present: false, ..all_ok_active() };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: startup must fail closed when KIRRA_ADMIN_TOKEN is absent/empty"
        );
    }

    #[test]
    fn test_startup_aborts_when_sqlite_not_wal() {
        let ctx = StartupContext { sqlite_wal: false, ..all_ok_active() };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::SqliteNotWal),
            "SG-008: startup must fail closed when the store is not in WAL mode"
        );
    }

    #[test]
    fn test_startup_aborts_when_watchdog_not_spawned_on_active() {
        let ctx = StartupContext { watchdog_spawned: false, ..all_ok_active() };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::WatchdogNotSpawned),
            "SG-008: an Active node must fail closed if the telemetry watchdog is not spawned"
        );
    }

    #[test]
    fn test_startup_aborts_when_posture_engine_down_on_active() {
        let ctx = StartupContext { posture_engine_running: false, ..all_ok_active() };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::PostureEngineDown),
            "SG-008: an Active node must fail closed if the posture-engine worker is not running"
        );
    }

    /// PassiveStandby is read-only and runs neither the watchdog nor the
    /// posture engine — so their absence must NOT abort a standby, but the
    /// admin-token and WAL invariants still apply.
    #[test]
    fn test_standby_ok_without_watchdog_or_posture_engine() {
        let ctx = StartupContext {
            admin_token_present: true,
            sqlite_wal: true,
            mode_active: false,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Ok(()),
            "SG-008: PassiveStandby must boot without watchdog/posture-engine (not required in read-only mode)"
        );
    }

    #[test]
    fn test_standby_still_requires_admin_token_and_wal() {
        let no_token = StartupContext {
            admin_token_present: false,
            sqlite_wal: true,
            mode_active: false,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&no_token),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: admin token is required in every mode"
        );
        let no_wal = StartupContext { admin_token_present: true, sqlite_wal: false, ..no_token };
        assert_eq!(
            check_startup_invariants(&no_wal),
            Err(StartupInvariant::SqliteNotWal),
            "SG-008: WAL mode is required in every mode"
        );
    }

    /// Order stability: when multiple invariants are violated, the admin-token
    /// check (first/highest-priority) wins — deterministic diagnosis.
    #[test]
    fn test_invariant_check_order_is_stable() {
        let ctx = StartupContext {
            admin_token_present: false,
            sqlite_wal: false,
            mode_active: true,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: the admin-token invariant must be reported first when several are violated"
        );
    }
}
