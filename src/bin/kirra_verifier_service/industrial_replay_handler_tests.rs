// industrial_replay_handler_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// Industrial replay/freshness gate — handler-level behavior (drives the DNP3
// handler, since the gate is shared across all four industrial handlers).
// ---------------------------------------------------------------------------

use super::{
    evaluate_canopen_adapter, evaluate_dnp3_adapter, evaluate_ethernet_ip_adapter, ReplayGuarded,
};
use axum::body::to_bytes;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use kirra_industrial::adapters::canopen::CanOpenMessage;
use kirra_industrial::adapters::dnp3::Dnp3Message;
use kirra_industrial::adapters::ethernet_ip::EtherNetIpMessage;
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
        perception_cap: kirra_core::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
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

// ---------------------------------------------------------------------------
// Dedicated CANopen / EtherNet-IP magnitude-bound WIRING (regression guard).
//
// BUG (external runtime review, "Bug 1"): the dedicated /industrial/canopen and
// /industrial/ethernet-ip handlers classified against posture but never invoked
// the adapter's configured magnitude bound, so an out-of-range SDO download / CIP
// Set_Attribute_Single that the unified path (dispatch_adapter -> A::bound_magnitude)
// and the DNP3 handler both reject was ACCEPTED on the dedicated route. The fix adds
// the same `if allowed { A::bound_magnitude(msg) }` block both siblings already run.
//
// These drive the real handlers through the added bound branch on the admit path
// (Nominal + unconfigured global bounds -> posture-only -> admitted), confirming the
// block is reached and the verdict shape is intact. The DENY-path *logic* both
// handlers now call is covered by the adapters' explicit-bounds unit tests
// (out_of_range_setpoint_is_denied, out_of_range_set_attribute_is_denied); a deny
// *route* test needs seeded global bounds, which INVARIANT #13 (no set_var in the
// parallel runner) precludes — the same reason the unified path's bound wiring also
// lacks a route test. Injectable bounds (a shared evaluate_protocol_message<A>) is
// the follow-up that would make the deny path route-testable.

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// An SDO expedited download (ccs=1, e=1, s=1) of `value` to (index, sub) on `node`.
fn sdo_download(node: u8, index: u16, sub: u8, value: &[u8]) -> CanOpenMessage {
    let width = value.len();
    let n = (4 - width) as u8;
    let cs = (1u8 << 5) | (n << 2) | 0x02 | 0x01;
    let idx = index.to_le_bytes();
    let mut data = vec![cs, idx[0], idx[1], sub];
    data.extend_from_slice(value);
    data.resize(8, 0x00);
    CanOpenMessage {
        node_id: node,
        function_code: 0xA, // SDOReceive (download channel)
        data,
        source_node: "rtu-1".into(),
    }
}

fn cip_set_attr(class: u16, instance: u16, attr: u16, value: &[u8]) -> EtherNetIpMessage {
    EtherNetIpMessage {
        command_code: 0x0065,
        session_handle: 1,
        status: 0,
        service_code: 0x10, // Set_Attribute_Single (a write)
        class_id: class,
        instance_id: instance,
        attribute_id: attr,
        data: value.to_vec(),
        source_node: "plc_01".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canopen_dedicated_handler_admits_write_and_reaches_bound_branch() {
    let g = ReplayGuarded {
        sequence: 1,
        timestamp_ms: now_ms(),
        message: sdo_download(5, 0x6042, 0, &100i16.to_le_bytes()),
    };
    let resp = evaluate_canopen_adapter(State(svc()), Ok(Json(g)))
        .await
        .into_response();
    let v = body_json(resp).await;
    assert_eq!(v["protocol"], "canopen");
    assert_eq!(
        v["allowed"], true,
        "Nominal + unconfigured SDO target -> admitted (bound is posture-only when unconfigured): {v}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ethernet_ip_dedicated_handler_admits_write_and_reaches_bound_branch() {
    let g = ReplayGuarded {
        sequence: 1,
        timestamp_ms: now_ms(),
        message: cip_set_attr(0x0A, 1, 3, &100i16.to_le_bytes()),
    };
    let resp = evaluate_ethernet_ip_adapter(State(svc()), Ok(Json(g)))
        .await
        .into_response();
    let v = body_json(resp).await;
    assert_eq!(v["protocol"], "ethernet_ip");
    assert_eq!(
        v["allowed"], true,
        "Nominal + unconfigured CIP target -> admitted (bound is posture-only when unconfigured): {v}"
    );
}
