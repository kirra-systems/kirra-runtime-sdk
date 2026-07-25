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
    TajConfig, TajPhaseA,
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
fn default_floor() -> f32 {
    0.5
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

pub fn handle_perception(req: &PerceptionRequest) -> PerceptionResponse {
    let scan = LaserScan {
        angle_min_rad: req.angle_min_rad,
        angle_increment_rad: req.angle_increment_rad,
        range_min_m: req.range_min_m,
        range_max_m: req.range_max_m,
        ranges: req.ranges.clone(),
        stamp_ms: req.stamp_ms,
    };
    // Process at the scan's own stamp → age 0; wall-clock staleness is the
    // consumer's job (the ROS node times the cap topic), keeping this
    // service stateless.
    let taj = TajPhaseA::new(TajConfig {
        forward_extent_m: req.forward_extent_m,
        ..Default::default()
    });
    let perception = taj.process(&scan, req.stamp_ms);

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
    // Fail-closed conjunction: the corridor must clear the confidence floor AND
    // the camera channel must not be faulted.
    let healthy = confidence >= req.confidence_floor && camera_healthy;

    // The nearest IN-LANE object (|y| within half a lane), as a discrete
    // clear-distance bound that complements the corridor reach.
    let nearest_object_m = perception
        .objects
        .iter()
        .filter(|o| o.pos.y_m.abs() <= req.lane_half_m && o.pos.x_m > 0.0)
        .map(|o| o.pos.x_m)
        .fold(f64::INFINITY, f64::min);
    let nearest_object_m = nearest_object_m.is_finite().then_some(nearest_object_m);

    // Clear distance = the tighter of the corridor reach and the nearest
    // in-lane object.
    // Reach is measured on the CLIPPED corridor, so a camera hazard tightens the
    // clear distance and therefore the ACD speed cap.
    let clear = corridor_reach(&corridor)
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
        vec![10.0; 300]
    }

    fn det(class: &str, near_x: f64, lat_min: f64, lat_max: f64) -> CameraDetection {
        CameraDetection {
            class: class.into(),
            near_x_m: near_x,
            lateral_min_m: lat_min,
            lateral_max_m: lat_max,
        }
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
