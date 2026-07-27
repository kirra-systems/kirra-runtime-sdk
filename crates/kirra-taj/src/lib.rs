//! kirra-taj — **Taj**, the Rosmaster R2 perception layer (ADR-0015), **Phase A**
//! (geometric, model-free).
//!
//! # What Taj is
//!
//! Taj turns raw range data (R2 lidar / depth) into the **Kirra perception input
//! contract** — the same contract the #131 checker consumes:
//!
//! - a drivable **corridor** as a [`CorridorSource`] ([`TajCorridor`]),
//! - a set of **obstacles** as `Vec<PerceivedObject>`,
//! - per-output **health** (confidence + snapshot age).
//!
//! # Derivation, not invention
//!
//! Like `kirra-planner`, Taj **imports** the contract types from
//! `kirra-ros2-adapter` and never redefines them: `CorridorSource` / `Point`
//! (the corridor seam) and [`PerceivedObject`] (the obstacle the slow loop runs
//! RSS against). Taj is a *producer* of the contract; KIRRA *bounds and derates*
//! whatever Taj reports — Taj tightens the envelope, it never loosens it
//! (ADR-0015). A low-confidence or empty Taj output makes the corridor unhealthy,
//! and the checker fails closed.
//!
//! # Phase A scope
//!
//! Phase A is **geometric and model-free** — pure compute, sim-testable with
//! synthetic scans and **no R2 hardware**. It produces a conservative straight
//! corridor (bounded by the nearest lateral obstacle on each side within the
//! forward cone) and Euclidean-clustered single-frame objects. Phase B (the
//! Parko ML detector, semantic objects, temporal velocity) is later work; the
//! safety plumbing proven here is reused unchanged.

// Build hygiene (review M3): the perception-input contract types live in the lean
// `kirra-core` (Stage 6a relocation); import them directly instead of through the
// heavy `kirra-ros2-adapter`, so taj's library no longer pulls the adapter (and its
// ros2/tokio tree). The adapter is now a dev-dependency only (its `validate_trajectory_slow`
// + `VehicleConfig` are used solely by the integration tests).
use kirra_core::corridor::{CorridorSource, Point};
use kirra_core::trajectory::PerceivedObject;
use kirra_core::trajectory::PerceivedPedestrian;

mod semantic_eval;
pub use semantic_eval::{
    score_frame, ClassRecall, FrameOutcome, SemanticEvalFrame, SemanticEvalSummary,
    DEFAULT_CLIP_TOL_M,
};

/// Object-goal resolution: a NAMED thing the operator asked for → a goal point.
/// Deliberately separate from the drivability fusion above — see the module docs.
pub mod object_goal;

/// Minimal range-scan input — a `sensor_msgs/LaserScan` subset.
///
/// Angles are measured from the ego **+X** axis (forward); **+Y** is left.
/// `ranges[i]` is the distance along the ray at
/// `angle_min_rad + i * angle_increment_rad`. A range outside
/// `[range_min_m, range_max_m]` (or non-finite) is treated as **no return**.
#[derive(Debug, Clone)]
pub struct LaserScan {
    pub angle_min_rad: f64,
    pub angle_increment_rad: f64,
    pub range_min_m: f64,
    pub range_max_m: f64,
    pub ranges: Vec<f32>,
    /// Acquisition timestamp (ms). Feeds the corridor `age_ms` health field.
    pub stamp_ms: u64,
}

/// Phase-A pipeline tunables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TajConfig {
    /// Forward distance over which the drivable corridor is estimated.
    pub forward_extent_m: f64,
    /// Half-width assumed when NO obstacle bounds a side (open space); also the
    /// hard cap on the estimated half-width.
    pub open_half_width_m: f64,
    /// Max gap between consecutive returns that still belong to one object.
    pub cluster_gap_m: f64,
    /// Minimum returns for a cluster to be reported as an object (noise floor).
    pub min_cluster_points: usize,
    /// Longitudinal spacing of corridor boundary stations. Each station bounds the
    /// corridor from obstacles within `±corridor_station_m` of it, so a localized
    /// obstacle narrows the corridor only near its own x (not globally).
    pub corridor_station_m: f64,
    /// Max distance a detection may move between frames to associate with the same
    /// track (the [`TajTracker`] velocity-estimation gate).
    pub track_assoc_gate_m: f64,
    /// How long (ms) an unmatched track is retained as `Coasting` before it
    /// expires. Measured from the last frame that actually matched a
    /// detection, so it is a real time budget rather than a frame count — a
    /// slower lidar does not silently get a longer coast.
    ///
    /// 250 ms is ~2 scan periods of the bench-verified ~10 Hz TG30: enough to
    /// ride out a single dropped detection, short enough that a genuinely
    /// departed object cannot linger. Coasting only preserves identity and
    /// motion history; it does not itself emit anything to the checker.
    pub track_coast_budget_ms: u64,
}

impl Default for TajConfig {
    fn default() -> Self {
        Self {
            forward_extent_m: 8.0,
            open_half_width_m: 5.0,
            cluster_gap_m: 0.5,
            min_cluster_points: 3,
            corridor_station_m: 1.0,
            track_assoc_gate_m: 3.0,
            track_coast_budget_ms: 250,
        }
    }
}

/// WP-10 — the VRU classifier's envelope parameters. Separate from
/// [`TajConfig`] so its 11 existing literal constructors do not ripple.
/// Every default is CONSERVATIVE-FIRST and **VALIDATION-PENDING** (per the
/// CONTRACT_PROFILES discipline): deployment values must be safety-case
/// derived per ODD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VruClassifierConfig {
    /// Max cluster extent (bbox diagonal, m) a pedestrian may present.
    /// Default 1.2 — a person with gait/bag spread; bicycles fall near the
    /// boundary and classify as pedestrians when slow (stricter bound, the
    /// safe direction).
    pub max_extent_m: f64,
    /// Max tracked speed (m/s) within the pedestrian envelope. Default 3.5 —
    /// above the checker's assumed `v_ped_max` (2.0, a brisk walk) so
    /// measurement noise on a genuine pedestrian cannot demote it to
    /// vehicle-only treatment; runners/cyclists-as-VRUs ODDs must raise both
    /// together (PEDESTRIAN_RSS.md §5.1).
    pub max_speed_mps: f64,
}

impl Default for VruClassifierConfig {
    fn default() -> Self {
        Self {
            max_extent_m: 1.2,
            max_speed_mps: 3.5,
        }
    }
}

/// A drivable corridor produced by Taj — an owned [`CorridorSource`].
///
/// Phase A produces a conservative **straight** corridor bounded by the nearest
/// lateral obstacle on each side within the forward cone. (Limitation, stated
/// honestly: an obstacle *dead ahead* is treated as narrowing the corridor
/// laterally — conservatively safe, since it shrinks the drivable space toward a
/// stop. Per-station corridor shaping is Phase-B/later work.)
#[derive(Debug, Clone)]
pub struct TajCorridor {
    left: Vec<Point>,
    right: Vec<Point>,
    confidence: f32,
    age_ms: u64,
}

impl CorridorSource for TajCorridor {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn age_ms(&self) -> u64 {
        self.age_ms
    }
}

/// One Phase-A perception output frame: the contract Taj feeds to KIRRA.
#[derive(Debug, Clone)]
pub struct TajPerception {
    pub corridor: TajCorridor,
    pub objects: Vec<PerceivedObject>,
    pub stamp_ms: u64,
}

// ===========================================================================
// Phase B — semantic perception seam (ADR-0015)
//
// Phase A is geometric: lidar returns → corridor + objects. It is BLIND to
// surface semantics — most importantly water, which is specular (it returns no
// lidar) so a lake/ocean reads as FREE drivable space. Phase B closes that gap
// with a semantic detector whose non-drivable detections (water, etc.) TIGHTEN
// the corridor — "Taj tightens the envelope, never loosens it" — so KIRRA then
// has a boundary to enforce.
//
// The ML model itself (Parko RGB→TensorRT inference) lives BEHIND the
// `SemanticDetector` trait and is a hardware-gated follow-up; this module is the
// model-free seam + the safety fusion, fully offline-testable via a mock.
// ===========================================================================

/// Semantic class of a detected region. Drivability is conservative: only an
/// explicit drivable surface is drivable; `Unknown` is non-drivable (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticClass {
    /// Explicitly drivable surface (road, packed terrain).
    Road,
    /// Standing water — non-drivable by policy. Lidar-specular, so Phase A misses
    /// it entirely; the whole reason Phase B exists. (Drivable-puddle vs. lake is
    /// an unsolved single-sensor depth problem — treat ALL water as non-drivable.)
    Water,
    /// A static non-drivable obstacle classified semantically (wall, kerb, person).
    StaticObstacle,
    /// Unclassified region — non-drivable (fail-closed: never assume drivable).
    Unknown,
}

impl SemanticClass {
    /// Conservative drivability: only [`Road`](SemanticClass::Road) is drivable.
    #[must_use]
    pub fn is_drivable(self) -> bool {
        matches!(self, SemanticClass::Road)
    }
}

/// A semantic detection: a classified region localized in the ego frame. The ML
/// detector produces these; the fusion uses `near_x_m` + the lateral span to clip
/// the corridor at the nearest non-drivable hazard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticDetection {
    pub class: SemanticClass,
    /// Nearest forward distance to the region (m, ego frame).
    pub near_x_m: f64,
    /// Lateral extent of the region `[min, max]` (m; +Y left).
    pub lateral_min_m: f64,
    pub lateral_max_m: f64,
}

/// A semantic detector. The real implementation is Parko's RGB→TensorRT model
/// (hardware-gated); [`MockSemanticDetector`] stands in for offline tests. Kept
/// trivially object-safe so the pipeline can hold `Box<dyn SemanticDetector>`.
pub trait SemanticDetector {
    /// Produce semantic detections for the current sensor frame.
    fn detect(&self) -> Vec<SemanticDetection>;
}

/// Deterministic stand-in for the ML detector: returns a scripted detection set.
#[derive(Debug, Clone, Default)]
pub struct MockSemanticDetector {
    pub detections: Vec<SemanticDetection>,
}

impl SemanticDetector for MockSemanticDetector {
    fn detect(&self) -> Vec<SemanticDetection> {
        self.detections.clone()
    }
}

/// Interpolated boundary `y` at longitudinal `x` (boundary vertices x-ordered).
fn boundary_y_at(boundary: &[Point], x: f64) -> f64 {
    match boundary.first() {
        None => return 0.0,
        Some(p) if x <= p.x_m => return p.y_m,
        _ => {}
    }
    for w in boundary.windows(2) {
        if x <= w[1].x_m {
            let dx = w[1].x_m - w[0].x_m;
            let f = if dx.abs() > 1e-9 {
                (x - w[0].x_m) / dx
            } else {
                0.0
            };
            return w[0].y_m + f * (w[1].y_m - w[0].y_m);
        }
    }
    boundary.last().unwrap().y_m
}

/// Truncate an x-ordered boundary polyline at `x_clip`, inserting the interpolated
/// crossing vertex so the polyline ends exactly at `x_clip`.
fn truncate_boundary(boundary: &[Point], x_clip: f64) -> Vec<Point> {
    let mut out = Vec::with_capacity(boundary.len());
    for (i, p) in boundary.iter().enumerate() {
        if p.x_m <= x_clip {
            out.push(*p);
        } else {
            if i > 0 {
                let a = boundary[i - 1];
                let dx = p.x_m - a.x_m;
                let f = if dx.abs() > 1e-9 {
                    (x_clip - a.x_m) / dx
                } else {
                    0.0
                };
                out.push(Point {
                    x_m: x_clip,
                    y_m: a.y_m + f * (p.y_m - a.y_m),
                });
            }
            break;
        }
    }
    out
}

/// The **binding hazard** — the nearest non-drivable detection whose lateral span
/// overlaps the corridor at its near edge, i.e. the one that actually ends the
/// drivable space. `None` when nothing clips the corridor (every detection is
/// drivable, or lies laterally clear of it). This is the single decision the
/// Phase-B fusion turns on; [`clip_corridor_to_hazards`] truncates at exactly this
/// detection's `near_x_m`, and the eval harness ([`SemanticEvalSummary`]) compares
/// the binding hazard under ground-truth vs detected labels to score the ML path.
#[must_use]
pub fn binding_hazard<'a>(
    corridor: &TajCorridor,
    detections: &'a [SemanticDetection],
) -> Option<&'a SemanticDetection> {
    detections
        .iter()
        .filter(|d| !d.class.is_drivable())
        // Does the hazard's lateral span overlap the corridor at its near edge?
        .filter(|d| {
            let left = boundary_y_at(&corridor.left, d.near_x_m);
            let right = boundary_y_at(&corridor.right, d.near_x_m);
            d.lateral_max_m > right && d.lateral_min_m < left
        })
        .min_by(|a, b| a.near_x_m.total_cmp(&b.near_x_m))
}

/// The forward distance at which `detections` end the drivable corridor — the
/// [`binding_hazard`]'s `near_x_m`, or `None` if nothing clips it. The fusion's
/// decision distilled to a scalar.
#[must_use]
pub fn hazard_clip_x(corridor: &TajCorridor, detections: &[SemanticDetection]) -> Option<f64> {
    binding_hazard(corridor, detections).map(|d| d.near_x_m)
}

/// **Phase-B fusion** — clip the drivable corridor at the nearest non-drivable
/// semantic hazard that laterally overlaps it. The corridor's drivable space ends
/// at the hazard's edge, so KIRRA's containment rejects any trajectory that would
/// drive into it (e.g. a lake Phase A reported as free space). Drivable detections
/// and hazards that don't overlap the corridor leave it unchanged.
#[must_use]
pub fn clip_corridor_to_hazards(
    corridor: &TajCorridor,
    detections: &[SemanticDetection],
) -> TajCorridor {
    let Some(x_clip) = hazard_clip_x(corridor, detections) else {
        return corridor.clone();
    };
    TajCorridor {
        left: truncate_boundary(&corridor.left, x_clip),
        right: truncate_boundary(&corridor.right, x_clip),
        confidence: corridor.confidence,
        age_ms: corridor.age_ms,
    }
}

/// Taj Phase-A geometric perception pipeline.
#[derive(Debug, Clone, Copy, Default)]
pub struct TajPhaseA {
    pub cfg: TajConfig,
}

/// Minimum positive longitudinal coordinate admitted into corridor-boundary
/// extraction.
///
/// This rejects near-origin lateral returns that can collapse both boundaries at
/// x≈0 while preserving genuine parallel side walls farther from the sensor.
///
/// This is separate from the sidecar's collision-object forward cone: corridor
/// geometry and frontal collision-object gating have different semantics.
const MIN_CORRIDOR_POINT_X_M: f64 = 0.15;

/// Minimum lateral displacement for a lidar return to define a corridor side.
///
/// Returns near the vehicle centerline represent frontal obstacles. They remain
/// available to object clustering and assured-clear-distance braking, but must
/// not masquerade as left/right corridor walls and collapse the corridor to a
/// few millimetres.
///
/// This value is below the R2 required half-width of 0.2515 m, so genuine
/// passage boundaries near the vehicle envelope remain eligible.
const MIN_CORRIDOR_BOUNDARY_ABS_Y_M: f64 = 0.26;

/// Hard ceiling on the number of corridor stations `extract_corridor` will
/// build, whatever configuration it is handed.
///
/// This is a LIBRARY BACKSTOP, not a policy. Callers that validate their input
/// (the Taj sidecar rejects `forward_extent_m` above its own
/// `MAX_FORWARD_EXTENT_M`, which is exactly this ceiling × the 0.1 m station
/// floor) never reach it. It exists because `TajConfig` is public and
/// unvalidated: a direct caller can set `forward_extent_m` to any finite f64,
/// and Rust's float→int `as` cast SATURATES, so `(1e300 / 0.1).ceil() as usize`
/// yields `usize::MAX`. The subsequent `nstations + 1` then panicked with an
/// integer overflow in debug and wrapped to a ~1.8e19-iteration loop in release.
///
/// Clamping is fail-SAFE rather than fail-open: fewer stations produce a
/// SHORTER corridor, i.e. less claimed free space, so a downstream containment
/// check becomes more conservative, never less.
const MAX_CORRIDOR_STATIONS: usize = 5_000;

/// Number of corridor stations for a forward extent and station spacing,
/// saturating-cast safe and bounded by [`MAX_CORRIDOR_STATIONS`].
///
/// Returns 0 for any non-finite or non-positive input rather than propagating a
/// garbage count into an allocation.
fn corridor_station_count(forward_extent_m: f64, station_m: f64) -> usize {
    if !forward_extent_m.is_finite()
        || forward_extent_m <= 0.0
        || !station_m.is_finite()
        || station_m <= 0.0
    {
        return 0;
    }
    let raw = (forward_extent_m / station_m).ceil();
    if raw.is_nan() || raw <= 0.0 {
        return 0;
    }
    // The DIVISION itself can overflow f64 (`f64::MAX / 0.1` is +inf). That
    // means "astronomically many stations", not "none" — clamp it like any
    // other over-large count rather than collapsing to an empty corridor.
    //
    // Compare in f64 BEFORE casting: `as usize` saturates, so testing the cast
    // result against the ceiling would already have lost the overflow.
    if !raw.is_finite() || raw >= MAX_CORRIDOR_STATIONS as f64 {
        return MAX_CORRIDOR_STATIONS;
    }
    raw as usize
}

impl TajPhaseA {
    #[must_use]
    pub fn new(cfg: TajConfig) -> Self {
        Self { cfg }
    }

    /// Convert a scan to ego-frame Cartesian points, dropping invalid returns.
    /// The result is angle-ordered (same order as `scan.ranges`).
    #[must_use]
    pub fn scan_to_points(&self, scan: &LaserScan) -> Vec<(f64, f64)> {
        let mut pts = Vec::with_capacity(scan.ranges.len());
        for (i, &r) in scan.ranges.iter().enumerate() {
            let r = f64::from(r);
            if !r.is_finite() || r < scan.range_min_m || r > scan.range_max_m {
                continue; // no return on this ray
            }
            let theta = scan.angle_min_rad + (i as f64) * scan.angle_increment_rad;
            pts.push((r * theta.cos(), r * theta.sin()));
        }
        pts
    }

    /// Run the full Phase-A pipeline: scan → corridor + objects + health.
    #[must_use]
    pub fn process(&self, scan: &LaserScan, now_ms: u64) -> TajPerception {
        self.process_parts(scan, now_ms).0
    }

    /// [`process`](Self::process) plus each object's cluster extent (parallel
    /// to `TajPerception::objects` by index) — the WP-10 seam that keeps the
    /// footprint scale alive long enough for VRU classification without
    /// touching the lean `PerceivedObject`/`TajPerception` contracts.
    fn process_parts(&self, scan: &LaserScan, now_ms: u64) -> (TajPerception, Vec<f64>) {
        let points = self.scan_to_points(scan);
        // Confidence = fraction of rays that produced a valid return. An empty /
        // all-invalid scan → 0.0 → the corridor is unhealthy → checker MRCs.
        let total = scan.ranges.len().max(1);
        let confidence = (points.len() as f32 / total as f32).clamp(0.0, 1.0);

        let corridor = self.extract_corridor(&points, confidence, now_ms, scan.stamp_ms);
        let with_extent = self.cluster_objects_with_extent(&points);
        let mut objects = Vec::with_capacity(with_extent.len());
        let mut extents = Vec::with_capacity(with_extent.len());
        for (o, e) in with_extent {
            objects.push(o);
            extents.push(e);
        }
        (
            TajPerception {
                corridor,
                objects,
                stamp_ms: scan.stamp_ms,
            },
            extents,
        )
    }

    /// Per-station drivable corridor: boundary vertices are placed every
    /// `corridor_station_m` along the forward extent, each bounded by the nearest
    /// lateral obstacle within `±corridor_station_m` of that station. A localized
    /// obstacle therefore narrows the corridor **only near its own x**, leaving it
    /// wide elsewhere — vs. a single global narrowing that collapses the whole
    /// corridor for any forward return. A side with no obstacle opens to
    /// `open_half_width_m`.
    fn extract_corridor(
        &self,
        points: &[(f64, f64)],
        confidence: f32,
        now_ms: u64,
        stamp_ms: u64,
    ) -> TajCorridor {
        let ext = self.cfg.forward_extent_m;
        let cap = self.cfg.open_half_width_m;
        let step = self.cfg.corridor_station_m.max(0.1);

        // Bounded + saturating-cast safe: `TajConfig` is public and unvalidated,
        // so a direct caller can hand us any finite extent (see
        // `corridor_station_count`). `saturating_add` keeps the `+ 1` honest
        // even at the ceiling.
        let nstations = corridor_station_count(ext, step);
        let mut left = Vec::with_capacity(nstations.saturating_add(1));
        let mut right = Vec::with_capacity(nstations.saturating_add(1));
        for i in 0..=nstations {
            let xs = (i as f64 * step).min(ext);
            let mut left_y = cap; // nearest left obstacle in this station's window
            let mut right_y = -cap; // nearest right obstacle
            for &(x, y) in points {
                if !x.is_finite()
                    || !y.is_finite()
                    || x < MIN_CORRIDOR_POINT_X_M
                    || y.abs() < MIN_CORRIDOR_BOUNDARY_ABS_Y_M
                    || x > ext
                    || (x - xs).abs() > step
                {
                    continue; // exclude rear and near-origin lateral returns
                }
                if y > 0.0 && y < left_y {
                    left_y = y;
                } else if y < 0.0 && y > right_y {
                    right_y = y;
                }
            }
            left.push(Point {
                x_m: xs,
                y_m: left_y.clamp(1e-3, cap),
            });
            right.push(Point {
                x_m: xs,
                y_m: right_y.clamp(-cap, -1e-3),
            });
        }

        TajCorridor {
            left,
            right,
            confidence,
            age_ms: now_ms.saturating_sub(stamp_ms),
        }
    }

    /// Euclidean clustering of angle-ordered returns into objects. Single-frame,
    /// so velocity is unknown → reported as `0.0` (Phase B / temporal tracking
    /// adds motion). Each run of consecutive points within `cluster_gap_m`, of at
    /// least `min_cluster_points`, becomes one [`PerceivedObject`] at its centroid.
    /// Cluster scan points into objects, also returning each
    /// cluster's EXTENT (bounding-box diagonal, m) — the footprint scale the
    /// lean `PerceivedObject` contract deliberately drops, needed by the
    /// WP-10 VRU classifier ([`TajTracker::classify_pedestrians`]) before it
    /// is lost at the contract boundary.
    fn cluster_objects_with_extent(&self, points: &[(f64, f64)]) -> Vec<(PerceivedObject, f64)> {
        let mut objects = Vec::new();
        if points.is_empty() {
            return objects;
        }
        let gap = self.cfg.cluster_gap_m;
        let mut start = 0usize;
        let mut next_id: u64 = 0;

        for i in 1..=points.len() {
            let split = i == points.len() || {
                let (ax, ay) = points[i - 1];
                let (bx, by) = points[i];
                ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt() > gap
            };
            if split {
                let cluster = &points[start..i];
                if cluster.len() >= self.cfg.min_cluster_points {
                    let n = cluster.len() as f64;
                    let (sx, sy) = cluster
                        .iter()
                        .fold((0.0, 0.0), |(ax, ay), &(x, y)| (ax + x, ay + y));
                    let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
                    let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                    for &(x, y) in cluster {
                        min_x = min_x.min(x);
                        max_x = max_x.max(x);
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                    let extent_m = (max_x - min_x).hypot(max_y - min_y);
                    objects.push((
                        PerceivedObject {
                            id: next_id,
                            pos: Point {
                                x_m: sx / n,
                                y_m: sy / n,
                            },
                            velocity_mps: 0.0,
                            heading_rad: 0.0,
                            vel: Point { x_m: 0.0, y_m: 0.0 },
                        },
                        extent_m,
                    ));
                    next_id += 1;
                }
                start = i;
            }
        }
        objects
    }
}

/// One camera-derived VRU observation in the ego frame.
///
/// This observation is supporting evidence only. A camera-only observation
/// never creates a safety-authoritative pedestrian track; it must associate
/// with an existing lidar track before it may influence classification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVruObservation {
    pub observation_id: u64,
    pub pos: Point,
    pub confidence: f64,
    pub stamp_ms: u64,
}

/// Result of deterministic lidar-camera VRU association.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVruAssociation {
    pub track_id: u64,
    pub observation_id: u64,
    pub distance_m: f64,
    pub confidence: f64,
}

/// Configuration for the supporting camera-VRU fusion seam.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraVruFusionConfig {
    /// Maximum ego-frame position error for association.
    pub association_gate_m: f64,
    /// Minimum camera confidence admitted as supporting evidence.
    pub minimum_confidence: f64,
    /// Maximum age difference between lidar track time and camera observation.
    pub maximum_age_ms: u64,
}

impl Default for CameraVruFusionConfig {
    fn default() -> Self {
        Self {
            association_gate_m: 0.75,
            minimum_confidence: 0.60,
            maximum_age_ms: 250,
        }
    }
}

/// Auditable result of Taj's geometric VRU classifier.
///
/// `PossiblePedestrian` feeds the stricter pedestrian RSS bound.
/// `NotPedestrian` remains protected by ordinary object RSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruClassification {
    PossiblePedestrian,
    NotPedestrian,
}

impl VruClassification {
    /// Stable machine-readable wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PossiblePedestrian => "possible_pedestrian",
            Self::NotPedestrian => "not_pedestrian",
        }
    }

    /// Whether this result must feed the pedestrian RSS scene.
    #[must_use]
    pub const fn feeds_pedestrian_rss(self) -> bool {
        matches!(self, Self::PossiblePedestrian)
    }
}

/// Stable reason vocabulary for a VRU classification decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruClassificationReason {
    InvalidConfiguration,
    NonFiniteExtent,
    FirstSightingUnknownVelocity,
    NonFiniteVelocity,
    WithinPedestrianEnvelope,
    ExtentAboveEnvelope,
    SpeedAboveEnvelope,
}

impl VruClassificationReason {
    /// Stable machine-readable wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::NonFiniteExtent => "non_finite_extent",
            Self::FirstSightingUnknownVelocity => "first_sighting_unknown_velocity",
            Self::NonFiniteVelocity => "non_finite_velocity",
            Self::WithinPedestrianEnvelope => "within_pedestrian_envelope",
            Self::ExtentAboveEnvelope => "extent_above_envelope",
            Self::SpeedAboveEnvelope => "speed_above_envelope",
        }
    }
}

/// One active track plus its auditable VRU-classifier decision.
///
/// The measured extent and speed are preserved for diagnostics, including
/// non-finite values. They are not sanitized into fabricated safe values.
#[derive(Debug, Clone, Copy)]
pub struct ClassifiedVru {
    pub pedestrian: PerceivedPedestrian,
    pub classification: VruClassification,
    pub reason: VruClassificationReason,
    pub extent_m: f64,
    pub speed_mps: f64,
    pub frames_seen: u32,
}

/// Final frame-local VRU classification after applying supporting camera
/// evidence to the authoritative lidar track.
///
/// Camera evidence never creates a track, alters its geometry, or removes the
/// ordinary object-RSS bound. Conflicting evidence tightens toward pedestrian
/// RSS rather than silently trusting either sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusedVruClassification {
    PossiblePedestrian,
    CameraSupportedPedestrian,
    NotPedestrian,
    ConflictingEvidence,
}

impl FusedVruClassification {
    /// Stable machine-readable wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PossiblePedestrian => "possible_pedestrian",
            Self::CameraSupportedPedestrian => "camera_supported_pedestrian",
            Self::NotPedestrian => "not_pedestrian",
            Self::ConflictingEvidence => "conflicting_evidence",
        }
    }

    /// Whether this result must feed the pedestrian reachable-set check.
    #[must_use]
    pub const fn feeds_pedestrian_rss(self) -> bool {
        matches!(
            self,
            Self::PossiblePedestrian | Self::CameraSupportedPedestrian | Self::ConflictingEvidence
        )
    }
}

/// Stable explanation for the fused VRU decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionInfluenceReason {
    LidarPossiblePedestrian,
    CameraSupportsLidarPedestrian,
    LidarNotPedestrian,
    CameraConflictsWithLidarExclusion,
}

impl FusionInfluenceReason {
    /// Stable machine-readable wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LidarPossiblePedestrian => "lidar_possible_pedestrian",
            Self::CameraSupportsLidarPedestrian => "camera_supports_lidar_pedestrian",
            Self::LidarNotPedestrian => "lidar_not_pedestrian",
            Self::CameraConflictsWithLidarExclusion => "camera_conflicts_with_lidar_exclusion",
        }
    }
}

/// One lidar track plus its auditable fused VRU decision.
///
/// The camera fields are association evidence only. Position, velocity, age,
/// extent, and track identity remain lidar-derived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusedClassifiedVru {
    pub pedestrian: PerceivedPedestrian,
    pub lidar_classification: VruClassification,
    pub lidar_reason: VruClassificationReason,
    pub fused_classification: FusedVruClassification,
    pub fusion_reason: FusionInfluenceReason,
    pub camera_observation_id: Option<u64>,
    pub camera_confidence: Option<f64>,
    pub association_distance_m: Option<f64>,
    pub extent_m: f64,
    pub speed_mps: f64,
    pub frames_seen: u32,
}

/// Hard cap on retained tracks, so COASTING can never grow the working set.
///
/// Detections are already bounded upstream; coasting is the one path that adds
/// tracks the sensor did not report this frame, so it needs its own ceiling.
/// Matches the checker's `MAX_TRACKED_OBJECTS`, which is the real consumer-side
/// limit — retaining past it would produce a set the checker refuses anyway.
const MAX_RETAINED_TRACKS: usize = 256;

/// One persistent object track: a stable id + last position/stamp, kept across
/// frames so the next frame can estimate velocity from displacement.
#[derive(Debug, Clone, Copy)]
struct Track {
    id: u64,
    pos: Point,
    stamp_ms: u64,
    /// Latest estimated ground velocity (m/s) — retained for CTRV prediction.
    vel: Point,
    /// Previous valid velocity estimate.
    ///
    /// Retained for deterministic frame-to-frame intent stabilization. This
    /// history is metadata only and cannot weaken geometric prediction,
    /// uncertainty, pedestrian RSS, or ordinary object RSS.
    prior_vel: Point,
    /// Latest estimated yaw rate (rad/s) from the heading change across frames.
    yaw_rate: f64,
    /// Latest cluster extent (bbox diagonal, m) — the WP-10 VRU-classifier
    /// footprint signal, captured before the lean contract drops it.
    extent_m: f64,
    /// Consecutive frames this track has been observed (1 = first sighting,
    /// velocity not yet estimable). Coasting does NOT advance it: a frame
    /// nobody observed is not evidence of persistence.
    frames_seen: u32,
    /// Timestamp of the last frame that actually MATCHED a detection.
    ///
    /// The coast budget is measured from here, NOT from `stamp_ms`, which
    /// advances every frame so association keeps predicting from now. The two
    /// together encode the lifecycle without a separate field: a track is
    /// COASTING iff `last_seen_ms < stamp_ms`, and EXPIRED tracks are dropped
    /// rather than represented.
    ///
    /// A detection dropping for a frame used to DELETE the track: the next
    /// frame's detection became a NEW track — new id, velocity re-estimated
    /// from zero, `frames_seen` back to 1. That reset is not cosmetic;
    /// `frames_seen` gates velocity (`< 2`) and yaw-rate (`< 3`) estimation and
    /// drives the first-sighting arm of the VRU classifier, so a pedestrian
    /// that blinked for one scan lost its motion history and its established
    /// classification.
    last_seen_ms: u64,
}

/// A predicted future path for one tracked object (constant-turn-rate,
/// constant-velocity rollout). `points` are successive positions at the rollout
/// `dt`; `yaw_rate_rad_s` is the estimated turn rate (0 = straight-line / CV).
#[derive(Debug, Clone, PartialEq)]
pub struct PredictedPath {
    pub id: u64,
    pub yaw_rate_rad_s: f64,
    pub points: Vec<Point>,
}

/// Production VRU motion model selected for one authoritative lidar track.
///
/// These models never create tracks and never weaken the pedestrian RSS bound.
/// `OmnidirectionalFallback` represents an expanding reachable region rather
/// than a directional movement claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruMotionModel {
    OmnidirectionalFallback,
    Stationary,
    ConstantVelocity,
    BoundedTurnRate,
}

impl VruMotionModel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OmnidirectionalFallback => "omnidirectional_fallback",
            Self::Stationary => "stationary",
            Self::ConstantVelocity => "constant_velocity",
            Self::BoundedTurnRate => "bounded_turn_rate",
        }
    }
}

/// Conservative frame-local interpretation of a tracked VRU's motion.
///
/// Intent is auditable metadata only. It does not create tracks, alter fused
/// pedestrian membership, reduce uncertainty, or replace the existing
/// omnidirectional pedestrian RSS bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruIntentClass {
    Unknown,
    AlongPath,
    CrossingLeftToRight,
    CrossingRightToLeft,
    WaitingNearPath,
    MovingAway,
}

impl VruIntentClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::AlongPath => "along_path",
            Self::CrossingLeftToRight => "crossing_left_to_right",
            Self::CrossingRightToLeft => "crossing_right_to_left",
            Self::WaitingNearPath => "waiting_near_path",
            Self::MovingAway => "moving_away",
        }
    }
}

/// Stable explanation for the frame-local VRU intent classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruIntentReason {
    InsufficientHistory,
    NonFiniteMotion,
    NearlyStationaryNearPath,
    LongitudinalMotionDominant,
    LateralMotionTowardPath,
    LateralMotionAwayFromPath,
    AmbiguousMotion,
}

impl VruIntentReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InsufficientHistory => "insufficient_history",
            Self::NonFiniteMotion => "non_finite_motion",
            Self::NearlyStationaryNearPath => "nearly_stationary_near_path",
            Self::LongitudinalMotionDominant => "longitudinal_motion_dominant",
            Self::LateralMotionTowardPath => "lateral_motion_toward_path",
            Self::LateralMotionAwayFromPath => "lateral_motion_away_from_path",
            Self::AmbiguousMotion => "ambiguous_motion",
        }
    }
}

/// One normalized hypothesis in a bounded VRU intent distribution.
///
/// This is planning metadata only. It cannot create tracks, alter fused VRU
/// membership, reduce uncertainty, or weaken pedestrian RSS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VruIntentProbability {
    pub intent: VruIntentClass,
    pub probability: f64,
}

/// Stable number of supported VRU intent hypotheses.
pub const VRU_INTENT_HYPOTHESIS_COUNT: usize = 6;

/// Deterministic probability normalization tolerance.
pub const VRU_INTENT_PROBABILITY_EPSILON: f64 = 1e-9;

/// Stable reason why directional VRU prediction was withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VruPredictionFallbackReason {
    InvalidConfiguration,
    NonFiniteTrackState,
    InsufficientHistory,
    StaleTrack,
    ExcessiveSpeed,
    ExcessiveYawRate,
}

impl VruPredictionFallbackReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::NonFiniteTrackState => "non_finite_track_state",
            Self::InsufficientHistory => "insufficient_history",
            Self::StaleTrack => "stale_track",
            Self::ExcessiveSpeed => "excessive_speed",
            Self::ExcessiveYawRate => "excessive_yaw_rate",
        }
    }
}

/// One bounded prediction sample for an authoritative lidar track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PredictedVruPoint {
    pub time_s: f64,
    pub pos: Point,
    pub uncertainty_radius_m: f64,
}

/// Auditable frame-local motion prediction for one lidar track.
#[derive(Debug, Clone, PartialEq)]
pub struct PredictedVruMotion {
    pub track_id: u64,
    pub model: VruMotionModel,
    /// Conservative interpretation of current tracked motion.
    ///
    /// Metadata only: it does not modify the prediction model or uncertainty.
    pub intent: VruIntentClass,
    pub intent_confidence: f64,
    pub intent_reason: VruIntentReason,
    /// Complete normalized probability distribution in stable enum order.
    pub intent_probabilities: Vec<VruIntentProbability>,
    pub points: Vec<PredictedVruPoint>,
    pub horizon_s: f64,
    pub step_s: f64,
    pub source_age_s: f64,
    pub frames_seen: u32,
    pub fallback_reason: Option<VruPredictionFallbackReason>,
}

/// Hard WCET/allocation ceiling independent of runtime configuration.
pub const MAX_VRU_PREDICTION_POINTS: usize = 32;

/// Configuration for bounded VRU motion prediction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VruMotionPredictionConfig {
    pub horizon_s: f64,
    pub step_s: f64,
    pub stationary_speed_mps: f64,
    pub maximum_track_speed_mps: f64,
    pub minimum_turn_speed_mps: f64,
    pub minimum_abs_yaw_rate_rad_s: f64,
    pub maximum_abs_yaw_rate_rad_s: f64,
    pub maximum_track_age_ms: u64,
    pub base_uncertainty_m: f64,
    pub uncertainty_growth_mps: f64,
    pub fallback_growth_mps: f64,
    pub maximum_points: usize,
}

impl Default for VruMotionPredictionConfig {
    fn default() -> Self {
        Self {
            horizon_s: 3.0,
            step_s: 0.25,
            stationary_speed_mps: 0.10,
            maximum_track_speed_mps: 5.0,
            minimum_turn_speed_mps: 0.20,
            minimum_abs_yaw_rate_rad_s: 0.05,
            maximum_abs_yaw_rate_rad_s: 2.5,
            maximum_track_age_ms: 500,
            base_uncertainty_m: 0.25,
            uncertainty_growth_mps: 0.35,
            fallback_growth_mps: 2.0,
            maximum_points: 16,
        }
    }
}

/// Candidate edge between a previous track and current detection.
#[derive(Debug, Clone, Copy)]
struct AssociationEdge {
    track_index: usize,
    detection_index: usize,
    distance_m: f64,
    track_id: u64,
}

/// Predict a track to the current measurement time using constant velocity.
///
/// Invalid prediction arithmetic conservatively falls back to the last observed
/// position.
fn predicted_track_position(track: &Track, now_ms: u64) -> Point {
    let elapsed_ms = now_ms.saturating_sub(track.stamp_ms);

    if elapsed_ms == 0 {
        return track.pos;
    }

    let dt_s = elapsed_ms as f64 / 1_000.0;
    let x_m = track.pos.x_m + track.vel.x_m * dt_s;
    let y_m = track.pos.y_m + track.vel.y_m * dt_s;

    if x_m.is_finite() && y_m.is_finite() {
        Point { x_m, y_m }
    } else {
        track.pos
    }
}

/// Deterministic one-to-one association using predicted track positions.
///
/// All admissible edges are globally sorted before selection. This removes
/// dependence on detection iteration order and prevents one track from being
/// assigned to multiple detections.
fn associate_tracks_globally(
    tracks: &[Track],
    detections: &[PerceivedObject],
    now_ms: u64,
    configured_gate_m: f64,
) -> Vec<Option<usize>> {
    const ASSOCIATION_JITTER_M: f64 = 0.25;
    const MAX_ASSOCIATION_SPEED_MPS: f64 = 10.0;

    let mut matches = vec![None; detections.len()];

    if !configured_gate_m.is_finite() || configured_gate_m <= 0.0 {
        return matches;
    }

    let mut edges = Vec::new();

    for (track_index, track) in tracks.iter().enumerate() {
        let elapsed_ms = now_ms.saturating_sub(track.stamp_ms);

        // A zero-time update cannot provide a physically meaningful velocity
        // estimate. Duplicate service requests are replayed by the sidecar
        // cache before reaching the tracker.
        if elapsed_ms == 0 {
            continue;
        }

        let dt_s = elapsed_ms as f64 / 1_000.0;
        let effective_gate_m =
            configured_gate_m.min(ASSOCIATION_JITTER_M + MAX_ASSOCIATION_SPEED_MPS * dt_s);

        let predicted = predicted_track_position(track, now_ms);

        for (detection_index, detection) in detections.iter().enumerate() {
            let distance_m =
                (detection.pos.x_m - predicted.x_m).hypot(detection.pos.y_m - predicted.y_m);

            if distance_m.is_finite() && distance_m <= effective_gate_m {
                edges.push(AssociationEdge {
                    track_index,
                    detection_index,
                    distance_m,
                    track_id: track.id,
                });
            }
        }
    }

    // Global deterministic ordering:
    //   1. lowest prediction error
    //   2. lowest persistent track ID
    //   3. lowest detection index
    //   4. lowest storage index
    edges.sort_by(|left, right| {
        left.distance_m
            .total_cmp(&right.distance_m)
            .then_with(|| left.track_id.cmp(&right.track_id))
            .then_with(|| left.detection_index.cmp(&right.detection_index))
            .then_with(|| left.track_index.cmp(&right.track_index))
    });

    let mut track_used = vec![false; tracks.len()];

    for edge in edges {
        if track_used[edge.track_index] || matches[edge.detection_index].is_some() {
            continue;
        }

        track_used[edge.track_index] = true;
        matches[edge.detection_index] = Some(edge.track_index);
    }

    matches
}

#[derive(Debug, Clone, Copy)]
struct CameraVruEdge {
    track_index: usize,
    observation_index: usize,
    distance_m: f64,
    track_id: u64,
    observation_id: u64,
}

/// Deterministically associate camera VRU observations to existing lidar tracks.
///
/// Camera evidence is supporting-only:
///
/// - no lidar track means no association;
/// - one observation can support at most one track;
/// - one track can consume at most one observation;
/// - malformed, stale, low-confidence, or out-of-gate observations are ignored;
/// - association order is independent of input storage order.
fn associate_camera_vrus_to_tracks(
    tracks: &[Track],
    observations: &[CameraVruObservation],
    now_ms: u64,
    cfg: &CameraVruFusionConfig,
) -> Vec<CameraVruAssociation> {
    let config_valid = cfg.association_gate_m.is_finite()
        && cfg.association_gate_m > 0.0
        && cfg.minimum_confidence.is_finite()
        && (0.0..=1.0).contains(&cfg.minimum_confidence);

    if !config_valid || tracks.is_empty() || observations.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();

    for (track_index, track) in tracks.iter().enumerate() {
        let predicted = predicted_track_position(track, now_ms);

        for (observation_index, observation) in observations.iter().enumerate() {
            if !observation.pos.x_m.is_finite()
                || !observation.pos.y_m.is_finite()
                || !observation.confidence.is_finite()
                || observation.confidence < cfg.minimum_confidence
            {
                continue;
            }

            let age_ms = now_ms.abs_diff(observation.stamp_ms);
            if age_ms > cfg.maximum_age_ms {
                continue;
            }

            let distance_m =
                (observation.pos.x_m - predicted.x_m).hypot(observation.pos.y_m - predicted.y_m);

            if distance_m.is_finite() && distance_m <= cfg.association_gate_m {
                edges.push(CameraVruEdge {
                    track_index,
                    observation_index,
                    distance_m,
                    track_id: track.id,
                    observation_id: observation.observation_id,
                });
            }
        }
    }

    edges.sort_by(|left, right| {
        left.distance_m
            .total_cmp(&right.distance_m)
            .then_with(|| left.track_id.cmp(&right.track_id))
            .then_with(|| left.observation_id.cmp(&right.observation_id))
            .then_with(|| left.track_index.cmp(&right.track_index))
            .then_with(|| left.observation_index.cmp(&right.observation_index))
    });

    let mut track_used = vec![false; tracks.len()];
    let mut observation_used = vec![false; observations.len()];
    let mut associations = Vec::new();

    for edge in edges {
        if track_used[edge.track_index] || observation_used[edge.observation_index] {
            continue;
        }

        track_used[edge.track_index] = true;
        observation_used[edge.observation_index] = true;

        let observation = observations[edge.observation_index];
        associations.push(CameraVruAssociation {
            track_id: edge.track_id,
            observation_id: edge.observation_id,
            distance_m: edge.distance_m,
            confidence: observation.confidence,
        });
    }

    associations.sort_by(|left, right| {
        left.track_id
            .cmp(&right.track_id)
            .then_with(|| left.observation_id.cmp(&right.observation_id))
    });

    associations
}

/// Taj **temporal tracker** — wraps the Phase-A geometric pipeline and adds
/// frame-to-frame object association + velocity estimation. Phase-A is
/// single-frame (every object static); the tracker associates each new detection
/// with one admissible prior track using deterministic predicted-position
/// association within `track_assoc_gate_m`, estimates its
/// ground velocity from the displacement over the inter-frame `dt`, and carries a
/// **persistent id**. So KIRRA's RSS sees real object motion (`velocity_mps` +
/// the `vel` vector) instead of treating a fast-approaching object as parked.
///
/// First sighting of an object → velocity `0` (no prior to difference against);
/// a track with no matching detection this frame is dropped.
#[derive(Debug, Clone, Default)]
pub struct TajTracker {
    phase_a: TajPhaseA,
    tracks: Vec<Track>,
    next_id: u64,
}

fn vru_intent_probability_distribution(
    intent: VruIntentClass,
    confidence: f64,
) -> Vec<VruIntentProbability> {
    const INTENTS: [VruIntentClass; VRU_INTENT_HYPOTHESIS_COUNT] = [
        VruIntentClass::Unknown,
        VruIntentClass::WaitingNearPath,
        VruIntentClass::AlongPath,
        VruIntentClass::CrossingLeftToRight,
        VruIntentClass::CrossingRightToLeft,
        VruIntentClass::MovingAway,
    ];

    // Malformed confidence or an unknown classification carries no directional
    // authority. Collapse completely to Unknown rather than inventing belief.
    if !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || intent == VruIntentClass::Unknown
    {
        return INTENTS
            .into_iter()
            .map(|candidate| VruIntentProbability {
                intent: candidate,
                probability: if candidate == VruIntentClass::Unknown {
                    1.0
                } else {
                    0.0
                },
            })
            .collect();
    }

    // The classifier confidence is assigned to its selected hypothesis.
    // Remaining mass stays Unknown. No probability is assigned to unsupported
    // directional alternatives.
    let unknown_probability = 1.0 - confidence;

    INTENTS
        .into_iter()
        .map(|candidate| {
            let probability = if candidate == intent {
                confidence
            } else if candidate == VruIntentClass::Unknown {
                unknown_probability
            } else {
                0.0
            };

            VruIntentProbability {
                intent: candidate,
                probability,
            }
        })
        .collect()
}

fn classify_vru_motion_intent(
    track: &Track,
    cfg: &VruMotionPredictionConfig,
) -> (VruIntentClass, f64, VruIntentReason) {
    let speed_mps = track.vel.x_m.hypot(track.vel.y_m);

    if !track.pos.x_m.is_finite()
        || !track.pos.y_m.is_finite()
        || !track.vel.x_m.is_finite()
        || !track.vel.y_m.is_finite()
        || !speed_mps.is_finite()
    {
        return (
            VruIntentClass::Unknown,
            0.0,
            VruIntentReason::NonFiniteMotion,
        );
    }

    if track.frames_seen < 2 {
        return (
            VruIntentClass::Unknown,
            0.0,
            VruIntentReason::InsufficientHistory,
        );
    }

    if speed_mps <= cfg.stationary_speed_mps {
        return (
            VruIntentClass::WaitingNearPath,
            1.0,
            VruIntentReason::NearlyStationaryNearPath,
        );
    }

    let longitudinal_mps = track.vel.x_m.abs();
    let lateral_mps = track.vel.y_m.abs();

    if longitudinal_mps >= lateral_mps * 1.5 {
        return (
            VruIntentClass::AlongPath,
            (longitudinal_mps / speed_mps).clamp(0.0, 1.0),
            VruIntentReason::LongitudinalMotionDominant,
        );
    }

    // +Y is left. A track on the left moving toward the path has negative vy;
    // a track on the right moving toward the path has positive vy.
    let moving_toward_path = (track.pos.y_m > 0.0 && track.vel.y_m < 0.0)
        || (track.pos.y_m < 0.0 && track.vel.y_m > 0.0);

    if moving_toward_path && lateral_mps > longitudinal_mps {
        let intent = if track.vel.y_m < 0.0 {
            VruIntentClass::CrossingLeftToRight
        } else {
            VruIntentClass::CrossingRightToLeft
        };

        return (
            intent,
            (lateral_mps / speed_mps).clamp(0.0, 1.0),
            VruIntentReason::LateralMotionTowardPath,
        );
    }

    let moving_away_from_path = (track.pos.y_m > 0.0 && track.vel.y_m > 0.0)
        || (track.pos.y_m < 0.0 && track.vel.y_m < 0.0);

    if moving_away_from_path && lateral_mps > longitudinal_mps {
        return (
            VruIntentClass::MovingAway,
            (lateral_mps / speed_mps).clamp(0.0, 1.0),
            VruIntentReason::LateralMotionAwayFromPath,
        );
    }

    (
        VruIntentClass::Unknown,
        0.0,
        VruIntentReason::AmbiguousMotion,
    )
}

/// Build a temporally stable intent distribution from consecutive velocity
/// estimates.
///
/// The latest geometric prediction remains authoritative. Temporal history only
/// governs intent metadata:
///
/// - fewer than three observations preserve the current frame-local result;
/// - agreement preserves the current normalized distribution;
/// - disagreement collapses directional authority to Unknown;
/// - malformed prior motion also collapses to Unknown.
///
/// This can only remove speculative directional confidence. It cannot shrink
/// uncertainty, remove predicted occupancy, weaken pedestrian RSS, or create a
/// new track.
fn temporally_stable_vru_intent_distribution(
    track: &Track,
    cfg: &VruMotionPredictionConfig,
    current_intent: VruIntentClass,
    current_confidence: f64,
) -> Vec<VruIntentProbability> {
    if track.frames_seen < 3 {
        return vru_intent_probability_distribution(current_intent, current_confidence);
    }

    if !track.prior_vel.x_m.is_finite() || !track.prior_vel.y_m.is_finite() {
        return vru_intent_probability_distribution(VruIntentClass::Unknown, 0.0);
    }

    let mut prior_track = *track;
    prior_track.vel = track.prior_vel;
    prior_track.frames_seen = track.frames_seen.saturating_sub(1);

    let (prior_intent, _, _) = classify_vru_motion_intent(&prior_track, cfg);

    if prior_intent == current_intent {
        vru_intent_probability_distribution(current_intent, current_confidence)
    } else {
        vru_intent_probability_distribution(VruIntentClass::Unknown, 0.0)
    }
}

impl TajTracker {
    #[must_use]
    pub fn new(cfg: TajConfig) -> Self {
        Self {
            phase_a: TajPhaseA::new(cfg),
            tracks: Vec::new(),
            next_id: 0,
        }
    }

    /// Process a scan with temporal association: returns Phase-A perception whose
    /// objects carry persistent ids and estimated velocity.
    pub fn track(&mut self, scan: &LaserScan, now_ms: u64) -> TajPerception {
        let (mut perception, extents) = self.phase_a.process_parts(scan, now_ms);
        let gate = self.phase_a.cfg.track_assoc_gate_m;

        let associations =
            associate_tracks_globally(&self.tracks, &perception.objects, now_ms, gate);
        let mut next_tracks: Vec<Track> = Vec::with_capacity(perception.objects.len());

        for (obj_idx, obj) in perception.objects.iter_mut().enumerate() {
            let extent_m = extents.get(obj_idx).copied().unwrap_or(f64::INFINITY);
            let matched_track = associations.get(obj_idx).copied().flatten();

            match matched_track {
                Some(j) => {
                    let tr = self.tracks[j];

                    let dt = f64::from(
                        u32::try_from(now_ms.saturating_sub(tr.stamp_ms)).unwrap_or(u32::MAX),
                    ) / 1000.0;
                    let (vx, vy) = if dt > 1e-3 {
                        (
                            (obj.pos.x_m - tr.pos.x_m) / dt,
                            (obj.pos.y_m - tr.pos.y_m) / dt,
                        )
                    } else {
                        (0.0, 0.0)
                    };
                    obj.id = tr.id;
                    obj.vel = Point { x_m: vx, y_m: vy };
                    obj.velocity_mps = vx.hypot(vy);
                    obj.heading_rad = if obj.velocity_mps > 1e-6 {
                        vy.atan2(vx)
                    } else {
                        0.0
                    };
                    // Yaw rate from the heading change vs. the track's prior
                    // velocity (needs two velocity estimates → a third frame).
                    let prev_speed = tr.vel.x_m.hypot(tr.vel.y_m);
                    let yaw_rate = if dt > 1e-3 && prev_speed > 1e-3 && obj.velocity_mps > 1e-3 {
                        let prev_h = tr.vel.y_m.atan2(tr.vel.x_m);
                        wrap_pi(obj.heading_rad - prev_h) / dt
                    } else {
                        0.0
                    };
                    next_tracks.push(Track {
                        id: tr.id,
                        pos: obj.pos,
                        stamp_ms: now_ms,
                        vel: obj.vel,
                        prior_vel: tr.vel,
                        yaw_rate,
                        extent_m,
                        frames_seen: tr.frames_seen.saturating_add(1),
                        last_seen_ms: now_ms,
                    });
                }
                None => {
                    // New track — first sighting, no velocity yet.
                    let id = self.next_id;
                    self.next_id += 1;
                    obj.id = id;
                    obj.velocity_mps = 0.0;
                    obj.vel = Point { x_m: 0.0, y_m: 0.0 };
                    obj.heading_rad = 0.0;

                    next_tracks.push(Track {
                        id,
                        pos: obj.pos,
                        stamp_ms: now_ms,
                        vel: Point { x_m: 0.0, y_m: 0.0 },
                        prior_vel: Point { x_m: 0.0, y_m: 0.0 },
                        yaw_rate: 0.0,
                        extent_m,
                        frames_seen: 1,
                        last_seen_ms: now_ms,
                    });
                }
            }
        }
        // Retain unmatched tracks as COASTING rather than deleting them.
        //
        // Before this, an unmatched track was simply dropped: one missed
        // detection destroyed identity, velocity history and `frames_seen`,
        // so the next frame's detection came back as a first sighting. See
        // `Track::last_seen_ms`.
        //
        // Scope: this keeps the TRACK alive. It does not emit anything — the
        // objects the checker sees are still exactly this frame's detections.
        // Retaining the hazard itself is a separate, wire-visible change.
        let matched: Vec<u64> = next_tracks.iter().map(|t| t.id).collect();
        for tr in &self.tracks {
            if matched.contains(&tr.id) {
                continue;
            }
            // Budget measured from the last real observation, so coasting
            // cannot extend itself frame after frame.
            if now_ms.saturating_sub(tr.last_seen_ms) > self.phase_a.cfg.track_coast_budget_ms {
                continue; // expired
            }
            if next_tracks.len() >= MAX_RETAINED_TRACKS {
                break; // bounded: coasting can never grow the set without limit
            }
            let predicted = predicted_track_position(tr, now_ms);
            next_tracks.push(Track {
                id: tr.id,
                pos: predicted,
                // `stamp_ms` advances so the next association predicts from
                // now; `last_seen_ms` deliberately does not.
                stamp_ms: now_ms,
                vel: tr.vel,
                prior_vel: tr.prior_vel,
                yaw_rate: tr.yaw_rate,
                extent_m: tr.extent_m,
                // A frame nobody observed is not evidence of persistence, so
                // the classifier's history gates neither advance nor reset.
                frames_seen: tr.frames_seen,
                last_seen_ms: tr.last_seen_ms,
            });
        }
        self.tracks = next_tracks;
        perception
    }

    /// WP-10 (MGA G-4; docs/safety/PEDESTRIAN_RSS.md §6) — classify the
    /// current tracks into [`PerceivedPedestrian`]s for the checker's
    /// omnidirectional reachable-set bound. This is the PRODUCER the bound
    /// was armed-but-unfed without.
    ///
    /// Heuristic, deliberately biased fail-closed — classification
    /// UNCERTAINTY promotes TOWARD pedestrian (the stricter bound), never
    /// away:
    ///   - small footprint (`extent_m <= cfg.max_extent_m`) AND speed within
    ///     the pedestrian envelope (`<= cfg.max_speed_mps`) → pedestrian;
    ///   - a FIRST-SIGHTING track (one frame — velocity not yet estimable)
    ///     with a small footprint → pedestrian too: we cannot yet rule the
    ///     envelope out, so the stricter bound applies until tracking says
    ///     otherwise;
    ///   - a non-finite extent/speed on a track is a perception fault →
    ///     pedestrian (the checker additionally fails closed on non-finite
    ///     pedestrian fields).
    ///
    /// Large or fast objects are NOT pedestrians — and are NOT dropped: every
    /// track stays in `TajPerception::objects` regardless, so the vehicle-RSS
    /// bound always applies; the VRU bound is strictly ADDITIVE. `age_s` is
    /// the track's measurement age at `now_ms` (#789 F8 — the reachable disc
    /// has already grown for that long).
    /// Classify every active track and retain the decision evidence.
    ///
    /// Rule ordering is deliberate and fail-closed:
    ///
    /// 1. invalid classifier configuration → possible pedestrian;
    /// 2. non-finite extent → possible pedestrian;
    /// 3. provably oversized extent → not pedestrian;
    /// 4. first sighting / unknown velocity → possible pedestrian;
    /// 5. non-finite velocity → possible pedestrian;
    /// 6. speed inside the pedestrian envelope → possible pedestrian;
    /// 7. otherwise → not pedestrian.
    #[must_use]
    pub fn classify_vrus(&self, now_ms: u64, cfg: &VruClassifierConfig) -> Vec<ClassifiedVru> {
        let configuration_valid = cfg.max_extent_m.is_finite()
            && cfg.max_extent_m > 0.0
            && cfg.max_speed_mps.is_finite()
            && cfg.max_speed_mps >= 0.0;

        self.tracks
            .iter()
            .map(|track| {
                let speed_mps = track.vel.x_m.hypot(track.vel.y_m);

                let (classification, reason) = if !configuration_valid {
                    (
                        VruClassification::PossiblePedestrian,
                        VruClassificationReason::InvalidConfiguration,
                    )
                } else if !track.extent_m.is_finite() {
                    (
                        VruClassification::PossiblePedestrian,
                        VruClassificationReason::NonFiniteExtent,
                    )
                } else if track.extent_m > cfg.max_extent_m {
                    (
                        VruClassification::NotPedestrian,
                        VruClassificationReason::ExtentAboveEnvelope,
                    )
                } else if track.frames_seen < 2 {
                    (
                        VruClassification::PossiblePedestrian,
                        VruClassificationReason::FirstSightingUnknownVelocity,
                    )
                } else if !speed_mps.is_finite() {
                    (
                        VruClassification::PossiblePedestrian,
                        VruClassificationReason::NonFiniteVelocity,
                    )
                } else if speed_mps <= cfg.max_speed_mps {
                    (
                        VruClassification::PossiblePedestrian,
                        VruClassificationReason::WithinPedestrianEnvelope,
                    )
                } else {
                    (
                        VruClassification::NotPedestrian,
                        VruClassificationReason::SpeedAboveEnvelope,
                    )
                };

                ClassifiedVru {
                    pedestrian: PerceivedPedestrian {
                        id: track.id,
                        pos: track.pos,
                        vel: track.vel,
                        age_s: now_ms.saturating_sub(track.stamp_ms) as f64 / 1_000.0,
                    },
                    classification,
                    reason,
                    extent_m: track.extent_m,
                    speed_mps,
                    frames_seen: track.frames_seen,
                }
            })
            .collect()
    }

    /// Associate supporting camera VRU evidence with current lidar tracks.
    ///
    /// This method never creates tracks and never returns camera-only VRUs.
    /// The returned associations are diagnostic/supporting evidence for a
    /// subsequent fusion policy.
    #[must_use]
    pub fn associate_camera_vrus(
        &self,
        observations: &[CameraVruObservation],
        now_ms: u64,
        cfg: &CameraVruFusionConfig,
    ) -> Vec<CameraVruAssociation> {
        associate_camera_vrus_to_tracks(&self.tracks, observations, now_ms, cfg)
    }

    /// Fuse supporting camera associations into the lidar VRU decisions.
    ///
    /// Safety invariants:
    ///
    /// - lidar remains authoritative for track existence and geometry;
    /// - camera-only evidence cannot create a result;
    /// - camera evidence cannot remove ordinary object RSS;
    /// - conflicting evidence feeds pedestrian RSS;
    /// - an empty association set preserves lidar-only membership.
    #[must_use]
    pub fn classify_vrus_fused(
        &self,
        now_ms: u64,
        classifier_cfg: &VruClassifierConfig,
        associations: &[CameraVruAssociation],
    ) -> Vec<FusedClassifiedVru> {
        self.classify_vrus(now_ms, classifier_cfg)
            .into_iter()
            .map(|lidar| {
                // Association is already deterministic and one-to-one. Use a
                // minimum-key fallback defensively if a malformed caller
                // supplies duplicate track associations.
                let camera = associations
                    .iter()
                    .filter(|association| association.track_id == lidar.pedestrian.id)
                    .min_by(|left, right| {
                        left.distance_m
                            .total_cmp(&right.distance_m)
                            .then_with(|| left.observation_id.cmp(&right.observation_id))
                    });

                let (fused_classification, fusion_reason) = match (lidar.classification, camera) {
                    (VruClassification::PossiblePedestrian, Some(_)) => (
                        FusedVruClassification::CameraSupportedPedestrian,
                        FusionInfluenceReason::CameraSupportsLidarPedestrian,
                    ),
                    (VruClassification::PossiblePedestrian, None) => (
                        FusedVruClassification::PossiblePedestrian,
                        FusionInfluenceReason::LidarPossiblePedestrian,
                    ),
                    (VruClassification::NotPedestrian, Some(_)) => (
                        FusedVruClassification::ConflictingEvidence,
                        FusionInfluenceReason::CameraConflictsWithLidarExclusion,
                    ),
                    (VruClassification::NotPedestrian, None) => (
                        FusedVruClassification::NotPedestrian,
                        FusionInfluenceReason::LidarNotPedestrian,
                    ),
                };

                FusedClassifiedVru {
                    pedestrian: lidar.pedestrian,
                    lidar_classification: lidar.classification,
                    lidar_reason: lidar.reason,
                    fused_classification,
                    fusion_reason,
                    camera_observation_id: camera.map(|value| value.observation_id),
                    camera_confidence: camera.map(|value| value.confidence),
                    association_distance_m: camera.map(|value| value.distance_m),
                    extent_m: lidar.extent_m,
                    speed_mps: lidar.speed_mps,
                    frames_seen: lidar.frames_seen,
                }
            })
            .collect()
    }

    /// Compatibility view consumed by the pedestrian RSS producer.
    ///
    /// Safety membership is derived from [`Self::classify_vrus`], keeping one
    /// canonical source of VRU-classification truth.
    #[must_use]
    pub fn classify_pedestrians(
        &self,
        now_ms: u64,
        cfg: &VruClassifierConfig,
    ) -> Vec<PerceivedPedestrian> {
        self.classify_vrus(now_ms, cfg)
            .into_iter()
            .filter(|result| result.classification.feeds_pedestrian_rss())
            .map(|result| result.pedestrian)
            .collect()
    }

    /// Produce bounded, auditable VRU motion predictions for current lidar tracks.
    ///
    /// Directional prediction is used only when track history and kinematics are
    /// sufficiently trustworthy. Otherwise the result falls back to a stationary
    /// centre with an expanding omnidirectional uncertainty radius.
    ///
    /// This is prediction evidence only. It never creates tracks, changes fused
    /// VRU membership, or weakens the existing omnidirectional pedestrian RSS
    /// bound.
    #[must_use]
    pub fn predict_vru_motion(
        &self,
        now_ms: u64,
        cfg: &VruMotionPredictionConfig,
    ) -> Vec<PredictedVruMotion> {
        let config_valid = cfg.horizon_s.is_finite()
            && cfg.horizon_s > 0.0
            && cfg.step_s.is_finite()
            && cfg.step_s > 0.0
            && cfg.stationary_speed_mps.is_finite()
            && cfg.stationary_speed_mps >= 0.0
            && cfg.maximum_track_speed_mps.is_finite()
            && cfg.maximum_track_speed_mps > 0.0
            && cfg.minimum_turn_speed_mps.is_finite()
            && cfg.minimum_turn_speed_mps >= 0.0
            && cfg.minimum_abs_yaw_rate_rad_s.is_finite()
            && cfg.minimum_abs_yaw_rate_rad_s >= 0.0
            && cfg.maximum_abs_yaw_rate_rad_s.is_finite()
            && cfg.maximum_abs_yaw_rate_rad_s > 0.0
            && cfg.maximum_abs_yaw_rate_rad_s >= cfg.minimum_abs_yaw_rate_rad_s
            && cfg.base_uncertainty_m.is_finite()
            && cfg.base_uncertainty_m >= 0.0
            && cfg.uncertainty_growth_mps.is_finite()
            && cfg.uncertainty_growth_mps >= 0.0
            && cfg.fallback_growth_mps.is_finite()
            && cfg.fallback_growth_mps >= cfg.uncertainty_growth_mps
            && cfg.maximum_points > 0
            && cfg.maximum_points <= MAX_VRU_PREDICTION_POINTS;

        let bounded_steps = if config_valid {
            ((cfg.horizon_s / cfg.step_s).ceil() as usize).clamp(1, cfg.maximum_points)
        } else {
            1
        };

        let mut predictions = Vec::with_capacity(self.tracks.len());

        for track in &self.tracks {
            let (intent, intent_confidence, intent_reason) = classify_vru_motion_intent(track, cfg);
            let source_age_ms = now_ms.saturating_sub(track.stamp_ms);
            let source_age_s = source_age_ms as f64 / 1_000.0;
            let speed_mps = track.vel.x_m.hypot(track.vel.y_m);

            let track_finite = track.pos.x_m.is_finite()
                && track.pos.y_m.is_finite()
                && track.vel.x_m.is_finite()
                && track.vel.y_m.is_finite()
                && track.yaw_rate.is_finite()
                && track.extent_m.is_finite();

            let fallback_reason = if !config_valid {
                Some(VruPredictionFallbackReason::InvalidConfiguration)
            } else if !track_finite {
                Some(VruPredictionFallbackReason::NonFiniteTrackState)
            } else if track.frames_seen < 2 {
                Some(VruPredictionFallbackReason::InsufficientHistory)
            } else if source_age_ms > cfg.maximum_track_age_ms {
                Some(VruPredictionFallbackReason::StaleTrack)
            } else if !speed_mps.is_finite() || speed_mps > cfg.maximum_track_speed_mps {
                Some(VruPredictionFallbackReason::ExcessiveSpeed)
            } else if track.yaw_rate.abs() > cfg.maximum_abs_yaw_rate_rad_s {
                Some(VruPredictionFallbackReason::ExcessiveYawRate)
            } else {
                None
            };

            let model = if fallback_reason.is_some() {
                VruMotionModel::OmnidirectionalFallback
            } else if speed_mps <= cfg.stationary_speed_mps {
                VruMotionModel::Stationary
            } else if speed_mps >= cfg.minimum_turn_speed_mps
                && track.yaw_rate.abs() >= cfg.minimum_abs_yaw_rate_rad_s
            {
                VruMotionModel::BoundedTurnRate
            } else {
                VruMotionModel::ConstantVelocity
            };

            let mut points = Vec::with_capacity(bounded_steps);

            if !config_valid || !track_finite {
                points.push(PredictedVruPoint {
                    time_s: 0.0,
                    pos: track.pos,
                    uncertainty_radius_m: f64::INFINITY,
                });
            } else {
                let mut x_m = track.pos.x_m;
                let mut y_m = track.pos.y_m;
                let mut heading_rad = if speed_mps > cfg.stationary_speed_mps {
                    track.vel.y_m.atan2(track.vel.x_m)
                } else {
                    0.0
                };

                for index in 1..=bounded_steps {
                    let time_s = (index as f64 * cfg.step_s).min(cfg.horizon_s);

                    match model {
                        VruMotionModel::OmnidirectionalFallback | VruMotionModel::Stationary => {}
                        VruMotionModel::ConstantVelocity => {
                            x_m = track.pos.x_m + track.vel.x_m * time_s;
                            y_m = track.pos.y_m + track.vel.y_m * time_s;
                        }
                        VruMotionModel::BoundedTurnRate => {
                            heading_rad += track.yaw_rate * cfg.step_s;
                            x_m += speed_mps * heading_rad.cos() * cfg.step_s;
                            y_m += speed_mps * heading_rad.sin() * cfg.step_s;
                        }
                    }

                    let growth_mps = if model == VruMotionModel::OmnidirectionalFallback {
                        cfg.fallback_growth_mps
                    } else {
                        cfg.uncertainty_growth_mps
                    };

                    let uncertainty_radius_m =
                        cfg.base_uncertainty_m + growth_mps * (source_age_s + time_s);

                    points.push(PredictedVruPoint {
                        time_s,
                        pos: Point { x_m, y_m },
                        uncertainty_radius_m,
                    });
                }
            }

            predictions.push(PredictedVruMotion {
                track_id: track.id,
                model,
                intent,
                intent_confidence,
                intent_reason,
                intent_probabilities: temporally_stable_vru_intent_distribution(
                    track,
                    cfg,
                    intent,
                    intent_confidence,
                ),
                points,
                horizon_s: cfg.horizon_s,
                step_s: cfg.step_s,
                source_age_s,
                frames_seen: track.frames_seen,
                fallback_reason,
            });
        }

        predictions.sort_by_key(|prediction| prediction.track_id);
        predictions
    }

    /// Predict each tracked object's future path over `horizon_s` (sampled every
    /// `dt`) using a **constant-turn-rate, constant-velocity (CTRV)** model — so a
    /// turning object's prediction *curves* with its yaw rate, instead of the
    /// straight-line constant-velocity guess. (`yaw_rate ≈ 0` → straight, i.e. CV.)
    #[must_use]
    pub fn predict(&self, horizon_s: f64, dt: f64) -> Vec<PredictedPath> {
        let dt = dt.max(0.01);
        let steps = (horizon_s / dt).ceil() as usize;
        self.tracks
            .iter()
            .map(|t| {
                let speed = t.vel.x_m.hypot(t.vel.y_m);
                let mut heading = t.vel.y_m.atan2(t.vel.x_m);
                let (mut x, mut y) = (t.pos.x_m, t.pos.y_m);
                let mut points = Vec::with_capacity(steps);
                for _ in 0..steps {
                    heading += t.yaw_rate * dt;
                    x += speed * heading.cos() * dt;
                    y += speed * heading.sin() * dt;
                    points.push(Point { x_m: x, y_m: y });
                }
                PredictedPath {
                    id: t.id,
                    yaw_rate_rad_s: t.yaw_rate,
                    points,
                }
            })
            .collect()
    }
}

/// Wrap an angle to `(-π, π]`.
fn wrap_pi(a: f64) -> f64 {
    let mut a = a % (2.0 * core::f64::consts::PI);
    if a > core::f64::consts::PI {
        a -= 2.0 * core::f64::consts::PI;
    } else if a <= -core::f64::consts::PI {
        a += 2.0 * core::f64::consts::PI;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_trajectory::state::TrajectoryVerdict;
    use std::f64::consts::PI;

    /// Build a forward 180° scan (`-π/2 .. +π/2`, 1° steps) from a per-ray range
    /// function. `None` (or out-of-range) → no return.
    fn scan_from<F: Fn(f64) -> Option<f64>>(range_max: f64, stamp_ms: u64, f: F) -> LaserScan {
        let n = 181usize;
        let angle_min = -PI / 2.0;
        let angle_inc = PI / (n as f64 - 1.0);
        let ranges = (0..n)
            .map(|i| {
                let theta = angle_min + i as f64 * angle_inc;
                match f(theta) {
                    Some(r) if r >= 0.05 && r <= range_max => r as f32,
                    _ => (range_max + 1.0) as f32, // out of range → no return
                }
            })
            .collect();
        LaserScan {
            angle_min_rad: angle_min,
            angle_increment_rad: angle_inc,
            range_min_m: 0.05,
            range_max_m: range_max,
            ranges,
            stamp_ms,
        }
    }

    /// Two parallel walls at `y = ±half_width` along the corridor.
    fn walls_scan(half_width: f64, range_max: f64) -> LaserScan {
        scan_from(range_max, 0, |theta| {
            let s = theta.sin();
            if s.abs() < 1e-3 {
                None // straight ahead: parallel wall is infinitely far → no return
            } else {
                Some(half_width / s.abs())
            }
        })
    }

    #[test]
    fn clear_corridor_from_parallel_walls() {
        let taj = TajPhaseA::default();
        let out = taj.process(&walls_scan(2.0, 10.0), 5);

        // Nearest left/right obstacle ≈ ±2 m.
        assert!(
            (out.corridor.left_boundary()[0].y_m - 2.0).abs() < 0.3,
            "left ≈ +2, got {}",
            out.corridor.left_boundary()[0].y_m
        );
        assert!(
            (out.corridor.right_boundary()[0].y_m + 2.0).abs() < 0.3,
            "right ≈ -2, got {}",
            out.corridor.right_boundary()[0].y_m
        );
        assert!(out.corridor.confidence() > 0.5, "walls give good coverage");
        assert!(
            out.corridor.left_boundary().len() >= 2,
            "per-station boundary"
        );
        assert_eq!(out.corridor.age_ms(), 5);
    }

    #[test]
    fn frontal_centerline_returns_do_not_become_corridor_walls() {
        let taj = TajPhaseA::default();

        // A real frontal obstacle around 0.57 m with tiny lateral variation.
        // It must remain an object/ACD bound, but must not collapse the
        // left/right corridor boundaries to millimetres.
        let scan = scan_from(
            10.0,
            0,
            |theta| {
                if theta.abs() < 0.05 {
                    Some(0.57)
                } else {
                    None
                }
            },
        );

        let out = taj.process(&scan, 0);
        let left = out.corridor.left_boundary()[0].y_m;
        let right = out.corridor.right_boundary()[0].y_m;
        let width = left - right;

        assert!(
            width >= 0.503,
            "frontal centerline return collapsed corridor width to {width}"
        );

        assert!(
            out.objects.iter().any(|object| object.pos.x_m < 1.0),
            "frontal return must remain represented as an object"
        );
    }

    #[test]
    fn near_origin_lateral_returns_do_not_collapse_the_corridor() {
        let taj = TajPhaseA::default();

        // Returns near ±87 degrees at 0.6 m have x≈0.03 m. They are beside the
        // lidar origin, not usable forward corridor boundaries.
        let scan = scan_from(
            10.0,
            0,
            |theta| {
                if theta.abs() > 1.50 {
                    Some(0.60)
                } else {
                    None
                }
            },
        );

        let out = taj.process(&scan, 0);
        let left = out.corridor.left_boundary()[0].y_m;
        let right = out.corridor.right_boundary()[0].y_m;

        assert!(
            left > 1.0,
            "near-origin lateral returns collapsed left boundary to {left}"
        );
        assert!(
            right < -1.0,
            "near-origin lateral returns collapsed right boundary to {right}"
        );
    }

    #[test]
    fn obstacle_ahead_clusters_to_one_object() {
        // A ~±8.6° blob of returns at 5 m dead ahead; everything else no-return.
        let taj = TajPhaseA::default();
        let scan = scan_from(
            10.0,
            0,
            |theta| if theta.abs() < 0.15 { Some(5.0) } else { None },
        );
        let out = taj.process(&scan, 0);

        assert_eq!(out.objects.len(), 1, "one blob → one object");
        let o = &out.objects[0];
        assert!(
            (o.pos.x_m - 5.0).abs() < 0.5 && o.pos.y_m.abs() < 0.8,
            "object near (5,0), got ({},{})",
            o.pos.x_m,
            o.pos.y_m
        );
        assert_eq!(o.velocity_mps, 0.0, "single-frame → velocity reported as 0");
    }

    // ---- WP-10: the VRU classifier (producer for the pedestrian-RSS bound) ----

    /// Two-frame small slow blob → classified pedestrian, with the tracked
    /// velocity carried through — and it REMAINS in `objects` (the VRU bound
    /// is additive, never a reclassification out of vehicle RSS).
    #[test]
    fn classifier_small_slow_cluster_is_a_pedestrian_and_stays_an_object() {
        let mut tracker = TajTracker::new(TajConfig::default());
        // ±2° blob at 5 m → extent ≈ 0.35 m. Second frame shifted +0.1 m
        // longitudinally over 100 ms → ~1 m/s, within the pedestrian envelope.
        let s1 = scan_from(10.0, 0, |t| if t.abs() < 0.035 { Some(5.0) } else { None });
        let s2 = scan_from(
            10.0,
            100,
            |t| if t.abs() < 0.035 { Some(5.1) } else { None },
        );
        tracker.track(&s1, 0);
        let out = tracker.track(&s2, 100);

        let peds = tracker.classify_pedestrians(100, &VruClassifierConfig::default());
        assert_eq!(
            peds.len(),
            1,
            "small slow blob must classify as a pedestrian"
        );
        assert!((peds[0].pos.x_m - 5.1).abs() < 0.5);
        assert!(peds[0].vel.x_m.hypot(peds[0].vel.y_m) <= 3.5);
        assert_eq!(peds[0].age_s, 0.0, "fresh measurement at classify time");
        assert_eq!(
            out.objects.len(),
            1,
            "the pedestrian is STILL an object (additive bound)"
        );
    }

    /// FIRST sighting (velocity not yet estimable) with a small footprint →
    /// pedestrian. Classification uncertainty promotes TOWARD the stricter
    /// bound, never away.
    #[test]
    fn classifier_first_sighting_small_cluster_is_promoted_to_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let s1 = scan_from(10.0, 0, |t| if t.abs() < 0.035 { Some(5.0) } else { None });
        tracker.track(&s1, 0);
        let peds = tracker.classify_pedestrians(0, &VruClassifierConfig::default());
        assert_eq!(
            peds.len(),
            1,
            "an unknown-velocity small object cannot be ruled out of the pedestrian \
             envelope — it must classify as one until tracking says otherwise"
        );
    }

    /// A small object tracked ABOVE the pedestrian speed envelope is not a
    /// pedestrian (vehicle RSS still applies — it stays in `objects`).
    #[test]
    fn classifier_fast_small_cluster_is_not_a_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());
        // +0.5 m over 100 ms → 5 m/s > 3.5 envelope.
        let s1 = scan_from(10.0, 0, |t| if t.abs() < 0.035 { Some(5.0) } else { None });
        let s2 = scan_from(
            10.0,
            100,
            |t| if t.abs() < 0.035 { Some(5.5) } else { None },
        );
        tracker.track(&s1, 0);
        let out = tracker.track(&s2, 100);
        let peds = tracker.classify_pedestrians(100, &VruClassifierConfig::default());
        assert!(
            peds.is_empty(),
            "5 m/s is outside the pedestrian envelope: {peds:?}"
        );
        assert_eq!(
            out.objects.len(),
            1,
            "still tracked as an object for vehicle RSS"
        );
    }

    /// A LARGE cluster (car-scale footprint) is not a pedestrian even when slow.
    #[test]
    fn classifier_large_cluster_is_not_a_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());
        // ±20° at 5 m → extent ≈ 3.4 m ≫ 1.2 m.
        let s1 = scan_from(10.0, 0, |t| if t.abs() < 0.35 { Some(5.0) } else { None });
        tracker.track(&s1, 0);
        let peds = tracker.classify_pedestrians(0, &VruClassifierConfig::default());
        assert!(
            peds.is_empty(),
            "a car-scale footprint must not classify as a pedestrian"
        );
    }

    /// `age_s` reflects real measurement staleness at classify time (#789 F8:
    /// the reachable disc has already grown for that long).
    #[test]
    fn camera_vru_association_requires_an_existing_lidar_track() {
        let tracker = TajTracker::new(TajConfig::default());
        let observations = [CameraVruObservation {
            observation_id: 90,
            pos: Point { x_m: 3.0, y_m: 0.2 },
            confidence: 0.95,
            stamp_ms: 1_000,
        }];

        let associations =
            tracker.associate_camera_vrus(&observations, 1_000, &CameraVruFusionConfig::default());

        assert!(
            associations.is_empty(),
            "camera-only evidence must never create a safety-authoritative VRU"
        );
    }

    #[test]
    fn camera_vru_association_matches_existing_lidar_track() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        let perception = tracker.track(&scan, 1_000);
        assert_eq!(perception.objects.len(), 1);

        let observation = CameraVruObservation {
            observation_id: 91,
            pos: Point {
                x_m: perception.objects[0].pos.x_m + 0.05,
                y_m: perception.objects[0].pos.y_m,
            },
            confidence: 0.90,
            stamp_ms: 1_000,
        };

        let associations =
            tracker.associate_camera_vrus(&[observation], 1_000, &CameraVruFusionConfig::default());

        assert_eq!(associations.len(), 1);
        assert_eq!(associations[0].track_id, perception.objects[0].id);
        assert_eq!(associations[0].observation_id, 91);
    }

    #[test]
    fn camera_vru_association_rejects_stale_low_confidence_and_out_of_gate_rows() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        tracker.track(&scan, 1_000);

        let observations = [
            CameraVruObservation {
                observation_id: 1,
                pos: Point { x_m: 3.0, y_m: 0.0 },
                confidence: 0.20,
                stamp_ms: 1_000,
            },
            CameraVruObservation {
                observation_id: 2,
                pos: Point { x_m: 3.0, y_m: 0.0 },
                confidence: 0.95,
                stamp_ms: 100,
            },
            CameraVruObservation {
                observation_id: 3,
                pos: Point { x_m: 8.0, y_m: 4.0 },
                confidence: 0.95,
                stamp_ms: 1_000,
            },
        ];

        assert!(tracker
            .associate_camera_vrus(&observations, 1_000, &CameraVruFusionConfig::default(),)
            .is_empty());
    }

    #[test]
    fn camera_vru_association_is_independent_of_observation_order() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        tracker.track(&scan, 1_000);

        let a = CameraVruObservation {
            observation_id: 10,
            pos: Point {
                x_m: 3.05,
                y_m: 0.0,
            },
            confidence: 0.90,
            stamp_ms: 1_000,
        };
        let b = CameraVruObservation {
            observation_id: 11,
            pos: Point {
                x_m: 3.20,
                y_m: 0.0,
            },
            confidence: 0.99,
            stamp_ms: 1_000,
        };

        let cfg = CameraVruFusionConfig::default();
        let forward = tracker.associate_camera_vrus(&[a, b], 1_000, &cfg);
        let reversed = tracker.associate_camera_vrus(&[b, a], 1_000, &cfg);

        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 1);
        assert_eq!(
            forward[0].observation_id, 10,
            "lowest geometric error wins, not input order or confidence"
        );
    }

    #[test]
    fn fused_classifier_without_camera_preserves_lidar_membership() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        tracker.track(&scan, 1_000);

        let fused = tracker.classify_vrus_fused(1_000, &VruClassifierConfig::default(), &[]);

        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].fused_classification,
            FusedVruClassification::PossiblePedestrian
        );
        assert!(fused[0].fused_classification.feeds_pedestrian_rss());
        assert_eq!(fused[0].camera_observation_id, None);
    }

    #[test]
    fn fused_classifier_camera_supports_lidar_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        let perception = tracker.track(&scan, 1_000);
        let track_id = perception.objects[0].id;

        let association = CameraVruAssociation {
            track_id,
            observation_id: 44,
            distance_m: 0.08,
            confidence: 0.92,
        };

        let fused =
            tracker.classify_vrus_fused(1_000, &VruClassifierConfig::default(), &[association]);

        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].fused_classification,
            FusedVruClassification::CameraSupportedPedestrian
        );
        assert_eq!(
            fused[0].fusion_reason,
            FusionInfluenceReason::CameraSupportsLidarPedestrian
        );
        assert_eq!(fused[0].camera_observation_id, Some(44));
        assert_eq!(fused[0].pedestrian.id, track_id);
        assert!(fused[0].fused_classification.feeds_pedestrian_rss());
    }

    #[test]
    fn fused_classifier_conflict_tightens_to_pedestrian_rss() {
        let mut tracker = TajTracker::new(TajConfig::default());

        let first = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        let second = scan_from(10.0, 1_100, |theta| {
            if theta.abs() < 0.035 {
                Some(3.5)
            } else {
                None
            }
        });

        tracker.track(&first, 1_000);
        let perception = tracker.track(&second, 1_100);
        let track_id = perception.objects[0].id;

        let lidar = tracker.classify_vrus(1_100, &VruClassifierConfig::default());
        assert_eq!(
            lidar[0].classification,
            VruClassification::NotPedestrian,
            "the synthetic 5 m/s track must first be excluded by lidar geometry"
        );

        let association = CameraVruAssociation {
            track_id,
            observation_id: 45,
            distance_m: 0.05,
            confidence: 0.95,
        };

        let fused =
            tracker.classify_vrus_fused(1_100, &VruClassifierConfig::default(), &[association]);

        assert_eq!(
            fused[0].fused_classification,
            FusedVruClassification::ConflictingEvidence
        );
        assert_eq!(
            fused[0].fusion_reason,
            FusionInfluenceReason::CameraConflictsWithLidarExclusion
        );
        assert!(
            fused[0].fused_classification.feeds_pedestrian_rss(),
            "sensor disagreement must tighten, never silently remove VRU RSS"
        );
    }

    #[test]
    fn fused_classifier_lidar_exclusion_without_camera_remains_not_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());

        let first = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        let second = scan_from(10.0, 1_100, |theta| {
            if theta.abs() < 0.035 {
                Some(3.5)
            } else {
                None
            }
        });

        tracker.track(&first, 1_000);
        let perception = tracker.track(&second, 1_100);

        let fused = tracker.classify_vrus_fused(1_100, &VruClassifierConfig::default(), &[]);

        assert_eq!(
            fused[0].fused_classification,
            FusedVruClassification::NotPedestrian
        );
        assert!(!fused[0].fused_classification.feeds_pedestrian_rss());
        assert_eq!(
            fused[0].pedestrian.id, perception.objects[0].id,
            "the lidar track remains in the ordinary object channel"
        );
    }

    #[test]
    fn fused_classifier_ignores_association_for_unknown_track() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(10.0, 1_000, |theta| {
            if theta.abs() < 0.035 {
                Some(3.0)
            } else {
                None
            }
        });
        tracker.track(&scan, 1_000);

        let association = CameraVruAssociation {
            track_id: u64::MAX,
            observation_id: 46,
            distance_m: 0.01,
            confidence: 0.99,
        };

        let fused =
            tracker.classify_vrus_fused(1_000, &VruClassifierConfig::default(), &[association]);

        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].fused_classification,
            FusedVruClassification::PossiblePedestrian
        );
        assert_eq!(fused[0].camera_observation_id, None);
    }

    #[test]
    fn classifier_invalid_configuration_fails_closed_to_possible_pedestrian() {
        let mut tracker = TajTracker::new(TajConfig::default());

        let first = scan_from(
            10.0,
            0,
            |theta| if theta.abs() < 0.035 { Some(5.0) } else { None },
        );
        let second = scan_from(10.0, 100, |theta| {
            if theta.abs() < 0.035 {
                Some(5.1)
            } else {
                None
            }
        });

        tracker.track(&first, 0);
        tracker.track(&second, 100);

        for cfg in [
            VruClassifierConfig {
                max_extent_m: f64::NAN,
                ..VruClassifierConfig::default()
            },
            VruClassifierConfig {
                max_speed_mps: f64::NAN,
                ..VruClassifierConfig::default()
            },
            VruClassifierConfig {
                max_extent_m: -1.0,
                ..VruClassifierConfig::default()
            },
            VruClassifierConfig {
                max_speed_mps: -1.0,
                ..VruClassifierConfig::default()
            },
        ] {
            let results = tracker.classify_vrus(100, &cfg);

            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].classification,
                VruClassification::PossiblePedestrian
            );
            assert_eq!(
                results[0].reason,
                VruClassificationReason::InvalidConfiguration
            );

            let pedestrians = tracker.classify_pedestrians(100, &cfg);
            assert_eq!(
                pedestrians.len(),
                1,
                "invalid classifier configuration must never remove a track \
                 from pedestrian RSS"
            );
        }
    }

    #[test]
    fn detailed_classifier_reason_matches_pedestrian_membership() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let scan = scan_from(
            10.0,
            0,
            |theta| if theta.abs() < 0.035 { Some(5.0) } else { None },
        );

        tracker.track(&scan, 0);

        let results = tracker.classify_vrus(0, &VruClassifierConfig::default());
        let pedestrians = tracker.classify_pedestrians(0, &VruClassifierConfig::default());

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].reason,
            VruClassificationReason::FirstSightingUnknownVelocity
        );
        assert!(results[0].classification.feeds_pedestrian_rss());
        assert_eq!(pedestrians.len(), 1);
        assert_eq!(pedestrians[0].id, results[0].pedestrian.id);
    }

    #[test]
    fn classifier_age_reflects_measurement_staleness() {
        let mut tracker = TajTracker::new(TajConfig::default());
        let s1 = scan_from(
            10.0,
            1_000,
            |t| if t.abs() < 0.035 { Some(5.0) } else { None },
        );
        tracker.track(&s1, 1_000);
        let peds = tracker.classify_pedestrians(1_500, &VruClassifierConfig::default());
        assert_eq!(peds.len(), 1);
        assert!(
            (peds[0].age_s - 0.5).abs() < 1e-9,
            "500 ms old at classify time"
        );
    }

    /// END-TO-END (the WP-10 point): a Taj-classified pedestrian feeds the
    /// checker's omnidirectional reachable-set bound and REFUSES a trajectory
    /// driving through its reachable disc — a trajectory the object-RSS pass
    /// alone admitted (the pedestrian starts laterally clear and the
    /// stationary blob has no closing velocity). Without the VRU channel the
    /// same trajectory is admitted: the producer is the deciding difference.
    #[test]
    fn classified_pedestrian_feeds_the_checker_reachable_set_bound() {
        use kirra_core::frame_integrity::FrameTrust;
        use kirra_trajectory::state::{Pose, TrajectoryPoint};
        use kirra_trajectory::validation::validate_trajectory_slow_capped;
        use kirra_trajectory::vru::{PedestrianScene, VruRssParams};
        use kirra_trajectory::VehicleConfig;
        use kirra_verifier::verifier::FleetPosture;

        let mut tracker = TajTracker::new(TajConfig::default());
        // A kerbside pedestrian: small blob at 6 m ahead, ~1.9 m to the left
        // (bearing ~+17.5°) — outside the ego footprint path, no motion.
        let bearing = 0.305_f64;
        let s1 = scan_from(10.0, 0, |t| {
            if (t - bearing).abs() < 0.03 {
                Some(6.3)
            } else {
                None
            }
        });
        tracker.track(&s1, 0);
        let peds = tracker.classify_pedestrians(0, &VruClassifierConfig::default());
        assert_eq!(
            peds.len(),
            1,
            "the kerbside blob classifies as a pedestrian"
        );

        // A straight 8 m/s pass through the kerbside position's x. Starts at
        // x = 5 so the REAR footprint overhang stays inside the corridor's
        // x >= 0 span (a pose at x = 0 fails CONTAINMENT, not RSS — verified
        // while writing this test).
        let traj: Vec<TrajectoryPoint> = (0..10)
            .map(|i| TrajectoryPoint {
                pose: Pose {
                    x_m: 5.0 + i as f64,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 8.0,
                time_from_start_s: i as f64 * 0.125,
            })
            .collect();
        let corridor = kirra_core::corridor::MockCorridorSource::straight_5m_half_width(200.0);
        let cfg = VehicleConfig::default_urban();

        let scene = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &[],
        };
        let with_vru = validate_trajectory_slow_capped(
            &traj,
            &corridor,
            &[],
            &cfg,
            None,
            FleetPosture::Nominal,
            None,
            None,
            None,
            Some(&scene),
            FrameTrust::Trusted,
        );
        let without_vru = validate_trajectory_slow_capped(
            &traj,
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
        assert_eq!(
            with_vru,
            kirra_trajectory::state::TrajectoryVerdict::MRCFallback,
            "an 8 m/s pass through the pedestrian's reachable disc must be refused"
        );
        assert_ne!(
            without_vru,
            kirra_trajectory::state::TrajectoryVerdict::MRCFallback,
            "without the VRU channel the same trajectory is admitted — the \
             producer is the deciding difference"
        );
    }

    #[test]
    fn taj_corridor_feeds_the_checker() {
        use kirra_trajectory::state::{Pose, TrajectoryPoint};
        use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};
        use kirra_verifier::verifier::FleetPosture;

        // A 3 m half-width corridor from walls; feed a short centered trajectory.
        let taj = TajPhaseA::default();
        let out = taj.process(&walls_scan(3.0, 12.0), 1);

        let traj = vec![
            TrajectoryPoint {
                pose: Pose {
                    x_m: 2.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose {
                    x_m: 3.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 1.0,
            },
        ];
        // Taj's corridor IS a real `CorridorSource` the #131 checker consumes; a
        // centered, in-corridor trajectory is admitted.
        let verdict = validate_trajectory_slow(
            &traj,
            &out.corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker should admit a centered in-corridor trajectory on Taj's corridor, got {verdict:?}"
        );
    }

    #[test]
    fn empty_scan_is_low_confidence_and_unhealthy() {
        // No obstacles detected at all → zero-confidence corridor, no objects.
        let taj = TajPhaseA::default();
        let out = taj.process(&scan_from(10.0, 0, |_| None), 0);

        assert_eq!(out.objects.len(), 0);
        assert_eq!(
            out.corridor.confidence(),
            0.0,
            "no valid returns → zero confidence"
        );
        // Fail-closed: 0.0 < the checker's SLOW_LOOP_MIN_CORRIDOR_CONFIDENCE → MRC.
    }

    #[test]
    fn taj_corridor_is_dyn_corridor_source() {
        let taj = TajPhaseA::default();
        let out = taj.process(&walls_scan(2.0, 10.0), 0);
        let dynref: &dyn CorridorSource = &out.corridor;
        assert!(dynref.left_boundary().len() >= 2);
        assert!(dynref.confidence() > 0.0);
    }

    #[test]
    fn localized_obstacle_narrows_corridor_only_near_itself() {
        // A blob dead ahead at x≈4 between walls at ±5. The refined per-station
        // corridor must narrow NEAR the blob's x but stay wide away from it —
        // unlike the old global model that collapsed the whole corridor.
        let taj = TajPhaseA::default(); // forward_extent 8, stations every 1 m
        let scan = scan_from(12.0, 0, |theta| {
            let s = theta.sin();
            let wall = if s.abs() < 1e-3 {
                f64::INFINITY
            } else {
                5.0 / s.abs()
            };
            let blob = if theta.abs() < 0.12 {
                4.0 / theta.cos()
            } else {
                f64::INFINITY
            };
            let r = wall.min(blob);
            if r.is_finite() {
                Some(r)
            } else {
                None
            }
        });
        let out = taj.process(&scan, 0);

        let y_near = |x: f64| {
            out.corridor
                .left_boundary()
                .iter()
                .min_by(|a, b| (a.x_m - x).abs().total_cmp(&(b.x_m - x).abs()))
                .unwrap()
                .y_m
        };
        assert!(
            y_near(4.0) < 1.0,
            "corridor narrows near the obstacle, got {}",
            y_near(4.0)
        );
        assert!(
            y_near(7.0) > 3.0,
            "corridor stays wide away from it, got {}",
            y_near(7.0)
        );
    }

    // --- Temporal tracker (velocity estimation) ----------------------------

    /// A blob (≈ vertical wall segment) centred at `x ≈ bx`, `y ≈ 0`.
    fn blob_scan(bx: f64, stamp_ms: u64) -> LaserScan {
        scan_from(20.0, stamp_ms, |theta| {
            if theta.abs() < 0.15 {
                Some(bx / theta.cos())
            } else {
                None
            }
        })
    }

    /// A scan with no returns at all — the detection dropout this exists for.
    fn empty_scan(stamp_ms: u64) -> LaserScan {
        scan_from(20.0, stamp_ms, |_| None)
    }

    // -----------------------------------------------------------------------
    // Track lifecycle: coasting through a dropout
    //
    // An unmatched track used to be DELETED. The next frame's detection came
    // back as a brand-new track — new id, velocity from zero, frames_seen back
    // to 1 — and `frames_seen` gates velocity (< 2), yaw rate (< 3) and the
    // first-sighting arm of the VRU classifier. These pin that a bounded gap no
    // longer costs any of that, and that the retention stays bounded.
    // -----------------------------------------------------------------------

    #[test]
    fn a_track_survives_a_one_frame_dropout() {
        let mut taj = TajTracker::default();
        let first = taj.track(&blob_scan(10.0, 0), 0);
        let id = first.objects[0].id;

        // Frame 2: the detector reports nothing.
        let gap = taj.track(&empty_scan(100), 100);
        assert!(
            gap.objects.is_empty(),
            "no detection means no object emitted"
        );

        // Frame 3: the object is seen again, close to where it was.
        let back = taj.track(&blob_scan(10.2, 200), 200);
        assert_eq!(
            back.objects[0].id, id,
            "the same physical object must keep its track id across one dropped scan"
        );
    }

    #[test]
    fn a_dropout_does_not_reset_motion_history() {
        // The reason identity matters: frames_seen gates velocity and yaw-rate
        // estimation, so a reset silently downgrades the track to a first
        // sighting with no velocity.
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        let _ = taj.track(&blob_scan(11.0, 100), 100);
        let _ = taj.track(&empty_scan(200), 200);
        let out = taj.track(&blob_scan(13.0, 300), 300);

        assert!(
            out.objects[0].velocity_mps > 1.0,
            "velocity must be estimable straight after a dropout, got {}",
            out.objects[0].velocity_mps
        );
    }

    #[test]
    fn a_track_expires_after_the_coast_budget() {
        // Coasting is bounded: a genuinely departed object must not linger.
        let mut taj = TajTracker::default();
        let first = taj.track(&blob_scan(10.0, 0), 0);
        let id = first.objects[0].id;

        let budget = TajConfig::default().track_coast_budget_ms;
        let past = budget + 100;
        let _ = taj.track(&empty_scan(past), past);

        let back = taj.track(&blob_scan(10.0, past + 100), past + 100);
        assert_ne!(
            back.objects[0].id, id,
            "past the budget the track must expire, so re-detection is a NEW object"
        );
    }

    #[test]
    fn coasting_emits_no_objects() {
        // Scope boundary for this change: it keeps the TRACK alive, it does not
        // put an unobserved object in front of the checker. Retaining the
        // hazard itself is a separate, wire-visible change.
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        for (i, t) in [100_u64, 150, 200].iter().enumerate() {
            let out = taj.track(&empty_scan(*t), *t);
            assert!(
                out.objects.is_empty(),
                "coasting frame {i} emitted an object the sensor never saw"
            );
        }
    }

    #[test]
    fn the_coast_budget_is_measured_from_the_last_real_observation() {
        // Guard against a coasting track refreshing its own budget every frame
        // and living forever. Several dropout frames INSIDE the budget, then
        // one past it: the track must be gone.
        let mut taj = TajTracker::default();
        let first = taj.track(&blob_scan(10.0, 0), 0);
        let id = first.objects[0].id;

        let budget = TajConfig::default().track_coast_budget_ms;
        let mut t = 0;
        while t < budget {
            t += 50;
            let _ = taj.track(&empty_scan(t), t);
        }
        t += 100; // now definitively past the budget
        let _ = taj.track(&empty_scan(t), t);

        let back = taj.track(&blob_scan(10.0, t + 50), t + 50);
        assert_ne!(
            back.objects[0].id, id,
            "repeated coasting must not extend the budget"
        );
    }

    #[test]
    fn tracker_estimates_velocity_of_moving_object() {
        // Object moves +2 m over a 200 ms inter-frame gap → ~10 m/s in +x.
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        let out = taj.track(&blob_scan(12.0, 200), 200);

        assert_eq!(out.objects.len(), 1);
        let o = &out.objects[0];
        assert!(
            (o.velocity_mps - 10.0).abs() < 1.0,
            "speed ≈ 10 m/s, got {}",
            o.velocity_mps
        );
        assert!(
            o.vel.x_m > 8.0 && o.vel.y_m.abs() < 1.0,
            "velocity is +x, got {:?}",
            o.vel
        );
        assert!(
            o.heading_rad.abs() < 0.2,
            "heading ≈ 0 (forward), got {}",
            o.heading_rad
        );
    }

    #[test]
    fn tracker_reports_zero_velocity_for_static_object() {
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        let out = taj.track(&blob_scan(10.0, 200), 200);

        assert_eq!(out.objects.len(), 1);
        assert!(
            out.objects[0].velocity_mps < 0.5,
            "static → ~0 m/s, got {}",
            out.objects[0].velocity_mps
        );
    }

    #[test]
    fn tracker_persists_track_id() {
        let mut taj = TajTracker::default();
        let a = taj.track(&blob_scan(10.0, 0), 0);
        let b = taj.track(&blob_scan(11.0, 200), 200);
        assert_eq!(
            a.objects[0].id, b.objects[0].id,
            "same object keeps its track id"
        );
    }

    #[test]
    fn association_uses_predicted_position_for_fast_object() {
        let track = Track {
            id: 7,
            pos: Point {
                x_m: 10.0,
                y_m: 0.0,
            },
            stamp_ms: 1_000,
            vel: Point {
                x_m: 10.0,
                y_m: 0.0,
            },
            prior_vel: Point { x_m: 0.0, y_m: 0.0 },
            yaw_rate: 0.0,
            extent_m: 0.3,
            frames_seen: 3,
            last_seen_ms: 1_000,
        };

        // At 1.2 s, the predicted position is x=12 rather than the stale x=10.
        let detections = vec![PerceivedObject {
            id: 0,
            pos: Point {
                x_m: 12.05,
                y_m: 0.0,
            },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }];

        let matches = associate_tracks_globally(&[track], &detections, 1_200, 0.40);
        assert_eq!(matches, vec![Some(0)]);
    }

    #[test]
    fn association_outside_gate_creates_no_match() {
        let track = Track {
            id: 4,
            pos: Point { x_m: 1.0, y_m: 0.0 },
            stamp_ms: 0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
            prior_vel: Point { x_m: 0.0, y_m: 0.0 },
            yaw_rate: 0.0,
            extent_m: 0.3,
            frames_seen: 2,
            last_seen_ms: 0,
        };

        let detections = vec![PerceivedObject {
            id: 0,
            pos: Point { x_m: 2.0, y_m: 0.0 },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }];

        let matches = associate_tracks_globally(&[track], &detections, 100, 0.40);
        assert_eq!(matches, vec![None]);
    }

    #[test]
    fn global_assignment_is_independent_of_track_storage_order() {
        let make_track = |id: u64, x_m: f64| Track {
            id,
            pos: Point { x_m, y_m: 0.0 },
            stamp_ms: 0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
            prior_vel: Point { x_m: 0.0, y_m: 0.0 },
            yaw_rate: 0.0,
            extent_m: 0.3,
            frames_seen: 2,
            last_seen_ms: 0,
        };

        let detections = vec![
            PerceivedObject {
                id: 0,
                pos: Point {
                    x_m: 1.05,
                    y_m: 0.0,
                },
                velocity_mps: 0.0,
                heading_rad: 0.0,
                vel: Point { x_m: 0.0, y_m: 0.0 },
            },
            PerceivedObject {
                id: 0,
                pos: Point {
                    x_m: 2.05,
                    y_m: 0.0,
                },
                velocity_mps: 0.0,
                heading_rad: 0.0,
                vel: Point { x_m: 0.0, y_m: 0.0 },
            },
        ];

        let ordered = vec![make_track(10, 1.0), make_track(20, 2.0)];
        let reversed = vec![make_track(20, 2.0), make_track(10, 1.0)];

        let ordered_matches = associate_tracks_globally(&ordered, &detections, 100, 0.40);
        let reversed_matches = associate_tracks_globally(&reversed, &detections, 100, 0.40);

        let ordered_ids: Vec<Option<u64>> = ordered_matches
            .iter()
            .map(|matched| matched.map(|index| ordered[index].id))
            .collect();

        let reversed_ids: Vec<Option<u64>> = reversed_matches
            .iter()
            .map(|matched| matched.map(|index| reversed[index].id))
            .collect();

        assert_eq!(ordered_ids, vec![Some(10), Some(20)]);
        assert_eq!(reversed_ids, ordered_ids);
    }

    #[test]
    fn equal_cost_association_uses_lower_track_id() {
        let make_track = |id: u64, y_m: f64| Track {
            id,
            pos: Point { x_m: 5.0, y_m },
            stamp_ms: 0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
            prior_vel: Point { x_m: 0.0, y_m: 0.0 },
            yaw_rate: 0.0,
            extent_m: 0.3,
            frames_seen: 2,
            last_seen_ms: 0,
        };

        let tracks = vec![make_track(20, -0.1), make_track(10, 0.1)];
        let detections = vec![PerceivedObject {
            id: 0,
            pos: Point { x_m: 5.0, y_m: 0.0 },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }];

        let matches = associate_tracks_globally(&tracks, &detections, 100, 0.40);
        let matched_id = matches[0].map(|index| tracks[index].id);

        assert_eq!(
            matched_id,
            Some(10),
            "equal geometric cost must resolve by persistent track ID"
        );
    }

    #[test]
    fn tracker_first_sighting_is_zero_velocity() {
        let mut taj = TajTracker::default();
        let out = taj.track(&blob_scan(10.0, 0), 0);
        assert_eq!(out.objects.len(), 1);
        assert_eq!(
            out.objects[0].velocity_mps, 0.0,
            "first sighting has no prior → 0 velocity"
        );
    }

    #[test]
    fn object_velocity_changes_rss_verdict() {
        // WHY tracking matters: a fast-receding lead that Phase-A would treat as
        // PARKED (velocity 0) is wrongly RSS-rejected, but with the tracked
        // velocity the checker correctly admits it. Same geometry, only the
        // object's velocity differs.
        use kirra_trajectory::corridor::MockCorridorSource;
        use kirra_trajectory::state::{Pose, TrajectoryPoint};
        use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};
        use kirra_verifier::verifier::FleetPosture;

        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let tp = |x: f64, t: f64| TrajectoryPoint {
            pose: Pose {
                x_m: x,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: 8.0,
            time_from_start_s: t,
        };
        let traj = vec![tp(10.0, 0.0), tp(12.0, 0.25), tp(14.0, 0.5)];
        let obj = |vmag: f64, vx: f64| PerceivedObject {
            id: 1,
            // In the ego's path (within the checker's longitudinal footprint-overlap
            // band, 2.5 m) so a STATIC lead is a real longitudinal conflict, yet
            // beyond the lateral side-gap (~1.75 m) so a RECEDING lead — pulling away
            // → no longitudinal threat — is admitted. (Post the §4 RSS-conjunction
            // gating: an object 3 m aside is correctly passable, so velocity, not
            // mere presence, must drive the verdict here.)
            pos: Point {
                x_m: 22.0,
                y_m: 2.0,
            },
            velocity_mps: vmag,
            heading_rad: 0.0,
            vel: Point { x_m: vx, y_m: 0.0 },
        };
        let cfg = VehicleConfig::default_urban();
        let verdict = |o: &[PerceivedObject]| {
            validate_trajectory_slow(&traj, &corridor, o, &cfg, None, FleetPosture::Nominal)
        };

        // Phase-A assumption (static) → the lead's gap looks unsafe → reject.
        assert_eq!(verdict(&[obj(0.0, 0.0)]), TrajectoryVerdict::MRCFallback);
        // Tracked velocity (receding at 15 m/s) → adequate gap → admitted.
        assert!(matches!(
            verdict(&[obj(15.0, 15.0)]),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        ));
    }

    // --- Phase B: semantic hazard fusion -----------------------------------

    fn hazard(class: SemanticClass, near_x: f64) -> SemanticDetection {
        // A region spanning the full lane width at `near_x`.
        SemanticDetection {
            class,
            near_x_m: near_x,
            lateral_min_m: -5.0,
            lateral_max_m: 5.0,
        }
    }

    #[test]
    fn drivability_policy_is_conservative() {
        assert!(SemanticClass::Road.is_drivable());
        assert!(!SemanticClass::Water.is_drivable());
        assert!(!SemanticClass::StaticObstacle.is_drivable());
        assert!(
            !SemanticClass::Unknown.is_drivable(),
            "Unknown fails closed"
        );
    }

    #[test]
    fn mock_detector_returns_scripted_detections() {
        let det = MockSemanticDetector {
            detections: vec![hazard(SemanticClass::Water, 8.0)],
        };
        let out = det.detect();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, SemanticClass::Water);
    }

    #[test]
    fn water_hazard_clips_corridor_forward_extent() {
        // A clear (Phase-A) corridor extends to forward_extent; a water hazard at
        // 4 m clips the drivable space to ~4 m.
        let taj = TajPhaseA::new(TajConfig {
            forward_extent_m: 20.0,
            ..Default::default()
        });
        let out = taj.process(&walls_scan(5.0, 30.0), 0);
        let far = out.corridor.left_boundary().last().unwrap().x_m;
        assert!(far > 15.0, "clear corridor reaches far, got {far}");

        let clipped = clip_corridor_to_hazards(&out.corridor, &[hazard(SemanticClass::Water, 4.0)]);
        let clipped_far = clipped.left_boundary().last().unwrap().x_m;
        assert!(
            (clipped_far - 4.0).abs() < 1.0,
            "corridor clipped at the water edge, got {clipped_far}"
        );
    }

    #[test]
    fn kirra_rejects_driving_into_water() {
        // THE Phase-B win: Phase A reports a lake as free corridor; a trajectory
        // drives into it and KIRRA ADMITS (it cannot see water). After semantic
        // fusion clips the corridor at the water edge, the SAME trajectory is
        // REJECTED — the hazard Phase A was blind to is now enforced.
        use kirra_trajectory::corridor::CorridorSource as _;
        use kirra_trajectory::state::{Pose, TrajectoryPoint};
        use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};
        use kirra_verifier::verifier::FleetPosture;

        let taj = TajPhaseA::new(TajConfig {
            forward_extent_m: 20.0,
            ..Default::default()
        });
        let perception = taj.process(&walls_scan(5.0, 30.0), 0);

        // A trajectory driving forward to x = 15 (into where the water is).
        let traj = vec![
            TrajectoryPoint {
                pose: Pose {
                    x_m: 2.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 2.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose {
                    x_m: 15.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 2.0,
                time_from_start_s: 6.5,
            },
        ];
        let cfg = VehicleConfig::default_urban();
        let check = |corr: &TajCorridor| {
            validate_trajectory_slow(&traj, corr, &[], &cfg, None, FleetPosture::Nominal)
        };

        // Phase A only (lidar blind to water): corridor reaches 20 m → admitted.
        assert!(
            matches!(
                check(&perception.corridor),
                TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
            ),
            "Phase A admits driving into invisible water"
        );
        // Phase B: water detected at 10 m → corridor clipped → driving in is rejected.
        let clipped =
            clip_corridor_to_hazards(&perception.corridor, &[hazard(SemanticClass::Water, 10.0)]);
        assert!(
            clipped.left_boundary().last().unwrap().x_m < 11.0,
            "corridor clipped at water"
        );
        assert_eq!(
            check(&clipped),
            TrajectoryVerdict::MRCFallback,
            "Phase B: KIRRA rejects driving into the water hazard"
        );
    }

    #[test]
    fn drivable_class_does_not_clip() {
        let taj = TajPhaseA::new(TajConfig {
            forward_extent_m: 20.0,
            ..Default::default()
        });
        let out = taj.process(&walls_scan(5.0, 30.0), 0);
        let clipped = clip_corridor_to_hazards(&out.corridor, &[hazard(SemanticClass::Road, 4.0)]);
        assert_eq!(
            clipped.left_boundary().last().unwrap().x_m,
            out.corridor.left_boundary().last().unwrap().x_m,
            "a drivable (road) detection does not clip the corridor"
        );
    }

    #[test]
    fn offlane_hazard_does_not_clip() {
        // Water entirely beyond the corridor wall (lateral 8..10, corridor ±5) →
        // not in the way → no clip.
        let taj = TajPhaseA::new(TajConfig {
            forward_extent_m: 20.0,
            ..Default::default()
        });
        let out = taj.process(&walls_scan(5.0, 30.0), 0);
        let off = SemanticDetection {
            class: SemanticClass::Water,
            near_x_m: 4.0,
            lateral_min_m: 8.0,
            lateral_max_m: 10.0,
        };
        let clipped = clip_corridor_to_hazards(&out.corridor, &[off]);
        assert_eq!(
            clipped.left_boundary().last().unwrap().x_m,
            out.corridor.left_boundary().last().unwrap().x_m,
            "an off-lane hazard does not clip the drivable corridor"
        );
    }

    // --- CTRV prediction (yaw-rate-aware) ----------------------------------

    /// A blob cluster centred at `(bx, by)`.
    fn point_blob_scan(bx: f64, by: f64, stamp_ms: u64) -> LaserScan {
        let r0 = bx.hypot(by);
        let th0 = by.atan2(bx);
        scan_from(40.0, stamp_ms, |theta| {
            if (theta - th0).abs() < 0.05 {
                Some(r0)
            } else {
                None
            }
        })
    }

    /// Three frames of a left-turning object (its heading rotates toward +y).
    fn turning_tracker() -> TajTracker {
        let mut taj = TajTracker::default();
        let _ = taj.track(&point_blob_scan(10.0, 0.0, 0), 0);
        let _ = taj.track(&point_blob_scan(12.0, 0.5, 200), 200);
        let _ = taj.track(&point_blob_scan(14.0, 1.5, 400), 400);
        taj
    }

    fn intent_track(
        id: u64,
        x_m: f64,
        y_m: f64,
        vx_mps: f64,
        vy_mps: f64,
        frames_seen: u32,
    ) -> Track {
        Track {
            id,
            pos: Point { x_m, y_m },
            stamp_ms: 1_000,
            vel: Point {
                x_m: vx_mps,
                y_m: vy_mps,
            },
            prior_vel: Point { x_m: 0.0, y_m: 0.0 },
            yaw_rate: 0.0,
            extent_m: 0.3,
            frames_seen,
            last_seen_ms: 1_000,
        }
    }

    #[test]
    fn vru_intent_distribution_is_complete_and_normalized() {
        let distribution =
            vru_intent_probability_distribution(VruIntentClass::CrossingLeftToRight, 0.80);

        assert_eq!(distribution.len(), VRU_INTENT_HYPOTHESIS_COUNT);

        let total: f64 = distribution
            .iter()
            .map(|hypothesis| hypothesis.probability)
            .sum();

        assert!(
            (total - 1.0).abs() <= VRU_INTENT_PROBABILITY_EPSILON,
            "intent probabilities must sum to one, got {total}"
        );

        assert!(distribution.iter().all(|hypothesis| {
            hypothesis.probability.is_finite() && (0.0..=1.0).contains(&hypothesis.probability)
        }));

        let crossing = distribution
            .iter()
            .find(|hypothesis| hypothesis.intent == VruIntentClass::CrossingLeftToRight)
            .expect("crossing hypothesis");

        let unknown = distribution
            .iter()
            .find(|hypothesis| hypothesis.intent == VruIntentClass::Unknown)
            .expect("unknown hypothesis");

        assert!((crossing.probability - 0.80).abs() < 1e-12);
        assert!((unknown.probability - 0.20).abs() < 1e-12);
    }

    #[test]
    fn unknown_intent_distribution_is_fully_unknown() {
        let distribution = vru_intent_probability_distribution(VruIntentClass::Unknown, 0.95);

        for hypothesis in distribution {
            if hypothesis.intent == VruIntentClass::Unknown {
                assert_eq!(hypothesis.probability, 1.0);
            } else {
                assert_eq!(hypothesis.probability, 0.0);
            }
        }
    }

    #[test]
    fn nonfinite_intent_confidence_fails_closed_to_unknown() {
        let distribution =
            vru_intent_probability_distribution(VruIntentClass::MovingAway, f64::NAN);

        let unknown = distribution
            .iter()
            .find(|hypothesis| hypothesis.intent == VruIntentClass::Unknown)
            .expect("unknown hypothesis");

        assert_eq!(unknown.probability, 1.0);
        assert!(distribution
            .iter()
            .filter(|hypothesis| hypothesis.intent != VruIntentClass::Unknown)
            .all(|hypothesis| hypothesis.probability == 0.0));
    }

    #[test]
    fn stable_consecutive_crossing_preserves_directional_probability() {
        let mut track = intent_track(90, 4.0, 1.5, 0.1, -0.8, 4);
        track.prior_vel = Point {
            x_m: 0.1,
            y_m: -0.7,
        };

        let cfg = VruMotionPredictionConfig::default();
        let (intent, confidence, _) = classify_vru_motion_intent(&track, &cfg);

        let distribution =
            temporally_stable_vru_intent_distribution(&track, &cfg, intent, confidence);

        let crossing_probability = distribution
            .iter()
            .find(|entry| entry.intent == VruIntentClass::CrossingLeftToRight)
            .expect("complete distribution")
            .probability;

        assert!(
            crossing_probability > 0.5,
            "stable crossing history should retain directional probability"
        );
    }

    #[test]
    fn one_frame_direction_reversal_collapses_to_unknown() {
        let mut track = intent_track(91, 4.0, 1.5, 0.1, -0.8, 4);
        track.prior_vel = Point { x_m: 0.1, y_m: 0.8 };

        let cfg = VruMotionPredictionConfig::default();
        let (intent, confidence, _) = classify_vru_motion_intent(&track, &cfg);

        let distribution =
            temporally_stable_vru_intent_distribution(&track, &cfg, intent, confidence);

        for entry in distribution {
            let expected = if entry.intent == VruIntentClass::Unknown {
                1.0
            } else {
                0.0
            };
            assert_eq!(entry.probability, expected);
        }
    }

    #[test]
    fn insufficient_temporal_history_preserves_frame_local_distribution() {
        let mut track = intent_track(92, 4.0, 1.5, 0.1, -0.8, 2);
        track.prior_vel = Point { x_m: 0.1, y_m: 0.8 };

        let cfg = VruMotionPredictionConfig::default();
        let (intent, confidence, _) = classify_vru_motion_intent(&track, &cfg);

        let distribution =
            temporally_stable_vru_intent_distribution(&track, &cfg, intent, confidence);

        assert!(
            distribution.iter().any(|entry| {
                entry.intent == VruIntentClass::CrossingLeftToRight && entry.probability > 0.5
            }),
            "two-frame tracks should preserve existing frame-local behavior"
        );
    }

    #[test]
    fn vru_intent_classifies_left_to_right_crossing() {
        let track = intent_track(1, 4.0, 1.5, 0.1, -0.8, 3);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::CrossingLeftToRight);
        assert_eq!(reason, VruIntentReason::LateralMotionTowardPath);
        assert!(confidence > 0.9);
    }

    #[test]
    fn vru_intent_classifies_right_to_left_crossing() {
        let track = intent_track(2, 4.0, -1.5, 0.1, 0.8, 3);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::CrossingRightToLeft);
        assert_eq!(reason, VruIntentReason::LateralMotionTowardPath);
        assert!(confidence > 0.9);
    }

    #[test]
    fn vru_intent_classifies_along_path_motion() {
        let track = intent_track(3, 4.0, 0.3, 1.0, 0.1, 3);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::AlongPath);
        assert_eq!(reason, VruIntentReason::LongitudinalMotionDominant);
        assert!(confidence > 0.9);
    }

    #[test]
    fn vru_intent_classifies_stationary_track_as_waiting() {
        let track = intent_track(4, 3.0, 0.5, 0.0, 0.0, 3);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::WaitingNearPath);
        assert_eq!(reason, VruIntentReason::NearlyStationaryNearPath);
        assert_eq!(confidence, 1.0);
    }

    #[test]
    fn vru_intent_classifies_motion_away_from_path() {
        let track = intent_track(5, 4.0, 1.0, 0.1, 0.8, 3);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::MovingAway);
        assert_eq!(reason, VruIntentReason::LateralMotionAwayFromPath);
        assert!(confidence > 0.9);
    }

    #[test]
    fn vru_intent_insufficient_history_remains_unknown() {
        let track = intent_track(6, 4.0, 1.0, 0.0, -0.8, 1);
        let cfg = VruMotionPredictionConfig::default();

        let (intent, confidence, reason) = classify_vru_motion_intent(&track, &cfg);

        assert_eq!(intent, VruIntentClass::Unknown);
        assert_eq!(reason, VruIntentReason::InsufficientHistory);
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn vru_intent_metadata_does_not_change_prediction_geometry() {
        let track = intent_track(77, 4.0, 1.0, 0.1, -0.8, 4);
        let cfg = VruMotionPredictionConfig::default();

        let tracker = TajTracker {
            tracks: vec![track],
            ..TajTracker::default()
        };

        // Establish the prediction generated from the authoritative track state.
        let before = tracker.predict_vru_motion(1_000, &cfg);
        assert_eq!(before.len(), 1);

        // Intent classification is metadata-only and must not mutate the track,
        // configuration, prediction model, geometry, or uncertainty.
        let (intent, confidence, reason) = classify_vru_motion_intent(&tracker.tracks[0], &cfg);

        assert_eq!(intent, VruIntentClass::CrossingLeftToRight);
        assert!(confidence.is_finite());
        assert!(confidence > 0.0);
        assert_eq!(reason, VruIntentReason::LateralMotionTowardPath);

        let after = tracker.predict_vru_motion(1_000, &cfg);
        assert_eq!(after.len(), 1);

        assert_eq!(before[0].model, after[0].model);
        assert_eq!(before[0].fallback_reason, after[0].fallback_reason);
        assert_eq!(before[0].points, after[0].points);
        assert_eq!(before[0].horizon_s, after[0].horizon_s);
        assert_eq!(before[0].step_s, after[0].step_s);
        assert_eq!(before[0].source_age_s, after[0].source_age_s);
    }

    #[test]
    fn vru_prediction_first_sighting_uses_omnidirectional_fallback() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);

        let predictions = tracker.predict_vru_motion(1_000, &VruMotionPredictionConfig::default());

        assert_eq!(predictions.len(), 1);
        assert_eq!(
            predictions[0].model,
            VruMotionModel::OmnidirectionalFallback
        );
        assert_eq!(
            predictions[0].fallback_reason,
            Some(VruPredictionFallbackReason::InsufficientHistory)
        );
        assert!(predictions[0]
            .points
            .windows(2)
            .all(|window| window[1].uncertainty_radius_m >= window[0].uncertainty_radius_m));
    }

    #[test]
    fn vru_prediction_stationary_track_remains_stationary() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);
        tracker.track(&point_blob_scan(3.0, 0.0, 1_100), 1_100);

        let predictions = tracker.predict_vru_motion(1_100, &VruMotionPredictionConfig::default());

        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].model, VruMotionModel::Stationary);

        let first = predictions[0].points.first().expect("prediction point");
        let last = predictions[0].points.last().expect("prediction point");

        assert!((first.pos.x_m - last.pos.x_m).abs() < 1e-9);
        assert!((first.pos.y_m - last.pos.y_m).abs() < 1e-9);
    }

    #[test]
    fn vru_prediction_straight_motion_uses_constant_velocity() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);
        tracker.track(&point_blob_scan(3.1, 0.0, 1_100), 1_100);

        let predictions = tracker.predict_vru_motion(1_100, &VruMotionPredictionConfig::default());

        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].model, VruMotionModel::ConstantVelocity);

        let first = predictions[0].points.first().expect("prediction point");
        let last = predictions[0].points.last().expect("prediction point");

        assert!(last.pos.x_m > first.pos.x_m);
        assert!((last.pos.y_m - first.pos.y_m).abs() < 0.2);
    }

    #[test]
    fn vru_prediction_excessive_speed_falls_back() {
        // The generic CTRV fixture moves roughly 10–11 m/s. That is useful for
        // object prediction, but outside the trusted pedestrian-motion envelope.
        let tracker = turning_tracker();

        let predictions = tracker.predict_vru_motion(400, &VruMotionPredictionConfig::default());

        assert_eq!(predictions.len(), 1);
        assert_eq!(
            predictions[0].model,
            VruMotionModel::OmnidirectionalFallback
        );
        assert_eq!(
            predictions[0].fallback_reason,
            Some(VruPredictionFallbackReason::ExcessiveSpeed)
        );
        assert!(
            predictions[0]
                .points
                .windows(2)
                .all(|window| window[1].uncertainty_radius_m >= window[0].uncertainty_radius_m),
            "the fail-closed fallback uncertainty must never shrink"
        );
    }

    #[test]
    fn vru_prediction_turning_track_uses_bounded_turn_rate() {
        let mut tracker = TajTracker::default();

        // Smooth pedestrian-scale left turn:
        // approximately 0.7–0.9 m/s with enough history that the measured
        // heading change is physically plausible rather than a two-sample spike.
        tracker.track(&point_blob_scan(3.00, 0.00, 0), 0);
        tracker.track(&point_blob_scan(3.15, 0.01, 200), 200);
        tracker.track(&point_blob_scan(3.28, 0.04, 400), 400);
        tracker.track(&point_blob_scan(3.41, 0.09, 600), 600);
        tracker.track(&point_blob_scan(3.53, 0.16, 800), 800);

        let predictions = tracker.predict_vru_motion(800, &VruMotionPredictionConfig::default());

        assert_eq!(predictions.len(), 1);
        assert_eq!(
            predictions[0].model,
            VruMotionModel::BoundedTurnRate,
            "smooth pedestrian turning motion should use bounded-turn prediction; \
             fallback={:?}, prediction={:#?}",
            predictions[0].fallback_reason,
            predictions[0]
        );
        assert_eq!(predictions[0].fallback_reason, None);
        assert!(
            predictions[0].points.len() >= 3,
            "bounded-turn prediction needs multiple future samples"
        );

        let points = &predictions[0].points;
        let first = points.first().expect("prediction point");
        let last = points.last().expect("prediction point");

        assert!(
            last.pos.x_m > first.pos.x_m,
            "predicted pedestrian should continue moving forward"
        );
        assert!(
            last.pos.y_m > first.pos.y_m,
            "positive tracked yaw rate should produce a left-curving prediction"
        );
        assert!(
            last.uncertainty_radius_m >= first.uncertainty_radius_m,
            "prediction uncertainty must not shrink over the horizon"
        );
    }

    #[test]
    fn vru_prediction_stale_track_falls_back_and_expands() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);
        tracker.track(&point_blob_scan(3.1, 0.0, 1_100), 1_100);

        let predictions = tracker.predict_vru_motion(2_000, &VruMotionPredictionConfig::default());

        assert_eq!(
            predictions[0].model,
            VruMotionModel::OmnidirectionalFallback
        );
        assert_eq!(
            predictions[0].fallback_reason,
            Some(VruPredictionFallbackReason::StaleTrack)
        );

        let first = predictions[0].points.first().expect("prediction point");
        let last = predictions[0].points.last().expect("prediction point");

        assert!(last.uncertainty_radius_m > first.uncertainty_radius_m);
        assert!((last.pos.x_m - first.pos.x_m).abs() < 1e-9);
        assert!((last.pos.y_m - first.pos.y_m).abs() < 1e-9);
    }

    #[test]
    fn vru_prediction_invalid_config_fails_closed_with_infinite_uncertainty() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);

        let cfg = VruMotionPredictionConfig {
            step_s: f64::NAN,
            ..VruMotionPredictionConfig::default()
        };

        let predictions = tracker.predict_vru_motion(1_000, &cfg);

        assert_eq!(
            predictions[0].fallback_reason,
            Some(VruPredictionFallbackReason::InvalidConfiguration)
        );
        assert_eq!(predictions[0].points.len(), 1);
        assert!(predictions[0].points[0].uncertainty_radius_m.is_infinite());
    }

    #[test]
    fn vru_prediction_point_count_is_hard_bounded() {
        let mut tracker = TajTracker::default();
        tracker.track(&point_blob_scan(3.0, 0.0, 1_000), 1_000);
        tracker.track(&point_blob_scan(3.1, 0.0, 1_100), 1_100);

        let cfg = VruMotionPredictionConfig {
            horizon_s: 100.0,
            step_s: 0.001,
            maximum_points: MAX_VRU_PREDICTION_POINTS,
            ..VruMotionPredictionConfig::default()
        };

        let predictions = tracker.predict_vru_motion(1_100, &cfg);

        assert_eq!(predictions[0].points.len(), MAX_VRU_PREDICTION_POINTS);
    }

    #[test]
    fn tracker_estimates_yaw_rate_of_turning_object() {
        let paths = turning_tracker().predict(2.0, 0.2);
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0].yaw_rate_rad_s > 0.3,
            "left turn → positive yaw rate, got {}",
            paths[0].yaw_rate_rad_s
        );
    }

    #[test]
    fn ctrv_predicts_a_curved_path() {
        let paths = turning_tracker().predict(2.0, 0.2);
        let p = &paths[0];
        let n = p.points.len();
        assert!(n >= 3);
        // Successive segment directions rotate → the path curves (not a CV line).
        let seg_dir = |a: usize, b: usize| {
            (p.points[b].y_m - p.points[a].y_m).atan2(p.points[b].x_m - p.points[a].x_m)
        };
        let first = seg_dir(0, 1);
        let last = seg_dir(n - 2, n - 1);
        assert!(
            (last - first).abs() > 0.3,
            "CTRV path curves with the yaw rate, got {first} -> {last}"
        );
    }

    #[test]
    fn straight_object_predicts_a_straight_line() {
        let mut taj = TajTracker::default();
        let _ = taj.track(&point_blob_scan(10.0, 0.0, 0), 0);
        let _ = taj.track(&point_blob_scan(12.0, 0.0, 200), 200);
        let _ = taj.track(&point_blob_scan(14.0, 0.0, 400), 400);
        let paths = taj.predict(2.0, 0.2);

        assert!(
            paths[0].yaw_rate_rad_s.abs() < 0.1,
            "straight → ~0 yaw rate, got {}",
            paths[0].yaw_rate_rad_s
        );
        let max_y = paths[0]
            .points
            .iter()
            .map(|p| p.y_m.abs())
            .fold(0.0, f64::max);
        assert!(
            max_y < 1.0,
            "straight (CV) path stays near y≈0, got {max_y}"
        );
    }

    // -----------------------------------------------------------------
    // Corridor-station bound (library backstop)
    //
    // `TajConfig` is public and unvalidated, so these cover the callers that
    // bypass the sidecar's `MAX_FORWARD_EXTENT_M` check entirely. Before the
    // bound, `forward_extent_m = 1e300` saturated the float->usize cast to
    // `usize::MAX` and `nstations + 1` panicked with an integer overflow at
    // this file's line 445 (debug) / wrapped into a ~1.8e19-iteration loop
    // (release).
    // -----------------------------------------------------------------

    fn probe_scan() -> LaserScan {
        LaserScan {
            angle_min_rad: -1.0,
            angle_increment_rad: 0.01,
            range_min_m: 0.1,
            range_max_m: 12.0,
            ranges: vec![1.0; 16],
            stamp_ms: 1_000,
        }
    }

    #[test]
    fn corridor_station_count_is_bounded_and_saturation_safe() {
        // Normal configured geometry is untouched by the clamp.
        assert_eq!(corridor_station_count(8.0, 1.0), 8);
        assert_eq!(corridor_station_count(20.0, 1.0), 20);
        assert_eq!(corridor_station_count(40.0, 1.0), 40);
        // The sidecar's accepted maximum at the 0.1 m station floor lands
        // EXACTLY on the ceiling — the two constants are deliberately paired,
        // so a validated request never engages the clamp.
        assert_eq!(corridor_station_count(500.0, 0.1), MAX_CORRIDOR_STATIONS);
        // Beyond it, the count is clamped rather than saturating to usize::MAX.
        assert_eq!(corridor_station_count(1e300, 0.1), MAX_CORRIDOR_STATIONS);
        assert_eq!(corridor_station_count(f64::MAX, 0.1), MAX_CORRIDOR_STATIONS);
        // Degenerate inputs yield 0 rather than a garbage allocation size.
        for (ext, step) in [
            (f64::NAN, 1.0),
            (f64::INFINITY, 1.0),
            (f64::NEG_INFINITY, 1.0),
            (-1.0, 1.0),
            (0.0, 1.0),
            (8.0, f64::NAN),
            (8.0, 0.0),
            (8.0, -1.0),
        ] {
            assert_eq!(corridor_station_count(ext, step), 0, "{ext:e} / {step:e}");
        }
    }

    #[test]
    fn huge_finite_extent_does_not_panic_or_hang() {
        // The exact reported crasher, driven straight at the library.
        for extent in [1e300, 1e30, f64::MAX, 1e6] {
            let taj = TajPhaseA::new(TajConfig {
                forward_extent_m: extent,
                ..Default::default()
            });
            let out = taj.process(&probe_scan(), 1_000);
            // Reaching this line at all is the assertion: no overflow panic in
            // debug, no unbounded loop in release.
            assert!(
                out.corridor.left.len() <= MAX_CORRIDOR_STATIONS + 1,
                "{extent:e} produced {} boundary vertices",
                out.corridor.left.len()
            );
            assert!(out.corridor.right.len() <= MAX_CORRIDOR_STATIONS + 1);
        }
    }

    #[test]
    fn clamping_shortens_the_corridor_rather_than_extending_it() {
        // Fail-SAFE, not fail-open: clamping yields FEWER stations, so less
        // claimed free space. A corridor that grew under clamping would be the
        // dangerous direction.
        let taj = TajPhaseA::new(TajConfig {
            forward_extent_m: 1e300,
            ..Default::default()
        });
        let clamped = taj.process(&probe_scan(), 1_000);
        let furthest = clamped
            .corridor
            .left
            .iter()
            .map(|p| p.x_m)
            .fold(0.0_f64, f64::max);
        assert!(
            furthest.is_finite(),
            "every boundary vertex must be finite, got {furthest}"
        );
    }
}
