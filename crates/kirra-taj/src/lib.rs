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
}

impl Default for TajConfig {
    fn default() -> Self {
        Self {
            forward_extent_m: 8.0,
            open_half_width_m: 5.0,
            cluster_gap_m: 0.5,
            min_cluster_points: 3,
            corridor_station_m: 1.0,
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
}
