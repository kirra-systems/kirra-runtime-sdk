// crates/kirra-ros2-adapter/tests/validation_tests.rs
//
// S131 Phase 2A — integration tests for the slow-loop validator.
//
// Each test exercises `validate_trajectory_slow` end-to-end through the
// kernel's `validate_trajectory_containment` + `validate_vehicle_command`
// + `parko_core::rss` modules with the Phase 1 `MockCorridorSource` as
// the corridor seam (Phase 2B replaces it with the real
// Lanelet2CorridorSource).

use kirra_ros2_adapter::{
    config::VehicleConfig,
    corridor::{MockCorridorSource, Point},
    state::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict},
    validation::validate_trajectory_slow,
};
use kirra_runtime_sdk::verifier::FleetPosture;

/// Build a straight n-pose trajectory along +X at uniform velocity.
///
/// Starts at `x_start` so the rear bumper (rear_x = -1.1 m on the default
/// urban vehicle) stays inside the corridor — `MockCorridorSource` starts
/// the corridor at x = 0 so we start ego poses at x = 5 m by default.
fn straight_trajectory(num_poses: usize, velocity_mps: f64, dt: f64) -> Vec<TrajectoryPoint> {
    let x_start = 5.0;
    (0..num_poses)
        .map(|i| TrajectoryPoint {
            pose: Pose {
                x_m: x_start + (i as f64) * velocity_mps * dt,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Clean trajectory → Accept
// ---------------------------------------------------------------------------

#[test]
fn test_clean_trajectory_accepts() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);  // 5 m/s for 1 s
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let start = std::time::Instant::now();
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    let elapsed_us = start.elapsed().as_micros();

    // Print the WCET for the report.
    eprintln!("clean_trajectory_accepts elapsed_us = {elapsed_us}");

    assert_eq!(verdict, TrajectoryVerdict::Accept,
        "10-pose straight trajectory at 5 m/s within a 5 m half-width corridor with no \
         objects must Accept; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 2. Corridor departure → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_corridor_departure_rejects() {
    let mut trajectory = straight_trajectory(10, 5.0, 0.1);
    // Pose 5 has the rear axle at y = 6 m → outside the 5 m half-width
    // corridor by ~1 m. The footprint extends another ~0.925 m → way
    // outside.
    trajectory[5].pose.y_m = 6.0;

    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "pose outside the corridor must MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 3. RSS violation → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_rss_violation_rejects() {
    // 5-pose straight trajectory at 10 m/s.
    let trajectory = straight_trajectory(5, 10.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    // Stationary object 4 m directly ahead of the first pose (pose 0 is
    // at x = 5; object at x = 9 → 4 m ahead). The RSS longitudinal-
    // required gap at 10 m/s ego against a stopped lead is tens of
    // metres — 4 m must violate.
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 9.0, y_m: 0.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }];
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "object 4 m ahead at ego 10 m/s must trigger RSS MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 3b. Head-on (oncoming) RSS — direction flips the verdict at the same gap
// ---------------------------------------------------------------------------

/// A vehicle ~15 m ahead at 12 m/s, evaluated two ways at IDENTICAL geometry:
/// as a same-direction lead (pulling away → safe) and as oncoming (head-on
/// closure → the opposite-direction bound needs ~40 m → MRC). Proves the
/// adapter checker now keys the *longitudinal* bound off the object's heading.
///
/// The object sits at y = 3 m — laterally offset enough to clear the lateral RSS
/// side-gap (so a dead-ahead lateral violation can't be the cause) yet inside the
/// 4 m alignment tolerance (so it is still evaluated longitudinally). Heading is
/// then the ONLY thing that differs between the two cases.
fn obj_at(x_m: f64, velocity_mps: f64, heading_rad: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m, y_m: 3.0 },
        velocity_mps,
        heading_rad,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

#[test]
fn oncoming_vehicle_triggers_head_on_rss_mrc() {
    let trajectory = straight_trajectory(5, 8.0, 0.1); // poses x≈5..8.2
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    // Heading π → opposing travel → head-on closure at |8 + 12| effective.
    let objects = vec![obj_at(20.0, 12.0, std::f64::consts::PI)];
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "an oncoming vehicle ~15 m ahead must trigger the head-on bound; got {verdict:?}");
}

#[test]
fn same_direction_lead_at_identical_gap_is_admitted() {
    let trajectory = straight_trajectory(5, 8.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    // Same position/speed but heading 0 → a same-direction lead pulling away
    // (12 > ego 8) → near-zero required gap → admitted. Only the heading differs.
    let objects = vec![obj_at(20.0, 12.0, 0.0)];
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a same-direction lead at the same gap is admitted (direction is the only change); got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Kinematics DenyBreach → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_kinematic_deny_rejects() {
    // 2-pose trajectory with delta_time_s = 0 → P1 InvalidTimeDelta
    // (a DenyBreach) per kinematics_contract.rs. This is the cheapest
    // deterministic DenyBreach available from a finite trajectory; NaN
    // can't be JSON-encoded so we use the dt=0 path.
    let trajectory = vec![
        TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,
        },
        TrajectoryPoint {
            pose: Pose { x_m: 5.5, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,  // dt = 0 → InvalidTimeDelta
        },
    ];
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "P1 InvalidTimeDelta DenyBreach must MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 5. Clamp-only → Clamp
// ---------------------------------------------------------------------------

#[test]
fn test_clamp_returns_clamp() {
    // 2-pose trajectory designed to trigger ClampLinear on the per-pose
    // kinematics (P3 implied-accel) without violating containment or
    // RSS. Acceleration step:
    //   v0 = 5 m/s, v1 = 30 m/s, dt = 0.5 s
    //   implied_accel = (30-5)/0.5 = 50 m/s²  >> kernel default 2.5 m/s².
    // Containment: both poses inside the 5 m half-width corridor, no
    // departure. RSS: empty objects → no violation. So the only
    // intervention is per-pose ClampLinear → expect Clamp.
    let trajectory = vec![
        TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,
        },
        TrajectoryPoint {
            pose: Pose { x_m: 13.75, y_m: 0.0, heading_rad: 0.0 },  // start + avg vel * dt
            velocity_mps: 30.0,
            time_from_start_s: 0.5,
        },
    ];
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::Clamp,
        "trajectory with per-pose ClampLinear (P3 accel ceiling) but containment + RSS \
         clean must produce Clamp; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 6. M1 — Posture-driven profile selection
// ---------------------------------------------------------------------------
//
// These tests cover the M1 wiring: validate_trajectory_slow consumes
// FleetPosture and selects the effective per-pose kinematics contract.
//
//   posture    | trajectory shape         | expected verdict
//   -----------+--------------------------+---------------------
//   Nominal    | clean 5 m/s              | Accept
//   Degraded   | 10 m/s (within Nominal)  | Clamp  (5 m/s MRC cap fires)
//   LockedOut  | any                       | MRCFallback (short-circuit)
//   Degraded   | corridor breach          | MRCFallback (geometry wins)
//   Nominal    | (regression)              | matches Accept from #1

#[test]
fn nominal_posture_clean_trajectory_accepts() {
    // Same shape as #1, named to anchor the matrix.
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::Accept,
        "Nominal posture + clean trajectory must Accept; got {verdict:?}");
}

#[test]
fn degraded_posture_caps_kinematics_to_mrc() {
    // 10 m/s is well within the Nominal max_speed (35 m/s) — Accepts under
    // Nominal. Under Degraded the contract drops to mrc_fallback_profile()
    // (max_speed = 5 m/s) so the per-pose check requests ClampLinear and
    // the aggregate verdict becomes Clamp (containment + RSS still pass).
    let trajectory = straight_trajectory(10, 10.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    // Sanity: Nominal accepts.
    let nominal_verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(nominal_verdict, TrajectoryVerdict::Accept,
        "10 m/s trajectory under Nominal must Accept (within 35 m/s vehicle max); got {nominal_verdict:?}");

    // Degraded must clamp (per-pose ClampLinear fires against the 5 m/s
    // MRC cap; containment + RSS remain green so the aggregate is Clamp).
    let degraded_verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Degraded,
    );
    assert_eq!(degraded_verdict, TrajectoryVerdict::Clamp,
        "10 m/s trajectory under Degraded must Clamp against the 5 m/s MRC cap; got {degraded_verdict:?}");
}

#[test]
fn degraded_reaccel_from_stop_mrcs() {
    // Issue #70 (Cruise Oct-2023 SF lesson): under Degraded the trajectory
    // must be a controlled decel-to-stop. A trajectory that starts stopped
    // and re-accelerates (a planned pullover-from-stop) must NOT be
    // admitted under Degraded — the re-initiation segment trips DenyBreach
    // and the aggregate verdict is MRCFallback (the controlled stop).
    // Under Nominal the same trajectory is a normal launch and is accepted.
    let dt = 0.1;
    let speeds = [0.0_f64, 0.5, 1.2, 2.0, 2.8, 3.5];
    let mut x = 5.0;
    let trajectory: Vec<TrajectoryPoint> = speeds
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let p = TrajectoryPoint {
                pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: v,
                time_from_start_s: (i as f64) * dt,
            };
            x += v * dt;
            p
        })
        .collect();
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let degraded_verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Degraded,
    );
    assert_eq!(degraded_verdict, TrajectoryVerdict::MRCFallback,
        "Degraded must refuse a re-acceleration-from-stop trajectory (no autonomous \
         re-initiation of motion); got {degraded_verdict:?}");
}

#[test]
fn degraded_decel_to_stop_trajectory_is_admitted() {
    // The complement of the Cruise lesson: a Degraded trajectory that bleeds
    // speed down toward a stop (monotonically non-increasing, within the MRC
    // envelope) is admitted — Accept or Clamp, never MRCFallback.
    let dt = 0.2;
    let speeds = [4.0_f64, 3.2, 2.4, 1.6, 0.8, 0.0];
    let mut x = 5.0;
    let trajectory: Vec<TrajectoryPoint> = speeds
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let p = TrajectoryPoint {
                pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: v,
                time_from_start_s: (i as f64) * dt,
            };
            x += v * dt;
            p
        })
        .collect();
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Degraded,
    );
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "Degraded decel-to-stop trajectory must be admitted (not MRCFallback); got {verdict:?}"
    );
}

#[test]
fn locked_out_short_circuits_to_mrcfallback() {
    // Even a perfectly clean trajectory must produce MRCFallback under
    // LockedOut. The geometry checks are not required to run — the
    // short-circuit is the contract.
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::LockedOut,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "LockedOut posture must MRC regardless of trajectory shape; got {verdict:?}");
}

#[test]
fn locked_out_short_circuits_even_on_empty_trajectory() {
    // Degenerate input — the short-circuit must still win.
    let trajectory: Vec<TrajectoryPoint> = Vec::new();
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::LockedOut,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback);
}

#[test]
fn degraded_with_corridor_breach_still_mrcs() {
    // Most-restrictive-wins: under Degraded a corridor breach still
    // produces MRCFallback (containment beats the kinematics Clamp the
    // MRC cap would otherwise produce).
    let mut trajectory = straight_trajectory(10, 5.0, 0.1);
    trajectory[5].pose.y_m = 6.0; // outside the 5 m half-width corridor

    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Degraded,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "Degraded + corridor breach must still MRC — most-restrictive-wins; got {verdict:?}");
}

#[test]
fn nominal_behavior_matches_prior_default() {
    // Regression: every prior test in this file passed Nominal explicitly
    // (above). This test pins the rule that Nominal is the construction
    // default for `AdaptorState::current_posture` — until M1b wires a
    // live posture source, the slow-loop verdict is byte-for-byte the
    // pre-M1 behaviour.
    use kirra_ros2_adapter::state::AdaptorState;
    let state = AdaptorState::new();
    assert_eq!(state.current_posture(), FleetPosture::Nominal,
        "AdaptorState must default to Nominal so pre-M1 callers see no behaviour change");
}

// ---------------------------------------------------------------------------
// 7. H2 + M1 reconciliation — the proof test
// ---------------------------------------------------------------------------
//
// This is THE integration test that proves H2 (ODD speed cap) and M1
// (posture-aware adapter) are consistent. The case is:
//
//   Nominal posture, trajectory at 30 m/s — below the 35 m/s vehicle
//   physical max but ABOVE the 22.35 m/s URBAN_ODD_SPEED_CAP_MPS.
//
// Before H2 landed, the slow loop built the per-pose contract from
// `VehicleConfig::to_kinematics_contract()` with no ODD cap; 30 m/s
// passed the kernel's Priority-2 ceiling check (30 < 35) and the slow
// loop returned `Accept`. Drift between the safety case (cap = 22.35)
// and the enforced behaviour (cap = 35) was silent.
//
// After H2 + M1 are both on main:
//   - default_urban() sets odd_speed_cap_mps = Some(URBAN_ODD_SPEED_CAP_MPS)
//     (config.rs:85)
//   - to_kinematics_contract() propagates the field (config.rs:163)
//   - validate_vehicle_command Priority 2 uses effective_max_speed_mps()
//     = min(max_speed_mps, odd_speed_cap_mps) = 22.35
//     (kinematics_contract.rs:340-343)
// → per-pose check fires ClampLinear(22.35); aggregate slow-loop verdict
// is `Clamp`. THIS test pins that chain.

#[test]
fn nominal_posture_clamps_above_odd_cap_to_22_35() {
    // 10-pose straight trajectory, all at 30 m/s. dt = 0.1 s so the
    // implied acceleration between consecutive poses is 0 (P3/P4 don't
    // fire); only P2 fires, and only because the velocity exceeds the
    // 22.35 m/s effective ceiling (NOT the 35 m/s vehicle max).
    let trajectory = straight_trajectory(10, 30.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    // Sanity: 30 m/s is below the vehicle physical max — so the test
    // really is probing the ODD cap, not the vehicle ceiling.
    assert!(30.0 < cfg.max_speed_mps,
        "test premise: 30 m/s must be below the vehicle physical max ({})",
        cfg.max_speed_mps);
    assert_eq!(
        cfg.odd_speed_cap_mps,
        Some(kirra_runtime_sdk::gateway::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS),
        "test premise: default_urban must carry the urban ODD cap"
    );

    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::Clamp,
        "30 m/s under Nominal posture must Clamp against the 22.35 m/s ODD cap, \
         NOT Accept-at-30. Before H2 + M1 reconciliation this case silently passed \
         at 35 m/s vehicle max. Got: {verdict:?}");
}
