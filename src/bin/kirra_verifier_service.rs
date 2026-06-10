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
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use kirra_runtime_sdk::verifier::{
    validate_client_identity_headers, AppState, BackupExport, FlapStatus, FleetNodePosture,
    FleetPosture, HealthResponse, NodeTrustState, PostureStreamEvent, RegisteredNode, VerifierOperationMode,
};
use kirra_runtime_sdk::verifier_store::{DurableWriteError, VerifierStore};
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

/// Verifier→fabric posture feed (#88, single-local-asset model).
///
/// Mirrors THIS controller's aggregate `FleetPosture` into the fabric
/// posture of the one locally governed asset named by `KIRRA_FABRIC_ASSET_ID`,
/// so fabric command enforcement for that asset reflects real verified trust
/// rather than the interim `Degraded` registration seed.
///
/// Seam: `recalculate_and_broadcast` lives in the lib and cannot see the
/// `FabricRouter` (it is on `ServiceState`, here in the binary). The posture
/// broadcast (`app.posture_tx`) fires on every fleet-posture transition,
/// including those produced by the lib-side posture-engine worker, so a
/// broadcast subscriber catches all transitions from one place.
///
/// Inert (logs once, no task spawned) when `KIRRA_FABRIC_ASSET_ID` is
/// unset/empty: the asset then keeps its registration seed. This is the
/// single-asset model — other registered assets are intentionally NOT fed
/// here, which is why the registration seed stays `Degraded` rather than
/// `LockedOut` (an unfed asset must not be bricked).
fn spawn_local_asset_posture_feed(svc: Arc<ServiceState>) {
    let asset_id = match std::env::var("KIRRA_FABRIC_ASSET_ID") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            tracing::info!(
                "verifier→fabric posture feed: KIRRA_FABRIC_ASSET_ID unset — \
                 feed inert (local fabric asset keeps its registration seed)"
            );
            return;
        }
    };

    tokio::spawn(async move {
        // Subscribe BEFORE the initial sync so a transition occurring in the
        // window between the initial cache read and entering recv() is
        // buffered by the broadcast channel rather than lost.
        let mut rx = svc.app.posture_tx.subscribe();
        tracing::info!(
            asset_id = %asset_id,
            "verifier→fabric posture feed: started (single-local-asset model)"
        );

        // Initial sync: the synchronous startup recalc already populated the
        // cache before this task subscribed, so reflect it once now.
        sync_local_asset_posture(&svc, &asset_id);

        loop {
            match rx.recv().await {
                Ok(_event) => sync_local_asset_posture(&svc, &asset_id),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // A lag only means we may have missed a transition; the
                    // cache is authoritative, so re-sync from it.
                    tracing::warn!(
                        skipped = n,
                        "verifier→fabric posture feed lagged; re-syncing from cache"
                    );
                    sync_local_asset_posture(&svc, &asset_id);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::warn!(
                        "verifier→fabric posture feed: broadcast channel closed; feed stopping"
                    );
                    break;
                }
            }
        }
    });
}

/// One idempotent push of the cached fleet posture into the local asset.
///
/// Fail-closed: a poisoned OR stale cache yields NO push. The actuator gate
/// already fail-closes on a stale fleet posture, so leaving the asset's last
/// good posture in place is correct — we never write a stale or
/// not-yet-computed posture forward. Compare-before-write avoids churn (and a
/// generation bump / propagation pass) when the posture is unchanged.
/// #88 tightening: seed the LOCAL fabric asset fail-closed `LockedOut`.
///
/// `register_asset` seeds every asset `Degraded` (the documented interim) —
/// correct for PEERS, which have no lifting feed (cross-asset propagation only
/// degrades, never lifts, so a `LockedOut` peer would be bricked). But the ONE
/// locally governed asset named by `KIRRA_FABRIC_ASSET_ID` DOES have a lifting
/// feed (`sync_local_asset_posture`), so it can be fail-closed: it starts
/// `LockedOut` and the feed lifts it to a real posture on the first Active
/// recalc. On `PassiveStandby` (no recalc) it correctly stays `LockedOut` until
/// promotion. Call this right after each `register_asset` for the just-
/// registered asset id; it only acts when that id IS the configured local asset.
fn seed_local_asset_lockedout(svc: &ServiceState, registered_id: &str) {
    let local = std::env::var("KIRRA_FABRIC_ASSET_ID").ok();
    let local = local.as_deref().map(str::trim).filter(|s| !s.is_empty());
    seed_local_asset_lockedout_inner(svc, registered_id, local);
}

/// Env-free core of [`seed_local_asset_lockedout`] (testable). Overrides the
/// `Degraded` registration seed with fail-closed `LockedOut` IFF `registered_id`
/// is the configured local asset. A peer (or an unset `local_id`) is left at its
/// `Degraded` seed — peers rely on it.
fn seed_local_asset_lockedout_inner(svc: &ServiceState, registered_id: &str, local_id: Option<&str>) {
    let Some(local_id) = local_id else { return };
    if local_id != registered_id {
        return;
    }
    svc.fabric_router.update_asset_posture(
        local_id,
        AssetPosture {
            asset_id: local_id.to_string(),
            posture: FleetPosture::LockedOut,
            // generation 0 = never-computed sentinel; the feed's first push
            // (>= generation 1) supersedes it, exactly like the register seed.
            generation: 0,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["LOCAL_ASSET_FAILCLOSED_PENDING_FEED".to_string()],
        },
    );
    tracing::info!(
        asset_id = %local_id,
        "local fabric asset seeded fail-closed LockedOut; the verifier→fabric feed lifts it on the first Active recalc"
    );
}

fn sync_local_asset_posture(svc: &ServiceState, asset_id: &str) {
    let now = now_ms();
    let fleet = {
        let guard = match svc.posture_cache.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!(
                    "verifier→fabric feed: posture cache poisoned — skipping push (fail-closed)"
                );
                return;
            }
        };
        match guard.as_ref() {
            Some(c) if !c.is_stale(now) => c.posture.clone(),
            Some(_) => return, // stale → do not propagate a stale posture
            None => return,    // not yet computed
        }
    };

    let current = svc.fabric_router.asset_posture(asset_id);
    if let Some(ref existing) = current {
        if existing.posture == fleet {
            return; // unchanged — nothing to do
        }
    }
    let next_gen = current
        .as_ref()
        .map(|p| p.generation.saturating_add(1))
        .unwrap_or(1);

    let blocked_by = match fleet {
        FleetPosture::Nominal => vec![],
        FleetPosture::Degraded => vec!["VERIFIER_FLEET_POSTURE_DEGRADED".to_string()],
        FleetPosture::LockedOut => vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
    };

    let updated = AssetPosture {
        asset_id: asset_id.to_string(),
        posture: fleet.clone(),
        generation: next_gen,
        computed_at_ms: now,
        contributing_nodes: vec![],
        blocked_by,
    };
    // External-entry update: runs one bounded cross-asset propagation pass so
    // a LockedOut local asset degrades its dependents in the same fabric pass.
    svc.fabric_router
        .update_asset_posture_and_propagate(asset_id, updated);
    tracing::info!(
        asset_id = %asset_id,
        posture = ?fleet,
        generation = next_gen,
        "verifier→fabric posture feed: local asset posture updated"
    );
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
    // #147: the challenge nonce comes from a CSPRNG (OsRng), NEVER the wall
    // clock. A `SystemTime`-derived nonce is predictable and can collide within
    // a single nanosecond; single-use + TTL + node-binding are enforced by the
    // challenge store and the verify-then-consume order in `verify_attestation`.
    let nonce = kirra_runtime_sdk::verifier::generate_challenge_nonce();
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
                // #77 anchor-head high-water mark: detects tail truncation/deletion.
                "head_verified": r.head_verified,
                "head_status": r.head_status,
                // Overall verdict folds in the head check so a truncated chain
                // (rows internally consistent but tail deleted) reads as not-verified.
                "verified": r.chain_intact && r.signature_valid && r.head_verified,
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
    // #79: pass our held fencing token so the durable write re-checks it INSIDE
    // the transaction, closing the gate→commit TOCTOU.
    let held_epoch = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
    match svc.app.store.lock() {
        Ok(mut store) => match store.record_key_rotation(new_signing_key, &req.reason, now_ms(), held_epoch) {
            Ok(_) => Json(json!({ "recorded": true, "event_type": "KEY_ROTATION", "new_key_id": new_key_id })).into_response(),
            Err(DurableWriteError::Fenced(reason)) => {
                // Superseded between the request-path gate and this commit.
                // Mirror the gate: self-demote and reject fail-closed (no write
                // landed). Subsequent mutations hit the standby check above.
                drop(store);
                svc.app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
                tracing::error!(
                    path = "/system/audit/rotate-signing-key",
                    fence = ?reason,
                    "FENCED at top-tier write (in-transaction epoch re-check) — self-demoting to PassiveStandby and rejecting"
                );
                (StatusCode::SERVICE_UNAVAILABLE,
                 Json(json!({ "error": "fenced: epoch superseded; instance demoted to passive standby" }))).into_response()
            }
            Err(DurableWriteError::Db(_)) => (StatusCode::INTERNAL_SERVER_ERROR,
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
        Ok(mut result) => {
            // SG-012 / H-011: a DNP3 broadcast (only DNP3 sets `is_broadcast`)
            // must carry a tamper-evident record; mirror the dedicated DNP3
            // handler's fail-closed policy on this generic path too.
            let is_broadcast = result.adapter_details
                .get("is_broadcast").and_then(|v| v.as_bool()).unwrap_or(false);
            let should_audit = !result.allowed || is_broadcast;

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
                let audit_ok = match svc.app.store.lock() {
                    Ok(mut store) => match store.save_posture_event_chained(
                        "industrial_adapter", event_type,
                        &audit.to_string(), None, now_ms(),
                    ) {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::error!(error = %e, event_type = event_type,
                                "AUDIT-CHAIN WRITE FAILED for industrial adapter event — event missing from tamper-evident log");
                            false
                        }
                    },
                    Err(_) => {
                        tracing::error!(event_type = event_type,
                            "industrial adapter: store lock poisoned — audit write SKIPPED for this event");
                        false
                    }
                };

                // TR-012a: a broadcast whose mandatory audit could not be
                // written is BLOCKED (fail-closed); non-broadcast audit failure
                // stays non-fatal (TR-012b).
                if is_broadcast && !audit_ok {
                    result.allowed = false;
                    result.denial_reason = Some("DNP3_BROADCAST_AUDIT_UNAVAILABLE".to_string());
                    tracing::error!(
                        "DNP3 broadcast BLOCKED (unified path) — mandatory audit write unavailable (SG-012 / H-011 fail-closed)");
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

    // #84: resolve the CANopen bus node-id to a FLEET node so an NMT-offline
    // event marks the correct asset and the recalc is EFFECTFUL. Unmapped or
    // unregistered ids are FAIL-CLOSED — surfaced as an unattributed offline
    // (distinct audit event + warning + response flag), never a silent no-op.
    let offline_outcome = if eval.triggers_recalculation {
        use kirra_runtime_sdk::adapters::canopen::{classify_nmt_offline, global_resolve};
        let resolved = global_resolve(eval.node_id);
        let registered = resolved
            .as_deref()
            .map(|n| svc.app.nodes.contains_key(n))
            .unwrap_or(false);
        Some(classify_nmt_offline(eval.node_id, resolved, registered))
    } else {
        None
    };

    // Apply the offline effect (mark the node + drive a recalc). The fleet node
    // actually marked offline (if any) is recorded for the audit + response.
    let mut attributed_fleet_node: Option<String> = None;
    if let Some(outcome) = &offline_outcome {
        use kirra_runtime_sdk::adapters::canopen::{NmtOfflineOutcome, UnattributedReason};
        use kirra_runtime_sdk::posture_engine_v2::PostureRecalcTrigger;
        match outcome {
            NmtOfflineOutcome::Attributed { fleet_node_id } => {
                match svc.app.mark_node_untrusted(fleet_node_id, "CANOPEN_NMT_OFFLINE", now_ms()) {
                    Ok(true) => {
                        attributed_fleet_node = Some(fleet_node_id.clone());
                        tracing::warn!(
                            canopen_node_id = eval.node_id,
                            fleet_node_id = %fleet_node_id,
                            "CANopen NMT node-offline → fleet node marked Untrusted; effectful recalc enqueued"
                        );
                        enqueue_recalc(&svc, PostureRecalcTrigger::NodeTrustChanged {
                            node_id: fleet_node_id.clone(),
                            reason: "CANOPEN_NMT_OFFLINE".to_string(),
                        });
                    }
                    // Mapping raced a deregistration, or the store write failed:
                    // fail-closed exactly like an unattributed offline.
                    Ok(false) | Err(()) => {
                        tracing::error!(
                            canopen_node_id = eval.node_id,
                            fleet_node_id = %fleet_node_id,
                            "CANopen NMT node-offline: mapped node missing or store write failed — \
                             treating as UNATTRIBUTED (fail-closed)"
                        );
                        enqueue_recalc(&svc, PostureRecalcTrigger::DependencyGraphChanged);
                    }
                }
            }
            NmtOfflineOutcome::Unattributed { canopen_node_id, reason } => {
                let reason_str = match reason {
                    UnattributedReason::NoMapping => "NO_MAPPING",
                    UnattributedReason::NodeNotRegistered => "NODE_NOT_REGISTERED",
                };
                tracing::warn!(
                    canopen_node_id = *canopen_node_id,
                    source_node = %msg.source_node,
                    reason = reason_str,
                    "CANopen NMT node-offline UNATTRIBUTED — recorded as unattributed offline; \
                     recalc enqueued (fail-closed, never silently dropped)"
                );
                enqueue_recalc(&svc, PostureRecalcTrigger::DependencyGraphChanged);
            }
        }
    }

    if !allowed || eval.triggers_recalculation {
        match svc.app.store.lock() {
            Ok(mut store) => {
                let event_type = if eval.triggers_recalculation {
                    if attributed_fleet_node.is_some() {
                        "CANOPEN_NMT_NODE_OFFLINE"
                    } else {
                        "CANOPEN_NMT_OFFLINE_UNATTRIBUTED"
                    }
                } else {
                    "INDUSTRIAL_ACTION_DENIED"
                };
                if let Err(e) = store.save_posture_event_chained(
                    "canopen_adapter", event_type,
                    &json!({
                        "node_id": eval.node_id,
                        "fleet_node_id": attributed_fleet_node.clone(),
                        "node_offline_attributed": attributed_fleet_node.is_some(),
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

    Json(json!({
        "protocol": "canopen",
        "command": format!("{:?}", eval.command),
        "allowed": allowed,
        "denial_reason": denial_reason,
        "posture_at_evaluation": posture_str,
        // #84: whether the NMT-offline was attributed to a fleet node (and which).
        // `false` on an offline event means the id was unmapped/unregistered and
        // handled fail-closed — surfaced here so callers never read silence as success.
        "node_offline_attributed": attributed_fleet_node.is_some(),
        "fleet_node_id": attributed_fleet_node,
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

    // SG-012 / H-011 — a DNP3 broadcast control command must carry a
    // tamper-evident record. Kirra CLASSIFIES, it does not actuate: "before
    // control output" means before this handler returns its verdict, and the
    // integrator MUST NOT actuate ahead of this audited verdict (a documented
    // assumption-of-use). Audit policy:
    //   * Broadcast (TR-012 / TR-012a): MUST be audited; if the mandatory audit
    //     write fails (or the store lock is poisoned), the command is BLOCKED
    //     (fail-closed) — H-011's hazard is a broadcast control executed without
    //     a tamper-evident record.
    //   * Unicast control (TR-012b): also audited, but an audit-write failure is
    //     NON-fatal — for a single target the enforcement decision outranks the
    //     record (blocking on a transient disk error would be fail-open).
    //   * Denials: audited.
    let mut allowed = allowed;
    let mut denial_reason = denial_reason;
    let mut status = StatusCode::OK;

    if eval.is_broadcast || eval.is_control || !allowed {
        let event_type = if eval.is_broadcast {
            "DNP3_BROADCAST_COMMAND"
        } else if !allowed {
            "INDUSTRIAL_ACTION_DENIED"
        } else {
            "DNP3_CONTROL_COMMAND"
        };
        let audit_payload = json!({
            "function_name": eval.function_name,
            "is_broadcast": eval.is_broadcast,
            "is_control": eval.is_control,
            "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
            "dest_address": msg.dest_address,
            "posture": posture_str,
            "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
        })
        .to_string();
        let audit_ok = match svc.app.store.lock() {
            Ok(mut store) => match store.save_posture_event_chained(
                "dnp3_adapter", event_type, &audit_payload, None, now_ms(),
            ) {
                Ok(()) => true,
                Err(e) => {
                    tracing::error!(error = %e, event_type = event_type,
                        "AUDIT-CHAIN WRITE FAILED for dnp3 adapter event — event missing from tamper-evident log");
                    false
                }
            },
            Err(_) => {
                tracing::error!(event_type = event_type,
                    "dnp3 adapter: store lock poisoned — audit write SKIPPED for this event");
                false
            }
        };

        // TR-012a: a BROADCAST whose mandatory audit could not be written is
        // BLOCKED (fail-closed). Unicast audit failure is non-fatal (TR-012b).
        if eval.is_broadcast && !audit_ok {
            allowed = false;
            denial_reason = Some("DNP3_BROADCAST_AUDIT_UNAVAILABLE".to_string());
            status = StatusCode::SERVICE_UNAVAILABLE;
            tracing::error!(dest_address = msg.dest_address,
                "DNP3 BROADCAST control BLOCKED — mandatory audit write unavailable (SG-012 / H-011 fail-closed)");
        }
    }

    (status, Json(json!({
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
    }))).into_response()
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

    // #79: held fencing token, read before locking the store. The durable commit
    // re-checks it INSIDE the transaction, closing the gate→commit TOCTOU.
    let held_epoch = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);

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

    match store.save_federated_report_chained(&report, received_at_ms, held_epoch) {
        Ok(()) => Json(evaluation).into_response(),
        Err(DurableWriteError::Fenced(reason)) => {
            // Superseded between the request-path gate and this commit. Mirror
            // the gate: self-demote and reject fail-closed — the report was NOT
            // persisted and the nonce was NOT burned (transaction dropped).
            drop(store);
            svc.app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                path = "/federation/reports/submit",
                fence = ?reason,
                "FENCED at top-tier write (in-transaction epoch re-check) — self-demoting to PassiveStandby and rejecting"
            );
            (StatusCode::SERVICE_UNAVAILABLE,
             Json(json!({ "error": "fenced: epoch superseded; instance demoted to passive standby" }))).into_response()
        }
        Err(DurableWriteError::Db(_)) => (StatusCode::INTERNAL_SERVER_ERROR,
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
    // #88: if this IS the configured local asset, override the Degraded seed
    // with fail-closed LockedOut (the feed lifts it); a no-op for peers.
    seed_local_asset_lockedout(&svc, &req.asset_id);
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
    // SG-007: propagate AND record each rule-firing to the causal log (decisions
    // unchanged). Use the current fabric generation for the recorded events.
    let fabric_generation = svc.fabric_router.fabric_state().fabric_generation;
    let changes = svc.fabric_router.propagate_and_record(&svc.fabric_causal_log, fabric_generation);
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
            use kirra_runtime_sdk::gateway::kinematics_contract::EnforceAction;
            let action_str = format!("{:?}", action);
            let now = now_ms();
            let fabric_generation = svc.fabric_router.fabric_state().fabric_generation;
            let clamp_occurred =
                matches!(action, EnforceAction::ClampLinear(_) | EnforceAction::ClampSteering(_));

            // #86: APPLY the verdict server-side and return the ENFORCED command.
            // `apply_enforce_action` substitutes the safe clamp value(s); a clamp
            // therefore lands in the response's `command` field, so a client using
            // it is within envelope even if it ignores the `action` label.
            //
            // FAIL-CLOSED: deny when no enforced command can be produced —
            // `DenyBreach`, OR (defensively) a clamp whose enforced value is
            // non-finite. We NEVER return the unclamped command.
            let enforced = kirra_runtime_sdk::kinematics_sim::apply_enforce_action(&cmd, &action)
                .filter(|c| c.linear_velocity_mps.is_finite() && c.steering_angle_deg.is_finite());

            match enforced {
                None => {
                    // `DenyCode -> &'static str` keeps this path alloc-free; the previous
                    // `r.clone()` of a `String` allocated per denial (S3 / #115).
                    let denial_reason: &'static str = match &action {
                        EnforceAction::DenyBreach(c) => c.reason(),
                        // Clamp produced a non-finite enforced value (contract bug):
                        // fail closed rather than forward an invalid command.
                        _ => "ENFORCED_COMMAND_UNPRODUCIBLE",
                    };
                    svc.fabric_causal_log.record(
                        &asset_id,
                        "COMMAND_DENIED",
                        &json!({"reason": denial_reason, "command": serde_json::to_value(&cmd).unwrap_or_default()}).to_string(),
                        vec![],
                        vec![],
                        fabric_generation,
                    );
                    if let Ok(mut store) = svc.app.store.lock() {
                        let _ = store.save_posture_event_chained(
                            &asset_id, "FABRIC_COMMAND_DENIED",
                            &json!({"asset_id": asset_id, "action": action_str}).to_string(),
                            None, now,
                        );
                    }
                    Json(json!({
                        "asset_id": asset_id,
                        "action": action_str,
                        "allowed": false,
                        "clamp_occurred": false,
                        "denial_reason": denial_reason,
                    })).into_response()
                }
                Some(enforced_cmd) => {
                    // A clamp is safety ENFORCEMENT, not a silent pass: record it
                    // (causal log + tamper-evident audit) with the original-vs-enforced
                    // values, mirroring the deny path and the actuator-handler pattern.
                    if clamp_occurred {
                        let enforcement = json!({
                            "asset_id": asset_id,
                            "action": action_str,
                            "clamp_occurred": true,
                            "original_linear_velocity_mps": cmd.linear_velocity_mps,
                            "original_steering_angle_deg": cmd.steering_angle_deg,
                            "enforced_linear_velocity_mps": enforced_cmd.linear_velocity_mps,
                            "enforced_steering_angle_deg": enforced_cmd.steering_angle_deg,
                        });
                        svc.fabric_causal_log.record(
                            &asset_id,
                            "COMMAND_CLAMPED",
                            &enforcement.to_string(),
                            vec![],
                            vec![],
                            fabric_generation,
                        );
                        if let Ok(mut store) = svc.app.store.lock() {
                            let _ = store.save_posture_event_chained(
                                &asset_id, "FABRIC_COMMAND_CLAMPED",
                                &enforcement.to_string(),
                                None, now,
                            );
                        }
                    }

                    Json(json!({
                        "asset_id": asset_id,
                        "action": action_str,
                        "allowed": true,
                        "clamp_occurred": clamp_occurred,
                        "original_linear_velocity_mps": cmd.linear_velocity_mps,
                        "original_steering_angle_deg": cmd.steering_angle_deg,
                        "enforced_linear_velocity_mps": enforced_cmd.linear_velocity_mps,
                        "enforced_steering_angle_deg": enforced_cmd.steering_angle_deg,
                        // AUTHORITATIVE output: the enforced (post-clamp) command.
                        "command": enforced_cmd,
                    })).into_response()
                }
            }
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
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn handle_fabric_causal_log(
    State(svc): State<Arc<ServiceState>>,
    Query(q): Query<CausalLogQuery>,
) -> impl IntoResponse {
    let from = q.from_ms.unwrap_or(0);
    let to = q.to_ms.unwrap_or(u64::MAX);
    // #87: bounded + paginated. `limit` is clamped to CAUSAL_EXPORT_MAX_PAGE
    // inside export_page so a forensic export is never unbounded.
    let limit = q
        .limit
        .unwrap_or(kirra_runtime_sdk::fabric::causal_log::CAUSAL_EXPORT_MAX_PAGE);
    let offset = q.offset.unwrap_or(0);
    let entries = svc.fabric_causal_log.export_page(from, to, limit, offset);
    let total = entries.len();
    Json(json!({"entries": entries, "total": total, "limit": limit, "offset": offset})).into_response()
}

/// #87: admin-gated verification of the causal-log forensic chain. Mirrors
/// `/system/audit/verify`. Mounted at `/system/audit/causal/verify` (NOT under
/// `/fabric/causal-log/...`, to avoid colliding with the `{entry_id}` wildcard).
async fn verify_causal_chain(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    let vk = svc.audit_verifying_key.as_ref();
    match svc.app.store.lock() {
        Ok(store) => match store.verify_causal_chain_integrity(vk) {
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
                "head_verified": r.head_verified,
                "head_status": r.head_status,
                "verified": r.chain_intact && r.signature_valid && r.head_verified,
            })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "causal chain query failed" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
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

// ---------------------------------------------------------------------------
// #46 — systemd `Type=notify` integration: READY notification + watchdog.
//
// READY=1 is sent once the listener is bound (the service is ready to accept);
// WATCHDOG=1 is pinged from the posture-engine liveness signal so a hung-but-
// alive process (dead posture worker → stale cache) misses the ping and systemd
// restarts it (fail-closed). No new dependency — sd_notify is a single line
// written to the `$NOTIFY_SOCKET` datagram socket. Every path is a best-effort
// no-op when not run under systemd (env vars unset).
// ---------------------------------------------------------------------------

/// Send one sd_notify message (e.g. `"READY=1"`, `"WATCHDOG=1"`) to the socket
/// at `socket` (a filesystem path, or an abstract socket when it begins with
/// `@`). Separated from [`sd_notify`] so it is testable without mutating
/// `NOTIFY_SOCKET` in-process (INVARIANT #13).
fn sd_notify_to(socket: &std::ffi::OsStr, message: &str) -> std::io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    let sock = UnixDatagram::unbound()?;
    let path_str = socket.to_string_lossy();
    if let Some(name) = path_str.strip_prefix('@') {
        // Linux abstract namespace socket (leading NUL).
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;
        let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
        sock.connect_addr(&addr)?;
    } else {
        sock.connect(std::path::Path::new(socket))?;
    }
    sock.send(message.as_bytes())?;
    Ok(())
}

/// Best-effort sd_notify. No-op when `NOTIFY_SOCKET` is unset (not run under
/// `Type=notify`); a send failure is logged, never fatal.
fn sd_notify(message: &str) {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(e) = sd_notify_to(&socket, message) {
        tracing::warn!(error = %e, message, "sd_notify: send failed");
    }
}

/// Whether the watchdog should ping this tick. On the Active node the ping is
/// GATED on posture-engine liveness (a fresh cache — the refresh loop restamps
/// it each interval; a hung worker lets it go stale → ping withheld → systemd
/// restarts, fail-closed). PassiveStandby has no posture engine by design, so
/// it pings as a plain keepalive (its liveness is the promotion monitor).
fn watchdog_should_ping(is_active: bool, cache_fresh: bool) -> bool {
    if is_active {
        cache_fresh
    } else {
        true
    }
}

/// Spawn the systemd watchdog keepalive (#46). No-op unless `WATCHDOG_USEC` is
/// set (i.e. the unit declares `WatchdogSec=`). Pings `WATCHDOG=1` at half the
/// configured interval, gated by [`watchdog_should_ping`].
fn spawn_systemd_watchdog(svc: Arc<ServiceState>) {
    let usec: u64 = match std::env::var("WATCHDOG_USEC").ok().and_then(|v| v.parse().ok()) {
        Some(u) if u > 0 => u,
        _ => return, // no WatchdogSec configured → nothing to feed
    };
    let period = std::time::Duration::from_micros(usec / 2); // systemd-recommended margin
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tracing::info!(watchdog_usec = usec, "systemd watchdog keepalive started");
        loop {
            tick.tick().await;
            let cache_fresh = svc
                .posture_cache
                .read()
                .ok()
                .and_then(|g| g.as_ref().map(|c| !c.is_stale(now_ms())))
                .unwrap_or(false);
            if watchdog_should_ping(svc.app.is_active(), cache_fresh) {
                sd_notify("WATCHDOG=1");
            } else {
                tracing::error!(
                    "systemd watchdog: Active posture engine appears stalled (cache stale) — \
                     withholding WATCHDOG ping; systemd will restart (SG-003 / SG9 fail-closed)"
                );
            }
        }
    });
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

    // #84: load the CANopen-node-id → fleet-node-id map from config so an NMT
    // node-offline event marks the correct asset (effectful recalc). Sourced
    // from KIRRA_CANOPEN_NODE_MAP; unset → empty map (every offline is then
    // unattributed, handled fail-closed in evaluate_canopen_adapter).
    kirra_runtime_sdk::adapters::canopen::init_node_map_from_env();

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
    // #87: the causal log persists to the SAME store the rest of the service
    // uses, so forensic causal rows land in the production DB and chain there.
    let causal_store = Arc::clone(&app_state.store);
    let svc_state = Arc::new(ServiceState {
        app: app_state,
        posture_cache: Arc::new(std::sync::RwLock::new(None)),
        audit_verifying_key,
        fabric_router: Arc::new(FabricRouter::new()),
        fabric_telemetry: Arc::new(FabricTelemetry::new()),
        fabric_causal_log: Arc::new(FabricCausalLog::new(causal_store, signing_key)),
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
                    // #88: the local fed asset is fail-closed LockedOut (peers
                    // keep the Degraded seed); a no-op for every peer.
                    seed_local_asset_lockedout(&svc_state, &asset.asset_id);
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
        // Anchor-head backfill (#77): a chain written by a pre-#77 binary has no
        // signed head; sign one from the current tail so an upgraded store
        // presents a head BEFORE serving /system/audit/verify (no false
        // HEAD_ABSENT). Idempotent. Log-and-continue: a missing head is itself
        // caught fail-closed at verify time (head_verified = false).
        match svc_state.app.store.lock() {
            Ok(mut store) => match store.ensure_audit_anchor_head(now_ms()) {
                Ok(()) => tracing::info!("audit: anchor-head high-water mark ensured"),
                Err(e) => tracing::error!(
                    error = %e,
                    "audit: anchor-head high-water mark FAILED at startup"
                ),
            },
            Err(_) => tracing::error!(
                "audit: anchor-head high-water mark skipped — store lock poisoned at startup"
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

        // #88: verifier→fabric posture feed (single-local-asset model).
        // Mirrors this controller's aggregate FleetPosture into the fabric
        // posture for the one locally governed asset, so fabric command
        // enforcement reflects real verified trust instead of the interim
        // registration seed. Spawned AFTER the initial recalc so the cache
        // is populated for the feed's initial sync.
        spawn_local_asset_posture_feed(Arc::clone(&svc_state));
    } else {
        tracing::info!(
            "posture: freshness wiring skipped — passive standby (no recalc/worker/refresh)"
        );
    }

    // Assemble the production router. Extracted into `build_app` (issue #72)
    // so the EXACT assembled router — identical routes, middleware layer
    // order, and state wiring — is what the binary-internal posture-gate
    // test exercises, rather than a representative stand-in.
    let app = build_app(Arc::clone(&svc_state));

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

    // #46: the listener is bound and startup invariants passed (SG-008) — tell
    // systemd we are READY (Type=notify) and start the watchdog keepalive
    // (gated on posture-engine liveness; fail-closed on a stalled engine). Both
    // are no-ops outside a `Type=notify` / `WatchdogSec=` unit.
    sd_notify("READY=1");
    spawn_systemd_watchdog(Arc::clone(&svc_state));

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

/// Assembles the complete production router from a fully-initialized
/// `ServiceState`. Extracted verbatim from `main()` (issue #72) so the EXACT
/// assembled router can be exercised by the binary-internal posture-gate test
/// below — not a representative stand-in.
///
/// SECURITY-CRITICAL: the route groups, the per-group auth/envelope layers, and
/// especially the OUTERMOST `enforce_posture_routing` gate must remain in this
/// exact order. The posture gate is the last `.layer(...)` so it runs FIRST on
/// every request (before auth and the actuator envelope); the identity/admin/
/// actuator-posture layering inside each group is the fail-closed security
/// boundary. Do not reorder, drop, or rewire any of this.
fn build_app(svc_state: Arc<ServiceState>) -> Router {
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
        .route("/system/audit/causal/verify", get(verify_causal_chain))
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

    Router::new()
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
        ))
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

// ---------------------------------------------------------------------------
// Issue #72 — posture gate is wired on the REAL assembled router.
//
// The external `tests/posture_gate_integration.rs` builds a *representative*
// router (stub handlers at the production paths) precisely because, as an
// out-of-crate integration test, it cannot see the binary's inline assembly.
// That left a residual gap: nothing asserted the gate is mounted on the
// router `main()` actually serves. These tests close it by driving requests
// through `build_app()` — the exact production assembly — and proving the
// posture gate (and its exemptions) are in force on it.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod posture_gate_real_router_tests {
    use super::build_app;

    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    use kirra_runtime_sdk::posture_cache::{
        CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    /// Builds an Active `ServiceState` with the given seeded posture (or a
    /// cold cache when `None`), mirroring the production field set.
    fn build_state(initial: Option<CachedFleetPosture>) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(initial));
        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    fn state_with(posture: FleetPosture) -> Arc<ServiceState> {
        build_state(Some(CachedFleetPosture::new(posture)))
    }

    /// Drives one request through the REAL assembled app and returns its status.
    /// A fresh app per call because `oneshot` consumes the router.
    async fn status_through_real_app(svc: Arc<ServiceState>, method: &str, path: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .expect("build request");
        build_app(svc)
            .oneshot(req)
            .await
            .expect("router service should not panic")
            .status()
    }

    /// LockedOut blocks a functional READ on the production router — proving
    /// the gate is mounted on the real assembly, not just the test stand-in.
    #[tokio::test]
    async fn lockedout_blocks_read_on_real_router() {
        let status =
            status_through_real_app(state_with(FleetPosture::LockedOut), "GET", "/fleet/posture").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "the real assembled router must deny GET /fleet/posture under LockedOut; got {status}"
        );
    }

    /// Posture-dependence on the SAME route + real handler: under Nominal the
    /// gate steps aside and the production `get_fleet_posture` handler returns
    /// 200 (empty fleet). The LockedOut→503 / Nominal→200 contrast is what
    /// proves it is the posture gate — not a blanket 503 — that is wired in.
    #[tokio::test]
    async fn nominal_passes_read_through_to_real_handler() {
        let status =
            status_through_real_app(state_with(FleetPosture::Nominal), "GET", "/fleet/posture").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the real router must let GET /fleet/posture reach the handler under Nominal; got {status}"
        );
    }

    /// The safety-critical actuator WRITE is denied under LockedOut on the real
    /// router. The posture gate is the outermost layer, so it returns 503
    /// before the admin-token / envelope layers ever run.
    #[tokio::test]
    async fn lockedout_blocks_actuator_write_on_real_router() {
        let status = status_through_real_app(
            state_with(FleetPosture::LockedOut),
            "POST",
            "/actuator/motion/command",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "the real router must deny POST /actuator/motion/command under LockedOut; got {status}"
        );
    }

    /// Exemption wiring on the real assembly: `/health` stays reachable under
    /// LockedOut (liveness is allowlisted by `is_posture_exempt`).
    #[tokio::test]
    async fn health_exempt_under_lockedout_on_real_router() {
        let status =
            status_through_real_app(state_with(FleetPosture::LockedOut), "GET", "/health").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "/health must remain reachable under LockedOut on the real router (exempt); got {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// #88: verifier→fabric posture feed (single-local-asset model).
//
// Exercises `sync_local_asset_posture` directly (the env-gated spawn wrapper
// is thin): a registered local asset's fabric posture must track the cached
// fleet posture, fail closed on a stale cache, avoid churn when unchanged,
// and run the bounded cross-asset propagation pass.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod fabric_posture_feed_tests {
    use super::sync_local_asset_posture;

    use std::sync::Arc;

    use kirra_runtime_sdk::fabric::asset::{
        AssetType, FabricAsset, KinematicProfileType,
    };
    use kirra_runtime_sdk::fabric::router::FabricRouter;
    use kirra_runtime_sdk::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    const LOCAL: &str = "local-asset";

    fn asset(id: &str) -> FabricAsset {
        let now = now_ms();
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::RobotNominal,
            registered_at_ms: now,
            last_seen_ms: now,
            metadata: Default::default(),
        }
    }

    /// Builds an Active `ServiceState` whose cache holds `cached` and whose
    /// fabric router has `LOCAL` registered (seeded Degraded).
    fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
        let fabric_router = Arc::new(FabricRouter::new());
        fabric_router.register_asset(&asset(LOCAL));
        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    /// A FRESH cache entry (generated now) carrying `posture`.
    fn fresh(posture: FleetPosture) -> CachedFleetPosture {
        CachedFleetPosture::new_with_generation(posture, 1, now_ms())
    }

    #[test]
    fn fresh_cache_pushes_fleet_posture_to_local_asset() {
        let svc = state(Some(fresh(FleetPosture::Nominal)));
        // Seeded Degraded by register_asset.
        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded
        );

        sync_local_asset_posture(&svc, LOCAL);

        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::Nominal, "feed must mirror the fleet posture");
        assert!(after.blocked_by.is_empty(), "Nominal carries no blocked_by reason");
        assert_eq!(after.generation, 1, "seed gen 0 → first feed write gen 1");
    }

    #[test]
    fn stale_cache_does_not_push_keeps_last_good() {
        // generated_at far in the past → is_stale(now) == true.
        let stale = CachedFleetPosture::new_with_generation(
            FleetPosture::Nominal,
            7,
            now_ms().saturating_sub(60_000),
        );
        let svc = state(Some(stale));

        sync_local_asset_posture(&svc, LOCAL);

        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded,
            "a stale cache must NOT propagate forward (fail-closed): seed is kept"
        );
    }

    #[test]
    fn empty_cache_does_not_push() {
        let svc = state(None);
        sync_local_asset_posture(&svc, LOCAL);
        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded,
            "a not-yet-computed cache must not overwrite the seed"
        );
    }

    #[test]
    fn unchanged_posture_does_not_bump_generation() {
        // Seed is Degraded; feeding Degraded again must be a no-op.
        let svc = state(Some(fresh(FleetPosture::Degraded)));
        let gen_before = svc.fabric_router.asset_posture(LOCAL).unwrap().generation;

        sync_local_asset_posture(&svc, LOCAL);

        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::Degraded);
        assert_eq!(
            after.generation, gen_before,
            "an unchanged posture must not bump the generation (no churn)"
        );
    }

    #[test]
    fn lockedout_fleet_posture_locks_the_local_asset() {
        let svc = state(Some(fresh(FleetPosture::LockedOut)));
        sync_local_asset_posture(&svc, LOCAL);
        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::LockedOut);
        assert_eq!(
            after.blocked_by,
            vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
            "LockedOut feed must tag the reason for operators"
        );
    }
}

// ---------------------------------------------------------------------------
// #86 — the fabric command endpoint is AUTHORITATIVE: it applies the clamp
// server-side and returns the ENFORCED command (closing the prior fail-open
// where a clamp was reported but not applied). These tests drive the handler
// directly (no auth/router), asserting the response `command` carries the safe
// values, that a clamp is reported, Allow is unchanged, and Deny is denied.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod fabric_command_authoritative_tests {
    use super::handle_fabric_command;

    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::Json;

    use kirra_runtime_sdk::fabric::asset::{
        AssetPosture, AssetType, FabricAsset, KinematicProfileType,
    };
    use kirra_runtime_sdk::fabric::router::FabricRouter;
    use kirra_runtime_sdk::gateway::kinematics_contract::ProposedVehicleCommand;
    use kirra_runtime_sdk::posture_cache::{ServiceState, SharedPostureCache};
    use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    const ASSET: &str = "av-01";

    fn svc_with_asset(posture: FleetPosture) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        let fabric_router = Arc::new(FabricRouter::new());

        let asset = FabricAsset {
            asset_id: ASSET.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: ASSET.to_string(),
            kinematic_profile: KinematicProfileType::AutomotiveNominal,
            registered_at_ms: 0,
            last_seen_ms: 0,
            metadata: Default::default(),
        };
        fabric_router.register_asset(&asset);
        // route_command reads the asset's fabric posture; set the one under test.
        fabric_router.update_asset_posture(
            ASSET,
            AssetPosture {
                asset_id: ASSET.to_string(),
                posture,
                generation: 1,
                computed_at_ms: 0,
                contributing_nodes: vec![],
                blocked_by: vec![],
            },
        );

        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    async fn post_command(svc: Arc<ServiceState>, cmd: ProposedVehicleCommand) -> serde_json::Value {
        let resp = handle_fabric_command(State(svc), Path(ASSET.to_string()), Ok(Json(cmd)))
            .await
            .into_response();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn cmd(linear: f64, current: f64, steering: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: linear,
            current_velocity_mps: current,
            delta_time_s: 0.1,
            steering_angle_deg: steering,
            current_steering_angle_deg: steering,
        }
    }

    #[tokio::test]
    async fn clamped_command_response_carries_enforced_values_within_envelope() {
        // 40 m/s exceeds the AutomotiveNominal envelope → ClampLinear.
        let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(40.0, 34.0, 0.0)).await;

        assert_eq!(v["allowed"], true);
        assert_eq!(v["clamp_occurred"], true, "a clamp must be reported as enforcement");
        assert_eq!(v["original_linear_velocity_mps"], 40.0);

        let enforced = v["enforced_linear_velocity_mps"].as_f64().expect("enforced velocity");
        assert!(enforced < 40.0, "enforced velocity must be clamped below the proposal (within envelope)");

        // THE KEY ASSERTION: the authoritative `command` carries the SAFE value,
        // so a client applying it is within envelope even ignoring `action`.
        let cmd_v = v["command"]["linear_velocity_mps"].as_f64().expect("command.linear");
        assert_eq!(cmd_v, enforced, "response.command must carry the enforced (clamped) velocity");
        assert!(cmd_v < 40.0, "the returned command is NOT the unclamped 40.0");
    }

    #[tokio::test]
    async fn allow_returns_command_unchanged() {
        // current == proposed → no rate-of-change clamp; within envelope → Allow.
        let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(10.0, 10.0, 1.0)).await;
        assert_eq!(v["allowed"], true);
        assert_eq!(v["clamp_occurred"], false);
        assert_eq!(v["command"]["linear_velocity_mps"].as_f64().unwrap(), 10.0);
        assert_eq!(v["command"]["steering_angle_deg"].as_f64().unwrap(), 1.0);
    }

    #[tokio::test]
    async fn lockedout_denies_and_omits_command() {
        let v = post_command(svc_with_asset(FleetPosture::LockedOut), cmd(10.0, 10.0, 0.0)).await;
        assert_eq!(v["allowed"], false, "LockedOut denies the command");
        assert!(v.get("command").is_none(), "a denied command carries no enforced command");
        assert!(v["denial_reason"].is_string(), "denial is recorded with a reason");
    }
}

// ---------------------------------------------------------------------------
// #147 — attestation nonce lifecycle: VERIFY-THEN-CONSUME at the handler.
//
// The crypto (verify_attestation_proof) and the store invariants (single-use,
// TTL, node-binding, CSPRNG) are tested in attestation.rs / verifier.rs. This
// proves the remaining handler-level invariant: a FAILED proof must NOT burn
// the pending nonce, so an attacker cannot force nonce exhaustion — the
// legitimate node can still attest with the same outstanding nonce.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod attestation_nonce_handler_tests {
    use super::{verify_attestation, VerifyAttestationRequest};

    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

    use kirra_runtime_sdk::attestation::attestation_signing_payload;
    use kirra_runtime_sdk::posture_cache::{now_ms, ServiceState, SharedPostureCache};
    use kirra_runtime_sdk::verifier::{
        AppState, NodeTrustState, RegisteredNode, VerifierOperationMode,
    };
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    const NODE: &str = "edge-node-1";

    /// Test-only Ed25519 SubjectPublicKeyInfo PEM (RFC 8410 prefix; public key only).
    fn public_key_to_pem(vk: &VerifyingKey) -> String {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        const ED25519_SPKI_PREFIX: [u8; 12] =
            [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(vk.as_bytes());
        format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", B64.encode(&der))
    }

    fn sign_proof(sk: &SigningKey, node_id: &str, nonce: u64) -> String {
        hex::encode(sk.sign(&attestation_signing_payload(node_id, nonce)).to_bytes())
    }

    fn svc_with_registered_node(ak_pem: String) -> Arc<ServiceState> {
        let app = Arc::new(AppState::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
            VerifierOperationMode::Active,
        ));
        app.persist_and_insert_node(RegisteredNode {
            node_id: NODE.to_string(),
            status: NodeTrustState::Unknown,
            registered_at_ms: 1,
            last_trust_update_ms: 0,
            ak_public_pem: Some(ak_pem),
            expected_pcr16_digest_hex: None,
        })
        .expect("register node");

        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    async fn verify(svc: Arc<ServiceState>, nonce: u64, proof_hex: String) -> StatusCode {
        let req: VerifyAttestationRequest = serde_json::from_value(serde_json::json!({
            "node_id": NODE, "nonce": nonce, "proof_hex": proof_hex,
        }))
        .expect("build request");
        verify_attestation(State(svc), Json(req)).await.into_response().status()
    }

    #[tokio::test]
    async fn failed_proof_does_not_burn_the_nonce_then_valid_proof_succeeds() {
        let node_key = SigningKey::from_bytes(&[7u8; 32]);
        let attacker_key = SigningKey::from_bytes(&[9u8; 32]); // not the registered AK
        let svc = svc_with_registered_node(public_key_to_pem(&node_key.verifying_key()));

        let nonce = 0xABCD_1234_5678_9F01;
        svc.app.issue_challenge(NODE, nonce, now_ms());

        // 1) A bad proof (signed by the wrong key) is rejected 401 — and the
        //    pending nonce is NOT consumed (verify-then-consume).
        let bad = verify(Arc::clone(&svc), nonce, sign_proof(&attacker_key, NODE, nonce)).await;
        assert_eq!(bad, StatusCode::UNAUTHORIZED, "a bad proof is refused");
        assert!(
            svc.app.pending_challenges.contains_key(NODE),
            "a FAILED proof must not burn the pending nonce"
        );

        // 2) The legitimate node, with the SAME outstanding nonce, now succeeds.
        let good = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
        assert_eq!(good, StatusCode::OK, "valid proof over the still-outstanding nonce attests");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
            "node becomes Trusted after a valid proof"
        );

        // 3) Single-use: the nonce is now consumed; a replay is a 409 conflict.
        let replay = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
        assert_eq!(replay, StatusCode::CONFLICT, "the consumed nonce cannot be replayed");
    }
}

// ---------------------------------------------------------------------------
// #88 tightening — the LOCAL fed asset is seeded fail-closed LockedOut; PEERS
// keep the Degraded interim seed; the feed lifts the local asset on recalc.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod local_asset_lockedout_seed_tests {
    use super::{seed_local_asset_lockedout_inner, sync_local_asset_posture};

    use std::sync::Arc;

    use kirra_runtime_sdk::fabric::asset::{AssetType, FabricAsset, KinematicProfileType};
    use kirra_runtime_sdk::fabric::router::FabricRouter;
    use kirra_runtime_sdk::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    const LOCAL: &str = "av-local";
    const PEER: &str = "av-peer";

    fn asset(id: &str) -> FabricAsset {
        let now = now_ms();
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::RobotNominal,
            registered_at_ms: now,
            last_seen_ms: now,
            metadata: Default::default(),
        }
    }

    /// ServiceState with LOCAL and PEER registered (both seeded Degraded by
    /// `register_asset`), and `cached` as the fleet posture cache.
    fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
        let fabric_router = Arc::new(FabricRouter::new());
        fabric_router.register_asset(&asset(LOCAL));
        fabric_router.register_asset(&asset(PEER));
        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    fn posture_of(svc: &ServiceState, id: &str) -> FleetPosture {
        svc.fabric_router.asset_posture(id).expect("asset registered").posture
    }

    #[test]
    fn local_asset_seeded_lockedout_peer_stays_degraded() {
        let svc = state(None);
        // register_asset seeds BOTH Degraded.
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::Degraded);
        assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);

        // The seed runs once per registered id with LOCAL configured.
        seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
        seed_local_asset_lockedout_inner(&svc, PEER, Some(LOCAL));

        assert_eq!(
            posture_of(&svc, LOCAL),
            FleetPosture::LockedOut,
            "the configured local asset is fail-closed LockedOut"
        );
        assert_eq!(
            posture_of(&svc, PEER),
            FleetPosture::Degraded,
            "peers keep the documented Degraded interim seed"
        );
    }

    #[test]
    fn unset_local_id_leaves_degraded_seed_unchanged() {
        let svc = state(None);
        seed_local_asset_lockedout_inner(&svc, LOCAL, None);
        seed_local_asset_lockedout_inner(&svc, PEER, None);
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::Degraded, "unset → no local asset to special-case");
        assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);
    }

    #[test]
    fn feed_lifts_lockedout_local_asset_on_recalc() {
        // Fresh Nominal fleet posture in the cache (as after the first Active recalc).
        let svc = state(Some(CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 1, now_ms())));
        seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::LockedOut, "starts fail-closed LockedOut");

        // The feed lifts it to the real fleet posture.
        sync_local_asset_posture(&svc, LOCAL);
        assert_eq!(
            posture_of(&svc, LOCAL),
            FleetPosture::Nominal,
            "the feed lifts the local asset out of LockedOut on recalc"
        );
    }
}

// ---------------------------------------------------------------------------
// SG-012 / H-011 — DNP3 broadcast mandatory audit (TR-012 / TR-012a / TR-012b).
// A broadcast control MUST carry a tamper-evident record; if the mandatory
// audit write fails, the command is BLOCKED (fail-closed). Unicast audit
// failure is non-fatal. The store mutex is poisoned to simulate audit failure.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod dnp3_mandatory_audit_tests {
    use super::evaluate_dnp3_adapter;

    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;

    use kirra_runtime_sdk::adapters::dnp3::{Dnp3Message, Dnp3Object, DNP3_BROADCAST_ADDRESS};
    use kirra_runtime_sdk::posture_cache::{CachedFleetPosture, ServiceState, SharedPostureCache};
    use kirra_runtime_sdk::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_runtime_sdk::verifier_store::VerifierStore;

    fn svc() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        // Fresh Nominal posture so the gate admits the command (we test the
        // AUDIT mechanism, not the posture gate).
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));
        Arc::new(ServiceState {
            app,
            posture_cache,
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_runtime_sdk::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_runtime_sdk::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_runtime_sdk::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_runtime_sdk::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    /// CROB control message (function 0x05 Direct_Operate + Group 12) to `dest`.
    fn control_msg(dest: u16) -> Dnp3Message {
        Dnp3Message {
            source_address: 0x0001,
            dest_address: dest,
            function_code: 0x05,
            data_link_control: 0,
            objects: vec![Dnp3Object { group: 12, variation: 1, data: vec![] }],
            source_node: "substation_01".to_string(),
        }
    }

    /// Poison the store mutex so every `store.lock()` returns `Err` — i.e. the
    /// mandatory audit write cannot land.
    fn poison_store(svc: &ServiceState) {
        let store = Arc::clone(&svc.app.store);
        let _ = std::thread::spawn(move || {
            let _g = store.lock().unwrap();
            panic!("intentionally poisoning the store mutex for the audit-failure test");
        })
        .join();
        assert!(svc.app.store.lock().is_err(), "store mutex should now be poisoned");
    }

    async fn post(svc: Arc<ServiceState>, msg: Dnp3Message) -> (StatusCode, serde_json::Value) {
        let resp = evaluate_dnp3_adapter(State(svc), Ok(Json(msg))).await.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        (status, serde_json::from_slice(&bytes).expect("json body"))
    }

    #[tokio::test]
    async fn test_dnp3_broadcast_always_audited() {
        let svc = svc();
        let (status, v) = post(Arc::clone(&svc), control_msg(DNP3_BROADCAST_ADDRESS)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["allowed"], true, "broadcast admitted in Nominal");
        assert_eq!(v["adapter_details"]["is_broadcast"], true);
        // The mandatory audit entry was written to the tamper-evident log.
        let n = svc.app.store.lock().unwrap()
            .count_recent_posture_events("dnp3_adapter", 0).unwrap();
        assert!(n >= 1, "a broadcast must always produce an audit entry, got {n}");
    }

    #[tokio::test]
    async fn test_dnp3_broadcast_blocked_on_audit_write_failure() {
        let svc = svc();
        poison_store(&svc); // mandatory audit write will fail
        let (status, v) = post(Arc::clone(&svc), control_msg(DNP3_BROADCAST_ADDRESS)).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a broadcast whose mandatory audit failed must be blocked"
        );
        assert_eq!(v["allowed"], false, "TR-012a: broadcast blocked when audit unavailable");
        assert_eq!(v["denial_reason"], "DNP3_BROADCAST_AUDIT_UNAVAILABLE");
    }

    #[tokio::test]
    async fn test_dnp3_unicast_audit_failure_non_fatal() {
        let svc = svc();
        poison_store(&svc); // audit write will fail for this unicast control too
        let (status, v) = post(Arc::clone(&svc), control_msg(0x0005)).await;
        assert_eq!(status, StatusCode::OK, "unicast audit failure is non-fatal");
        assert_eq!(
            v["allowed"], true,
            "TR-012b: a unicast command is NOT blocked by an audit-write failure"
        );
        assert_eq!(v["adapter_details"]["is_broadcast"], false);
    }
}

// ---------------------------------------------------------------------------
// #46 — systemd sd_notify / watchdog wiring.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod systemd_notify_tests {
    use super::{sd_notify_to, watchdog_should_ping};
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn watchdog_pings_active_only_when_cache_fresh() {
        // Active liveness is gated on a fresh posture cache (engine-liveness).
        assert!(watchdog_should_ping(true, true), "Active + fresh cache → ping");
        assert!(!watchdog_should_ping(true, false), "Active + STALE cache → withhold (fail-closed)");
        // PassiveStandby has no posture engine by design → plain keepalive.
        assert!(watchdog_should_ping(false, false), "PassiveStandby → keepalive ping");
        assert!(watchdog_should_ping(false, true), "PassiveStandby → keepalive ping");
    }

    #[test]
    fn sd_notify_to_delivers_the_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notify.sock");
        let listener = UnixDatagram::bind(&path).expect("bind notify socket");
        sd_notify_to(path.as_os_str(), "READY=1").expect("sd_notify_to send");
        let mut buf = [0u8; 64];
        let n = listener.recv(&mut buf).expect("recv");
        assert_eq!(&buf[..n], b"READY=1", "the exact sd_notify datagram must be delivered");
    }
}
