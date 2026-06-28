//! **Global speed-envelope invariants (proptest).** The planner's speed knobs may only ever
//! SLOW the vehicle; pinning that across arbitrary inputs is exactly where exhaustive-ish
//! coverage pays off (vs a hand-picked example):
//!
//!   1. **Cruise is a monotone ceiling, never a throttle** — a `Cruise { target }` request never
//!      raises the peak speed above the unbounded-cruise envelope, and a *lower* request never
//!      yields a faster plan than a higher one. A buggy / adversarial "go faster" cannot speed
//!      the vehicle up. (The request bounds the *target*, not the instantaneous speed — a plan
//!      that starts above the request decelerates toward it — so the invariant is stated against
//!      the baseline and monotone in the request, not `peak ≤ req`.)
//!   2. **Posture is decel-only** — a `Degraded` posture never produces a faster plan than
//!      `Nominal` for the same world. The conservative envelope can only lower the target.
//!
//! KIRRA still bounds the result downstream; these pin the *planner's* half of the contract.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, MickIntent, PlanInput,
    PlanOutput, Pose,
};
use kirra_trajectory::corridor::{CorridorSource, MockCorridorSource};
use proptest::prelude::*;

fn peak_speed(plan: &PlanOutput) -> f64 {
    plan.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max)
}

/// A long straight corridor with a far-ahead goal and no objects, so the binding speed limit is
/// the cruise target / posture envelope itself (not an obstacle, curvature, or the goal).
fn world<'a>(map: &'a dyn CorridorSource, ego_v: f64, posture: FleetPosture) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState { pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: ego_v, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 180.0, y_m: 0.0, heading_rad: 0.0 } },
        map,
        objects: &[],
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// For any two finite requests and any ego speed: neither raises the peak speed above the
    /// unbounded-cruise envelope, and the LOWER request never yields a faster plan than the
    /// higher one (the request is a monotone ceiling — "go faster" cannot speed up).
    #[test]
    fn cruise_request_is_a_monotone_ceiling_never_a_throttle(
        a in 0.0f64..40.0,
        b in 0.0f64..40.0,
        ego_v in 0.0f64..12.0,
    ) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let corridor = MockCorridorSource::straight_5m_half_width(200.0);
        let peak = |req: f64| {
            let mut planner = GeometricPlanner::default();
            peak_speed(&plan_for_intent(&mut planner, &MickIntent::Cruise { target_speed_mps: req }, &world(&corridor, ego_v, FleetPosture::Nominal)))
        };

        let baseline = peak(1.0e9); // unbounded cruise = the full posture envelope
        let p_lo = peak(lo);
        let p_hi = peak(hi);

        prop_assert!(p_hi <= baseline + 1e-6, "Cruise raised speed above the envelope: {p_hi} > baseline {baseline} (req={hi}, ego_v={ego_v})");
        prop_assert!(p_lo <= baseline + 1e-6, "Cruise raised speed above the envelope: {p_lo} > baseline {baseline} (req={lo}, ego_v={ego_v})");
        prop_assert!(p_lo <= p_hi + 1e-6, "a lower cruise {lo} went faster ({p_lo}) than a higher one {hi} ({p_hi}), ego_v={ego_v}");
    }

    /// For any ego speed: a `Degraded` posture never produces a faster plan than `Nominal`. The
    /// conservative envelope (and its decel-only clamp) can only LOWER the target.
    #[test]
    fn degraded_posture_is_never_faster_than_nominal(
        ego_v in 0.0f64..12.0,
    ) {
        let corridor = MockCorridorSource::straight_5m_half_width(200.0);
        let peak = |posture: FleetPosture| {
            let mut planner = GeometricPlanner::default();
            peak_speed(&plan_for_intent(&mut planner, &MickIntent::Cruise { target_speed_mps: 1.0e9 }, &world(&corridor, ego_v, posture)))
        };
        let nominal = peak(FleetPosture::Nominal);
        let degraded = peak(FleetPosture::Degraded);
        prop_assert!(degraded <= nominal + 1e-6, "Degraded ({degraded}) was faster than Nominal ({nominal}), ego_v={ego_v}");
    }
}
