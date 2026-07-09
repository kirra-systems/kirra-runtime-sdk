// tests/rss_posture_tests.rs
//
// Integration tests for RSS safe-distance posture escalation (PARK-015).
//
// Verifies:
//   1. An active RSS violation (safe==false) escalates Nominal → Degraded.
//   2. Recovery requires exactly AV_RECOVERY_STREAK_THRESHOLD consecutive safe
//      ticks; one fewer is insufficient.

use std::sync::Arc;

use kirra_verifier::posture_cache::{CachedFleetPosture, SharedPostureCache};
use kirra_verifier::recovery_hysteresis::AV_RECOVERY_STREAK_THRESHOLD;
use kirra_verifier::scenario_runner::{PostureAssertion, ScenarioEvent, ScenarioRunner};
use kirra_verifier::verifier::{
    AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode,
};
use kirra_verifier::verifier_store::VerifierStore;
use parko_core::RssState;

fn build_rss_test_app() -> (Arc<AppState>, SharedPostureCache) {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    // One live, Trusted node → the DAG genuinely derives Nominal, so the RSS
    // violation layer under test escalates from a real Nominal baseline. (An
    // EMPTY live set now fails closed to LockedOut — the M-9 guard — which would
    // mask the escalation this test targets.)
    app.persist_and_insert_node(RegisteredNode {
        node_id: "rss-node".to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 1,
        last_trust_update_ms: 1,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    })
    .expect("register baseline node");
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
        CachedFleetPosture::new(FleetPosture::Nominal),
    )));
    (app, cache)
}

fn violation() -> RssState {
    RssState {
        safe: false,
        longitudinal_margin: 1.2,
        lateral_margin: 0.4,
    }
}

fn safe_tick() -> RssState {
    RssState {
        safe: true,
        longitudinal_margin: 14.0,
        lateral_margin: 6.0,
    }
}

// ---------------------------------------------------------------------------
// Test 1: RSS violation escalates Nominal fleet posture to Degraded
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rss_violation_degrades_nominal_posture() {
    let (app, cache) = build_rss_test_app();

    ScenarioRunner::new(app, cache)
        .at_ms(0, ScenarioEvent::RssReport(violation()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 2: Recovery requires full AV_RECOVERY_STREAK_THRESHOLD safe ticks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rss_recovery_requires_full_streak() {
    let (app, cache) = build_rss_test_app();

    let mut runner = ScenarioRunner::new(app, cache)
        // t=0: violation
        .at_ms(0, ScenarioEvent::RssReport(violation()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded));

    // Send (threshold - 1) safe ticks at 100 ms intervals — must still be Degraded.
    for i in 0..(AV_RECOVERY_STREAK_THRESHOLD - 1) {
        runner = runner.at_ms(100 + i as u64 * 100, ScenarioEvent::RssReport(safe_tick()));
    }

    let last_building_t = 100 + (AV_RECOVERY_STREAK_THRESHOLD as u64 - 1) * 100;
    runner = runner.assert_at_ms(
        last_building_t,
        PostureAssertion::FleetPostureIs(FleetPosture::Degraded),
    );

    // Send the threshold-th safe tick — violation must be cleared.
    let recovery_t = last_building_t + 100;
    runner
        .at_ms(recovery_t, ScenarioEvent::RssReport(safe_tick()))
        .assert_at_ms(
            recovery_t,
            PostureAssertion::FleetPostureIs(FleetPosture::Nominal),
        )
        .run()
        .await;
}
