// console_phase_a_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ===========================================================================
// Operator console — Phase A tests (#103 SG6).
// ===========================================================================

use super::{
    admin_token_fingerprint, build_app, composite_challenge_key, register_operator,
    supervisor_key_ok, valid_identifier, RegisterOperatorRequest,
};

use serde_json::json;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceExt; // oneshot

use kirra_verifier::posture_cache::{
    now_ms, ServiceState, SharedPostureCache, POSTURE_CACHE_TTL_MS,
};
use kirra_verifier::verifier::{AppState, NodeTrustState, RegisteredNode, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn build_state() -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(
            kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
        ),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    })
}

fn seed_node(svc: &Arc<ServiceState>, node_id: &str) {
    let node = RegisteredNode {
        node_id: node_id.to_string(),
        status: NodeTrustState::Untrusted("post-collision latch".to_string()),
        registered_at_ms: 1,
        last_trust_update_ms: 1_700_000_000_000,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    };
    svc.app.persist_and_insert_node(node).expect("seed node");
}

async fn get(svc: Arc<ServiceState>, path: &str) -> (StatusCode, String) {
    let resp = build_app(svc, None)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn console_html_is_served() {
    let (status, body) = get(build_state(), "/console").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("OPERATOR CONSOLE"),
        "the embedded UI must be served"
    );
}

#[tokio::test]
async fn console_fleet_returns_seeded_node() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (status, body) = get(svc, "/console/fleet").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("robot-01"),
        "fleet view must list the seeded node"
    );
    assert!(
        body.contains("Untrusted"),
        "posture mapped from NodeTrustState"
    );
    assert!(
        body.contains("post-collision latch"),
        "the Untrusted note carries through"
    );
}

#[tokio::test]
async fn console_audit_returns_a_page() {
    let (status, body) = get(build_state(), "/console/audit?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"entries\""), "audit page passthrough");
    assert!(
        body.contains("\"chain_intact\""),
        "the chain-verified flag is exposed"
    );
}

#[tokio::test]
async fn grant_without_supervisor_env_is_fail_closed_503() {
    // No KIRRA_SUPERVISOR_RESET_KEY in the test env → fail-closed 503 (never
    // a silent accept). The 401/422 paths require the env set, which a
    // multithreaded test cannot do (INV-13); those are covered by the pure
    // `supervisor_key_ok` truth table + the store-level audit/grant tests.
    let svc = build_state();
    let resp = build_app(svc, None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/clearance-grants")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"node_id":"robot-01","operator_id":"alice"}"#,
                ))
                .unwrap(),
        )
        .await
        .expect("router");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn console_plane_is_posture_exempt_with_cold_cache() {
    // `build_state()` has a COLD (None) posture cache. A non-exempt READ
    // would be denied 503 by the posture gate on a cold cache (fail-closed) —
    // the `/console` plane returns 200, proving it is posture-exempt
    // (reachable to observe and recover, e.g. during LockedOut). Regression
    // lock for `gateway::policy_layer::is_posture_exempt`.
    let (status, _) = get(build_state(), "/console/fleet").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the /console plane must be posture-exempt"
    );
}

// --- #394 live console endpoints ----------------------------------------

/// Seed a node with explicit trust status + optional site/firmware. Reuses
/// the production write path (`persist_and_insert_node` — disk THEN memory).
fn seed_node_full(
    svc: &Arc<ServiceState>,
    node_id: &str,
    status: NodeTrustState,
    site: Option<&str>,
    firmware_version: Option<&str>,
) {
    let node = RegisteredNode {
        node_id: node_id.to_string(),
        status,
        registered_at_ms: 1,
        last_trust_update_ms: 1,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: site.map(|s| s.to_string()),
        firmware_version: firmware_version.map(|s| s.to_string()),
    };
    svc.app.persist_and_insert_node(node).expect("seed node");
}

fn parse(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("valid json")
}

#[test]
fn audit_chain_len_counts_rows() {
    // #395 store-level: empty chain is 0; each chained write increments it.
    let mut store = VerifierStore::new(":memory:").expect("store");
    assert_eq!(store.audit_chain_len().expect("len"), 0);
    store
        .save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
        .expect("record grant");
    assert_eq!(store.audit_chain_len().expect("len"), 1);
}

#[tokio::test]
async fn console_runtime_reports_live_state() {
    // #395: empty fleet → Active mode, 0 nodes, null heartbeat, 0 audit rows.
    let svc = build_state();
    let (status, body) = get(svc, "/console/runtime").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["mode"], "Active");
    assert_eq!(v["total_nodes"], 0);
    assert_eq!(v["audit_entries"], 0);
    assert_eq!(v["posture_cache_ttl_ms"], POSTURE_CACHE_TTL_MS);
    assert!(
        v["ha_heartbeat_age_ms"].is_null(),
        "no heartbeat written yet"
    );
    assert!(v["uptime_ms"].is_u64());
}

#[tokio::test]
async fn console_sites_rolls_up_by_trust_status() {
    // #397: Trusted→nominal, Unknown→degraded, Untrusted→lockedout; NULL site
    // → unassigned. Two nodes at "alpha", one NULL-site node.
    let svc = build_state();
    seed_node_full(&svc, "n1", NodeTrustState::Trusted, Some("alpha"), None);
    seed_node_full(
        &svc,
        "n2",
        NodeTrustState::Untrusted("fault".into()),
        Some("alpha"),
        None,
    );
    seed_node_full(&svc, "n3", NodeTrustState::Unknown, None, None);

    let (status, body) = get(svc, "/console/sites").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["unassigned"], 1, "the NULL-site node is unassigned");
    let alpha = v["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["site"] == "alpha")
        .expect("alpha site present");
    assert_eq!(alpha["total"], 2);
    assert_eq!(alpha["nominal"], 1, "Trusted maps to nominal");
    assert_eq!(alpha["lockedout"], 1, "Untrusted maps to lockedout");
    assert_eq!(alpha["degraded"], 0);
}

#[tokio::test]
async fn console_versions_rolls_up_with_pct() {
    // #398: two nodes on v1.0, one NULL → unknown; total 3.
    let svc = build_state();
    seed_node_full(&svc, "n1", NodeTrustState::Trusted, None, Some("v1.0"));
    seed_node_full(&svc, "n2", NodeTrustState::Trusted, None, Some("v1.0"));
    seed_node_full(&svc, "n3", NodeTrustState::Trusted, None, None);

    let (status, body) = get(svc, "/console/versions").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["total"], 3);
    assert_eq!(v["unknown"], 1);
    let v10 = v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["version"] == "v1.0")
        .expect("v1.0 present");
    assert_eq!(v10["count"], 2);
    let pct = v10["pct"].as_f64().unwrap();
    assert!(
        (pct - (2.0 / 3.0 * 100.0)).abs() < 1e-9,
        "pct = count/total*100"
    );
}

#[tokio::test]
async fn console_campaigns_shows_rollout_and_adoption() {
    // WS-4: the public console rollout view mirrors the admin summary — a Rolling
    // campaign with its stage progress + the adoption count from node reports.
    use kirra_verifier::ota_campaign::{Campaign, NodeArtifactStatus};
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let svc = build_state();

    // Seed a reachable Rolling@10% campaign (arm + one advance) and two adoption
    // reports on its digest, directly through the store.
    let mut c = Campaign::new(
        "c-live",
        digest,
        "v2",
        vec!["fleet".into()],
        vec![10, 100],
        1,
    )
    .unwrap();
    c.arm(2).unwrap();
    c.advance(kirra_verifier::verifier::FleetPosture::Nominal, 3)
        .unwrap();
    svc.app
        .store
        .call(move |s| {
            s.insert_campaign(&c)?;
            for node in ["robot-1", "robot-2"] {
                s.upsert_node_artifact_status(&NodeArtifactStatus {
                    node_id: node.into(),
                    applied_digest: digest.into(),
                    campaign_id: Some("c-live".into()),
                    artifact_version: Some("v2".into()),
                    reported_at_ms: 4,
                    attested: false,
                })?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .expect("store task")
        .expect("seed campaign + reports");

    let (status, body) = get(svc, "/console/campaigns").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["total"], 1);
    assert_eq!(v["rolling"], 1);
    let roll = &v["active"][0];
    assert_eq!(roll["campaign_id"], "c-live");
    assert_eq!(roll["rollout_percent"], 10);
    assert_eq!(roll["stage"], 1);
    assert_eq!(roll["stage_count"], 2);
    assert_eq!(roll["applied_nodes"], 2, "both reports adopted the digest");
}

#[tokio::test]
async fn console_analytics_empty_and_seeded_do_not_panic() {
    // #396: empty store → valid shape, no panic.
    let svc = build_state();
    let (status, body) = get(svc.clone(), "/console/analytics").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["window_ms"], 86_400_000u64);
    assert!(v["posture_transitions"].as_array().unwrap().len() == 24);
    assert!(v["denial_rate_series"].is_array());
    assert!(v["interventions_by_asset"].is_array());
    assert!(v["flapping_top"].as_array().unwrap().is_empty());

    // Seed a real chained posture event, then re-query: flapping_top picks it
    // up and a Nominal transition lands in a bucket.
    svc.app.store.with(|store| {
        let posture_json =
            serde_json::to_string(&kirra_verifier::verifier::FleetPosture::Nominal).unwrap();
        store
            .save_posture_event_chained(
                "robot-09",
                "ATTESTATION_TRUSTED",
                &posture_json,
                None,
                now_ms(),
            )
            .expect("seed posture event");
    });
    let (status, body) = get(svc, "/console/analytics?window_ms=86400000").await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    let flap = v["flapping_top"].as_array().unwrap();
    assert!(
        flap.iter().any(|f| f["node_id"] == "robot-09"),
        "the seeded node appears in flapping_top"
    );
    let total_nominal: u64 = v["posture_transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["to_nominal"].as_u64().unwrap())
        .sum();
    assert_eq!(total_nominal, 1, "one Nominal transition bucketed");
}

#[test]
fn supervisor_key_ok_truth_table() {
    // unconfigured / empty / over-length → 503 (fail-closed, INV-7)
    assert_eq!(
        supervisor_key_ok(Some("k"), None),
        Err(StatusCode::SERVICE_UNAVAILABLE)
    );
    assert_eq!(
        supervisor_key_ok(Some("k"), Some("")),
        Err(StatusCode::SERVICE_UNAVAILABLE)
    );
    let too_long = "x".repeat(65);
    assert_eq!(
        supervisor_key_ok(Some("x"), Some(&too_long)),
        Err(StatusCode::SERVICE_UNAVAILABLE)
    );
    // configured but no / wrong key → 401 ("no auth → 401")
    assert_eq!(
        supervisor_key_ok(None, Some("secret")),
        Err(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        supervisor_key_ok(Some("wrong"), Some("secret")),
        Err(StatusCode::UNAUTHORIZED)
    );
    // correct key → Ok
    assert_eq!(supervisor_key_ok(Some("secret"), Some("secret")), Ok(()));
}

#[test]
fn valid_grant_recorded_in_chain_with_pending_marker() {
    let store = VerifierStore::new(":memory:").expect("store");
    let app = AppState::new(store, VerifierOperationMode::Active);
    app.store.with(|s| {
        s.save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
            .expect("record grant");
    });
    let page = app
        .store
        .with(|s| s.load_audit_chain_page(50, 0, None))
        .expect("page");
    let found = page.entries.iter().any(|e| {
        let v = serde_json::to_value(e).unwrap();
        v.get("event_type").and_then(|x| x.as_str()) == Some("OperatorClearanceGrantIssued")
            && serde_json::to_string(&v)
                .unwrap()
                .contains("PENDING-NODE-TRANSPORT")
    });
    assert!(
        found,
        "the grant must be a signed chain event with the PENDING delivery marker"
    );
}

#[test]
fn rejected_attempt_is_audited() {
    let store = VerifierStore::new(":memory:").expect("store");
    let app = AppState::new(store, VerifierOperationMode::Active);
    app.store.with(|s| {
        s.append_clearance_audit_event(
            "OperatorClearanceGrantRejected",
            r#"{"reason":"empty_operator_id","node_id":"robot-01"}"#,
            1_700_000_000_000,
        )
        .expect("audit reject");
    });
    let page = app
        .store
        .with(|s| s.load_audit_chain_page(50, 0, None))
        .expect("page");
    assert!(
        page.entries.iter().any(|e| serde_json::to_value(e)
            .unwrap()
            .get("event_type")
            .and_then(|x| x.as_str())
            == Some("OperatorClearanceGrantRejected")),
        "a rejected attempt must leave a signed audit row"
    );
}

#[test]
fn grant_never_mutates_posture() {
    // The Phase-A honesty proof: recording a grant changes NO posture.
    let store = VerifierStore::new(":memory:").expect("store");
    let app = AppState::new(store, VerifierOperationMode::Active);
    seed_node_app(&app, "robot-01");

    let before = app.calculate_posture("robot-01");
    app.store.with(|s| {
        s.save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
            .expect("record grant");
    });
    let after = app.calculate_posture("robot-01");
    assert_eq!(
        serde_json::to_string(&before).unwrap(),
        serde_json::to_string(&after).unwrap(),
        "a recorded grant must NOT mutate posture (Phase A records; it does not release)"
    );
}

fn seed_node_app(app: &AppState, node_id: &str) {
    let node = RegisteredNode {
        node_id: node_id.to_string(),
        status: NodeTrustState::Untrusted("post-collision latch".to_string()),
        registered_at_ms: 1,
        last_trust_update_ms: 1_700_000_000_000,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    };
    app.persist_and_insert_node(node).expect("seed node");
}

// ===================================================================
// #314 Phase 1 — operator-proven identity. The operator-signed flow uses
// NO env (no admin / supervisor key), so it is fully exercisable here; the
// operator is seeded via the store directly (the admin route's gating is
// proved separately, since INV-13 forbids set_var in a multithread test).
// ===================================================================

use ed25519_dalek::{Signer, SigningKey};

/// A deterministic test operator keypair + its SPKI PEM (reuses the in-repo
/// RFC-8410 prefix convention from `attestation_nonce_handler_tests`).
fn operator_keypair(seed: u8) -> (SigningKey, String) {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(sk.verifying_key().as_bytes());
    let pem = format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        b64e.encode(&der)
    );
    (sk, pem)
}

fn sign_grant_b64(sk: &SigningKey, operator_id: &str, node_id: &str, nonce: &str) -> String {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    let payload =
        kirra_verifier::attestation::operator_grant_signing_payload(operator_id, node_id, nonce);
    b64e.encode(sk.sign(&payload).to_bytes())
}

/// #412 — sign the EMERGENCY-STOP payload (domain-distinct from a grant).
fn sign_stop_b64(sk: &SigningKey, operator_id: &str, node_id: &str, nonce: &str) -> String {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    let payload =
        kirra_verifier::attestation::operator_stop_signing_payload(operator_id, node_id, nonce);
    b64e.encode(sk.sign(&payload).to_bytes())
}

fn register_op(svc: &Arc<ServiceState>, operator_id: &str, pem: &str) {
    svc.app
        .store
        .with(|s| s.register_operator(operator_id, pem, 1))
        .unwrap();
}

fn parse_nonce(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .unwrap()
        .get("nonce")
        .and_then(|x| x.as_str())
        .expect("challenge body has a nonce")
        .to_string()
}

async fn post_json(
    svc: Arc<ServiceState>,
    path: &str,
    body: String,
    supervisor_key: Option<&str>,
) -> (StatusCode, String) {
    let mut rb = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(k) = supervisor_key {
        rb = rb.header("x-kirra-supervisor-key", k);
    }
    let resp = build_app(svc, None)
        .oneshot(rb.body(Body::from(body)).unwrap())
        .await
        .expect("router");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

// ===================================================================
// #325 / #326 / #327 — medium hardening. The admin-gated /console/operators
// route is 503 without KIRRA_ADMIN_TOKEN (INV-13 forbids set_var here), so the
// register handler is exercised by a DIRECT call (its admin gating is proved
// separately by `require_admin_token`); the unauthenticated challenge/grant
// routes go through the real router.
// ===================================================================

fn audit_has(svc: &Arc<ServiceState>, event_type: &str) -> bool {
    let page = svc
        .app
        .store
        .with(|s| s.load_audit_chain_page(200, 0, None))
        .unwrap();
    page.entries.iter().any(|e| e.event_type == event_type)
}

fn chain_json(svc: &Arc<ServiceState>) -> String {
    let page = svc
        .app
        .store
        .with(|s| s.load_audit_chain_page(200, 0, None))
        .unwrap();
    serde_json::to_string(&page.entries).unwrap()
}

fn admin_headers(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    h
}

/// #326 — the composite challenge-map key resolves the `{op}|{node}` ambiguity
/// (the old form collided `(a|b,c)` with `(a,b|c)` to `"a|b|c"`).
#[test]
fn composite_key_resolves_delimiter_ambiguity() {
    assert_ne!(
        composite_challenge_key("a|b", "c"),
        composite_challenge_key("a", "b|c"),
        "length-prefixing must distinguish (a|b,c) from (a,b|c)"
    );
    assert_eq!(
        composite_challenge_key("alice", "robot-01"),
        "5:alice:robot-01"
    );
}

/// #326 — identifier charset: `|` and control characters rejected; clean ids pass.
#[test]
fn valid_identifier_rejects_pipe_and_controls() {
    assert!(valid_identifier("alice"));
    assert!(valid_identifier("op-7_A.B"));
    assert!(!valid_identifier(""), "empty rejected");
    assert!(!valid_identifier("a|b"), "pipe rejected");
    assert!(!valid_identifier("a\nb"), "newline rejected");
    assert!(!valid_identifier("a\tb"), "tab rejected");
    assert!(!valid_identifier("a\u{7}b"), "bell control char rejected");
}

/// #326 — the register route rejects a `|`-bearing / control-char operator_id
/// with 400 and accepts a clean one (201). Handler called directly.
#[tokio::test]
async fn register_operator_rejects_bad_charset() {
    let svc = build_state();
    let (_sk, pem) = operator_keypair(3);
    let headers = admin_headers("t");

    let bad_pipe = RegisterOperatorRequest {
        operator_id: "a|b".into(),
        ed25519_pubkey_pem: pem.clone(),
    };
    let r = register_operator(State(svc.clone()), headers.clone(), Json(bad_pipe))
        .await
        .into_response();
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "pipe in operator_id → 400"
    );

    let bad_ctrl = RegisterOperatorRequest {
        operator_id: "a\nb".into(),
        ed25519_pubkey_pem: pem.clone(),
    };
    let r = register_operator(State(svc.clone()), headers.clone(), Json(bad_ctrl))
        .await
        .into_response();
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "control char in operator_id → 400"
    );

    let ok = RegisterOperatorRequest {
        operator_id: "alice".into(),
        ed25519_pubkey_pem: pem,
    };
    let r = register_operator(State(svc), headers, Json(ok))
        .await
        .into_response();
    assert_eq!(r.status(), StatusCode::CREATED, "a clean id registers");
}

/// #325 — NO enumeration oracle: an unknown operator gets a uniform 200 with a
/// nonce-shaped body and NOTHING stored (the decoy proof); an active operator
/// gets a real stored challenge; and a grant attempt for the unknown operator
/// still 403s at the unchanged grant-time check.
#[tokio::test]
async fn unknown_operator_challenge_is_a_decoy_no_oracle() {
    let svc = build_state();
    seed_node(&svc, "robot-01");

    // Unknown operator → 200 + nonce, but NOTHING stored (no map growth, no 403).
    let (status, body) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=ghost&node_id=robot-01",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "no 403 oracle — unknown operator still gets 200"
    );
    assert!(
        !parse_nonce(&body).is_empty(),
        "the decoy response is nonce-shaped"
    );
    assert!(
        svc.app.pending_clearance_challenges.is_empty(),
        "the decoy nonce is NEVER stored"
    );

    // Active operator → 200 + nonce AND a real stored challenge under the key.
    let (_sk, pem) = operator_keypair(4);
    register_op(&svc, "alice", &pem);
    let (status, body) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!parse_nonce(&body).is_empty());
    assert!(
        svc.app
            .pending_clearance_challenges
            .contains_key(&composite_challenge_key("alice", "robot-01")),
        "an active operator's challenge IS stored under the composite key"
    );

    // Grant-time still 403s for the unknown operator (the decoy buys nothing).
    let body = json!({
        "node_id": "robot-01", "operator_id": "ghost",
        "nonce": "abcd", "signature_b64": "AAAA"
    })
    .to_string();
    let (status, _) = post_json(svc, "/console/clearance-grants", body, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an unknown operator cannot redeem a decoy at grant time"
    );
}

/// #327 — re-registering a REVOKED operator emits a distinct OperatorReactivated
/// chain event (carrying the reactivating admin's token fingerprint), not a
/// silent OperatorRegistered. A FRESH registration emits OperatorRegistered only.
#[tokio::test]
async fn reregistering_revoked_operator_audits_reactivation() {
    let svc = build_state();
    let (_sk, pem) = operator_keypair(9);
    let headers = admin_headers("admin-secret");

    // Fresh registration → OperatorRegistered only.
    let req = RegisterOperatorRequest {
        operator_id: "alice".into(),
        ed25519_pubkey_pem: pem.clone(),
    };
    let r = register_operator(State(svc.clone()), headers.clone(), Json(req))
        .await
        .into_response();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert!(
        audit_has(&svc, "OperatorRegistered"),
        "fresh registration is audited"
    );
    assert!(
        !audit_has(&svc, "OperatorReactivated"),
        "a fresh registration is NOT a reactivation"
    );

    // Revoke, then re-register → OperatorReactivated appears, attributed.
    svc.app
        .store
        .with(|s| s.revoke_operator("alice", 2))
        .unwrap();
    let req2 = RegisterOperatorRequest {
        operator_id: "alice".into(),
        ed25519_pubkey_pem: pem,
    };
    let r = register_operator(State(svc.clone()), headers, Json(req2))
        .await
        .into_response();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert!(
        audit_has(&svc, "OperatorReactivated"),
        "reactivation is distinctly audited"
    );

    let fp = admin_token_fingerprint(&admin_headers("admin-secret")).unwrap();
    assert!(
        chain_json(&svc).contains(&fp),
        "the reactivation event carries the reactivating admin's token fingerprint"
    );
}

/// HAPPY PATH + the ADDITIVE PROOF: an operator-signed grant records with the
/// key fingerprint in the signed chain, and the EXISTING Phase-B
/// `take_pending_clearance_grant` consumes the new row shape unchanged.
#[tokio::test]
async fn operator_signed_grant_records_fingerprint_and_phase_b_consumes() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(7);
    register_op(&svc, "alice", &pem);

    let (cs, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    assert_eq!(cs, StatusCode::OK, "challenge issued; body={cb}");
    let nonce = parse_nonce(&cb);

    let sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig})
            .to_string();
    let (gs, gb) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
    assert_eq!(
        gs,
        StatusCode::OK,
        "operator-signed grant recorded; body={gb}"
    );
    assert!(gb.contains("operator-signed"), "auth_method in response");
    let fp = kirra_verifier::attestation::operator_key_fingerprint(&pem).unwrap();
    assert!(gb.contains(&fp), "response carries the key fingerprint");

    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("OperatorClearanceGrantIssued"));
    assert!(
        ab.contains("operator-signed"),
        "chain event carries auth_method"
    );
    assert!(
        ab.contains(&fp),
        "chain event carries the fingerprint (non-repudiation)"
    );

    // THE ADDITIVE PROOF — Phase-B pickup is unchanged by the new columns.
    let picked = svc
        .app
        .store
        .with(|s| s.take_pending_clearance_grant("robot-01", 9_999_999_999_999))
        .unwrap()
        .expect("Phase-B consumes the operator-signed grant row");
    assert_eq!(picked.operator_id, "alice");
}

/// VERIFY-THEN-CONSUME: a replayed nonce is rejected on the second use.
#[tokio::test]
async fn nonce_replay_is_rejected_and_audited() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(8);
    register_op(&svc, "alice", &pem);
    let (_c, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    let nonce = parse_nonce(&cb);
    let sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig})
            .to_string();

    let (s1, _) = post_json(svc.clone(), "/console/clearance-grants", body.clone(), None).await;
    assert_eq!(s1, StatusCode::OK, "first use accepted");
    let (s2, b2) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
    assert_eq!(
        s2,
        StatusCode::UNAUTHORIZED,
        "replayed nonce rejected; body={b2}"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(
        ab.contains("nonce_replay_or_expired"),
        "the replay is audited"
    );
}

/// BAD SIGNATURE (signed by the wrong key) → 401, audited. Verify happens
/// before the nonce is consumed.
#[tokio::test]
async fn bad_signature_is_rejected_and_audited() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (_sk, pem) = operator_keypair(9);
    register_op(&svc, "alice", &pem);
    let (_c, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    let nonce = parse_nonce(&cb);
    let (wrong, _wpem) = operator_keypair(99);
    let sig = sign_grant_b64(&wrong, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig})
            .to_string();
    let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "wrong-key signature rejected; body={b}"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("bad_signature"));
}

/// UNKNOWN operator → 403, audited (load operator fails before anything else).
#[tokio::test]
async fn unknown_operator_is_rejected_403_audited() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let body =
        json!({"node_id":"robot-01","operator_id":"ghost","nonce":"00","signature_b64":"AAAA"})
            .to_string();
    let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "unknown operator rejected; body={b}"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("unknown_operator"));
}

/// REVOKED operator → 403, audited.
#[tokio::test]
async fn revoked_operator_is_rejected_403_audited() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(11);
    register_op(&svc, "alice", &pem);
    svc.app
        .store
        .with(|s| s.revoke_operator("alice", 2))
        .unwrap();
    let sig = sign_grant_b64(&sk, "alice", "robot-01", "00");
    let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":sig})
        .to_string();
    let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "revoked operator rejected; body={b}"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("revoked_operator"));
}

/// SEPARATE POWERS: operator registration is ADMIN-gated, NOT supervisor-gated.
/// A supervisor key alone (no admin token) cannot register an operator. (Env
/// unset → require_admin_token 503; with the env set it would be 401 — the
/// admin_token_ok decision is unit-tested elsewhere. Either way: never 2xx.)
#[tokio::test]
async fn supervisor_key_cannot_register_operators_admin_gated() {
    let svc = build_state();
    let (_sk, pem) = operator_keypair(12);
    let body = json!({"operator_id":"alice","ed25519_pubkey_pem":pem}).to_string();
    let (s, _b) = post_json(svc, "/console/operators", body, Some("a-supervisor-value")).await;
    assert_eq!(
        s,
        StatusCode::SERVICE_UNAVAILABLE,
        "operator registration is admin-gated — the supervisor key does not open it"
    );
}

/// BREAK-GLASS is DISTINCTLY audited. The success path needs the supervisor env
/// (INV-13 forbids set_var here), so prove the distinct-audit property at the
/// store level: the auth_method "supervisor-break-glass" lands in the signed
/// chain, visibly different from "operator-signed".
#[test]
fn break_glass_auth_method_is_distinct_in_the_chain() {
    let store = VerifierStore::new(":memory:").expect("store");
    let app = AppState::new(store, VerifierOperationMode::Active);
    app.store.with(|s| {
        s.save_clearance_grant_chained_with_auth(
            "robot-01",
            "alice",
            1_700_000_000_000,
            "supervisor-break-glass",
            None,
        )
        .unwrap();
    });
    let page = app
        .store
        .with(|s| s.load_audit_chain_page(50, 0, None))
        .unwrap();
    let blob = serde_json::to_string(&page.entries).unwrap();
    assert!(
        blob.contains("supervisor-break-glass"),
        "break-glass auth_method recorded distinctly in the signed chain"
    );
}

/// #323 — a passive-standby instance must REJECT a clearance-grant write (the
/// HA split-brain guard), mirroring every other mutation handler. The
/// `/console` posture-exemption keeps it reachable, but is_active() fail-closes.
#[tokio::test]
async fn standby_instance_rejects_clearance_grant() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    // Demote this instance to passive standby.
    svc.app
        .mode_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Any grant shape — the is_active guard fires FIRST, before auth.
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":"AAAA"})
            .to_string();
    let (s, _b) = post_json(svc, "/console/clearance-grants", body, None).await;
    assert_eq!(
        s,
        StatusCode::SERVICE_UNAVAILABLE,
        "a passive-standby instance must not accept grant writes (split-brain guard)"
    );
}

// ----- #412 / ADR-0013 governor-routed authenticated e-stop request --------

/// HAPPY PATH: an operator-signed stop request makes the GOVERNOR command the
/// MRC under its own authority — supervisor_tripped is set and BOTH chain
/// events (request + governor action) are recorded.
#[tokio::test]
async fn estop_request_commands_mrc_and_chains_both_events() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(31);
    register_op(&svc, "alice", &pem);
    let (_c, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    let nonce = parse_nonce(&cb);
    let sig = sign_stop_b64(&sk, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig})
            .to_string();

    let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
    assert_eq!(s, StatusCode::OK, "operator-signed stop accepted; body={b}");
    assert!(
        b.contains("stop_commanded") && b.contains("MRC_COMMANDED"),
        "body={b}"
    );

    // The governor commanded the sticky MRC under its own authority.
    assert!(
        svc.app
            .supervisor_tripped
            .load(std::sync::atomic::Ordering::SeqCst),
        "an accepted e-stop must set the sticky supervisor_tripped flag (force_lockout)"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(
        ab.contains("OperatorStopRequested"),
        "the authenticated request is chained"
    );
    assert!(
        ab.contains("GovernorMRCCommanded"),
        "the governor's MRC action is chained"
    );
    let fp = kirra_verifier::attestation::operator_key_fingerprint(&pem).unwrap();
    assert!(
        ab.contains(&fp),
        "the request event carries the operator key fingerprint (non-repudiation)"
    );
}

/// THE DOMAIN-SEPARATION SECURITY PROPERTY: a CLEARANCE (release) signature
/// must NOT be accepted as a STOP request (different signing domain). Without
/// domain separation an operator's release could be replayed as a stop.
#[tokio::test]
async fn clearance_signature_is_not_accepted_as_an_estop() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(32);
    register_op(&svc, "alice", &pem);
    let (_c, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    let nonce = parse_nonce(&cb);
    // Sign the GRANT payload, submit to the e-stop endpoint.
    let grant_sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":grant_sig})
            .to_string();
    let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "a clearance signature must not satisfy the e-stop domain; body={b}"
    );
    assert!(
        !svc.app
            .supervisor_tripped
            .load(std::sync::atomic::Ordering::SeqCst),
        "a rejected e-stop must NOT command the MRC"
    );
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("OperatorStopRequestRejected") && ab.contains("bad_signature"));
}

/// UNKNOWN operator → 403, audited, no MRC.
#[tokio::test]
async fn estop_unknown_operator_rejected_403() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let body =
        json!({"node_id":"robot-01","operator_id":"ghost","nonce":"00","signature_b64":"AAAA"})
            .to_string();
    let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "unknown operator rejected; body={b}"
    );
    assert!(!svc
        .app
        .supervisor_tripped
        .load(std::sync::atomic::Ordering::SeqCst));
    let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
    assert!(ab.contains("OperatorStopRequestRejected") && ab.contains("unknown_operator"));
}

/// REPLAY: a stop nonce is single-use — the second identical request is rejected.
#[tokio::test]
async fn estop_nonce_replay_is_rejected() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    let (sk, pem) = operator_keypair(33);
    register_op(&svc, "alice", &pem);
    let (_c, cb) = get(
        svc.clone(),
        "/console/clearance-challenge?operator_id=alice&node_id=robot-01",
    )
    .await;
    let nonce = parse_nonce(&cb);
    let sig = sign_stop_b64(&sk, "alice", "robot-01", &nonce);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig})
            .to_string();
    let (s1, _) = post_json(svc.clone(), "/console/estop-requests", body.clone(), None).await;
    assert_eq!(s1, StatusCode::OK, "first stop accepted");
    let (s2, b2) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
    assert_eq!(
        s2,
        StatusCode::UNAUTHORIZED,
        "replayed stop nonce rejected; body={b2}"
    );
}

/// HA split-brain guard: a passive-standby instance must not command the MRC.
#[tokio::test]
async fn standby_instance_rejects_estop_request() {
    let svc = build_state();
    seed_node(&svc, "robot-01");
    svc.app
        .mode_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let body =
        json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":"AAAA"})
            .to_string();
    let (s, _b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
    assert_eq!(
        s,
        StatusCode::SERVICE_UNAVAILABLE,
        "a passive-standby instance must not command the MRC (split-brain guard)"
    );
    assert!(!svc
        .app
        .supervisor_tripped
        .load(std::sync::atomic::Ordering::SeqCst));
}
