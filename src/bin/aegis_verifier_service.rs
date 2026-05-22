// src/bin/aegis_verifier_service.rs
// Aegis Verifier Service — distributed legitimacy fabric entry point.

use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use aegis_runtime_sdk::verifier::{
    validate_client_identity_headers, AppState, BackupExport, FlapStatus, FleetNodePosture,
    HealthResponse, NodeTrustState, PostureStreamEvent, RegisteredNode, VerifierOperationMode,
};
use aegis_runtime_sdk::verifier_store::VerifierStore;
use aegis_runtime_sdk::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use aegis_runtime_sdk::security::constant_time_compare;
use aegis_runtime_sdk::action_filter::{evaluate_action_claim, ActionClaim};
use aegis_runtime_sdk::protocol_adapter::{evaluate_industrial_event, IndustrialEvent};
use aegis_runtime_sdk::federation::{
    evaluate_federated_report,
    verify_federated_report_signature,
    FederatedTrustReport,
    RegisterFederationControllerRequest,
    ReportEvaluation,
};

// --- Auth middleware ---------------------------------------------------------

async fn require_admin_token(request: Request, next: Next) -> Result<Response, StatusCode> {
    let expected = std::env::var("AEGIS_ADMIN_TOKEN")
        .unwrap_or_default();

    if expected.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !constant_time_compare(provided.as_bytes(), expected.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
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
    if !svc.app.mode.allows_mutation() {
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
    if !svc.app.mode.allows_mutation() {
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
    if !svc.app.mode.allows_mutation() {
        return (StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "instance is in passive standby mode" }))).into_response();
    }
    let now = now_ms();

    let admin_token = match std::env::var("AEGIS_ADMIN_TOKEN").ok().filter(|s| !s.is_empty()) {
        Some(t) => t,
        None => return (StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({ "error": "attestation key not configured" }))).into_response(),
    };
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(admin_token.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(&req.nonce.to_le_bytes());
    let expected_hex = hex::encode(mac.finalize().into_bytes());

    if !constant_time_compare(req.proof_hex.as_bytes(), expected_hex.as_bytes()) {
        return (StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "attestation proof invalid" }))).into_response();
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
        if let Ok(store) = svc.app.store.lock() {
            let _ = store.save_posture_event(
                &req.node_id, "ATTESTATION_TRUSTED", &posture_json, None, now,
            );
        }
    }
    emit_posture_event(&svc.app, "NODE_STATUS_CHANGED", Some(req.node_id.clone()));

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
    let now = now_ms();
    let cached = CachedFleetPosture::from_posture(&posture, now);
    if let Ok(mut guard) = svc.posture_cache.write() {
        *guard = Some(cached);
    }
    Json(posture)
}

async fn register_dependencies(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterDependenciesRequest>,
) -> impl IntoResponse {
    if !svc.app.mode.allows_mutation() {
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
        if let Ok(store) = svc.app.store.lock() {
            let _ = store.save_posture_event(
                &req.node_id, "DEPENDENCY_UPDATED", &posture_json, None, now,
            );
        }
    }
    emit_posture_event(&svc.app, "DEPENDENCY_GRAPH_MUTATED", Some(req.node_id.clone()));

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
    match svc.app.store.lock() {
        Ok(store) => match store.verify_audit_chain_integrity() {
            Ok(valid) => Json(json!({ "valid": valid })).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({ "error": "audit chain query failed" }))).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({ "error": "store lock poisoned" }))).into_response(),
    }
}

async fn evaluate_action_filter(
    State(svc): State<Arc<ServiceState>>,
    Json(claim): Json<ActionClaim>,
) -> impl IntoResponse {
    let posture = svc.posture_cache
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|c| c.propagated_status.clone()))
        .unwrap_or(aegis_runtime_sdk::verifier::FleetPosture::LockedOut);

    let decision = evaluate_action_claim(claim.clone(), posture);

    if !decision.allowed {
        let event = json!({
            "target_node": claim.target_node,
            "action_type": claim.action_type,
            "risk_class": claim.risk_class,
            "reason": decision.reason,
        });
        if let Ok(mut store) = svc.app.store.lock() {
            let _ = store.save_posture_event_chained(
                "action_filter", "ACTION_FILTER_DENIED",
                &event.to_string(), Some("action denied"), now_ms(),
            );
        }
    }
    Json(decision).into_response()
}

async fn evaluate_industrial_adapter(
    State(svc): State<Arc<ServiceState>>,
    Json(event): Json<IndustrialEvent>,
) -> impl IntoResponse {
    let posture = svc.posture_cache
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|c| c.propagated_status.clone()))
        .unwrap_or(aegis_runtime_sdk::verifier::FleetPosture::LockedOut);

    let asset_id = event.asset_id.clone();
    let protocol = format!("{:?}", event.protocol);
    let operation = event.operation.clone();
    let address = event.address.clone();
    let risk_class = event.risk_class.clone();

    let decision = evaluate_industrial_event(event, posture);

    if !decision.allowed {
        let audit = json!({
            "asset_id": asset_id,
            "protocol": protocol,
            "operation": operation,
            "address": address,
            "risk_class": risk_class,
            "reason": decision.reason,
        });
        if let Ok(mut store) = svc.app.store.lock() {
            let _ = store.save_posture_event_chained(
                "industrial_adapter", "INDUSTRIAL_ACTION_DENIED",
                &audit.to_string(), Some("industrial action denied"), now_ms(),
            );
        }
    }
    Json(decision).into_response()
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

// --- Entry point ------------------------------------------------------------

#[tokio::main]
async fn main() {
    let db_path = std::env::var("AEGIS_DB_PATH")
        .unwrap_or_else(|_| "aegis_verifier.sqlite".to_string());
    let listen_addr = std::env::var("AEGIS_VERIFIER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string());

    let store = VerifierStore::new(&db_path)
        .expect("failed to initialize verifier store");

    let mode = VerifierOperationMode::from_env();
    println!("Aegis Verifier starting in {mode:?} mode (db: {db_path})");

    let app_state = Arc::new(AppState::new(store, mode));

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

    let svc_state = Arc::new(ServiceState {
        app: app_state,
        posture_cache: Arc::new(std::sync::RwLock::new(None)),
    });

    let identity_gated_routes = Router::new()
        .route("/system/posture/stream", get(system_posture_stream))
        .route("/federation/reports/submit", post(submit_federated_report))
        .route("/action_filter/evaluate", post(evaluate_action_filter))
        .route("/industrial/evaluate", post(evaluate_industrial_adapter))
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_client_identity))
        .layer(middleware::from_fn(require_admin_token));

    let admin_routes = Router::new()
        .route("/attestation/register", post(register_node))
        .route("/fleet/dependencies", post(register_dependencies))
        .route("/system/backup/export", post(export_backup))
        .route("/system/audit/verify", get(verify_audit_chain))
        .route("/federation/controllers/register", post(register_federation_controller))
        .route("/attestation/identity/register", post(register_node_identity))
        .layer(middleware::from_fn(require_admin_token));

    let attestation_routes = Router::new()
        .route("/attestation/challenge/:node_id", post(issue_challenge))
        .route("/attestation/verify", post(verify_attestation));

    let probe_routes = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready));

    let read_routes = Router::new()
        .route("/attestation/status/:node_id", get(get_node_status))
        .route("/fleet/posture", get(get_fleet_posture))
        .route("/fleet/posture/:node_id", get(get_node_posture))
        .route("/fleet/history/:node_id", get(get_node_history))
        .route("/fleet/flapping/:node_id", get(get_node_flap_status))
        .route("/federation/reports/:asset_id", get(get_federated_reports));

    let app = Router::new()
        .merge(probe_routes)
        .merge(identity_gated_routes)
        .merge(admin_routes)
        .merge(attestation_routes)
        .merge(read_routes)
        .with_state(svc_state);

    println!("Aegis Verifier Service listening on {listen_addr} (db: {db_path})");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await
        .expect("failed to bind listener");
    axum::serve(listener, app).await
        .expect("server error");
}
