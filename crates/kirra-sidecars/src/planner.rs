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

use kirra_core::frame_integrity::FrameTrust;
use kirra_planner::{
    behavior::{
        intent_aware_predicted_vru_speed_cap, predicted_vru_speed_cap,
        probabilistic_vru_uncertainty_radius, IntentAwarePredictedVruOccupancy, PredictedVruIntent,
        PredictedVruIntentProbability, PredictedVruOccupancy,
        DEFAULT_CROSSING_INTENT_BAND_EXTENSION_M, DEFAULT_MINIMUM_VRU_INTENT_CONFIDENCE,
        DEFAULT_VRU_YIELD_BAND_HALF_WIDTH_M, DEFAULT_VRU_YIELD_STANDOFF_M,
    },
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind,
};
use kirra_taj::object_goal::{
    resolve_object_goal, LabeledTarget, DEFAULT_GOAL_MAX_AGE_MS, DEFAULT_MIN_CONFIDENCE,
    DEFAULT_TIE_EPSILON_M,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::validation::validate_trajectory_slow_explained;
use kirra_trajectory::vru::{PedestrianScene, PerceivedPedestrian, VruRssParams};
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

/// One tracked pedestrian/VRU supplied by Taj.
///
/// Coordinates, velocity, and age are already expressed in the ego-frame
/// perception contract consumed by the checker.
#[derive(Deserialize)]
pub struct PedestrianReq {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub age_s: f64,
}

/// One future occupied point for a fused lidar-authoritative VRU track.
///
/// Coordinates use the same frame as `pedestrians` and Taj's corridor output.
#[derive(Debug, Deserialize)]
pub struct PredictedVruPointReq {
    pub time_s: f64,
    pub x: f64,
    pub y: f64,
    pub uncertainty_radius_m: f64,
}

/// One normalized intent hypothesis produced by Taj.
///
/// This is planner metadata only. It cannot create a track, create pedestrian
/// authority, remove geometric occupancy, or weaken pedestrian RSS.
#[derive(Debug, Clone, Deserialize)]
pub struct PredictedVruIntentProbabilityReq {
    pub intent: String,
    pub probability: f64,
}

/// Bounded VRU prediction produced by Taj.
///
/// The model and fallback fields remain auditable metadata. Occy's behavioral
/// decision consumes the occupied points rather than reinterpreting the model.
#[derive(Debug, Deserialize)]
pub struct PredictedVruReq {
    pub track_id: u64,
    pub model: String,
    pub intent: String,
    pub intent_confidence: f64,
    pub intent_reason: String,
    /// Complete normalized intent distribution emitted by Taj.
    ///
    /// Empty preserves compatibility with older producers.
    #[serde(default)]
    pub intent_probabilities: Vec<PredictedVruIntentProbabilityReq>,
    pub points: Vec<PredictedVruPointReq>,
    pub horizon_s: f64,
    pub step_s: f64,
    pub source_age_s: f64,
    pub frames_seen: u32,
    #[serde(default)]
    pub fallback_reason: Option<String>,
}

const MAX_PREDICTED_VRUS: usize = 64;
const MAX_PREDICTED_VRU_POINTS: usize = 16;
const OCCY_VRU_BRAKE_DECEL_MPS2: f64 = 1.0;

/// Maximum additional occupied radius contributed by crossing probability.
///
/// This is tighten-only: probability may increase uncertainty but can never
/// shrink Taj's geometric uncertainty or weaken the baseline VRU cap.
const OCCY_PROBABILISTIC_CROSSING_EXTENSION_M: f64 = 0.60;

/// Additional occupied radius contributed by waiting-near-path probability.
const OCCY_PROBABILISTIC_WAITING_EXTENSION_M: f64 = 0.25;

/// Additional occupied radius contributed by unresolved intent probability.
const OCCY_PROBABILISTIC_UNKNOWN_EXTENSION_M: f64 = 0.40;

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
    /// Taj's conservative tracked VRU classifications.
    ///
    /// Empty or absent preserves the pre-VRU planner path byte-for-byte.
    #[serde(default)]
    pub pedestrians: Vec<PedestrianReq>,
    /// Taj's bounded future occupancy for fused pedestrian-RSS tracks.
    ///
    /// Empty or absent preserves the current planner behavior. Every prediction
    /// must refer to an entry in `pedestrians`; camera-only evidence cannot
    /// introduce a planning authority through this channel.
    #[serde(default)]
    pub predicted_vrus: Vec<PredictedVruReq>,
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
    /// **Object goal** (OPT-IN) — the operator's requested thing, e.g. `"red cup"`.
    /// Resolved against `targets` into a plain `GoTo`, so it is a DESTINATION and
    /// asserts nothing about drivability (`kirra_taj::object_goal`). Absent →
    /// byte-identical prior behaviour. Supplying BOTH this and `intent` is a
    /// rejected ambiguity (two goal sources), never a silent precedence rule.
    #[serde(default)]
    pub object_goal: Option<String>,
    /// Camera-detected LABELLED things, in the **ego frame** (+X forward, +Y left) —
    /// the goal channel's input. Not a drivability claim; the corridor still comes
    /// from lidar (see `taj::CameraDetection` for the drivability channel).
    #[serde(default)]
    pub targets: Vec<TargetReq>,
    /// Producer stamp of the camera frame `targets` came from. Absent while
    /// `object_goal` is set → refused as STALE ("the detector did not look" is
    /// never "it isn't there").
    #[serde(default)]
    pub targets_stamp_ms: Option<u64>,
    /// Consumer clock for the freshness check. Absent → the frame's own stamp
    /// (age 0), matching this service's stateless convention where wall-clock
    /// staleness is the caller's job.
    #[serde(default)]
    pub now_ms: Option<u64>,
}

/// One labelled camera target on the wire — see [`PlanRequest::targets`].
#[derive(Deserialize)]
pub struct TargetReq {
    pub label: String,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_target_confidence")]
    pub confidence: f32,
}
fn default_target_confidence() -> f32 {
    1.0
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
    /// F13 (#1097): this `/plan` result is ADVISORY — a demo/inspection verdict from
    /// the slow-loop checker, NEVER actuation authority. Real authority is the
    /// in-line governor / the ADR-0033 release-token chokepoint. Always `true`; an
    /// explicit marker so a consumer cannot mistake this response for an
    /// actuation-authorized command.
    pub advisory: bool,
    /// F13 (#1097): whether the checker ADMITTED the proposal (`Accept`/`Clamp`).
    /// `false` on a refusal (`MRCFallback`/`Pending`), in which case `trajectory`
    /// is EMPTY — a refused proposal's geometry is never returned as if drivable.
    pub admitted: bool,
    /// The proposed trajectory — populated ONLY when `admitted` (F13 #1097).
    /// A refusal returns an empty array so a naive 2xx consumer cannot read a
    /// refused proposal's poses as an authoritative path.
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

fn parse_predicted_vru_intent(value: &str) -> Option<PredictedVruIntent> {
    match value {
        "unknown" => Some(PredictedVruIntent::Unknown),
        "waiting_near_path" => Some(PredictedVruIntent::WaitingNearPath),
        "along_path" => Some(PredictedVruIntent::AlongPath),
        "crossing_left_to_right" => Some(PredictedVruIntent::CrossingLeftToRight),
        "crossing_right_to_left" => Some(PredictedVruIntent::CrossingRightToLeft),
        "moving_away" => Some(PredictedVruIntent::MovingAway),
        _ => None,
    }
}

fn valid_vru_model(value: &str) -> bool {
    matches!(
        value,
        "stationary" | "constant_velocity" | "bounded_turn_rate" | "omnidirectional_fallback"
    )
}

fn valid_vru_fallback(value: &str) -> bool {
    matches!(
        value,
        "invalid_configuration"
            | "non_finite_track_state"
            | "insufficient_history"
            | "stale_track"
            | "excessive_speed"
            | "excessive_yaw_rate"
    )
}

fn validate_intent_probability_distribution(
    prediction: &PredictedVruReq,
) -> Result<(), SeamRejection> {
    if prediction.intent_probabilities.is_empty() {
        return Ok(());
    }

    const EXPECTED_INTENT_COUNT: usize = 6;
    const NORMALIZATION_EPSILON: f64 = 1.0e-6;

    if prediction.intent_probabilities.len() != EXPECTED_INTENT_COUNT {
        return Err(SeamRejection {
            code: "INVALID_VRU_INTENT_DISTRIBUTION",
            detail: format!(
                "prediction track {} carries {} intent hypotheses; expected {}",
                prediction.track_id,
                prediction.intent_probabilities.len(),
                EXPECTED_INTENT_COUNT
            ),
        });
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut probability_sum = 0.0;

    for hypothesis in &prediction.intent_probabilities {
        let intent =
            parse_predicted_vru_intent(&hypothesis.intent).ok_or_else(|| SeamRejection {
                code: "UNKNOWN_VRU_INTENT_PROBABILITY",
                detail: format!(
                    "prediction track {} carries unknown probability intent token {:?}",
                    prediction.track_id, hypothesis.intent
                ),
            })?;

        if !seen.insert(hypothesis.intent.as_str()) {
            return Err(SeamRejection {
                code: "DUPLICATE_VRU_INTENT_PROBABILITY",
                detail: format!(
                    "prediction track {} repeats intent token {:?}",
                    prediction.track_id, hypothesis.intent
                ),
            });
        }

        if !hypothesis.probability.is_finite() || !(0.0..=1.0).contains(&hypothesis.probability) {
            return Err(SeamRejection {
                code: "INVALID_VRU_INTENT_PROBABILITY",
                detail: format!(
                    "prediction track {} carries invalid probability for {:?}",
                    prediction.track_id, hypothesis.intent
                ),
            });
        }

        let _ = intent;
        probability_sum += hypothesis.probability;
    }

    if !probability_sum.is_finite() || (probability_sum - 1.0).abs() > NORMALIZATION_EPSILON {
        return Err(SeamRejection {
            code: "UNNORMALIZED_VRU_INTENT_DISTRIBUTION",
            detail: format!(
                "prediction track {} intent probabilities sum to {}",
                prediction.track_id, probability_sum
            ),
        });
    }

    Ok(())
}

/// Validate prediction bounds and authority relationships.
///
/// A prediction cannot exist without a matching fused pedestrian entry. This
/// preserves lidar-authoritative track creation and prevents a second semantic
/// channel from independently influencing motion.
fn validate_predicted_vrus(req: &PlanRequest) -> Result<(), SeamRejection> {
    if req.predicted_vrus.len() > MAX_PREDICTED_VRUS {
        return Err(SeamRejection {
            code: "PREDICTED_VRU_BOUND_EXCEEDED",
            detail: format!(
                "predicted_vrus contains {} tracks; maximum is {}",
                req.predicted_vrus.len(),
                MAX_PREDICTED_VRUS
            ),
        });
    }

    let pedestrian_ids: std::collections::BTreeSet<u64> = req
        .pedestrians
        .iter()
        .map(|pedestrian| pedestrian.id)
        .collect();

    let mut prediction_ids = std::collections::BTreeSet::new();

    for prediction in &req.predicted_vrus {
        validate_intent_probability_distribution(prediction)?;

        if parse_predicted_vru_intent(&prediction.intent).is_none() {
            return Err(SeamRejection {
                code: "UNKNOWN_VRU_INTENT",
                detail: format!(
                    "prediction track {} carries unknown intent token {:?}",
                    prediction.track_id, prediction.intent
                ),
            });
        }

        if !prediction.intent_confidence.is_finite()
            || !(0.0..=1.0).contains(&prediction.intent_confidence)
        {
            return Err(SeamRejection {
                code: "INVALID_VRU_INTENT_CONFIDENCE",
                detail: format!(
                    "prediction track {} carries invalid intent confidence",
                    prediction.track_id
                ),
            });
        }

        if prediction.intent_reason.is_empty() {
            return Err(SeamRejection {
                code: "INVALID_VRU_INTENT_REASON",
                detail: format!(
                    "prediction track {} carries an empty intent reason",
                    prediction.track_id
                ),
            });
        }

        if !pedestrian_ids.contains(&prediction.track_id) {
            return Err(SeamRejection {
                code: "PREDICTED_VRU_WITHOUT_PEDESTRIAN",
                detail: format!(
                    "prediction track {} has no fused pedestrian authority",
                    prediction.track_id
                ),
            });
        }

        if !prediction_ids.insert(prediction.track_id) {
            return Err(SeamRejection {
                code: "DUPLICATE_PREDICTED_VRU_TRACK",
                detail: format!(
                    "prediction track {} appears more than once",
                    prediction.track_id
                ),
            });
        }

        if !valid_vru_model(&prediction.model) {
            return Err(SeamRejection {
                code: "UNKNOWN_VRU_PREDICTION_MODEL",
                detail: format!(
                    "track {} supplied unknown model {:?}",
                    prediction.track_id, prediction.model
                ),
            });
        }

        if prediction
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| !valid_vru_fallback(reason))
        {
            return Err(SeamRejection {
                code: "UNKNOWN_VRU_FALLBACK_REASON",
                detail: format!(
                    "track {} supplied an unknown fallback reason",
                    prediction.track_id
                ),
            });
        }

        if prediction.points.is_empty() || prediction.points.len() > MAX_PREDICTED_VRU_POINTS {
            return Err(SeamRejection {
                code: "PREDICTED_VRU_POINT_BOUND",
                detail: format!(
                    "track {} contains {} prediction points; valid range is 1..={}",
                    prediction.track_id,
                    prediction.points.len(),
                    MAX_PREDICTED_VRU_POINTS
                ),
            });
        }

        if !prediction.horizon_s.is_finite()
            || prediction.horizon_s <= 0.0
            || !prediction.step_s.is_finite()
            || prediction.step_s <= 0.0
            || !prediction.source_age_s.is_finite()
            || prediction.source_age_s < 0.0
        {
            return Err(SeamRejection {
                code: "NONFINITE_VRU_PREDICTION",
                detail: format!(
                    "track {} contains invalid prediction metadata",
                    prediction.track_id
                ),
            });
        }

        let mut previous_time_s = -1.0_f64;

        for point in &prediction.points {
            if !point.time_s.is_finite()
                || point.time_s < 0.0
                || point.time_s <= previous_time_s
                || point.time_s > prediction.horizon_s + f64::EPSILON
                || !point.x.is_finite()
                || !point.y.is_finite()
                || !point.uncertainty_radius_m.is_finite()
                || point.uncertainty_radius_m < 0.0
            {
                return Err(SeamRejection {
                    code: "NONFINITE_VRU_PREDICTION",
                    detail: format!(
                        "track {} contains malformed or non-monotonic points",
                        prediction.track_id
                    ),
                });
            }

            previous_time_s = point.time_s;
        }
    }

    Ok(())
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
    let obj_ok = req.objects.iter().all(|o| finite(&[o.x, o.y, o.vx, o.vy]));
    let pedestrian_ok = req
        .pedestrians
        .iter()
        .all(|p| finite(&[p.x, p.y, p.vx, p.vy, p.age_s]) && p.age_s >= 0.0);
    // The optional vehicle overrides feed the checker's VehicleConfig and the
    // planner preset — a NaN footprint would mask comparisons downstream, so
    // they get the same gate (review: Copilot on #894).
    let veh_ok = req.vehicle.as_ref().is_none_or(|v| {
        [
            v.wheelbase_m,
            v.half_length_m,
            v.half_width_m,
            v.max_speed_mps,
            v.max_steering_deg,
            v.rss_lateral_alignment_tolerance_m,
            v.lateral_clearance_target_m,
        ]
        .iter()
        .flatten()
        .all(|x| x.is_finite())
    });
    if ego_ok && goal_ok && corr_ok && obj_ok && pedestrian_ok && veh_ok {
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
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
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
/// Resolve an `object_goal` request ("drive to the red cup") into a plain
/// `MickIntent::GoTo`.
///
/// 🔴 The resolved point is a DESTINATION ONLY — it makes no drivability claim.
/// The corridor still comes from lidar and the checker still bounds the
/// trajectory, so a target that is visible but unreachable yields a plan that
/// stops short rather than one that drives at it. Emitting an ordinary `GoTo`
/// (rather than a new intent variant) is what keeps that true: nothing
/// downstream can tell this goal came from a camera.
///
/// The resolver works in the EGO frame; `GoTo` is world-framed, so the goal is
/// transformed through the ego pose (`ObjectGoal::to_world`). Every refusal is
/// fail-closed → `SeamRejection` → NO MOTION, carrying the stable machine code
/// plus the operator sentence for narration.
fn object_goal_intent(req: &PlanRequest, label: &str) -> Result<MickIntent, SeamRejection> {
    if req.intent.is_some() {
        return Err(SeamRejection {
            code: "PLAN_AMBIGUOUS_GOAL_SOURCE",
            detail: "both `intent` and `object_goal` were supplied — refusing rather \
                     than silently preferring one; NO MOTION"
                .to_string(),
        });
    }
    let targets: Vec<LabeledTarget> = req
        .targets
        .iter()
        .map(|t| LabeledTarget {
            label: t.label.clone(),
            x_m: t.x,
            y_m: t.y,
            confidence: t.confidence,
        })
        .collect();
    // Stateless freshness: absent consumer clock → the frame's own stamp (age 0).
    let now = req.now_ms.or(req.targets_stamp_ms).unwrap_or(0);
    let goal = resolve_object_goal(
        label,
        &targets,
        req.targets_stamp_ms,
        now,
        DEFAULT_GOAL_MAX_AGE_MS,
        DEFAULT_MIN_CONFIDENCE,
        DEFAULT_TIE_EPSILON_M,
    )
    .map_err(|why| SeamRejection {
        code: why.code(),
        detail: kirra_taj::object_goal::refusal_sentence(label, why),
    })?;
    let (x_m, y_m) = goal.to_world(req.ego.x, req.ego.y, req.ego.heading);
    Ok(MickIntent::GoTo { x_m, y_m })
}

fn effective_intent(req: &PlanRequest) -> Result<MickIntent, SeamRejection> {
    if let Some(label) = req.object_goal.as_deref() {
        return object_goal_intent(req, label);
    }
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
    validate_predicted_vrus(req)?;
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

    let pedestrians: Vec<PerceivedPedestrian> = req
        .pedestrians
        .iter()
        .map(|pedestrian| PerceivedPedestrian {
            id: pedestrian.id,
            pos: Point {
                x_m: pedestrian.x,
                y_m: pedestrian.y,
            },
            vel: Point {
                x_m: pedestrian.vx,
                y_m: pedestrian.vy,
            },
            age_s: pedestrian.age_s,
        })
        .collect();

    let pedestrian_scene = PedestrianScene {
        pedestrians: &pedestrians,
        params: VruRssParams::default(),
        barriers: &[],
    };

    // Taj points are expressed in the same ego-relative planning frame as the
    // fused pedestrian channel. Occy consumes their conservative occupied
    // regions and derives only a LOWER speed ceiling.
    let predicted_vru_occupancies: Vec<PredictedVruOccupancy> = req
        .predicted_vrus
        .iter()
        .flat_map(|prediction| {
            let probability_distribution: Vec<PredictedVruIntentProbability> = prediction
                .intent_probabilities
                .iter()
                .map(|hypothesis| PredictedVruIntentProbability {
                    intent: parse_predicted_vru_intent(&hypothesis.intent)
                        .expect("validated intent probability token above"),
                    probability: hypothesis.probability,
                })
                .collect();

            prediction.points.iter().map(move |point| {
                let uncertainty_radius_m = probabilistic_vru_uncertainty_radius(
                    point.uncertainty_radius_m,
                    &probability_distribution,
                    OCCY_PROBABILISTIC_CROSSING_EXTENSION_M,
                    OCCY_PROBABILISTIC_WAITING_EXTENSION_M,
                    OCCY_PROBABILISTIC_UNKNOWN_EXTENSION_M,
                );

                PredictedVruOccupancy {
                    track_id: prediction.track_id,
                    time_s: point.time_s,
                    ahead_m: point.x,
                    lateral_offset_m: point.y,
                    uncertainty_radius_m,
                }
            })
        })
        .collect();

    let baseline_predicted_vru_cap_mps = predicted_vru_speed_cap(
        &predicted_vru_occupancies,
        DEFAULT_VRU_YIELD_BAND_HALF_WIDTH_M,
        DEFAULT_VRU_YIELD_STANDOFF_M,
        OCCY_VRU_BRAKE_DECEL_MPS2,
    );

    let intent_aware_occupancies: Vec<IntentAwarePredictedVruOccupancy> = req
        .predicted_vrus
        .iter()
        .flat_map(|prediction| {
            let intent = parse_predicted_vru_intent(&prediction.intent)
                .expect("validated predicted VRU intent above");

            let probability_distribution: Vec<PredictedVruIntentProbability> = prediction
                .intent_probabilities
                .iter()
                .map(|hypothesis| PredictedVruIntentProbability {
                    intent: parse_predicted_vru_intent(&hypothesis.intent)
                        .expect("validated intent probability token above"),
                    probability: hypothesis.probability,
                })
                .collect();

            prediction.points.iter().map(move |point| {
                let uncertainty_radius_m = probabilistic_vru_uncertainty_radius(
                    point.uncertainty_radius_m,
                    &probability_distribution,
                    OCCY_PROBABILISTIC_CROSSING_EXTENSION_M,
                    OCCY_PROBABILISTIC_WAITING_EXTENSION_M,
                    OCCY_PROBABILISTIC_UNKNOWN_EXTENSION_M,
                );

                IntentAwarePredictedVruOccupancy {
                    track_id: prediction.track_id,
                    intent,
                    intent_confidence: prediction.intent_confidence,
                    time_s: point.time_s,
                    ahead_m: point.x,
                    lateral_offset_m: point.y,
                    uncertainty_radius_m,
                }
            })
        })
        .collect();

    let predicted_vru_cap_mps = intent_aware_predicted_vru_speed_cap(
        &intent_aware_occupancies,
        baseline_predicted_vru_cap_mps,
        DEFAULT_VRU_YIELD_BAND_HALF_WIDTH_M,
        DEFAULT_CROSSING_INTENT_BAND_EXTENSION_M,
        DEFAULT_VRU_YIELD_STANDOFF_M,
        OCCY_VRU_BRAKE_DECEL_MPS2,
        DEFAULT_MINIMUM_VRU_INTENT_CONFIDENCE,
    );

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
        target_speed_mps: predicted_vru_cap_mps,
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
        Some(&pedestrian_scene),
        FrameTrust::Trusted,
    );

    // F13 (#1097): the checker's admit decision. Accept / Clamp are drivable
    // (admitted); MRCFallback / Pending are refusals. Only an admitted proposal
    // carries its trajectory on the wire (below).
    let admitted = matches!(
        verdict,
        TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
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
        // F13 (#1097): an advisory result, never actuation authority.
        advisory: true,
        admitted,
        // F13 (#1097): the proposed trajectory rides ONLY on an admitted verdict.
        // A refusal returns an EMPTY trajectory so a consumer that ignores the
        // verdict field cannot read a refused proposal's poses as a drivable path.
        trajectory: if admitted {
            plan.trajectory
                .iter()
                .map(|p| TrajPt {
                    x: p.pose.x_m,
                    y: p.pose.y_m,
                    heading: p.pose.heading_rad,
                    v: p.velocity_mps,
                    t: p.time_from_start_s,
                })
                .collect()
        } else {
            Vec::new()
        },
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
            pedestrians: vec![],
            predicted_vrus: vec![],
            vehicle: None,
            intent: None,
            // Object-goal channel off by default, so every pre-existing
            // assertion still describes the plain-goal path.
            object_goal: None,
            targets: vec![],
            targets_stamp_ms: None,
            now_ms: None,
        }
    }

    #[test]
    fn goal_only_request_still_grounds_as_goto_and_reports_a_verdict() {
        let resp = handle_plan(&base_request()).expect("seam admits");
        assert!(
            resp.advisory,
            "the /plan result is always advisory (F13 #1097)"
        );
        // A clear goal down an open corridor is admitted, so the trajectory rides.
        assert!(
            resp.verdict == "Accept" || resp.verdict == "Clamp",
            "clear goal is admitted, got {}",
            resp.verdict
        );
        assert!(resp.admitted, "an Accept/Clamp verdict is admitted");
        assert!(
            !resp.trajectory.is_empty(),
            "an admitted proposal carries its trajectory"
        );
    }

    #[test]
    fn typed_intent_object_grounds_through_the_one_parse() {
        let mut req = base_request();
        req.intent = Some(serde_json::json!({"intent":"go_to","x_m":40.0,"y_m":0.0}));
        let resp = handle_plan(&req).expect("valid intent admits");
        assert!(!resp.trajectory.is_empty());
        // The string form (the /intent/last relay shape) parses identically.
        let mut req = base_request();
        req.intent = Some(serde_json::json!(
            r#"{"intent":"go_to","x_m":40.0,"y_m":0.0}"#
        ));
        handle_plan(&req).expect("string-embedded intent admits");
    }

    // ---- object goal: "drive to the red cup" -------------------------------

    fn target(label: &str, x: f64, y: f64) -> TargetReq {
        TargetReq {
            label: label.into(),
            x,
            y,
            confidence: 0.9,
        }
    }

    /// The end-to-end shape of "drive to the red cup": a named camera target
    /// becomes an ordinary `GoTo` and is planned + checked like any other goal.
    #[test]
    fn object_goal_grounds_a_named_target_as_a_plain_goto() {
        let mut req = base_request();
        req.object_goal = Some("red cup".into());
        req.targets = vec![target("red cup", 30.0, 0.0)];
        req.targets_stamp_ms = Some(1_000);
        req.now_ms = Some(1_000);
        let resp = handle_plan(&req).expect("a seen, reachable cup plans");
        assert!(
            !resp.trajectory.is_empty(),
            "an admitted plan carries its trajectory"
        );
    }

    /// Every resolver refusal is fail-closed at the seam: NO MOTION, carrying the
    /// stable `OBJECT_GOAL_*` code — never a silent fallback to the request goal.
    #[test]
    fn object_goal_refusals_fail_closed_to_no_motion() {
        // Not seen.
        let mut req = base_request();
        req.object_goal = Some("red cup".into());
        req.targets = vec![target("chair", 10.0, 0.0)];
        req.targets_stamp_ms = Some(1_000);
        let e = handle_plan(&req).expect_err("an unseen target must refuse");
        assert_eq!(e.code, "OBJECT_GOAL_NOT_SEEN");

        // Armed but no camera frame → stale, never "it isn't there".
        let mut req = base_request();
        req.object_goal = Some("red cup".into());
        req.targets = vec![target("red cup", 10.0, 0.0)];
        req.targets_stamp_ms = None;
        assert_eq!(
            handle_plan(&req).expect_err("no frame must refuse").code,
            "OBJECT_GOAL_STALE"
        );

        // A red request must never drive to a blue cup.
        let mut req = base_request();
        req.object_goal = Some("red cup".into());
        req.targets = vec![target("blue cup", 10.0, 0.0)];
        req.targets_stamp_ms = Some(1_000);
        assert_eq!(
            handle_plan(&req)
                .expect_err("wrong colour must refuse")
                .code,
            "OBJECT_GOAL_NOT_SEEN"
        );
    }

    /// Two goal sources is an ambiguity we refuse rather than resolve by a silent
    /// precedence rule.
    #[test]
    fn object_goal_plus_typed_intent_is_a_refused_ambiguity() {
        let mut req = base_request();
        req.object_goal = Some("red cup".into());
        req.targets = vec![target("red cup", 30.0, 0.0)];
        req.targets_stamp_ms = Some(1_000);
        req.intent = Some(serde_json::json!({"intent":"go_to","x_m":40.0,"y_m":0.0}));
        assert_eq!(
            handle_plan(&req)
                .expect_err("two goal sources must refuse")
                .code,
            "PLAN_AMBIGUOUS_GOAL_SOURCE"
        );
    }

    /// Absent `object_goal` leaves the endpoint byte-identical to its prior form.
    #[test]
    fn no_object_goal_is_unchanged_behaviour() {
        let req = base_request();
        assert!(req.object_goal.is_none() && req.targets.is_empty());
        handle_plan(&req).expect("the plain-goal path still plans");
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
    fn pedestrian_channel_binds_the_real_checker() {
        let mut req = base_request();

        // Keep the proposal short and slow enough that the ordinary vehicle
        // checks admit it, then place a pedestrian directly in its path.
        req.goal = Xy { x: 6.0, y: 0.0 };
        req.cruise = 1.0;
        req.pedestrians = vec![PedestrianReq {
            id: 44,
            x: 3.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            age_s: 0.0,
        }];

        let response = handle_plan(&req).expect("valid VRU request reaches checker");

        assert_eq!(
            response.verdict, "MRCFallback",
            "a pedestrian in the proposed path must bind the VRU RSS checker"
        );
        assert!(!response.admitted);
        assert!(response.trajectory.is_empty());
    }

    fn predicted_point(
        time_s: f64,
        x: f64,
        y: f64,
        uncertainty_radius_m: f64,
    ) -> PredictedVruPointReq {
        PredictedVruPointReq {
            time_s,
            x,
            y,
            uncertainty_radius_m,
        }
    }

    fn predicted_vru(track_id: u64, points: Vec<PredictedVruPointReq>) -> PredictedVruReq {
        PredictedVruReq {
            track_id,
            model: "constant_velocity".to_string(),
            intent: "unknown".to_string(),
            intent_confidence: 0.0,
            intent_reason: "ambiguous_motion".to_string(),
            intent_probabilities: vec![],
            points,
            horizon_s: 3.0,
            step_s: 1.0,
            source_age_s: 0.0,
            frames_seen: 3,
            fallback_reason: None,
        }
    }

    #[test]
    fn predicted_crossing_vru_derates_occy_before_snapshot_conflict() {
        let mut baseline = base_request();
        baseline.cruise = 3.0;

        let baseline_response = handle_plan(&baseline).expect("baseline planning request");

        let baseline_peak = baseline_response
            .trajectory
            .iter()
            .map(|point| point.v)
            .fold(0.0_f64, f64::max);

        let mut predicted = base_request();
        predicted.cruise = 3.0;
        predicted.pedestrians = vec![PedestrianReq {
            id: 44,
            x: 5.0,
            y: 2.0,
            vx: 0.0,
            vy: -0.8,
            age_s: 0.0,
        }];
        predicted.predicted_vrus = vec![predicted_vru(
            44,
            vec![
                predicted_point(1.0, 5.0, 1.2, 0.25),
                predicted_point(2.0, 5.0, 0.2, 0.35),
                predicted_point(3.0, 5.0, -0.4, 0.45),
            ],
        )];

        let response = handle_plan(&predicted).expect("valid prediction reaches Occy");

        let predicted_peak = response
            .trajectory
            .iter()
            .map(|point| point.v)
            .fold(0.0_f64, f64::max);

        assert!(
            predicted_peak < baseline_peak,
            "future crossing must tighten Occy's speed: {predicted_peak} < {baseline_peak}"
        );
    }

    fn intent_probability(intent: &str, probability: f64) -> PredictedVruIntentProbabilityReq {
        PredictedVruIntentProbabilityReq {
            intent: intent.to_string(),
            probability,
        }
    }

    fn complete_intent_distribution(
        unknown: f64,
        waiting: f64,
        along_path: f64,
        crossing_left_to_right: f64,
        crossing_right_to_left: f64,
        moving_away: f64,
    ) -> Vec<PredictedVruIntentProbabilityReq> {
        vec![
            intent_probability("unknown", unknown),
            intent_probability("waiting_near_path", waiting),
            intent_probability("along_path", along_path),
            intent_probability("crossing_left_to_right", crossing_left_to_right),
            intent_probability("crossing_right_to_left", crossing_right_to_left),
            intent_probability("moving_away", moving_away),
        ]
    }

    fn peak_trajectory_speed(response: &PlanResponse) -> f64 {
        response
            .trajectory
            .iter()
            .map(|point| point.v)
            .fold(0.0_f64, f64::max)
    }

    #[test]
    fn higher_crossing_probability_only_tightens_the_checked_plan() {
        let mut low_probability_request = base_request();
        low_probability_request.cruise = 3.0;
        low_probability_request.pedestrians = vec![PedestrianReq {
            id: 77,
            x: 5.0,
            y: 1.5,
            vx: 0.0,
            vy: -0.5,
            age_s: 0.0,
        }];

        let mut low_prediction = predicted_vru(
            77,
            vec![
                predicted_point(1.0, 5.0, 1.45, 0.10),
                predicted_point(2.0, 5.0, 1.30, 0.10),
            ],
        );
        low_prediction.intent = "crossing_left_to_right".to_string();
        low_prediction.intent_confidence = 0.80;
        low_prediction.intent_reason = "lateral_motion_toward_path".to_string();
        low_prediction.intent_probabilities =
            complete_intent_distribution(0.05, 0.05, 0.75, 0.05, 0.05, 0.05);

        low_probability_request.predicted_vrus = vec![low_prediction];

        let mut high_probability_request = base_request();
        high_probability_request.cruise = 3.0;
        high_probability_request.pedestrians = vec![PedestrianReq {
            id: 77,
            x: 5.0,
            y: 1.5,
            vx: 0.0,
            vy: -0.5,
            age_s: 0.0,
        }];

        let mut high_prediction = predicted_vru(
            77,
            vec![
                predicted_point(1.0, 5.0, 1.45, 0.10),
                predicted_point(2.0, 5.0, 1.30, 0.10),
            ],
        );
        high_prediction.intent = "crossing_left_to_right".to_string();
        high_prediction.intent_confidence = 0.80;
        high_prediction.intent_reason = "lateral_motion_toward_path".to_string();
        high_prediction.intent_probabilities =
            complete_intent_distribution(0.05, 0.05, 0.05, 0.75, 0.05, 0.05);

        high_probability_request.predicted_vrus = vec![high_prediction];

        let low_response = handle_plan(&low_probability_request)
            .expect("low crossing probability reaches Occy and Kirra");
        let high_response = handle_plan(&high_probability_request)
            .expect("high crossing probability reaches Occy and Kirra");

        let low_peak = peak_trajectory_speed(&low_response);
        let high_peak = peak_trajectory_speed(&high_response);

        assert!(
            high_peak <= low_peak,
            "greater crossing probability must never increase planned speed: \
             high={high_peak}, low={low_peak}"
        );

        assert!(
            matches!(
                low_response.verdict.as_str(),
                "Accept" | "Clamp" | "MRCFallback"
            ),
            "low-probability trajectory must pass through Kirra, got {}",
            low_response.verdict
        );

        assert!(
            matches!(
                high_response.verdict.as_str(),
                "Accept" | "Clamp" | "MRCFallback"
            ),
            "high-probability trajectory must pass through Kirra, got {}",
            high_response.verdict
        );

        assert!(
            high_response.advisory,
            "planner-side Kirra verdict remains advisory"
        );
    }

    #[test]
    fn prediction_without_fused_pedestrian_is_refused() {
        let mut request = base_request();
        request.predicted_vrus = vec![predicted_vru(99, vec![predicted_point(1.0, 4.0, 0.0, 0.2)])];

        assert_eq!(
            handle_plan(&request).unwrap_err().code,
            "PREDICTED_VRU_WITHOUT_PEDESTRIAN"
        );
    }

    #[test]
    fn malformed_prediction_fails_closed_at_the_seam() {
        let mut request = base_request();
        request.pedestrians = vec![PedestrianReq {
            id: 55,
            x: 5.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            age_s: 0.0,
        }];
        request.predicted_vrus = vec![predicted_vru(
            55,
            vec![predicted_point(1.0, 4.0, 0.0, f64::NAN)],
        )];

        assert_eq!(
            handle_plan(&request).unwrap_err().code,
            "NONFINITE_VRU_PREDICTION"
        );
    }

    #[test]
    fn absent_prediction_channel_preserves_existing_plan_path() {
        let mut request = base_request();
        request.predicted_vrus.clear();

        let response = handle_plan(&request).expect("empty prediction channel is a no-op");

        assert!(
            matches!(response.verdict.as_str(), "Accept" | "Clamp"),
            "empty prediction channel must preserve the clean planning path"
        );
    }

    #[test]
    fn absent_pedestrian_channel_preserves_existing_plan_path() {
        let mut req = base_request();
        req.pedestrians.clear();

        let response = handle_plan(&req).expect("empty VRU channel is a no-op");

        assert!(
            matches!(response.verdict.as_str(), "Accept" | "Clamp"),
            "empty VRU input must preserve the existing clean-plan path, got {}",
            response.verdict
        );
    }

    #[test]
    fn malformed_pedestrian_input_is_refused_at_the_seam() {
        let mut req = base_request();
        req.pedestrians = vec![PedestrianReq {
            id: 1,
            x: f64::NAN,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            age_s: 0.0,
        }];

        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");

        let mut req = base_request();
        req.pedestrians = vec![PedestrianReq {
            id: 1,
            x: 2.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            age_s: -0.1,
        }];

        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");
    }

    #[test]
    fn nonfinite_world_input_is_refused_at_the_seam() {
        let mut req = base_request();
        req.ego.speed = f64::NAN;
        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");
        // The optional vehicle overrides get the same gate — a NaN footprint
        // must not reach the checker's VehicleConfig.
        let mut req = base_request();
        req.vehicle = Some(VehicleReq {
            class: None,
            wheelbase_m: Some(f64::NAN),
            half_length_m: None,
            half_width_m: None,
            max_speed_mps: None,
            max_steering_deg: None,
            rss_lateral_alignment_tolerance_m: None,
            lateral_clearance_target_m: None,
        });
        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");
        let mut req = base_request();
        req.vehicle = Some(VehicleReq {
            class: None,
            wheelbase_m: None,
            half_length_m: None,
            half_width_m: None,
            max_speed_mps: Some(f64::INFINITY),
            max_steering_deg: None,
            rss_lateral_alignment_tolerance_m: None,
            lateral_clearance_target_m: None,
        });
        assert_eq!(handle_plan(&req).unwrap_err().code, "NONFINITE_INPUT");
    }

    #[test]
    fn out_of_map_goal_is_refused_at_the_seam() {
        // Direct goal.
        let mut req = base_request();
        req.goal = Xy { x: 9e6, y: 0.0 };
        assert_eq!(
            handle_plan(&req).unwrap_err().code,
            "INTENT_GOAL_OUT_OF_MAP"
        );
        // And an intent-carried target gets the SAME bound.
        let mut req = base_request();
        req.intent = Some(serde_json::json!({"intent":"go_to","x_m":9e6,"y_m":0.0}));
        assert_eq!(
            handle_plan(&req).unwrap_err().code,
            "INTENT_GOAL_OUT_OF_MAP"
        );
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
        // F13 (#1097): a refusal is advisory, NOT admitted, and carries NO
        // trajectory — a consumer cannot mistake the refused proposal's geometry
        // for an actuation-authorized path.
        assert!(resp.advisory, "the result is advisory");
        assert!(!resp.admitted, "a refused proposal is not admitted");
        assert!(
            resp.trajectory.is_empty(),
            "a refused proposal must carry no trajectory (F13 #1097)"
        );
        let code = resp.reason_code.expect("a refusal must carry its code");
        let sentence = resp.reason.expect("a refusal must carry its sentence");
        assert!(code.starts_with("TRAJECTORY_"), "stable vocabulary: {code}");
        assert!(
            sentence.len() > 40,
            "the sentence must be specific, not a generic marker: {sentence}"
        );
    }
}
