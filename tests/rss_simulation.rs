// tests/rss_simulation.rs
//
// 10,000-scenario adversarial trajectory simulation (PARK-019).
//
// Three tests:
//   1. test_rss_adversarial_10k_scenarios — direct sync loop; 10,000 scenarios
//      × 10 governor evaluations each; deterministic seed; zero violations allowed.
//   2. test_rss_posture_lifecycle_violation_to_recovery — ScenarioRunner:
//      violation → Degraded; sub-threshold recovery → still Degraded;
//      threshold-th safe tick → Nominal.
//   3. test_locked_out_hard_stop_dominates_rss_gate — untrusted node drives
//      fleet to LockedOut; RSS-safe state must not override the hard stop.

use std::sync::Arc;

use rand::{rngs::StdRng, Rng, SeedableRng};

use kirra_runtime_sdk::posture_cache::{CachedFleetPosture, SharedPostureCache};
use kirra_runtime_sdk::posture_engine::recalculate_and_broadcast;
use kirra_runtime_sdk::posture_engine_v2::apply_rss_state;
use kirra_runtime_sdk::recovery_hysteresis::AV_RECOVERY_STREAK_THRESHOLD;
use kirra_runtime_sdk::scenario_runner::{PostureAssertion, ScenarioEvent, ScenarioRunner};
use kirra_runtime_sdk::verifier::{
    AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode,
};
use kirra_runtime_sdk::verifier_store::VerifierStore;

use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::{longitudinal_safe_distance, RssState};
use parko_kirra::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn build_app() -> (Arc<AppState>, SharedPostureCache) {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
        CachedFleetPosture::new(FleetPosture::Nominal),
    )));
    (app, cache)
}

fn reset_rss_state(app: &Arc<AppState>) {
    app.rss_active_violation
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut streak) = app.rss_recovery_streak.lock() {
        streak.count = 0;
        streak.start_ms = 0;
    }
}

fn fleet_to_safety_posture(fleet: &FleetPosture) -> SafetyPosture {
    match fleet {
        FleetPosture::Nominal => SafetyPosture::Nominal,
        FleetPosture::Degraded => SafetyPosture::Degraded,
        FleetPosture::LockedOut => SafetyPosture::LockedOut,
    }
}

fn effective_velocity(action: EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(v) => v,
        EnforcementAction::ClampAngularVelocity(_) => proposed,
        EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
        EnforcementAction::Deny { .. } => 0.0,
    }
}

fn cmd(v: f64) -> ControlCommand {
    ControlCommand { linear_velocity: v, angular_velocity: 0.0, timestamp_ms: 0 }
}

fn read_fleet_posture(cache: &SharedPostureCache) -> FleetPosture {
    cache
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.posture.clone())
        .unwrap_or(FleetPosture::Nominal)
}

// ---------------------------------------------------------------------------
// Test 1 — 10,000-scenario adversarial simulation
// ---------------------------------------------------------------------------

#[test]
fn test_rss_adversarial_10k_scenarios() {
    let mut rng = StdRng::seed_from_u64(0xDEAD_BEEF_CAFE);

    let (app, cache) = build_app();
    let mut violations_escaped = 0u64;

    for scenario in 0..10_000u64 {
        let now_ms = scenario * 500;

        // Random kinematic scenario: derive RSS safe/unsafe from IEEE 2846 formula.
        let ego_vel: f64 = rng.gen_range(0.0..30.0);
        let lead_vel: f64 = rng.gen_range(0.0..30.0);
        let gap: f64 = rng.gen_range(0.5..100.0);

        let safe_dist = longitudinal_safe_distance(ego_vel, lead_vel, 0.5, 3.0, 6.0, 8.0);
        let rss_safe = gap >= safe_dist;

        let scenario_rss = RssState {
            safe: rss_safe,
            longitudinal_margin: if rss_safe { gap - safe_dist } else { 0.0 },
            lateral_margin: f64::MAX,
        };

        // Apply once per scenario — avoids 100,000 SQLite writes.
        apply_rss_state(&app, &scenario_rss, now_ms);
        recalculate_and_broadcast(&app, &cache);

        let fleet_posture = read_fleet_posture(&cache);
        let safety_posture = fleet_to_safety_posture(&fleet_posture);

        // Ten governor evaluations with varying commanded velocities.
        for _tick in 0..10 {
            let commanded: f64 = rng.gen_range(0.0..35.0);
            let prev_vel: f64 = rng.gen_range(0.0..35.0);

            let mut gov = KirraGovernor::new();
            gov.update_rss_state(scenario_rss.clone());

            let action = gov.evaluate(&cmd(commanded), Some(&cmd(prev_vel)), 0.05, safety_posture);
            let out_vel = effective_velocity(action, commanded);

            // Invariant: RSS unsafe → effective velocity must not exceed MRC ceiling.
            // (Nominal's over-deceleration clamp can return v > commanded, but never
            //  when the RSS gate is active — apply_mrc_profile always caps at ceiling.)
            if !rss_safe && out_vel > MRC_VELOCITY_CEILING_MPS {
                violations_escaped += 1;
            }

            // Invariant: LockedOut → hard stop (exact zero).
            if fleet_posture == FleetPosture::LockedOut {
                assert_eq!(
                    out_vel, 0.0,
                    "scenario {scenario}: LockedOut must be hard stop, got {out_vel}"
                );
            }
        }

        reset_rss_state(&app);
    }

    assert_eq!(
        violations_escaped, 0,
        "{violations_escaped} unsafe commands escaped the governor across 10,000 scenarios"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — Posture lifecycle: violation → Degraded → recovery → Nominal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rss_posture_lifecycle_violation_to_recovery() {
    let (app, cache) = build_app();

    let violation =
        RssState { safe: false, longitudinal_margin: 0.5, lateral_margin: 0.2 };
    let safe_tick =
        RssState { safe: true, longitudinal_margin: 12.0, lateral_margin: 5.0 };

    let mut runner = ScenarioRunner::new(app, cache)
        .at_ms(0, ScenarioEvent::RssReport(violation))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded));

    // Sub-threshold safe ticks must leave posture at Degraded.
    for i in 0..(AV_RECOVERY_STREAK_THRESHOLD - 1) {
        let t = 100 + i as u64 * 100;
        runner = runner.at_ms(t, ScenarioEvent::RssReport(safe_tick.clone()));
    }

    let last_pre_recovery_t = 100 + (AV_RECOVERY_STREAK_THRESHOLD as u64 - 1) * 100;
    runner = runner.assert_at_ms(
        last_pre_recovery_t,
        PostureAssertion::FleetPostureIs(FleetPosture::Degraded),
    );

    // Threshold-th safe tick clears the violation; posture returns to Nominal.
    let recovery_t = last_pre_recovery_t + 100;
    runner
        .at_ms(recovery_t, ScenarioEvent::RssReport(safe_tick.clone()))
        .assert_at_ms(recovery_t, PostureAssertion::FleetPostureIs(FleetPosture::Nominal))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 3 — LockedOut DAG posture dominates the RSS gate
// ---------------------------------------------------------------------------

#[test]
fn test_locked_out_hard_stop_dominates_rss_gate() {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
        CachedFleetPosture::new(FleetPosture::Nominal),
    )));

    // Register an untrusted node — DAG drives fleet posture to LockedOut.
    app.nodes.insert(
        "adversarial_node".to_string(),
        RegisteredNode {
            node_id: "adversarial_node".to_string(),
            status: NodeTrustState::Untrusted("adversarial test node".to_string()),
            registered_at_ms: 0,
            last_trust_update_ms: 0,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
        },
    );

    // RSS state is safe — LockedOut from the DAG must not be overridden.
    let safe_rss = RssState { safe: true, longitudinal_margin: 50.0, lateral_margin: 20.0 };
    apply_rss_state(&app, &safe_rss, 0);
    recalculate_and_broadcast(&app, &cache);

    let fleet_posture = read_fleet_posture(&cache);
    assert_eq!(
        fleet_posture,
        FleetPosture::LockedOut,
        "Untrusted node must drive fleet to LockedOut"
    );

    let mut gov = KirraGovernor::new();
    gov.update_rss_state(safe_rss);

    // Every commanded velocity must produce a hard stop (0.0).
    for &commanded in &[0.0_f64, 1.0, 3.0, 5.0, 10.0, 35.0, 100.0] {
        let action = gov.evaluate(&cmd(commanded), None, 0.05, SafetyPosture::LockedOut);
        let out_vel = effective_velocity(action, commanded);
        assert_eq!(
            out_vel, 0.0,
            "LockedOut must always return 0.0, got {out_vel} for commanded={commanded}"
        );
    }
}
