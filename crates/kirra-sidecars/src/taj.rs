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
use kirra_taj::{
    clip_corridor_to_hazards, hazard_clip_x, LaserScan, SemanticClass, SemanticDetection,
    TajConfig, TajTracker,
};
use serde::{Deserialize, Serialize};

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
    /// Producer stamp of the camera frame the detections came from. `None` while
    /// `camera_armed` is a SILENT channel → fail closed ("the detector did not
    /// look" is never "clear").
    #[serde(default)]
    pub camera_stamp_ms: Option<u64>,
    /// Freshness budget for `camera_stamp_ms` against `stamp_ms`.
    #[serde(default = "default_camera_age")]
    pub camera_max_age_ms: u64,
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
fn invalid_perception_response(camera_armed: bool) -> PerceptionResponse {
    PerceptionResponse {
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
        camera_healthy: !camera_armed,
        camera_clip_x_m: None,
    }
}

#[derive(Serialize)]
pub struct ObjOut {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
}

#[derive(Serialize)]
pub struct PerceptionResponse {
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
    /// Phase-B observability: `false` when the camera channel is ARMED but the
    /// frame is missing / stale / undecodable (the request then fails closed to
    /// the MRC floor). `true` when disarmed (nothing to be unhealthy about) or
    /// fresh.
    pub camera_healthy: bool,
    /// Forward distance at which the camera fusion clipped the corridor, when a
    /// detection bound it. `None` = no camera hazard bound the corridor. The
    /// clip can only SHORTEN the drivable space.
    pub camera_clip_x_m: Option<f64>,
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

const MAX_TRACKING_GAP_MS: u64 = 500;

pub struct TrackedPerceptionState {
    tracker: Option<TajTracker>,
    tracker_config: Option<TajConfig>,
    last_stamp_ms: Option<u64>,
}

impl Default for TrackedPerceptionState {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackedPerceptionState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tracker: None,
            tracker_config: None,
            last_stamp_ms: None,
        }
    }

    pub fn reset(&mut self) {
        self.tracker = None;
        self.tracker_config = None;
        self.last_stamp_ms = None;
    }
}

/// Stateless compatibility entry point.
///
/// Production callers should use [`handle_perception_tracked`] with persistent
/// state so object IDs and velocity estimates survive across scan frames.
pub fn handle_perception(req: &PerceptionRequest) -> PerceptionResponse {
    let mut state = TrackedPerceptionState::new();
    handle_perception_tracked(req, &mut state)
}

/// Stateful perception path used by the Taj service.
///
/// Tracking state is reset on malformed input, timestamp regression, excessive
/// inter-frame gaps, or a material tracker-configuration change. Every reset
/// returns to conservative first-sighting behavior with zero estimated velocity.
pub fn handle_perception_tracked(
    req: &PerceptionRequest,
    state: &mut TrackedPerceptionState,
) -> PerceptionResponse {
    // Priority 0: validate every request-level assumption before cloning the
    // scan, constructing Taj, or performing floating-point safety arithmetic.
    if validate_perception_request(req).is_err() {
        state.reset();
        return invalid_perception_response(req.camera_armed);
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

    state.last_stamp_ms = Some(req.stamp_ms);

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
    let nearest_object_m = perception
        .objects
        .iter()
        .filter(|o| {
            is_forward_object_candidate(o.pos.x_m, o.pos.y_m, req.forward_extent_m, req.lane_half_m)
        })
        .map(|o| o.pos.x_m)
        .fold(f64::INFINITY, f64::min);
    let nearest_object_m = nearest_object_m.is_finite().then_some(nearest_object_m);

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
        })
        .collect();

    PerceptionResponse {
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
        camera_healthy,
        camera_clip_x_m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request over `ranges`, with the camera channel DISARMED by default —
    /// so every pre-existing assertion still describes the lidar-only path.
    fn req(ranges: Vec<f32>) -> PerceptionRequest {
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
            camera_stamp_ms: None,
            camera_max_age_ms: DEFAULT_CAMERA_MAX_AGE_MS,
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
            detections: Vec::new(),
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
