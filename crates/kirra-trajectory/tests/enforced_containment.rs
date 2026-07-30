//! #1213 — every released trajectory is containment-checked using the steering
//! angle that will actually be executed.
//!
//! THE FINDING these tests pin: the per-pose gate could clamp steering (P5a rack
//! limit / P5b slew ceiling / P6 lateral envelope) and then DISCARD the clamped
//! value — `ClampSteering(_) => { clamp_seen = true; }`. Containment had already
//! run, on the arc the PLANNER proposed. Every consumer treats `Clamp` as
//! admitted (`Accept | Clamp`), so the arc the vehicle actually drove was
//! released without ever being containment-checked.
//!
//! A clamped arc is WIDER through a turn, so "less steering" is not "safer": the
//! reduced command departs the corridor on the OUTSIDE of the bend, exactly
//! where the proposed arc was comfortably inside. The tests below construct that
//! case, and its converse, so the gate cannot be satisfied by refusing every
//! clamp.

use kirra_core::corridor::Point;
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::trajectory::Pose;
use kirra_core::FleetPosture;
use kirra_trajectory::state::{TrajectoryPoint, TrajectoryVerdict};
use kirra_trajectory::validation::{
    validate_trajectory_slow_detailed, validate_trajectory_slow_with_envelope, SlowLoopOutcome,
    TrajectoryRefusalReason,
};
use kirra_trajectory::MockCorridorSource;
use kirra_trajectory::VehicleConfig;

const DT: f64 = 0.2;
const SPEED_MPS: f64 = 2.0;
const WHEELBASE_M: f64 = 2.0;
/// The rack limit the checker will clamp TO. Deliberately far below the
/// proposed angle so the clamp is unambiguous rather than marginal.
const MAX_STEERING_DEG: f64 = 10.0;
/// What the planner proposes — a turn the rack cannot deliver.
const PROPOSED_STEERING_DEG: f64 = 30.0;
const SEGMENTS: usize = 12;
/// Inside the window found by construction: wide enough that the PROPOSED arc
/// (plus footprint, plus the 0.40 m Trusted margin, plus the per-segment sagitta
/// inflation) is contained, and narrow enough that the understeered arc is not.
const CORRIDOR_HALF_WIDTH_M: f64 = 2.5;
/// Wide enough to contain BOTH arcs — the availability arm.
const WIDE_CORRIDOR_HALF_WIDTH_M: f64 = 5.0;

/// A vehicle whose ONLY binding limit in these scenarios is the steering rack.
/// No ODD cap and generous accel/brake, so no LINEAR clamp fires and the single
/// variable under test is the steering axis.
fn rack_limited_config() -> VehicleConfig {
    let mut cfg = VehicleConfig::default_urban();
    cfg.wheelbase_m = WHEELBASE_M;
    cfg.half_length_m = 0.5;
    cfg.half_width_m = 0.4;
    cfg.max_steering_rad = MAX_STEERING_DEG.to_radians();
    cfg.odd_speed_cap_mps = None;
    cfg.max_speed_mps = 20.0;
    cfg.max_accel_mps2 = 5.0;
    cfg.max_decel_mps2 = 8.0;
    cfg
}

/// Forward-integrate a constant-steering arc with the SAME bicycle model
/// `pose_pair_to_command` inverts, so the checker recovers exactly
/// `steering_deg` from the pose pairs and the clamp fires deterministically —
/// not as an artifact of a hand-placed pose sequence.
fn arc_poses(steering_deg: f64, n: usize) -> Vec<TrajectoryPoint> {
    let arc_len = SPEED_MPS * DT;
    let d_heading = arc_len * steering_deg.to_radians().tan() / WHEELBASE_M;
    let (mut x, mut y, mut h) = (0.0_f64, 0.0_f64, 0.0_f64);
    let mut out = Vec::with_capacity(n + 1);
    out.push(TrajectoryPoint {
        pose: Pose {
            x_m: x,
            y_m: y,
            heading_rad: h,
        },
        velocity_mps: SPEED_MPS,
        time_from_start_s: 0.0,
    });
    for i in 0..n {
        let h1 = h + d_heading;
        if d_heading.abs() < 1e-12 {
            x += arc_len * h.cos();
            y += arc_len * h.sin();
        } else {
            let radius = arc_len / d_heading;
            x += radius * (h1.sin() - h.sin());
            y += radius * (h.cos() - h1.cos());
        }
        h = h1;
        out.push(TrajectoryPoint {
            pose: Pose {
                x_m: x,
                y_m: y,
                heading_rad: h,
            },
            velocity_mps: SPEED_MPS,
            time_from_start_s: ((i + 1) as f64) * DT,
        });
    }
    out
}

/// A corridor of half-width `w` wrapped around `path`, extended `pad` metres
/// beyond each end so the end-caps (which also bind the lateral margin) do not
/// dominate the verdict.
fn corridor_around(path: &[TrajectoryPoint], w: f64, pad: f64) -> MockCorridorSource {
    let mut left = Vec::with_capacity(path.len() + 2);
    let mut right = Vec::with_capacity(path.len() + 2);
    let extend = |p: &TrajectoryPoint, d: f64| Pose {
        x_m: p.pose.x_m + d * p.pose.heading_rad.cos(),
        y_m: p.pose.y_m + d * p.pose.heading_rad.sin(),
        heading_rad: p.pose.heading_rad,
    };
    let push = |pose: Pose, left: &mut Vec<Point>, right: &mut Vec<Point>| {
        let (nx, ny) = (-pose.heading_rad.sin(), pose.heading_rad.cos());
        left.push(Point {
            x_m: pose.x_m + w * nx,
            y_m: pose.y_m + w * ny,
        });
        right.push(Point {
            x_m: pose.x_m - w * nx,
            y_m: pose.y_m - w * ny,
        });
    };
    push(extend(&path[0], -pad), &mut left, &mut right);
    for p in path {
        push(p.pose, &mut left, &mut right);
    }
    push(extend(&path[path.len() - 1], pad), &mut left, &mut right);
    MockCorridorSource::from_boundaries(left, right)
}

fn run(
    path: &[TrajectoryPoint],
    corridor: &MockCorridorSource,
) -> (TrajectoryVerdict, Option<TrajectoryRefusalReason>) {
    let cfg = rack_limited_config();
    let (verdict, reason, _) = validate_trajectory_slow_with_envelope(
        path,
        corridor,
        &[],
        &cfg,
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    );
    (verdict, reason)
}

/// THE HEADLINE (#1213 done-when): a proposal that fits the corridor is refused
/// because the arc the checker's OWN clamp produces does not.
///
/// This trajectory passes under the old check by construction — the refusal
/// reason is `EnforcedContainmentBreach`, which is only reachable AFTER pass A
/// admitted the proposed geometry. Had the proposed arc breached, the reason
/// would be the plain `ContainmentBreach` returned earlier. So the assertion
/// below is simultaneously "the new gate refuses it" and "the old gate did not".
#[test]
fn a_clamped_arc_that_leaves_the_corridor_is_refused_though_the_proposal_fitted() {
    let proposed = arc_poses(PROPOSED_STEERING_DEG, SEGMENTS);
    // Corridor hugged to the PROPOSED arc: wide enough for the footprint plus
    // the 0.40 m Trusted containment margin, not wide enough to absorb a
    // 30°→10° understeer.
    let corridor = corridor_around(&proposed, CORRIDOR_HALF_WIDTH_M, 3.0);

    let (verdict, reason) = run(&proposed, &corridor);

    assert_eq!(
        verdict,
        TrajectoryVerdict::MRCFallback,
        "the enforced arc leaves the corridor — the trajectory must not be released"
    );
    assert_eq!(
        reason,
        Some(TrajectoryRefusalReason::EnforcedContainmentBreach),
        "the refusal must name the ENFORCED geometry; a plain ContainmentBreach \
         here would mean the proposal itself failed and the test proved nothing"
    );
}

/// NON-VACUITY (the availability half): the same steering clamp, in a corridor
/// wide enough to contain the understeered arc, is still ADMITTED.
///
/// Without this, the property could be satisfied by refusing every clamp — which
/// is safe and useless. The user-visible contract is "clamp + recheck passed →
/// release the clamped command", and this is that arm.
#[test]
fn the_same_clamp_is_admitted_when_the_enforced_arc_stays_inside() {
    let proposed = arc_poses(PROPOSED_STEERING_DEG, SEGMENTS);
    // Wide enough to contain both arcs. The clamp still fires — only the
    // containment outcome differs.
    let corridor = corridor_around(&proposed, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0);

    let (verdict, reason) = run(&proposed, &corridor);

    assert_eq!(
        reason, None,
        "a containable clamp carries no refusal reason"
    );
    assert_eq!(
        verdict,
        TrajectoryVerdict::Clamp,
        "the steering clamp must still be reported as a derate, not upgraded to Accept"
    );
}

/// The clamp is real, and it is the STEERING axis that fires — pinned so a
/// future contract change cannot turn the two tests above into vacuous passes by
/// removing the clamp entirely (both would then read `Accept`, and the headline
/// test's refusal would have to come from somewhere else).
#[test]
fn the_scenario_actually_provokes_a_steering_clamp() {
    let proposed = arc_poses(PROPOSED_STEERING_DEG, SEGMENTS);
    let (verdict, _) = run(
        &proposed,
        &corridor_around(&proposed, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0),
    );
    assert_eq!(verdict, TrajectoryVerdict::Clamp);

    // And the same geometry WITHIN the rack limit is a clean Accept — so the
    // Clamp above is attributable to the steering angle, not to the arc shape.
    let within_limit = arc_poses(MAX_STEERING_DEG - 2.0, SEGMENTS);
    let (verdict, reason) = run(
        &within_limit,
        &corridor_around(&within_limit, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0),
    );
    assert_eq!(reason, None);
    assert_eq!(
        verdict,
        TrajectoryVerdict::Accept,
        "an unclamped arc must take the untouched path — no re-containment, no derate"
    );
}

/// STRAIGHTER IS NOT SAFER — and, in the same pair, PERMANENT IS NOT TRANSIENT.
///
/// Identical geometry and identical corridor; the only difference between the
/// two arms is how much steering authority the vehicle has. That makes this the
/// discriminator for both properties the gate rests on:
///
///   * the arc that DEPARTS is the one that turns LESS, so a checker treating
///     reduced steering authority as inherently conservative would admit it;
///   * the permissive arm still CLAMPS — segment 0 ramps from the assumed-
///     neutral rack and trips the slew ceiling — but that clamp is TRANSIENT
///     (the rack reaches the angle, just a tick later), so it does not arm the
///     re-check and the trajectory is admitted. The rack-limited arm's clamp is
///     PERMANENT (35 deg of demand against a 10 deg rack is never reachable),
///     so it does arm, and the enforced arc is refused.
///
/// Collapsing those two clamp kinds together would refuse the permissive arm as
/// well — a trajectory the vehicle can actually follow.
#[test]
fn the_departing_arc_is_the_one_with_less_steering() {
    let proposed = arc_poses(PROPOSED_STEERING_DEG, SEGMENTS);
    let corridor = corridor_around(&proposed, CORRIDOR_HALF_WIDTH_M, 3.0);

    // The proposed (tighter) arc is inside: driving it as-is, with a rack that
    // could deliver it, is admitted.
    let mut permissive = rack_limited_config();
    permissive.max_steering_rad = 45.0_f64.to_radians();
    let (verdict, reason, _) = validate_trajectory_slow_with_envelope(
        &proposed,
        &corridor,
        &[],
        &permissive,
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    );
    assert_eq!(reason, None, "the tighter proposed arc fits this corridor");
    // `Clamp`, not `Accept`: segment 0 ramps from the assumed-neutral rack
    // (no odom → 0°) to the proposed angle in one 200 ms step, which trips the
    // slew ceiling even on a 45° rack. That clamp is small and its enforced arc
    // is contained — the trajectory is ADMITTED, which is all this arm claims.
    assert_eq!(verdict, TrajectoryVerdict::Clamp);

    // The identical geometry under the rack-limited vehicle is refused — the
    // ONLY difference is that the steering gets reduced.
    let (verdict, reason) = run(&proposed, &corridor);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback);
    assert_eq!(
        reason,
        Some(TrajectoryRefusalReason::EnforcedContainmentBreach)
    );
}

/// A straight trajectory never provokes a steering clamp, so the reconstruction
/// and the second containment pass must not run at all — the unclamped path
/// keeps its previous behaviour and its previous cost.
#[test]
fn an_unclamped_trajectory_is_unaffected() {
    let straight = arc_poses(0.0, SEGMENTS);
    let (verdict, reason) = run(&straight, &corridor_around(&straight, 3.0, 3.0));
    assert_eq!(reason, None);
    assert_eq!(verdict, TrajectoryVerdict::Accept);
}

// ---------------------------------------------------------------------------
// #1213 — the per-pose enforced steering ceiling handed to the fast loop.
// ---------------------------------------------------------------------------

fn detailed(path: &[TrajectoryPoint], corridor: &MockCorridorSource) -> SlowLoopOutcome {
    validate_trajectory_slow_detailed(
        path,
        corridor,
        &[],
        &rack_limited_config(),
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    )
}

/// The ceiling carries the angle the checker VALIDATED, not the angle the
/// planner asked for — and it is the same clamp that armed the re-check, so the
/// two halves of #1213 cannot disagree about what geometry was checked.
///
/// The wide corridor is deliberate: it makes the enforced arc CONTAINED, so the
/// trajectory is released rather than refused. A ceiling only matters on a
/// released trajectory — a refused one never reaches the fast loop.
#[test]
fn a_released_clamped_trajectory_hands_the_validated_angle_to_the_fast_loop() {
    let path = arc_poses(PROPOSED_STEERING_DEG, SEGMENTS);
    let corridor = corridor_around(&path, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0);
    let out = detailed(&path, &corridor);
    assert_eq!(
        out.verdict,
        TrajectoryVerdict::Clamp,
        "the wide corridor must RELEASE the clamped arc, else the ceiling is moot"
    );
    let ceiling = out
        .steering_ceiling_rad
        .expect("a permanent clamp must publish a ceiling");
    assert_eq!(
        ceiling.len(),
        path.len(),
        "aligned index-for-index with poses"
    );

    let rack = MAX_STEERING_DEG.to_radians();
    // Every entry is at or under the rack limit, and the clamped poses carry the
    // ENFORCED angle — not the 30 deg the planner proposed.
    for (k, &c) in ceiling.iter().enumerate() {
        assert!(
            c <= rack + 1e-12,
            "pose {k}: ceiling {c} exceeds the rack limit {rack}"
        );
    }
    assert!(
        ceiling[1..]
            .iter()
            .all(|&c| c < PROPOSED_STEERING_DEG.to_radians()),
        "the ceiling must bind BELOW the proposed angle, else it bounds nothing"
    );
}

/// A clean trajectory publishes NO ceiling, so the fast loop's D1/D2 path stays
/// byte-identical for every trajectory that was never clamped.
#[test]
fn an_unclamped_trajectory_publishes_no_steering_ceiling() {
    // Steering well inside the rack limit — nothing to clamp.
    let path = arc_poses(MAX_STEERING_DEG / 2.0, SEGMENTS);
    let corridor = corridor_around(&path, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0);
    let out = detailed(&path, &corridor);
    assert_eq!(out.verdict, TrajectoryVerdict::Accept);
    assert!(
        out.steering_ceiling_rad.is_none(),
        "no clamp ⇒ no ceiling ⇒ the fast loop is unchanged"
    );
}

/// The ceiling and the re-check are armed by the SAME rule. A transient P5b
/// slew clamp populates neither: if it published a ceiling it would bind the
/// fast loop to an angle the rack reaches a tick later, re-introducing on the
/// command path exactly the over-conservatism the re-check avoids on the plan.
#[test]
fn a_transient_rate_clamp_publishes_no_steering_ceiling() {
    let mut cfg = rack_limited_config();
    // Generous rack + lateral envelope so only the SLEW ceiling can bite.
    cfg.max_steering_rad = 45.0_f64.to_radians();
    let path = arc_poses(30.0, SEGMENTS);
    let corridor = corridor_around(&path, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0);
    let out = validate_trajectory_slow_detailed(
        &path,
        &corridor,
        &[],
        &cfg,
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    );
    assert!(
        out.steering_ceiling_rad.is_none(),
        "a transient slew clamp must not bind the fast loop; got {:?}",
        out.steering_ceiling_rad
    );
}

/// The velocity ceiling is indexed the same way, and the two are independent:
/// a speed-capped arc lowers pose `i + 1`'s velocity while pose 0 keeps the
/// planner's own.
#[test]
fn the_velocity_ceiling_is_indexed_to_the_segment_end_pose() {
    let mut cfg = rack_limited_config();
    cfg.max_speed_mps = SPEED_MPS / 2.0;
    let path = arc_poses(0.0, SEGMENTS);
    let corridor = corridor_around(&path, WIDE_CORRIDOR_HALF_WIDTH_M, 3.0);
    let out = validate_trajectory_slow_detailed(
        &path,
        &corridor,
        &[],
        &cfg,
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    );
    let v = out
        .velocity_ceiling
        .expect("a speed cap publishes a velocity ceiling");
    assert!(
        (v[0] - SPEED_MPS).abs() < 1e-12,
        "pose 0 keeps the planner velocity, got {}",
        v[0]
    );
    assert!(
        v[v.len() - 1] <= SPEED_MPS / 2.0 + 1e-9,
        "the last pose is derated, got {}",
        v[v.len() - 1]
    );
}
