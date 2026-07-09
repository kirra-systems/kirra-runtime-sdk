// industrial_replay_handler_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// Industrial replay/freshness gate — handler-level behavior (drives the DNP3
// handler, since the gate is shared across all four industrial handlers).
// ---------------------------------------------------------------------------

use super::{evaluate_dnp3_adapter, ReplayGuarded};
use axum::body::to_bytes;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use kirra_verifier::adapters::dnp3::Dnp3Message;
use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;
use std::sync::Arc;

fn svc() -> Arc<ServiceState> {
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
    })
}

// A benign DNP3 READ (fc 0x01) so the only gate exercised is replay/freshness
// (a read is ReadTelemetry → admitted in Nominal, not a control, not bounded).
fn read_msg(source: &str) -> Dnp3Message {
    Dnp3Message {
        source_address: 1,
        dest_address: 1,
        function_code: 0x01,
        data_link_control: 0,
        objects: vec![],
        source_node: source.to_string(),
    }
}

async fn post(
    svc: Arc<ServiceState>,
    msg: Dnp3Message,
    sequence: u64,
    timestamp_ms: u64,
) -> serde_json::Value {
    let g = ReplayGuarded {
        sequence,
        timestamp_ms,
        message: msg,
    };
    let resp = evaluate_dnp3_adapter(State(svc), Ok(Json(g)))
        .await
        .into_response();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_in_order_admitted_then_replay_and_regress_rejected() {
    let svc = svc();
    let now = now_ms();
    let v1 = post(svc.clone(), read_msg("plc-1"), 10, now).await;
    assert_eq!(v1["allowed"], true, "fresh in-order read admitted: {v1}");
    let v2 = post(svc.clone(), read_msg("plc-1"), 10, now).await;
    assert_eq!(v2["allowed"], false);
    assert_eq!(
        v2["denial_reason"], "INDUSTRIAL_MESSAGE_REPLAY",
        "replay rejected: {v2}"
    );
    let v3 = post(svc.clone(), read_msg("plc-1"), 5, now).await;
    assert_eq!(
        v3["denial_reason"], "INDUSTRIAL_MESSAGE_REPLAY",
        "regress rejected: {v3}"
    );
    let v4 = post(svc.clone(), read_msg("plc-1"), 11, now).await;
    assert_eq!(v4["allowed"], true, "higher seq admitted again: {v4}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_and_future_rejected_and_stale_does_not_burn_sequence() {
    let svc = svc();
    let now = now_ms();
    let stale = post(
        svc.clone(),
        read_msg("plc-2"),
        1,
        now.saturating_sub(60_000),
    )
    .await;
    assert_eq!(
        stale["denial_reason"], "INDUSTRIAL_MESSAGE_STALE",
        "{stale}"
    );
    let future = post(svc.clone(), read_msg("plc-3"), 1, now + 60_000).await;
    assert_eq!(
        future["denial_reason"], "INDUSTRIAL_MESSAGE_FUTURE_DATED",
        "{future}"
    );
    // The stale message (freshness-checked BEFORE the sequence advance) must NOT
    // have burned the sequence: a later in-window seq-1 from plc-2 is admitted.
    let ok = post(svc.clone(), read_msg("plc-2"), 1, now).await;
    assert_eq!(
        ok["allowed"], true,
        "a stale message must not advance the sequence: {ok}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_sources_have_independent_sequences() {
    let svc = svc();
    let now = now_ms();
    assert_eq!(
        post(svc.clone(), read_msg("plc-a"), 100, now).await["allowed"],
        true
    );
    // plc-b starts fresh; its seq 1 is admitted despite plc-a sitting at 100.
    assert_eq!(
        post(svc.clone(), read_msg("plc-b"), 1, now).await["allowed"],
        true
    );
}
