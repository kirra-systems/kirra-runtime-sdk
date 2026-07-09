// crates/kirra-ros2-adapter/tests/conformance_tests.rs
//
// S131 Phase 3 — fast-loop conformance integration tests.
//
// Each test constructs an `AcceptedTrajectory` directly (the slow loop
// is exercised separately in `validation_tests.rs`), then asks the
// conformance check for a verdict. No ROS, no spawned tasks — the
// conformance check is sync and pure.

use kirra_ros2_adapter::{
    config::VehicleConfig,
    state::{
        AcceptedTrajectory, AdaptorState, EgoOdom, Pose, TrajectoryPoint, TrajectoryVerdict,
        DEFAULT_MAX_AGE_MS, SUBSCRIPTION_STALENESS_TIMEOUT_MS,
    },
    validation::{check_command_conforms, ConformanceVerdict, IncomingControl},
};

fn straight_pts(n: usize, v: f64, dt: f64) -> Vec<TrajectoryPoint> {
    (0..n)
        .map(|i| TrajectoryPoint {
            pose: Pose {
                x_m: (i as f64) * v * dt,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: v,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

fn fresh_accepted(promoted_at_ms: u64, pts: Vec<TrajectoryPoint>) -> AcceptedTrajectory {
    AcceptedTrajectory::with_verdict("av_01", 1, pts, TrajectoryVerdict::Accept, promoted_at_ms)
}

// ---------------------------------------------------------------------------
// 1. Conforming command → Accept
// ---------------------------------------------------------------------------

#[test]
fn test_conforming_command_passes() {
    // Trajectory promoted 50 ms ago at 5 m/s for 1 s. Command velocity
    // 5.0 m/s (== nearest), steering 0 → conforms.
    let promoted = 100_000;
    let now = promoted + 50; // 50 ms after promotion
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    let cfg = VehicleConfig::default_urban();
    let ego = EgoOdom {
        linear_x_mps: 5.0,
        yaw_rate_rads: 0.0,
        stamp_ms: now,
    };

    let start = std::time::Instant::now();
    let verdict = check_command_conforms(&cmd, &traj, &ego, &cfg, now);
    let elapsed_us = start.elapsed().as_micros();
    eprintln!("conforming_command_passes elapsed_us = {elapsed_us}");

    assert_eq!(
        verdict,
        ConformanceVerdict::Accept,
        "conforming command (cmd.v = nearest.v, steering in range, fresh trajectory) \
         must Accept; got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Overspeed command → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_overspeed_command_mrcs() {
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    // VELOCITY_TOLERANCE_MPS = 0.5 → 5.6 is 0.1 m/s past the tolerance.
    let cmd = IncomingControl {
        velocity_mps: 5.6,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    let cfg = VehicleConfig::default_urban();
    let ego = EgoOdom::default();

    let verdict = check_command_conforms(&cmd, &traj, &ego, &cfg, now);
    assert_eq!(
        verdict,
        ConformanceVerdict::MRCFallback,
        "command velocity 5.6 m/s > nearest.v (5.0) + tolerance (0.5) must MRC; got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Stale trajectory → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_stale_trajectory_mrcs() {
    let promoted = 100_000;
    // now is past promoted + DEFAULT_MAX_AGE_MS (200 ms) → stale.
    let now = promoted + DEFAULT_MAX_AGE_MS + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    let cfg = VehicleConfig::default_urban();
    let ego = EgoOdom::default();

    let verdict = check_command_conforms(&cmd, &traj, &ego, &cfg, now);
    assert_eq!(
        verdict,
        ConformanceVerdict::MRCFallback,
        "trajectory aged past DEFAULT_MAX_AGE_MS must MRC even on a conforming command; \
         got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. No trajectory installed → MRCFallback (driven through AdaptorState)
// ---------------------------------------------------------------------------

#[test]
fn test_no_trajectory_mrcs() {
    // Build an AdaptorState with no trajectory for the asset. The fast
    // loop's "no trajectory installed" branch is the same as the
    // AdaptorState::snapshot returning None → caller emits MRC. We
    // exercise that path directly (snapshot returns None → MRC).
    let state = AdaptorState::new();
    let snap = state.snapshot("ghost_av");
    assert!(
        snap.is_none(),
        "AdaptorState with no install must return None for unknown asset"
    );

    // current_verdict (the fast-loop's other entry point) also collapses
    // to MRCFallback per the Phase 1 contract.
    let now = 100_000;
    let verdict = state.current_verdict("ghost_av", now);
    assert_eq!(
        verdict,
        TrajectoryVerdict::MRCFallback,
        "AdaptorState::current_verdict on unknown asset must be MRCFallback; got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Subscription staleness — Phase 4 SG9 fail-closed
// ---------------------------------------------------------------------------

/// SG9: if ANY of the three required upstream subscriptions
/// (trajectory / objects / odometry) is stale, the fast loop must MRC
/// regardless of the AcceptedTrajectory + command state. The freshly-
/// constructed AdaptorState has all three `last_*_ms = 0` so it is
/// stale immediately — the safe direction at cold start.
///
/// NOTE (Phase 4b): this test exercises the staleness logic and the
/// `touch_*` / `any_subscription_stale` contract directly. The
/// subscription → state plumbing (the spawned drain tasks in
/// `node.rs::run_adapter` that turn r2r `subscribe_untyped` streams
/// into `state.touch_*(now_ms)` calls) is gated behind the `ros2`
/// feature and is tested as part of the CARLA scenario suite (requires
/// ROS env). See `docs/testing/CARLA_SCENARIO_SUITE.md` §B and §C.1.
#[test]
fn test_stale_subscription_mrcs() {
    let state = AdaptorState::new();
    // Cold start: nothing has been touched. Even with `now_ms = 0`,
    // the "0 sentinel = never seen" rule fires.
    assert!(
        state.any_subscription_stale(0, SUBSCRIPTION_STALENESS_TIMEOUT_MS),
        "cold-start AdaptorState (all last_*_ms = 0) must read as stale"
    );

    // Touch all three at t = 1_000. At t = 1_400 (400 ms later) nothing
    // is stale yet (under the 500 ms default).
    state.touch_trajectory(1_000);
    state.touch_objects(1_000);
    state.touch_odom(1_000);
    assert!(
        !state.any_subscription_stale(1_400, SUBSCRIPTION_STALENESS_TIMEOUT_MS),
        "subscriptions touched 400 ms ago must NOT be stale at 500 ms timeout"
    );

    // At t = 1_600 (600 ms later) the threshold is exceeded → stale.
    assert!(
        state.any_subscription_stale(1_600, SUBSCRIPTION_STALENESS_TIMEOUT_MS),
        "subscriptions touched 600 ms ago must be stale at 500 ms timeout"
    );

    // Asymmetric staleness: trajectory + objects fresh, odom stale.
    state.touch_trajectory(2_000);
    state.touch_objects(2_000);
    // odom last touched at 1_000, now at 2_000 → 1_000 ms stale.
    assert!(
        state.any_subscription_stale(2_000, SUBSCRIPTION_STALENESS_TIMEOUT_MS),
        "any single stale subscription must surface stale=true"
    );
}
