//! The Occy planner endpoint core (promoted verbatim from the
//! `kirra-mick --example planner_service`, then HARDENED at the Mick seam):
//! POST a world snapshot (+ optionally a typed Mick intent), get back Occy's
//! KIRRA-validated trajectory with the checker's verdict AND — on a refusal —
//! the #893 narration reason (stable code + operator sentence).
//!
//! This serves the DOER's proposal; the governor's enforcement is the separate
//! verifier service and the verifying motor consumer. The checker verdict here
//! is the slow-loop (`validate_trajectory_slow_explained`) — advisory to the
//! doer bridge, re-enforced downstream.
//!
//! **The Mick seam (hardened, Part 2.3):**
//! * `intent` is parsed by the ONE fail-closed parse
//!   ([`MickIntent::from_llm_json`] via `parse_llm_json`) — never a second
//!   parser. A rejected intent fails closed to NO MOTION (a 422 with an empty
//!   trajectory), never to the request's default goal.
//! * finite-coordinate validation on every numeric input (ego, goal, cruise,
//!   corridor, objects) — non-finite → 422.
//! * in-map bounds: the effective goal (the intent's target when it carries
//!   one, else the request goal) must lie within the supplied corridor's
//!   bounding box inflated by [`GOAL_MARGIN_M`] — an absurd/hallucinated goal
//!   is refused at the seam instead of walking into the planner.
//! * rate limiting and the loopback bind policy live in the binary
//!   (`net::RateLimiter` / `net::enforce_bind_policy`).

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind,
};
use kirra_core::frame_integrity::FrameTrust;
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::validation::validate_trajectory_slow_explained;
use kirra_trajectory::VehicleConfig;
use serde::{Deserialize, Serialize};

/// In-map goal slack (m): how far outside the supplied corridor's bounding
/// box a goal may point. A PLUMBING bound, not a safety number — the checker
/// bounds all motion regardless; this only refuses absurd goals (a
/// hallucinated `x_m: 9e9`) at the seam, cheaply and with a specific error.
pub const GOAL_MARGIN_M: f64 = 50.0;

#[derive(Deserialize)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}
#[derive(Deserialize)]
pub struct EgoReq {
    pub x: f64,
    pub y: f64,
    pub heading: f64,
    pub speed: f64,
}
#[derive(Deserialize)]
pub struct ObjReq {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
}

#[derive(Deserialize)]
pub struct PlanRequest {
    pub ego: EgoReq,
    pub goal: Xy,
    #[serde(default = "default_cruise")]
    pub cruise: f64,
    pub left: Vec<[f64; 2]>,
    pub right: Vec<[f64; 2]>,
    #[serde(default)]
    pub objects: Vec<ObjReq>,
    /// Optional vehicle footprint/kinematics for the CHECKER. Absent → the
    /// urban-car default (4.8 m). A small differential robot MUST pass its own
    /// dimensions, or the car-sized footprint can't fit a robot-scale corridor
    /// and KIRRA MRCs every plan.
    #[serde(default)]
    pub vehicle: Option<VehicleReq>,
    /// Optional **typed Mick intent** — either the raw accepted JSON object
    /// (`{"intent":"go_to",...}`, what `mick_service` publishes on
    /// `/intent/last`) or that object as a JSON string. Parsed by the one
    /// fail-closed `MickIntent` parse; a rejected intent → 422 + NO MOTION.
    /// Absent → the request `goal` grounds as a plain `GoTo` (the pre-intent
    /// behavior, byte-identical).
    #[serde(default)]
    pub intent: Option<serde_json::Value>,
}
fn default_cruise() -> f64 {
    10.0
}

/// Vehicle profile for BOTH the checker (`VehicleConfig`) and the doer's
/// lateral-clearance target — see `docs/CONTRACT_PROFILES.md`.
#[derive(Deserialize)]
pub struct VehicleReq {
    pub class: Option<String>,
    pub wheelbase_m: Option<f64>,
    pub half_length_m: Option<f64>,
    pub half_width_m: Option<f64>,
    pub max_speed_mps: Option<f64>,
    pub max_steering_deg: Option<f64>,
    /// Per-class RSS lateral-alignment band (m).
    pub rss_lateral_alignment_tolerance_m: Option<f64>,
    /// The DOER's lateral clearance target (m).
    pub lateral_clearance_target_m: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct TrajPt {
    pub x: f64,
    pub y: f64,
    pub heading: f64,
    pub v: f64,
    pub t: f64,
}

#[derive(Debug, Serialize)]
pub struct PlanResponse {
    pub kind: String,
    pub verdict: String,
    pub trajectory: Vec<TrajPt>,
    /// #893 narration: the stable refusal code (`TRAJECTORY_*`) when the
    /// checker refused, else null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// #893 narration: the operator sentence for the refusal, else null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A seam rejection: the request never reached the planner. Fail-closed to NO
/// MOTION — the wire shape still carries `kind: SafeStop` + an empty
/// trajectory so a naive client that ignores the status code still holds.
#[derive(Debug)]
pub struct SeamRejection {
    pub code: &'static str,
    pub detail: String,
}

impl SeamRejection {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "error": self.code,
            "detail": self.detail,
            "kind": "SafeStop",
            "verdict": "IntentRejected",
            "trajectory": [],
        })
        .to_string()
    }
}

fn vehicle_config(req: &PlanRequest) -> VehicleConfig {
    let mut v = match req.vehicle.as_ref().and_then(|o| o.class.as_deref()) {
        Some(class) => VehicleConfig::for_class(class),
        None => VehicleConfig::default_urban(),
    };
    if let Some(o) = &req.vehicle {
        if let Some(x) = o.wheelbase_m {
            v.wheelbase_m = x;
        }
        if let Some(x) = o.half_length_m {
            v.half_length_m = x;
        }
        if let Some(x) = o.half_width_m {
            v.half_width_m = x;
        }
        if let Some(x) = o.max_speed_mps {
            v.max_speed_mps = x;
        }
        if let Some(x) = o.max_steering_deg {
            v.max_steering_rad = x.to_radians();
        }
        if let Some(x) = o.rss_lateral_alignment_tolerance_m {
            v.rss_lateral_alignment_tolerance_m = x;
        }
    }
    v
}

fn lateral_clearance_target(req: &PlanRequest) -> Option<f64> {
    req.vehicle
        .as_ref()
        .and_then(|o| o.lateral_clearance_target_m)
}

/// A `CorridorSource` straight off the request's boundary polylines.
pub struct ReqCorridor {
    pub left: Vec<Point>,
    pub right: Vec<Point>,
}
impl CorridorSource for ReqCorridor {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        0.95
    }
    fn age_ms(&self) -> u64 {
        10
    }
}

fn pts(v: &[[f64; 2]]) -> Vec<Point> {
    v.iter()
        .map(|p| Point {
            x_m: p[0],
            y_m: p[1],
        })
        .collect()
}

/// Finite-input validation (seam hygiene): every numeric the request carries.
fn validate_finite(req: &PlanRequest) -> Result<(), SeamRejection> {
    let finite = |vals: &[f64]| vals.iter().all(|v| v.is_finite());
    let ego_ok = finite(&[req.ego.x, req.ego.y, req.ego.heading, req.ego.speed]);
    let goal_ok = finite(&[req.goal.x, req.goal.y, req.cruise]);
    let corr_ok = req
        .left
        .iter()
        .chain(req.right.iter())
        .all(|p| finite(&[p[0], p[1]]));
    let obj_ok = req
        .objects
        .iter()
        .all(|o| finite(&[o.x, o.y, o.vx, o.vy]));
    if ego_ok && goal_ok && corr_ok && obj_ok {
        Ok(())
    } else {
        Err(SeamRejection {
            code: "NONFINITE_INPUT",
            detail: "a numeric field was NaN/Inf; refused at the seam".to_string(),
        })
    }
}

/// The world-frame target the effective intent points at, if it carries one.
fn intent_target(intent: &MickIntent) -> Option<(f64, f64)> {
    match *intent {
        MickIntent::GoTo { x_m, y_m }
        | MickIntent::RouteTo { x_m, y_m }
        | MickIntent::Yield { x_m, y_m }
        | MickIntent::CrossWhenClear { x_m, y_m }
        | MickIntent::CreepThrough { x_m, y_m } => Some((x_m, y_m)),
        _ => None,
    }
}

/// In-map bound: the effective goal must lie within the corridor's bounding
/// box inflated by [`GOAL_MARGIN_M`]. Only enforced when a corridor is
/// actually supplied (an empty corridor is already refused by the checker).
fn validate_in_map(req: &PlanRequest, target: (f64, f64)) -> Result<(), SeamRejection> {
    if req.left.is_empty() || req.right.is_empty() {
        return Ok(());
    }
    let all = req.left.iter().chain(req.right.iter());
    let (mut min_x, mut min_y, mut max_x, mut max_y) =
        (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in all {
        min_x = min_x.min(p[0]);
        min_y = min_y.min(p[1]);
        max_x = max_x.max(p[0]);
        max_y = max_y.max(p[1]);
    }
    let (gx, gy) = target;
    let inside = gx >= min_x - GOAL_MARGIN_M
        && gx <= max_x + GOAL_MARGIN_M
        && gy >= min_y - GOAL_MARGIN_M
        && gy <= max_y + GOAL_MARGIN_M;
    if inside {
        Ok(())
    } else {
        Err(SeamRejection {
            code: "INTENT_GOAL_OUT_OF_MAP",
            detail: format!(
                "goal ({gx:.1}, {gy:.1}) is outside the supplied corridor's bounds \
                 (+{GOAL_MARGIN_M} m margin); refused at the seam"
            ),
        })
    }
}

/// Resolve the effective intent: the request's typed `intent` when present
/// (the ONE fail-closed parse — a rejected intent is a [`SeamRejection`],
/// never a fallback to the request goal), else a `GoTo` at the request goal
/// (the pre-intent behavior).
fn effective_intent(req: &PlanRequest) -> Result<MickIntent, SeamRejection> {
    match &req.intent {
        None => Ok(MickIntent::GoTo {
            x_m: req.goal.x,
            y_m: req.goal.y,
        }),
        Some(value) => {
            // Accept the object form (what /intent/last publishes) or that
            // object embedded as a JSON string. Both routes land in the same
            // fail-closed parse; there is no second parser.
            let raw = match value.as_str() {
                Some(s) => s.to_string(),
                None => value.to_string(),
            };
            MickIntent::from_llm_json(&raw).map_err(|code| SeamRejection {
                code,
                detail: "typed intent failed the fail-closed parse; NO MOTION".to_string(),
            })
        }
    }
}

/// Handle one plan request: seam validation → Occy grounds the intent →
/// the KIRRA slow-loop checker bounds it and narrates a refusal.
pub fn handle_plan(req: &PlanRequest) -> Result<PlanResponse, SeamRejection> {
    validate_finite(req)?;
    let intent = effective_intent(req)?;
    let target = intent_target(&intent).unwrap_or((req.goal.x, req.goal.y));
    validate_in_map(req, target)?;

    let corr = ReqCorridor {
        left: pts(&req.left),
        right: pts(&req.right),
    };
    let objects: Vec<PerceivedObject> = req
        .objects
        .iter()
        .map(|o| PerceivedObject {
            id: o.id,
            pos: Point { x_m: o.x, y_m: o.y },
            velocity_mps: o.vx.hypot(o.vy),
            heading_rad: o.vy.atan2(o.vx),
            vel: Point {
                x_m: o.vx,
                y_m: o.vy,
            },
        })
        .collect();

    let world = PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: req.ego.x,
                y_m: req.ego.y,
                heading_rad: req.ego.heading,
            },
            linear_x_mps: req.ego.speed,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: req.goal.x,
                y_m: req.goal.y,
                heading_rad: req.ego.heading,
            },
        },
        map: &corr,
        objects: &objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    };

    // The DOER: real Occy grounds the intent. A courier class selects the
    // robot-scale planner preset; the checker's per-class profile then bounds
    // it. `class` mirrors the VehicleConfig selector.
    let mut cfg = match req.vehicle.as_ref().and_then(|o| o.class.as_deref()) {
        Some("courier") | Some("robot") | Some("sidewalk") => GeometricPlannerConfig::courier(),
        _ => GeometricPlannerConfig::default(),
    };
    cfg.cruise_speed_mps = req.cruise;
    if let Some(ct) = lateral_clearance_target(req) {
        cfg.lateral_clearance_target_m = ct;
    }
    let plan = plan_for_intent(&mut GeometricPlanner::new(cfg), &intent, &world);

    // The CHECKER: KIRRA's verdict on the proposal, WITH the #893 narration
    // reason riding alongside (the verdict core and hot type are untouched —
    // the reason is the side-channel).
    let (verdict, reason) = validate_trajectory_slow_explained(
        &plan.trajectory,
        &corr,
        &objects,
        &vehicle_config(req),
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    );

    Ok(PlanResponse {
        kind: match plan.kind {
            ProposalKind::Motion => "Motion",
            ProposalKind::SafeStop => "SafeStop",
        }
        .to_string(),
        verdict: match verdict {
            TrajectoryVerdict::Accept => "Accept",
            TrajectoryVerdict::Clamp => "Clamp",
            TrajectoryVerdict::MRCFallback => "MRCFallback",
            // Transitional registration state — never produced by the checker;
            // named for exhaustiveness (fails closed downstream regardless).
            TrajectoryVerdict::Pending => "Pending",
        }
        .to_string(),
        trajectory: plan
            .trajectory
            .iter()
            .map(|p| TrajPt {
                x: p.pose.x_m,
                y: p.pose.y_m,
                heading: p.pose.heading_rad,
                v: p.velocity_mps,
                t: p.time_from_start_s,
            })
            .collect(),
        reason_code: reason.map(|r| r.code().to_string()),
        reason: reason.map(|r| r.explain().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> PlanRequest {
        PlanRequest {
            ego: EgoReq {
                x: 2.0,
                y: 0.0,
                heading: 0.0,
                speed: 1.0,
            },
            goal: Xy { x: 40.0, y: 0.0 },
            cruise: 3.0,
            left: vec![[-5.0, 5.0], [100.0, 5.0]],
            right: vec![[-5.0, -5.0], [100.0, -5.0]],
            objects: vec![],
            vehicle: None,
            intent: None,
        }
    }

    #[test]
    fn goal_only_request_still_grounds_as_goto_and_reports_a_verdict() {
        let resp = handle_plan(&base_request()).expect("seam admits");
        assert!(resp.verdict == "Accept" || resp.verdict == "Clamp" || resp.verdict == "MRCFallback");
        assert!(!resp.trajectory.is_empty(), "a proposal is always returned");
    }

    #[test]
    fn typed_intent_object_grounds_through_the_one_parse() {
        let mut req = base_request();
        req.intent = Some(serde_json::json!({"intent":"go_to","x_m":40.0,"y_m":0.0}));
        let resp = handle_plan(&req).expect("valid intent admits");
        assert!(!resp.trajectory.is_empty());
        // The string form (the /intent/last relay shape) parses identically.
        let mut req = base_request();
        req.intent = Some(serde_json::json!(r#"{"intent":"go_to","x_m":40.0,"y_m":0.0}"#));
        handle_plan(&req).expect("string-embedded intent admits");
    }

    #[test]
    fn hold_intent_is_a_safe_stop_not_a_goal_chase() {
        let mut req = base_request();
        req.intent = Some(serde_json::json!({"intent":"hold"}));
        let resp = handle_plan(&req).expect("hold admits");
        assert_eq!(resp.kind, "SafeStop");
        assert!(resp.trajectory.iter().all(|p| p.v == 0.0));
    }

    /// Part 2.4 — a rejected intent fails closed to NO MOTION: a 422 seam
    /// rejection carrying an EMPTY trajectory, never a fallback to the
    /// request's default goal.
    #[test]
    fn unparseable_intent_fails_closed_to_no_motion_never_the_default_goal() {
        for bad in [
            serde_json::json!("just floor it, trust me"),
            serde_json::json!({"intent":"warp_speed"}),
            serde_json::json!({"intent":"go_to","x_m":"NaN","y_m":0.0}),
            serde_json::json!({"intent":"cruise"}), // missing required field
        ] {
            let mut req = base_request();
            req.intent = Some(bad.clone());
            let rej = handle_plan(&req).expect_err(&format!("{bad} must be rejected"));
            let wire: serde_json::Value = serde_json::from_str(&rej.to_json()).unwrap();
            assert_eq!(wire["kind"], "SafeStop");
            assert_eq!(
                wire["trajectory"].as_array().map(Vec::len),
                Some(0),
                "no motion may ride on a rejected intent"
            );
        }
    }

    #[test]
    fn nonfinite_world_input_is_refused_at_the_seam() {
        let mut req = base_request();
        req.ego.speed = f64::NAN;
        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");
    }

    #[test]
    fn out_of_map_goal_is_refused_at_the_seam() {
        // Direct goal.
        let mut req = base_request();
        req.goal = Xy { x: 9e6, y: 0.0 };
        assert_eq!(handle_plan(&req).unwrap_err().code, "INTENT_GOAL_OUT_OF_MAP");
        // And an intent-carried target gets the SAME bound.
        let mut req = base_request();
        req.intent = Some(serde_json::json!({"intent":"go_to","x_m":9e6,"y_m":0.0}));
        assert_eq!(handle_plan(&req).unwrap_err().code, "INTENT_GOAL_OUT_OF_MAP");
    }

    /// The #893 narration rides on a refused proposal: a corridor far too
    /// tight for the vehicle footprint forces a containment refusal, and the
    /// response carries the SPECIFIC reason — code + operator sentence.
    #[test]
    fn refused_plan_carries_the_specific_narration_reason() {
        let mut req = base_request();
        req.left = vec![[-5.0, 0.05], [100.0, 0.05]];
        req.right = vec![[-5.0, -0.05], [100.0, -0.05]];
        let resp = handle_plan(&req).expect("seam admits; the checker refuses");
        assert_eq!(resp.verdict, "MRCFallback");
        let code = resp.reason_code.expect("a refusal must carry its code");
        let sentence = resp.reason.expect("a refusal must carry its sentence");
        assert!(code.starts_with("TRAJECTORY_"), "stable vocabulary: {code}");
        assert!(
            sentence.len() > 40,
            "the sentence must be specific, not a generic marker: {sentence}"
        );
    }
}
