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
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::FleetPosture;

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
// 3b. H-2 — a NON-FINITE perception object must FAIL CLOSED (MRCFallback), not be
//     silently skipped. A NaN/Inf field poisons every RSS `<`/`abs()` comparison
//     (NaN compares false), so pre-fix the dangerous object was neither rejected
//     nor skipped and the trajectory was wrongly Accepted.
// ---------------------------------------------------------------------------

/// Control: a FINITE object placed far off-corridor is cleanly skipped and the
/// trajectory Accepts — so the test below isolates the *non-finite* effect (the
/// guard must reject only on non-finiteness, never over-reject a clean object).
#[test]
fn test_finite_far_object_is_skipped_and_accepts() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 50.0, y_m: 20.0 }, // far ahead AND 20 m off to the side
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }];
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::Accept,
        "a finite, far, off-corridor object must be skipped (Accept); got {verdict:?}");
}

/// H-2: each non-finite object field independently fails closed to MRCFallback.
#[test]
fn test_nonfinite_object_field_fails_closed_h2() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();

    // Base object: far off-corridor, so a FINITE version Accepts (see control
    // above). Each variant corrupts exactly one field with NaN or Inf.
    let base = PerceivedObject {
        id: 1,
        pos: Point { x_m: 50.0, y_m: 20.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    };

    let variants: Vec<(&str, PerceivedObject)> = vec![
        ("pos.x_m=NaN", PerceivedObject { pos: Point { x_m: f64::NAN, y_m: 0.0 }, ..base }),
        ("pos.y_m=Inf", PerceivedObject { pos: Point { x_m: 50.0, y_m: f64::INFINITY }, ..base }),
        ("velocity_mps=NaN", PerceivedObject { velocity_mps: f64::NAN, ..base }),
        ("heading_rad=NaN", PerceivedObject { heading_rad: f64::NAN, ..base }),
        ("vel.x_m=Inf", PerceivedObject { vel: Point { x_m: f64::INFINITY, y_m: 0.0 }, ..base }),
        ("vel.y_m=-Inf", PerceivedObject { vel: Point { x_m: 0.0, y_m: f64::NEG_INFINITY }, ..base }),
    ];

    for (label, obj) in variants {
        let verdict = validate_trajectory_slow(
            &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal,
        );
        assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
            "H-2: a non-finite object field ({label}) must fail closed to MRC, not be \
             silently skipped; got {verdict:?}");
    }
}

// ---------------------------------------------------------------------------
// 3a. Per-class RSS lateral band — a small-robot (courier) profile admits a side
//     object a robotaxi refuses, WITHOUT changing the robotaxi number (#1 / the
//     CONTRACT_PROFILES.md sibling rule). The checker LOGIC is identical; only the
//     `rss_lateral_alignment_tolerance_m` differs between the two profiles.
// ---------------------------------------------------------------------------

#[test]
fn courier_admits_a_side_object_a_robotaxi_refuses() {
    // A stationary object ~1 m ahead and 1.0 m to the SIDE — inside both the robotaxi
    // 4.0 m RSS lateral band AND the 2.5 m longitudinal-overlap band, so the robotaxi
    // evaluates it as a near lead → MRC. For the courier (0.6 m band) it is in ANOTHER
    // lane → filtered → containment covers it → admitted. Same scene, same checker.
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 6.0, y_m: 1.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }];

    // Robotaxi (default_urban, band 4.0 m) — REFUSES.
    let robotaxi = straight_trajectory(5, 10.0, 0.1);
    let v_taxi = validate_trajectory_slow(
        &robotaxi, &corridor, &objects,
        &VehicleConfig::default_urban(), None, FleetPosture::Nominal,
    );
    assert_eq!(v_taxi, TrajectoryVerdict::MRCFallback,
        "robotaxi: a 1 m-ahead object within the 4 m band is RSS-evaluated → MRC; got {v_taxi:?}");

    // Courier (band 0.6 m, robot speed) — ADMITS the SAME scene.
    let courier = straight_trajectory(5, 2.0, 0.1);
    let v_courier = validate_trajectory_slow(
        &courier, &corridor, &objects,
        &VehicleConfig::courier(), None, FleetPosture::Nominal,
    );
    assert!(matches!(v_courier, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "courier: the same object is beyond the 0.6 m band → containment covers it → admitted; got {v_courier:?}");
}

// ---------------------------------------------------------------------------
// 3b. Head-on (oncoming) RSS — direction flips the verdict at the same gap
// ---------------------------------------------------------------------------

/// A vehicle ~15 m ahead at 12 m/s, evaluated two ways at IDENTICAL geometry:
/// as a same-direction lead (pulling away → safe) and as oncoming (head-on
/// closure → the opposite-direction bound needs ~40 m → MRC). Proves the
/// adapter checker keys the *longitudinal* bound off the object's heading.
///
/// The object sits at y = 1.5 m — inside the longitudinal footprint-overlap band
/// (`RSS_LONGITUDINAL_OVERLAP_M`) so the head-on/lead bound is the live check, and
/// more than 8 m ahead so the longitudinally-gated *lateral* RSS does not fire (it
/// can't be the cause). Heading is then the ONLY difference between the two cases.
fn obj_at(x_m: f64, velocity_mps: f64, heading_rad: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m, y_m: 1.5 },
        velocity_mps,
        heading_rad,
        // D/C-1: keep `vel` CONSISTENT with `velocity_mps`/`heading_rad` (as real
        // ingest does: `velocity_mps = |vel|`). These scenarios use `heading_rad`
        // as the motion direction, so the velocity vector is the speed rotated by
        // it. The snapshot RSS now reads motion from `vel`, so leaving it zero
        // would model a stationary object and contradict the test's intent.
        vel: Point {
            x_m: velocity_mps * heading_rad.cos(),
            y_m: velocity_mps * heading_rad.sin(),
        },
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

#[test]
fn snapshot_rss_uses_velocity_vector_not_orientation_dc1() {
    // D/C-1: the snapshot RSS verdict must depend on the object's MOTION (the
    // velocity vector `vel`), NOT its FACING (`heading_rad`). Two objects with the
    // SAME velocity vector but different orientations must get the SAME verdict.
    // Pre-fix the verdict changed with orientation, since lateral motion was read
    // as `velocity_mps · sin(heading − ego_heading)`.
    let trajectory = straight_trajectory(20, 3.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();

    let vel = Point { x_m: 0.5, y_m: -2.0 }; // mostly-lateral motion toward the ego lane
    let pos = Point { x_m: 8.0, y_m: 3.0 };
    let speed = vel.x_m.hypot(vel.y_m);
    let facing_forward = PerceivedObject { id: 1, pos, velocity_mps: speed, heading_rad: 0.0, vel };
    let facing_motion = PerceivedObject {
        id: 1, pos, velocity_mps: speed, heading_rad: vel.y_m.atan2(vel.x_m), vel,
    };

    let v_forward = validate_trajectory_slow(
        &trajectory, &corridor, std::slice::from_ref(&facing_forward), &cfg, None, FleetPosture::Nominal,
    );
    let v_motion = validate_trajectory_slow(
        &trajectory, &corridor, std::slice::from_ref(&facing_motion), &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(
        format!("{v_forward:?}"), format!("{v_motion:?}"),
        "snapshot RSS must depend on the velocity vector, not orientation; got {v_forward:?} vs {v_motion:?}"
    );
}

#[test]
fn snapshot_rss_catches_a_forward_facing_lateral_cut_in_dc1() {
    // An object FACING forward (heading 0) but MOVING laterally into the ego path
    // must be refused. The orientation-based form read its lateral motion as ~0
    // (sin(0) = 0) and could miss it; the vector form reads the true lateral
    // component and catches the cut-in.
    let ego = held_ego(10.0); // stopped ego at x=10, y=0, heading 0
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let obj = PerceivedObject {
        id: 1,
        pos: Point { x_m: 13.0, y_m: 1.0 }, // dx 3 (in the conflict band), small lateral gap
        velocity_mps: 2.5,
        heading_rad: 0.0,                    // facing forward, but...
        vel: Point { x_m: 0.0, y_m: -2.5 },  // ...moving laterally toward the ego
    };
    let verdict = validate_trajectory_slow(
        &ego, &corridor, std::slice::from_ref(&obj), &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(
        verdict, TrajectoryVerdict::MRCFallback,
        "a forward-facing object cutting in laterally must be refused (D/C-1 vector motion); got {verdict:?}"
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
        Some(kirra_core::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS),
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

// ---------------------------------------------------------------------------
// Limited-visibility / occlusion bound (RSS Rule 4)
// ---------------------------------------------------------------------------

use kirra_ros2_adapter::validation::validate_trajectory_slow_capped;

/// A decel-to-stop straight trajectory: starts at `v0`, brakes at `decel` to 0,
/// then holds. Stays at y=0 from x=5 (inside the corridor).
fn decel_to_stop_trajectory(v0: f64, decel: f64, dt: f64, n: usize) -> Vec<TrajectoryPoint> {
    let mut v = v0;
    let mut x = 5.0;
    (0..n)
        .map(|i| {
            let p = TrajectoryPoint {
                pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: v,
                time_from_start_s: i as f64 * dt,
            };
            x += v * dt;
            v = (v - decel * dt).max(0.0);
            p
        })
        .collect()
}

#[test]
fn occlusion_rejects_a_trajectory_that_outruns_assured_clear_distance() {
    // 10 m/s for 2 s into only 5 m of assured-clear distance: the ego could never
    // stop within what it can see → RSS Rule 4 refuses it (MRCFallback).
    let trajectory = straight_trajectory(20, 10.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal, None, Some(5.0), None, None, FrameTrust::Trusted,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "10 m/s into 5 m of visibility outruns the assured clear distance; got {verdict:?}");
}

#[test]
fn occlusion_bound_is_gated_off_when_no_visibility_is_supplied() {
    // The SAME trajectory with no visibility input is unaffected by the occlusion
    // pass (it Clamps to the ODD cap on its own merits, but is NOT MRC'd for occlusion).
    let trajectory = straight_trajectory(20, 10.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal, None, None, None, None, FrameTrust::Trusted,
    );
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "with no visibility input the occlusion bound is a no-op; got {verdict:?}");
}

#[test]
fn untrusted_frame_mrcs_an_otherwise_clean_trajectory() {
    // S-FI1e: the frame-integrity gate is LIVE through the adapter. A trajectory
    // that is accepted under a Trusted frame must MRC under an Untrusted frame —
    // the containment check refuses to validate geometry in an untrusted frame.
    let trajectory = straight_trajectory(20, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let trusted = validate_trajectory_slow_capped(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal, None, None, None, None, FrameTrust::Trusted,
    );
    assert_ne!(trusted, TrajectoryVerdict::MRCFallback,
        "a clean trajectory under a Trusted frame must not MRC; got {trusted:?}");

    let untrusted = validate_trajectory_slow_capped(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal, None, None, None, None, FrameTrust::Untrusted,
    );
    assert_eq!(untrusted, TrajectoryVerdict::MRCFallback,
        "the SAME clean trajectory must MRC under an Untrusted frame; got {untrusted:?}");
}

#[test]
fn occlusion_admits_a_decel_to_stop_within_visibility() {
    // A 3 m/s trajectory that brakes to a stop within ~3 m, against 20 m of visibility:
    // it always could stop within what it sees → admitted.
    let trajectory = decel_to_stop_trajectory(3.0, 1.5, 0.1, 30);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal, None, Some(20.0), None, None, FrameTrust::Trusted,
    );
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a decel-to-stop within the assured clear distance is admissible; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// Multi-modal predictive RSS (space-time over predicted modes)
// ---------------------------------------------------------------------------

use kirra_ros2_adapter::validation::{PredictedMode, PredictedSample};

/// Build a predicted mode: an object moving in a straight line from `(x0,y0)` at
/// `(vx,vy)` m/s, sampled every 0.5 s over `horizon_s`.
fn linear_mode(id: u64, x0: f64, y0: f64, vx: f64, vy: f64, horizon_s: f64) -> Vec<PredictedSample> {
    let mut t = 0.0;
    let mut out = Vec::new();
    while t <= horizon_s + 1e-9 {
        out.push(PredictedSample {
            pos: Point { x_m: x0 + vx * t, y_m: y0 + vy * t },
            time_from_start_s: t,
        });
        t += 0.5;
    }
    let _ = id;
    out
}

#[test]
fn predictive_rss_does_not_regress_a_lane_keeping_neighbor() {
    // A fast car ABREAST in the next lane (y=-3.5), predicted to STAY in its lane.
    // The §4 lateral gating must keep admitting the ego — the predicted mode never
    // overlaps the ego's path.
    let trajectory = straight_trajectory(20, 6.0, 0.1); // ego 6 m/s straight at y=0
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let samples = linear_mode(1, 6.0, -3.5, 9.0, 0.0, 2.0); // fast, but stays at y=-3.5
    let modes = [PredictedMode { object_id: 1, samples: &samples }];

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a neighbor predicted to keep its lane must not be rejected; got {verdict:?}");
}

#[test]
fn predictive_rss_catches_a_predicted_cut_in() {
    // The SAME kind of neighbor, but its predicted mode now CUTS IN: it crosses from
    // the right lane (y=-3.5) INTO the ego's lane just ahead of the ego, then sits
    // there slow. The snapshot (t=0) shows it laterally clear in another lane, but the
    // predicted mode brings it into the path within the RSS gap → MRCFallback.
    let trajectory = straight_trajectory(20, 3.0, 0.1); // ego 3 m/s, x: 5 → ~10.7
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    // Phase 1 (t 0→1.0): cross from (9,-3.5) to (9,0). Phase 2 (t 1.0→2.0): creep
    // forward in-lane at ~0.6 m/s — 1 m ahead of the ego when it arrives at x=8 (t=1.0).
    let samples = [
        PredictedSample { pos: Point { x_m: 9.0, y_m: -3.5 }, time_from_start_s: 0.0 },
        PredictedSample { pos: Point { x_m: 9.0, y_m: -1.75 }, time_from_start_s: 0.5 },
        PredictedSample { pos: Point { x_m: 9.0, y_m: 0.0 }, time_from_start_s: 1.0 },
        PredictedSample { pos: Point { x_m: 9.3, y_m: 0.0 }, time_from_start_s: 1.5 },
        PredictedSample { pos: Point { x_m: 9.6, y_m: 0.0 }, time_from_start_s: 2.0 },
    ];
    let modes = [PredictedMode { object_id: 1, samples: &samples }];

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "a predicted cut-in into the ego's path must be refused; got {verdict:?}");
}

#[test]
fn predictive_rss_catches_a_mid_band_lateral_cut_in() {
    // REGRESSION (predictive lateral gap): a cut-in that lives in the MID lateral band —
    // RSS_LONGITUDINAL_OVERLAP_M (2.5 m) ≤ |dy| ≤ rss_lateral_alignment_tolerance_m (4.0 m
    // urban) — at a longitudinally SAFE distance. It is laterally clear of the overlap band,
    // so the longitudinal-only predictive pass admitted it; but it is CLOSING LATERALLY into
    // the ego's path, which the lateral side-RSS conjunction must refuse. The object moves
    // purely laterally (x fixed) so the longitudinal check alone never fires — isolating the
    // lateral branch as the sole reason for the MRC.
    let ego = held_ego(10.0); // ego held (stopped) at x=10, y=0
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    // x fixed at 13 (dx_ego = 3 m, longitudinally safe for a stopped ego, < 8 m conflict
    // band); y sweeps 4.0 → 0.0 at ~2 m/s — crossing the 2.5–4.0 m mid-band while closing.
    let samples = [
        PredictedSample { pos: Point { x_m: 13.0, y_m: 4.0 }, time_from_start_s: 0.0 },
        PredictedSample { pos: Point { x_m: 13.0, y_m: 3.0 }, time_from_start_s: 0.5 },
        PredictedSample { pos: Point { x_m: 13.0, y_m: 2.0 }, time_from_start_s: 1.0 },
        PredictedSample { pos: Point { x_m: 13.0, y_m: 1.0 }, time_from_start_s: 1.5 },
        PredictedSample { pos: Point { x_m: 13.0, y_m: 0.0 }, time_from_start_s: 2.0 },
    ];
    let modes = [PredictedMode { object_id: 1, samples: &samples }];

    let verdict = validate_trajectory_slow_capped(
        &ego, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "a mid-band predicted lateral cut-in at a safe longitudinal distance must be refused; got {verdict:?}");
}

// ---- The PRODUCER: derive modes from live objects (gap #3 made live) ----
//
// The tests above hand-build modes; these prove `predicted_modes_from_objects` turns LIVE
// perceived objects into modes the checker then acts on — the bridge that makes the multi-modal
// pass run against real perception instead of dormant `None`.

use kirra_ros2_adapter::prediction::predicted_modes_from_objects;

fn perceived(id: u64, x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
    PerceivedObject {
        id,
        pos: Point { x_m: x, y_m: y },
        velocity_mps: vx.hypot(vy),
        heading_rad: vy.atan2(vx),
        vel: Point { x_m: vx, y_m: vy },
    }
}

#[test]
fn produced_cv_mode_catches_a_cut_in_the_snapshot_rss_misses() {
    // An object laterally CLEAR at the snapshot (y=-5, well outside the 4 m alignment band) but
    // moving +y so its constant-velocity rollout crosses INTO the ego lane just ahead. The
    // snapshot pass skips it (out of lane now); the PRODUCED CV mode catches the cut-in.
    let trajectory = straight_trajectory(30, 2.0, 0.1); // ego 2 m/s straight at y=0, x: 5 → ~10.8
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let obj = perceived(1, 10.0, -5.0, 0.0, 2.5); // crosses y=0 at t=2, at x=10 (just ahead)

    // Snapshot-only (object passed, no produced modes): laterally clear now → admitted.
    let snapshot_only = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, None, None, FrameTrust::Trusted,
    );
    assert!(matches!(snapshot_only, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "snapshot RSS alone sees the object as out-of-lane → admits; got {snapshot_only:?}");

    // With the PRODUCED CV mode, the predicted cut-in is caught → refused.
    let owned = predicted_modes_from_objects(&[obj], &[], &[], 3.0, 0.5);
    let modes: Vec<_> = owned.iter().map(|m| m.as_mode()).collect();
    let with_modes = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert_eq!(with_modes, TrajectoryVerdict::MRCFallback,
        "the produced CV mode catches the cut-in the snapshot missed; got {with_modes:?}");
}

#[test]
fn produced_ctrv_mode_catches_a_turn_in_that_cv_misses_multimodal_payoff() {
    // An object PARALLEL to the ego lane (moving +x in the next lane, never crossing on CV) but
    // TURNING toward the ego lane. CV alone keeps it clear → admit. The PRODUCED CTRV mode (from
    // the tracker's yaw estimate) curves it INTO the path → refuse. Worst-case over modes: the
    // single dangerous hypothesis decides — the point of multi-modal prediction.
    let trajectory = straight_trajectory(30, 2.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let obj = perceived(1, 9.0, -4.5, 3.0, 0.0); // parallel +x near the right lane

    // CV-only (no yaw): the object stays in its lane → admitted.
    let cv_owned = predicted_modes_from_objects(&[obj], &[], &[], 3.0, 0.5);
    let cv_modes: Vec<_> = cv_owned.iter().map(|m| m.as_mode()).collect();
    let cv = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, Some(&cv_modes), None, FrameTrust::Trusted,
    );
    assert!(matches!(cv, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "CV alone: a lane-parallel object is admitted; got {cv:?}");

    // CV + CTRV (yaw rate turning it into the ego lane): the turn-in hypothesis refuses.
    let mm_owned = predicted_modes_from_objects(&[obj], &[(1, 0.9)], &[], 3.0, 0.5);
    assert_eq!(mm_owned.len(), 2, "the turning object yields BOTH a CV and a CTRV mode");
    let mm_modes: Vec<_> = mm_owned.iter().map(|m| m.as_mode()).collect();
    let mm = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, Some(&mm_modes), None, FrameTrust::Trusted,
    );
    assert_eq!(mm, TrajectoryVerdict::MRCFallback,
        "the produced CTRV turn-in mode catches what CV missed; got {mm:?}");
}

#[test]
fn produced_lane_follow_mode_catches_a_curving_in_object_that_cv_misses() {
    // An object laterally CLEAR (y=-5) moving PARALLEL (+x) — CV keeps it in its lane → admit. But
    // its map lane-follow PATH bends into the ego lane and ends there ahead. The lane-follow mode
    // traces the bend → the curving-in object is refused. Same idea as the CTRV payoff, sourced
    // from the lane map instead of a yaw estimate.
    let trajectory = straight_trajectory(30, 6.0, 0.1); // ego 6 m/s straight at y=0, x: 5 → ~22
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let obj = perceived(1, 16.0, -5.0, 2.5, 0.0); // moving +x in the right lane (CV stays clear)
    let path = [
        Point { x_m: 16.0, y_m: -5.0 }, Point { x_m: 17.0, y_m: -2.5 },
        Point { x_m: 18.0, y_m: 0.0 }, Point { x_m: 18.0, y_m: 0.01 },
    ];

    // CV-only: the object holds y=-5 → admitted.
    let cv_owned = predicted_modes_from_objects(&[obj], &[], &[], 3.0, 0.3);
    let cv_modes: Vec<_> = cv_owned.iter().map(|m| m.as_mode()).collect();
    let cv = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, Some(&cv_modes), None, FrameTrust::Trusted,
    );
    assert!(matches!(cv, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "CV alone: a lane-parallel object is admitted; got {cv:?}");

    // CV + lane-follow: the bend-in hypothesis brings it into the ego path → refuse.
    let lf_owned = predicted_modes_from_objects(&[obj], &[], &[(1, &path[..])], 3.0, 0.3);
    assert_eq!(lf_owned.len(), 2, "the object yields BOTH a CV and a lane-follow mode");
    let lf_modes: Vec<_> = lf_owned.iter().map(|m| m.as_mode()).collect();
    let lf = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[obj], &cfg, None, FleetPosture::Nominal, None, None, Some(&lf_modes), None, FrameTrust::Trusted,
    );
    assert_eq!(lf, TrajectoryVerdict::MRCFallback,
        "the produced lane-follow mode catches the curving-in object CV missed; got {lf:?}");
}

#[test]
fn predictive_rss_is_a_no_op_when_no_modes_are_supplied() {
    // Same cut-in geometry, but no predicted modes → the pass is skipped (snapshot
    // RSS sees no object at all here), so the trajectory is not MRC'd for prediction.
    let trajectory = straight_trajectory(20, 6.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, None, None, FrameTrust::Trusted,
    );
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "with no predicted modes the predictive pass is a no-op; got {verdict:?}");
}

#[test]
fn predictive_rss_fails_closed_on_modes_supplied_but_all_unevaluable_b3() {
    // B3 (silent fail-open fix): a neighbor whose predicted mode is MALFORMED —
    // every sample carries the SAME timestamp, so `dt <= 0` for every window and
    // the multi-modal pass can evaluate NOTHING. Previously it fell through to
    // "safe" (fail-OPEN): a producer emitting equal timestamps could silently
    // neutralize the cut-in detector. The geometry is the lane-keeping neighbor
    // that normally ACCEPTS (so neither the snapshot RSS — no objects passed — nor
    // any other pass fires); the ONLY thing that can MRC here is the new
    // fail-closed guard. It must now return MRCFallback.
    let trajectory = straight_trajectory(20, 6.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let samples = [
        PredictedSample { pos: Point { x_m: 9.0, y_m: -3.5 }, time_from_start_s: 0.0 },
        PredictedSample { pos: Point { x_m: 9.0, y_m: -3.5 }, time_from_start_s: 0.0 },
        PredictedSample { pos: Point { x_m: 9.0, y_m: -3.5 }, time_from_start_s: 0.0 },
    ];
    let modes = [PredictedMode { object_id: 1, samples: &samples }];

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "modes supplied but all unevaluable (equal timestamps) must fail closed; got {verdict:?}");
}

#[test]
fn predictive_rss_fails_closed_on_modes_with_no_evaluable_window_b3() {
    // B3: a non-empty mode set whose mode carries only a SINGLE sample — no
    // inter-sample window at all (the degenerate sub-`dt` horizon the producer can
    // emit). There is no motion to roll forward, so the pass evaluates nothing and
    // must fail closed rather than Accept.
    let trajectory = straight_trajectory(20, 6.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let samples = [PredictedSample { pos: Point { x_m: 9.0, y_m: -3.5 }, time_from_start_s: 0.0 }];
    let modes = [PredictedMode { object_id: 1, samples: &samples }];

    let verdict = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal, None, None, Some(&modes), None, FrameTrust::Trusted,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "a non-empty mode set with no evaluable window must fail closed; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// RSS conjunction (§4): a safe stationary queue is admitted; genuine danger
// (driving in, or a lateral cut-in) is still rejected. A deterministic SWEEP —
// property-style coverage of the invariant that the lateral side-RSS only fires
// on an actual side-collision possibility (abreast OR lateral closing).
// ---------------------------------------------------------------------------

fn held_ego(x_m: f64) -> Vec<TrajectoryPoint> {
    (0..3)
        .map(|i| TrajectoryPoint {
            pose: Pose { x_m, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 0.0,
            time_from_start_s: (i as f64) * 0.1,
        })
        .collect()
}

fn stopped_object(x_m: f64, y_m: f64) -> PerceivedObject {
    PerceivedObject { id: 1, pos: Point { x_m, y_m }, velocity_mps: 0.0, heading_rad: 0.0, vel: Point { x_m: 0.0, y_m: 0.0 } }
}

#[test]
fn rss_conjunction_admits_a_safe_stationary_queue() {
    // INVARIANT (the fix): a STOPPED ego a safe longitudinal distance behind a STOPPED
    // dead-center object — a stationary queue — is ADMITTED across the whole gap range, not
    // spuriously MRC'd by the lateral side-RSS (the §4 over-rejection of a safe same-lane stop).
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let ego = held_ego(10.0);
    for gap_dm in 20..=80 {
        let gap = gap_dm as f64 / 10.0; // 2.0 .. 8.0 m, 0.1 m steps
        let objs = [stopped_object(10.0 + gap, 0.0)];
        let v = validate_trajectory_slow(&ego, &corridor, &objs, &cfg, None, FleetPosture::Nominal);
        assert_ne!(v, TrajectoryVerdict::MRCFallback, "a stopped queue at gap {gap} m must be admitted, got {v:?}");
    }
}

#[test]
fn rss_conjunction_still_rejects_driving_into_a_stopped_object() {
    // INVARIANT (safety preserved): an ego at SPEED inside the longitudinal RSS distance of a
    // stopped object — driving into it — is still MRC'd, across a range of speeds.
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let objs = [stopped_object(30.0, 0.0)];
    for speed in [3.0, 5.0, 7.0, 9.0] {
        let into: Vec<TrajectoryPoint> = (0..3)
            .map(|i| TrajectoryPoint {
                pose: Pose { x_m: 27.0 + (i as f64), y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: speed,
                time_from_start_s: (i as f64) * 0.1,
            })
            .collect();
        let v = validate_trajectory_slow(&into, &corridor, &objs, &cfg, None, FleetPosture::Nominal);
        assert_eq!(v, TrajectoryVerdict::MRCFallback, "driving into a stopped object at {speed} m/s must be MRC'd, got {v:?}");
    }
}

#[test]
fn rss_conjunction_still_rejects_a_lateral_cut_in_at_a_safe_longitudinal_distance() {
    // INVARIANT (cut-in defense preserved): even with the ego HELD and the object at a SAFE
    // longitudinal distance (so the longitudinal check alone would pass), an object CLOSING
    // LATERALLY (a cut-in: nonzero lateral velocity) within the side-gap is still MRC'd — the
    // fix narrows the lateral check to a genuine side-collision possibility, it does not remove
    // it. Sweep the lateral approach speed.
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let ego = held_ego(10.0);
    for lat_speed in [1.0, 2.0, 4.0] {
        // Object 3 m ahead (longitudinally safe for a stopped ego), 0.3 m to the side, heading
        // across the ego (lateral velocity component) → a cut-in.
        let cut_in = PerceivedObject {
            id: 2,
            pos: Point { x_m: 13.0, y_m: 0.3 },
            velocity_mps: lat_speed,
            heading_rad: std::f64::consts::FRAC_PI_2,
            vel: Point { x_m: 0.0, y_m: lat_speed },
        };
        let v = validate_trajectory_slow(&ego, &corridor, &[cut_in], &cfg, None, FleetPosture::Nominal);
        assert_eq!(v, TrajectoryVerdict::MRCFallback, "a lateral cut-in at {lat_speed} m/s must be MRC'd, got {v:?}");
    }
}

// ---------------------------------------------------------------------------
// ADR-0029 — courier angular (yaw-rate) channel
// ---------------------------------------------------------------------------

/// In-place rotation: poses fixed at (5, 0), heading sweeping at `omega_rad_s`,
/// zero linear velocity — exactly the regime the bicycle steering term silently
/// drops (`v·Δt ≈ 0` → steering = 0).
fn in_place_rotation(num_poses: usize, omega_rad_s: f64, dt: f64) -> Vec<TrajectoryPoint> {
    (0..num_poses)
        .map(|i| TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: (i as f64) * omega_rad_s * dt },
            velocity_mps: 0.0,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

#[test]
fn courier_in_place_rotation_at_sane_yaw_is_admitted() {
    // ω = 0.5 rad/s < courier ω_max(0) ≈ 0.833 → now CHECKED and admitted
    // (was silently passed before; now it is positively bounded).
    let traj = in_place_rotation(6, 0.5, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(&traj, &corridor, &[], &cfg, None, FleetPosture::Nominal);
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "a sane in-place yaw must be admitted; got {verdict:?}");
}

#[test]
fn courier_in_place_rotation_at_excessive_yaw_mrcs() {
    // ω = 1.5 rad/s > courier ω_max(0) ≈ 0.833 → the silent-drop bug, now refused.
    let traj = in_place_rotation(6, 1.5, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(&traj, &corridor, &[], &cfg, None, FleetPosture::Nominal);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "an excessive in-place yaw must MRC (was silently passed pre-ADR-0029); got {verdict:?}");
}

#[test]
fn ackermann_trajectory_has_no_angular_channel() {
    // FROZEN PROOF: the SAME excessive-yaw trajectory under the robotaxi profile
    // (angular = None) is byte-identical to today — the angular check is skipped,
    // so the rotation is admitted exactly as before. The AV path is untouched.
    let traj = in_place_rotation(6, 1.5, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    assert!(cfg.angular.is_none(), "robotaxi must carry no angular channel");
    let verdict = validate_trajectory_slow(&traj, &corridor, &[], &cfg, None, FleetPosture::Nominal);
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "robotaxi (angular None) must NOT angular-MRC — byte-identical to pre-ADR-0029; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// ADR-0029 — Degraded converge-to-stop-and-HOLD on the ANGULAR channel.
//
// The magnitude bound only caps |ω|; under Degraded the courier must also
// converge the yaw axis to zero and hold (no re-initiation from a stop, no
// speed increase), the angular analog of the linear `enforce_degraded_decel_to_stop`.
// All ω values below sit UNDER the Degraded magnitude ceiling
// ω_max(0, 0.5) ≈ 0.417 rad/s, so the magnitude check passes and it is the
// stop-and-HOLD gate (not the ceiling) under test.
// ---------------------------------------------------------------------------

/// In-place rotation with a per-segment yaw-rate sequence (fixed position,
/// v=0). `omegas[i]` is the yaw rate of segment i (`Δheading_i = omegas[i]·dt`).
fn in_place_rotation_seq(omegas: &[f64], dt: f64) -> Vec<TrajectoryPoint> {
    let mut heading = 0.0_f64;
    let mut pts = vec![TrajectoryPoint {
        pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
        velocity_mps: 0.0,
        time_from_start_s: 0.0,
    }];
    for (i, &w) in omegas.iter().enumerate() {
        heading += w * dt;
        pts.push(TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: heading },
            velocity_mps: 0.0,
            time_from_start_s: ((i + 1) as f64) * dt,
        });
    }
    pts
}

/// Ego odometry snapshot carrying a current yaw rate (linear stopped).
fn odom_yaw(yaw_rate_rads: f64) -> kirra_ros2_adapter::state::EgoOdom {
    kirra_ros2_adapter::state::EgoOdom { linear_x_mps: 0.0, yaw_rate_rads, stamp_ms: 0 }
}

#[test]
fn courier_degraded_angular_reinitiation_from_stop_mrcs() {
    // Stopped courier (odom yaw = 0) + a Degraded trajectory that BEGINS an
    // in-place rotation at ω=0.3 (< the 0.417 Degraded ceiling, so the
    // magnitude bound passes). The stop-and-HOLD gate must refuse the
    // re-initiation from a stop → MRC.
    let traj = in_place_rotation(6, 0.3, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(
        &traj, &corridor, &[], &cfg, Some(&odom_yaw(0.0)), FleetPosture::Degraded,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "Degraded must refuse angular re-initiation from a stop (HOLD); got {verdict:?}");
}

#[test]
fn courier_degraded_angular_speed_increase_mrcs() {
    // Already rotating slowly (odom yaw = 0.1) + a Degraded trajectory that
    // SPEEDS UP to ω=0.3 (still < the 0.417 ceiling). Non-increasing-|ω| is
    // violated → MRC.
    let traj = in_place_rotation(6, 0.3, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(
        &traj, &corridor, &[], &cfg, Some(&odom_yaw(0.1)), FleetPosture::Degraded,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "Degraded must refuse an angular speed increase; got {verdict:?}");
}

#[test]
fn courier_degraded_angular_converging_to_stop_is_admitted() {
    // The complement: a Degraded yaw trajectory that converges to zero
    // (ω: 0.3 → 0.2 → 0.1 → 0.0), seeded from a matching odom yaw = 0.3, all
    // under the ceiling. Non-increasing throughout → admitted (Accept/Clamp),
    // never MRC. Proves the gate does not over-reject a proper angular
    // decel-to-stop.
    let traj = in_place_rotation_seq(&[0.3, 0.2, 0.1, 0.0], 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(
        &traj, &corridor, &[], &cfg, Some(&odom_yaw(0.3)), FleetPosture::Degraded,
    );
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "a converging-to-stop yaw trajectory must be admitted under Degraded; got {verdict:?}");
}

#[test]
fn courier_degraded_angular_gate_is_degraded_only() {
    // FROZEN: the SAME re-initiation-from-stop trajectory under NOMINAL is
    // admitted — the stop-and-HOLD gate is Degraded-only (ω=0.3 < the Nominal
    // 0.833 ceiling, so the magnitude bound also passes). Nominal behaviour is
    // unchanged by this gate.
    let traj = in_place_rotation(6, 0.3, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::courier();
    let verdict = validate_trajectory_slow(
        &traj, &corridor, &[], &cfg, Some(&odom_yaw(0.0)), FleetPosture::Nominal,
    );
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "the angular stop-and-HOLD gate must not fire under Nominal; got {verdict:?}");
}

#[test]
fn ackermann_degraded_has_no_angular_stop_gate() {
    // FROZEN: the robotaxi profile (angular = None) under Degraded with the
    // same re-initiation trajectory carries NO angular stop gate — the block is
    // skipped entirely (v=0 in-place rotation, so the linear gate admits it
    // too). Byte-identical to pre-ADR-0029 on the AV path.
    let traj = in_place_rotation(6, 0.3, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    assert!(cfg.angular.is_none());
    let verdict = validate_trajectory_slow(
        &traj, &corridor, &[], &cfg, Some(&odom_yaw(0.0)), FleetPosture::Degraded,
    );
    assert_ne!(verdict, TrajectoryVerdict::MRCFallback,
        "robotaxi (angular None) must carry no angular stop gate; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// #683/#684 — closing-speed-scaled lateral-conflict window
// ---------------------------------------------------------------------------

#[test]
fn high_speed_cut_in_beyond_8m_is_caught_dc2() {
    // #684: at highway-adjacent speed a lateral cut-in originating MORE than the
    // old fixed 8 m ceiling ahead must still be refused. The ego runs at 18 m/s;
    // the object sits ~12 m ahead at dy = 3 m (above the 2.5 m overlap gate, inside
    // the 4 m alignment band) and is closing laterally into the ego lane. The poses
    // barely advance (0.9 m each), so the object is ≥12 m ahead at EVERY pose —
    // beyond the old 8 m lateral ceiling, which therefore skipped it (Accept). With
    // the fix the window is RSS_LONGITUDINAL_CONFLICT_M.max(lon_required) (~50 m at
    // 18 m/s), so the lateral RSS fires → MRC.
    let trajectory = straight_trajectory(3, 18.0, 0.05); // poses x = 5.0, 5.9, 6.8
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban(); // ODD cap 22.35 > 18 → no kinematics clamp
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 19.0, y_m: 3.0 }, // ~12 m ahead, in the 2.5–4 m band
        velocity_mps: 3.0,
        heading_rad: -std::f64::consts::FRAC_PI_2, // facing its motion direction
        vel: Point { x_m: 0.0, y_m: -3.0 },        // closing laterally toward y = 0
    }];
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "an 18 m/s ego must refuse a lateral cut-in ~12 m ahead (beyond the old 8 m ceiling); got {verdict:?}");
}

#[test]
fn stationary_side_object_in_band_beyond_8m_stays_admitted_683() {
    // #683 regression guard: widening the lateral window must NOT start
    // over-rejecting a longitudinally-unsafe but laterally-STILL object in the
    // 2.5–4 m band. The zero-lateral-velocity required gap (= 2·a_lat·ρ² ≈ 1.75 m
    // at the 3.5 m/s² / 0.5 s defaults) is below the 3 m offset, so the object —
    // which the ego is closing on longitudinally — is still admitted. Same geometry
    // as the cut-in test but with zero object velocity.
    let trajectory = straight_trajectory(3, 18.0, 0.05);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 19.0, y_m: 3.0 }, // ~12 m ahead, 3 m to the side, STATIONARY
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }];
    let verdict = validate_trajectory_slow(
        &trajectory, &corridor, &objects, &cfg, None, FleetPosture::Nominal,
    );
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a stationary object 3 m to the side stays admitted inside the widened window; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// WS-2 — Pedestrian / VRU RSS (KIRRA-VRU-RSS-001), end-to-end through the
// slow checker. Design: docs/safety/PEDESTRIAN_RSS.md; primitive:
// kirra_trajectory::vru.
// ---------------------------------------------------------------------------

use kirra_trajectory::vru::{PedestrianScene, PerceivedPedestrian, VruRssParams};

fn vru_scene(peds: &[PerceivedPedestrian]) -> PedestrianScene<'_> {
    PedestrianScene { pedestrians: peds, params: VruRssParams::default() }
}

fn walker(id: u64, x: f64, y: f64) -> PerceivedPedestrian {
    PerceivedPedestrian {
        id,
        pos: Point { x_m: x, y_m: y },
        vel: Point { x_m: 0.0, y_m: 0.0 },
        // #789 F8 — fresh synchronous measurement (no additional disc growth).
        age_s: 0.0,
    }
}

fn vru_verdict(
    trajectory: &[TrajectoryPoint],
    peds: &[PerceivedPedestrian],
) -> TrajectoryVerdict {
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let scene = vru_scene(peds);
    validate_trajectory_slow_capped(
        trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal,
        None, None, None, Some(&scene), FrameTrust::Trusted,
    )
}

/// A pedestrian inside the moving ego's stopping+reachable envelope → MRC.
#[test]
fn vru_pedestrian_in_path_mrcs() {
    let trajectory = straight_trajectory(10, 5.0, 0.1); // 5 m/s from x=5
    let verdict = vru_verdict(&trajectory, &[walker(1, 12.0, 0.0)]);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "a pedestrian 7 m ahead of a 5 m/s ego is inside the reachable-set bound");
}

/// A pedestrian far beyond the envelope → the trajectory is admitted.
#[test]
fn vru_far_pedestrian_admits() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let verdict = vru_verdict(&trajectory, &[walker(1, 60.0, 0.0)]);
    assert_eq!(verdict, TrajectoryVerdict::Accept,
        "a pedestrian ~50 m ahead must not bind a 1 s, 5 m/s trajectory");
}

/// THE STOP-PROPOSAL INVARIANT at the checker level: a stopped trajectory
/// next to a pedestrian is admitted — the VRU gate can never make the
/// always-available safe_stop proposal inadmissible (doer↔checker deadlock).
#[test]
fn vru_safe_stop_next_to_pedestrian_admits() {
    let stop: Vec<TrajectoryPoint> = (0..5)
        .map(|i| TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 0.0,
            time_from_start_s: f64::from(i) * 0.1,
        })
        .collect();
    let verdict = vru_verdict(&stop, &[walker(1, 5.6, 0.4)]);
    assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "a stationary trajectory imposes no VRU requirement (responsibility rule); got {verdict:?}");
}

/// Omnidirectionality: a kerbside pedestrian laterally OUTSIDE the
/// vehicle-RSS alignment band still binds a moving ego — VRUs step in.
#[test]
fn vru_kerbside_pedestrian_binds_despite_lateral_clearance() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let cfg = VehicleConfig::default_urban();
    let lateral = cfg.rss_lateral_alignment_tolerance_m + 0.5; // outside the vehicle band
    let verdict = vru_verdict(&trajectory, &[walker(1, 9.0, lateral)]);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "a kerbside pedestrian outside the vehicle lateral band must still bind (they can step in)");
}

/// Derate-only invariant: absent VRU input is byte-identical to the
/// pre-WS-2 path (None and an empty scene both admit the clean trajectory).
#[test]
fn vru_absent_channel_is_byte_identical() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let cfg = VehicleConfig::default_urban();
    let without = validate_trajectory_slow_capped(
        &trajectory, &corridor, &[], &cfg, None, FleetPosture::Nominal,
        None, None, None, None, FrameTrust::Trusted,
    );
    let empty = vru_verdict(&trajectory, &[]);
    assert_eq!(without, TrajectoryVerdict::Accept);
    assert_eq!(empty, without, "an empty scene must not change the verdict");
}

/// Fail-closed: a non-finite pedestrian is a perception fault → MRC.
#[test]
fn vru_non_finite_pedestrian_mrcs() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);
    let verdict = vru_verdict(&trajectory, &[walker(1, f64::NAN, 0.0)]);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "an unlocalizable pedestrian must fail closed");
}
