// dnp3_mandatory_audit_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// SG-012 / H-011 — DNP3 broadcast mandatory audit (TR-012 / TR-012a / TR-012b).
// A broadcast control MUST carry a tamper-evident record; if the mandatory
// audit write fails, the command is BLOCKED (fail-closed). Unicast audit
// failure is non-fatal. The store mutex is poisoned to simulate audit failure.
// ---------------------------------------------------------------------------

use super::{evaluate_dnp3_adapter, ReplayGuarded};

use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use kirra_industrial::adapters::dnp3::{Dnp3Message, Dnp3Object, DNP3_BROADCAST_ADDRESS};
use kirra_persistence::VerifierStore;
use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};

fn svc() -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    // Fresh Nominal posture so the gate admits the command (we test the
    // AUDIT mechanism, not the posture gate).
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

/// CROB control message (function 0x05 Direct_Operate + Group 12) to `dest`.
fn control_msg(dest: u16) -> Dnp3Message {
    Dnp3Message {
        source_address: 0x0001,
        dest_address: dest,
        function_code: 0x05,
        data_link_control: 0,
        objects: vec![Dnp3Object {
            group: 12,
            variation: 1,
            data: vec![],
        }],
        source_node: "substation_01".to_string(),
    }
}

/// Poison the underlying store mutex by panicking inside a `StoreHandle::with`
/// closure. NOTE (DB-actor migration phase 1): `StoreHandle` RECOVERS a poisoned
/// lock internally (`into_inner`), so this no longer makes subsequent store
/// access fail — it only exercises that the handle keeps working after a
/// panicking holder. The former fail-closed-on-poison replay arm is gone with
/// the bare-mutex; see the two tests below.
fn poison_store(svc: &ServiceState) {
    let store = svc.app.store.clone();
    let _ = std::thread::spawn(move || {
        store.with(|_s| {
            panic!("intentionally poisoning the store mutex for the audit-failure test")
        });
    })
    .join();
}

async fn post(svc: Arc<ServiceState>, msg: Dnp3Message) -> (StatusCode, serde_json::Value) {
    // Wrap with fresh replay metadata (seq 1 on a fresh per-test store, current
    // timestamp) so the replay/freshness gate admits and we reach the audit path.
    let guarded = ReplayGuarded {
        sequence: 1,
        timestamp_ms: now_ms(),
        message: msg,
    };
    let resp = evaluate_dnp3_adapter(State(svc), Ok(Json(guarded)))
        .await
        .into_response();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
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
    let n = svc
        .app
        .store
        .with(|store| store.count_recent_posture_events("dnp3_adapter", 0))
        .unwrap();
    assert!(
        n >= 1,
        "a broadcast must always produce an audit entry, got {n}"
    );
}

// NOTE on TR-012a/b interaction with the replay gate: the replay/freshness gate
// is a PRIMARY security control that runs BEFORE evaluation and needs the store,
// so it fail-closes (blocks) when the store is unavailable. A fully-poisoned
// store therefore now blocks at the replay gate, ahead of the TR-012a/b audit
// logic. The TR-012a "broadcast blocked when its mandatory audit write fails" and
// TR-012b "unicast audit-write failure is non-fatal" branches still exist in the
// handler and apply once the replay gate has PASSED (healthy store, failing audit
// write). The broadcast-IS-audited path (healthy store) is covered by
// `test_dnp3_broadcast_always_audited` above.

// DB-actor migration phase 1: `StoreHandle` recovers a poisoned lock
// internally, so a one-off panicking holder no longer wedges the store. The
// replay gate therefore RUNS normally after a poison (rather than emitting the
// old `INDUSTRIAL_REPLAY_STORE_POISONED` fail-closed reason, which is gone with
// the bare mutex). These tests pin the new recovery behavior: a broadcast/
// unicast control still evaluates after a transient poison.
#[tokio::test]
async fn test_store_recovers_after_poison_broadcast_still_evaluates() {
    let svc = svc();
    poison_store(&svc); // the handle recovers the poison; the gate runs normally
    let (status, v) = post(Arc::clone(&svc), control_msg(DNP3_BROADCAST_ADDRESS)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v["allowed"], true,
        "a recovered store evaluates the command normally (Nominal)"
    );
    assert_eq!(v["adapter_details"]["is_broadcast"], true);
}

#[tokio::test]
async fn test_store_recovers_after_poison_unicast_still_evaluates() {
    let svc = svc();
    poison_store(&svc);
    let (status, v) = post(Arc::clone(&svc), control_msg(0x0005)).await;
    assert_eq!(status, StatusCode::OK);
    // Nominal posture admits the unicast control once the handle recovers.
    assert_eq!(
        v["allowed"], true,
        "a recovered store evaluates the unicast command normally"
    );
}
