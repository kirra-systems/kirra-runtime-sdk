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
    state::{
        AcceptedTrajectory, EgoOdom, Pose, TrajectoryPoint, TrajectoryVerdict, DEFAULT_MAX_AGE_MS,
    },
    validation::{
        check_command_conforms, ConformanceVerdict, IncomingControl, VELOCITY_TOLERANCE_MPS,
    },
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

#[test]
fn conforming_command_accepts() {
    // Fresh trajectory, cmd velocity == nearest pose velocity, steering in range.
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts_at(10.0, 10, 5.0, 0.1));
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
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
    let traj = fresh_accepted(promoted, straight_pts_at(10.0, 10, 5.0, 0.1));
    let cmd = IncomingControl {
        velocity_mps: 5.0,
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
    let cmd = IncomingControl {
        velocity_mps: 5.0,
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
fn overspeed_command_mrcs() {
    // Arm C: cmd velocity beyond nearest pose velocity + tolerance.
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = fresh_accepted(promoted, straight_pts_at(10.0, 10, 5.0, 0.1));
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
    let traj = fresh_accepted(promoted, straight_pts_at(10.0, 10, 5.0, 0.1));
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

// ---------------------------------------------------------------------------
// S1 fix (#1024): the lateral-acceleration / rollover envelope on the OUTGOING
// command. Arm D previously bounded steering only against the STATIC rack limit
// and never bounded lateral acceleration, so a within-rack steer at ODD speed
// (a_lat = v²·tan(δ)/L far above the envelope) passed conformance and was
// republished verbatim → rollover. These drive the real checker envelope.
// ---------------------------------------------------------------------------

use kirra_trajectory::state::LateralEnvelope;

/// A fresh Accept record carrying the posture-composed lateral envelope, exactly
/// as the slow loop attaches it at the promote site.
fn accepted_with_envelope(
    promoted_at_ms: u64,
    pts: Vec<TrajectoryPoint>,
    cfg: &VehicleConfig,
    posture: FleetPosture,
) -> AcceptedTrajectory {
    AcceptedTrajectory::with_verdict("av_01", 1, pts, TrajectoryVerdict::Accept, promoted_at_ms)
        .with_lateral_envelope(Some(LateralEnvelope::from_contract(
            &cfg.to_posture_kinematics_contract(posture),
        )))
}

#[test]
fn s1_within_rack_but_over_lateral_accel_mrcs() {
    // THE FINDING. 0.3 rad ≈ 17.2° is well within the 35° rack limit, but at
    // 10 m/s the bicycle-model lateral accel is a_lat = 100·tan(0.3)/2.8
    // ≈ 11.05 m/s² — ~3× the 3.5 m/s² envelope (+0.5 tol). With the envelope
    // attached the fast loop now MRCs it.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = accepted_with_envelope(
        promoted,
        straight_pts(10, 10.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    let cmd = IncomingControl {
        velocity_mps: 10.0,
        steering_rad: 0.3,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
        "a within-rack steer whose lateral accel exceeds the envelope must MRC",
    );
}

#[test]
fn s1_legacy_record_without_envelope_admits_the_rollover_command() {
    // The SAME command against a record with NO lateral envelope (a legacy /
    // pre-#1024 record) is admitted — |0.3| ≤ max_steering_rad on the static
    // fallback path. This pins that the None path is byte-identical to the old
    // behaviour AND documents precisely the gap the envelope closes.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = fresh_accepted(promoted, straight_pts(10, 10.0, 0.1)); // envelope = None
    let cmd = IncomingControl {
        velocity_mps: 10.0,
        steering_rad: 0.3,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
        "static-limit fallback (no envelope) stays byte-identical — this is the gap S1 closes",
    );
}

#[test]
fn s1_command_within_lateral_envelope_accepts() {
    // 0.05 rad at 10 m/s → a_lat = 100·tan(0.05)/2.8 ≈ 1.79 m/s² < 3.5 (+0.5).
    // "Drive gently, don't stop" — a command inside the envelope still passes.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = accepted_with_envelope(
        promoted,
        straight_pts(10, 10.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    let cmd = IncomingControl {
        velocity_mps: 10.0,
        steering_rad: 0.05,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );
}

#[test]
fn s1_degraded_tightens_lateral_envelope() {
    // 0.25 rad at 5 m/s → a_lat = 25·tan(0.25)/2.8 ≈ 2.28 m/s². Under Nominal
    // (3.5 +0.5) this passes; under Degraded (MRC lateral 1.5 +0.5 = 2.0) it
    // MRCs. Same command, posture-composed envelope decides — Degraded is
    // tighter, exactly as the slow loop enforces.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.25,
        stamp_ms: now,
    };

    let nominal = accepted_with_envelope(
        promoted,
        straight_pts(10, 5.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    assert_eq!(
        check_command_conforms(&cmd, &nominal, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );

    let degraded = accepted_with_envelope(
        promoted,
        straight_pts(10, 5.0, 0.1),
        &cfg,
        FleetPosture::Degraded,
    );
    assert_eq!(
        check_command_conforms(&cmd, &degraded, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
        "the MRC contract's tighter lateral limit must reject a steer the Nominal envelope admits",
    );
}

#[test]
fn s1_degraded_tightens_hard_steering_limit() {
    // 0.4 rad ≈ 22.9° at 2 m/s: lateral accel is tiny (a_lat ≈ 0.60 m/s²), so
    // only the HARD steering limit differs. Within the 35° Nominal rack (pass)
    // but beyond the 15° MRC limit (MRC) — the posture-composed D1 bound.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let cmd = IncomingControl {
        velocity_mps: 2.0,
        steering_rad: 0.4,
        stamp_ms: now,
    };

    let nominal = accepted_with_envelope(
        promoted,
        straight_pts(10, 2.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    assert_eq!(
        check_command_conforms(&cmd, &nominal, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );

    let degraded = accepted_with_envelope(
        promoted,
        straight_pts(10, 2.0, 0.1),
        &cfg,
        FleetPosture::Degraded,
    );
    assert_eq!(
        check_command_conforms(&cmd, &degraded, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
    );
}

#[test]
fn s1_non_finite_command_fails_closed() {
    // A NaN comparison is always false, so a non-finite steer/velocity would
    // slip every bound below. The gate must fail closed — with OR without an
    // envelope.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = accepted_with_envelope(
        promoted,
        straight_pts(10, 5.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    for (v, s) in [
        (f64::NAN, 0.0),
        (5.0, f64::NAN),
        (f64::INFINITY, 0.0),
        (5.0, f64::INFINITY),
    ] {
        let cmd = IncomingControl {
            velocity_mps: v,
            steering_rad: s,
            stamp_ms: now,
        };
        assert_eq!(
            check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
            ConformanceVerdict::MRCFallback,
            "non-finite command ({v}, {s}) must fail closed",
        );
    }
}

// ---------------------------------------------------------------------------
// B1 regression — a `Clamp` verdict must derate the forwarded command.
//
// The finding (verified on 8ea3e90): the checker computed `ClampLinear(v)`,
// discarded it, and `check_command_conforms` gated a `Clamp`-verdict command
// against the ORIGINAL planner velocity — so a command at the unclamped speed
// PASSED despite the checker requiring a derate. These tests drive the REAL
// checker (`validate_trajectory_slow_with_envelope`) so the ceiling is the
// checker's own value, not a hand-set fixture, and assert the fast loop now
// gates against it. Companion to the ROS suite; kept here so the checker
// -coverage gate (`-p kirra-trajectory`) measures the new arm.
// ---------------------------------------------------------------------------
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::FleetPosture;
use kirra_trajectory::MockCorridorSource;

/// Straight poses starting at `x0` (so the vehicle footprint behind the ego
/// stays inside the corridor — containment is checked on the full footprint).
fn straight_pts_at(x0: f64, n: usize, v: f64, dt: f64) -> Vec<TrajectoryPoint> {
    (0..n)
        .map(|i| TrajectoryPoint {
            pose: Pose {
                x_m: x0 + (i as f64) * v * dt,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: v,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

/// Produce a REAL `Clamp` verdict + its effective envelope: a 5 m/s straight
/// trajectory in a wide corridor with no objects, derated by a perception cap
/// of `cap` m/s. The only clamp that fires is the ODD-speed cap, so the
/// checker returns `ClampLinear(cap)` per over-cap pose → a known ceiling.
fn clamp_verdict_and_envelope(cap: f64) -> (TrajectoryVerdict, Option<Vec<f64>>) {
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let traj = straight_pts_at(10.0, 10, 5.0, 0.1);
    let cfg = VehicleConfig::default_urban();
    let (verdict, reason, envelope) =
        kirra_trajectory::validation::validate_trajectory_slow_with_envelope(
            &traj,
            &corridor,
            &[],
            &cfg,
            None,
            FleetPosture::Nominal,
            Some(cap), // the perception-derate cap → per-pose ClampLinear(cap)
            None,
            None,
            None,
            FrameTrust::Trusted,
        );
    assert_eq!(
        reason, None,
        "a pure speed-cap derate carries no refusal reason"
    );
    (verdict, envelope)
}

#[test]
fn b1_clamp_verdict_derates_the_conformance_ceiling() {
    let cap = 2.0;
    let (verdict, envelope) = clamp_verdict_and_envelope(cap);
    assert_eq!(
        verdict,
        TrajectoryVerdict::Clamp,
        "the speed cap must Clamp, not Accept"
    );
    let ceilings = envelope.expect("a Clamp verdict must carry the effective envelope");

    let promoted = 100_000;
    let now = promoted + 50; // lands on pose 1 (elapsed 0.05 s, poses at 0.1 s steps)
    let traj = AcceptedTrajectory::with_verdict(
        "av_01",
        1,
        straight_pts_at(10.0, 10, 5.0, 0.1),
        TrajectoryVerdict::Clamp,
        promoted,
    )
    .with_effective_ceiling(Some(ceilings.clone()));

    // Sanity — AND per-index pins (kill any index-shift mutant in the envelope
    // accumulation, e.g. `i + 1` → `i * 1`): pose 0 is the CURRENT pose, never
    // derated by a segment, so its ceiling stays the planner speed; every
    // downstream pose is clamped to the cap; the LAST pose is clamped too (an
    // off-by-one at the tail would leave it at the planner speed).
    assert_eq!(
        ceilings[0], 5.0,
        "pose 0 (current) must keep the planner speed, not be derated: {ceilings:?}"
    );
    assert!(
        ceilings.iter().skip(1).all(|&c| c <= cap + 1e-9),
        "post-current poses must be clamped to the cap: {ceilings:?}"
    );
    assert!(
        *ceilings.last().unwrap() <= cap + 1e-9,
        "the last pose must be clamped (no tail off-by-one): {ceilings:?}"
    );

    let cfg = VehicleConfig::default_urban();
    let ego = EgoOdom::default();

    // 🔴 THE B1 CASE: a command at the planner's ORIGINAL (unclamped) 5 m/s on
    // a Clamp verdict must now FAIL conformance → MRC. Before the fix it passed.
    let unclamped = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&unclamped, &traj, &ego, &cfg, now),
        ConformanceVerdict::MRCFallback,
        "a command at the unclamped speed must be refused on a Clamp verdict (B1)"
    );

    // A command AT the derated ceiling (within tolerance) must PASS — the
    // vehicle drives, just slower, exactly as the Clamp contract intends.
    let at_ceiling = IncomingControl {
        velocity_mps: cap,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&at_ceiling, &traj, &ego, &cfg, now),
        ConformanceVerdict::Accept,
        "a command at the derated ceiling must pass (Clamp = drive slower, not stop)"
    );

    // And just above the ceiling+tolerance must fail.
    let over_ceiling = IncomingControl {
        velocity_mps: cap + VELOCITY_TOLERANCE_MPS + 0.5,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&over_ceiling, &traj, &ego, &cfg, now),
        ConformanceVerdict::MRCFallback,
    );

    // Boundary pin (kills `>` → `>=`): a command EXACTLY at
    // `ceiling + VELOCITY_TOLERANCE_MPS` is the last ACCEPTED value — the gate
    // is `>`, strict. `>=` would MRC it.
    let exactly_at_bound = IncomingControl {
        velocity_mps: cap + VELOCITY_TOLERANCE_MPS,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&exactly_at_bound, &traj, &ego, &cfg, now),
        ConformanceVerdict::Accept,
        "a command exactly at ceiling + tolerance must PASS (the bound is strict `>`)"
    );
}

/// Copilot #898 fail-closed hardening: a `Clamp` verdict whose envelope is
/// `Some` but SHORTER than `points` (a missing ceiling entry at the nearest
/// pose) must MRC — never silently fall back to the planner speed (which would
/// reintroduce B1). Also kills any mutant on that fail-closed arm.
#[test]
fn b1_short_ceiling_on_a_clamp_verdict_fails_closed() {
    let promoted = 100_000;
    let now = promoted + 50; // nearest pose = index 1
    let traj = AcceptedTrajectory::with_verdict(
        "av_01",
        1,
        straight_pts_at(10.0, 10, 5.0, 0.1),
        TrajectoryVerdict::Clamp,
        promoted,
    )
    // Envelope length 1 — index 1 (the nearest pose) is MISSING.
    .with_effective_ceiling(Some(vec![5.0]));
    let cfg = VehicleConfig::default_urban();
    // Even a modest command must MRC: the derate for this pose is unknown, so
    // fail closed rather than trust the planner speed.
    let cmd = IncomingControl {
        velocity_mps: 1.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::MRCFallback,
        "a Some-but-short ceiling must fail closed, not fall back to planner speed"
    );
}

#[test]
fn b1_accept_path_is_byte_identical_no_envelope() {
    // The honest Accept path is unchanged: no cap → Accept, envelope None, and
    // a command at the planner speed still passes (the fix must not over-derate
    // a trajectory the checker admitted at full speed).
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let traj_pts = straight_pts_at(10.0, 10, 5.0, 0.1);
    let cfg = VehicleConfig::default_urban();
    let (verdict, _r, envelope) =
        kirra_trajectory::validation::validate_trajectory_slow_with_envelope(
            &traj_pts,
            &corridor,
            &[],
            &cfg,
            None,
            FleetPosture::Nominal,
            None, // no derate
            None,
            None,
            None,
            FrameTrust::Trusted,
        );
    assert_eq!(verdict, TrajectoryVerdict::Accept);
    assert_eq!(
        envelope, None,
        "an Accept verdict carries no envelope (byte-identical fast path)"
    );

    let promoted = 100_000;
    let now = promoted + 50;
    let traj =
        AcceptedTrajectory::with_verdict("av_01", 1, traj_pts, TrajectoryVerdict::Accept, promoted);
    let cmd = IncomingControl {
        velocity_mps: 5.0,
        steering_rad: 0.0,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );
}
