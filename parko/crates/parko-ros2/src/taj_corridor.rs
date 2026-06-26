// parko/crates/parko-ros2/src/taj_corridor.rs
//
// ADR-0029 Phase 3b — the live ego-relative corridor source for the Phase-3a
// containment gate. Runs Taj Phase-A (`kirra-taj`) on a `LaserScan` to produce a
// drivable corridor, then converts it into a `kirra_core::containment::Corridor`
// snapshot the gate (`apply_containment_gate`) consumes.
//
// This is the PURE, sim-testable half (Taj Phase-A is model-free geometry — no
// ROS, no hardware): a synthetic scan → corridor snapshot → the gate admits an
// in-corridor command and MRCs one that veers out. The ros2-gated half (the
// lidar subscription + per-tick drive) lives in `node.rs`, exactly as Phase 2's
// node-binary governor swap and Phase 3a's seam split.
//
// Frame: Taj measures angles from ego +X (forward), +Y left — already
// ego-relative, which is the whole basis for the Phase-3 ego-frame design
// (no global localization needed; FrameTrust::Trusted inside the gate).
//
// Point conversion: Taj's `CorridorSource` returns `kirra_core::corridor::Point`;
// the kernel `Corridor` takes `kirra_core::containment::Point`. Both are
// `{ x_m, y_m }` — a field-for-field copy (mirrors the SDK adapter's
// `adapter_to_kernel_point`).

use kirra_core::containment::{Corridor, Point as KernelPoint};
use kirra_core::corridor::CorridorSource;
use kirra_taj::{LaserScan, TajCorridor, TajPhaseA};

/// An OWNED ego-relative corridor snapshot — the boundaries copied out of a
/// `TajCorridor` so the node can hold the latest in an `Arc<Mutex<Option<_>>>`
/// and borrow it into a `Corridor<'a>` per tick (the kernel `Corridor` borrows
/// its boundary slices).
#[derive(Debug, Clone, PartialEq)]
pub struct CorridorSnapshot {
    left: Vec<KernelPoint>,
    right: Vec<KernelPoint>,
    confidence: f32,
    age_ms: u64,
}

impl CorridorSnapshot {
    /// Copy a `TajCorridor`'s boundaries into an owned snapshot, converting
    /// `corridor::Point` → `containment::Point` field-for-field.
    #[must_use]
    pub fn from_taj(corridor: &TajCorridor) -> Self {
        let conv = |p: &kirra_core::corridor::Point| KernelPoint { x_m: p.x_m, y_m: p.y_m };
        Self {
            left: corridor.left_boundary().iter().map(conv).collect(),
            right: corridor.right_boundary().iter().map(conv).collect(),
            confidence: corridor.confidence(),
            age_ms: corridor.age_ms(),
        }
    }

    /// Extend the (forward-cone) corridor BACKWARD by `rear_m` so it covers the
    /// ego's OWN footprint, which extends behind the origin (a center-convention
    /// diff-drive footprint reaches `overhang_rear` aft of the pose). Taj's
    /// corridor starts at `x = 0`; without this, the stationary ego footprint's
    /// rear reads as a longitudinal departure at the very first lookahead pose.
    /// The near-field clearance (the first station's half-width) is extruded
    /// straight back — a safe assertion, since the ego already sits there. No-op
    /// on an empty corridor.
    #[must_use]
    pub fn with_ego_rear_cover(mut self, rear_m: f64) -> Self {
        if let (Some(&l0), Some(&r0)) = (self.left.first(), self.right.first()) {
            self.left.insert(0, KernelPoint { x_m: l0.x_m - rear_m, y_m: l0.y_m });
            self.right.insert(0, KernelPoint { x_m: r0.x_m - rear_m, y_m: r0.y_m });
        }
        self
    }

    /// Borrow the snapshot into a kernel [`Corridor`] with the configured health
    /// gates (`min_confidence` / `max_age_ms`). The gate's containment check
    /// then enforces those — a low-confidence or stale corridor fails closed.
    #[must_use]
    pub fn to_corridor(&self, min_confidence: f32, max_age_ms: u64) -> Corridor<'_> {
        Corridor {
            left: &self.left,
            right: &self.right,
            confidence: self.confidence,
            age_ms: self.age_ms,
            min_confidence,
            max_age_ms,
        }
    }
}

/// Backward extension (m) applied to the live corridor so it covers the ego's
/// own footprint behind the origin: courier rear overhang (0.45 m) + the 0.40 m
/// SG2 containment margin + slack. See [`CorridorSnapshot::with_ego_rear_cover`].
pub const EGO_REAR_COVER_M: f64 = 1.5;

/// Run Taj Phase-A on a scan and snapshot the resulting corridor, extended to
/// cover the ego footprint ([`EGO_REAR_COVER_M`]). `now_ms` is the wall clock at
/// processing time (Taj derives `age_ms` from `scan.stamp_ms`). This is the
/// production path the node feeds into the gate.
#[must_use]
pub fn corridor_from_scan(taj: &TajPhaseA, scan: &LaserScan, now_ms: u64) -> CorridorSnapshot {
    CorridorSnapshot::from_taj(&taj.process(scan, now_ms).corridor).with_ego_rear_cover(EGO_REAR_COVER_M)
}

/// Plain mirror of the `sensor_msgs/LaserScan` fields Taj reads — r2r-free, so
/// the scan-conversion CORE is unit-tested in the default build (mirrors the
/// `imu_shim` pure-core + thin-r2r-adapter split).
#[derive(Debug, Clone, PartialEq)]
pub struct LaserScanRawFields {
    pub angle_min_rad: f64,
    pub angle_increment_rad: f64,
    pub range_min_m: f64,
    pub range_max_m: f64,
    pub ranges: Vec<f32>,
    /// `header.stamp.sec` (ROS Time seconds).
    pub stamp_sec: i64,
    /// `header.stamp.nanosec`.
    pub stamp_nanosec: u32,
}

/// Pure field copy into `kirra_taj::LaserScan`, computing `stamp_ms` from the ROS
/// `Time` (`sec*1000 + nanosec/1e6`; a negative sec clamps to 0). r2r-free.
#[must_use]
pub fn taj_scan_from_raw(raw: &LaserScanRawFields) -> LaserScan {
    let stamp_ms = (raw.stamp_sec.max(0) as u64) * 1000 + (raw.stamp_nanosec as u64) / 1_000_000;
    LaserScan {
        angle_min_rad: raw.angle_min_rad,
        angle_increment_rad: raw.angle_increment_rad,
        range_min_m: raw.range_min_m,
        range_max_m: raw.range_max_m,
        ranges: raw.ranges.clone(),
        stamp_ms,
    }
}

/// THIN r2r ADAPTER — extraction only. Compiles only under `--features ros2`.
#[cfg(feature = "ros2")]
#[must_use]
pub fn laserscan_msg_to_taj(msg: &r2r::sensor_msgs::msg::LaserScan) -> LaserScan {
    taj_scan_from_raw(&LaserScanRawFields {
        angle_min_rad: msg.angle_min as f64,
        angle_increment_rad: msg.angle_increment as f64,
        range_min_m: msg.range_min as f64,
        range_max_m: msg.range_max as f64,
        ranges: msg.ranges.clone(),
        stamp_sec: msg.header.stamp.sec as i64,
        stamp_nanosec: msg.header.stamp.nanosec,
    })
}

// SAFETY: SG2 | REQ: parko-ros2-taj-corridor-source | TEST: walls_scan_bounds_the_corridor_and_gate_admits_straight,walls_scan_gate_mrcs_a_command_veering_into_the_wall,far_walls_yield_a_wide_corridor_admitting_a_gentle_curve,no_return_scan_fails_closed,snapshot_round_trips_taj_boundaries,taj_scan_from_raw_copies_fields_and_computes_stamp_ms
#[cfg(test)]
mod tests {
    use super::*;
    use crate::containment_gate::command_stays_in_corridor_default;
    use kirra_core::kinematics_contract::EnforceAction;
    use parko_core::commands::ControlCommand;

    /// A synthetic forward-facing scan with two straight walls at `y = ±half`
    /// (ego at the origin, +X forward). Rays span [-π/2, +π/2]; each ray's range
    /// is the distance to whichever wall it hits first (or `range_max` ahead).
    fn walls_scan(half_width_m: f64, stamp_ms: u64) -> LaserScan {
        let n = 181;
        let angle_min = -std::f64::consts::FRAC_PI_2;
        let inc = std::f64::consts::PI / (n as f64 - 1.0);
        let range_max = 10.0;
        let ranges = (0..n)
            .map(|i| {
                let a = angle_min + i as f64 * inc;
                let sin = a.sin();
                // Distance along the ray to the nearer wall (|y| = half).
                let r = if sin.abs() < 1e-6 { range_max } else { (half_width_m / sin.abs()).min(range_max) };
                r as f32
            })
            .collect();
        LaserScan {
            angle_min_rad: angle_min,
            angle_increment_rad: inc,
            range_min_m: 0.05,
            range_max_m: range_max,
            ranges,
            stamp_ms,
        }
    }

    /// A no-return scan — every ray is at/beyond `range_max` (no obstacle seen).
    /// Taj's confidence is `returns / total` → ~0 → an UNHEALTHY corridor.
    fn no_return_scan(stamp_ms: u64) -> LaserScan {
        let n = 181;
        LaserScan {
            angle_min_rad: -std::f64::consts::FRAC_PI_2,
            angle_increment_rad: std::f64::consts::PI / (n as f64 - 1.0),
            range_min_m: 0.05,
            range_max_m: 10.0,
            ranges: vec![f32::INFINITY; n],
            stamp_ms,
        }
    }

    fn cmd(v: f64, w: f64) -> ControlCommand {
        ControlCommand { linear_velocity: v, angular_velocity: w, timestamp_ms: 0 }
    }

    fn courier_footprint() -> kirra_core::containment::VehicleFootprint {
        crate::platform_profile::CourierPlatformProfile::courier_reference().footprint()
    }

    #[test]
    fn walls_scan_bounds_the_corridor_and_gate_admits_straight() {
        // Walls at ±1.2 m → corridor half-width ~1.2 m. Courier half-width 0.3 +
        // 0.40 margin = 0.70 < 1.2 → a straight command is contained.
        let taj = TajPhaseA::new(kirra_taj::TajConfig::default());
        let snap = corridor_from_scan(&taj, &walls_scan(1.2, 100), 100);
        assert!(!snap.left.is_empty() && !snap.right.is_empty(), "Taj must produce a bounded corridor");
        let corridor = snap.to_corridor(0.5, 500);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.0, 0.0), &corridor);
        assert!(matches!(verdict, EnforceAction::Allow),
            "a straight command in a ~1.2 m corridor must be admitted; got {verdict:?}");
    }

    #[test]
    fn walls_scan_gate_mrcs_a_command_veering_into_the_wall() {
        // Same ±1.2 m walls; a hard yaw curves the lookahead into the wall.
        let taj = TajPhaseA::new(kirra_taj::TajConfig::default());
        let snap = corridor_from_scan(&taj, &walls_scan(1.2, 100), 100);
        let corridor = snap.to_corridor(0.5, 500);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.5, 3.0), &corridor);
        assert!(matches!(verdict, EnforceAction::DenyBreach(_)),
            "a command curving into the wall must MRC; got {verdict:?}");
    }

    #[test]
    fn far_walls_yield_a_wide_corridor_admitting_a_gentle_curve() {
        // Walls far out at ±3 m (returns present → healthy) → a wide corridor; a
        // gentle curve stays contained.
        let taj = TajPhaseA::new(kirra_taj::TajConfig::default());
        let snap = corridor_from_scan(&taj, &walls_scan(3.0, 100), 100);
        let corridor = snap.to_corridor(0.5, 500);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.0, 0.2), &corridor);
        assert!(matches!(verdict, EnforceAction::Allow),
            "a wide corridor must admit a gentle curve; got {verdict:?}");
    }

    #[test]
    fn no_return_scan_fails_closed() {
        // No perception (all rays no-return) → confidence ~0 → unhealthy corridor
        // → the gate MRCs even a benign straight command. Fail-closed.
        let taj = TajPhaseA::new(kirra_taj::TajConfig::default());
        let snap = corridor_from_scan(&taj, &no_return_scan(100), 100);
        let corridor = snap.to_corridor(0.5, 500);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.0, 0.0), &corridor);
        assert!(matches!(verdict, EnforceAction::DenyBreach(_)),
            "a no-perception (low-confidence) corridor must fail closed; got {verdict:?}");
    }

    #[test]
    fn taj_scan_from_raw_copies_fields_and_computes_stamp_ms() {
        let raw = LaserScanRawFields {
            angle_min_rad: -1.0,
            angle_increment_rad: 0.01,
            range_min_m: 0.05,
            range_max_m: 12.0,
            ranges: vec![1.0, 2.0, 3.0],
            stamp_sec: 5,
            stamp_nanosec: 500_000_000, // 0.5 s
        };
        let scan = taj_scan_from_raw(&raw);
        assert_eq!(scan.stamp_ms, 5_500, "stamp = 5 s + 0.5 s = 5500 ms");
        assert_eq!(scan.angle_min_rad, -1.0);
        assert_eq!(scan.range_max_m, 12.0);
        assert_eq!(scan.ranges, vec![1.0, 2.0, 3.0]);
        // A negative sec clamps to 0 (no underflow).
        let neg = taj_scan_from_raw(&LaserScanRawFields { stamp_sec: -3, ..raw });
        assert_eq!(neg.stamp_ms, 500, "negative sec clamps to 0; only the nanosec remains");
    }

    #[test]
    fn snapshot_round_trips_taj_boundaries() {
        // The snapshot must copy Taj's boundaries field-for-field (the corridor::
        // Point → containment::Point conversion preserves coordinates).
        let taj = TajPhaseA::new(kirra_taj::TajConfig::default());
        let perception = taj.process(&walls_scan(1.0, 42), 42);
        let snap = CorridorSnapshot::from_taj(&perception.corridor);
        assert_eq!(snap.left.len(), perception.corridor.left_boundary().len());
        assert_eq!(snap.confidence, perception.corridor.confidence());
        assert_eq!(snap.age_ms, perception.corridor.age_ms());
        if let (Some(a), Some(b)) = (snap.left.first(), perception.corridor.left_boundary().first()) {
            assert_eq!(a.x_m, b.x_m);
            assert_eq!(a.y_m, b.y_m);
        }
    }
}
