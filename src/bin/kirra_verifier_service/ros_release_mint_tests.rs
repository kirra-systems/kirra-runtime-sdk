// ADR-0033 — the verifier-side minting invariants, pinned on the REAL handler
// and the REAL envelope middleware (bin-internal so the production
// `handle_actuator_motion_command` is in scope):
//
//   🔴 the deny path NEVER mints a token — 400 (envelope deny) and 503
//      (epoch fence) bodies are token-free, always;
//   the 200 (enforced) arm mints a token that VERIFIES over exactly the
//      enforced bytes through the same `RosReleaseGate` a consumer runs;
//   with no signer configured the 200 body is token-free (legacy shape).
//
// INV-13: no `std::env::set_var` — the signer is constructed directly, never
// from env (env-driven provisioning is exercised by the startup path).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use tower::ServiceExt; // oneshot

use ed25519_dalek::SigningKey;
use kirra_verifier::gateway::policy_layer::enforce_actuator_safety_envelope;
use kirra_verifier::governor_release::{RosReleaseGate, RosReleaseSigner};
use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn nominal_state() -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
        CachedFleetPosture::new(FleetPosture::Nominal),
    )));
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

/// Claim the durable HA epoch so the handler's final actuator fence admits
/// the request (a fresh AppState holds epoch 0, which is fenced by design).
async fn claim_epoch(svc: &Arc<ServiceState>) {
    let claimed = svc
        .app
        .store
        .call(|store| store.try_claim_epoch(0, "ros-mint-test", 1_000))
        .await
        .expect("store call")
        .expect("claim query")
        .expect("epoch granted from genesis");
    svc.app
        .ha_fence
        .held_epoch
        .store(claimed, std::sync::atomic::Ordering::SeqCst);
}

fn signer() -> Arc<RosReleaseSigner> {
    Arc::new(RosReleaseSigner::new(
        SigningKey::from_bytes(&[42u8; 32]),
        1_000,
    ))
}

/// The REAL handler behind the REAL envelope middleware, with the signer
/// threaded exactly as `build_app_with_release_signer` threads it.
fn mint_router(svc: Arc<ServiceState>, with_signer: Option<Arc<RosReleaseSigner>>) -> Router {
    let r = Router::new()
        .route(
            "/actuator/motion/command",
            post(super::handle_actuator_motion_command),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc),
            enforce_actuator_safety_envelope,
        ));
    let r = match with_signer {
        Some(s) => r.layer(Extension(s)),
        None => r,
    };
    r.with_state(svc)
}

fn command_json(linear: f64) -> String {
    serde_json::json!({
        "linear_velocity_mps": linear,
        "steering_angle_deg": 0.0,
        "current_velocity_mps": 0.9,
        "current_steering_angle_deg": 0.0,
        "delta_time_s": 0.1,
    })
    .to_string()
}

async fn post_command(router: Router, body: String) -> (StatusCode, serde_json::Value) {
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/actuator/motion/command")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test]
async fn enforced_200_mints_a_token_that_verifies_over_exactly_the_enforced_bytes() {
    let svc = nominal_state();
    claim_epoch(&svc).await;
    let s = signer();
    let vk = s.verifying_key();

    let (status, body) = post_command(mint_router(svc, Some(s)), command_json(1.0)).await;
    assert_eq!(status, StatusCode::OK);

    let release = body
        .get("release")
        .expect("200 enforced response must carry the release object");
    let payload_hex = release["payload_hex"].as_str().expect("payload_hex");
    let token_hex = release["token_hex"].as_str().expect("token_hex");

    let payload_bytes: [u8; 32] = hex::decode(payload_hex)
        .expect("hex payload")
        .try_into()
        .expect("32-byte payload image");
    let token_bytes: [u8; 96] = hex::decode(token_hex)
        .expect("hex token")
        .try_into()
        .expect("96-byte token");
    let token = kirra_verifier::governor_release::ReleaseToken::from_bytes(&token_bytes);

    // The SAME gate a consumer runs: verify + decode FROM the signed bytes.
    let mut gate = RosReleaseGate::new(vk, 60_000);
    let released = gate
        .release(
            &payload_bytes,
            Some(&token),
            release["issued_at_ms"].as_u64().unwrap(),
        )
        .expect("the minted token must release through the consumer gate");

    // The signed twist is EXACTLY the enforced command the body reports.
    assert_eq!(
        released.linear_mps,
        body["enforced_linear_velocity_mps"].as_f64().unwrap()
    );
    assert_eq!(released.sequence, release["sequence"].as_u64().unwrap());
    // Straight steering → zero angular under the bicycle relation.
    assert_eq!(released.angular_rad_s, 0.0);

    // Track-A A3 (single wheelbase source): the release object must carry the
    // wheelbase the mint's steering→angular conversion used, and it must be
    // EXACTLY the active class contract's wheelbase — the same L the P6
    // lateral-accel check runs against. The ROS interceptor cross-checks its
    // own `wheelbase_m` parameter against this field and fail-closes on
    // mismatch, so this assertion is what makes that cross-check meaningful.
    let reported_wheelbase = release["wheelbase_m"]
        .as_f64()
        .expect("release must report the conversion wheelbase (A3)");
    let contract_wheelbase = kirra_verifier::gateway::contract_profiles::contract_for(
        kirra_verifier::gateway::contract_profiles::global_vehicle_class(),
    )
    .wheelbase_m;
    assert_eq!(
        reported_wheelbase, contract_wheelbase,
        "the reported wheelbase must be the active class contract's wheelbase"
    );
}

#[tokio::test]
async fn sequences_strictly_advance_across_consecutive_200s() {
    let svc = nominal_state();
    claim_epoch(&svc).await;
    let s = signer();
    let r1 = mint_router(Arc::clone(&svc), Some(Arc::clone(&s)));
    let r2 = mint_router(svc, Some(s));

    let (_, b1) = post_command(r1, command_json(1.0)).await;
    let (_, b2) = post_command(r2, command_json(1.0)).await;
    let s1 = b1["release"]["sequence"].as_u64().unwrap();
    let s2 = b2["release"]["sequence"].as_u64().unwrap();
    assert!(s2 > s1, "sequence must strictly advance: {s1} then {s2}");
}

/// 🔴 The invariant flag pinned: the DENY path never mints a token — with a
/// signer present and layered, the envelope 400 body carries no release
/// object, no token, nothing to replay.
#[tokio::test]
async fn deny_400_never_carries_a_token_even_with_a_signer_configured() {
    let svc = nominal_state();
    claim_epoch(&svc).await;

    // Non-finite command → P0 deny at the envelope middleware.
    let (status, body) =
        post_command(mint_router(svc, Some(signer())), command_json(f64::NAN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.get("release").is_none(),
        "deny body must be token-free, got: {body}"
    );
    let raw = body.to_string();
    assert!(
        !raw.contains("token_hex") && !raw.contains("payload_hex"),
        "deny body must carry no token material: {raw}"
    );
}

/// The epoch-fenced 503 (the handler's own deny arm, AFTER the envelope) is
/// also token-free: minting sits after the fence.
#[tokio::test]
async fn fenced_503_never_carries_a_token() {
    let svc = nominal_state(); // NO epoch claim: held_epoch = 0 → fenced
    let (status, body) = post_command(mint_router(svc, Some(signer())), command_json(1.0)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !body.to_string().contains("token_hex"),
        "fenced deny must be token-free"
    );
}

/// C4 (#1035): the actuator-release audit write is FAIL-CLOSED. With the epoch
/// HELD (the fence admits) but the audit chain unwritable, the handler must
/// REJECT the command (503) and mint NO token — releasing a motion command with
/// no durable tamper-evident record is exactly the ASIL/compliance evidence hole
/// this seals. Mirrors the posture engine's suppress-on-audit-fail policy.
#[tokio::test]
async fn audit_write_failure_rejects_the_command_and_mints_no_token() {
    let svc = nominal_state();
    claim_epoch(&svc).await;
    // Drop `audit_log_chain` so `save_posture_event_chained` fails while the
    // epoch fence (a different table) still admits — isolating the audit path.
    svc.app
        .store
        .call(|store| store.break_audit_chain_table_for_test())
        .await
        .expect("store call");

    let (status, body) = post_command(mint_router(svc, Some(signer())), command_json(1.0)).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unwritable audit chain must fail the actuator release closed"
    );
    assert!(
        !body.to_string().contains("token_hex") && !body.to_string().contains("payload_hex"),
        "a fail-closed audit rejection must mint no token: {body}"
    );
}

/// No signer configured → the 200 body has no release object (legacy shape).
#[tokio::test]
async fn no_signer_means_no_release_object_on_200() {
    let svc = nominal_state();
    claim_epoch(&svc).await;
    let (status, body) = post_command(mint_router(svc, None), command_json(1.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("release").is_none());
}
