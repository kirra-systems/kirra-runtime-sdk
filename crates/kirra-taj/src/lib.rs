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

use kirra_ros2_adapter::corridor::{CorridorSource, Point};
use kirra_ros2_adapter::state::PerceivedObject;

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
            let f = if dx.abs() > 1e-9 { (x - w[0].x_m) / dx } else { 0.0 };
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
                let f = if dx.abs() > 1e-9 { (x_clip - a.x_m) / dx } else { 0.0 };
                out.push(Point { x_m: x_clip, y_m: a.y_m + f * (p.y_m - a.y_m) });
            }
            break;
        }
    }
    out
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
    let mut x_clip = f64::INFINITY;
    for d in detections {
        if d.class.is_drivable() {
            continue;
        }
        // Does the hazard's lateral span overlap the corridor at its near edge?
        let left = boundary_y_at(&corridor.left, d.near_x_m);
        let right = boundary_y_at(&corridor.right, d.near_x_m);
        if d.lateral_max_m > right && d.lateral_min_m < left {
            x_clip = x_clip.min(d.near_x_m);
        }
    }
    if !x_clip.is_finite() {
        return corridor.clone();
    }
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
        let points = self.scan_to_points(scan);
        // Confidence = fraction of rays that produced a valid return. An empty /
        // all-invalid scan → 0.0 → the corridor is unhealthy → checker MRCs.
        let total = scan.ranges.len().max(1);
        let confidence = (points.len() as f32 / total as f32).clamp(0.0, 1.0);

        let corridor = self.extract_corridor(&points, confidence, now_ms, scan.stamp_ms);
        let objects = self.cluster_objects(&points);
        TajPerception { corridor, objects, stamp_ms: scan.stamp_ms }
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

        let nstations = (ext / step).ceil() as usize;
        let mut left = Vec::with_capacity(nstations + 1);
        let mut right = Vec::with_capacity(nstations + 1);
        for i in 0..=nstations {
            let xs = (i as f64 * step).min(ext);
            let mut left_y = cap; // nearest left obstacle in this station's window
            let mut right_y = -cap; // nearest right obstacle
            for &(x, y) in points {
                if x <= 0.0 || x > ext || (x - xs).abs() > step {
                    continue; // forward cone, local window only
                }
                if y > 0.0 && y < left_y {
                    left_y = y;
                } else if y < 0.0 && y > right_y {
                    right_y = y;
                }
            }
            left.push(Point { x_m: xs, y_m: left_y.clamp(1e-3, cap) });
            right.push(Point { x_m: xs, y_m: right_y.clamp(-cap, -1e-3) });
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
    fn cluster_objects(&self, points: &[(f64, f64)]) -> Vec<PerceivedObject> {
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
                    objects.push(PerceivedObject {
                        id: next_id,
                        pos: Point { x_m: sx / n, y_m: sy / n },
                        velocity_mps: 0.0,
                        heading_rad: 0.0,
                        vel: Point { x_m: 0.0, y_m: 0.0 },
                    });
                    next_id += 1;
                }
                start = i;
            }
        }
        objects
    }
}

/// One persistent object track: a stable id + last position/stamp, kept across
/// frames so the next frame can estimate velocity from displacement.
#[derive(Debug, Clone, Copy)]
struct Track {
    id: u64,
    pos: Point,
    stamp_ms: u64,
    /// Latest estimated ground velocity (m/s) — retained for CTRV prediction.
    vel: Point,
    /// Latest estimated yaw rate (rad/s) from the heading change across frames.
    yaw_rate: f64,
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

/// Taj **temporal tracker** — wraps the Phase-A geometric pipeline and adds
/// frame-to-frame object association + velocity estimation. Phase-A is
/// single-frame (every object static); the tracker associates each new detection
/// with the nearest prior track (within `track_assoc_gate_m`), estimates its
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

impl TajTracker {
    #[must_use]
    pub fn new(cfg: TajConfig) -> Self {
        Self { phase_a: TajPhaseA::new(cfg), tracks: Vec::new(), next_id: 0 }
    }

    /// Process a scan with temporal association: returns Phase-A perception whose
    /// objects carry persistent ids and estimated velocity.
    pub fn track(&mut self, scan: &LaserScan, now_ms: u64) -> TajPerception {
        let mut perception = self.phase_a.process(scan, now_ms);
        let gate = self.phase_a.cfg.track_assoc_gate_m;

        let mut next_tracks: Vec<Track> = Vec::with_capacity(perception.objects.len());
        let mut used = vec![false; self.tracks.len()];

        for obj in &mut perception.objects {
            // Nearest unused prior track within the association gate.
            let mut best: Option<(usize, f64)> = None;
            for (j, tr) in self.tracks.iter().enumerate() {
                if used[j] {
                    continue;
                }
                let d = (obj.pos.x_m - tr.pos.x_m).hypot(obj.pos.y_m - tr.pos.y_m);
                if d <= gate && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((j, d));
                }
            }

            match best {
                Some((j, _)) => {
                    used[j] = true;
                    let tr = self.tracks[j];
                    let dt = f64::from(u32::try_from(now_ms.saturating_sub(tr.stamp_ms)).unwrap_or(u32::MAX))
                        / 1000.0;
                    let (vx, vy) = if dt > 1e-3 {
                        ((obj.pos.x_m - tr.pos.x_m) / dt, (obj.pos.y_m - tr.pos.y_m) / dt)
                    } else {
                        (0.0, 0.0)
                    };
                    obj.id = tr.id;
                    obj.vel = Point { x_m: vx, y_m: vy };
                    obj.velocity_mps = vx.hypot(vy);
                    obj.heading_rad = if obj.velocity_mps > 1e-6 { vy.atan2(vx) } else { 0.0 };
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
                        yaw_rate,
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
                        yaw_rate: 0.0,
                    });
                }
            }
        }
        self.tracks = next_tracks;
        perception
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
                PredictedPath { id: t.id, yaw_rate_rad_s: t.yaw_rate, points }
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
    use kirra_ros2_adapter::state::TrajectoryVerdict;
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
        assert!(out.corridor.left_boundary().len() >= 2, "per-station boundary");
        assert_eq!(out.corridor.age_ms(), 5);
    }

    #[test]
    fn obstacle_ahead_clusters_to_one_object() {
        // A ~±8.6° blob of returns at 5 m dead ahead; everything else no-return.
        let taj = TajPhaseA::default();
        let scan = scan_from(10.0, 0, |theta| if theta.abs() < 0.15 { Some(5.0) } else { None });
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

    #[test]
    fn taj_corridor_feeds_the_checker() {
        use kirra_ros2_adapter::state::{Pose, TrajectoryPoint};
        use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
        use kirra_runtime_sdk::verifier::FleetPosture;

        // A 3 m half-width corridor from walls; feed a short centered trajectory.
        let taj = TajPhaseA::default();
        let out = taj.process(&walls_scan(3.0, 12.0), 1);

        let traj = vec![
            TrajectoryPoint {
                pose: Pose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose { x_m: 3.0, y_m: 0.0, heading_rad: 0.0 },
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
        assert_eq!(out.corridor.confidence(), 0.0, "no valid returns → zero confidence");
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
            let wall = if s.abs() < 1e-3 { f64::INFINITY } else { 5.0 / s.abs() };
            let blob = if theta.abs() < 0.12 { 4.0 / theta.cos() } else { f64::INFINITY };
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
        assert!(y_near(4.0) < 1.0, "corridor narrows near the obstacle, got {}", y_near(4.0));
        assert!(y_near(7.0) > 3.0, "corridor stays wide away from it, got {}", y_near(7.0));
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

    #[test]
    fn tracker_estimates_velocity_of_moving_object() {
        // Object moves +2 m over a 200 ms inter-frame gap → ~10 m/s in +x.
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        let out = taj.track(&blob_scan(12.0, 200), 200);

        assert_eq!(out.objects.len(), 1);
        let o = &out.objects[0];
        assert!((o.velocity_mps - 10.0).abs() < 1.0, "speed ≈ 10 m/s, got {}", o.velocity_mps);
        assert!(o.vel.x_m > 8.0 && o.vel.y_m.abs() < 1.0, "velocity is +x, got {:?}", o.vel);
        assert!(o.heading_rad.abs() < 0.2, "heading ≈ 0 (forward), got {}", o.heading_rad);
    }

    #[test]
    fn tracker_reports_zero_velocity_for_static_object() {
        let mut taj = TajTracker::default();
        let _ = taj.track(&blob_scan(10.0, 0), 0);
        let out = taj.track(&blob_scan(10.0, 200), 200);

        assert_eq!(out.objects.len(), 1);
        assert!(out.objects[0].velocity_mps < 0.5, "static → ~0 m/s, got {}", out.objects[0].velocity_mps);
    }

    #[test]
    fn tracker_persists_track_id() {
        let mut taj = TajTracker::default();
        let a = taj.track(&blob_scan(10.0, 0), 0);
        let b = taj.track(&blob_scan(11.0, 200), 200);
        assert_eq!(a.objects[0].id, b.objects[0].id, "same object keeps its track id");
    }

    #[test]
    fn tracker_first_sighting_is_zero_velocity() {
        let mut taj = TajTracker::default();
        let out = taj.track(&blob_scan(10.0, 0), 0);
        assert_eq!(out.objects.len(), 1);
        assert_eq!(out.objects[0].velocity_mps, 0.0, "first sighting has no prior → 0 velocity");
    }

    #[test]
    fn object_velocity_changes_rss_verdict() {
        // WHY tracking matters: a fast-receding lead that Phase-A would treat as
        // PARKED (velocity 0) is wrongly RSS-rejected, but with the tracked
        // velocity the checker correctly admits it. Same geometry, only the
        // object's velocity differs.
        use kirra_ros2_adapter::corridor::MockCorridorSource;
        use kirra_ros2_adapter::state::{Pose, TrajectoryPoint};
        use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
        use kirra_runtime_sdk::verifier::FleetPosture;

        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let tp = |x: f64, t: f64| TrajectoryPoint {
            pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
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
            pos: Point { x_m: 22.0, y_m: 2.0 },
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
        SemanticDetection { class, near_x_m: near_x, lateral_min_m: -5.0, lateral_max_m: 5.0 }
    }

    #[test]
    fn drivability_policy_is_conservative() {
        assert!(SemanticClass::Road.is_drivable());
        assert!(!SemanticClass::Water.is_drivable());
        assert!(!SemanticClass::StaticObstacle.is_drivable());
        assert!(!SemanticClass::Unknown.is_drivable(), "Unknown fails closed");
    }

    #[test]
    fn mock_detector_returns_scripted_detections() {
        let det = MockSemanticDetector { detections: vec![hazard(SemanticClass::Water, 8.0)] };
        let out = det.detect();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, SemanticClass::Water);
    }

    #[test]
    fn water_hazard_clips_corridor_forward_extent() {
        // A clear (Phase-A) corridor extends to forward_extent; a water hazard at
        // 4 m clips the drivable space to ~4 m.
        let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
        let out = taj.process(&walls_scan(5.0, 30.0), 0);
        let far = out.corridor.left_boundary().last().unwrap().x_m;
        assert!(far > 15.0, "clear corridor reaches far, got {far}");

        let clipped = clip_corridor_to_hazards(&out.corridor, &[hazard(SemanticClass::Water, 4.0)]);
        let clipped_far = clipped.left_boundary().last().unwrap().x_m;
        assert!((clipped_far - 4.0).abs() < 1.0, "corridor clipped at the water edge, got {clipped_far}");
    }

    #[test]
    fn kirra_rejects_driving_into_water() {
        // THE Phase-B win: Phase A reports a lake as free corridor; a trajectory
        // drives into it and KIRRA ADMITS (it cannot see water). After semantic
        // fusion clips the corridor at the water edge, the SAME trajectory is
        // REJECTED — the hazard Phase A was blind to is now enforced.
        use kirra_ros2_adapter::corridor::CorridorSource as _;
        use kirra_ros2_adapter::state::{Pose, TrajectoryPoint};
        use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
        use kirra_runtime_sdk::verifier::FleetPosture;

        let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
        let perception = taj.process(&walls_scan(5.0, 30.0), 0);

        // A trajectory driving forward to x = 15 (into where the water is).
        let traj = vec![
            TrajectoryPoint { pose: Pose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 }, velocity_mps: 2.0, time_from_start_s: 0.0 },
            TrajectoryPoint { pose: Pose { x_m: 15.0, y_m: 0.0, heading_rad: 0.0 }, velocity_mps: 2.0, time_from_start_s: 6.5 },
        ];
        let cfg = VehicleConfig::default_urban();
        let check = |corr: &TajCorridor| {
            validate_trajectory_slow(&traj, corr, &[], &cfg, None, FleetPosture::Nominal)
        };

        // Phase A only (lidar blind to water): corridor reaches 20 m → admitted.
        assert!(
            matches!(check(&perception.corridor), TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "Phase A admits driving into invisible water"
        );
        // Phase B: water detected at 10 m → corridor clipped → driving in is rejected.
        let clipped = clip_corridor_to_hazards(&perception.corridor, &[hazard(SemanticClass::Water, 10.0)]);
        assert!(clipped.left_boundary().last().unwrap().x_m < 11.0, "corridor clipped at water");
        assert_eq!(
            check(&clipped),
            TrajectoryVerdict::MRCFallback,
            "Phase B: KIRRA rejects driving into the water hazard"
        );
    }

    #[test]
    fn drivable_class_does_not_clip() {
        let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
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
        let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
        let out = taj.process(&walls_scan(5.0, 30.0), 0);
        let off = SemanticDetection { class: SemanticClass::Water, near_x_m: 4.0, lateral_min_m: 8.0, lateral_max_m: 10.0 };
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
        scan_from(40.0, stamp_ms, |theta| if (theta - th0).abs() < 0.05 { Some(r0) } else { None })
    }

    /// Three frames of a left-turning object (its heading rotates toward +y).
    fn turning_tracker() -> TajTracker {
        let mut taj = TajTracker::default();
        let _ = taj.track(&point_blob_scan(10.0, 0.0, 0), 0);
        let _ = taj.track(&point_blob_scan(12.0, 0.5, 200), 200);
        let _ = taj.track(&point_blob_scan(14.0, 1.5, 400), 400);
        taj
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

        assert!(paths[0].yaw_rate_rad_s.abs() < 0.1, "straight → ~0 yaw rate, got {}", paths[0].yaw_rate_rad_s);
        let max_y = paths[0].points.iter().map(|p| p.y_m.abs()).fold(0.0, f64::max);
        assert!(max_y < 1.0, "straight (CV) path stays near y≈0, got {max_y}");
    }
}

