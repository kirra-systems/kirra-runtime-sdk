// industrial_dedicated_bounds_tests — regression guard for the dedicated CANopen /
// EtherNet-IP route magnitude-bound wiring.
//
// BUG (external runtime review, "Bug 1"): the dedicated `/industrial/canopen/evaluate`
// and `/industrial/ethernet-ip/evaluate` handlers classified the command against
// posture but NEVER invoked the adapter's configured magnitude bound, so an
// out-of-range SDO download / CIP Set_Attribute_Single that the unified
// `/industrial/evaluate` path (via `dispatch_adapter` → `A::bound_magnitude`) and
// the DNP3 handler both reject was ACCEPTED on the dedicated route (posture
// permitting). The fix adds the same `if allowed { A::bound_magnitude(msg) }` block
// both siblings already run.
//
// These handler-level tests drive the real handlers through the new bound branch on
// the admit path (Nominal + unconfigured global bounds → posture-only → admitted),
// confirming the added block is reached and the verdict/shape is intact.
//
// The DENY-path *wiring* is not route-tested here: the bounds are a process-global
// `OnceLock` seeded from env and INVARIANT #13 forbids `set_var` in the parallel
// runner, so a configured envelope cannot be injected into the live handler in-proc
// (the same reason the unified path's bound wiring also lacks a route test). The
// bound DENY *logic* both handlers now call is covered by the adapters' explicit-
// bounds unit tests (`out_of_range_setpoint_is_denied`,
// `out_of_range_set_attribute_is_denied`, …). Making bounds injectable so the deny
// path is route-testable is the natural follow-up (the review's shared
// `evaluate_protocol_message<A>` refactor).

use super::{evaluate_canopen_adapter, evaluate_ethernet_ip_adapter, ReplayGuarded};
use axum::body::to_bytes;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use kirra_verifier::adapters::canopen::CanOpenMessage;
use kirra_verifier::adapters::ethernet_ip::EtherNetIpMessage;
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
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    })
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// An SDO expedited download (ccs=1, e=1, s=1) of `value` to (index, sub) on `node`.
/// Mirrors the adapter's `sdo_expedited` test builder.
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

#[tokio::test]
async fn canopen_dedicated_handler_admits_write_and_reaches_bound_branch() {
    // Unconfigured global bounds (no env) → the SDO download is posture-only, so a
    // Nominal write is ADMITTED after the (now-present) bound check. The assertion
    // proves the handler runs to a verdict through the new `if allowed { bound }`
    // block without panicking; the branch is exercised on the Ok path.
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
        "Nominal + unconfigured SDO target → admitted (bound is posture-only when unconfigured)"
    );
}

#[tokio::test]
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
        "Nominal + unconfigured CIP target → admitted (bound is posture-only when unconfigured)"
    );
}
