//! **Mick** — the LLM "brain" (System-2) intent seam for Occy.
//!
//! Mick *proposes* high-level typed **intent**; it never commands the actuator and
//! never describes the world. An intent sets only the **goal / maneuver** of a
//! plan — the WORLD it is planned against (corridor, objects, posture, kinematic
//! envelope) comes from perception / KIRRA, never from Mick. Occy *grounds* the
//! intent into a trajectory inside that world; the #131 checker then *bounds* it
//! (RSS + containment). So even a hallucinated or adversarial intent cannot make
//! the robot unsafe: at worst Occy stops short / refuses the maneuver, and KIRRA
//! rejects an unsafe path. This is the doer-checker thesis with a black-box doer —
//! the same safety case holds whatever (or whoever) authored the intent.
//!
//! Distinct from the main crate's low-level `action_filter` (an LLM `cmd_vel`
//! scalar → governor sanitization): that is a *command* gate; this is the
//! *intent → plan* bridge that routes Mick through the full Occy + KIRRA pipeline.

use serde::Deserialize;

use crate::{Goal, PlanInput, PlanOutput, Planner, Pose};

/// A high-level intent the LLM brain proposes — the Mick → Occy contract. It maps
/// ONLY to the goal / maneuver of a plan; it can express nothing about the world
/// or the actuator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MickIntent {
    /// Drive toward a world-frame goal point. Occy plans the route within the
    /// corridor and stops short of any hazard; it never drives *to* the point if
    /// getting there is unsafe.
    GoTo { x_m: f64, y_m: f64 },
    /// Change to the lane at `target_offset_m` from the current path centerline.
    /// Honored only if the lane-line rules permit the crossing and the corridor
    /// fits — Occy's behavioral layer adjudicates; an unlawful change is ignored.
    LaneChange { target_offset_m: f64 },
    /// Stop and hold.
    Hold,
}

/// LLM JSON wire schema (tagged on `"intent"`). Kept separate from [`MickIntent`]
/// so the public type is decoupled from the wire format.
#[derive(Deserialize)]
#[serde(tag = "intent")]
enum IntentJson {
    #[serde(rename = "go_to")]
    GoTo { x_m: f64, y_m: f64 },
    #[serde(rename = "lane_change")]
    LaneChange { target_offset_m: f64 },
    #[serde(rename = "hold")]
    Hold,
}

impl MickIntent {
    /// Parse the LLM's JSON intent into a typed [`MickIntent`]. **Fail-closed**: any
    /// malformed / unknown-tag / non-finite payload returns `Err` so the caller
    /// HOLDs rather than acting on garbage — a hallucinated `NaN` goal must never
    /// flow into the planner.
    pub fn from_llm_json(raw: &str) -> Result<Self, &'static str> {
        let parsed: IntentJson =
            serde_json::from_str(raw).map_err(|_| "MICK_JSON_PARSE_ERROR")?;
        let intent = match parsed {
            IntentJson::GoTo { x_m, y_m } => MickIntent::GoTo { x_m, y_m },
            IntentJson::LaneChange { target_offset_m } => MickIntent::LaneChange { target_offset_m },
            IntentJson::Hold => MickIntent::Hold,
        };
        if !intent.is_finite() {
            return Err("MICK_NONFINITE_INTENT");
        }
        Ok(intent)
    }

    fn is_finite(&self) -> bool {
        match self {
            MickIntent::GoTo { x_m, y_m } => x_m.is_finite() && y_m.is_finite(),
            MickIntent::LaneChange { target_offset_m } => target_offset_m.is_finite(),
            MickIntent::Hold => true,
        }
    }
}

/// Ground a Mick intent into a trajectory: the intent overrides ONLY the goal /
/// maneuver on the perception-derived `world` plan input; everything safety-bearing
/// (corridor, objects, posture, channels) is re-borrowed unchanged, and Occy plans
/// within it. The returned trajectory is still a *proposal* — the #131 checker
/// validates it downstream. Fail-closed on a non-finite intent (→ safe stop).
///
/// Generic over the planner so the same grounding holds for any doer behind the
/// seam (the geometric Occy today, a learned planner tomorrow).
pub fn plan_for_intent(
    planner: &mut impl Planner,
    intent: &MickIntent,
    world: &PlanInput,
) -> PlanOutput {
    match *intent {
        MickIntent::Hold => PlanOutput::safe_stop(world.ego.pose),
        MickIntent::GoTo { x_m, y_m } => {
            if !x_m.is_finite() || !y_m.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            // Override ONLY the goal; keep the world's heading reference.
            let goal = Goal {
                target: Pose { x_m, y_m, heading_rad: world.goal.target.heading_rad },
            };
            planner.plan(&PlanInput { goal, ..world.clone() })
        }
        MickIntent::LaneChange { target_offset_m } => {
            if !target_offset_m.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            planner.plan(&PlanInput { lane_change_to_m: Some(target_offset_m), ..world.clone() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EgoState, GeometricPlanner, LaneBoundary, LineType, PerceivedObject, ProposalKind,
        TrajectoryVerdict,
    };
    use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
    use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
    use kirra_runtime_sdk::verifier::FleetPosture;

    /// A perception-derived world: ego at x=5, a placeholder goal (the intent
    /// overrides it), and whatever objects / lane lines the test supplies.
    fn world<'a>(
        map: &'a dyn CorridorSource,
        objects: &'a [PerceivedObject],
        lanes: &'a [LaneBoundary],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects,
            controls: &[],
            lane_boundaries: lanes,
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
        }
    }

    fn stopped_car(x: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: Point { x_m: x, y_m: 0.0 },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }
    }

    fn admits(traj: &[crate::TrajectoryPoint], corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> bool {
        matches!(
            validate_trajectory_slow(traj, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        )
    }

    #[test]
    fn reasonable_intent_plans_toward_the_goal_and_kirra_admits() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }, &w);
        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 10.0, "Mick's GoTo drives the ego toward the goal, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &[]), "KIRRA admits the grounded plan");
    }

    #[test]
    fn unsafe_intent_is_grounded_by_occy_and_kirra_not_obeyed() {
        // Mick says "go to x=40", but a stopped car blocks the lane at x=25. Occy
        // grounds the intent — it STOPS SHORT of the obstacle rather than driving to
        // the point Mick named — and KIRRA admits the safe trajectory. The LLM's
        // intent does not override safety: the doer is bounded whatever it proposes.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(25.0)];
        let w = world(&corr, &objs, &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }, &w);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 25.0, "stops short of the obstacle Mick told it to drive past, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &objs), "and the bounded plan is admissible");
    }

    #[test]
    fn unlawful_lane_change_intent_is_refused() {
        // Mick proposes a lane change across a SOLID line; Occy's behavioral layer
        // refuses it (stays in lane). Even a maneuver intent is adjudicated, not obeyed.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let solid = [LaneBoundary { y_m: -0.5, line: LineType::Solid }];
        let w = PlanInput {
            goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
            ..world(&corr, &[], &solid)
        };
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::LaneChange { target_offset_m: -3.0 }, &w);
        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y > -0.5, "solid line → lane-change intent refused (no crossing), got {min_y}");
    }

    #[test]
    fn hold_intent_is_a_safe_stop() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::Hold, &w);
        assert_eq!(out.kind, ProposalKind::SafeStop);
        assert!(out.trajectory.iter().all(|t| t.velocity_mps == 0.0));
    }

    #[test]
    fn nonfinite_intent_fails_closed_to_a_safe_stop() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: f64::NAN, y_m: 0.0 }, &w);
        assert_eq!(out.kind, ProposalKind::SafeStop, "a NaN goal must not flow into the planner");
    }

    #[test]
    fn malformed_or_adversarial_llm_json_is_fail_closed() {
        // Not JSON; unknown action tag; and a non-finite (overflow → Inf) number all
        // reject — the caller HOLDs rather than acting on a hallucination.
        assert!(MickIntent::from_llm_json("the robot should floor it").is_err());
        assert!(MickIntent::from_llm_json(r#"{"intent":"deploy_at_max_velocity"}"#).is_err());
        assert!(MickIntent::from_llm_json(r#"{"intent":"go_to","x_m":1e400,"y_m":0.0}"#).is_err());
        // A well-formed intent parses to the typed value.
        assert_eq!(
            MickIntent::from_llm_json(r#"{"intent":"go_to","x_m":40.0,"y_m":0.0}"#).unwrap(),
            MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }
        );
        assert_eq!(MickIntent::from_llm_json(r#"{"intent":"hold"}"#).unwrap(), MickIntent::Hold);
    }
}
