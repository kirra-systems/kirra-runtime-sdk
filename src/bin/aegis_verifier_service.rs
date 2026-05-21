// src/bin/aegis_verifier_service.rs
// Aegis Verifier Service — distributed legitimacy fabric entry point.

use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aegis_runtime_sdk::verifier::{AppState, FleetNodePosture, NodeTrustState, RegisteredNode};
use aegis_runtime_sdk::verifier_store::VerifierStore;
use aegis_runtime_sdk::posture_cache::{now_ms, CachedFleetPosture, SharedPostureCache};
use aegis_runtime_sdk::security::constant_time_compare;

// --- Auth middleware ---------------------------------------------------------

/// Reads the expected admin token from AEGIS_ADMIN_TOKEN.
/// Fail-closed: if the env var is absent or empty, ALL mutation requests are denied
/// (503 Service Unavailable — the service is misconfigured, not the caller).
/// Timing-safe comparison via constant_time_compare prevents oracle attacks on the token.
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

// --- Shared service state ---------------------------------------------------

struct ServiceState {
    app: Arc<AppState>,
    posture_cache: SharedPostureCache,
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
    /// HMAC-SHA256(AEGIS_ADMIN_TOKEN, nonce_as_le_bytes) encoded as hex.
    /// In a full PKI deployment replace with a node-specific certificate signature.
    proof_hex: String,
}

#[derive(Serialize)]
struct AttestationStatusResponse {
    node_id: String,
    status: String,
    registered_at_ms: u64,
}

// --- Handlers ----------------------------------------------------------------

async fn register_node(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    let now = now_ms();
    let node = RegisteredNode {
        node_id: req.node_id.clone(),
        status: NodeTrustState::Unknown,
        registered_at_ms: now,
        last_trust_update_ms: 0,
        ak_public_pem: req.ak_public_pem,
        expected_pcr16_digest_hex: req.expected_pcr16_digest_hex,
    };

    // Fail-closed: disk must accept the write before memory is updated.
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
    let now = now_ms();

    // Verify the proof: HMAC-SHA256(admin_token, nonce_le_bytes) == proof_hex.
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

    // Consume the nonce (replay protection — second use is rejected).
    if !svc.app.consume_challenge(&req.node_id, req.nonce, now) {
        return (StatusCode::CONFLICT,
                Json(json!({ "error": "nonce absent, expired, or already consumed" }))).into_response();
    }

    // Promote node to Trusted, persist before updating memory.
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
    // Refresh the cache on read so the gateway interceptor has a fresh entry.
    if let Ok(mut guard) = svc.posture_cache.write() {
        *guard = Some(cached);
    }
    Json(posture)
}

async fn register_dependencies(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<RegisterDependenciesRequest>,
) -> impl IntoResponse {
    if svc.app.persist_and_insert_deps(&req.node_id, req.depends_on).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to persist dependencies" }))).into_response();
    }
    (StatusCode::OK, Json(json!({ "node_id": req.node_id, "dependencies_registered": true }))).into_response()
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

    let app_state = Arc::new(AppState::new(store));

    // Boot hydration — load persisted nodes and dependency graph into memory.
    // Mutex is released before the server starts; the lock window is startup-only.
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

    // Mutation routes require Bearer token auth (AEGIS_ADMIN_TOKEN env var).
    let admin_routes = Router::new()
        .route("/attestation/register", post(register_node))
        .route("/fleet/dependencies", post(register_dependencies))
        .layer(middleware::from_fn(require_admin_token));

    // Challenge and verify are unauthenticated — the challenge-response protocol
    // itself provides the attestation guarantee.
    let attestation_routes = Router::new()
        .route("/attestation/challenge/:node_id", post(issue_challenge))
        .route("/attestation/verify", post(verify_attestation));

    // Read-only routes need no auth.
    let read_routes = Router::new()
        .route("/attestation/status/:node_id", get(get_node_status))
        .route("/fleet/posture", get(get_fleet_posture))
        .route("/fleet/posture/:node_id", get(get_node_posture));

    let app = Router::new()
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
