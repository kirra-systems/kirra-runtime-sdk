//! The Taj perception endpoint core (promoted verbatim from the
//! `kirra-mick --example taj_service`): POST a `LaserScan`, get back the
//! geometric corridor's health plus an **assured-clear-distance (ACD) speed
//! cap** — the speed from which the robot can still stop within the clear
//! distance ahead (RSS Rule 4 / the ADR-0014 "lidar safety buffer").
//!
//! A perception PRODUCER, not the safety authority — Taj tightens the
//! envelope, the KIRRA governor still bounds the result. Fail-closed:
//! perception below the confidence floor → `healthy:false` and
//! `speed_cap_mps: 0.0` (the MRC floor — the consumer holds).

use kirra_core::corridor::CorridorSource;
use kirra_trajectory::cap_stabilizer::{CapObservation, CapStabilizer, CapStabilizerConfig};
use kirra_trajectory::sensor_watchdog::{
    fingerprint_from_ranges, FrameObservation, SensorHealth, SensorTick, SensorWatchdog,
    SENSOR_WATCHDOG_ENABLED_ENV,
};
// Re-exported so the service binary configures the watchdog through this module
// rather than depending on kirra-trajectory directly.
use kirra_taj::{
    clip_corridor_to_hazards, hazard_clip_x, CameraVruFusionConfig, CameraVruObservation,
    LaserScan, PredictedVruMotion, SemanticClass, SemanticDetection, TajConfig, TajTracker,
    VruClassifierConfig, VruIntentClass, VruIntentReason, VruMotionModel,
    VruMotionPredictionConfig, VruPredictionFallbackReason,
};
pub use kirra_trajectory::sensor_watchdog::SensorWatchdogConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;

/// Default freshness budget for the camera (Phase-B) channel, ms.
pub const DEFAULT_CAMERA_MAX_AGE_MS: u64 = 500;

/// One camera-derived semantic region, in the ego frame (+X forward, +Y left) —
/// the wire form of [`SemanticDetection`].
///
/// 🔴 **This is a SAFETY classification, not a goal label.** `class` answers only
/// "may the ego drive here?", and the fusion is **TIGHTEN-ONLY**: a detection can
/// shorten the drivable corridor, never extend it (the KPI gate's `ForbiddenLoosen`
/// is a hard zero). Lidar (Phase A) remains the **sole authority on free space** —
/// the camera cannot make unseen ground drivable. An unrecognized class token
/// decodes to [`SemanticClass::Unknown`], which is non-drivable (fail-closed:
/// never assume drivable).
///
/// Naming a *destination* ("the red cup") is a different concern entirely — see
/// `kirra_taj::object_goal`, which produces a GOAL POINT and makes no drivability
/// claim whatsoever.
#[derive(Deserialize)]
pub struct CameraDetection {
    /// `road` | `water` | `static_obstacle` | anything else → `unknown`.
    pub class: String,
    /// Nearest forward distance to the region (m, ego frame).
    pub near_x_m: f64,
    /// Lateral extent `[min, max]` (m; +Y left).
    pub lateral_min_m: f64,
    pub lateral_max_m: f64,
}

impl CameraDetection {
    /// Decode to the safety type. Unknown/garbage tokens fail closed to
    /// [`SemanticClass::Unknown`] (non-drivable) rather than being dropped — a
    /// detection we cannot classify is a hazard, never free space.
    fn to_semantic(&self) -> SemanticDetection {
        let class = match self.class.trim().to_ascii_lowercase().as_str() {
            "road" => SemanticClass::Road,
            "water" => SemanticClass::Water,
            "static_obstacle" | "obstacle" => SemanticClass::StaticObstacle,
            _ => SemanticClass::Unknown,
        };
        SemanticDetection {
            class,
            near_x_m: self.near_x_m,
            lateral_min_m: self.lateral_min_m,
            lateral_max_m: self.lateral_max_m,
        }
    }

    /// A detection with any non-finite geometry cannot be reasoned about; the
    /// caller treats its presence as a perception fault (fail-closed), never
    /// silently ignores it.
    fn is_finite(&self) -> bool {
        self.near_x_m.is_finite()
            && self.lateral_min_m.is_finite()
            && self.lateral_max_m.is_finite()
    }
}

/// Supporting camera/depth position observation.
///
/// This is geometry evidence only. It does not assert pedestrian identity,
/// create a track, or loosen any lidar-derived bound.
#[derive(Debug, Clone, Deserialize)]
pub struct CameraObservationReq {
    pub observation_id: u64,
    pub x: f64,
    pub y: f64,
    pub confidence: f64,
}

/// Camera (Phase-B) channel verdict for one request — the same three-way,
/// fail-closed decision the occlusion / VRU channels make: DISARMED → no-op,
/// armed+fresh → live fusion, armed+silent-or-garbage → MRC floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraChannel {
    /// Not configured → Phase-B never runs (byte-identical to lidar-only).
    Disarmed,
    /// Armed and the frame is fresh → fuse the detections (tighten-only).
    Fresh,
    /// Armed but no frame / stale / non-finite geometry → fail closed.
    Faulted,
}

#[derive(Deserialize)]
pub struct PerceptionRequest {
    pub angle_min_rad: f64,
    pub angle_increment_rad: f64,
    pub range_min_m: f64,
    pub range_max_m: f64,
    pub ranges: Vec<f32>,
    #[serde(default)]
    pub stamp_ms: u64,
    #[serde(default = "default_extent")]
    pub forward_extent_m: f64,
    #[serde(default = "default_decel")]
    pub decel_mps2: f64,
    #[serde(default = "default_margin")]
    pub margin_m: f64,
    #[serde(default = "default_lane_half")]
    pub lane_half_m: f64,
    /// Physical vehicle width used to validate whether the perceived corridor
    /// can safely contain the platform.
    #[serde(default = "default_vehicle_width")]
    pub vehicle_width_m: f64,
    /// Required free-space clearance on each side of the vehicle.
    #[serde(default = "default_lateral_clearance")]
    pub lateral_clearance_m: f64,
    #[serde(default = "default_floor")]
    pub confidence_floor: f32,
    /// Phase-B camera channel (OPT-IN). `false` (default) → the camera fusion
    /// never runs and this endpoint is byte-identical to its lidar-only form.
    #[serde(default)]
    pub camera_armed: bool,
    /// Camera-derived semantic regions for THIS frame (tighten-only; see
    /// [`CameraDetection`]). Empty with a fresh `camera_stamp_ms` legitimately
    /// means "the camera looked and saw no hazard".
    #[serde(default)]
    pub detections: Vec<CameraDetection>,
    /// Supporting depth-camera geometry for association with existing lidar
    /// tracks. This channel never creates tracks or pedestrian membership.
    #[serde(default)]
    pub camera_observations: Vec<CameraObservationReq>,
    /// Producer stamp of the camera frame the detections came from. `None` while
    /// `camera_armed` is a SILENT channel → fail closed ("the detector did not
    /// look" is never "clear").
    #[serde(default)]
    pub camera_stamp_ms: Option<u64>,
    /// Freshness budget for `camera_stamp_ms` against `stamp_ms`.
    #[serde(default = "default_camera_age")]
    pub camera_max_age_ms: u64,
    /// Publishers currently advertising the scan topic, as observed by the ROS
    /// subscriber that produced this request (#1211).
    ///
    /// This value can only be obtained by inspecting the ROS graph, which this
    /// HTTP service cannot do — so it is carried as DATA from the layer that can.
    /// `None` means "not observed", not "zero": a non-ROS caller (a test harness,
    /// `inspect_corridor.py`) has no graph to look at, and faulting it for a fact
    /// it cannot know would make the publisher check fire on the wrong condition.
    /// The response reports which case applied via
    /// [`PerceptionResponse::publisher_count_observed`], so an omission is visible
    /// rather than silent.
    #[serde(default)]
    pub publisher_count: Option<usize>,
    /// This request is a DIAGNOSTIC PROBE, not a frame from the live scan stream
    /// (`kirra_doctor`'s profile-digest probe, `verify_deployment`). Excluded from
    /// liveness accounting.
    ///
    /// Necessary because those tools POST a canned scan — identical bytes, a fixed
    /// `stamp_ms` — on every run. To the watchdog that is indistinguishable from a
    /// driver republishing one buffer forever, so an unflagged probe run would trip
    /// `SENSOR_FROZEN_FRAME` and hold the real robot in recovery afterwards. A
    /// diagnostic must not be able to cause the fault it is diagnosing.
    ///
    /// This never RELAXES a bound: a probe is not observed, so it can neither set
    /// nor clear a fault, and any standing verdict from the live stream is
    /// untouched. The probe's own response reports `sensor_liveness: "disarmed"`,
    /// because liveness genuinely was not assessed for it.
    #[serde(default)]
    pub diagnostic_probe: bool,
}
fn default_camera_age() -> u64 {
    DEFAULT_CAMERA_MAX_AGE_MS
}

/// Pure three-way camera-channel decision (host-tested). Mirrors
/// `resolve_occlusion_channel` / `resolve_vru_channel`: an ARMED-but-silent or
/// stale or geometrically-invalid channel is a perception GAP, never a verdict.
/// A camera stamp implausibly far in the FUTURE is also stale (the same
/// non-monotonic-clock hole the scene-veto gates close).
#[must_use]
pub fn resolve_camera_channel(
    armed: bool,
    camera_stamp_ms: Option<u64>,
    now_ms: u64,
    max_age_ms: u64,
    detections: &[CameraDetection],
) -> CameraChannel {
    if !armed {
        return CameraChannel::Disarmed;
    }
    let Some(stamp) = camera_stamp_ms else {
        return CameraChannel::Faulted; // armed but silent
    };
    if stamp > now_ms.saturating_add(max_age_ms) {
        return CameraChannel::Faulted; // implausible future stamp
    }
    if now_ms.saturating_sub(stamp) > max_age_ms {
        return CameraChannel::Faulted; // stale
    }
    if detections.iter().any(|d| !d.is_finite()) {
        return CameraChannel::Faulted; // undecodable geometry
    }
    CameraChannel::Fresh
}
fn default_extent() -> f64 {
    20.0
}
fn default_decel() -> f64 {
    1.5
}
fn default_margin() -> f64 {
    0.4
}
fn default_lane_half() -> f64 {
    0.6
}
fn default_vehicle_width() -> f64 {
    0.203
}
fn default_lateral_clearance() -> f64 {
    0.15
}
fn default_floor() -> f32 {
    0.5
}

/// Maximum number of lidar rays accepted by the safety-facing perception
/// endpoint.
///
/// This bounds request allocation and Phase-A processing work. The R2 TG30
/// currently emits approximately 2,020 samples per scan, so 4,096 leaves
/// platform headroom without permitting an unbounded request body to drive
/// checker-adjacent WCET.
const MAX_LIDAR_RAYS: usize = 4_096;

/// Stable validation failures for malformed or contradictory perception input.
///
/// The HTTP endpoint currently returns a normal fail-closed response rather
/// than exposing this code on the wire. Keeping the reason typed here makes the
/// gate testable and ready for structured diagnostics without changing the
/// existing response schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerceptionRequestError {
    InvalidAngleMinimum,
    InvalidAngleIncrement,
    InvalidRangeBounds,
    EmptyScan,
    ScanTooLarge,
    NanRangeSample,
    InvalidForwardExtent,
    /// Finite, but beyond [`MAX_FORWARD_EXTENT_M`]. Distinct from
    /// `InvalidForwardExtent` so a unit mistake (metres given as millimetres,
    /// say) is diagnosable as "too far" rather than "malformed".
    ForwardExtentTooLarge,
    InvalidDeceleration,
    InvalidMargin,
    InvalidLaneHalfWidth,
    InvalidVehicleWidth,
    InvalidLateralClearance,
    LaneNarrowerThanRequiredCorridor,
    InvalidConfidenceFloor,
}

/// Validate all request-level assumptions before allocation, geometry
/// processing or ACD arithmetic.
///
/// Positive infinity in `ranges` is intentionally accepted: ROS LaserScan uses
/// it as a normal "no return" value. NaN is rejected because it represents an
/// undecodable measurement rather than an absent return.
fn validate_perception_request(req: &PerceptionRequest) -> Result<(), PerceptionRequestError> {
    if !req.angle_min_rad.is_finite() {
        return Err(PerceptionRequestError::InvalidAngleMinimum);
    }

    if !req.angle_increment_rad.is_finite() || req.angle_increment_rad == 0.0 {
        return Err(PerceptionRequestError::InvalidAngleIncrement);
    }

    if !req.range_min_m.is_finite()
        || !req.range_max_m.is_finite()
        || req.range_min_m < 0.0
        || req.range_max_m <= req.range_min_m
    {
        return Err(PerceptionRequestError::InvalidRangeBounds);
    }

    if req.ranges.is_empty() {
        return Err(PerceptionRequestError::EmptyScan);
    }

    if req.ranges.len() > MAX_LIDAR_RAYS {
        return Err(PerceptionRequestError::ScanTooLarge);
    }

    if req.ranges.iter().any(|range| range.is_nan()) {
        return Err(PerceptionRequestError::NanRangeSample);
    }

    if !req.forward_extent_m.is_finite() || req.forward_extent_m <= MIN_FORWARD_OBJECT_X_M {
        return Err(PerceptionRequestError::InvalidForwardExtent);
    }

    if req.forward_extent_m > MAX_FORWARD_EXTENT_M {
        return Err(PerceptionRequestError::ForwardExtentTooLarge);
    }

    if !req.decel_mps2.is_finite() || req.decel_mps2 <= 0.0 {
        return Err(PerceptionRequestError::InvalidDeceleration);
    }

    if !req.margin_m.is_finite() || req.margin_m < 0.0 || req.margin_m >= req.forward_extent_m {
        return Err(PerceptionRequestError::InvalidMargin);
    }

    if !req.lane_half_m.is_finite() || req.lane_half_m <= 0.0 {
        return Err(PerceptionRequestError::InvalidLaneHalfWidth);
    }

    if !req.vehicle_width_m.is_finite() || req.vehicle_width_m <= 0.0 {
        return Err(PerceptionRequestError::InvalidVehicleWidth);
    }

    if !req.lateral_clearance_m.is_finite() || req.lateral_clearance_m < 0.0 {
        return Err(PerceptionRequestError::InvalidLateralClearance);
    }

    let required_corridor_width_m = req.vehicle_width_m + 2.0 * req.lateral_clearance_m;

    if !required_corridor_width_m.is_finite() || 2.0 * req.lane_half_m < required_corridor_width_m {
        return Err(PerceptionRequestError::LaneNarrowerThanRequiredCorridor);
    }

    if !req.confidence_floor.is_finite() || !(0.0..=1.0).contains(&req.confidence_floor) {
        return Err(PerceptionRequestError::InvalidConfidenceFloor);
    }

    Ok(())
}

/// Normal-schema fail-closed result for an invalid perception request.
///
/// Empty geometry prevents downstream consumers from accidentally planning
/// against partially processed evidence, while `healthy=false` and
/// `speed_cap_mps=0.0` enforce stop-and-hold behavior.
fn invalid_perception_response(camera_armed: bool, health: SensorHealth) -> PerceptionResponse {
    let mut response = PerceptionResponse {
        frame_id: PerceptionFrameId::invalid(),
        healthy: false,
        confidence: 0.0,
        age_ms: 0,
        clear_distance_m: 0.0,
        nearest_object_m: None,
        object_count: 0,
        minimum_corridor_width_m: None,
        required_corridor_width_m: 0.0,
        speed_cap_mps: 0.0,
        left: Vec::new(),
        right: Vec::new(),
        objects: Vec::new(),
        camera_associations: Vec::new(),
        pedestrians: Vec::new(),
        predicted_vrus: Vec::new(),
        camera_healthy: !camera_armed,
        camera_clip_x_m: None,
        sensor_liveness: "disarmed",
        sensor_fault: None,
        sensor_measured_hz: None,
        publisher_count_observed: false,
        // A refused request produced no perception, so there is no raw cap to
        // report and nothing was stabilized. Zero is the enforced value either way.
        raw_speed_cap_mps: 0.0,
        stabilized_speed_cap_mps: 0.0,
        cap_reason: None,
        cap_clear_streak: 0,
        cap_limiting_object: None,
        // A refused request never reached ingest, so nothing was discarded —
        // as distinct from "discarded nothing", which this shape cannot
        // express. The frame is already marked unhealthy with a zero cap.
        rejected: RejectionOut::default(),
    };
    // The response is already at the MRC floor, so the liveness verdict cannot
    // lower it further — it is stamped so the REASON travels with the refusal.
    apply_sensor_health(&mut response, health, false);
    response
}

#[derive(Clone, Serialize)]
pub struct ObjOut {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    /// True when this object was NOT observed this frame — a coasted track,
    /// retained through a detection dropout and emitted at an extrapolated
    /// position.
    ///
    /// The checker treats it exactly as an observed object: retaining a hazard
    /// tightens, and a consumer ignoring this flag gets the conservative
    /// reading. The flag exists so an AUDIT can tell what the robot actually
    /// saw from what it inferred — which is why it is also hashed into the
    /// evidence digest rather than being presentation-only.
    pub coasted: bool,
}

/// One tracked pedestrian/VRU emitted by Taj for the planner's pedestrian RSS
/// scene. Coordinates and velocity are in the ego frame used by `objects`.
#[derive(Clone, Serialize)]
pub struct PedestrianOut {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub age_s: f64,
    /// Geometric lidar classification before camera influence.
    pub lidar_classification: &'static str,
    /// Final classification used for pedestrian RSS membership.
    pub fused_classification: &'static str,
    /// Stable explanation of the camera influence decision.
    pub fusion_reason: &'static str,
    pub camera_observation_id: Option<u64>,
    pub camera_confidence: Option<f64>,
    pub association_distance_m: Option<f64>,
    /// True when the underlying lidar track was NOT observed this frame.
    ///
    /// This channel is the reason the marker matters most: a coasted track
    /// already reaches the checker's omnidirectional pedestrian RSS bound
    /// (`classify_vrus` walks every retained track), so without this flag a
    /// pedestrian the sensor did not see is indistinguishable from one it did.
    pub coasted: bool,
}

/// Auditable supporting association between camera/depth geometry and an
/// existing lidar track.
#[derive(Clone, Serialize)]
pub struct CameraAssociationOut {
    pub track_id: u64,
    pub observation_id: u64,
    pub distance_m: f64,
    pub confidence: f64,
}

/// One bounded future sample for a fused VRU track.
#[derive(Clone, Serialize)]
pub struct PredictedVruPointOut {
    pub time_s: f64,
    pub x: f64,
    pub y: f64,
    pub uncertainty_radius_m: f64,
}

/// One normalized intent hypothesis emitted by Taj.
#[derive(Clone, Serialize)]
pub struct PredictedVruIntentProbabilityOut {
    pub intent: &'static str,
    pub probability: f64,
}

/// Auditable VRU motion prediction emitted by Taj.
///
/// Prediction remains supporting evidence. It does not create tracks, alter
/// fused classification membership, or replace Kirra's pedestrian RSS bound.
#[derive(Clone, Serialize)]
pub struct PredictedVruMotionOut {
    pub track_id: u64,
    pub model: &'static str,
    pub intent: &'static str,
    pub intent_confidence: f64,
    pub intent_reason: &'static str,
    /// Complete normalized distribution in stable intent order.
    pub intent_probabilities: Vec<PredictedVruIntentProbabilityOut>,
    pub fallback_reason: Option<&'static str>,
    pub horizon_s: f64,
    pub step_s: f64,
    pub source_age_s: f64,
    pub frames_seen: u32,
    pub points: Vec<PredictedVruPointOut>,
}

/// The generation value reserved for "this is not real evidence" (#1214).
///
/// [`PerceptionFrameId::invalid`] is all-zeros, and every downstream consumer
/// refuses a zero generation on exactly that basis — a default-constructed or
/// zeroed frame must never pass as a perception result. That reservation only
/// holds if no REAL frame can also carry zero, so the first live generation is
/// [`FIRST_TRACKER_GENERATION`], not zero.
///
/// This is not cosmetic. Taj's first generation used to be zero, which collided
/// with the sentinel: a freshly-started sidecar served frames the planner seam
/// refused as invalid (`INVALID_PERCEPTION_FRAME_ID`), so the evidence-bound
/// motion path could not work from cold start at all. It failed closed — the
/// robot held rather than moving on unbound evidence — but the binding was dead
/// on arrival until a tracker reset happened to advance the counter off zero.
const RESERVED_INVALID_TRACKER_GENERATION: u64 = 0;

/// The first generation a live tracker may report. See
/// [`RESERVED_INVALID_TRACKER_GENERATION`].
/// The live counter's type is [`std::num::NonZeroU64`], so the reserved value is
/// not merely avoided by the code that advances it — it is unrepresentable. That
/// is deliberately stronger than a guard at each mutation site: the original
/// defect was a single initializer holding zero, and a scattered-guard scheme
/// depends on every future initializer and every future arithmetic call
/// remembering the reservation. `saturating_add` cannot wrap to zero today, but
/// nothing except the type stops someone changing it to `wrapping_add`.
const FIRST_TRACKER_GENERATION: NonZeroU64 = NonZeroU64::MIN;

/// Stable identity for one accepted Taj perception result.
///
/// Sequence and generation fields prevent stale or replayed evidence from being
/// confused with a newer perception cycle. The evidence digest binds the
/// safety-relevant result bytes using a deterministic canonical encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PerceptionFrameId {
    pub scan_sequence: u64,
    pub camera_sequence: Option<u64>,
    pub tracker_generation: u64,
    pub profile_digest: String,
    pub evidence_digest: String,
}

impl PerceptionFrameId {
    #[must_use]
    fn invalid() -> Self {
        Self {
            scan_sequence: 0,
            camera_sequence: None,
            tracker_generation: RESERVED_INVALID_TRACKER_GENERATION,
            profile_digest: String::new(),
            evidence_digest: String::new(),
        }
    }

    /// Temporary construction used until the canonical profile/evidence digest
    /// encoder is wired in the next slice.
    #[must_use]
    fn pending(scan_sequence: u64, camera_sequence: Option<u64>, tracker_generation: u64) -> Self {
        Self {
            scan_sequence,
            camera_sequence,
            tracker_generation,
            profile_digest: String::new(),
            evidence_digest: String::new(),
        }
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    hasher.update(len.to_le_bytes());
    hasher.update(bytes);
}

fn hash_str(hasher: &mut Sha256, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_bool(hasher: &mut Sha256, value: bool) {
    hasher.update([u8::from(value)]);
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

fn hash_usize(hasher: &mut Sha256, value: usize) {
    hash_u64(hasher, u64::try_from(value).unwrap_or(u64::MAX));
}

fn hash_f64(hasher: &mut Sha256, value: f64) {
    hasher.update(value.to_bits().to_le_bytes());
}

fn hash_f32(hasher: &mut Sha256, value: f32) {
    hasher.update(value.to_bits().to_le_bytes());
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hash_bool(hasher, true);
            hash_u64(hasher, value);
        }
        None => hash_bool(hasher, false),
    }
}

fn hash_optional_f64(hasher: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            hash_bool(hasher, true);
            hash_f64(hasher, value);
        }
        None => hash_bool(hasher, false),
    }
}

fn hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_bool(hasher, true);
            hash_str(hasher, value);
        }
        None => hash_bool(hasher, false),
    }
}

/// Digest of the safety-relevant configuration used to interpret one request.
///
/// This deliberately excludes sensor observations and timestamps. Those belong
/// to the evidence digest. The domain tag prevents reuse as another Kirra hash.
fn compute_perception_profile_digest(req: &PerceptionRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"KIRRA-TAJ-PROFILE-V1");

    hash_f64(&mut hasher, req.angle_min_rad);
    hash_f64(&mut hasher, req.angle_increment_rad);
    hash_f64(&mut hasher, req.range_min_m);
    hash_f64(&mut hasher, req.range_max_m);
    hash_f64(&mut hasher, req.forward_extent_m);
    hash_f64(&mut hasher, req.decel_mps2);
    hash_f64(&mut hasher, req.margin_m);
    hash_f64(&mut hasher, req.lane_half_m);
    hash_f64(&mut hasher, req.vehicle_width_m);
    hash_f64(&mut hasher, req.lateral_clearance_m);
    hash_f32(&mut hasher, req.confidence_floor);
    hash_bool(&mut hasher, req.camera_armed);
    hash_u64(&mut hasher, req.camera_max_age_ms);

    hex::encode(hasher.finalize())
}

/// Digest of the complete accepted safety-relevant Taj output.
///
/// `frame_id.evidence_digest` is intentionally excluded to avoid circular
/// hashing. The already-computed profile digest is included, binding evidence
/// interpretation to the exact configuration that produced it.
fn compute_perception_evidence_digest(response: &PerceptionResponse) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"KIRRA-TAJ-EVIDENCE-V1");

    hash_u64(&mut hasher, response.frame_id.scan_sequence);
    hash_optional_u64(&mut hasher, response.frame_id.camera_sequence);
    hash_u64(&mut hasher, response.frame_id.tracker_generation);
    hash_str(&mut hasher, &response.frame_id.profile_digest);

    hash_bool(&mut hasher, response.healthy);
    hash_f32(&mut hasher, response.confidence);
    hash_u64(&mut hasher, response.age_ms);
    hash_f64(&mut hasher, response.clear_distance_m);
    hash_optional_f64(&mut hasher, response.nearest_object_m);
    hash_usize(&mut hasher, response.object_count);
    hash_optional_f64(&mut hasher, response.minimum_corridor_width_m);
    hash_f64(&mut hasher, response.required_corridor_width_m);
    hash_f64(&mut hasher, response.speed_cap_mps);

    hash_usize(&mut hasher, response.left.len());
    for point in &response.left {
        hash_f64(&mut hasher, point[0]);
        hash_f64(&mut hasher, point[1]);
    }

    hash_usize(&mut hasher, response.right.len());
    for point in &response.right {
        hash_f64(&mut hasher, point[0]);
        hash_f64(&mut hasher, point[1]);
    }

    hash_usize(&mut hasher, response.objects.len());
    for object in &response.objects {
        hash_u64(&mut hasher, object.id);
        hash_f64(&mut hasher, object.x);
        hash_f64(&mut hasher, object.y);
        hash_f64(&mut hasher, object.vx);
        hash_f64(&mut hasher, object.vy);
        // Observed vs coasted changes what the evidence MEANS, so it belongs
        // in the digest: two frames identical but for which objects were
        // actually seen must not share an evidence identity.
        hash_bool(&mut hasher, object.coasted);
    }

    hash_usize(&mut hasher, response.camera_associations.len());
    for association in &response.camera_associations {
        hash_u64(&mut hasher, association.track_id);
        hash_u64(&mut hasher, association.observation_id);
        hash_f64(&mut hasher, association.distance_m);
        hash_f64(&mut hasher, association.confidence);
    }

    hash_usize(&mut hasher, response.pedestrians.len());
    for pedestrian in &response.pedestrians {
        hash_u64(&mut hasher, pedestrian.id);
        hash_f64(&mut hasher, pedestrian.x);
        hash_f64(&mut hasher, pedestrian.y);
        hash_f64(&mut hasher, pedestrian.vx);
        hash_f64(&mut hasher, pedestrian.vy);
        hash_f64(&mut hasher, pedestrian.age_s);
        hash_str(&mut hasher, pedestrian.lidar_classification);
        hash_str(&mut hasher, pedestrian.fused_classification);
        hash_str(&mut hasher, pedestrian.fusion_reason);
        hash_bool(&mut hasher, pedestrian.coasted);
        hash_optional_u64(&mut hasher, pedestrian.camera_observation_id);
        hash_optional_f64(&mut hasher, pedestrian.camera_confidence);
        hash_optional_f64(&mut hasher, pedestrian.association_distance_m);
    }

    hash_usize(&mut hasher, response.predicted_vrus.len());
    for prediction in &response.predicted_vrus {
        hash_u64(&mut hasher, prediction.track_id);
        hash_str(&mut hasher, prediction.model);
        hash_str(&mut hasher, prediction.intent);
        hash_f64(&mut hasher, prediction.intent_confidence);
        hash_str(&mut hasher, prediction.intent_reason);

        hash_usize(&mut hasher, prediction.intent_probabilities.len());
        for hypothesis in &prediction.intent_probabilities {
            hash_str(&mut hasher, hypothesis.intent);
            hash_f64(&mut hasher, hypothesis.probability);
        }

        hash_optional_str(&mut hasher, prediction.fallback_reason);
        hash_f64(&mut hasher, prediction.horizon_s);
        hash_f64(&mut hasher, prediction.step_s);
        hash_f64(&mut hasher, prediction.source_age_s);
        hash_u64(&mut hasher, u64::from(prediction.frames_seen));

        hash_usize(&mut hasher, prediction.points.len());
        for point in &prediction.points {
            hash_f64(&mut hasher, point.time_s);
            hash_f64(&mut hasher, point.x);
            hash_f64(&mut hasher, point.y);
            hash_f64(&mut hasher, point.uncertainty_radius_m);
        }
    }

    hash_bool(&mut hasher, response.camera_healthy);
    hash_optional_f64(&mut hasher, response.camera_clip_x_m);

    hex::encode(hasher.finalize())
}

#[derive(Clone, Serialize)]
pub struct PerceptionResponse {
    pub frame_id: PerceptionFrameId,
    pub healthy: bool,
    pub confidence: f32,
    pub age_ms: u64,
    pub clear_distance_m: f64,
    pub nearest_object_m: Option<f64>,
    pub object_count: usize,
    /// Narrowest measured free-space width across the fused corridor.
    pub minimum_corridor_width_m: Option<f64>,
    /// Width required for the configured vehicle plus bilateral clearance.
    pub required_corridor_width_m: f64,
    pub speed_cap_mps: f64,
    // The corridor geometry + objects, in the SAME shapes the Occy planner
    // endpoint (POST /plan) consumes, so the doer bridge passes them through.
    pub left: Vec<[f64; 2]>,
    pub right: Vec<[f64; 2]>,
    pub objects: Vec<ObjOut>,
    /// Supporting-only camera/depth associations. These do not create tracks,
    /// modify positions, or change pedestrian classification membership.
    pub camera_associations: Vec<CameraAssociationOut>,
    /// Conservative VRU classifications derived from the persistent tracker.
    ///
    /// First sightings with a pedestrian-sized footprint are included because
    /// unknown velocity cannot safely disprove the pedestrian envelope.
    pub pedestrians: Vec<PedestrianOut>,
    /// Bounded predictions for tracks whose fused classification feeds
    /// pedestrian RSS. Missing predictions preserve snapshot-only behavior.
    pub predicted_vrus: Vec<PredictedVruMotionOut>,
    /// Phase-B observability: `false` when the camera channel is ARMED but the
    /// frame is missing / stale / undecodable (the request then fails closed to
    /// the MRC floor). `true` when disarmed (nothing to be unhealthy about) or
    /// fresh.
    pub camera_healthy: bool,
    /// Forward distance at which the camera fusion clipped the corridor, when a
    /// detection bound it. `None` = no camera hazard bound the corridor. The
    /// clip can only SHORTEN the drivable space.
    pub camera_clip_x_m: Option<f64>,
    /// #1211 sensor-liveness verdict for this scan: `disarmed` | `healthy` |
    /// `warming_up` | `recovering` | `faulted`.
    pub sensor_liveness: &'static str,
    /// The distinct fault code when the watchdog faulted (`SENSOR_FROZEN_FRAME`,
    /// `SENSOR_RATE_BELOW_FLOOR`, …), else `None`. Never collapsed into a single
    /// "sensor bad" — the whole point of #1211's reason codes.
    pub sensor_fault: Option<&'static str>,
    /// Measured publish rate over the watchdog's arrival window, when enough
    /// frames have been seen to compute one. A NUMBER, so an operator can see how
    /// starved the sensor is rather than only that it tripped a threshold.
    pub sensor_measured_hz: Option<f64>,
    /// Whether the request carried a ROS-observed publisher count. `false` means
    /// the publisher check did not run for this frame because the caller had no
    /// ROS graph to observe — surfaced so the gap is visible, never silent.
    pub publisher_count_observed: bool,
    /// #1212: the assured-clear-distance cap **as perception computed it**, before
    /// temporal stabilization. Observability only — `speed_cap_mps` is the enforced
    /// number. Carried separately because conflating the two is how the Python
    /// version's behaviour became invisible: a consumer that only ever saw one
    /// number could not tell a hazard from a filter still catching up.
    pub raw_speed_cap_mps: f64,
    /// The stabilized cap the checker enforces. Always equal to `speed_cap_mps`;
    /// named explicitly so the wire is unambiguous about which authority produced
    /// it.
    pub stabilized_speed_cap_mps: f64,
    /// Why the stabilizer produced this value (`CAP_RESTRICTED`, `CAP_RISING`,
    /// `CAP_TIME_GAP`, `CAP_REPLAY_HELD`, …).
    ///
    /// `None` means **this sidecar performed no stabilization at all** — a stateless
    /// call or a build that does not stabilize. It is a statement about the
    /// authority that produced `speed_cap_mps`, not about the outcome, which is why
    /// a consumer may treat it as unusable and fail closed. Every path where the
    /// stabilizer DID author the number carries a code, including a replay of a cap
    /// it is holding from an earlier frame.
    pub cap_reason: Option<&'static str>,
    /// Consecutive clear frames accumulated toward a release.
    pub cap_clear_streak: u32,
    /// The object currently holding the cap down, including one retained through a
    /// single-scan dropout. `None` when the corridor geometry binds rather than an
    /// object, or when nothing limits.
    pub cap_limiting_object: Option<u64>,
    /// Per-reason accounting for the lidar returns this frame discarded
    /// (#1210) — the operator-facing half of the self-filter.
    ///
    /// **Observability only, deliberately OUTSIDE the evidence digest.** The
    /// tally does not change any verdict, and the one way self-filtering can
    /// affect safety — a mask consuming so much of the scan that the remainder
    /// is thin evidence — already reaches the digest through `confidence`,
    /// which the ray-fraction ceiling drives to zero. Folding a pure counter
    /// into the digest would make two frames with identical geometry hash
    /// differently because one had a noisier ray, which is not what evidence
    /// identity should mean.
    pub rejected: RejectionOut,
}

/// Wire form of [`kirra_taj::RejectionTally`].
///
/// Mirrored rather than re-exported so the JSON field names are owned by this
/// wire contract: a rename in the perception crate should break this file
/// loudly rather than silently reshaping what an operator's dashboard reads.
#[derive(Clone, Copy, Default, Serialize)]
pub struct RejectionOut {
    /// Range was NaN — a sensor producing garbage. Note that `+inf` is NOT
    /// counted here: it is the ROS no-return convention and lands in
    /// `beyond_max_range`, so an empty room does not read as a failing lidar.
    pub not_a_number: u32,
    /// Range below the scan's declared minimum.
    pub below_min_range: u32,
    /// Range above the scan's declared maximum, including the `+inf` a ROS
    /// LaserScan uses for "no return on this ray". A count near the ray total
    /// is open space or blocked optics — the two look identical here, which is
    /// why the watchdog work in #1211 is a separate issue.
    pub beyond_max_range: u32,
    /// Discarded as the robot's own body. Zero whenever no mask is configured;
    /// a value that climbs after a mask revision is the signal that the new
    /// geometry filters more than the old one.
    pub self_masked: u32,
}

impl From<kirra_taj::RejectionTally> for RejectionOut {
    fn from(t: kirra_taj::RejectionTally) -> Self {
        Self {
            not_a_number: t.not_a_number,
            below_min_range: t.below_min_range,
            beyond_max_range: t.beyond_max_range,
            self_masked: t.self_masked,
        }
    }
}

/// The corridor's straight-ahead reach: the smaller of the two boundary
/// polylines' furthest forward point. Taj clips this at a dead-ahead
/// obstacle, so it already encodes the clear distance for the lane centre.
fn corridor_reach(corr: &impl CorridorSource) -> f64 {
    let far =
        |pts: &[kirra_core::corridor::Point]| pts.iter().map(|p| p.x_m).fold(0.0_f64, f64::max);
    far(corr.left_boundary()).min(far(corr.right_boundary()))
}

/// Minimum traversable corridor width before the terminal reach.
///
/// Left and right boundaries may contain different numbers of vertices after
/// camera hazard clipping. Width is therefore measured only at longitudinal
/// stations represented by both boundaries.
///
/// The terminal stopping boundary is excluded: it limits forward reach rather
/// than representing space through which the vehicle must fit.
fn minimum_corridor_width_before_reach_m(
    corr: &impl CorridorSource,
    corridor_reach_m: f64,
) -> Option<f64> {
    const X_EPSILON_M: f64 = 1e-6;

    if !corridor_reach_m.is_finite() || corridor_reach_m <= 0.0 {
        return None;
    }

    let left = corr.left_boundary();
    let right = corr.right_boundary();

    if left.is_empty() || right.is_empty() {
        return None;
    }

    // Reject malformed geometry anywhere in either boundary, including points
    // that are not ultimately used as shared width stations.
    if left
        .iter()
        .chain(right.iter())
        .any(|point| !point.x_m.is_finite() || !point.y_m.is_finite())
    {
        return None;
    }

    let mut minimum_width_m = f64::INFINITY;
    let mut shared_station_count = 0usize;

    for left_point in left {
        // A clipped terminal point represents the stop line/hazard boundary.
        // It constrains reach but is not traversable corridor width.
        if left_point.x_m >= corridor_reach_m - X_EPSILON_M {
            continue;
        }

        let Some(right_point) = right
            .iter()
            .find(|point| (point.x_m - left_point.x_m).abs() <= X_EPSILON_M)
        else {
            continue;
        };

        let width_m = left_point.y_m - right_point.y_m;

        if !width_m.is_finite() || width_m <= 0.0 {
            return None;
        }

        minimum_width_m = minimum_width_m.min(width_m);
        shared_station_count += 1;
    }

    if shared_station_count == 0 || !minimum_width_m.is_finite() {
        None
    } else {
        Some(minimum_width_m)
    }
}

/// Minimum forward longitudinal distance for an object to participate in
/// collision-distance gating.
///
/// Returns closer than this are treated as near-origin/self-geometry candidates.
/// This does NOT remove them from raw perception or corridor construction.
const MIN_FORWARD_OBJECT_X_M: f64 = 0.15;

/// Maximum accepted `forward_extent_m` (metres) for a perception request.
///
/// PHYSICAL RATIONALE. The forward extent is a perception HORIZON, and no
/// ground-vehicle horizon usefully exceeds this. The longest-range production
/// automotive lidars reach roughly 250-300 m; the deployed R2 TG30 maxes at
/// 12 m, and every extent configured anywhere in this repository is 8-40 m. So
/// 500 m is ~2x the best sensor in the industry and >12x the largest value we
/// actually use — generous enough that no legitimate deployment meets it, tight
/// enough that a unit mistake does not sail through.
///
/// COMPUTATIONAL RATIONALE. `TajPhaseA::extract_corridor` is
/// O(stations x points) with `stations = ceil(extent / station_m)` and
/// `station_m` floored at 0.1 m, so the extent alone sets the work. Capping at
/// 500 m bounds stations at 5,000 (== `kirra_taj::MAX_CORRIDOR_STATIONS`),
/// keeping the corridor pass finite and predictable for any accepted request.
/// Without a cap the cost is unbounded in a caller-supplied f64: 1e300 m does
/// not merely run slowly, it saturates the float->usize cast to `usize::MAX`,
/// which panicked on `nstations + 1` in debug and became a ~1.8e19-iteration
/// loop in release.
///
/// Deliberately paired with that library ceiling: a request accepted here can
/// never reach the backstop, and the backstop covers callers that bypass this
/// validation entirely (`TajConfig` is public).
const MAX_FORWARD_EXTENT_M: f64 = 500.0;

/// Maximum bearing from the vehicle forward axis for an object to participate in
/// the scalar nearest-object braking bound.
///
/// Objects outside this cone may still influence corridor geometry and turning
/// safety; this predicate only governs the straight-ahead ACD object bound.
const MAX_FORWARD_OBJECT_BEARING_RAD: f64 = std::f64::consts::FRAC_PI_3;

/// Whether a perceived object is eligible for the nearest-forward-object
/// assured-clear-distance bound.
///
/// Collision-object gating is intentionally separate from corridor-boundary
/// extraction. Side walls and nearly lateral returns must not become frontal
/// collision objects, but they may still define drivable-space boundaries.
#[inline]
#[must_use]
fn is_forward_object_candidate(
    x_m: f64,
    y_m: f64,
    forward_extent_m: f64,
    lane_half_m: f64,
) -> bool {
    if !x_m.is_finite()
        || !y_m.is_finite()
        || !forward_extent_m.is_finite()
        || !lane_half_m.is_finite()
        || forward_extent_m <= 0.0
        || lane_half_m < 0.0
    {
        return false;
    }

    x_m >= MIN_FORWARD_OBJECT_X_M
        && x_m <= forward_extent_m
        && y_m.abs() <= lane_half_m
        && y_m.atan2(x_m).abs() <= MAX_FORWARD_OBJECT_BEARING_RAD
}

/// Pick the nearest `(id, distance)` candidate, or `None` when none is usable.
///
/// Extracted from the perception path so the ordering is testable on its own. It has
/// two properties the caller depends on and neither is obvious from the call site:
///
/// **Non-finite distances are dropped BEFORE the comparison**, not after a winner is
/// chosen. The ordering below is a total order over finite values only; a NaN reaching
/// it compares `false` against every candidate and therefore WINS every match arm.
/// Filtering afterwards would discard that winner and report no nearest object at all
/// — so one malformed distance would hide every real obstacle in the frame. Today the
/// caller's inputs are already proven finite (`is_forward_object_candidate` rejects a
/// non-finite `x`, and the near-edge lookup is finite-filtered), which is exactly why
/// the guard belongs here: the invariant lives with the code that needs it rather than
/// depending on two callers upstream continuing to hold it.
///
/// **Ties break on the lower id, compared exactly.** Two objects at an identical
/// distance must not alternate the reported identity frame to frame — to the
/// stabilizer's limiting-object persistence that alternation is indistinguishable from
/// a dropout. The comparison is never epsilon-based: an approximate "near enough"
/// predicate is not transitive, so a three-candidate frame could order inconsistently
/// depending on iteration order. Two distances differing in the last bit are two
/// distances, and the nearer one wins.
#[must_use]
fn nearest_candidate(candidates: impl Iterator<Item = (u64, f64)>) -> Option<(u64, f64)> {
    candidates
        .filter(|(_, d)| d.is_finite())
        .fold(None::<(u64, f64)>, |acc, (id, d)| match acc {
            Some((bid, bd)) if bd < d || (bd == d && bid <= id) => Some((bid, bd)),
            _ => Some((id, d)),
        })
}

/// `cap_reason` for a cap served WITHOUT the stabilizer observing the frame — a
/// duplicate re-post, served the value the stabilizer is currently holding.
///
/// Sidecar-local rather than a [`kirra_trajectory::cap_stabilizer::CapReason`]
/// variant, because it is not a decision the stabilizer made. The stabilizer never
/// saw this frame; the sidecar chose not to show it one. Putting it in the checker's
/// enum would oblige every consumer of that enum to reason about a state the state
/// machine cannot enter.
const CAP_REPLAY_HELD: &str = "CAP_REPLAY_HELD";

const MAX_TRACKING_GAP_MS: u64 = 500;

pub struct TrackedPerceptionState {
    tracker: Option<TajTracker>,
    tracker_config: Option<TajConfig>,
    last_stamp_ms: Option<u64>,
    last_response: Option<PerceptionResponse>,
    /// Monotonic identity of accepted scan frames.
    scan_sequence: u64,
    /// Monotonic identity of accepted camera frames.
    camera_sequence: u64,
    /// Producer timestamp of the last accepted camera frame.
    last_camera_stamp_ms: Option<u64>,
    /// Advances whenever temporal tracking state is invalidated.
    /// Live generation counter. NonZero by TYPE: zero is the invalid-frame
    /// sentinel and must be unrepresentable here, not merely unwritten.
    tracker_generation: NonZeroU64,
    /// #1211 sensor-liveness watchdog. Deliberately OUTSIDE [`Self::reset`]: the
    /// tracker resets on a timestamp regression or an oversized gap, and those are
    /// exactly the events the watchdog exists to remember. Clearing its history
    /// alongside the tracker would let a frozen driver launder its record by
    /// tripping a tracker reset each time it stalled.
    watchdog: SensorWatchdog,
    /// #1212 cap stabilizer. Like the watchdog, deliberately OUTSIDE [`Self::reset`]:
    /// a tracking reset is a discontinuity in object identity, not evidence that the
    /// speed envelope may be relaxed. Clearing the stabilizer here would let a
    /// tracker reset hand back an unearned cap.
    stabilizer: CapStabilizer,
    /// Whether cap stabilization runs. Off only for the stateless entry point, for
    /// the same reason the watchdog is off there: stabilization is a property of a
    /// STREAM — a restriction is "tighter than the last frame", a release is "clear
    /// for N frames". A one-shot call has no previous frame, so an armed stabilizer
    /// would hold every caller at the floor forever while asserting a temporal
    /// judgement it has no history to make.
    stabilize: bool,
}

impl Default for TrackedPerceptionState {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackedPerceptionState {
    /// State with the watchdog ARMED on the R2 LiDAR defaults — the deployment
    /// posture, since #1211's whole subject is that a frozen scan currently drives
    /// the robot. `KIRRA_SENSOR_WATCHDOG_ENABLED=0` is the opt-out; see
    /// [`watchdog_armed_from_env`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_watchdog(SensorWatchdogConfig::r2_lidar(), true)
    }

    /// State with an explicitly configured watchdog. Tests use this to inject
    /// thresholds and to run the disarmed path.
    ///
    /// # Panics
    ///
    /// Panics if `cfg` fails [`SensorWatchdogConfig::validate`]. Callers holding a
    /// config from configuration input should validate it themselves first and
    /// report the error; this constructor is for statically-known configs.
    #[must_use]
    pub fn with_watchdog(cfg: SensorWatchdogConfig, armed: bool) -> Self {
        Self::with_options(cfg, armed, true)
    }

    /// State with independent control of the liveness watchdog and cap
    /// stabilization. They are separate concerns — one asks whether the sensor is
    /// alive, the other how fast the envelope may change — so a deployment that
    /// disables one must not silently disable the other.
    ///
    /// # Panics
    ///
    /// Panics if `cfg` fails validation; see [`Self::with_watchdog`].
    #[must_use]
    pub fn with_options(cfg: SensorWatchdogConfig, armed: bool, stabilize: bool) -> Self {
        Self {
            tracker: None,
            tracker_config: None,
            last_stamp_ms: None,
            last_response: None,
            scan_sequence: 0,
            camera_sequence: 0,
            last_camera_stamp_ms: None,
            // Starts at ONE. Zero is the invalid-frame sentinel every consumer
            // refuses, so a live tracker must never report it — see
            // `RESERVED_INVALID_TRACKER_GENERATION`.
            tracker_generation: FIRST_TRACKER_GENERATION,
            watchdog: SensorWatchdog::new(cfg, armed).expect("watchdog config must be valid"),
            stabilizer: CapStabilizer::new(CapStabilizerConfig::default())
                .expect("the default stabilizer config is valid"),
            stabilize,
        }
    }

    /// Reset TRACKING state. The watchdog is untouched — see the field comment.
    pub fn reset(&mut self) {
        self.tracker = None;
        self.tracker_config = None;
        self.last_stamp_ms = None;
        self.last_response = None;
        self.last_camera_stamp_ms = None;
        // Saturates at u64::MAX rather than wrapping. NonZeroU64::saturating_add
        // cannot produce zero, so exhausting the counter degrades to a stuck
        // generation (frames stop reading as new) rather than to one that reads as
        // invalid evidence.
        self.tracker_generation = self.tracker_generation.saturating_add(1);
    }

    /// Operator reset of the watchdog's terminal latch (the counterpart to its
    /// sticky restart exhaustion). Separate from [`Self::reset`] precisely because
    /// clearing a terminal sensor fault is a human decision, not a side effect of
    /// a tracking discontinuity.
    pub fn reset_sensor_watchdog(&mut self) {
        self.watchdog.reset();
    }

    /// The watchdog's current terminal state, for operator surfaces.
    #[must_use]
    pub fn sensor_watchdog_terminal(&self) -> bool {
        self.watchdog.is_terminal()
    }

    /// Whether scan-liveness checking is ARMED on this service.
    ///
    /// Surfaced on `GET /health` so a disarmed deployment is legible to a
    /// diagnostic rather than only to whoever read the startup log. A safety check
    /// that is off must never be indistinguishable from one that is on and
    /// passing — that is the same failure the `/health` endpoint's own contract
    /// field exists to prevent.
    #[must_use]
    pub fn sensor_watchdog_armed(&self) -> bool {
        self.watchdog.is_armed()
    }
}

/// Whether the scan-liveness watchdog is armed, from
/// `KIRRA_SENSOR_WATCHDOG_ENABLED`.
///
/// **Default ARMED**, inverting the pure module's default-off. The inversion is
/// deliberate and belongs here: `kirra_trajectory::sensor_watchdog` defaults off
/// because it is a library with unknown consumers, while this sidecar IS the R2
/// deployment, and #1211 exists because a frozen LiDAR currently drives that
/// robot. Shipping the wiring disarmed would leave the failure exactly where the
/// issue found it.
///
/// `0` / `false` / `no` (case-insensitive) opts out — the immediate mitigation if
/// bring-up shows false trips before the thresholds are characterised on hardware.
#[must_use]
pub fn watchdog_armed_from_env() -> bool {
    std::env::var(SENSOR_WATCHDOG_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            !(t == "0" || t.eq_ignore_ascii_case("false") || t.eq_ignore_ascii_case("no"))
        })
        .unwrap_or(true)
}

/// Whether a rejected request was rejected because of the SENSOR or because of
/// the CALLER's configuration.
///
/// The split matters because the two have different owners and different fixes. A
/// sensor fault must reach the watchdog (it resets the recovery streak, and it is
/// what an operator needs to see); a caller's bad `margin_m` must not, or a
/// misconfigured client could pin a perfectly healthy LiDAR in recovery forever.
fn is_scan_fault(err: PerceptionRequestError) -> bool {
    match err {
        // The scan itself, or the geometry describing it.
        PerceptionRequestError::EmptyScan
        | PerceptionRequestError::ScanTooLarge
        | PerceptionRequestError::NanRangeSample
        | PerceptionRequestError::InvalidRangeBounds
        | PerceptionRequestError::InvalidAngleMinimum
        | PerceptionRequestError::InvalidAngleIncrement => true,
        // Everything else describes how the CALLER wants the scan interpreted.
        PerceptionRequestError::InvalidForwardExtent
        | PerceptionRequestError::ForwardExtentTooLarge
        | PerceptionRequestError::InvalidDeceleration
        | PerceptionRequestError::InvalidMargin
        | PerceptionRequestError::InvalidLaneHalfWidth
        | PerceptionRequestError::InvalidVehicleWidth
        | PerceptionRequestError::InvalidLateralClearance
        | PerceptionRequestError::LaneNarrowerThanRequiredCorridor
        | PerceptionRequestError::InvalidConfidenceFloor => false,
    }
}

/// Count returns that are finite AND inside the sensor's own declared range
/// bounds.
///
/// The ROS convention is that a no-return reads `+inf`, so an infinite value is
/// not a broken ray — it is "nothing out there". It is still not *evidence of
/// clear space* the way a real measurement is, which is why it does not count
/// toward the valid ratio: a scan of nothing but no-returns is a scan that saw
/// nothing, and `SENSOR_LOW_VALID_RAY_RATIO` is the correct verdict on it.
fn count_valid_rays(req: &PerceptionRequest) -> u32 {
    req.ranges
        .iter()
        .filter(|r| r.is_finite() && **r >= req.range_min_m as f32 && **r <= req.range_max_m as f32)
        .count()
        .min(u32::MAX as usize) as u32
}

/// Build the frame observation and tick the watchdog.
///
/// The fingerprint comes from `kirra_trajectory::sensor_watchdog` rather than
/// being computed here. That is not merely code reuse: the freeze check's entire
/// discrimination between a frozen buffer and a static scene rests on hashing raw
/// IEEE bits, and a second implementation in this file would be free to quantise
/// and silently destroy it.
fn observe_scan(
    state: &mut TrackedPerceptionState,
    req: &PerceptionRequest,
    now_ms: u64,
) -> SensorHealth {
    let frame = FrameObservation {
        header_stamp_ms: req.stamp_ms,
        fingerprint: fingerprint_from_ranges(&req.ranges),
        valid_rays: count_valid_rays(req),
        total_rays: req.ranges.len().min(u32::MAX as usize) as u32,
    };
    state.watchdog.observe(SensorTick {
        now_ms,
        // An unobserved publisher count is passed as 1 — "not zero" — so the
        // publisher check is inert rather than firing on a fact the caller could
        // not know. `publisher_count_observed` on the response records which
        // happened, so this is visible rather than an invisible default.
        publisher_count: req.publisher_count.unwrap_or(1),
        frame: Some(frame),
    })
}

/// Tick the watchdog for a scan that failed validation.
///
/// The frame is reported as it actually is — zero valid rays for an empty or
/// all-NaN scan — so a malformed scan lands on the same `SENSOR_LOW_VALID_RAY_RATIO`
/// code a blinded one does, rather than inventing a parallel vocabulary for the
/// same physical condition.
fn observe_scan_fault(
    state: &mut TrackedPerceptionState,
    req: &PerceptionRequest,
    now_ms: u64,
) -> SensorHealth {
    let total_rays = req.ranges.len().min(u32::MAX as usize) as u32;
    let frame = FrameObservation {
        header_stamp_ms: req.stamp_ms,
        fingerprint: fingerprint_from_ranges(&req.ranges),
        // Range bounds may themselves be the invalid thing, so no ray can be
        // certified valid against them.
        valid_rays: 0,
        total_rays,
    };
    state.watchdog.observe(SensorTick {
        now_ms,
        publisher_count: req.publisher_count.unwrap_or(1),
        frame: Some(frame),
    })
}

/// The wire name for a liveness verdict.
fn sensor_liveness_name(health: SensorHealth) -> &'static str {
    match health {
        SensorHealth::Disarmed => "disarmed",
        SensorHealth::Healthy => "healthy",
        SensorHealth::WarmingUp { .. } => "warming_up",
        SensorHealth::Recovering { .. } => "recovering",
        SensorHealth::Faulted(_) => "faulted",
    }
}

/// Compose the liveness verdict into a response.
///
/// The cap is composed with `min`, never assignment, so a watchdog fault cannot be
/// overwritten by a healthier channel — and equally, a healthy watchdog cannot
/// raise a cap that Taj's own corridor arithmetic, the camera channel, or the
/// confidence floor already lowered. Whichever bound is tighter wins, which is the
/// same composition rule every sibling perception channel uses.
fn apply_sensor_health(
    response: &mut PerceptionResponse,
    health: SensorHealth,
    publisher_count_observed: bool,
) {
    response.sensor_liveness = sensor_liveness_name(health);
    response.sensor_fault = health.fault().map(|f| f.code());
    response.publisher_count_observed = publisher_count_observed;

    if let Some(cap) = health.perception_cap() {
        response.speed_cap_mps = response.speed_cap_mps.min(cap);
        // A sensor that is not demonstrably live makes the whole perception
        // result unhealthy, not merely slow. A consumer watching `healthy` must
        // not read "the corridor is fine" off evidence the watchdog just refused.
        response.healthy = false;
    }
}

/// Stateless compatibility entry point.
///
/// Production callers should use [`handle_perception_tracked`] with persistent
/// state so object IDs and velocity estimates survive across scan frames.
pub fn handle_perception(req: &PerceptionRequest) -> PerceptionResponse {
    // The watchdog is DISARMED on this path, and that is a correctness choice
    // rather than a compatibility concession. Liveness is a property of a STREAM:
    // frozen stamps, repeated buffers and measured rate all mean "compared to the
    // frames before this one". A stateless call has no frames before this one, so
    // an armed watchdog here would report `WarmingUp` on every single call and cap
    // every caller forever — asserting a liveness verdict from one sample with no
    // continuity to derive it from.
    //
    // `sensor_liveness: "disarmed"` is the truthful answer for a one-shot
    // evaluation. Callers that need liveness must hold state, which is what the
    // service does.
    let mut state =
        TrackedPerceptionState::with_options(SensorWatchdogConfig::r2_lidar(), false, false);
    handle_perception_tracked(req, &mut state)
}

fn vru_motion_model_wire_name(model: VruMotionModel) -> &'static str {
    match model {
        VruMotionModel::Stationary => "stationary",
        VruMotionModel::ConstantVelocity => "constant_velocity",
        VruMotionModel::BoundedTurnRate => "bounded_turn_rate",
        VruMotionModel::OmnidirectionalFallback => "omnidirectional_fallback",
    }
}

fn vru_intent_wire_name(intent: VruIntentClass) -> &'static str {
    intent.as_str()
}

fn vru_intent_reason_wire_name(reason: VruIntentReason) -> &'static str {
    reason.as_str()
}

fn vru_prediction_fallback_wire_name(reason: VruPredictionFallbackReason) -> &'static str {
    match reason {
        VruPredictionFallbackReason::InvalidConfiguration => "invalid_configuration",
        VruPredictionFallbackReason::NonFiniteTrackState => "nonfinite_track_state",
        VruPredictionFallbackReason::InsufficientHistory => "insufficient_history",
        VruPredictionFallbackReason::StaleTrack => "stale_track",
        VruPredictionFallbackReason::ExcessiveSpeed => "excessive_speed",
        VruPredictionFallbackReason::ExcessiveYawRate => "excessive_yaw_rate",
    }
}

fn predicted_vru_to_wire(prediction: &PredictedVruMotion) -> PredictedVruMotionOut {
    PredictedVruMotionOut {
        track_id: prediction.track_id,
        model: vru_motion_model_wire_name(prediction.model),
        intent: vru_intent_wire_name(prediction.intent),
        intent_confidence: prediction.intent_confidence,
        intent_reason: vru_intent_reason_wire_name(prediction.intent_reason),
        intent_probabilities: prediction
            .intent_probabilities
            .iter()
            .map(|hypothesis| PredictedVruIntentProbabilityOut {
                intent: vru_intent_wire_name(hypothesis.intent),
                probability: hypothesis.probability,
            })
            .collect(),
        fallback_reason: prediction
            .fallback_reason
            .map(vru_prediction_fallback_wire_name),
        horizon_s: prediction.horizon_s,
        step_s: prediction.step_s,
        source_age_s: prediction.source_age_s,
        frames_seen: prediction.frames_seen,
        points: prediction
            .points
            .iter()
            .map(|point| PredictedVruPointOut {
                time_s: point.time_s,
                x: point.pos.x_m,
                y: point.pos.y_m,
                uncertainty_radius_m: point.uncertainty_radius_m,
            })
            .collect(),
    }
}

/// Stateful perception path used by the Taj service.
///
/// Tracking state is reset on malformed input, timestamp regression, excessive
/// inter-frame gaps, or a material tracker-configuration change. Every reset
/// returns to conservative first-sighting behavior with zero estimated velocity.
///
/// Reads the wall clock for the watchdog's arrival observation. Prefer
/// [`handle_perception_tracked_at`], which takes the clock as a parameter — the
/// service binary uses it, and every test does.
pub fn handle_perception_tracked(
    req: &PerceptionRequest,
    state: &mut TrackedPerceptionState,
) -> PerceptionResponse {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    handle_perception_tracked_at(req, state, now_ms)
}

/// [`handle_perception_tracked`] with the arrival clock supplied by the caller.
///
/// `now_ms` is the time this scan was OBSERVED, not the stamp the sensor wrote.
/// Keeping them separate is the point: a frozen driver's stamp stops advancing
/// while arrivals keep coming, and a watchdog reading liveness off the sensor's
/// own stamp would freeze with it.
pub fn handle_perception_tracked_at(
    req: &PerceptionRequest,
    state: &mut TrackedPerceptionState,
    now_ms: u64,
) -> PerceptionResponse {
    // Priority 0: validate every request-level assumption before cloning the
    // scan, constructing Taj, or performing floating-point safety arithmetic.
    if let Err(err) = validate_perception_request(req) {
        state.reset();
        // A rejected request still tells the watchdog something — but only when
        // the SENSOR is what was wrong. An empty scan, a NaN ray or impossible
        // range bounds are sensor faults and must land on the #1211 reason codes
        // (and must reset any recovery streak). A bad `margin_m` or
        // `confidence_floor` is the CALLER's configuration error: faulting the
        // sensor for it would blame the wrong component and, worse, would let a
        // misconfigured client hold a healthy sensor in recovery indefinitely.
        let health = if is_scan_fault(err) && !req.diagnostic_probe {
            observe_scan_fault(state, req, now_ms)
        } else {
            SensorHealth::Disarmed
        };
        return invalid_perception_response(req.camera_armed, health);
    }

    // Observe BEFORE the duplicate short-circuit below — a driver republishing one
    // frame forever produces exactly the duplicate-stamp case, so a tick placed
    // after the short-circuit would never see the freeze it exists to catch.
    //
    // But a repeated stamp is NOT automatically a frozen sensor here. This service
    // has several legitimate consumers (`perception_governor` and `occy_doer` both
    // post the same live scan; that is why the short-circuit below exists at all),
    // so the same frame arriving twice usually means two nodes asked about it.
    // Counting the second copy as a fresh observation would let a perfectly healthy
    // two-consumer deployment trip the frozen-stamp check and ground the robot.
    //
    // So a re-post is observed as an ARRIVAL THAT CARRIES NO NEW FRAME. A frozen
    // driver still fails closed, because it produces no NEW frames either: the
    // watchdog's silence budget expires and the fault surfaces as
    // `SENSOR_NO_MESSAGES` — "nothing new arrived", which is exactly the truth
    // about a driver replaying one buffer. Detection is preserved; the false
    // positive is not.
    let is_repost = state.last_stamp_ms == Some(req.stamp_ms);
    let health = if req.diagnostic_probe {
        SensorHealth::Disarmed
    } else if is_repost {
        state.watchdog.observe(SensorTick {
            now_ms,
            publisher_count: req.publisher_count.unwrap_or(1),
            frame: None,
        })
    } else {
        observe_scan(state, req, now_ms)
    };

    // Multiple consumers may submit the exact same LaserScan. Replaying a
    // duplicate must not advance tracking, increment frames_seen, or overwrite
    // velocity with a zero-dt estimate.
    if state.last_stamp_ms == Some(req.stamp_ms) {
        if let Some(response) = state.last_response.as_ref() {
            // Serve the cached perception, but re-apply THIS tick's liveness
            // verdict: the cached response was computed when the sensor may still
            // have looked healthy, and a stale cap is exactly what a frozen driver
            // would be served otherwise.
            let mut response = response.clone();
            response.sensor_measured_hz = state.watchdog.measured_hz();
            apply_sensor_health(&mut response, health, req.publisher_count.is_some());
            if state.stabilize {
                apply_cap_without_observing(&mut response, &state.stabilizer);
            }
            return response;
        }

        // Defensive fallback: duplicate metadata without a cached response is
        // inconsistent state, so reset rather than process ambiguously.
        state.reset();
    }

    let scan = LaserScan {
        angle_min_rad: req.angle_min_rad,
        angle_increment_rad: req.angle_increment_rad,
        range_min_m: req.range_min_m,
        range_max_m: req.range_max_m,
        ranges: req.ranges.clone(),
        stamp_ms: req.stamp_ms,
    };
    // Process at the scan's own stamp. Wall-clock staleness remains the
    // downstream consumer's responsibility, while this stateful service tracks
    // object identity and velocity between consecutive scans.
    let tracker_config = TajConfig {
        forward_extent_m: req.forward_extent_m,
        ..Default::default()
    };

    let timestamp_fault = state.last_stamp_ms.is_some_and(|last_stamp_ms| {
        req.stamp_ms < last_stamp_ms
            || req.stamp_ms.saturating_sub(last_stamp_ms) > MAX_TRACKING_GAP_MS
    });

    let configuration_changed = state.tracker_config.is_some_and(|previous| {
        previous.forward_extent_m.to_bits() != tracker_config.forward_extent_m.to_bits()
            || previous.track_assoc_gate_m.to_bits() != tracker_config.track_assoc_gate_m.to_bits()
            || previous.cluster_gap_m.to_bits() != tracker_config.cluster_gap_m.to_bits()
            || previous.min_cluster_points != tracker_config.min_cluster_points
    });

    if timestamp_fault || configuration_changed {
        state.reset();
    }

    if state.tracker.is_none() {
        state.tracker = Some(TajTracker::new(tracker_config));
        state.tracker_config = Some(tracker_config);
    }

    let perception = state
        .tracker
        .as_mut()
        .expect("tracker initialized above")
        .track(&scan, req.stamp_ms);

    // Supporting-only camera/depth association. The lidar tracker remains
    // authoritative: camera observations cannot create tracks, alter track
    // state, or change pedestrian classifier membership.
    let camera_observations: Vec<CameraVruObservation> = req
        .camera_observations
        .iter()
        .map(|observation| CameraVruObservation {
            observation_id: observation.observation_id,
            pos: kirra_core::corridor::Point {
                x_m: observation.x,
                y_m: observation.y,
            },
            confidence: observation.confidence,
            stamp_ms: req.camera_stamp_ms.unwrap_or(req.stamp_ms),
        })
        .collect();

    let camera_associations = state
        .tracker
        .as_ref()
        .expect("tracker initialized above")
        .associate_camera_vrus(
            &camera_observations,
            req.stamp_ms,
            &CameraVruFusionConfig::default(),
        );

    // Canonical fused classification. Camera evidence may tighten semantic
    // membership, but it cannot create a lidar track, change its geometry, or
    // remove the ordinary object-RSS channel.
    let fused_vrus = state
        .tracker
        .as_ref()
        .expect("tracker initialized above")
        .classify_vrus_fused(
            req.stamp_ms,
            &VruClassifierConfig::default(),
            &camera_associations,
        );

    // ---- Phase B: camera fusion (TIGHTEN-ONLY) -----------------------------
    // The camera may only SHORTEN the drivable corridor Phase A produced; it can
    // never extend it (`clip_corridor_to_hazards` truncates the boundaries, and
    // the KPI gate's `ForbiddenLoosen` is a hard zero). Lidar stays the sole
    // authority on free space.
    let channel = resolve_camera_channel(
        req.camera_armed,
        req.camera_stamp_ms,
        req.stamp_ms,
        req.camera_max_age_ms,
        &req.detections,
    );
    let camera_healthy = channel != CameraChannel::Faulted;
    let (corridor, camera_clip_x_m) = match channel {
        // Disarmed → untouched Phase-A corridor (byte-identical prior behaviour).
        // Faulted → also untouched, but `camera_healthy=false` forces the MRC
        // floor below, so a blind armed camera stops the robot rather than
        // letting it drive on lidar alone as if nothing were wrong.
        CameraChannel::Disarmed | CameraChannel::Faulted => (perception.corridor.clone(), None),
        CameraChannel::Fresh => {
            let dets: Vec<SemanticDetection> = req
                .detections
                .iter()
                .map(CameraDetection::to_semantic)
                .collect();
            let clip = hazard_clip_x(&perception.corridor, &dets);
            (clip_corridor_to_hazards(&perception.corridor, &dets), clip)
        }
    };

    let confidence = corridor.confidence();
    let age_ms = corridor.age_ms();
    // The platform must physically fit inside every observed corridor station.
    // Configuration validation already proves these inputs are finite and
    // internally consistent.
    let required_corridor_width_m = req.vehicle_width_m + 2.0 * req.lateral_clearance_m;
    let corridor_reach_m = corridor_reach(&corridor);
    let minimum_observed_corridor_width_m =
        minimum_corridor_width_before_reach_m(&corridor, corridor_reach_m);

    // Fail closed when confidence is insufficient, the armed camera is faulted,
    // corridor geometry is malformed, or observed free space is too narrow.
    let corridor_width_healthy = minimum_observed_corridor_width_m
        .is_some_and(|width_m| width_m >= required_corridor_width_m);

    let healthy = confidence >= req.confidence_floor && camera_healthy && corridor_width_healthy;

    // The nearest IN-LANE object (|y| within half a lane), as a discrete
    // clear-distance bound that complements the corridor reach.
    // Measured to each object's NEAREST FORWARD EDGE, not its centroid: an
    // obstacle is not at its middle, and the centroid reading reported it as up
    // to half its own extent further away than it is. An absent entry (an
    // object Taj did not cluster this frame) falls back to the centroid —
    // today's behaviour, never worse. `min` with the centroid means a
    // pathological edge BEHIND the centroid can only tighten, never relax.
    //
    // Eligibility still uses the centroid, so WHICH objects count as forward
    // and in-lane is unchanged; only the DISTANCE to a counted object tightens.
    let near_edge_of: std::collections::BTreeMap<u64, f64> =
        perception.near_edge_x_m.iter().copied().collect();
    // #1212: track WHICH object is nearest, not just how near. The identity is what
    // lets a cap transition be logged as "held at 0.8 by object 41" instead of an
    // unattributable number, and it is what the stabilizer's limiting-object
    // persistence keys on across a detection dropout.
    let nearest = nearest_candidate(
        perception
            .objects
            .iter()
            .filter(|o| {
                is_forward_object_candidate(
                    o.pos.x_m,
                    o.pos.y_m,
                    req.forward_extent_m,
                    req.lane_half_m,
                )
            })
            .map(|o| {
                let d = near_edge_of
                    .get(&o.id)
                    .copied()
                    .filter(|edge: &f64| edge.is_finite())
                    .map_or(o.pos.x_m, |edge: f64| edge.min(o.pos.x_m));
                (o.id, d)
            }),
    );
    // One Option, destructured — the identity and the distance are the same fact and
    // must not be able to disagree. A `Some` id beside a `None` distance would name an
    // object bounding a cap that reports nothing bounding it.
    let nearest_object_id = nearest.map(|(id, _)| id);
    let nearest_object_m = nearest.map(|(_, d)| d);

    // Clear distance = the tighter of the corridor reach and the nearest
    // in-lane object.
    // Reach is measured on the CLIPPED corridor, so a camera hazard tightens the
    // clear distance and therefore the ACD speed cap.
    let clear = corridor_reach_m
        .min(nearest_object_m.unwrap_or(f64::INFINITY))
        .max(0.0);

    // ACD cap: the speed from which a `decel_mps2` brake still stops within
    // (clear - margin). Unhealthy perception → 0.0 (MRC floor): never trust
    // an empty/low-confidence corridor.
    let speed_cap_mps = if healthy {
        (2.0 * req.decel_mps2 * (clear - req.margin_m).max(0.0)).sqrt()
    } else {
        0.0
    };

    let to_poly = |pts: &[kirra_core::corridor::Point]| -> Vec<[f64; 2]> {
        pts.iter().map(|p| [p.x_m, p.y_m]).collect()
    };
    // Which emitted objects the sensor did not actually see this frame. Set
    // membership rather than a field on `PerceivedObject`: coasting is a Taj
    // concept, and that type is the WCET-critical one the checker consumes,
    // with ~138 construction sites across this workspace and parko that have
    // no opinion about it. The marker becomes structural HERE, at the wire.
    let coasted_ids: std::collections::BTreeSet<u64> =
        perception.coasted_ids.iter().copied().collect();

    let left = to_poly(corridor.left_boundary());
    let right = to_poly(corridor.right_boundary());
    let objects = perception
        .objects
        .iter()
        .map(|o| ObjOut {
            id: o.id,
            x: o.pos.x_m,
            y: o.pos.y_m,
            vx: o.vel.x_m,
            vy: o.vel.y_m,
            coasted: coasted_ids.contains(&o.id),
        })
        .collect();

    let camera_associations: Vec<CameraAssociationOut> = camera_associations
        .iter()
        .map(|association| CameraAssociationOut {
            track_id: association.track_id,
            observation_id: association.observation_id,
            distance_m: association.distance_m,
            confidence: association.confidence,
        })
        .collect();

    let pedestrians = fused_vrus
        .iter()
        .filter(|result| result.fused_classification.feeds_pedestrian_rss())
        .map(|result| PedestrianOut {
            id: result.pedestrian.id,
            x: result.pedestrian.pos.x_m,
            y: result.pedestrian.pos.y_m,
            vx: result.pedestrian.vel.x_m,
            vy: result.pedestrian.vel.y_m,
            age_s: result.pedestrian.age_s,
            lidar_classification: result.lidar_classification.as_str(),
            fused_classification: result.fused_classification.as_str(),
            fusion_reason: result.fusion_reason.as_str(),
            camera_observation_id: result.camera_observation_id,
            camera_confidence: result.camera_confidence,
            association_distance_m: result.association_distance_m,
            coasted: coasted_ids.contains(&result.pedestrian.id),
        })
        .collect();

    // Produce predictions only for tracks whose final fused classification
    // feeds pedestrian RSS. Camera-only evidence still cannot create a track.
    let pedestrian_track_ids: std::collections::BTreeSet<u64> = fused_vrus
        .iter()
        .filter(|result| result.fused_classification.feeds_pedestrian_rss())
        .map(|result| result.pedestrian.id)
        .collect();

    let predicted_vrus: Vec<PredictedVruMotionOut> = state
        .tracker
        .as_ref()
        .expect("tracker initialized above")
        .predict_vru_motion(req.stamp_ms, &VruMotionPredictionConfig::default())
        .iter()
        .filter(|prediction| pedestrian_track_ids.contains(&prediction.track_id))
        .map(predicted_vru_to_wire)
        .collect();

    state.scan_sequence = state.scan_sequence.saturating_add(1);

    let camera_sequence = if req.camera_armed {
        req.camera_stamp_ms.map(|camera_stamp_ms| {
            if state.last_camera_stamp_ms != Some(camera_stamp_ms) {
                state.camera_sequence = state.camera_sequence.saturating_add(1);
                state.last_camera_stamp_ms = Some(camera_stamp_ms);
            }
            state.camera_sequence
        })
    } else {
        None
    };

    let mut response = PerceptionResponse {
        frame_id: PerceptionFrameId::pending(
            state.scan_sequence,
            camera_sequence,
            state.tracker_generation.get(),
        ),
        healthy,
        confidence,
        age_ms,
        clear_distance_m: clear,
        nearest_object_m,
        object_count: perception.objects.len(),
        minimum_corridor_width_m: minimum_observed_corridor_width_m,
        required_corridor_width_m,
        speed_cap_mps,
        left,
        right,
        objects,
        camera_associations,
        pedestrians,
        predicted_vrus,
        camera_healthy,
        camera_clip_x_m,
        sensor_liveness: "disarmed",
        sensor_fault: None,
        sensor_measured_hz: None,
        publisher_count_observed: false,
        raw_speed_cap_mps: speed_cap_mps,
        stabilized_speed_cap_mps: speed_cap_mps,
        cap_reason: None,
        cap_clear_streak: 0,
        cap_limiting_object: None,
        rejected: perception.rejected.into(),
    };

    response.frame_id.profile_digest = compute_perception_profile_digest(req);
    // The evidence digest is computed BEFORE the liveness verdict is stamped, and
    // the cached response is stored before it too. Both are deliberate.
    //
    // The digest identifies the perception EVIDENCE — what the sensor showed and
    // what Taj made of it. Liveness is a verdict about whether that evidence is
    // CURRENT, which is orthogonal: the same scan bytes produce the same evidence
    // whether they arrived first or third in a frozen run. Folding it in would make
    // the digest of a scan depend on the history of unrelated scans, and would
    // change the identity of a frame that a downstream consumer binds commands to.
    // This is the same line #1210 drew for the rejection tally.
    //
    // Caching pre-verdict matters for the duplicate path: a stale `Some(0.0)` baked
    // into the cache would compose with `min` forever, so a transient fault would
    // outlive itself. The cache holds the perception result; the verdict is applied
    // fresh on every serve, including every replay.
    response.frame_id.evidence_digest = compute_perception_evidence_digest(&response);

    state.last_stamp_ms = Some(req.stamp_ms);
    state.last_response = Some(response.clone());

    response.sensor_measured_hz = state.watchdog.measured_hz();
    apply_sensor_health(&mut response, health, req.publisher_count.is_some());

    // Stabilize LAST, over the liveness-adjusted cap.
    //
    // The ordering is the point. `apply_sensor_health` floors the cap to zero on a
    // sensor fault, and feeding that zero through the stabilizer as the raw
    // observation makes a liveness fault behave exactly like a hazard: restrict
    // instantly, then re-earn the full streak on recovery. That is what the Python
    // did with `effective_raw_cap = 0.0 if not healthy`, and it is the composition
    // that keeps a blinking sensor from ratcheting the robot back up to speed
    // between faults.
    //
    // The limiting object is reported only when an OBJECT bound the clear distance.
    // When the corridor geometry is the tighter constraint there is no object to
    // name, and inventing one would make the persistence logic hold a cap against a
    // hazard that never existed.
    let limiting =
        nearest_object_id.filter(|_| nearest_object_m.is_some_and(|d| d <= corridor_reach_m));
    if state.stabilize {
        apply_cap_stabilization(&mut response, &mut state.stabilizer, limiting, now_ms);
    }
    response
}

/// Run the stabilizer over a response's already-liveness-adjusted cap and stamp the
/// result. `speed_cap_mps` becomes the stabilized value — the enforced number — with
/// the pre-stabilization value preserved on `raw_speed_cap_mps`.
fn apply_cap_stabilization(
    response: &mut PerceptionResponse,
    stabilizer: &mut CapStabilizer,
    limiting_object: Option<u64>,
    now_ms: u64,
) {
    // The stabilizer observes the LIVENESS-ADJUSTED cap (zero on a sensor fault),
    // which is what makes a fault behave like a hazard.
    //
    // Two numbers reach the wire: `raw_speed_cap_mps` — perception's own proposal,
    // set when the response was built and NOT overwritten here — and the enforced
    // stabilized cap. The liveness-adjusted value in between is not carried as a
    // third field; it is recoverable from `sensor_fault` / `sensor_liveness`, which
    // say whether liveness floored the cap and why. Carrying perception's proposal
    // is the part that matters: without it a consumer cannot tell a real hazard from
    // a filter still climbing, which is precisely how the Python version's behaviour
    // became invisible.
    let observed = response.speed_cap_mps;
    let decision = stabilizer.observe(CapObservation {
        raw_cap_mps: observed,
        limiting_object,
        now_ms,
    });
    response.stabilized_speed_cap_mps = decision.stabilized_cap_mps;
    response.speed_cap_mps = decision.stabilized_cap_mps;
    response.cap_reason = Some(decision.reason.code());
    response.cap_clear_streak = decision.clear_streak;
    response.cap_limiting_object = decision.limiting_object;
    // A cap held below what was observed means the envelope is not yet the one
    // perception would allow, so the frame is not "healthy" in the sense a consumer
    // enforces on.
    if decision.stabilized_cap_mps < observed {
        response.healthy = false;
    }
}

/// Serve a response WITHOUT advancing the stabilizer, stamping the cap it is
/// currently holding.
///
/// Used for re-posts and diagnostic probes. Same reasoning as the watchdog: a frame
/// the stabilizer has already accounted for is not new evidence, and letting a
/// second consumer's copy advance the clock would inflate the measured frame rate
/// the rise limiter divides by — a two-consumer deployment would climb twice as
/// fast as a one-consumer deployment on identical perception.
fn apply_cap_without_observing(response: &mut PerceptionResponse, stabilizer: &CapStabilizer) {
    let observed = response.speed_cap_mps;
    let held = stabilizer.stabilized_cap_mps().min(observed);
    response.stabilized_speed_cap_mps = held;
    response.speed_cap_mps = held;
    // A replay is still a stabilizer-authored number, so it is stamped as one — with
    // its own code, because "the stabilizer evaluated this frame and passed it" and
    // "the stabilizer never saw this frame and is holding" are different facts and a
    // consumer deciding whether to enforce needs to tell them apart.
    //
    // It is NOT left as `None`. A consumer that fail-closes on a missing reason (the
    // ROS relay does) would floor a duplicate to zero, and a two-node deployment
    // where the second node re-posts would then stutter between the held cap and
    // zero on identical perception. Absent-reason must keep meaning "this sidecar
    // does not stabilize", which is the case the relay exists to refuse.
    response.cap_reason = Some(CAP_REPLAY_HELD);
    response.cap_clear_streak = stabilizer.clear_streak();
    // The identity travels with the number. Reporting the held cap while clearing the
    // limiting object would describe a cap held against nothing.
    response.cap_limiting_object = stabilizer.limiting_object();
    if held < observed {
        response.healthy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request over `ranges`, with the camera channel DISARMED by default —
    /// so every pre-existing assertion still describes the lidar-only path.
    pub(super) fn req(ranges: Vec<f32>) -> PerceptionRequest {
        PerceptionRequest {
            angle_min_rad: -1.5,
            angle_increment_rad: 0.01,
            range_min_m: 0.1,
            range_max_m: 12.0,
            ranges,
            stamp_ms: 1_000,
            forward_extent_m: default_extent(),
            decel_mps2: default_decel(),
            margin_m: default_margin(),
            lane_half_m: default_lane_half(),
            vehicle_width_m: default_vehicle_width(),
            lateral_clearance_m: default_lateral_clearance(),
            confidence_floor: default_floor(),
            camera_armed: false,
            detections: Vec::new(),
            camera_observations: Vec::new(),
            camera_stamp_ms: None,
            camera_max_age_ms: DEFAULT_CAMERA_MAX_AGE_MS,
            publisher_count: None,
            diagnostic_probe: false,
        }
    }

    // -----------------------------------------------------------------------
    // Coasted-object marker
    //
    // A coasted track is a hazard the sensor did NOT report this frame. It is
    // emitted so the hazard survives a dropout, and the checker treats it as
    // real — the conservative reading. What must never happen is an audit being
    // unable to tell it apart from an observation, so the marker rides the wire
    // AND the evidence digest.
    // -----------------------------------------------------------------------

    /// An obstacle straight ahead at `bx` metres.
    fn blob_ranges(bx: f32) -> Vec<f32> {
        (0..300)
            .map(|i| {
                let theta = -1.5 + (i as f64) * 0.01;
                if theta.abs() < 0.15 {
                    (f64::from(bx) / theta.cos()) as f32
                } else {
                    f32::INFINITY
                }
            })
            .collect()
    }

    fn at(ranges: Vec<f32>, stamp_ms: u64) -> PerceptionRequest {
        let mut r = req(ranges);
        r.stamp_ms = stamp_ms;
        r
    }

    #[test]
    fn an_observed_object_is_never_marked_coasted() {
        let mut state = TrackedPerceptionState::default();
        let response = handle_perception_tracked(&at(blob_ranges(4.0), 1_000), &mut state);
        assert!(
            !response.objects.is_empty(),
            "fixture must detect something"
        );
        assert!(
            response.objects.iter().all(|o| !o.coasted),
            "a detected object must never carry the coasted marker"
        );
    }

    #[test]
    fn a_coasted_object_reaches_the_wire_marked() {
        let mut state = TrackedPerceptionState::default();
        let seen = handle_perception_tracked(&at(blob_ranges(4.0), 1_000), &mut state);
        let id = seen.objects[0].id;

        // The detector reports nothing; the hazard is retained.
        let gap = handle_perception_tracked(&at(vec![f32::INFINITY; 300], 1_100), &mut state);
        let coasted: Vec<&ObjOut> = gap.objects.iter().filter(|o| o.id == id).collect();
        assert_eq!(coasted.len(), 1, "the hazard must survive the dropout");
        assert!(
            coasted[0].coasted,
            "an object the sensor did not see must be marked on the wire"
        );
    }

    // -----------------------------------------------------------------------
    // Rejection accounting (#1210)
    // -----------------------------------------------------------------------

    #[test]
    fn the_rejection_tally_reaches_the_wire() {
        // An all-infinity scan is the ROS no-return convention over every ray.
        // It must land in `beyond_max_range` and nowhere else — an empty room
        // is not a broken lidar.
        let response = handle_perception(&req(vec![f32::INFINITY; 300]));
        assert_eq!(response.rejected.beyond_max_range, 300);
        assert_eq!(response.rejected.not_a_number, 0);
        assert_eq!(response.rejected.self_masked, 0);
    }

    #[test]
    fn a_nan_ray_refuses_the_whole_frame_rather_than_being_counted() {
        // At THIS boundary a NaN never reaches the tally, because
        // `validate_perception_request` refuses the entire request first. That
        // is the stronger behaviour and worth pinning: an undecodable
        // measurement is not something to count and carry on past.
        //
        // The per-ray `not_a_number` counter still earns its place — callers
        // that drive `TajPhaseA` directly (the ROS adapter, the tracker) have
        // no such request gate in front of them.
        let mut ranges = vec![f32::INFINITY; 300];
        ranges[0] = f32::NAN;
        let response = handle_perception(&req(ranges));
        assert!(!response.healthy, "a NaN range must fail the frame closed");
        assert_eq!(response.speed_cap_mps, 0.0);
        assert_eq!(
            response.rejected.not_a_number, 0,
            "the frame never reached ingest, so nothing was counted"
        );
    }

    #[test]
    fn the_tally_stays_out_of_the_evidence_digest() {
        // The claim in `PerceptionResponse::rejected`'s doc comment, proven
        // rather than asserted.
        //
        // Both scans admit exactly the same points — no ray produces one — so
        // the geometry, the confidence and therefore the evidence are
        // identical. They differ only in WHY each ray produced nothing: one
        // scan is all no-return, the other reads short of the minimum range.
        // Evidence identity must track what was perceived, not the health
        // counters alongside it.
        let below_min = vec![0.05_f32; 300]; // req() sets range_min_m = 0.1
        let clean_response = handle_perception(&req(vec![f32::INFINITY; 300]));
        let short_response = handle_perception(&req(below_min));

        assert_eq!(
            clean_response.rejected.beyond_max_range, 300,
            "fixture: every ray is a no-return"
        );
        assert_eq!(
            short_response.rejected.below_min_range, 300,
            "fixture: every ray reads short"
        );
        assert_ne!(
            clean_response.rejected.beyond_max_range, short_response.rejected.beyond_max_range,
            "the two tallies must actually differ, else this proves nothing"
        );
        assert_eq!(
            compute_perception_evidence_digest(&clean_response),
            compute_perception_evidence_digest(&short_response),
            "identical evidence must keep one identity regardless of the tally"
        );
    }

    #[test]
    fn the_marker_changes_the_evidence_digest() {
        // The load-bearing property: observed and coasted are DIFFERENT
        // evidence, so two frames identical but for what was actually seen must
        // not share an evidence identity. If the marker were presentation-only
        // this assertion fails and the audit trail is forgeable by omission.
        let mut response = invalid_perception_response(false, SensorHealth::Disarmed);
        response.objects = vec![ObjOut {
            id: 7,
            x: 4.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            coasted: false,
        }];
        let observed = compute_perception_evidence_digest(&response);

        response.objects[0].coasted = true;
        let coasted = compute_perception_evidence_digest(&response);

        assert_ne!(
            observed, coasted,
            "the coasted marker must be part of the evidence identity"
        );
    }

    // -----------------------------------------------------------------------
    // Nearest-forward-edge clear distance
    //
    // Clear distance used to be measured to an object's CENTROID, reporting an
    // obstacle as up to half its own extent further away than it is. The
    // clustering already computes each cluster's bounding box and discarded it.
    // -----------------------------------------------------------------------

    /// A wide obstacle at range `bx`, spanning enough rays to have real
    /// x-EXTENT — the whole point of the test.
    ///
    /// Constant RANGE, deliberately: it traces an arc about the lidar, so
    /// `x = bx·cos θ` varies across the cluster and its nearest edge sits
    /// genuinely nearer than its centroid. The `bx / cos θ` form used elsewhere
    /// in this file describes a flat wall — every point at `x == bx` exactly —
    /// which has NO near edge and would make these assertions vacuous.
    fn wide_blob_ranges(bx: f32) -> Vec<f32> {
        (0..300)
            .map(|i| {
                let theta = -1.5 + (i as f64) * 0.01;
                if theta.abs() < 0.35 {
                    bx
                } else {
                    f32::INFINITY
                }
            })
            .collect()
    }

    #[test]
    fn clear_distance_is_measured_to_the_near_edge_not_the_centroid() {
        let mut state = TrackedPerceptionState::default();
        let response = handle_perception_tracked(&at(wide_blob_ranges(4.0), 1_000), &mut state);

        let object = response
            .objects
            .first()
            .expect("fixture must produce an obstacle");
        let clear = response.clear_distance_m;

        // A real gap, not floating-point noise: the arc spans |theta| < 0.35 at
        // range 4, so the near edge sits ~4(1 - cos 0.35) ~ 0.24 m nearer than
        // the centroid. Requiring a centimetre-scale gap is what stops a
        // zero-extent fixture passing this vacuously.
        assert!(
            clear < object.x - 0.05,
            "clear distance {clear} must be materially nearer than the centroid \
             {} — an obstacle is not at its middle",
            object.x
        );
    }

    #[test]
    fn the_near_edge_never_reports_an_object_further_than_its_centroid() {
        // Direction guard. Whatever the geometry, this change may only TIGHTEN:
        // a bound that could relax would be a safety regression, not a fix.
        let mut state = TrackedPerceptionState::default();
        for bx in [2.0_f32, 4.0, 6.0, 8.0] {
            let response = handle_perception_tracked(&at(wide_blob_ranges(bx), 1_000), &mut state);
            if let Some(object) = response.objects.first() {
                let clear = response.clear_distance_m;
                assert!(
                    clear <= object.x + 1e-9,
                    "at bx={bx} clear distance {clear} exceeded the centroid {}",
                    object.x
                );
            }
        }
    }

    /// A clear-ahead scan: returns far out on every ray, so Phase A yields a
    /// healthy corridor with real forward reach for the camera to clip.
    fn clear_scan() -> Vec<f32> {
        // A wide, traversable corridor bounded by parallel walls at y = ±2 m.
        //
        // Returning the same range on every ray would describe a circular wall
        // around the lidar, not clear road. That synthetic geometry can narrow
        // the extracted corridor and incorrectly fail the physical-width gate.
        let angle_min_rad = -1.5_f64;
        let angle_increment_rad = 0.01_f64;
        let wall_half_width_m = 2.0_f64;
        let range_max_m = 12.0_f64;

        (0..300)
            .map(|index| {
                let angle_rad = angle_min_rad + index as f64 * angle_increment_rad;
                let sin_angle = angle_rad.sin().abs();

                if sin_angle < 1e-6 {
                    // Looking parallel to the walls: no return.
                    f32::INFINITY
                } else {
                    let range_m = wall_half_width_m / sin_angle;
                    if range_m <= range_max_m {
                        range_m as f32
                    } else {
                        f32::INFINITY
                    }
                }
            })
            .collect()
    }

    fn det(class: &str, near_x: f64, lat_min: f64, lat_max: f64) -> CameraDetection {
        CameraDetection {
            class: class.into(),
            near_x_m: near_x,
            lateral_min_m: lat_min,
            lateral_max_m: lat_max,
        }
    }

    #[test]
    fn tracked_response_emits_conservative_pedestrian_channel() {
        let mut state = TrackedPerceptionState::new();
        let request = req((0..301)
            .map(|index| {
                let angle = -1.5 + index as f64 * 0.01;
                if angle.abs() < 0.035 {
                    3.0
                } else {
                    f32::INFINITY
                }
            })
            .collect());

        let response = handle_perception_tracked(&request, &mut state);

        assert_eq!(response.objects.len(), 1);
        assert_eq!(
            response.pedestrians.len(),
            1,
            "a first-sighting pedestrian-sized cluster must feed the VRU channel"
        );
        assert_eq!(
            response.pedestrians[0].id, response.objects[0].id,
            "the additive object and VRU channels must preserve the same track ID"
        );
        assert_eq!(response.pedestrians[0].age_s, 0.0);
        assert_eq!(
            response.pedestrians[0].lidar_classification,
            "possible_pedestrian"
        );
        assert_eq!(
            response.pedestrians[0].fused_classification,
            "possible_pedestrian"
        );
        assert_eq!(
            response.pedestrians[0].fusion_reason,
            "lidar_possible_pedestrian"
        );
        assert_eq!(response.pedestrians[0].camera_observation_id, None);
    }

    #[test]
    fn tracked_response_emits_prediction_for_fused_vru() {
        let mut state = TrackedPerceptionState::new();
        let request = req((0..301)
            .map(|index| {
                let angle = -1.5 + index as f64 * 0.01;
                if angle.abs() < 0.035 {
                    3.0
                } else {
                    f32::INFINITY
                }
            })
            .collect());

        let response = handle_perception_tracked(&request, &mut state);

        assert_eq!(response.pedestrians.len(), 1);
        assert_eq!(response.predicted_vrus.len(), 1);
        assert_eq!(
            response.predicted_vrus[0].track_id,
            response.pedestrians[0].id
        );
        assert!(
            !response.predicted_vrus[0].points.is_empty(),
            "a fused VRU must carry bounded prediction evidence"
        );
        assert_eq!(
            response.predicted_vrus[0].intent_probabilities.len(),
            kirra_taj::VRU_INTENT_HYPOTHESIS_COUNT,
            "the sidecar must preserve Taj's complete intent distribution"
        );

        let probability_sum: f64 = response.predicted_vrus[0]
            .intent_probabilities
            .iter()
            .map(|hypothesis| hypothesis.probability)
            .sum();

        assert!(
            (probability_sum - 1.0).abs() <= 1.0e-6,
            "the emitted intent distribution must remain normalized"
        );
        assert!(
            response.predicted_vrus[0].points.len() <= 16,
            "the default prediction contract must remain hard bounded"
        );
    }

    #[test]
    fn invalid_perception_response_has_no_vru_predictions() {
        let response = invalid_perception_response(true, SensorHealth::Disarmed);

        assert!(response.predicted_vrus.is_empty());
        assert!(!response.healthy);
        assert_eq!(response.speed_cap_mps, 0.0);
    }

    #[test]
    fn nearly_lateral_return_is_not_a_forward_collision_object() {
        assert!(
            !is_forward_object_candidate(0.03, -0.60, 8.0, 0.60),
            "a nearly lateral return must not become the frontal ACD object"
        );
        assert!(
            !is_forward_object_candidate(0.03, 0.60, 8.0, 0.60),
            "the rule must be symmetric across the vehicle centerline"
        );
    }

    #[test]
    fn real_forward_obstacle_remains_eligible() {
        assert!(
            is_forward_object_candidate(0.50, -0.20, 8.0, 0.25),
            "a real obstacle inside the R2 forward lane must remain visible"
        );
        assert!(
            is_forward_object_candidate(0.60, 0.0, 8.0, 0.25),
            "a directly forward obstacle must remain visible"
        );
    }

    #[test]
    fn rear_out_of_horizon_and_nonfinite_objects_are_excluded() {
        assert!(!is_forward_object_candidate(-0.20, 0.0, 8.0, 0.25));
        assert!(!is_forward_object_candidate(8.01, 0.0, 8.0, 0.25));
        assert!(!is_forward_object_candidate(f64::NAN, 0.0, 8.0, 0.25));
        assert!(!is_forward_object_candidate(1.0, f64::INFINITY, 8.0, 0.25));
    }

    #[test]
    fn invalid_forward_region_configuration_fails_closed() {
        assert!(!is_forward_object_candidate(1.0, 0.0, 0.0, 0.25));
        assert!(!is_forward_object_candidate(1.0, 0.0, 8.0, -0.1));
        assert!(!is_forward_object_candidate(1.0, 0.0, f64::NAN, 0.25));
    }

    #[test]
    fn malformed_request_configuration_fails_closed_before_processing() {
        let mut cases = Vec::new();

        let mut invalid = req(clear_scan());
        invalid.angle_min_rad = f64::NAN;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.angle_increment_rad = 0.0;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.range_min_m = 2.0;
        invalid.range_max_m = 1.0;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.forward_extent_m = 0.1;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.decel_mps2 = 0.0;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.margin_m = invalid.forward_extent_m;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.lane_half_m = 0.0;
        cases.push(invalid);

        let mut invalid = req(clear_scan());
        invalid.confidence_floor = 1.1;
        cases.push(invalid);

        for invalid in cases {
            let response = handle_perception(&invalid);
            assert!(!response.healthy);
            assert_eq!(response.speed_cap_mps, 0.0);
            assert_eq!(response.clear_distance_m, 0.0);
            assert!(response.left.is_empty());
            assert!(response.right.is_empty());
            assert!(response.objects.is_empty());
        }
    }

    /// The forward-extent bound, walked across its whole domain.
    ///
    /// The load-bearing rows are `1e300` and `just above the maximum`: both are
    /// FINITE, so the pre-existing `is_finite() || <= MIN` guard admitted them.
    /// A finite-but-enormous extent is not merely slow — `extract_corridor`
    /// saturates its float->usize station cast to `usize::MAX`, which panicked
    /// on `nstations + 1` in debug and became a ~1.8e19-iteration loop in
    /// release.
    #[test]
    fn forward_extent_bound_accepts_the_usable_range_and_refuses_the_rest() {
        use PerceptionRequestError::{ForwardExtentTooLarge, InvalidForwardExtent};

        let cases: [(f64, Option<PerceptionRequestError>); 11] = [
            // --- lower bound: unchanged pre-existing behaviour ---
            (f64::NAN, Some(InvalidForwardExtent)),
            (f64::INFINITY, Some(InvalidForwardExtent)),
            (f64::NEG_INFINITY, Some(InvalidForwardExtent)),
            (-1.0, Some(InvalidForwardExtent)),
            (0.0, Some(InvalidForwardExtent)),
            (MIN_FORWARD_OBJECT_X_M, Some(InvalidForwardExtent)), // boundary is exclusive
            // --- normal configured values (kirra_params.yaml, defaults, tests) ---
            (8.0, None),
            (default_extent(), None),
            // --- the new upper bound ---
            (MAX_FORWARD_EXTENT_M, None), // exact maximum is ACCEPTED
            (
                MAX_FORWARD_EXTENT_M + f64::EPSILON * MAX_FORWARD_EXTENT_M,
                Some(ForwardExtentTooLarge),
            ),
            (1e300, Some(ForwardExtentTooLarge)), // the reported crasher
        ];

        for (extent, expected) in cases {
            let mut request = req(clear_scan());
            request.forward_extent_m = extent;
            // `margin_m` must stay below the extent or it masks the case under
            // test with InvalidMargin.
            request.margin_m = 0.0;
            assert_eq!(
                validate_perception_request(&request).err(),
                expected,
                "forward_extent_m = {extent:e}"
            );
        }
    }

    /// Regression: a huge FINITE extent is refused at the perimeter, so the
    /// request never reaches Taj computation — and the endpoint answers with
    /// the fail-closed response rather than panicking or hanging.
    ///
    /// Exercised in both profiles: debug catches the arithmetic overflow that
    /// used to fire at `kirra_taj::lib.rs:445`, release catches the wrapped
    /// loop that used to hang instead.
    #[test]
    fn huge_finite_forward_extent_is_refused_before_taj_computation() {
        for extent in [1e300, 1e30, f64::MAX, MAX_FORWARD_EXTENT_M * 2.0] {
            let mut request = req(clear_scan());
            request.forward_extent_m = extent;
            request.margin_m = 0.0;

            assert_eq!(
                validate_perception_request(&request).err(),
                Some(PerceptionRequestError::ForwardExtentTooLarge),
                "forward_extent_m = {extent:e} must be refused"
            );

            // The full endpoint must return the fail-closed response — reaching
            // here at all proves no panic and no unbounded loop.
            let response = handle_perception(&request);
            assert!(!response.healthy, "{extent:e}");
            assert_eq!(response.speed_cap_mps, 0.0, "{extent:e}");
            assert!(response.left.is_empty() && response.right.is_empty());
            assert!(response.objects.is_empty());
        }
    }

    /// Every extent configured anywhere in the repository must remain valid, so
    /// the new ceiling cannot have broken a live deployment.
    #[test]
    fn all_shipped_forward_extents_are_within_the_bound() {
        // kirra_params.yaml (perception_governor + occy_doer), TajConfig's own
        // default, the sidecar default, and the largest test-stack value.
        for extent in [8.0, 8.0, 20.0, default_extent(), 40.0] {
            assert!(
                extent > MIN_FORWARD_OBJECT_X_M && extent <= MAX_FORWARD_EXTENT_M,
                "shipped extent {extent} is outside the accepted band"
            );
            let mut request = req(clear_scan());
            request.forward_extent_m = extent;
            assert!(validate_perception_request(&request).is_ok(), "{extent}");
        }
    }

    #[test]
    fn empty_oversized_and_nan_scans_fail_closed() {
        let empty = handle_perception(&req(Vec::new()));
        assert!(!empty.healthy);
        assert_eq!(empty.speed_cap_mps, 0.0);

        let oversized = handle_perception(&req(vec![1.0; MAX_LIDAR_RAYS + 1]));
        assert!(!oversized.healthy);
        assert_eq!(oversized.speed_cap_mps, 0.0);

        let mut nan_scan = clear_scan();
        nan_scan[10] = f32::NAN;
        let nan = handle_perception(&req(nan_scan));
        assert!(!nan.healthy);
        assert_eq!(nan.speed_cap_mps, 0.0);
    }

    #[test]
    fn infinite_no_return_samples_remain_valid_input() {
        let request = req(vec![f32::INFINITY; 300]);

        assert_eq!(validate_perception_request(&request), Ok(()));

        let response = handle_perception(&request);
        assert_eq!(
            response.speed_cap_mps, 0.0,
            "an all-no-return scan may be syntactically valid but must remain              perception-unhealthy"
        );
    }

    #[test]
    fn invalid_request_preserves_armed_camera_fail_closed_state() {
        let mut request = req(Vec::new());
        request.camera_armed = true;

        let response = handle_perception(&request);

        assert!(!response.healthy);
        assert!(!response.camera_healthy);
        assert_eq!(response.speed_cap_mps, 0.0);
    }

    #[test]
    fn corridor_wider_than_vehicle_and_clearance_remains_eligible() {
        let request = req(clear_scan());

        assert_eq!(
            validate_perception_request(&request),
            Ok(()),
            "default request must provide enough width for the configured platform"
        );

        let required_width = request.vehicle_width_m + 2.0 * request.lateral_clearance_m;

        assert!(
            2.0 * request.lane_half_m >= required_width,
            "configured corridor width must contain vehicle plus clearance"
        );
    }

    #[test]
    fn observed_corridor_narrower_than_r2_requirement_fails_closed() {
        let mut request = req(clear_scan());

        // Use legitimate side-wall geometry outside the frontal-centerline
        // exclusion band. The observed corridor is approximately 0.80 m wide.
        //
        // Configure a deliberately larger platform requirement so this test
        // isolates the observed-width fail-closed gate instead of depending on
        // near-centerline returns that corridor extraction intentionally rejects.
        request.vehicle_width_m = 0.70;
        request.lateral_clearance_m = 0.10;
        request.lane_half_m = 0.50;

        let wall_half_width_m = 0.40;
        request.ranges = request
            .ranges
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let theta = request.angle_min_rad + index as f64 * request.angle_increment_rad;
                let sin_theta = theta.sin().abs();

                if sin_theta <= 1e-6 {
                    f32::INFINITY
                } else {
                    let range_m = wall_half_width_m / sin_theta;
                    if range_m >= request.range_min_m && range_m <= request.range_max_m {
                        range_m as f32
                    } else {
                        f32::INFINITY
                    }
                }
            })
            .collect();

        let response = handle_perception(&request);

        let observed_width_m = response
            .minimum_corridor_width_m
            .expect("parallel walls must produce a measurable corridor width");

        assert!(
            observed_width_m < response.required_corridor_width_m,
            "test setup must produce an undersized observed corridor:              {observed_width_m} vs {}",
            response.required_corridor_width_m,
        );
        assert!(
            observed_width_m > 0.60 && observed_width_m < 1.00,
            "fixture should produce a realistic approximately 0.80 m corridor,              got {observed_width_m}",
        );
        assert!(!response.healthy);
        assert_eq!(response.speed_cap_mps, 0.0);
    }

    #[test]
    fn malformed_or_empty_corridor_width_fails_closed() {
        struct EmptyCorridor;

        impl CorridorSource for EmptyCorridor {
            fn left_boundary(&self) -> &[kirra_core::corridor::Point] {
                &[]
            }

            fn right_boundary(&self) -> &[kirra_core::corridor::Point] {
                &[]
            }

            fn confidence(&self) -> f32 {
                1.0
            }

            fn age_ms(&self) -> u64 {
                0
            }
        }

        assert_eq!(
            minimum_corridor_width_before_reach_m(&EmptyCorridor, 1.0),
            None
        );
    }

    #[test]
    fn deployed_r2_dimensions_are_internally_consistent() {
        let mut request = req(clear_scan());
        request.lane_half_m = 0.26;
        request.vehicle_width_m = 0.203;
        request.lateral_clearance_m = 0.15;

        assert_eq!(validate_perception_request(&request), Ok(()));

        let available_width_m = 2.0 * request.lane_half_m;
        let required_width_m = request.vehicle_width_m + 2.0 * request.lateral_clearance_m;

        assert!(
            available_width_m >= required_width_m,
            "R2 available width {available_width_m} m is below required width {required_width_m} m"
        );
    }

    #[test]
    fn corridor_narrower_than_vehicle_and_clearance_fails_closed() {
        let mut request = req(clear_scan());
        request.lane_half_m = 0.20;
        request.vehicle_width_m = 0.203;
        request.lateral_clearance_m = 0.15;

        assert_eq!(
            validate_perception_request(&request),
            Err(PerceptionRequestError::LaneNarrowerThanRequiredCorridor)
        );

        let response = handle_perception(&request);
        assert!(!response.healthy);
        assert_eq!(response.speed_cap_mps, 0.0);
    }

    #[test]
    fn contradictory_lane_and_platform_dimensions_fail_validation() {
        let mut request = req(clear_scan());
        request.lane_half_m = 0.25;
        request.vehicle_width_m = 0.40;
        request.lateral_clearance_m = 0.10;

        assert_eq!(
            validate_perception_request(&request),
            Err(PerceptionRequestError::LaneNarrowerThanRequiredCorridor)
        );
    }

    #[test]
    fn invalid_platform_dimensions_fail_closed() {
        let mut invalid_width = req(clear_scan());
        invalid_width.vehicle_width_m = 0.0;
        assert_eq!(
            validate_perception_request(&invalid_width),
            Err(PerceptionRequestError::InvalidVehicleWidth)
        );
        assert_eq!(handle_perception(&invalid_width).speed_cap_mps, 0.0);

        let mut invalid_clearance = req(clear_scan());
        invalid_clearance.lateral_clearance_m = -0.01;
        assert_eq!(
            validate_perception_request(&invalid_clearance),
            Err(PerceptionRequestError::InvalidLateralClearance)
        );
        assert_eq!(handle_perception(&invalid_clearance).speed_cap_mps, 0.0);
    }

    /// An empty / all-no-return scan reads as an unhealthy corridor → the
    /// MRC floor cap (fail-closed, unchanged from the example's contract).
    #[test]
    fn empty_scan_fails_closed_to_the_mrc_floor() {
        let resp = handle_perception(&req(vec![f32::INFINITY; 300]));
        if !resp.healthy {
            assert_eq!(resp.speed_cap_mps, 0.0, "unhealthy → MRC floor");
        }
    }

    // ---- camera channel arming (the three-way, fail-closed decision) --------

    #[test]
    fn camera_channel_resolution_is_three_way() {
        let none: Vec<CameraDetection> = vec![];
        // Disarmed regardless of anything else.
        assert_eq!(
            resolve_camera_channel(false, None, 1_000, 500, &none),
            CameraChannel::Disarmed
        );
        // Armed + fresh stamp → live.
        assert_eq!(
            resolve_camera_channel(true, Some(900), 1_000, 500, &none),
            CameraChannel::Fresh
        );
        // Armed + SILENT (no frame) → fail closed.
        assert_eq!(
            resolve_camera_channel(true, None, 1_000, 500, &none),
            CameraChannel::Faulted
        );
        // Armed + stale.
        assert_eq!(
            resolve_camera_channel(true, Some(100), 5_000, 500, &none),
            CameraChannel::Faulted
        );
        // Armed + implausibly FUTURE stamp (non-monotonic clock) → stale.
        assert_eq!(
            resolve_camera_channel(true, Some(90_000), 1_000, 500, &none),
            CameraChannel::Faulted
        );
        // Armed + fresh but non-finite geometry → fault, never silently skipped.
        let bad = vec![det("water", f64::NAN, -1.0, 1.0)];
        assert_eq!(
            resolve_camera_channel(true, Some(900), 1_000, 500, &bad),
            CameraChannel::Faulted
        );
    }

    /// DEFAULT (disarmed) is byte-identical to the lidar-only endpoint: no clip,
    /// camera reported healthy (nothing to be unhealthy about).
    #[test]
    fn disarmed_camera_leaves_the_corridor_untouched() {
        let resp = handle_perception(&req(clear_scan()));
        assert!(resp.camera_healthy);
        assert_eq!(resp.camera_clip_x_m, None);
    }

    /// ARMED but silent → the robot must STOP, not carry on using lidar alone as
    /// if nothing were wrong ("the detector did not look" is never "clear").
    #[test]
    fn armed_but_silent_camera_fails_closed_to_the_mrc_floor() {
        let mut r = req(clear_scan());
        r.camera_armed = true;
        r.camera_stamp_ms = None;
        let resp = handle_perception(&r);
        assert!(!resp.camera_healthy);
        assert!(!resp.healthy, "a blind armed camera must not read healthy");
        assert_eq!(resp.speed_cap_mps, 0.0, "armed+silent → MRC floor");
    }

    /// A fresh frame with NO hazards is a legitimate "I looked, it's clear" —
    /// distinct from silence, so it must NOT fail closed.
    #[test]
    fn armed_and_fresh_with_no_hazards_is_clear_not_faulted() {
        let mut r = req(clear_scan());
        r.camera_armed = true;
        r.camera_stamp_ms = Some(1_000);
        let resp = handle_perception(&r);
        assert!(resp.camera_healthy);
        assert_eq!(resp.camera_clip_x_m, None);
        assert!(
            resp.speed_cap_mps > 0.0,
            "a clear look must not stop the robot"
        );
    }

    // ---- TIGHTEN-ONLY: the load-bearing safety property --------------------

    /// A water hazard across the lane clips the corridor and therefore the ACD
    /// cap: the camera SHORTENS the clear distance.
    #[test]
    fn camera_hazard_tightens_clear_distance_and_speed_cap() {
        let baseline = handle_perception(&req(clear_scan()));
        let mut r = req(clear_scan());
        r.camera_armed = true;
        r.camera_stamp_ms = Some(1_000);
        r.detections = vec![det("water", 2.0, -2.0, 2.0)];
        let fused = handle_perception(&r);
        assert_eq!(fused.camera_clip_x_m, Some(2.0));
        assert!(
            fused.clear_distance_m < baseline.clear_distance_m,
            "camera hazard must SHORTEN clear distance ({} !< {})",
            fused.clear_distance_m,
            baseline.clear_distance_m
        );
        assert!(
            fused.speed_cap_mps < baseline.speed_cap_mps,
            "and therefore the cap"
        );
    }

    /// 🔴 The property the KPI gate calls `ForbiddenLoosen`: no camera input of any
    /// class, at any range, may make the corridor reach FARTHER than lidar-only.
    /// Swept over classes × ranges — the camera can only ever tighten.
    #[test]
    fn camera_can_never_extend_the_corridor() {
        let baseline = handle_perception(&req(clear_scan()));
        for class in ["road", "water", "static_obstacle", "unknown", "banana"] {
            for near_x in [0.5_f64, 1.0, 3.0, 7.0, 50.0] {
                let mut r = req(clear_scan());
                r.camera_armed = true;
                r.camera_stamp_ms = Some(1_000);
                r.detections = vec![det(class, near_x, -3.0, 3.0)];
                let fused = handle_perception(&r);
                assert!(
                    fused.clear_distance_m <= baseline.clear_distance_m + 1e-9,
                    "ForbiddenLoosen: class={class} near_x={near_x} extended reach \
                     ({} > {})",
                    fused.clear_distance_m,
                    baseline.clear_distance_m
                );
                assert!(fused.speed_cap_mps <= baseline.speed_cap_mps + 1e-9);
            }
        }
    }

    /// `road` is the only drivable class, so a road detection never clips — and an
    /// UNRECOGNIZED token must behave like `unknown` (non-drivable), not like road.
    #[test]
    fn unknown_class_tokens_fail_closed_to_non_drivable() {
        let mut road = req(clear_scan());
        road.camera_armed = true;
        road.camera_stamp_ms = Some(1_000);
        road.detections = vec![det("road", 2.0, -2.0, 2.0)];
        assert_eq!(
            handle_perception(&road).camera_clip_x_m,
            None,
            "road is drivable"
        );

        let mut garbage = req(clear_scan());
        garbage.camera_armed = true;
        garbage.camera_stamp_ms = Some(1_000);
        garbage.detections = vec![det("definitely-not-a-class", 2.0, -2.0, 2.0)];
        assert_eq!(
            handle_perception(&garbage).camera_clip_x_m,
            Some(2.0),
            "an unclassifiable region is a hazard, never free space"
        );
    }

    /// A hazard entirely outside the corridor's lateral span leaves it alone (no
    /// over-tightening on things that aren't in the way).
    #[test]
    fn hazard_beside_the_corridor_does_not_clip() {
        let mut r = req(clear_scan());
        r.camera_armed = true;
        r.camera_stamp_ms = Some(1_000);
        r.detections = vec![det("water", 2.0, 8.0, 9.0)];
        assert_eq!(handle_perception(&r).camera_clip_x_m, None);
    }
}

#[cfg(test)]
mod tracked_perception_tests {
    use super::*;

    fn tracked_request(range_m: f64, stamp_ms: u64) -> PerceptionRequest {
        let ray_count = 301usize;
        let angle_min_rad = -1.5;
        let angle_increment_rad = 0.01;

        let ranges = (0..ray_count)
            .map(|index| {
                let angle = angle_min_rad + index as f64 * angle_increment_rad;
                if angle.abs() < 0.035 {
                    range_m as f32
                } else {
                    f32::INFINITY
                }
            })
            .collect();

        PerceptionRequest {
            angle_min_rad,
            angle_increment_rad,
            range_min_m: 0.1,
            range_max_m: 12.0,
            ranges,
            stamp_ms,
            forward_extent_m: default_extent(),
            decel_mps2: default_decel(),
            margin_m: default_margin(),
            lane_half_m: default_lane_half(),
            vehicle_width_m: default_vehicle_width(),
            lateral_clearance_m: default_lateral_clearance(),
            confidence_floor: 0.0,
            camera_armed: false,
            camera_stamp_ms: None,
            camera_max_age_ms: DEFAULT_CAMERA_MAX_AGE_MS,
            publisher_count: None,
            diagnostic_probe: false,
            detections: Vec::new(),
            camera_observations: Vec::new(),
        }
    }

    #[test]
    fn tracked_handler_preserves_object_id_and_estimates_velocity() {
        let mut state = TrackedPerceptionState::new();

        let first = handle_perception_tracked(&tracked_request(5.0, 1_000), &mut state);
        let second = handle_perception_tracked(&tracked_request(5.1, 1_100), &mut state);

        assert_eq!(first.objects.len(), 1);
        assert_eq!(second.objects.len(), 1);
        assert_eq!(first.objects[0].id, second.objects[0].id);

        assert!(
            second.objects[0].vx > 0.5,
            "expected positive tracked velocity, got {}",
            second.objects[0].vx,
        );
        assert!(
            second.objects[0].vx < 1.5,
            "expected approximately 1 m/s, got {}",
            second.objects[0].vx,
        );
        assert!(second.objects[0].vy.abs() < 0.2);
    }

    #[test]
    fn timestamp_regression_resets_tracking_to_first_sighting() {
        let mut state = TrackedPerceptionState::new();

        let first = handle_perception_tracked(&tracked_request(5.0, 1_000), &mut state);
        let second = handle_perception_tracked(&tracked_request(5.1, 900), &mut state);

        assert_eq!(first.objects.len(), 1);
        assert_eq!(second.objects.len(), 1);
        assert_eq!(second.objects[0].vx, 0.0);
        assert_eq!(second.objects[0].vy, 0.0);
    }

    #[test]
    fn excessive_tracking_gap_resets_velocity_history() {
        let mut state = TrackedPerceptionState::new();

        handle_perception_tracked(&tracked_request(5.0, 1_000), &mut state);
        let response = handle_perception_tracked(&tracked_request(5.2, 1_501), &mut state);

        assert_eq!(response.objects.len(), 1);
        assert_eq!(response.objects[0].vx, 0.0);
        assert_eq!(response.objects[0].vy, 0.0);
    }

    #[test]
    fn malformed_request_clears_existing_tracking_state() {
        let mut state = TrackedPerceptionState::new();

        handle_perception_tracked(&tracked_request(5.0, 1_000), &mut state);

        let mut invalid = tracked_request(5.1, 1_100);
        invalid.ranges.clear();

        let invalid_response = handle_perception_tracked(&invalid, &mut state);

        assert!(!invalid_response.healthy);
        assert_eq!(invalid_response.speed_cap_mps, 0.0);

        let recovered = handle_perception_tracked(&tracked_request(5.2, 1_200), &mut state);

        assert_eq!(recovered.objects.len(), 1);
        assert_eq!(recovered.objects[0].vx, 0.0);
        assert_eq!(recovered.objects[0].vy, 0.0);
    }
}

#[cfg(test)]
mod duplicate_frame_tracking_tests {
    use super::*;

    fn request(range_m: f64, stamp_ms: u64) -> PerceptionRequest {
        let angle_min_rad = -1.5;
        let angle_increment_rad = 0.01;
        let ranges = (0..301)
            .map(|index| {
                let angle = angle_min_rad + index as f64 * angle_increment_rad;
                if angle.abs() < 0.035 {
                    range_m as f32
                } else {
                    f32::INFINITY
                }
            })
            .collect();

        PerceptionRequest {
            angle_min_rad,
            angle_increment_rad,
            range_min_m: 0.1,
            range_max_m: 12.0,
            ranges,
            stamp_ms,
            forward_extent_m: default_extent(),
            decel_mps2: default_decel(),
            margin_m: default_margin(),
            lane_half_m: default_lane_half(),
            vehicle_width_m: default_vehicle_width(),
            lateral_clearance_m: default_lateral_clearance(),
            confidence_floor: 0.5,
            camera_armed: false,
            camera_stamp_ms: None,
            camera_max_age_ms: DEFAULT_CAMERA_MAX_AGE_MS,
            publisher_count: None,
            diagnostic_probe: false,
            detections: Vec::new(),
            camera_observations: Vec::new(),
        }
    }

    #[test]
    fn duplicate_scan_timestamp_replays_response_without_advancing_tracker() {
        let mut state = TrackedPerceptionState::new();

        let first = handle_perception_tracked(&request(5.0, 1_000), &mut state);
        let duplicate = handle_perception_tracked(&request(5.0, 1_000), &mut state);
        let moved = handle_perception_tracked(&request(5.1, 1_100), &mut state);

        assert_eq!(first.objects.len(), 1);
        assert_eq!(duplicate.objects.len(), 1);
        assert_eq!(first.objects[0].id, duplicate.objects[0].id);
        assert_eq!(first.objects[0].vx, duplicate.objects[0].vx);
        assert_eq!(first.objects[0].vy, duplicate.objects[0].vy);

        assert_eq!(moved.objects.len(), 1);
        assert_eq!(moved.objects[0].id, first.objects[0].id);
        assert!(
            moved.objects[0].vx > 0.5 && moved.objects[0].vx < 1.5,
            "expected roughly 1 m/s after one real 100 ms interval, got {}",
            moved.objects[0].vx,
        );
    }
}

#[cfg(test)]
mod scan_liveness_tests {
    //! #1211 end-to-end: a frozen `/scan` reaching the served speed cap.
    //!
    //! Every test injects scans and a clock. Nothing sleeps — wall-clock waits
    //! would make a timing gate's own tests the flakiest thing in CI.

    use super::*;

    /// The configured recovery streak, for readability in the tests below.
    const SENSOR_RECOVERY_STREAK_LOCAL: u32 = 5;

    /// A live-stream scan: an obstacle straight ahead, with per-frame variation
    /// in the low bits so consecutive frames are NOT bit-identical. Real sensor
    /// noise looks like this, and it is what the freeze check keys on.
    fn live_scan(stamp_ms: u64, seq: u32) -> PerceptionRequest {
        let ranges: Vec<f32> = (0..300)
            .map(|i| {
                let theta = -1.5 + f64::from(i) * 0.01;
                let base = if theta.abs() < 0.15 {
                    6.0 / theta.cos()
                } else {
                    8.0
                };
                // One ulp of jitter, varying per frame — the low-bit noise a real
                // lidar produces even when nothing in the world moved.
                f32::from_bits((base as f32).to_bits() + (seq % 7))
            })
            .collect();
        let mut r = super::tests::req(ranges);
        r.stamp_ms = stamp_ms;
        r.publisher_count = Some(1);
        r
    }

    /// A live scan with an obstacle at `blob_x` metres, for the #1212 cap tests.
    pub(super) fn live_scan_for_cap(seq: u32, blob_x: f64) -> PerceptionRequest {
        let ranges: Vec<f32> = (0..300)
            .map(|i| {
                let theta = -1.5 + f64::from(i) * 0.01;
                let base = if theta.abs() < 0.15 {
                    blob_x / theta.cos()
                } else {
                    8.0
                };
                f32::from_bits((base as f32).to_bits() + (seq % 7))
            })
            .collect();
        let mut r = super::tests::req(ranges);
        r.stamp_ms = 1_000 + u64::from(seq) * 100;
        r.publisher_count = Some(1);
        r
    }

    /// The same buffer and the same stamp, forever — a driver republishing one
    /// frame.
    fn frozen_scan() -> PerceptionRequest {
        live_scan(1_000, 0)
    }

    fn state() -> TrackedPerceptionState {
        TrackedPerceptionState::with_watchdog(SensorWatchdogConfig::r2_lidar(), true)
    }

    /// THE issue: repeated identical `/scan` messages eventually floor the served
    /// cap. Before this wiring the same sequence produced a healthy corridor and a
    /// non-zero cap indefinitely.
    #[test]
    fn repeated_identical_scans_eventually_floor_the_served_cap() {
        let mut st = state();
        let frozen = frozen_scan();

        // Warm-up already holds the cap, so step past it on live frames first and
        // confirm the service is genuinely serving motion before the freeze.
        let mut now = 0;
        for seq in 0..6 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }
        let healthy = handle_perception_tracked_at(&live_scan(1_600, 99), &mut st, now);
        assert_eq!(healthy.sensor_liveness, "healthy");
        assert!(
            healthy.speed_cap_mps > 0.0,
            "precondition: a live stream must serve a moving cap, else the \
             assertion below proves nothing"
        );

        // Now the driver freezes: same bytes, same stamp, still arriving at the
        // same 10 Hz. Run past the 500 ms silence budget.
        let mut last = healthy;
        for _ in 0..8 {
            now += 100;
            last = handle_perception_tracked_at(&frozen, &mut st, now);
        }

        assert_eq!(last.sensor_liveness, "faulted");
        assert_eq!(last.speed_cap_mps, 0.0, "a frozen scan must floor the cap");
        assert!(!last.healthy);
        // A driver replaying one buffer produces no NEW frames, so the liveness
        // failure surfaces as "nothing new arrived" rather than as a frozen-stamp
        // count. See the dedupe rationale in `handle_perception_tracked_at`: the
        // frozen-stamp counter cannot be used at this call site without falsely
        // faulting a healthy two-consumer deployment.
        assert_eq!(last.sensor_fault, Some("SENSOR_NO_MESSAGES"));
    }

    /// Recovery takes the EXACT streak — not one frame, not an approximation.
    #[test]
    fn health_returns_only_after_the_exact_recovery_streak() {
        let mut st = state();
        let mut now = 0;

        // Fault it: a frozen stream, run past the silence budget.
        for _ in 0..10 {
            let _ = handle_perception_tracked_at(&frozen_scan(), &mut st, now);
            now += 100;
        }
        assert_eq!(
            handle_perception_tracked_at(&frozen_scan(), &mut st, now).sensor_liveness,
            "faulted"
        );

        // Valid, CHANGING frames at an acceptable rate. The cap must stay floored
        // for exactly `recovery_streak - 1` of them.
        let streak = SensorWatchdogConfig::r2_lidar().recovery_streak;
        for i in 1..streak {
            now += 100;
            let r = handle_perception_tracked_at(
                &live_scan(5_000 + u64::from(i) * 100, i),
                &mut st,
                now,
            );
            assert_eq!(
                r.sensor_liveness, "recovering",
                "frame {i} of {streak} must still be recovering"
            );
            assert_eq!(r.speed_cap_mps, 0.0, "frame {i} must stay floored");
            assert!(!r.healthy);
        }

        now += 100;
        let r = handle_perception_tracked_at(
            &live_scan(5_000 + u64::from(streak) * 100, streak),
            &mut st,
            now,
        );
        assert_eq!(
            r.sensor_liveness, "healthy",
            "the LIVENESS streak completed — this test's subject"
        );
        // The cap is a separate claim. #1212's stabilizer must also earn its own
        // streak before the envelope widens, so liveness recovering does not by
        // itself release motion. `a_recovering_sensor_must_earn_both_streaks` pins
        // the composition.
        assert_eq!(
            r.speed_cap_mps, 0.0,
            "liveness recovery alone does not release the cap"
        );
    }

    /// Startup begins unavailable: the very first scan is capped, because one
    /// observation is not evidence of a rate.
    #[test]
    fn startup_begins_unavailable() {
        let mut st = state();
        let first = handle_perception_tracked_at(&live_scan(1_000, 0), &mut st, 0);
        assert_eq!(first.sensor_liveness, "warming_up");
        assert_eq!(first.speed_cap_mps, 0.0);
        assert!(!first.healthy);
    }

    /// A watchdog fault cannot be overwritten by a healthy camera channel. The
    /// cap composes with `min`, so the tighter bound always wins regardless of
    /// which channel produced it.
    #[test]
    fn a_healthy_camera_cannot_lift_a_watchdog_fault() {
        let mut st = state();
        let mut now = 0;
        for _ in 0..10 {
            let _ = handle_perception_tracked_at(&frozen_scan(), &mut st, now);
            now += 100;
        }

        // Same frozen scan, but with the camera channel armed and fresh — the
        // healthiest possible companion channel.
        let mut req = frozen_scan();
        req.camera_armed = true;
        req.camera_stamp_ms = Some(req.stamp_ms);
        let r = handle_perception_tracked_at(&req, &mut st, now + 100);

        assert!(
            r.camera_healthy,
            "precondition: the camera channel is healthy"
        );
        assert_eq!(r.sensor_liveness, "faulted");
        assert_eq!(
            r.speed_cap_mps, 0.0,
            "a healthy camera must not lift the scan-liveness floor"
        );
    }

    /// The duplicate-stamp short-circuit serves a CACHED perception result. That
    /// path must still carry this tick's liveness verdict — otherwise a frozen
    /// driver is served the healthy cap computed before it froze, which is
    /// precisely the bug.
    #[test]
    fn the_cached_duplicate_response_still_carries_the_verdict() {
        let mut st = state();
        let mut now = 0;
        for seq in 0..6 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }
        // Establish a cached healthy response at this stamp.
        let seeded = handle_perception_tracked_at(&live_scan(9_000, 1), &mut st, now);
        assert_eq!(seeded.sensor_liveness, "healthy");
        assert!(seeded.speed_cap_mps > 0.0);

        // Replay the SAME stamp until the freeze trips. Every reply is served from
        // cache, so if the verdict were not re-applied these would stay healthy.
        let replay = live_scan(9_000, 1);
        let mut last = seeded;
        for _ in 0..8 {
            now += 100;
            last = handle_perception_tracked_at(&replay, &mut st, now);
        }
        assert_eq!(last.sensor_liveness, "faulted");
        assert_eq!(last.speed_cap_mps, 0.0);
    }

    /// Two legitimate consumers posting the SAME live scan must not read as a
    /// frozen sensor.
    ///
    /// `perception_governor` and `occy_doer` both subscribe to `/scan` and both POST
    /// what they receive, so every frame reaches this service twice. Counting the
    /// second copy as a fresh observation would trip the frozen-stamp check on a
    /// perfectly healthy robot — the false positive that would have grounded the R2
    /// the first time this shipped.
    #[test]
    fn two_consumers_posting_the_same_scan_are_not_a_frozen_sensor() {
        let mut st = state();
        let mut now = 0;
        let mut last = None;
        for seq in 0..12 {
            let scan = live_scan(1_000 + u64::from(seq) * 100, seq);
            // Consumer A, then consumer B, on the identical frame.
            let _ = handle_perception_tracked_at(&scan, &mut st, now);
            last = Some(handle_perception_tracked_at(&scan, &mut st, now + 10));
            now += 100;
        }
        let last = last.expect("ran");
        assert_eq!(
            last.sensor_liveness, "healthy",
            "a double-posted healthy stream must stay healthy"
        );
        assert_eq!(last.sensor_fault, None);
        assert!(last.speed_cap_mps > 0.0);
    }

    /// A diagnostic probe is excluded from liveness accounting, so running the
    /// doctor repeatedly cannot manufacture the freeze it is looking for.
    #[test]
    fn a_diagnostic_probe_cannot_trip_the_freeze_it_diagnoses() {
        let mut st = state();
        let mut now = 0;
        for seq in 0..6 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }

        // Ten identical probes, INTERLEAVED with the live stream — which is how a
        // doctor run actually happens: the lidar keeps publishing while the tool
        // pokes the service. (Ten probes with no live frames between them would be
        // a genuine one-second scan outage, and the watchdog is right to fault
        // that; excluding probes from liveness must not extend to masking silence.)
        let mut probe = frozen_scan();
        probe.diagnostic_probe = true;
        for seq in 10..20 {
            let r = handle_perception_tracked_at(&probe, &mut st, now);
            assert_eq!(r.sensor_liveness, "disarmed", "a probe is not assessed");
            now += 100;
            let _ = handle_perception_tracked_at(
                &live_scan(2_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
        }

        // The live stream is untouched: still healthy, no recovery streak owed.
        now += 100;
        let live = handle_perception_tracked_at(&live_scan(20_000, 3), &mut st, now);
        assert_eq!(
            live.sensor_liveness, "healthy",
            "probes must not have left the live stream in recovery"
        );
        assert!(live.speed_cap_mps > 0.0);
    }

    /// A malformed scan lands on the already-defined reason code rather than a
    /// parallel vocabulary for the same physical condition.
    #[test]
    fn an_all_invalid_scan_reports_the_ray_ratio_code() {
        let mut st = state();
        let mut bad = live_scan(1_000, 0);
        bad.ranges = vec![f32::NAN; 300];
        let r = handle_perception_tracked_at(&bad, &mut st, 0);
        assert_eq!(r.sensor_fault, Some("SENSOR_LOW_VALID_RAY_RATIO"));
        assert_eq!(r.speed_cap_mps, 0.0);
        assert!(!r.healthy);
    }

    /// A CALLER's configuration error is not a sensor fault. Blaming the sensor
    /// would both misdirect the operator and let a misconfigured client hold a
    /// healthy lidar in recovery indefinitely.
    #[test]
    fn a_caller_config_error_is_not_charged_to_the_sensor() {
        let mut st = state();
        let mut now = 0;
        for seq in 0..6 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }
        assert_eq!(
            handle_perception_tracked_at(&live_scan(2_000, 9), &mut st, now).sensor_liveness,
            "healthy"
        );

        // A bad margin — nothing to do with the lidar.
        now += 100;
        let mut bad = live_scan(2_100, 10);
        bad.margin_m = -1.0;
        let r = handle_perception_tracked_at(&bad, &mut st, now);
        assert_eq!(r.speed_cap_mps, 0.0, "the request is still refused");
        assert_eq!(r.sensor_fault, None, "…but not blamed on the sensor");

        // …and the sensor is still healthy on the next good frame, with no
        // recovery streak owed.
        now += 100;
        let r = handle_perception_tracked_at(&live_scan(2_200, 11), &mut st, now);
        assert_eq!(r.sensor_liveness, "healthy");
        assert!(r.speed_cap_mps > 0.0);
    }

    /// An unobserved publisher count is reported as unobserved rather than
    /// silently treated as zero publishers.
    #[test]
    fn an_unobserved_publisher_count_is_visible_not_assumed() {
        let mut st = state();
        let mut req = live_scan(1_000, 0);
        req.publisher_count = None;
        let r = handle_perception_tracked_at(&req, &mut st, 0);
        assert!(!r.publisher_count_observed);
        assert_eq!(r.sensor_fault, None, "absence is not a NoPublisher fault");

        let mut req = live_scan(1_100, 1);
        req.publisher_count = Some(1);
        let r = handle_perception_tracked_at(&req, &mut st, 100);
        assert!(r.publisher_count_observed);
    }

    /// Zero publishers, observed at the ROS layer, is its own fault code.
    #[test]
    fn zero_observed_publishers_faults_distinctly() {
        let mut st = state();
        let mut req = live_scan(1_000, 0);
        req.publisher_count = Some(0);
        let r = handle_perception_tracked_at(&req, &mut st, 0);
        assert_eq!(r.sensor_fault, Some("SENSOR_NO_PUBLISHER"));
        assert_eq!(r.speed_cap_mps, 0.0);
    }

    /// The disarmed path is byte-identical to pre-#1211 behaviour: no cap, no
    /// verdict, whatever the stream does.
    #[test]
    fn the_disarmed_path_is_unchanged() {
        // Both gates off: this test is about the LIVENESS watchdog being disarmed,
        // so cap stabilization (#1212, a separate concern) is off too — otherwise it
        // would be asserting the absence of two features while naming one.
        let mut st =
            TrackedPerceptionState::with_options(SensorWatchdogConfig::r2_lidar(), false, false);
        let frozen = frozen_scan();
        let mut now = 0;
        let mut last = None;
        for _ in 0..10 {
            last = Some(handle_perception_tracked_at(&frozen, &mut st, now));
            now += 100;
        }
        let last = last.expect("ran");
        assert_eq!(last.sensor_liveness, "disarmed");
        assert_eq!(last.sensor_fault, None);
        assert!(
            last.speed_cap_mps > 0.0,
            "disarmed must not floor the cap — that is the pre-#1211 behaviour \
             this gate exists to change only when armed"
        );
    }

    // -----------------------------------------------------------------------
    // Pre-merge review focus (#1223). Each names the property it pins.
    // -----------------------------------------------------------------------

    /// (1) A diagnostic probe cannot advance the recovery streak.
    ///
    /// `diagnostic_probe` must be observationally INERT: it may neither create
    /// liveness state nor clear it. A probe that could pay down a recovery streak
    /// would let a diagnostic tool restore motion the sensor had not earned.
    #[test]
    fn a_probe_cannot_advance_the_recovery_streak() {
        let mut st = state();
        let mut now = 0;
        // Fault the stream.
        for _ in 0..10 {
            let _ = handle_perception_tracked_at(&frozen_scan(), &mut st, now);
            now += 100;
        }

        // A live frame starts the streak at 1.
        now += 100;
        let r = handle_perception_tracked_at(&live_scan(50_000, 1), &mut st, now);
        assert_eq!(
            r.sensor_liveness, "recovering",
            "precondition: the stream is recovering"
        );

        // Probes in between must not pay it down.
        let mut probe = live_scan(51_000, 2);
        probe.diagnostic_probe = true;
        for _ in 0..10 {
            now += 10;
            let _ = handle_perception_tracked_at(&probe, &mut st, now);
        }

        now += 100;
        let r = handle_perception_tracked_at(&live_scan(50_100, 2), &mut st, now);
        assert_eq!(
            r.sensor_liveness, "recovering",
            "10 probes must not have completed a 5-frame streak"
        );
        assert_eq!(r.speed_cap_mps, 0.0);
    }

    /// (2) Duplicate re-posts cannot advance the recovery streak.
    ///
    /// The subtle one. A re-post is not silence — the budget has not expired — so
    /// it reaches the watchdog as a clean tick. If a clean tick advanced the
    /// streak, a frozen driver's own repetitions would earn its recovery: the
    /// stream would "recover" from a freeze on the strength of the very
    /// repetitions that constitute the freeze.
    #[test]
    fn duplicate_reposts_cannot_advance_the_recovery_streak() {
        let mut st = state();
        let mut now = 0;
        for _ in 0..10 {
            let _ = handle_perception_tracked_at(&frozen_scan(), &mut st, now);
            now += 100;
        }

        // One real frame — streak 1.
        now += 100;
        let fresh = live_scan(60_000, 1);
        let r = handle_perception_tracked_at(&fresh, &mut st, now);
        assert_eq!(r.sensor_liveness, "recovering");

        // Re-post that SAME frame repeatedly, inside the silence budget.
        for _ in 0..10 {
            now += 10;
            let r = handle_perception_tracked_at(&fresh, &mut st, now);
            assert_eq!(
                r.sensor_liveness, "recovering",
                "a re-post must never complete the streak"
            );
            assert_eq!(r.speed_cap_mps, 0.0);
        }

        // The streak is still owed: four more REAL frames are required.
        for i in 2..SENSOR_RECOVERY_STREAK_LOCAL {
            now += 100;
            let r = handle_perception_tracked_at(
                &live_scan(60_000 + u64::from(i) * 100, i),
                &mut st,
                now,
            );
            assert_eq!(
                r.sensor_liveness, "recovering",
                "frame {i} still recovering"
            );
        }
        now += 100;
        let r = handle_perception_tracked_at(&live_scan(70_000, 9), &mut st, now);
        assert_eq!(r.sensor_liveness, "healthy");
    }

    /// (3) Continuous duplicate re-posts eventually fault as no-new-messages.
    #[test]
    fn continuous_reposts_fault_as_no_new_messages() {
        let mut st = state();
        let mut now = 0;
        for seq in 0..8 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }
        let seeded = live_scan(80_000, 1);
        assert_eq!(
            handle_perception_tracked_at(&seeded, &mut st, now).sensor_liveness,
            "healthy"
        );

        // Nothing but re-posts from here.
        let mut last = None;
        for _ in 0..10 {
            now += 100;
            last = Some(handle_perception_tracked_at(&seeded, &mut st, now));
        }
        let last = last.expect("ran");
        assert_eq!(last.sensor_fault, Some("SENSOR_NO_MESSAGES"));
        assert_eq!(last.speed_cap_mps, 0.0);
    }

    /// (4) A real new frame after duplicates resumes from the correct state —
    /// recovering with the streak the stream actually earned, not healthy and not
    /// reset to zero.
    #[test]
    fn a_real_frame_after_duplicates_resumes_correctly() {
        let mut st = state();
        let mut now = 0;
        for seq in 0..8 {
            let _ = handle_perception_tracked_at(
                &live_scan(1_000 + u64::from(seq) * 100, seq),
                &mut st,
                now,
            );
            now += 100;
        }

        // Freeze into a fault via re-posts.
        let stuck = live_scan(90_000, 1);
        for _ in 0..10 {
            now += 100;
            let _ = handle_perception_tracked_at(&stuck, &mut st, now);
        }
        assert_eq!(
            handle_perception_tracked_at(&stuck, &mut st, now).sensor_fault,
            Some("SENSOR_NO_MESSAGES")
        );

        // A genuinely new frame resumes recovery from 1 — not healthy (the streak
        // is owed) and not stuck faulted (the sensor is producing again).
        now += 100;
        let r = handle_perception_tracked_at(&live_scan(95_000, 2), &mut st, now);
        assert_eq!(
            r.sensor_liveness, "recovering",
            "a new frame after a freeze resumes recovery"
        );
        assert_eq!(r.speed_cap_mps, 0.0);

        // …and the streak completes normally from there.
        for i in 2..SENSOR_RECOVERY_STREAK_LOCAL {
            now += 100;
            let _ = handle_perception_tracked_at(
                &live_scan(95_000 + u64::from(i) * 100, i),
                &mut st,
                now,
            );
        }
        now += 100;
        let r = handle_perception_tracked_at(&live_scan(99_000, 9), &mut st, now);
        assert_eq!(r.sensor_liveness, "healthy");
        // Cap release is the stabilizer's separate judgement — see
        // `a_recovering_sensor_must_earn_both_streaks`.
    }

    /// (5) Two live clients posting the same scan cannot double the measured rate.
    ///
    /// If re-posts recorded arrivals, a 10 Hz sensor read by two consumers would
    /// measure 20 Hz — which would mask a sensor that had genuinely halved its
    /// rate to 10 Hz while two clients kept it looking nominal.
    #[test]
    fn two_clients_cannot_double_the_measured_rate() {
        let mut st = state();
        let mut now = 0;
        let mut last = None;
        for seq in 0..12 {
            let scan = live_scan(1_000 + u64::from(seq) * 100, seq);
            let _ = handle_perception_tracked_at(&scan, &mut st, now);
            last = Some(handle_perception_tracked_at(&scan, &mut st, now + 10));
            now += 100;
        }
        let hz = last.expect("ran").sensor_measured_hz.expect("measurable");
        assert!(
            (hz - 10.0).abs() < 0.5,
            "double-posted 10 Hz must measure ~10 Hz, got {hz}"
        );
    }

    /// (7) A restarted sidecar begins unavailable and stays there until the
    /// required healthy evidence has arrived — fresh state is the restart.
    #[test]
    fn a_restarted_sidecar_begins_unavailable() {
        let mut st = state();
        let first = handle_perception_tracked_at(&live_scan(1_000, 0), &mut st, 0);
        assert_eq!(first.sensor_liveness, "warming_up");
        assert_eq!(first.speed_cap_mps, 0.0);
        assert!(!first.healthy);

        // Availability arrives only with a second observation — the first frame
        // that makes a rate measurable.
        let second = handle_perception_tracked_at(&live_scan(1_100, 1), &mut st, 100);
        assert_eq!(
            second.sensor_liveness, "healthy",
            "two observations make a rate measurable — the liveness claim"
        );
        // The cap stays floored: the stabilizer has its own evidence to gather, and
        // a restarted sidecar has none of it.
        assert_eq!(second.speed_cap_mps, 0.0);
    }

    /// The evidence digest identifies the perception EVIDENCE, not the liveness
    /// verdict about it: the same scan bytes digest identically whether they
    /// arrived first or third in a frozen run.
    #[test]
    fn the_liveness_verdict_stays_out_of_the_evidence_digest() {
        let mut st = state();
        let frozen = frozen_scan();
        let first = handle_perception_tracked_at(&frozen, &mut st, 0);

        let mut fresh = state();
        let independent = handle_perception_tracked_at(&frozen, &mut fresh, 0);

        assert_eq!(
            first.frame_id.evidence_digest, independent.frame_id.evidence_digest,
            "identical scan bytes must carry identical evidence identity"
        );
    }
}

#[cfg(test)]
mod cap_authority_tests {
    //! #1212 authority migration: the checker's stabilizer is what the sidecar
    //! serves. Injected scans and an injected clock throughout; nothing sleeps.

    use super::scan_liveness_tests::live_scan_for_cap as live_scan;
    use super::*;

    fn state() -> TrackedPerceptionState {
        TrackedPerceptionState::with_watchdog(SensorWatchdogConfig::r2_lidar(), true)
    }

    /// Warm both the liveness watchdog and the stabilizer to steady state, so a test
    /// about one is not measuring the other's warm-up. Returns the next timestamp.
    fn warm(st: &mut TrackedPerceptionState, seq_from: u32, mut now: u64) -> (u32, u64) {
        let mut seq = seq_from;
        for _ in 0..20 {
            now += 100;
            seq += 1;
            let _ = handle_perception_tracked_at(&live_scan(seq, 6.0), st, now);
        }
        (seq, now)
    }

    /// The served `speed_cap_mps` is the STABILIZED value, and the pre-stabilization
    /// number is carried alongside rather than discarded. A consumer that only ever
    /// saw one number could not tell a hazard from a filter still catching up —
    /// which is how the Python's behaviour became invisible.
    #[test]
    fn the_served_cap_is_the_stabilized_one_with_raw_alongside() {
        let mut st = state();
        let mut now = 0;
        let mut seq = 0;

        // First frame: perception proposes a real cap, the stabilizer holds at zero.
        now += 100;
        seq += 1;
        let r = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);
        assert!(
            r.raw_speed_cap_mps > 0.0,
            "perception proposed a cap: {}",
            r.raw_speed_cap_mps
        );
        assert_eq!(r.speed_cap_mps, 0.0, "…and the checker holds it");
        assert_eq!(r.stabilized_speed_cap_mps, r.speed_cap_mps);
        assert_eq!(r.cap_reason, Some("CAP_INITIAL_HOLD"));
    }

    /// A restriction reaches the wire on the frame it is observed — no smoothing on
    /// the way down, through the whole sidecar path.
    #[test]
    fn a_restriction_reaches_the_wire_immediately() {
        let mut st = state();
        let (mut seq, mut now) = warm(&mut st, 0, 0);
        let before = {
            now += 100;
            seq += 1;
            handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now)
        };
        assert!(
            before.speed_cap_mps > 0.5,
            "precondition: a cap was earned, got {}",
            before.speed_cap_mps
        );

        // An obstacle appears VERY close, so the new raw cap is below the standing
        // stabilized one and the drop is visible. A nearer-but-still-above-standing
        // hazard would register as CAP_RESTRICTED without moving the served number,
        // because the enforced cap was already more conservative than the new
        // hazard requires — correct, but it proves nothing about immediacy.
        now += 100;
        seq += 1;
        let r = handle_perception_tracked_at(&live_scan(seq, 0.4), &mut st, now);
        assert_eq!(r.cap_reason, Some("CAP_RESTRICTED"));
        assert!(
            r.speed_cap_mps < before.speed_cap_mps,
            "a tightening cap must apply on this frame: {} -> {}",
            before.speed_cap_mps,
            r.speed_cap_mps
        );
        assert!(
            r.speed_cap_mps <= r.raw_speed_cap_mps + 1e-9,
            "and the served cap never exceeds what perception proposed"
        );
    }

    /// A recovering sensor must earn BOTH streaks — liveness and stabilization.
    ///
    /// They are different claims: "the sensor is producing frames again" and "the
    /// envelope may widen". Composing them is deliberate, and it means recovery
    /// costs more than either alone. This pins that the cap is still floored at the
    /// moment liveness reports healthy.
    #[test]
    fn a_recovering_sensor_must_earn_both_streaks() {
        let mut st = state();
        let (mut seq, mut now) = warm(&mut st, 0, 0);

        // Fault it: no publisher.
        now += 100;
        let mut dead = live_scan(seq + 1, 6.0);
        dead.publisher_count = Some(0);
        let r = handle_perception_tracked_at(&dead, &mut st, now);
        assert_eq!(r.sensor_fault, Some("SENSOR_NO_PUBLISHER"));
        assert_eq!(r.speed_cap_mps, 0.0);

        // Drive recovery and record when each claim clears.
        let mut liveness_healthy_at = None;
        let mut cap_released_at = None;
        for i in 1..40 {
            now += 100;
            seq += 1;
            let r = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);
            if liveness_healthy_at.is_none() && r.sensor_liveness == "healthy" {
                liveness_healthy_at = Some(i);
            }
            if cap_released_at.is_none() && r.speed_cap_mps > 0.0 {
                cap_released_at = Some(i);
            }
        }
        let live_at = liveness_healthy_at.expect("liveness must recover");
        let cap_at = cap_released_at.expect("the cap must eventually be released");
        assert!(
            cap_at >= live_at,
            "the cap must not be released before liveness recovers (cap {cap_at}, \
             liveness {live_at})"
        );
        assert!(
            cap_at > 1,
            "the cap must not be released on the first frame after a fault"
        );
    }

    /// A re-post does not advance the stabilizer.
    ///
    /// Two consumers posting the same scan would otherwise inflate the frame rate
    /// the rise limiter divides by, so a two-consumer deployment would climb twice
    /// as fast as a one-consumer deployment on identical perception.
    #[test]
    fn a_repost_does_not_advance_the_stabilizer() {
        let mut single = state();
        let mut doubled = state();
        let mut now = 0;
        let mut seq = 0;
        let mut last_single = None;
        let mut last_doubled = None;
        for _ in 0..25 {
            now += 100;
            seq += 1;
            let scan = live_scan(seq, 6.0);
            last_single = Some(handle_perception_tracked_at(&scan, &mut single, now));
            // Consumer A then consumer B, same frame.
            let _ = handle_perception_tracked_at(&scan, &mut doubled, now);
            last_doubled = Some(handle_perception_tracked_at(&scan, &mut doubled, now + 10));
        }
        let a = last_single.expect("ran").speed_cap_mps;
        let b = last_doubled.expect("ran").speed_cap_mps;
        assert!(
            (a - b).abs() < 1e-9,
            "double-posting must not change the climb: single {a}, doubled {b}"
        );
    }

    /// The stabilizer is not cleared by a tracking reset. A tracker reset is a
    /// discontinuity in object identity, not evidence that the speed envelope may be
    /// relaxed — clearing it there would hand back an unearned cap.
    #[test]
    fn a_tracking_reset_does_not_hand_back_the_cap() {
        let mut st = state();
        let (seq, now) = warm(&mut st, 0, 0);
        let earned = handle_perception_tracked_at(&live_scan(seq + 1, 6.0), &mut st, now + 100)
            .speed_cap_mps;
        assert!(earned > 0.0, "precondition");

        st.reset();
        let r = handle_perception_tracked_at(&live_scan(seq + 2, 6.0), &mut st, now + 200);

        // The cap may continue its normal bounded climb — one frame of rise is
        // expected and correct. What must NOT happen is the reset handing back the
        // full perception cap, which is what clearing the stabilizer would do.
        let one_step = kirra_trajectory::cap_stabilizer::DEFAULT_RISE_RATE_MPS_PER_S * 0.1;
        assert!(
            r.speed_cap_mps <= earned + one_step + 1e-9,
            "a tracking reset must not jump the cap beyond one bounded step: {} > {} \
             + {}",
            r.speed_cap_mps,
            earned,
            one_step
        );
        assert!(
            r.speed_cap_mps < r.raw_speed_cap_mps,
            "…and certainly not to perception's full proposal"
        );
    }

    /// Stabilization does not run on the stateless entry point, and says so rather
    /// than reporting a stabilized number it did not compute.
    #[test]
    fn the_stateless_path_does_not_stabilize() {
        let r = handle_perception(&live_scan(1, 6.0));
        assert_eq!(r.cap_reason, None, "no stabilization was performed");
        assert_eq!(
            r.speed_cap_mps, r.raw_speed_cap_mps,
            "the served cap is perception's own, unfiltered"
        );
    }

    /// A duplicate frame is served the cap the stabilizer is HOLDING, and says so.
    ///
    /// The reason is not left absent. A consumer that fail-closes on a missing reason
    /// — the ROS relay does, deliberately, so an unstabilized sidecar cannot pass its
    /// raw cap off as stabilized — would floor every duplicate to zero, and a
    /// two-node deployment where the second node re-posts would stutter between the
    /// held cap and a stop on identical perception.
    #[test]
    fn a_replayed_frame_reports_a_held_cap_as_a_held_cap() {
        let mut st = state();
        let (seq, now) = warm(&mut st, 0, 0);

        let scan = live_scan(seq + 1, 6.0);
        let first = handle_perception_tracked_at(&scan, &mut st, now + 100);
        assert!(first.speed_cap_mps > 0.0, "precondition: a cap was earned");

        // The same frame again, from a second consumer.
        let replay = handle_perception_tracked_at(&scan, &mut st, now + 110);
        assert_eq!(
            replay.cap_reason,
            Some("CAP_REPLAY_HELD"),
            "a replay is stabilizer-authored and must claim authorship"
        );
        assert_eq!(
            replay.speed_cap_mps, replay.stabilized_speed_cap_mps,
            "the served number is the stabilized one"
        );
        assert!(
            replay.speed_cap_mps <= first.speed_cap_mps,
            "a replay must never climb: {} > {}",
            replay.speed_cap_mps,
            first.speed_cap_mps
        );
    }

    /// A replay reports the limiting object along with the cap it is holding.
    ///
    /// Serving the held number while clearing the identity describes a cap held
    /// against nothing — which reads as an open corridor, the opposite of the truth,
    /// to anything that attributes the restriction.
    #[test]
    fn a_replay_carries_the_limiting_identity_with_the_number() {
        let mut st = state();
        let (seq, now) = warm(&mut st, 0, 0);

        // A blob close enough to bind the clear distance rather than the corridor.
        let scan = live_scan(seq + 1, 1.2);
        let first = handle_perception_tracked_at(&scan, &mut st, now + 100);
        let held = first.cap_limiting_object;
        assert!(
            held.is_some(),
            "precondition: an object bound the cap, got {held:?}"
        );

        let replay = handle_perception_tracked_at(&scan, &mut st, now + 110);
        assert_eq!(
            replay.cap_limiting_object, held,
            "the identity travels with the number it explains"
        );
    }

    /// Gate: the whole authority boundary, walked as ONE sequence rather than as
    /// separate assertions about separate stages.
    ///
    /// The claim under test is not any single transition — it is that across a run
    /// containing a warm-up, a climb, a hazard, a recovery and a replay, the number
    /// on the wire is ALWAYS the checker's and never perception's. A per-stage test
    /// can pass while the composition leaks: one path that forgets to stabilize, or
    /// one that stamps `speed_cap_mps` after the stabilizer ran, would be invisible
    /// to every test that only looks at its own stage.
    #[test]
    fn the_checker_owns_the_served_cap_across_the_whole_sequence() {
        let mut st = state();
        let mut now = 0u64;
        let mut seq = 0u32;
        let mut ever_below_raw = false;
        let mut steps = 0usize;

        // Warm-up, climb, a hazard, a recovery — then the last frame served twice.
        let mut plan: Vec<f64> = Vec::new();
        plan.extend(std::iter::repeat_n(6.0, 20)); // warm + climb
        plan.push(0.4); // a hazard arrives
        plan.extend(std::iter::repeat_n(6.0, 12)); // and clears

        let check = |r: &PerceptionResponse, steps: &mut usize, ever_below_raw: &mut bool| {
            assert_eq!(
                r.speed_cap_mps, r.stabilized_speed_cap_mps,
                "the enforced number must be the stabilized one at every step"
            );
            assert!(
                r.speed_cap_mps <= r.raw_speed_cap_mps + 1e-9,
                "the checker may only tighten perception's proposal: {} > {}",
                r.speed_cap_mps,
                r.raw_speed_cap_mps
            );
            assert!(
                r.cap_reason.is_some(),
                "every frame the stabilizer authored carries its authorship claim"
            );
            *steps += 1;
            if r.speed_cap_mps < r.raw_speed_cap_mps {
                *ever_below_raw = true;
            }
        };

        let mut last = None;
        for x in plan {
            now += 100;
            seq += 1;
            let scan = live_scan(seq, x);
            let r = handle_perception_tracked_at(&scan, &mut st, now);
            check(&r, &mut steps, &mut ever_below_raw);
            last = Some(scan);
        }

        // The replay path is part of the boundary, not a separate feature: a second
        // consumer asking about a frame already served must get the same answer under
        // the same authority, not an unstabilized one.
        let replay = handle_perception_tracked_at(&last.expect("plan ran"), &mut st, now + 10);
        check(&replay, &mut steps, &mut ever_below_raw);
        assert_eq!(replay.cap_reason, Some("CAP_REPLAY_HELD"));

        assert_eq!(steps, 34, "the whole sequence ran, including the replay");
        // Non-vacuity: if the stabilized cap had merely tracked the raw one the whole
        // way, every assertion above would hold while nothing was being stabilized.
        assert!(
            ever_below_raw,
            "the stabilized cap must actually differ from the raw one somewhere, \
             or this test proves nothing"
        );
    }

    /// Gate: liveness recovery and cap release COMPOSE, and the composite latency is
    /// bounded rather than open-ended.
    ///
    /// Two independent gates stand between a recovered sensor and a moving robot: the
    /// watchdog's healthy-frame streak, and the stabilizer's clear confirmations plus
    /// bounded rise. Neither alone is the answer an integrator needs. This pins the
    /// composite: nothing is served before BOTH are satisfied, and the wait does
    /// terminate.
    #[test]
    fn liveness_recovery_and_cap_release_compose_to_a_bounded_latency() {
        let mut st = state();
        let (mut seq, mut now) = warm(&mut st, 0, 0);
        let earned = handle_perception_tracked_at(&live_scan(seq + 1, 6.0), &mut st, now + 100)
            .speed_cap_mps;
        seq += 1;
        now += 100;
        assert!(earned > 0.0, "precondition: the robot was moving");

        // A silence long past the watchdog's budget: the sensor stops, then returns.
        now += 5_000;
        let faulted = {
            seq += 1;
            handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now)
        };
        assert_eq!(
            faulted.speed_cap_mps, 0.0,
            "a liveness fault floors the cap immediately"
        );

        // Now feed clean frames and find the first that serves motion again.
        let mut frames_to_move = None;
        for i in 1..=200 {
            now += 100;
            seq += 1;
            let r = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);
            if r.speed_cap_mps > 0.0 {
                assert_eq!(
                    r.sensor_liveness, "healthy",
                    "motion was served while liveness was still {}",
                    r.sensor_liveness
                );
                frames_to_move = Some(i);
                break;
            }
        }

        let frames = frames_to_move.expect("recovery must terminate, not stall forever");
        // The composite is at least the liveness streak: the cap cannot begin to
        // climb while the watchdog is still floor-capping every frame.
        let streak = SensorWatchdogConfig::r2_lidar().recovery_streak;
        assert!(
            frames >= streak,
            "motion returned in {frames} frames, before the {streak}-frame liveness \
             streak could complete — the two gates are not composing"
        );
        // …and it is bounded. Measured at 17 frames — 1.7 s at the 10 Hz this feeds:
        // roughly 0.5 s of liveness streak, then the stabilizer's confirmations and
        // its bounded climb off the floor. That figure is what an integrator needs
        // and neither gate produces on its own. The bound is a regression pin, not a
        // specification: a change that doubles the wait should be a deliberate one.
        assert!(
            frames <= 25,
            "recovery took {frames} frames (measured 17); if this is intended, move \
             the bound and say why"
        );
    }
}

#[cfg(test)]
mod nearest_candidate_tests {
    //! The nearest-object ordering, tested directly. Driving these cases through a
    //! synthetic scan is possible but proves less: the scan generator cannot produce
    //! a NaN distance or two bit-adjacent ones on demand, which are exactly the
    //! cases where an ordering stops being total.

    use super::nearest_candidate;

    fn pick(items: &[(u64, f64)]) -> Option<(u64, f64)> {
        nearest_candidate(items.iter().copied())
    }

    #[test]
    fn no_candidates_yields_none() {
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn the_nearest_wins() {
        assert_eq!(pick(&[(7, 4.0), (3, 1.5), (9, 2.5)]), Some((3, 1.5)));
    }

    /// A NaN distance must not win, and — the part that matters — must not take the
    /// other candidates down with it.
    ///
    /// The ordering compares `false` in both directions against NaN, so a NaN reaching
    /// the fold wins every arm. Dropping it afterwards would then report NO nearest
    /// object, and a real obstacle two metres ahead would be excluded from the clear
    /// distance entirely. The filter runs first for exactly this reason.
    #[test]
    fn a_nan_distance_does_not_mask_a_real_object() {
        assert_eq!(pick(&[(1, f64::NAN), (2, 2.0)]), Some((2, 2.0)));
        assert_eq!(pick(&[(2, 2.0), (1, f64::NAN)]), Some((2, 2.0)));
        assert_eq!(pick(&[(1, f64::NAN)]), None, "nothing usable is None");
    }

    #[test]
    fn infinite_distances_are_not_candidates() {
        assert_eq!(pick(&[(1, f64::INFINITY), (2, 3.0)]), Some((2, 3.0)));
        assert_eq!(pick(&[(1, f64::NEG_INFINITY), (2, 3.0)]), Some((2, 3.0)));
    }

    /// Exact ties break on the lower id, whichever order they arrive in. An
    /// order-dependent tie-break would alternate the reported identity frame to
    /// frame, which the stabilizer's persistence cannot tell from a dropout.
    #[test]
    fn an_exact_tie_breaks_on_the_lower_id_in_either_order() {
        assert_eq!(pick(&[(9, 2.0), (4, 2.0)]), Some((4, 2.0)));
        assert_eq!(pick(&[(4, 2.0), (9, 2.0)]), Some((4, 2.0)));
    }

    /// Distances that differ only in the last bit are two distances, not a tie. The
    /// nearer wins on its own merit and the id is not consulted — which is what makes
    /// the ordering total. An epsilon-based "close enough" predicate would call these
    /// equal, and equality that is not transitive orders a three-element set
    /// differently depending on which pair is compared first.
    #[test]
    fn bit_adjacent_distances_are_ordered_not_tied() {
        let near = 2.0_f64;
        let far = f64::from_bits(near.to_bits() + 1);
        assert!(near < far, "precondition: the two differ");
        // The HIGHER id is nearer, so an epsilon that swallowed the difference would
        // hand this to the lower id and the assertion would catch it.
        assert_eq!(pick(&[(1, far), (9, near)]), Some((9, near)));
        assert_eq!(pick(&[(9, near), (1, far)]), Some((9, near)));
    }

    /// A duplicate id is not a contradiction to resolve, it is two readings. The
    /// nearer one is kept; on an exact tie the first is kept, so a duplicate cannot
    /// make the result depend on how many times it appears.
    #[test]
    fn duplicate_ids_keep_the_nearer_reading() {
        assert_eq!(pick(&[(5, 3.0), (5, 1.0)]), Some((5, 1.0)));
        assert_eq!(pick(&[(5, 1.0), (5, 3.0)]), Some((5, 1.0)));
        assert_eq!(pick(&[(5, 2.0), (5, 2.0)]), Some((5, 2.0)));
    }

    /// Zero and negative distances are real readings (an object at or behind the
    /// sensor origin) and must be ordered, not discarded. Discarding them would drop
    /// the single most urgent case.
    #[test]
    fn zero_and_negative_distances_are_ordered_not_dropped() {
        assert_eq!(pick(&[(1, 1.0), (2, 0.0)]), Some((2, 0.0)));
        assert_eq!(pick(&[(1, 0.0), (2, -0.5)]), Some((2, -0.5)));
    }

    /// The ordering is a total order: every permutation of the same set yields the
    /// same winner. This is the property an epsilon comparison would break, and it is
    /// invisible in any test that only ever feeds one ordering.
    #[test]
    fn every_permutation_of_a_set_yields_the_same_winner() {
        let base = [(3, 2.0), (1, 2.0), (7, 1.25), (5, f64::NAN), (2, 9.0)];
        let expected = pick(&base);
        assert_eq!(expected, Some((7, 1.25)), "precondition");

        let mut items = base;
        // Every rotation, and every rotation reversed — enough orderings that a
        // fold-order dependency shows up.
        for _ in 0..items.len() {
            items.rotate_left(1);
            assert_eq!(pick(&items), expected, "rotation {items:?}");
            let mut rev = items;
            rev.reverse();
            assert_eq!(pick(&rev), expected, "reversed {rev:?}");
        }
    }
}

#[cfg(test)]
mod evidence_frame_identity_tests {
    //! #1214 — the identity Taj stamps on a frame is the binding target every
    //! downstream hop carries. These tests pin the properties consumers rely on.

    use super::scan_liveness_tests::live_scan_for_cap as live_scan;
    use super::*;

    fn state() -> TrackedPerceptionState {
        TrackedPerceptionState::with_options(SensorWatchdogConfig::r2_lidar(), false, false)
    }

    /// A freshly-started sidecar must serve evidence a consumer will ACCEPT.
    ///
    /// The regression: `tracker_generation` began at zero, which is the value
    /// `PerceptionFrameId::invalid()` uses and which every consumer refuses as
    /// "not real evidence". A cold-started Taj therefore served frames the
    /// planner seam rejected with `INVALID_PERCEPTION_FRAME_ID`, so the
    /// evidence-bound motion path never worked from a cold start — it only came
    /// alive if a tracker reset happened to advance the counter off zero.
    ///
    /// It failed CLOSED, so nothing unsafe was admitted; the binding was simply
    /// inert. That is the harder failure to notice, and the reason this is a
    /// test rather than a comment.
    #[test]
    fn a_cold_started_tracker_does_not_report_the_invalid_sentinel() {
        let mut st = state();
        let first = handle_perception_tracked_at(&live_scan(1, 6.0), &mut st, 0);

        assert_ne!(
            first.frame_id.tracker_generation, RESERVED_INVALID_TRACKER_GENERATION,
            "the first live generation collides with the invalid-frame sentinel, \
             so every consumer that refuses a zero generation refuses real evidence"
        );
        assert!(
            first.frame_id.scan_sequence > 0,
            "scan_sequence is stamped after its pre-increment, so the first frame is 1"
        );
    }

    /// The sentinel is still refusable — the point of moving the first live
    /// generation off zero is to KEEP zero meaning "no evidence", not to retire
    /// it. Without this, the fix above could be "achieved" by making consumers
    /// accept zero, which would let a default-constructed frame authorize motion.
    #[test]
    fn the_invalid_sentinel_remains_distinguishable_from_every_live_frame() {
        let sentinel = PerceptionFrameId::invalid();
        assert_eq!(
            sentinel.tracker_generation,
            RESERVED_INVALID_TRACKER_GENERATION
        );
        assert_eq!(sentinel.scan_sequence, 0);
        assert!(sentinel.evidence_digest.is_empty());

        let mut st = state();
        let mut now = 0;
        for seq in 1..=12 {
            now += 100;
            let r = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);
            assert_ne!(
                r.frame_id.tracker_generation, sentinel.tracker_generation,
                "a live frame must never be mistakable for the sentinel"
            );
        }

        // …including after the resets that advance the generation.
        st.reset();
        now += 100;
        let after = handle_perception_tracked_at(&live_scan(99, 6.0), &mut st, now);
        assert!(after.frame_id.tracker_generation > FIRST_TRACKER_GENERATION.get());
    }

    /// Exhausting the generation counter cannot wrap it back onto the sentinel.
    ///
    /// The counter is `NonZeroU64` and advances with `saturating_add`, so at the
    /// ceiling it sticks at `u64::MAX`. The degradation is that further resets
    /// stop being distinguishable as new epochs — a real loss, but a
    /// conservative one: the supersession rule keeps refusing anything OLDER,
    /// and the frame never reads as invalid evidence.
    ///
    /// Reaching this requires 2^64 resets, so the test drives the counter
    /// directly rather than pretending to get there. The point is the shape of
    /// the failure at the boundary, not its reachability.
    #[test]
    fn an_exhausted_generation_counter_saturates_rather_than_reaching_the_sentinel() {
        let ceiling = NonZeroU64::new(u64::MAX).expect("nonzero");
        let advanced = ceiling.saturating_add(1);
        assert_eq!(
            advanced.get(),
            u64::MAX,
            "the counter must stick at the ceiling, not wrap"
        );
        assert_ne!(
            advanced.get(),
            RESERVED_INVALID_TRACKER_GENERATION,
            "wrapping onto the sentinel would turn counter exhaustion into \
             every frame reading as invalid evidence"
        );

        // And one below the ceiling still advances normally, so the assertion
        // above is about saturation rather than a counter that never moves.
        let below = NonZeroU64::new(u64::MAX - 1).expect("nonzero");
        assert_eq!(below.saturating_add(1).get(), u64::MAX);
    }

    /// Track ids never recycle WITHIN a generation, and every tracker
    /// replacement advances the generation — so `(tracker_generation, id)` is a
    /// stable binding target.
    ///
    /// This is what makes "a proposal naming track 7" refusable when track 7 has
    /// been re-established as a different object: the re-establishment can only
    /// happen through a tracker replacement, and every such replacement lands in
    /// `reset()`, which bumps the generation the payload binds.
    #[test]
    fn a_tracker_reset_advances_the_generation_so_recycled_ids_are_distinguishable() {
        let mut st = state();
        let mut now = 0;
        let mut seq = 0;

        seq += 1;
        now += 100;
        let before = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);
        let generation_before = before.frame_id.tracker_generation;

        // A reset discards the tracker, so the next scan starts id assignment
        // over from zero — ids DO repeat across this boundary.
        st.reset();
        seq += 1;
        now += 100;
        let after = handle_perception_tracked_at(&live_scan(seq, 6.0), &mut st, now);

        assert!(
            after.frame_id.tracker_generation > generation_before,
            "ids restart at zero after a reset, so the generation MUST advance — \
             otherwise two different objects share an identity"
        );
    }
}
