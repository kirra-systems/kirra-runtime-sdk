// federation_submit_e2e_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// Federation-submit handler E2E (closes the coverage gap flagged in the
// store-offload PR). Drives `submit_federated_report` directly against a real
// in-memory store with a registered controller and genuinely Ed25519-signed
// reports, exercising the full refactored path: offload via `with_store_blocking`
// → the locked commit closure → store persistence + nonce burn → outcome mapping.
//
// This is a HANDLER-level test, not full-router: the route is admin+identity
// gated via `KIRRA_ADMIN_TOKEN` (env), which cannot be set safely in the parallel
// test runner (INVARIANT #13). The auth/router layer is unchanged by the offload
// refactor; this test covers the handler logic the refactor actually touched.
// ---------------------------------------------------------------------------

use super::submit_federated_report;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{body::to_bytes, Json};
use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use kirra_verifier::federation_reconciliation::{
    canonical_federation_payload_v2, FederatedTrustReportV2,
};
use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;
use std::sync::Arc;

fn service() -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    // An Active node must hold a claimed epoch, or the federation commit's #79
    // in-transaction fence rejects every write as Fenced. Mirror startup's
    // claim-then-store: claim epoch 1 on the fresh ha_state row and publish it.
    {
        let claimed = app
            .store
            .with(|store| store.try_claim_epoch(0, "test-instance", 0))
            .unwrap()
            .expect("claim initial epoch on fresh store");
        app.held_epoch
            .store(claimed, std::sync::atomic::Ordering::SeqCst);
    }
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
    })
}

fn register(svc: &ServiceState, controller: &str, sk: &SigningKey) {
    let pk_b64 = b64.encode(sk.verifying_key().to_bytes());
    svc.app
        .store
        .with(|store| store.save_trusted_federation_controller(controller, &pk_b64, now_ms()))
        .expect("register controller");
}

/// A fresh, correctly Ed25519-signed v2 report (issued "now" → inside the
/// 5 s replay window) for `controller`/`asset`/`nonce`.
fn signed_report(
    sk: &SigningKey,
    controller: &str,
    asset: &str,
    nonce: &str,
    generation: Option<u64>,
) -> FederatedTrustReportV2 {
    let now = now_ms();
    let mut report = FederatedTrustReportV2 {
        source_controller_id: controller.to_string(),
        asset_id: asset.to_string(),
        posture: FleetPosture::Degraded,
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
        nonce_hex: nonce.to_string(),
        signature_b64: String::new(),
        source_generation: generation,
    };
    let sig = sk.sign(canonical_federation_payload_v2(&report).as_bytes());
    report.signature_b64 = b64.encode(sig.to_bytes());
    report
}

async fn submit(
    svc: Arc<ServiceState>,
    report: FederatedTrustReportV2,
) -> (StatusCode, serde_json::Value) {
    let resp = submit_federated_report(State(svc), Json(report))
        .await
        .into_response();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepts_valid_report_persists_and_burns_nonce() {
    let svc = service();
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    register(&svc, "ctrl-a", &sk);

    let (status, body) = submit(
        svc.clone(),
        signed_report(&sk, "ctrl-a", "lidar_front", "nonce-aaaa", Some(412)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["accepted"],
        serde_json::json!(true),
        "valid report must be accepted: {body}"
    );

    let (has_reports, burned) = svc.app.store.with(|store| {
        let has_reports = !store
            .load_federated_reports_for_asset("lidar_front")
            .unwrap()
            .is_empty();
        let burned = store.has_seen_federation_nonce("nonce-aaaa").unwrap();
        (has_reports, burned)
    });
    assert!(has_reports, "an accepted report must be persisted");
    assert!(burned, "an accepted report must burn its nonce");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_nonce_is_rejected() {
    let svc = service();
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    register(&svc, "ctrl-a", &sk);
    let report = signed_report(&sk, "ctrl-a", "lidar_front", "nonce-dup", Some(1));

    let (_, first) = submit(svc.clone(), report.clone()).await;
    assert_eq!(
        first["accepted"],
        serde_json::json!(true),
        "first submit must be accepted: {first}"
    );

    let (_, second) = submit(svc.clone(), report).await;
    assert_eq!(second["accepted"], serde_json::json!(false));
    assert_eq!(
        second["reason"],
        serde_json::json!("FEDERATED_NONCE_REPLAY"),
        "a replayed nonce must be rejected: {second}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_controller_is_rejected() {
    let svc = service();
    let sk = SigningKey::from_bytes(&[9u8; 32]); // never registered
    let (_, body) = submit(
        svc,
        signed_report(&sk, "ctrl-unknown", "lidar_front", "nonce-x", None),
    )
    .await;
    assert_eq!(body["accepted"], serde_json::json!(false));
    assert_eq!(
        body["reason"],
        serde_json::json!("UNREGISTERED_FEDERATION_CONTROLLER"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_signature_is_rejected() {
    let svc = service();
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    register(&svc, "ctrl-a", &sk);
    let mut report = signed_report(&sk, "ctrl-a", "lidar_front", "nonce-bad", None);
    report.signature_b64 = b64.encode([0u8; 64]); // tamper after signing

    let (_, body) = submit(svc.clone(), report).await;
    assert_eq!(body["accepted"], serde_json::json!(false));
    assert_eq!(
        body["reason"],
        serde_json::json!("INVALID_FEDERATION_SIGNATURE"),
        "{body}"
    );
    // A signature-rejected report must NOT burn the nonce.
    assert!(
        !svc.app
            .store
            .with(|store| store.has_seen_federation_nonce("nonce-bad"))
            .unwrap(),
        "a rejected report must not burn its nonce"
    );
}
