// crates/kirra-core/src/containment.rs (de-monolith Stage 4: relocated verbatim from the gateway)
//
// SG2 — drivable-space containment check.
//
// SG2 WIRING STATUS (confirmed/updated per #409 — supersedes the original
// "PENDING-WIRING" note): this module is the SG2 corridor-containment checker, a
// SIBLING entry point to `validate_vehicle_command`. SG2 is now ENFORCED via the
// Option-B per-trajectory path — the follow-up #131 ("Realize Option-B
// per-trajectory checking") is CLOSED/completed. Concretely, the kirra-ros2
// adapter slow loop (`crates/kirra-ros2-adapter/src/validation.rs`) calls
// `validate_trajectory_containment` per accepted trajectory; a non-`Allow`
// verdict returns `TrajectoryVerdict::MRCFallback`, collapsing the per-asset slot
// so the fast loop publishes the MRC. See `docs/safety/TRACEABILITY_MATRIX.md`
// (SG2 = ENFORCED) and `docs/safety/OCCY_SG2_MARGIN.md`
// (KIRRA-OCCY-SG2-MARGIN-001).
//
// The SDK's own live HTTP request path (`ProposedVehicleCommand`) is UNCHANGED —
// it remains per-command; SG2 is enforced in the ROS 2 deployment topology by
// the adapter, not by the per-command HTTP handler. This module is also
// unit-tested directly against constructed inputs. (Related: #128 SG2 coverage
// hole, #126 Perception Input Contract.)
//
// Design (per the discovery report + design prompt):
//   - Sibling entry `validate_trajectory_containment(traj, corridor, footprint)`
//     returning the existing `EnforceAction` enum.
//   - Bounded inputs: `&[Pose]` capped at `MAX_TRAJECTORY_HORIZON`;
//     `Corridor` polylines capped at `MAX_CORRIDOR_VERTICES` per side.
//   - Footprint = platform geometry, lives on `VehicleKinematicsContract`.
//   - Reject (not Clamp) on departure: containment is a hard binary, the
//     standing MRC is the right disposition.
//   - Conservative degraded mode: absent / stale / low-confidence corridor
//     → `DenyCode::DrivableSpaceDeparture` (SG4 untraversable-default pattern).
//   - Zero heap allocation; all loops bounded; scalar f64 math; no recursion.

use crate::frame_integrity::{containment_margin_m, FrameTrust};
use crate::kinematics_contract::{DenyCode, EnforceAction, VehicleKinematicsContract};

/// Maximum number of poses in a trajectory the Governor will inspect per call.
/// Bounds the per-call WCET via the inner per-pose loop. ~5 s at 10 Hz.
pub const MAX_TRAJECTORY_HORIZON: usize = 50;

/// Maximum number of polyline vertices per corridor side. Bounds the
/// per-pose work (each footprint corner is tested against every polygon
/// edge in the corridor polygon).
pub const MAX_CORRIDOR_VERTICES: usize = 128;

/// Minimum `|2 × signed area|` (m²) for a corridor polygon to count as
/// non-degenerate and consistently wound (#409 Obs 2 / M-11 winding gate). A
/// real drivable corridor (≥ footprint-width + margin laterally, spanning the
/// trajectory longitudinally) has an area orders of magnitude above this; the
/// epsilon only rejects a collapsed or area-balanced self-intersecting polygon
/// sitting at ~0.
pub const CORRIDOR_WINDING_AREA_EPS_M2: f64 = 1e-9;

/// Inward lateral safety margin from the drivable-space boundary (meters).
///
/// **Derived value — KIRRA-OCCY-SG2-MARGIN-001.** The PRIMARY pilot setting
/// is 0.40 m, the rounded-up sum of:
///   - `v_lat_max × FTTI_fast` (per-cycle residual)  ≈ 0.128 m
///   - `ε_localization` (G2 AoU — RTK 95th-pct typical)   = 0.10  m
///   - `ε_perception`   (HD-map lane-edge typical)        = 0.10  m
///   - `ε_control`      (urban steering typical)          = 0.05  m
///   →                                                    ≈ 0.378 m → 0.40 m
///
/// **G2 assumption-of-use (#123):** this value is valid IFF the integrator's
/// localization stack achieves ≤ 0.10 m 95th-percentile lateral error within
/// the deployment ODD. Above that, the conservative-fallback 0.75 m
/// (documented in `docs/safety/OCCY_SG2_MARGIN.md` §3) is required —
/// configuration-flag-driven, not the code default.
///
/// See `docs/safety/OCCY_SG2_MARGIN.md` (KIRRA-OCCY-SG2-MARGIN-001) for the
/// full derivation, navigability analysis, and AoU residuals. The
/// containment check enforces that every footprint corner is at least
/// `CONTAINMENT_LATERAL_MARGIN_M` inside the corridor polygon's edges.
pub const CONTAINMENT_LATERAL_MARGIN_M: f64 = 0.40;

/// Vehicle pose in world frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// 2D point in world frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x_m: f64,
    pub y_m: f64,
}

/// Drivable-space corridor, supplied by the integrator's perception / map
/// layer through the Perception Input Contract (#126). The corridor spans
/// the maneuver envelope (e.g. all same-direction lanes when a lane change
/// is intended), not just the current lane — so legitimate planned lane
/// changes that stay within the corridor pass containment.
///
/// The corridor is two polylines: left and right boundaries, advancing in
/// the same direction along the corridor. The implicit polygon is
/// `left[0..N], right[N-1..0]` (closed loop).
///
/// Health is checked by [`Corridor::is_healthy`]; failure → conservative
/// containment failure (DenyCode::DrivableSpaceDeparture) per the S7
/// sensor-availability rule.
#[derive(Debug, Clone, Copy)]
pub struct Corridor<'a> {
    pub left: &'a [Point],
    pub right: &'a [Point],
    /// Source confidence in `[0.0, 1.0]`. Below `min_confidence` → unhealthy.
    pub confidence: f32,
    /// Age (ms) of the corridor snapshot vs. now. Above `max_age_ms` →
    /// unhealthy (stale).
    pub age_ms: u64,
    /// Minimum acceptable confidence for the corridor to be considered
    /// healthy. Below this, the check fails conservative.
    pub min_confidence: f32,
    /// Maximum acceptable staleness (ms). Above this, the check fails
    /// conservative. Tied to the per-cycle FTTI / posture-cache TTL in
    /// the S7 fault model.
    pub max_age_ms: u64,
}

impl Corridor<'_> {
    /// Returns true iff the corridor input is present, fresh, plausible, and
    /// shape-valid (≥ 2 vertices per side, ≤ `MAX_CORRIDOR_VERTICES`).
    /// Failure → conservative containment failure (the entry function
    /// returns `DenyCode::DrivableSpaceDeparture`).
    ///
    /// # Trust boundary: corridor WINDING is now validated (#409 Obs 2 / M-11)
    ///
    /// This checks confidence, age, and per-side vertex counts. The companion
    /// `corridor_winding_is_consistent` gate (run in `validate_trajectory_containment`
    /// right after this) now enforces CONSISTENT WINDING — `left` must be on the
    /// vehicle's left of `right` (a clockwise, negative-signed-area polygon). A
    /// swapped (`left`/`right` exchanged) or a front/back-reversed side flips the
    /// orientation, and a collapsed / area-balanced self-intersection drives the
    /// signed area to ~0 — all rejected, so the previously fail-OPEN class (a
    /// self-intersecting polygon whose PNPoly inside/outside verdict is
    /// ill-defined) now fails closed to the MRC.
    ///
    /// Residual (honestly narrower than the prior "not validated at all"): an
    /// UNbalanced *partial* self-intersection that preserves the dominant
    /// clockwise orientation is not caught by the orientation test alone; a full
    /// simple-polygon proof would need the O((N+M)²) pairwise edge-intersection
    /// test, deliberately not taken here to preserve the O(N+M)-per-call WCET
    /// framing. Supplying a well-formed corridor remains the integrator's
    /// Perception Input Contract (#126) responsibility.
    pub fn is_healthy(&self) -> bool {
        self.confidence >= self.min_confidence
            && self.age_ms <= self.max_age_ms
            && self.left.len() >= 2
            && self.right.len() >= 2
            && self.left.len() <= MAX_CORRIDOR_VERTICES
            && self.right.len() <= MAX_CORRIDOR_VERTICES
            && self.confidence.is_finite()
    }
}

/// Vehicle footprint extracted from the kinematics contract. Pose convention:
/// `Pose` (`x_m`, `y_m`) is the **rear axle**, matching the bicycle-model
/// convention used by P6 (lateral-accel) in `validate_vehicle_command`.
#[derive(Debug, Clone, Copy)]
pub struct VehicleFootprint {
    pub width_m: f64,
    pub length_m: f64,
    pub overhang_front_m: f64,
    pub overhang_rear_m: f64,
    pub wheelbase_m: f64,
}

impl From<&VehicleKinematicsContract> for VehicleFootprint {
    fn from(c: &VehicleKinematicsContract) -> Self {
        Self {
            width_m: c.width_m,
            length_m: c.length_m,
            overhang_front_m: c.overhang_front_m,
            overhang_rear_m: c.overhang_rear_m,
            wheelbase_m: c.wheelbase_m,
        }
    }
}

// ---------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------

// SAFETY: SG2 | REQ: drivable-space-containment,frame-integrity-gate | TEST: containment_allows_pose_centered_in_straight_corridor,containment_rejects_pose_outside_left,containment_rejects_pose_outside_right,containment_rejects_oncoming_excursion,containment_rejects_footprint_corner_clip,containment_rejects_when_corridor_unhealthy_low_confidence,containment_rejects_when_corridor_unhealthy_stale,containment_rejects_when_trajectory_exceeds_horizon,containment_allows_lane_change_within_wide_corridor,deny_code_drivable_space_departure_renders_stable_token,untrusted_refuses_before_geometry,degraded_margin_is_stricter_than_trusted
/// SG2 drivable-space containment check, **frame-integrity-gated** (Stage S-FI1).
///
/// Resolves the lateral margin from the [`FrameTrust`] verdict
/// ([`containment_margin_m`]): `Trusted` → 0.40 m primary, `Degraded` → 0.75 m
/// fallback, `Untrusted` → refuse. The frame gate runs **FIRST**: an `Untrusted`
/// frame means the corridor cannot be trusted to be correctly placed relative to
/// the ego, so we refuse to validate geometry at all and return
/// `DenyBreach(DenyCode::FrameIntegrityUntrusted)` (→ caller commits the MRC,
/// the frame-trust-minimal maneuver). This is the one fault class the governor
/// structurally cannot otherwise catch, because localization is the governor's
/// own coordinate-frame input (AOU-LOCALIZATION-001, `ASSUMPTIONS_OF_USE.md`).
///
/// For a trusted/degraded frame, each pose's 4 footprint corners must lie inside
/// the `corridor` polygon by at least the selected margin; any failure (incl. an
/// absent / stale / low-confidence / degenerate corridor — conservative per
/// OCCY_FAULT_MODEL.md §3) → `DenyBreach(DenyCode::DrivableSpaceDeparture)`.
/// Note a *larger* margin is *stricter*, so `Degraded` rejects strictly more
/// than `Trusted` — graduation tightens safety under worse localization.
///
/// Bounded properties (per `wcet_gate` framing): the frame gate adds O(1) work
/// (a `match`); the geometry loop is unchanged — `trajectory.len() ≤
/// MAX_TRAJECTORY_HORIZON`, per-pose `O(left.len() + right.len())`, total
/// `≤ 50 × 256 × 4 = 51 200` polygon-edge tests; no heap, no recursion, scalar
/// f64 only.
#[must_use]
pub fn validate_trajectory_containment(
    trajectory: &[Pose],
    corridor: &Corridor,
    footprint: &VehicleFootprint,
    frame_trust: FrameTrust,
) -> EnforceAction {
    // Frame-integrity gate FIRST (S-FI1): refuse to validate geometry in an
    // untrusted frame. `None` margin ⇒ Untrusted ⇒ fail closed.
    let margin = match containment_margin_m(frame_trust) {
        Some(m) => m,
        None => return EnforceAction::DenyBreach(DenyCode::FrameIntegrityUntrusted),
    };

    // Conservative gates: any of these failing → containment failure (NOT
    // skip-the-check). Aligns with OCCY_FAULT_MODEL §3.
    if !corridor.is_healthy() {
        return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
    }
    // #409 Obs 2 / M-11: a malformed corridor (sides swapped, a side reversed,
    // or crossing) makes a self-intersecting polygon whose PNPoly inside/outside
    // verdict is ill-defined — previously a FAIL-OPEN delegated to the integrator.
    // Require consistent (clockwise) winding; anything else fails closed. O(N+M).
    if !corridor_winding_is_consistent(corridor) {
        return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
    }
    if trajectory.is_empty() {
        return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
    }
    // Over-horizon is a DOER↔CHECKER CONTRACT violation, NOT a geometry
    // departure — the trajectory may be entirely inside the corridor. The
    // horizon bounds the per-call verdict WCET and every planner is built and
    // unit-tested to emit `≤ MAX_TRAJECTORY_HORIZON` poses (`kirra_planner`'s
    // `HORIZON` == this constant), so an over-length proposal is a misbehaving
    // doer → fail-closed to the MRC.
    //
    // B10 (deliberate, reject-not-truncate): we do NOT truncate to the cap and
    // validate the prefix. Truncation would silently ADMIT a planner that
    // ignores the horizon contract — contrary to "the doer is untrusted; the
    // checker is the invariant." The receding-horizon argument that *would*
    // justify validating a prefix (the tail is re-planned next tick) presumes a
    // well-behaved planner that simply emitted finer sampling; but the doers
    // here are written to the cap, so an over-length proposal is anomalous, not
    // routine. If a deployment genuinely needs a longer actionable horizon, the
    // correct change is to raise `MAX_TRAJECTORY_HORIZON` (re-deriving the WCET
    // budget) — keeping one honest bound — NOT to silently accept a prefix.
    // A distinct `TrajectoryHorizonExceeded` code (vs `DrivableSpaceDeparture`)
    // makes the failure diagnosable: an integrator sees "your planner exceeded
    // the horizon contract", not a misleading "you left the drivable space".
    if trajectory.len() > MAX_TRAJECTORY_HORIZON {
        return EnforceAction::DenyBreach(DenyCode::TrajectoryHorizonExceeded);
    }
    // Footprint sanity (NaN/Inf in geometry → fail closed).
    if !footprint_is_finite(footprint) {
        return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
    }

    let margin_sq = margin * margin;

    for pose in trajectory.iter() {
        if !pose_is_finite(pose) {
            return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
        }
        let corners = footprint_corners(pose, footprint);
        for corner in &corners {
            if !corner_inside_corridor(corner, corridor, margin_sq) {
                return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
            }
        }
    }

    EnforceAction::Allow
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[inline]
fn pose_is_finite(p: &Pose) -> bool {
    p.x_m.is_finite() && p.y_m.is_finite() && p.heading_rad.is_finite()
}

#[inline]
fn point_is_finite(p: &Point) -> bool {
    p.x_m.is_finite() && p.y_m.is_finite()
}

#[inline]
fn footprint_is_finite(f: &VehicleFootprint) -> bool {
    f.width_m.is_finite()
        && f.length_m.is_finite()
        && f.overhang_front_m.is_finite()
        && f.overhang_rear_m.is_finite()
        && f.wheelbase_m.is_finite()
        && f.width_m > 0.0
        && f.length_m > 0.0
}

/// Returns the 4 footprint corners (FR, FL, RL, RR) in world frame given a
/// pose at the **rear axle**.
fn footprint_corners(pose: &Pose, f: &VehicleFootprint) -> [Point; 4] {
    let cos_h = pose.heading_rad.cos();
    let sin_h = pose.heading_rad.sin();
    let half_w = f.width_m * 0.5;
    let front_x = f.wheelbase_m + f.overhang_front_m;
    let rear_x = -f.overhang_rear_m;
    // Body-frame corners (x_b, y_b), then rotate by heading and translate by pose.
    let body = [
        (front_x, -half_w), // FR
        (front_x, half_w),  // FL
        (rear_x, half_w),   // RL
        (rear_x, -half_w),  // RR
    ];
    let mut out = [Point { x_m: 0.0, y_m: 0.0 }; 4];
    let mut i = 0;
    while i < 4 {
        let (xb, yb) = body[i];
        out[i] = Point {
            x_m: pose.x_m + cos_h * xb - sin_h * yb,
            y_m: pose.y_m + sin_h * xb + cos_h * yb,
        };
        i += 1;
    }
    out
}

/// Tests whether `corner` lies inside the corridor polygon with at least
/// the given margin (squared, to avoid a sqrt per edge).
///
/// The corridor polygon is the closed loop:
///   left[0], left[1], ..., left[N-1], right[M-1], right[M-2], ..., right[0]
///
/// We walk all polygon edges in a single pass and accumulate (a) the
/// ray-cast winding for the inside test and (b) the minimum squared
/// distance from the corner to any edge. The corner passes iff it is
/// inside AND min-distance ≥ margin.
///
/// # Over-conservatism: the lateral margin also binds the end-caps (#409 Obs 1)
///
/// The loop closes via `(k + 1) % n_total`, so it includes the two END-CAP edges
/// (corridor front `left[N-1] -> right[M-1]` and the closing back
/// `right[0] -> left[0]`). `min_dist_sq` is taken over ALL edges including these
/// caps, so `CONTAINMENT_LATERAL_MARGIN_M` — a documented *lateral* margin — is
/// ALSO enforced longitudinally, shrinking the usable corridor by that margin at
/// each end. This is fail-SAFE (it rejects more, never less), and a no-op when
/// corridors extend well beyond the footprint horizon — the assumed case, since
/// the Perception Input Contract (#126) supplies a corridor ahead of the vehicle.
/// On a short / sensor-range-truncated corridor it could spuriously reject a pose
/// validly within the lateral boundaries yet near a cap. Tracked alternative:
/// exclude the two cap edges from the *margin* test while keeping them in the
/// *inside* test.
fn corner_inside_corridor(corner: &Point, corridor: &Corridor, margin_sq: f64) -> bool {
    if !point_is_finite(corner) {
        return false;
    }

    let n_left = corridor.left.len();
    let n_right = corridor.right.len();
    let n_total = n_left + n_right;
    if n_total < 4 {
        return false; // degenerate; conservative reject
    }

    let mut inside = false;
    let mut min_dist_sq = f64::INFINITY;

    // Iterate every polygon edge.
    let mut k = 0;
    while k < n_total {
        let a = polygon_vertex(corridor, k);
        let b = polygon_vertex(corridor, (k + 1) % n_total);

        if !(point_is_finite(&a) && point_is_finite(&b)) {
            return false; // degenerate vertex; conservative reject
        }

        // Ray-cast inside test (horizontal ray to +x). Standard PNPoly.
        if (a.y_m > corner.y_m) != (b.y_m > corner.y_m) {
            let dy = b.y_m - a.y_m;
            if dy != 0.0 {
                let x_cross = (b.x_m - a.x_m) * (corner.y_m - a.y_m) / dy + a.x_m;
                if corner.x_m < x_cross {
                    inside = !inside;
                }
            }
        }

        // Squared distance from corner to edge AB.
        let dist_sq = point_to_segment_dist_sq(corner, &a, &b);
        if dist_sq < min_dist_sq {
            min_dist_sq = dist_sq;
        }

        k += 1;
    }

    inside && min_dist_sq >= margin_sq
}

#[inline]
fn polygon_vertex(corridor: &Corridor, idx: usize) -> Point {
    let n_left = corridor.left.len();
    if idx < n_left {
        corridor.left[idx]
    } else {
        // Right side, walked in reverse: idx = n_left + j  →  right[N_right - 1 - j].
        let j = idx - n_left;
        corridor.right[corridor.right.len() - 1 - j]
    }
}

/// `2 ×` the signed area (shoelace) of the assembled corridor polygon, walked in
/// the canonical order `left[0..N], right[M-1..0]`. The SIGN encodes winding: a
/// well-formed corridor (the `left` field is the boundary on the vehicle's left
/// of travel) is traversed CLOCKWISE, so the signed area is NEGATIVE. This is
/// invariant to world rotation/translation and to corridor curvature (rigid
/// motions preserve orientation), so a single fixed expected sign is valid for
/// straight, curved, and lane-change corridors alike. O(N+M), no heap.
///
/// Vertices are shifted by the first polygon vertex before the cross products.
/// The signed area is translation-invariant in exact arithmetic, but the raw
/// `a.x * b.y` form loses precision when coordinates are large (a world frame —
/// UTM ~1e6, ECEF ~6.4e6) relative to the corridor's own area; the rest of
/// `containment` works in relative differences and is translation-stable
/// (`verdict_is_translation_invariant`), so referencing to vertex 0 keeps this
/// gate consistent with that discipline. Modeled offsets up to ~1e8 never flip
/// the sign, but this removes the dependence entirely.
#[inline]
fn corridor_signed_area_2x(corridor: &Corridor) -> f64 {
    let n_total = corridor.left.len() + corridor.right.len();
    if n_total == 0 {
        return 0.0;
    }
    let origin = polygon_vertex(corridor, 0);
    let mut acc = 0.0;
    let mut k = 0;
    while k < n_total {
        let a = polygon_vertex(corridor, k);
        let b = polygon_vertex(corridor, (k + 1) % n_total);
        let (ax, ay) = (a.x_m - origin.x_m, a.y_m - origin.y_m);
        let (bx, by) = (b.x_m - origin.x_m, b.y_m - origin.y_m);
        acc += ax * by - bx * ay;
        k += 1;
    }
    acc
}

/// Conservative WINDING / orientation gate (#409 Obs 2 / M-11). A well-formed
/// corridor is clockwise (negative signed area); a swapped (`left`/`right`
/// exchanged) or front/back-reversed side flips the sign, and a collapsed or
/// area-balanced self-intersecting polygon drives it to ~0. Accept ONLY a firmly
/// clockwise polygon — everything else (CCW, degenerate, balanced bowtie) fails
/// closed. A non-finite vertex makes the sum NaN and `NaN < -eps` is `false`, so
/// that too rejects (conservative). O(N+M), no heap.
#[inline]
fn corridor_winding_is_consistent(corridor: &Corridor) -> bool {
    corridor_signed_area_2x(corridor) < -CORRIDOR_WINDING_AREA_EPS_M2
}

/// Squared distance from point P to segment AB. Standard closed-form.
#[inline]
fn point_to_segment_dist_sq(p: &Point, a: &Point, b: &Point) -> f64 {
    let abx = b.x_m - a.x_m;
    let aby = b.y_m - a.y_m;
    let apx = p.x_m - a.x_m;
    let apy = p.y_m - a.y_m;
    let denom = abx * abx + aby * aby;
    if denom <= 0.0 {
        // Degenerate segment (A == B). Distance is just |P - A|^2.
        return apx * apx + apy * apy;
    }
    let t = ((apx * abx + apy * aby) / denom).clamp(0.0, 1.0);
    let qx = a.x_m + t * abx;
    let qy = a.y_m + t * aby;
    let dx = p.x_m - qx;
    let dy = p.y_m - qy;
    dx * dx + dy * dy
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Small reference footprint (a sedan).
    fn sedan() -> VehicleFootprint {
        VehicleFootprint {
            width_m: 1.85,
            length_m: 4.8,
            overhang_front_m: 0.9,
            overhang_rear_m: 1.1,
            wheelbase_m: 2.8,
        }
    }

    /// Builds a straight corridor along +x at y ∈ [-half_w, +half_w] over
    /// x ∈ [0, x_max]. Uses a fixed vertex count so the polylines are
    /// non-degenerate.
    fn straight_corridor(half_w: f64, x_max: f64) -> (Vec<Point>, Vec<Point>) {
        let n = 8;
        let dx = x_max / (n as f64 - 1.0);
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let x = i as f64 * dx;
            left.push(Point { x_m: x, y_m: half_w });
            right.push(Point { x_m: x, y_m: -half_w });
        }
        (left, right)
    }

    fn healthy_corridor<'a>(left: &'a [Point], right: &'a [Point]) -> Corridor<'a> {
        Corridor {
            left,
            right,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        }
    }

    fn pose(x: f64, y: f64, heading_rad: f64) -> Pose {
        Pose { x_m: x, y_m: y, heading_rad }
    }

    #[test]
    fn containment_allows_pose_centered_in_straight_corridor() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(20.0, 0.0, 0.0), pose(30.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(action, EnforceAction::Allow, "centered pose must Allow");
    }

    // --- Stage S-FI1b: frame-integrity gate ---------------------------------

    #[test]
    fn untrusted_refuses_before_geometry() {
        // A pose that WOULD Allow under a trusted frame (centered, wide corridor)
        // must still be refused when the frame is Untrusted — the gate runs before
        // any geometry is considered.
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(50.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(
            &traj,
            &corridor,
            &sedan(),
            FrameTrust::Untrusted,
        );
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::FrameIntegrityUntrusted),
            "untrusted frame must refuse with FrameIntegrityUntrusted, even for a safe pose"
        );
    }

    #[test]
    fn degraded_margin_is_stricter_than_trusted() {
        // sedan half-width 0.925 m; corridor half-width 1.425 m → lateral
        // clearance 0.50 m, which is ≥ 0.40 (primary) but < 0.75 (fallback).
        // Pose at x = 50 sits far from the end-caps so only the lateral edges bind.
        let (left, right) = straight_corridor(1.425, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(50.0, 0.0, 0.0)];
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted),
            EnforceAction::Allow,
            "0.50 m clearance passes the 0.40 m primary margin (Trusted)"
        );
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Degraded),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "0.50 m clearance fails the stricter 0.75 m fallback margin (Degraded)"
        );
    }

    #[test]
    fn containment_rejects_pose_outside_left() {
        // Sedan width 1.85 → half-width 0.925. Corridor half-width 3 m.
        // Pose at y = 2.5 → left corner at y = 3.425, well outside.
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(40.0, 2.5, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "off-left pose must Reject"
        );
    }

    #[test]
    fn containment_rejects_pose_outside_right() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(40.0, -2.5, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)
        );
    }

    #[test]
    fn containment_rejects_oncoming_excursion() {
        // Vehicle yawed across the corridor — front corners far outside.
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(40.0, 0.0, std::f64::consts::FRAC_PI_2)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)
        );
    }

    #[test]
    fn containment_rejects_footprint_corner_clip() {
        // Pose center is inside, but a yaw at the edge makes a single corner
        // clip outside the corridor. Sedan half-width 0.925; rear-axle at
        // y=2.0, heading slightly off-axis → front-left corner at y ≈ 3.29,
        // which is past the corridor edge (y = 3.0) regardless of the
        // CONTAINMENT_LATERAL_MARGIN_M value — the test exercises the
        // corner-clip rejection itself, not the margin boundary.
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(40.0, 2.0, 0.1)]; // ~5.7° yaw left
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "corner-clip past the margin must Reject even if center is inside"
        );
    }

    #[test]
    fn containment_rejects_when_corridor_unhealthy_low_confidence() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let mut corridor = healthy_corridor(&left, &right);
        corridor.confidence = 0.1; // below min_confidence
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "low-confidence corridor must conservative-Reject"
        );
    }

    #[test]
    fn containment_rejects_when_corridor_unhealthy_stale() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let mut corridor = healthy_corridor(&left, &right);
        corridor.age_ms = 10_000; // > max_age_ms
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "stale corridor must conservative-Reject"
        );
    }

    #[test]
    fn containment_rejects_when_corridor_degenerate() {
        let corridor = Corridor {
            left: &[],
            right: &[],
            confidence: 1.0,
            age_ms: 0,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj = vec![pose(0.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "empty-corridor must Reject"
        );
    }

    #[test]
    fn containment_rejects_when_trajectory_exceeds_horizon() {
        let (left, right) = straight_corridor(3.0, 1000.0);
        let corridor = healthy_corridor(&left, &right);
        // 51 > MAX_TRAJECTORY_HORIZON = 50.
        let traj: Vec<Pose> = (0..(MAX_TRAJECTORY_HORIZON + 1))
            .map(|i| pose(i as f64, 0.0, 0.0))
            .collect();
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::TrajectoryHorizonExceeded),
            "horizon overflow must Reject (B10: doer-contract violation, fail-closed) \
             with the honest TrajectoryHorizonExceeded code — NOT DrivableSpaceDeparture \
             (the trajectory here is entirely inside the corridor)"
        );
    }

    #[test]
    fn containment_rejects_empty_trajectory() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj: Vec<Pose> = vec![];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)
        );
    }

    #[test]
    fn containment_rejects_nan_pose() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(f64::NAN, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)
        );
    }

    #[test]
    fn containment_allows_lane_change_within_wide_corridor() {
        // Wide corridor (half-width 5 m) spans two same-direction lanes.
        // Trajectory drifts laterally from y=-2 to y=+2 (a lane change)
        // while staying well inside the corridor. Should Allow.
        let (left, right) = straight_corridor(5.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj: Vec<Pose> = (0..20)
            .map(|i| {
                let t = i as f64 / 19.0;
                let x = 10.0 + 4.0 * i as f64;
                let y = -2.0 + 4.0 * t;
                pose(x, y, 0.0)
            })
            .collect();
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::Allow,
            "lane change within wide corridor must Allow"
        );
    }

    // ---------------------------------------------------------------------
    // #409 Obs 2 / M-11: corridor WINDING / simplicity gate.
    // A malformed corridor yields a self-intersecting polygon whose PNPoly
    // inside/outside verdict is ill-defined — previously a FAIL-OPEN. These pin
    // that it now fails closed, while a well-formed (incl. curved) corridor still
    // Allows (covered by the straight/lane-change/translation tests above).
    // ---------------------------------------------------------------------

    /// A centered pose that a WELL-FORMED corridor admits is REJECTED once the
    /// two sides are swapped (`left`/`right` exchanged → counter-clockwise
    /// winding). This is the headline fail-open: a swapped corridor must not be
    /// trusted to bound the trajectory.
    #[test]
    fn containment_rejects_swapped_winding_corridor() {
        let (l, r) = straight_corridor(3.0, 100.0);
        // Sanity: well-formed, the centered pose Allows.
        let well_formed = healthy_corridor(&l, &r);
        let traj = vec![pose(50.0, 0.0, 0.0)];
        assert_eq!(
            validate_trajectory_containment(&traj, &well_formed, &sedan(), FrameTrust::Trusted),
            EnforceAction::Allow,
            "control: the centered pose is admitted in the well-formed corridor"
        );
        // Swap the sides → inconsistent winding → fail closed.
        let swapped = healthy_corridor(&r, &l);
        assert_eq!(
            validate_trajectory_containment(&traj, &swapped, &sedan(), FrameTrust::Trusted),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "a swapped-winding corridor must fail closed, not admit the trajectory"
        );
    }

    /// A self-intersecting "bowtie" corridor (the two sides cross) has an
    /// ill-defined inside/outside; its signed area collapses to ~0 → rejected.
    #[test]
    fn containment_rejects_self_intersecting_bowtie_corridor() {
        // left and right cross between station 0 and station 1.
        let left = vec![Point { x_m: 0.0, y_m: 1.0 }, Point { x_m: 2.0, y_m: -1.0 }];
        let right = vec![Point { x_m: 0.0, y_m: -1.0 }, Point { x_m: 2.0, y_m: 1.0 }];
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(1.0, 0.0, 0.0)];
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "a self-intersecting corridor must fail closed"
        );
    }

    /// The winding sign is curvature-invariant: a corridor that curves (left stays
    /// on the +lateral side throughout) is still consistently wound and admits a
    /// pose that follows the curve — proving the gate does NOT over-reject turns.
    #[test]
    fn containment_allows_consistently_wound_curved_corridor() {
        // A gentle right-then-left S, left edge offset +3 m, right edge -3 m of a
        // curving centerline. Both sides share the same per-station offset sign,
        // so winding stays clockwise.
        let n = 12;
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let x = i as f64 * 5.0;
            let yc = (i as f64 * 0.4).sin() * 2.0; // wavy centerline
            left.push(Point { x_m: x, y_m: yc + 3.0 });
            right.push(Point { x_m: x, y_m: yc - 3.0 });
        }
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(25.0, (5.0_f64 * 0.4).sin() * 2.0, 0.0)];
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted),
            EnforceAction::Allow,
            "a consistently-wound curved corridor must still Allow (no over-rejection)"
        );
    }

    /// Winding gate robustness: the same well-formed / swapped corridors at an
    /// ECEF-scale world offset must still Allow / fail-closed respectively. Locks
    /// the reference-origin shoelace (the naive absolute-coordinate form loses
    /// precision far from the origin).
    #[test]
    fn containment_winding_stable_at_large_world_offset() {
        let (ox, oy) = (4.5e6_f64, 4.5e6_f64); // ~ECEF magnitude
        let (n, half_w, x_max) = (8usize, 3.0_f64, 100.0_f64);
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let x = ox + i as f64 * (x_max / (n as f64 - 1.0));
            left.push(Point { x_m: x, y_m: oy + half_w });
            right.push(Point { x_m: x, y_m: oy - half_w });
        }
        let traj = vec![pose(ox + 50.0, oy, 0.0)];

        let corridor = healthy_corridor(&left, &right);
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted),
            EnforceAction::Allow,
            "a well-formed corridor at a large world offset must still Allow"
        );

        let swapped = healthy_corridor(&right, &left);
        assert_eq!(
            validate_trajectory_containment(&traj, &swapped, &sedan(), FrameTrust::Trusted),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "a swapped corridor at a large world offset must still fail closed"
        );
    }

    #[test]
    fn deny_code_drivable_space_departure_renders_stable_token() {
        // Audit-chain hash stability: the rendered token must match what
        // serde produces (SCREAMING_SNAKE_CASE) and the .reason() arm.
        assert_eq!(DenyCode::DrivableSpaceDeparture.reason(), "DRIVABLE_SPACE_DEPARTURE");
        let as_json = serde_json::to_string(&DenyCode::DrivableSpaceDeparture).expect("serialize");
        assert_eq!(as_json, "\"DRIVABLE_SPACE_DEPARTURE\"");
    }

    // ---------------------------------------------------------------------
    // MC/DC pair-completion tests (S3 / #115 — KIRRA-OCCY-MCDC-001).
    //
    // The AND-chain in `Corridor::is_healthy` (l.96–104) and the
    // `footprint_is_finite` guard at l.171 each need an independent-effect
    // demonstration of every clause. Existing tests cover the
    // `confidence < min_confidence` and `age_ms > max_age_ms` clauses; the
    // remaining false-arm gaps are the `right.len() >= 2`, both
    // `len() <= MAX_CORRIDOR_VERTICES` clauses, the `confidence.is_finite()`
    // tail, and the footprint-finite guard.
    // ---------------------------------------------------------------------

    /// MC/DC: independent-effect of `right.len() >= 2` (is_healthy l.100
    /// FALSE arm). Left side is fine; right side has only one vertex.
    #[test]
    fn containment_rejects_when_right_side_too_short() {
        let left = vec![
            Point { x_m: 0.0,   y_m: 3.0 },
            Point { x_m: 100.0, y_m: 3.0 },
        ];
        let right = vec![Point { x_m: 0.0, y_m: -3.0 }]; // < 2
        let corridor = Corridor {
            left: &left, right: &right,
            confidence: 0.95, age_ms: 10,
            min_confidence: 0.5, max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "single-vertex right side must Reject (is_healthy false)"
        );
    }

    /// MC/DC: independent-effect of `left.len() <= MAX_CORRIDOR_VERTICES`
    /// (is_healthy l.101 FALSE arm). Right side ok; left side over-cap.
    #[test]
    fn containment_rejects_when_left_side_overflows_max_vertices() {
        let left: Vec<Point> = (0..(MAX_CORRIDOR_VERTICES + 1))
            .map(|i| Point { x_m: i as f64, y_m: 3.0 })
            .collect();
        let right: Vec<Point> = (0..8)
            .map(|i| Point { x_m: (i as f64) * 10.0, y_m: -3.0 })
            .collect();
        let corridor = Corridor {
            left: &left, right: &right,
            confidence: 0.95, age_ms: 10,
            min_confidence: 0.5, max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "left-side over MAX_CORRIDOR_VERTICES must Reject"
        );
    }

    /// MC/DC: independent-effect of `right.len() <= MAX_CORRIDOR_VERTICES`
    /// (is_healthy l.102 FALSE arm). Left side ok; right side over-cap.
    #[test]
    fn containment_rejects_when_right_side_overflows_max_vertices() {
        let left: Vec<Point> = (0..8)
            .map(|i| Point { x_m: (i as f64) * 10.0, y_m: 3.0 })
            .collect();
        let right: Vec<Point> = (0..(MAX_CORRIDOR_VERTICES + 1))
            .map(|i| Point { x_m: i as f64, y_m: -3.0 })
            .collect();
        let corridor = Corridor {
            left: &left, right: &right,
            confidence: 0.95, age_ms: 10,
            min_confidence: 0.5, max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "right-side over MAX_CORRIDOR_VERTICES must Reject"
        );
    }

    /// MC/DC: independent-effect of `confidence.is_finite()` (is_healthy
    /// l.103 FALSE arm). A NaN confidence with all other geometry clauses
    /// true must reject. The other clauses are constructed so the
    /// confidence.is_finite() result is the sole determinant.
    #[test]
    fn containment_rejects_when_confidence_is_nan() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = Corridor {
            left: &left, right: &right,
            confidence: f32::NAN, // breaks is_finite()
            age_ms: 10,
            min_confidence: 0.5, max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "NaN confidence must Reject (is_finite false)"
        );
    }

    /// MC/DC: independent-effect of `footprint_is_finite(footprint)`
    /// guard in validate_trajectory_containment (l.171 TRUE arm). All
    /// other gates pass; the footprint has a NaN field.
    #[test]
    fn containment_rejects_when_footprint_nonfinite() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let mut bad = sedan();
        bad.width_m = f64::NAN;
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action = validate_trajectory_containment(&traj, &corridor, &bad, FrameTrust::Trusted);
        assert_eq!(
            action,
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "non-finite footprint geometry must Reject"
        );
    }

    #[test]
    fn footprint_from_contract_picks_up_geometry() {
        let c = VehicleKinematicsContract::nominal_reference_profile();
        let fp = VehicleFootprint::from(&c);
        assert_eq!(fp.width_m, c.width_m);
        assert_eq!(fp.length_m, c.length_m);
        assert_eq!(fp.overhang_front_m, c.overhang_front_m);
        assert_eq!(fp.overhang_rear_m, c.overhang_rear_m);
        assert_eq!(fp.wheelbase_m, c.wheelbase_m);
        // Internal consistency: length should be wheelbase + overhangs.
        let derived = c.wheelbase_m + c.overhang_front_m + c.overhang_rear_m;
        assert!((c.length_m - derived).abs() < 1e-9);
    }
}
