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

/// Largest heading change (rad) tolerated between two CONSECUTIVE poses before
/// the swept-footprint bound refuses to reason about the segment.
///
/// The sweep bound models motion between consecutive poses as ONE
/// constant-curvature rigid-body segment — the bicycle-model arc every doer
/// here emits, and what makes the sagitta formula valid. A half-turn or more
/// between two samples is not a sampling of such an arc: it is a mis-ordered
/// trajectory, a wrapped heading, or a planner sampling far too sparsely to
/// bound. No inflation rescues an unmodellable segment, so it fails closed.
///
/// π rather than something smaller because the formula stays valid right up to
/// a half-turn — this marks where the MODEL stops applying, not where the
/// geometry gets uncomfortable. Tighter sampling discipline belongs in the
/// planner contract, not here.
const MAX_SEGMENT_HEADING_CHANGE_RAD: f64 = core::f64::consts::PI;

/// Heading change (rad) below which a segment is treated as exactly straight.
///
/// Both `R = chord / (2 sin(θ/2))` and the sagitta are 0/0 at exactly zero.
/// Below this threshold the true sagitta is under 1e-18 × chord — orders of
/// magnitude beneath f64 resolution at metre scale — so returning zero is exact
/// in floating point and sidesteps the singularity.
const STRAIGHT_SEGMENT_HEADING_EPS_RAD: f64 = 1e-9;

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

    for pose in trajectory.iter() {
        if !pose_is_finite(pose) {
            return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
        }
    }

    // A single pose sweeps nothing: check it where it stands. Byte-identical to
    // the pre-sweep behaviour for this input class.
    if trajectory.len() == 1 {
        let margin_sq = margin * margin;
        for corner in &footprint_corners(&trajectory[0], footprint) {
            if !corner_inside_corridor(corner, corridor, margin_sq) {
                return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
            }
        }
        return EnforceAction::Allow;
    }

    // C1 swept-footprint check. Every corner's CHORD between consecutive poses
    // is tested as a segment, inflated by the segment's sagitta — so the
    // along-track span is covered continuously (no sampling gap) and the
    // cross-track arc bulge is covered by the inflation. Every pose is an
    // endpoint of some chord, so per-pose containment is subsumed, not skipped.
    let r_max = max_corner_radius_m(footprint);
    for i in 1..trajectory.len() {
        let (prev, cur) = (&trajectory[i - 1], &trajectory[i]);
        let sagitta = match segment_sagitta_m(prev, cur, r_max) {
            Some(s) => s,
            // Unmodellable segment: no inflation makes it safe, so refuse.
            None => return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
        };
        let inflated = margin + sagitta;
        if !inflated.is_finite() {
            return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
        }
        let margin_sq = inflated * inflated;
        let from = footprint_corners(prev, footprint);
        let to = footprint_corners(cur, footprint);
        for c in 0..4 {
            if !chord_clears_corridor(&from[c], &to[c], corridor, margin_sq) {
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

/// Distance (m) from the rear axle to the FARTHEST footprint corner.
///
/// One radius bounds the whole body, so the sagitta is valid for every corner
/// at once. The rear corners sit closer and are therefore bounded slightly more
/// conservatively than strictly necessary — centimetres at robot scale, traded
/// for one bound instead of four.
#[inline]
fn max_corner_radius_m(f: &VehicleFootprint) -> f64 {
    let half_w = f.width_m * 0.5;
    let front = (f.wheelbase_m + f.overhang_front_m).hypot(half_w);
    let rear = f.overhang_rear_m.hypot(half_w);
    if front > rear {
        front
    } else {
        rear
    }
}

/// Wraps an angle to `(-π, π]`. A raw difference of `+3π/2` is a quarter-turn
/// the other way, not a three-quarter turn; without this an ordinary heading
/// wrap would look like an unmodellable segment and fail closed spuriously.
#[inline]
fn wrap_to_pi(a: f64) -> f64 {
    let two_pi = 2.0 * core::f64::consts::PI;
    let mut x = a % two_pi;
    if x > core::f64::consts::PI {
        x -= two_pi;
    } else if x <= -core::f64::consts::PI {
        x += two_pi;
    }
    x
}

/// Maximum distance (m) any footprint corner strays from its own CHORD while
/// traversing one trajectory segment. `None` = unmodellable, fail closed.
///
/// # What this covers, and why a chord
///
/// Sampling the footprint AT poses misses the region swept BETWEEN them. Two
/// distinct holes live there: the along-track span the samples never test, and
/// the cross-track BULGE where a turning corner's path bows away from the
/// straight line between its sampled positions.
///
/// Testing the chord as a segment closes the first hole exactly — there is no
/// residual along-track gap to bound. This function bounds only the second.
///
/// Note what does NOT work: the convex hull of two consecutive footprints. The
/// arc bulges outside it, so the hull is not a conservative envelope. Neither
/// is an isotropic "within L/2 of an endpoint" displacement bound: it is
/// rigorous but scales with total travel, so a straight 10 m segment down the
/// middle of a corridor would demand 5 m of lateral clearance. The deviation
/// has to be measured from the chord, which is what the sagitta is.
///
/// # The bound
///
/// Modelling the segment as a constant-curvature rigid-body arc, a point at
/// radius `R` about the instantaneous centre deviates from its chord by
/// `R·(1 − cos(θ/2))` at the midpoint, the maximum over the arc. The rear axle
/// turns at `R_axle = chord / (2 sin(θ/2))`, and no corner is farther than
/// `r_max` from the axle, so every corner satisfies
///
///   `sagitta ≤ (R_axle + r_max) · (1 − cos(θ/2))`
///
/// Straight motion gives exactly zero: the bound costs nothing where there is
/// no curvature, and grows only with the turn actually proposed.
#[inline]
fn segment_sagitta_m(a: &Pose, b: &Pose, r_max: f64) -> Option<f64> {
    let theta = wrap_to_pi(b.heading_rad - a.heading_rad).abs();
    if !theta.is_finite() || theta >= MAX_SEGMENT_HEADING_CHANGE_RAD {
        return None;
    }
    if theta < STRAIGHT_SEGMENT_HEADING_EPS_RAD {
        return Some(0.0);
    }
    let chord = (b.x_m - a.x_m).hypot(b.y_m - a.y_m);
    if !chord.is_finite() || !r_max.is_finite() {
        return None;
    }
    let half = 0.5 * theta;
    let r_axle = chord / (2.0 * half.sin());
    let sagitta = (r_axle + r_max) * (1.0 - half.cos());
    if !sagitta.is_finite() {
        return None;
    }
    Some(sagitta)
}

/// True iff the whole segment `a→b` lies inside the corridor with at least
/// `margin` (squared) clearance from every polygon edge.
///
/// Same shape as [`corner_inside_corridor`], lifted from a point to a segment:
/// one PNPoly test anchors which side we are on, and the clearance test becomes
/// segment-to-edge. Clearance `≥ margin > 0` on every edge means the segment
/// cannot have crossed the boundary, so anchoring inside-ness at one endpoint
/// is sufficient for the whole chord.
fn chord_clears_corridor(a: &Point, b: &Point, corridor: &Corridor, margin_sq: f64) -> bool {
    if !(point_is_finite(a) && point_is_finite(b)) {
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

    let mut k = 0;
    while k < n_total {
        let e0 = polygon_vertex(corridor, k);
        let e1 = polygon_vertex(corridor, (k + 1) % n_total);

        if !(point_is_finite(&e0) && point_is_finite(&e1)) {
            return false; // degenerate vertex; conservative reject
        }

        // Ray-cast inside test for endpoint `a` (standard PNPoly).
        if (e0.y_m > a.y_m) != (e1.y_m > a.y_m) {
            let dy = e1.y_m - e0.y_m;
            if dy != 0.0 {
                let x_cross = (e1.x_m - e0.x_m) * (a.y_m - e0.y_m) / dy + e0.x_m;
                if a.x_m < x_cross {
                    inside = !inside;
                }
            }
        }

        let dist_sq = segment_to_segment_dist_sq(a, b, &e0, &e1);
        if dist_sq < min_dist_sq {
            min_dist_sq = dist_sq;
        }

        k += 1;
    }

    inside && min_dist_sq >= margin_sq
}

/// Squared distance between segments `p0→p1` and `q0→q1`. Zero if they cross.
///
/// Non-crossing segments attain their minimum separation at an endpoint of one
/// against the other, so the four point-to-segment distances cover every case.
#[inline]
fn segment_to_segment_dist_sq(p0: &Point, p1: &Point, q0: &Point, q1: &Point) -> f64 {
    if segments_intersect(p0, p1, q0, q1) {
        return 0.0;
    }
    let mut best = point_to_segment_dist_sq(p0, q0, q1);
    let d = point_to_segment_dist_sq(p1, q0, q1);
    if d < best {
        best = d;
    }
    let d = point_to_segment_dist_sq(q0, p0, p1);
    if d < best {
        best = d;
    }
    let d = point_to_segment_dist_sq(q1, p0, p1);
    if d < best {
        best = d;
    }
    best
}

#[inline]
fn cross_z(o: &Point, a: &Point, b: &Point) -> f64 {
    (a.x_m - o.x_m) * (b.y_m - o.y_m) - (a.y_m - o.y_m) * (b.x_m - o.x_m)
}

/// Proper-or-touching segment intersection via the four orientation signs.
///
/// Collinear-overlap cases return `false` here; they are still caught, because
/// an overlapping pair has zero endpoint-to-segment distance and the caller
/// takes the minimum over all four.
#[inline]
fn segments_intersect(p0: &Point, p1: &Point, q0: &Point, q1: &Point) -> bool {
    let d1 = cross_z(p0, p1, q0);
    let d2 = cross_z(p0, p1, q1);
    let d3 = cross_z(q0, q1, p0);
    let d4 = cross_z(q0, q1, p1);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
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
            left.push(Point {
                x_m: x,
                y_m: half_w,
            });
            right.push(Point {
                x_m: x,
                y_m: -half_w,
            });
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
        Pose {
            x_m: x,
            y_m: y,
            heading_rad,
        }
    }

    #[test]
    fn containment_allows_pose_centered_in_straight_corridor() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj = vec![pose(20.0, 0.0, 0.0), pose(30.0, 0.0, 0.0)];
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Untrusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
        let left = vec![
            Point { x_m: 0.0, y_m: 1.0 },
            Point {
                x_m: 2.0,
                y_m: -1.0,
            },
        ];
        let right = vec![
            Point {
                x_m: 0.0,
                y_m: -1.0,
            },
            Point { x_m: 2.0, y_m: 1.0 },
        ];
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
            left.push(Point {
                x_m: x,
                y_m: yc + 3.0,
            });
            right.push(Point {
                x_m: x,
                y_m: yc - 3.0,
            });
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
            left.push(Point {
                x_m: x,
                y_m: oy + half_w,
            });
            right.push(Point {
                x_m: x,
                y_m: oy - half_w,
            });
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
        assert_eq!(
            DenyCode::DrivableSpaceDeparture.reason(),
            "DRIVABLE_SPACE_DEPARTURE"
        );
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
            Point { x_m: 0.0, y_m: 3.0 },
            Point {
                x_m: 100.0,
                y_m: 3.0,
            },
        ];
        let right = vec![Point {
            x_m: 0.0,
            y_m: -3.0,
        }]; // < 2
        let corridor = Corridor {
            left: &left,
            right: &right,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
            .map(|i| Point {
                x_m: i as f64,
                y_m: 3.0,
            })
            .collect();
        let right: Vec<Point> = (0..8)
            .map(|i| Point {
                x_m: (i as f64) * 10.0,
                y_m: -3.0,
            })
            .collect();
        let corridor = Corridor {
            left: &left,
            right: &right,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
            .map(|i| Point {
                x_m: (i as f64) * 10.0,
                y_m: 3.0,
            })
            .collect();
        let right: Vec<Point> = (0..(MAX_CORRIDOR_VERTICES + 1))
            .map(|i| Point {
                x_m: i as f64,
                y_m: -3.0,
            })
            .collect();
        let corridor = Corridor {
            left: &left,
            right: &right,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
            left: &left,
            right: &right,
            confidence: f32::NAN, // breaks is_finite()
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj = vec![pose(20.0, 0.0, 0.0)];
        let action =
            validate_trajectory_containment(&traj, &corridor, &sedan(), FrameTrust::Trusted);
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
    // -----------------------------------------------------------------------
    // C1 — swept-footprint containment
    //
    // The pre-C1 check sampled the footprint AT poses, so two holes lived
    // between samples: the along-track span nothing tested, and the cross-track
    // bulge of a turning corner. These pin both, and pin that closing them did
    // not cost the straight-line cases.
    // -----------------------------------------------------------------------

    /// A small differential robot, the deployed R2 scale.
    fn r2() -> VehicleFootprint {
        VehicleFootprint {
            width_m: 0.30,
            length_m: 0.47,
            overhang_front_m: 0.18,
            overhang_rear_m: 0.09,
            wheelbase_m: 0.20,
        }
    }

    /// Poses along a circular arc of radius `r`, rear axle on the arc, heading
    /// tangent — the constant-curvature bicycle segment the bound models.
    fn arc_poses(cx: f64, cy: f64, r: f64, from_rad: f64, to_rad: f64, n: usize) -> Vec<Pose> {
        (0..n)
            .map(|i| {
                let t = from_rad + (to_rad - from_rad) * (i as f64) / ((n - 1) as f64);
                Pose {
                    x_m: cx + r * t.cos(),
                    y_m: cy + r * t.sin(),
                    heading_rad: t + core::f64::consts::FRAC_PI_2,
                }
            })
            .collect()
    }

    // --- the bound itself ---------------------------------------------------

    #[test]
    fn straight_motion_has_exactly_zero_sagitta() {
        // The whole point of measuring deviation from the CHORD rather than
        // from an endpoint: straight travel costs nothing, however far it goes.
        for chord in [0.1, 1.0, 10.0, 100.0] {
            let a = Pose {
                x_m: 0.0,
                y_m: 0.0,
                heading_rad: 0.3,
            };
            let b = Pose {
                x_m: chord * 0.3_f64.cos(),
                y_m: chord * 0.3_f64.sin(),
                heading_rad: 0.3,
            };
            assert_eq!(segment_sagitta_m(&a, &b, 1.0), Some(0.0), "chord {chord}");
        }
    }

    #[test]
    fn sagitta_grows_with_curvature() {
        let r_max = 0.3;
        let mut previous = -1.0;
        for deg in [1.0_f64, 5.0, 15.0, 30.0, 60.0, 120.0] {
            let theta = deg.to_radians();
            let a = Pose {
                x_m: 0.0,
                y_m: 0.0,
                heading_rad: 0.0,
            };
            let b = Pose {
                x_m: 0.5,
                y_m: 0.0,
                heading_rad: theta,
            };
            let s = segment_sagitta_m(&a, &b, r_max).expect("modellable");
            assert!(
                s > previous,
                "sagitta must grow with heading change at {deg}deg"
            );
            previous = s;
        }
    }

    #[test]
    fn a_heading_wrap_is_not_a_half_turn() {
        // Crossing +pi is an ordinary small turn. Without wrapping, the raw
        // difference (~ -6.2 rad) would look unmodellable and fail closed on a
        // perfectly normal trajectory.
        let a = Pose {
            x_m: 0.0,
            y_m: 0.0,
            heading_rad: 3.10,
        };
        let b = Pose {
            x_m: 0.1,
            y_m: 0.0,
            heading_rad: -3.10,
        };
        let s = segment_sagitta_m(&a, &b, 0.3).expect("a wrap is modellable");
        assert!(s.is_finite() && s < 0.05, "small turn through pi, got {s}");
    }

    #[test]
    fn a_half_turn_between_samples_is_unmodellable() {
        let a = Pose {
            x_m: 0.0,
            y_m: 0.0,
            heading_rad: 0.0,
        };
        let b = Pose {
            x_m: 0.1,
            y_m: 0.0,
            heading_rad: core::f64::consts::PI,
        };
        assert_eq!(segment_sagitta_m(&a, &b, 0.3), None);
    }

    #[test]
    fn an_unmodellable_segment_fails_closed_end_to_end() {
        let (left, right) = straight_corridor(3.0, 100.0);
        let corridor = healthy_corridor(&left, &right);
        // Both poses sit dead centre and would pass individually.
        let traj = vec![pose(20.0, 0.0, 0.0), pose(21.0, 0.0, core::f64::consts::PI)];
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &r2(), FrameTrust::Trusted),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
        );
    }

    // --- the holes it closes ------------------------------------------------

    #[test]
    fn an_inter_sample_excursion_is_caught() {
        // Two poses inside an L-bend, sampled sparsely enough that the straight
        // path between them cuts the inside corner. Both endpoints pass the
        // pose-only check; the chord does not.
        let left = vec![
            Point { x_m: 0.0, y_m: 1.2 },
            Point { x_m: 4.0, y_m: 1.2 },
            Point {
                x_m: 4.0,
                y_m: 12.0,
            },
        ];
        let right = vec![
            Point {
                x_m: 0.0,
                y_m: -1.2,
            },
            Point {
                x_m: 8.0,
                y_m: -1.2,
            },
            Point {
                x_m: 8.0,
                y_m: 12.0,
            },
        ];
        let corridor = healthy_corridor(&left, &right);
        let fp = r2();

        let entering = pose(1.0, 0.0, 0.0);
        let leaving = pose(6.0, 8.0, core::f64::consts::FRAC_PI_2);

        // Each pose on its own is comfortably contained...
        for p in [entering, leaving] {
            assert_eq!(
                validate_trajectory_containment(&[p], &corridor, &fp, FrameTrust::Trusted),
                EnforceAction::Allow,
                "endpoint must pass in isolation, else the test proves nothing"
            );
        }
        // ...but travelling straight between them leaves the corridor.
        assert_eq!(
            validate_trajectory_containment(
                &[entering, leaving],
                &corridor,
                &fp,
                FrameTrust::Trusted
            ),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
            "the path between two contained poses must itself be contained"
        );
    }

    #[test]
    fn a_straight_run_down_a_corridor_is_still_admitted() {
        // The regression guard for the bound being too fat: an ordinary
        // straight trajectory, sampled sparsely, must still Allow.
        let (left, right) = straight_corridor(1.2, 100.0);
        let corridor = healthy_corridor(&left, &right);
        let traj: Vec<Pose> = (0..20)
            .map(|i| pose(5.0 + i as f64 * 4.0, 0.0, 0.0))
            .collect();
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &r2(), FrameTrust::Trusted),
            EnforceAction::Allow,
        );
    }

    #[test]
    fn a_gentle_arc_inside_a_wide_corridor_is_admitted() {
        // Curvature alone must not reject: the inflation has to be proportional
        // to the actual bulge, not to the fact that a turn happened.
        let (left, right) = straight_corridor(3.0, 60.0);
        let corridor = healthy_corridor(&left, &right);
        // A very shallow arc that stays near the centreline.
        let traj: Vec<Pose> = (0..10)
            .map(|i| {
                let x = 10.0 + i as f64 * 2.0;
                pose(x, 0.0002 * (x - 10.0) * (x - 10.0), 0.0004 * (x - 10.0))
            })
            .collect();
        assert_eq!(
            validate_trajectory_containment(&traj, &corridor, &r2(), FrameTrust::Trusted),
            EnforceAction::Allow,
        );
    }

    #[test]
    fn a_single_pose_trajectory_is_unchanged_by_the_sweep_check() {
        // No segment, nothing swept: the pre-C1 verdict must be preserved
        // exactly, on both sides of the boundary.
        let (left, right) = straight_corridor(1.2, 50.0);
        let corridor = healthy_corridor(&left, &right);
        let fp = r2();
        assert_eq!(
            validate_trajectory_containment(
                &[pose(10.0, 0.0, 0.0)],
                &corridor,
                &fp,
                FrameTrust::Trusted
            ),
            EnforceAction::Allow,
        );
        assert_eq!(
            validate_trajectory_containment(
                &[pose(10.0, 1.1, 0.0)],
                &corridor,
                &fp,
                FrameTrust::Trusted
            ),
            EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
        );
    }

    // --- the oracle ---------------------------------------------------------

    /// Densely resample a constant-curvature segment and run the POSE-ONLY
    /// check at every substep. This is the reference for "what the sweep really
    /// does" — expensive, but it does not need to be fast in a test.
    fn dense_pose_check(
        a: &Pose,
        b: &Pose,
        corridor: &Corridor,
        fp: &VehicleFootprint,
        margin: f64,
        steps: usize,
    ) -> bool {
        let margin_sq = margin * margin;
        let dtheta = wrap_to_pi(b.heading_rad - a.heading_rad);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            // Interpolate along the arc: heading linearly, position along the
            // chord plus the arc offset. For the shallow segments used here the
            // chord interpolation with heading blend is a close approximation,
            // and any residual only makes the oracle LESS strict — which is the
            // safe direction for the assertion below.
            let p = Pose {
                x_m: a.x_m + (b.x_m - a.x_m) * t,
                y_m: a.y_m + (b.y_m - a.y_m) * t,
                heading_rad: a.heading_rad + dtheta * t,
            };
            for corner in &footprint_corners(&p, fp) {
                if !corner_inside_corridor(corner, corridor, margin_sq) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn the_shipped_bound_is_never_looser_than_a_dense_sweep() {
        // The conservatism claim, checked rather than asserted: wherever a dense
        // resampling of the real motion finds a departure, the shipped
        // chord+sagitta check must also refuse. A failure here means the bound
        // admits something that actually leaves the corridor.
        let (left, right) = straight_corridor(1.0, 40.0);
        let corridor = healthy_corridor(&left, &right);
        let fp = r2();
        let margin = containment_margin_m(FrameTrust::Trusted).unwrap();

        let mut checked = 0;
        let mut oracle_rejections = 0;
        // Sweep arcs of many radii and offsets across the corridor.
        for radius in [2.0_f64, 4.0, 8.0, 16.0, 40.0] {
            for offset in [-0.5_f64, -0.25, 0.0, 0.25, 0.5] {
                for span_deg in [5.0_f64, 15.0, 40.0] {
                    let span = span_deg.to_radians();
                    let poses = arc_poses(
                        8.0,
                        offset - radius,
                        radius,
                        core::f64::consts::FRAC_PI_2 - span * 0.5,
                        core::f64::consts::FRAC_PI_2 + span * 0.5,
                        6,
                    );
                    for w in poses.windows(2) {
                        checked += 1;
                        let dense = dense_pose_check(&w[0], &w[1], &corridor, &fp, margin, 200);
                        let shipped = validate_trajectory_containment(
                            &[w[0], w[1]],
                            &corridor,
                            &fp,
                            FrameTrust::Trusted,
                        ) == EnforceAction::Allow;
                        if !dense {
                            oracle_rejections += 1;
                            assert!(
                                !shipped,
                                "dense sweep found a departure the shipped bound admitted: \
                                 radius={radius} offset={offset} span={span_deg}deg \
                                 a={:?} b={:?}",
                                w[0], w[1]
                            );
                        }
                    }
                }
            }
        }
        assert!(
            checked > 100,
            "corpus too small to mean anything: {checked}"
        );
        assert!(
            oracle_rejections > 0,
            "no segment in the corpus departed, so the assertion never fired — \
             the corpus must contain real departures to be evidence of anything"
        );
    }
}
