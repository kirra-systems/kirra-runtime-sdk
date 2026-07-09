// crates/kirra-trajectory/tests/conformance.rs
//
// Native (kirra-trajectory-scoped) coverage of the fast-loop conformance
// check `validation::check_command_conforms`. The behaviour is also exercised
// from the adapter's `conformance_tests.rs`, but that suite runs under
// `-p kirra-ros2-adapter`; the checker-coverage gate measures
// `-p kirra-trajectory`, so this file drives every conformance decision arm
// (staleness / horizon-exhaustion / velocity / steering / Accept) from within
// the checker crate itself. Pure and sync — no ROS, no spawned tasks.

use kirra_trajectory::{
    config::VehicleConfig,
    state::{AcceptedTrajectory, EgoOdom, Pose, TrajectoryPoint, TrajectoryVerdict, DEFAULT_MAX_AGE_MS},
    validation::{check_command_conforms, ConformanceVerdict, IncomingControl, VELOCITY_TOLERANCE_MPS},
};

fn straight_pts(n: usize, v: f64, dt: f64) -> Vec<TrajectoryPoint> {
    (0..n)
        .map(|i| TrajectoryPoint {
            pose: Pose { x_m: (i as f64) * v * dt, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: v,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

fn fresh_accepted(promoted_at_ms: u64, pts: Vec<TrajectoryPoint>) -> AcceptedTrajectory {
    AcceptedTrajectory::with_verdict("av_01", 1, pts, TrajectoryVerdict::Accept, promoted_at_ms)
}

#[test]
fn conforming_command_accepts() {
    // Fresh trajectory, cmd velocity == nearest pose velocity, steering in range.
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cmd = IncomingControl { velocity_mps: 5.0, steering_rad: 0.0, stamp_ms: now };
    let cfg = VehicleConfig::default_urban();
    let ego = EgoOdom::default();
    assert_eq!(
        check_command_conforms(&cmd, &traj, &ego, &cfg, now),
        ConformanceVerdict::Accept,
    );
}

#[test]
fn stale_trajectory_mrcs() {
    // Arm A: `is_stale(now)` — now is past promotion + DEFAULT_MAX_AGE_MS.
    let promoted = 100_000;
    let now = promoted + DEFAULT_MAX_AGE_MS + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cmd = IncomingControl { velocity_mps: 5.0, steering_rad: 0.0, stamp_ms: now };
    let cfg = VehicleConfig::default_urban();
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
    );
}

#[test]
fn horizon_exhausted_mrcs() {
    // Arm B: no pose with `time_from_start_s >= elapsed` — the trajectory's
    // whole horizon is in the past while it is still fresh enough to pass
    // `is_stale(now)`. The accepted trajectory spans 0.04 s (poses at
    // 0.00/0.02/0.04 s) but elapsed since promotion is 0.15 s, so `find`
    // returns None → MRCFallback. The exact numbers matter: 150 ms keeps the
    // trajectory fresh (< DEFAULT_MAX_AGE_MS = 200 ms) yet past the 0.04 s
    // horizon, so this pins horizon exhaustion WITHOUT tripping staleness.
    let promoted = 100_000;
    let now = promoted + 150; // 0.15 s: < DEFAULT_MAX_AGE_MS (fresh), past the 0.04 s horizon
    let traj = fresh_accepted(promoted, straight_pts(3, 5.0, 0.02)); // poses at 0, 0.02, 0.04 s
    let cmd = IncomingControl { velocity_mps: 5.0, steering_rad: 0.0, stamp_ms: now };
    let cfg = VehicleConfig::default_urban();
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
    );
}

#[test]
fn overspeed_command_mrcs() {
    // Arm C: cmd velocity beyond nearest pose velocity + tolerance.
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cmd = IncomingControl {
        velocity_mps: 5.0 + VELOCITY_TOLERANCE_MPS + 0.1,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    let cfg = VehicleConfig::default_urban();
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
    );
}

#[test]
fn oversteer_command_mrcs() {
    // Arm D: |steering| beyond the vehicle's max steering angle.
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1));
    let cfg = VehicleConfig::default_urban();
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: cfg.max_steering_rad + 0.1,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
    );
}
