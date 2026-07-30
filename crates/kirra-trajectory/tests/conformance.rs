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
        check_command_conforms, ConformanceVerdict, IncomingControl, STEERING_TOLERANCE_RAD,
        VELOCITY_TOLERANCE_MPS,
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

#[test]
fn s1_steering_exactly_at_posture_limit_accepts() {
    // Boundary pin for D1 (`>` NOT `>=`): |steering| EXACTLY == the posture
    // envelope's hard steering limit. The kernel P5a clamps only when strictly
    // greater, so at-exactly-the-limit is admissible. Read the envelope's own f64
    // so the equality is bit-exact. Low speed keeps the lateral bound (D2) clear.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let env =
        LateralEnvelope::from_contract(&cfg.to_posture_kinematics_contract(FleetPosture::Nominal));
    let traj = accepted_with_envelope(
        promoted,
        straight_pts(10, 1.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    let cmd = IncomingControl {
        velocity_mps: 1.0,
        steering_rad: env.max_steering_rad, // exactly at the limit
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
        "at-exactly-the-steering-limit is admissible; a `>=` here would wrongly MRC it",
    );
}

#[test]
fn s1_static_fallback_steering_exactly_at_limit_accepts() {
    // Boundary pin for the None (legacy) path (`>` NOT `>=`): |steering| EXACTLY
    // == config.max_steering_rad → admitted. Complements `oversteer_command_mrcs`
    // (which sits just OVER the limit) so the operator is pinned on both sides.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = fresh_accepted(promoted, straight_pts(10, 1.0, 0.1)); // envelope = None
    let cmd = IncomingControl {
        velocity_mps: 1.0,
        steering_rad: cfg.max_steering_rad, // exactly at the static rack limit
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );
}

#[test]
fn s1_lateral_accel_within_tolerance_band_accepts() {
    // Pins the `+` tolerance sign (NOT `-`) and the `*` arithmetic in the lateral
    // bound. 0.0977 rad at 10 m/s → a_lat ≈ 3.5 m/s²: ABOVE max−tol (3.0) but
    // BELOW max+tol (4.0), so it is admitted. A `+`→`-` mutant would use a 3.0
    // ceiling and MRC this command; the arithmetic mutants shift the ceiling
    // enough to flip the verdict too.
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
        steering_rad: 0.0977, // a_lat = 100·tan(0.0977)/2.8 ≈ 3.50 m/s²
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
        "a command inside the tolerance band (max−tol, max+tol) must be admitted",
    );
}

#[test]
fn s1_high_speed_low_steer_within_envelope_accepts() {
    // Pins the v² term (NOT v+…) in the lateral bound. At 20 m/s with a small
    // 0.02 rad steer, a_lat = 400·tan(0.02)/2.8 ≈ 2.86 m/s² < 3.5 → admitted.
    // A mutant that turns `v·v·|tan|` into `v + v·|tan|` (≈ 20.4, over the 11.2
    // RHS) would wrongly MRC this — so an Accept here kills that mutant, where
    // the low-speed cases (v small ⇒ v² ≈ v) cannot.
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = accepted_with_envelope(
        promoted,
        straight_pts(10, 20.0, 0.1),
        &cfg,
        FleetPosture::Nominal,
    );
    let cmd = IncomingControl {
        velocity_mps: 20.0,
        steering_rad: 0.02,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
        ConformanceVerdict::Accept,
    );
}

#[test]
fn s1_non_finite_on_legacy_path_fails_closed() {
    // The non-finite guard is load-bearing ONLY on the None (legacy) path: with an
    // envelope, D2 also rejects a non-finite (v²·|tan| → NaN → not within). On the
    // None path there is no D2, so a `||`→`&&` mutation of the guard would let a
    // single non-finite field slip C and the static steering check → Accept. These
    // cases pin the `||` (each has exactly ONE non-finite field).
    let promoted = 100_000;
    let now = promoted + 50;
    let cfg = VehicleConfig::default_urban();
    let traj = fresh_accepted(promoted, straight_pts(10, 5.0, 0.1)); // envelope = None
    for (v, s) in [(f64::NAN, 0.0), (5.0, f64::NAN), (f64::INFINITY, 0.0)] {
        let cmd = IncomingControl {
            velocity_mps: v,
            steering_rad: s,
            stamp_ms: now,
        };
        assert_eq!(
            check_command_conforms(&cmd, &traj, &EgoOdom::default(), &cfg, now),
            ConformanceVerdict::MRCFallback,
            "non-finite ({v}, {s}) on the legacy path must fail closed via the guard",
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

// ---------------------------------------------------------------------------
// #1213 — bound E: the per-pose ENFORCED STEERING ceiling.
// ---------------------------------------------------------------------------

/// Build a trajectory whose poses run at `pose_v`, with the slow loop's
/// enforced steering ceiling pinned to `ceiling_rad` at every pose.
fn with_steering_ceiling(
    promoted_at_ms: u64,
    pose_v: f64,
    n: usize,
    ceiling_rad: f64,
) -> AcceptedTrajectory {
    let pts = straight_pts(n, pose_v, 0.1);
    let env = LateralEnvelope::from_contract(
        &VehicleConfig::default_urban().to_posture_kinematics_contract(FleetPosture::Nominal),
    );
    AcceptedTrajectory::with_verdict("av_01", 1, pts, TrajectoryVerdict::Clamp, promoted_at_ms)
        .with_lateral_envelope(Some(env))
        .with_steering_ceiling(Some(vec![ceiling_rad; n]))
}

/// THE GAP #1213 CLOSES AT THE FAST LOOP, and the reason D1/D2 were not enough.
///
/// The slow loop clamped this pose to 21 deg and re-checked containment on THAT
/// arc. The fast loop then receives a command that is SLOWER than the pose —
/// which bound C happily allows — asking for 30 deg.
///
/// It passes every pre-existing bound:
///   C  (velocity)      3.0 <= 5.05
///   D1 (rack limit)    30 deg < the ~35 deg posture-composed rack limit
///   D2 (lateral accel) P6 re-solved at the COMMAND's 3.0 m/s, not the pose's
///                      5.05 m/s — and P6 is LOOSER at lower speed, so 30 deg
///                      is comfortably inside the envelope there.
///
/// So without bound E the vehicle drives 30 deg of steer at a pose whose
/// geometry was only ever checked at 21 deg. That is the slow-loop defect
/// #1213 fixed, reappearing one layer down.
///
/// The control arm is what makes this non-vacuous: the IDENTICAL command on the
/// IDENTICAL trajectory is ACCEPTED once the ceiling is removed. The refusal is
/// caused by bound E and by nothing else.
#[test]
fn a_slower_command_may_not_exceed_the_steering_the_checker_validated() {
    let promoted = 100_000;
    let now = promoted + 50;
    let pose_v = 5.05;
    let validated_rad = 21.0_f64.to_radians();
    let cmd = IncomingControl {
        velocity_mps: 3.0,
        steering_rad: 30.0_f64.to_radians(),
        stamp_ms: now,
    };
    let odom = EgoOdom::default();
    let config = VehicleConfig::default_urban();

    let bound = with_steering_ceiling(promoted, pose_v, 10, validated_rad);
    assert_eq!(
        check_command_conforms(&cmd, &bound, &odom, &config, now),
        ConformanceVerdict::MRCFallback,
        "a command exceeding the validated steering angle must MRC"
    );

    // CONTROL — same command, same trajectory, ceiling removed.
    let mut unbound = bound.clone();
    unbound.effective_steering_ceiling_rad = None;
    assert_eq!(
        check_command_conforms(&cmd, &unbound, &odom, &config, now),
        ConformanceVerdict::Accept,
        "non-vacuity: without the ceiling this same command sails through C/D1/D2 \
         — which is exactly the gap"
    );
}

/// The command AT the validated angle is still admitted: bound E bounds the
/// command, it does not forbid steering.
#[test]
fn a_command_at_the_validated_steering_angle_is_admitted() {
    let promoted = 100_000;
    let now = promoted + 50;
    let validated_rad = 21.0_f64.to_radians();
    let traj = with_steering_ceiling(promoted, 5.05, 10, validated_rad);
    let cmd = IncomingControl {
        velocity_mps: 3.0,
        steering_rad: validated_rad,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(
            &cmd,
            &traj,
            &EgoOdom::default(),
            &VehicleConfig::default_urban(),
            now
        ),
        ConformanceVerdict::Accept
    );
}

/// Sign symmetry — the ceiling bounds |delta|, so the mirrored command is
/// refused identically. A ceiling that only caught one turn direction would
/// leave half the corridor unguarded.
#[test]
fn the_steering_ceiling_bounds_magnitude_in_both_directions() {
    let promoted = 100_000;
    let now = promoted + 50;
    let traj = with_steering_ceiling(promoted, 5.05, 10, 21.0_f64.to_radians());
    for sign in [1.0_f64, -1.0] {
        let cmd = IncomingControl {
            velocity_mps: 3.0,
            steering_rad: sign * 30.0_f64.to_radians(),
            stamp_ms: now,
        };
        assert_eq!(
            check_command_conforms(
                &cmd,
                &traj,
                &EgoOdom::default(),
                &VehicleConfig::default_urban(),
                now
            ),
            ConformanceVerdict::MRCFallback,
            "sign {sign} must be bounded the same"
        );
    }
}

/// FAIL CLOSED on a short ceiling vector, exactly as the velocity ceiling does.
/// A dropped entry must never silently widen the bound back to the rack limit —
/// that would reintroduce the defect precisely where the data went missing.
#[test]
fn a_missing_steering_ceiling_entry_fails_closed() {
    let promoted = 100_000;
    let now = promoted + 500; // lands late in the trajectory
    let mut traj = with_steering_ceiling(promoted, 5.05, 10, 21.0_f64.to_radians());
    // Present, but shorter than `points`.
    traj.effective_steering_ceiling_rad = Some(vec![21.0_f64.to_radians(); 2]);
    let cmd = IncomingControl {
        velocity_mps: 3.0,
        // Well within every other bound — only the missing entry can refuse it.
        steering_rad: 5.0_f64.to_radians(),
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(
            &cmd,
            &traj,
            &EgoOdom::default(),
            &VehicleConfig::default_urban(),
            now
        ),
        ConformanceVerdict::MRCFallback,
        "a Some-but-short ceiling must fail closed, not fall back to the rack limit"
    );
}

/// A non-finite ceiling entry cannot open the gate. NaN compares false against
/// every bound, so an unguarded `>` would ACCEPT here.
#[test]
fn a_non_finite_steering_ceiling_fails_closed() {
    let promoted = 100_000;
    let now = promoted + 50;
    let mut traj = with_steering_ceiling(promoted, 5.05, 10, 21.0_f64.to_radians());
    traj.effective_steering_ceiling_rad = Some(vec![f64::NAN; 10]);
    let cmd = IncomingControl {
        velocity_mps: 3.0,
        steering_rad: 5.0_f64.to_radians(),
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(
            &cmd,
            &traj,
            &EgoOdom::default(),
            &VehicleConfig::default_urban(),
            now
        ),
        ConformanceVerdict::MRCFallback
    );
}

/// BOUNDARY of bound E. The gate is `>` (strictly exceeding the ceiling plus
/// tolerance), so the command exactly AT `ceiling + tolerance` must be
/// admitted and anything past it refused. A `>=` here would refuse the
/// boundary — availability lost at the exact angle the checker validated.
#[test]
fn bound_e_admits_exactly_at_the_tolerance_and_refuses_past_it() {
    let promoted = 100_000;
    let now = promoted + 50;
    let ceiling = 21.0_f64.to_radians();
    let traj = with_steering_ceiling(promoted, 5.05, 10, ceiling);
    let at = IncomingControl {
        velocity_mps: 3.0,
        steering_rad: ceiling + STEERING_TOLERANCE_RAD,
        stamp_ms: now,
    };
    assert_eq!(
        check_command_conforms(
            &at,
            &traj,
            &EgoOdom::default(),
            &VehicleConfig::default_urban(),
            now
        ),
        ConformanceVerdict::Accept,
        "exactly at ceiling+tolerance must be admitted (the bound is strict `>`)"
    );
    let past = IncomingControl {
        steering_rad: ceiling + STEERING_TOLERANCE_RAD * 1.5,
        ..at
    };
    assert_eq!(
        check_command_conforms(
            &past,
            &traj,
            &EgoOdom::default(),
            &VehicleConfig::default_urban(),
            now
        ),
        ConformanceVerdict::MRCFallback,
        "past the tolerance must refuse"
    );
}
