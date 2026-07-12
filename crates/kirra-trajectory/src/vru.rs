//! **Pedestrian / VRU RSS primitive (WS-2, KIRRA-VRU-RSS-001).**
//!
//! The vehicle-vehicle RSS in `validation.rs` models road users with a
//! defined direction of travel (the §5 longitudinal/lateral conjunction).
//! A pedestrian breaks that model's core assumption: a VRU can change
//! direction and speed essentially instantly relative to vehicle dynamics,
//! so lateral-alignment filtering and directional closing-speed bounds are
//! unsound for them — a pedestrian standing laterally "clear" on the kerb
//! can step into the corridor within the ego's stopping time.
//!
//! This module implements the **omnidirectional reachable-set bound**
//! (design: `docs/safety/PEDESTRIAN_RSS.md`): at each time-matched
//! trajectory pose, the pedestrian is assumed able to move in ANY direction
//! at `v_ped_max`; the ego must be able to come to a full stop (reaction +
//! braking, the same RSS stopping model as the vehicle case) WITHOUT its
//! stopping envelope intersecting the pedestrian's grown reachable disc
//! plus a clearance margin. The disc model deliberately SUBSUMES a directed
//! "crossing model" — every crossing trajectory of speed ≤ `v_ped_max` lies
//! inside the disc — trading availability for soundness in v0; the directed
//! refinement is a tracked follow-up, allowed only to RELAX this bound with
//! validated tracking evidence, never to weaken it silently.
//!
//! **Responsibility semantics / the stop-proposal invariant.** A pose with
//! ego speed ≤ `stop_epsilon_mps` imposes NO requirement: a stationary ego
//! strikes nothing (a pedestrian contacting a stopped vehicle is not an
//! ego-caused collision under RSS responsibility), and — load-bearing —
//! this keeps `PlanOutput::safe_stop` admissible next to any pedestrian, so
//! the pedestrian gate can never deadlock the doer↔checker loop.
//!
//! **Fail-closed:** a non-finite pedestrian field is an unlocalizable
//! perception fault → breach (MRC), mirroring the vehicle-object rule.
//! Absent input (`None` scene) is a no-op — the Nominal path without a VRU
//! channel is byte-identical (the derate-only invariant).
//!
//! **Go-live hardening (#789).** Before any producer feeds live
//! `PerceivedPedestrian`s the bound also (F4) refuses to trust a doer-declared
//! `velocity_mps` alone — the stop-epsilon skip uses the MAX of the declared and
//! the displacement-implied speed of the adjacent pose pair, so a planner emitting
//! translating poses that DECLARE `v = 0` cannot bypass the check; (F5) sanitizes
//! the caller-supplied [`VruRssParams`] at the single choke point so a loose param
//! set can never WEAKEN the bound (`v_ped_max` floored at [`V_PED_MAX_FLOOR_MPS`],
//! `stop_epsilon` clamped to the kernel [`VRU_STOP_EPSILON_CEILING_MPS`]); (F8)
//! grows the reachable disc by `v_ped_max · age` for a stale measurement
//! ([`PerceivedPedestrian::age_s`], fail-closed on negative/non-finite age); and
//! (F9) bounds the pedestrian count at [`MAX_PEDESTRIANS`] (fail-closed above it)
//! and hoists the per-pose `required` out of the pedestrian loop.
//!
//! **Corridor-clip availability seam (#789 F6, default-off).** The
//! omnidirectional disc treats a pedestrian PHYSICALLY separated from the ego by
//! an impassable obstacle (kerb, median, wall) as if they could teleport through
//! it. The opt-in `barriers` input ([`BarrierSegment`]) clips the disc toward the
//! corridor-geodesic: a pedestrian whose straight path to the ego crosses an
//! impassable barrier is bounded by a lower bound on the walk-AROUND distance
//! ([`barrier_aware_distance`]), so it binds LESS. Sound by construction —
//! `euclidean ≤ dist_geo ≤ true_geodesic` — so the clip only ever RELAXES and
//! never admits a pedestrian the true geodesic would not. Barriers are a SEPARATE
//! input from the drivable corridor so a possibly-painted corridor edge can never
//! be treated as a wall (the load-bearing input gate; design:
//! `docs/safety/PEDESTRIAN_RSS_F6_DESIGN.md`). Default-off: no caller supplies
//! barriers today (`&[]` → the pure euclidean disc, byte-identical); per-ODD map
//! enablement is a further reviewed change. A corrupt or over-bound barrier set
//! fails closed to the pure disc, never to a relaxation.
//!
//! **WCET (F9/F6).** With `T` poses (`T ≤ MAX_TRAJECTORY_HORIZON`), `P`
//! pedestrians (`P ≤ MAX_PEDESTRIANS`) and `B` barriers (`B ≤ MAX_BARRIERS`),
//! [`pedestrian_breach`] is `O(T·P·B)` comparisons (`O(T·P)` when barriers are
//! absent, the only case today) with exactly ONE `required` evaluation per pose
//! (the heavy term is pose-only, not per-pedestrian) — a bounded, allocation-free
//! per-tick cost.

use kirra_core::corridor::Point;
use kirra_core::kinematics_contract::STOP_EPSILON_MPS;
use kirra_core::trajectory::TrajectoryPoint;

/// Fail-closed input bound on pedestrians per scene (#789 F9, WCET). Mirrors
/// `MAX_TRAJECTORY_HORIZON`'s role for the trajectory: a scene carrying more
/// than this is a malformed / over-bound perception input → breach, never an
/// unbounded loop on the slow-loop path.
pub const MAX_PEDESTRIANS: usize = 64;

/// Floor on the assumed max pedestrian speed (#789 F5), the validated 2.0 m/s
/// brisk walk. A caller cannot supply a smaller `v_ped_max` — that would silently
/// SHRINK the reachable-disc growth. ODDs may only RAISE it (doc §5.1).
pub const V_PED_MAX_FLOOR_MPS: f64 = 2.0;

/// Ceiling on the VRU stop-epsilon (#789 F5): the kernel stop-and-hold epsilon
/// (`STOP_EPSILON_MPS`). A larger VRU `stop_epsilon` would let a still-rolling ego
/// (up to the looser value) SKIP the bound; clamping to the kernel epsilon keeps
/// one stop semantics across the checker.
pub const VRU_STOP_EPSILON_CEILING_MPS: f64 = STOP_EPSILON_MPS;

/// Displacement below which a zero/negative-`dt` segment is treated as a
/// stationary dwell rather than a malformed teleport (#789 F4). A nonzero
/// displacement over a non-positive/non-finite `dt` is a time-inconsistent
/// trajectory and fails closed.
const SEG_DEGENERATE_EPS_M: f64 = 1e-9;

/// Fail-closed / WCET input bound on impassable barrier segments per scene
/// (#789 F6). Mirrors [`MAX_PEDESTRIANS`]: an over-bound barrier set is a
/// malformed perception/map input. Because barriers only ever RELAX the bound
/// (§3.1 of `docs/safety/PEDESTRIAN_RSS_F6_DESIGN.md`), the fail-closed action
/// is to IGNORE all barriers (fall back to the pure euclidean disc — the more
/// conservative bound), never to admit; this also bounds the `O(T·P·B)` work.
pub const MAX_BARRIERS: usize = 64;

/// An **impassable** barrier segment in the world frame (#789 F6): a physical
/// obstacle (kerb, median, wall, guardrail) a pedestrian must walk AROUND, not
/// through. Supplied as a SEPARATE input from the drivable corridor
/// (`PEDESTRIAN_RSS_F6_DESIGN.md` §3.2 option (b)) so a possibly-painted
/// corridor edge can never be treated as a wall. Only segments an ODD's
/// reviewed map profile has classified impassable belong here — the load-bearing
/// soundness input (§2). Absent (`&[]`) → the pure euclidean disc, unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarrierSegment {
    pub a: Point,
    pub b: Point,
}

/// The pedestrian contract type now lives in the lean contract crate
/// (`kirra_core::trajectory`, next to `PerceivedObject`) so the PRODUCER
/// (kirra-taj's WP-10 classifier) can emit it without depending on this
/// checker crate; re-exported here so every existing consumer path keeps
/// working unchanged.
pub use kirra_core::trajectory::PerceivedPedestrian;

/// Parameters of the VRU bound. Every default is CONSERVATIVE-FIRST and
/// **VALIDATION-PENDING** (deployment-tunable per ODD; see the design doc
/// §5 for the rationale and the tuning obligations).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VruRssParams {
    /// Assumed max pedestrian speed, m/s (any direction). Default 2.0 —
    /// a brisk walk; ODDs with expected runners/cyclists-as-VRUs must
    /// raise it (doc §5.1).
    pub v_ped_max_mps: f64,
    /// Pedestrian body radius, m (their footprint is not a point).
    pub ped_radius_m: f64,
    /// Additional clearance margin, m, beyond the geometric envelopes.
    pub clearance_m: f64,
    /// Ego reaction time, s — the delay before assured braking begins.
    /// Default matches the vehicle-RSS `RSS_REACTION_TIME_S` (0.5 s).
    pub reaction_time_s: f64,
    /// Ego speed at/below which a pose imposes no VRU requirement, m/s
    /// (the stationary-ego responsibility rule + safe-stop admissibility).
    pub stop_epsilon_mps: f64,
}

impl Default for VruRssParams {
    fn default() -> Self {
        Self {
            v_ped_max_mps: 2.0,
            ped_radius_m: 0.3,
            clearance_m: 0.5,
            reaction_time_s: 0.5,
            stop_epsilon_mps: 0.05,
        }
    }
}

impl VruRssParams {
    /// Safety sanitization (#789 F5), applied at the single choke point
    /// ([`pedestrian_breach`]) so a caller-supplied param set can never WEAKEN the
    /// bound however it was constructed: `v_ped_max` is floored at
    /// [`V_PED_MAX_FLOOR_MPS`] and `stop_epsilon` is clamped to
    /// [`VRU_STOP_EPSILON_CEILING_MPS`] (the kernel stop-and-hold epsilon). Both
    /// clamps are MONOTONE-TIGHTENING — a caller may raise `v_ped_max` or lower
    /// `stop_epsilon`, never the reverse. Non-finite fields are left untouched for
    /// `params_valid` to fail closed (an infinite requirement), never clamped
    /// into a finite value that would admit a trajectory. (A full per-ODD
    /// derivation of `v_ped_max` — runners, children, cyclists-as-VRUs — remains a
    /// deployment tuning obligation on top of this floor; doc §5.)
    #[must_use]
    pub fn sanitized(&self) -> VruRssParams {
        VruRssParams {
            // Only floor a VALID (finite AND non-negative) value. A negative
            // v_ped_max is corrupt — `max(-1.0, 2.0)` would LAUNDER it into a valid
            // 2.0 and defeat the design-doc §4 fail-closed rule (a corrupt param →
            // ∞ requirement → breach). Leave it untouched so `params_valid` fails
            // closed. (Copilot #799.)
            v_ped_max_mps: if self.v_ped_max_mps.is_finite() && self.v_ped_max_mps >= 0.0 {
                self.v_ped_max_mps.max(V_PED_MAX_FLOOR_MPS)
            } else {
                self.v_ped_max_mps
            },
            // Same discipline for the epsilon clamp: `min(neg, 0.05)` already keeps a
            // negative (so `params_valid` still fails), but guard explicitly so the
            // fail-closed intent does not depend on `min`'s sign behavior.
            stop_epsilon_mps: if self.stop_epsilon_mps.is_finite() && self.stop_epsilon_mps >= 0.0 {
                self.stop_epsilon_mps.min(VRU_STOP_EPSILON_CEILING_MPS)
            } else {
                self.stop_epsilon_mps
            },
            ..*self
        }
    }
}

/// The pedestrian input to the slow checker: the perceived VRUs plus the
/// bound's parameters, carried together so the checker call site stays a
/// single optional argument (absent → no-op).
#[derive(Debug, Clone, Copy)]
pub struct PedestrianScene<'a> {
    pub pedestrians: &'a [PerceivedPedestrian],
    pub params: VruRssParams,
    /// F6 (opt-in, default-off): impassable barrier segments that clip the
    /// omnidirectional reachable disc toward the corridor-geodesic (a pedestrian
    /// separated from the ego by one of these must walk around it, so binds
    /// less). `&[]` → the pure euclidean disc, byte-identical to the pre-F6
    /// bound. Populated ONLY by an ODD whose reviewed map profile classifies
    /// the segments impassable (`PEDESTRIAN_RSS_F6_DESIGN.md` §3.2, §4); no
    /// caller supplies barriers until that per-ODD enablement lands, so today
    /// this is always empty and the bound is unchanged.
    pub barriers: &'a [BarrierSegment],
}

fn finite_point(p: &Point) -> bool {
    p.x_m.is_finite() && p.y_m.is_finite()
}

/// A pedestrian is usable iff its position/velocity are finite AND its
/// measurement age is finite and non-negative (#789 F8). A negative or non-finite
/// age is a perception fault → fail closed (the caller treats `false` as breach).
fn pedestrian_fields_valid(p: &PerceivedPedestrian) -> bool {
    finite_point(&p.pos) && finite_point(&p.vel) && p.age_s.is_finite() && p.age_s >= 0.0
}

/// Displacement-implied speed of a trajectory segment (#789 F4). `Ok(v)` is the
/// segment's average speed for a well-formed segment (`dt` finite and positive);
/// a stationary dwell (`dt ≤ 0` with no displacement) is `Ok(0.0)`; a MALFORMED
/// segment — nonzero displacement over a non-positive/non-finite `dt` (a teleport
/// or a time reversal) — is `Err(())`, which the caller fails closed on. The point
/// is that a doer cannot DECLARE `v = 0` while its poses translate: the geometry
/// betrays the motion regardless of the declared scalar.
fn segment_implied_speed(a: &TrajectoryPoint, b: &TrajectoryPoint) -> Result<f64, ()> {
    let dt = b.time_from_start_s - a.time_from_start_s;
    let dx = b.pose.x_m - a.pose.x_m;
    let dy = b.pose.y_m - a.pose.y_m;
    let disp = (dx * dx + dy * dy).sqrt();
    if dt.is_finite() && dt > 0.0 {
        Ok(disp / dt)
    } else if disp > SEG_DEGENERATE_EPS_M {
        Err(()) // teleport / time reversal → fail closed
    } else {
        Ok(0.0) // coincident-time dwell, no motion
    }
}

/// Params are usable iff every field is finite and non-negative. A corrupt
/// parameter set must FAIL CLOSED (an infinite requirement → guaranteed
/// breach), never NaN-poison a comparison into admitting a trajectory.
fn params_valid(p: &VruRssParams) -> bool {
    let ok = |x: f64| x.is_finite() && x >= 0.0;
    ok(p.v_ped_max_mps)
        && ok(p.ped_radius_m)
        && ok(p.clearance_m)
        && ok(p.reaction_time_s)
        && ok(p.stop_epsilon_mps)
}

/// The ego-body reach term for the VRU bound (#779 F1): the max distance from
/// the pose (the REAR AXLE) to any point of the ego footprint,
/// `max(wheelbase+overhang_front, overhang_rear).hypot(half_width)` —
/// direction-independent, matching the omnidirectional disc model.
///
/// FAIL-CLOSED (Copilot #788): returns `f64::NAN` — which makes
/// [`required_pedestrian_clearance_m`] return `∞`, a guaranteed breach — on ANY
/// non-finite OR negative geometry input. `f64::max` uses IEEE `maxNum` semantics
/// (it IGNORES NaN and returns the other argument), so a naive `max` would MASK a
/// corrupt footprint field (e.g. `overhang_rear = NaN`) into a FINITE reach and
/// defeat the downstream `is_finite()` check. The explicit validation prevents that.
#[must_use]
pub fn ego_reach_m(
    wheelbase_m: f64,
    overhang_front_m: f64,
    overhang_rear_m: f64,
    width_m: f64,
) -> f64 {
    let geom_ok = [wheelbase_m, overhang_front_m, overhang_rear_m, width_m]
        .iter()
        .all(|x| x.is_finite() && *x >= 0.0);
    if !geom_ok {
        return f64::NAN; // corrupt ego geometry → fail closed downstream
    }
    f64::max(wheelbase_m + overhang_front_m, overhang_rear_m).hypot(width_m / 2.0)
}

/// The required distance between a trajectory pose (the ego REAR AXLE) and a
/// pedestrian for the pose to be VRU-safe (doc §4):
///
/// ```text
/// v_after = v + a_max · ρ                        (F2: worst-case speed AFTER the
///                                                 response phase — the plan may
///                                                 still be accelerating during ρ)
/// t_stop  = ρ + v_after / a_brake                (time to full stop)
/// d_stop  = v·ρ + ½·a_max·ρ² + v_after² / (2·a_brake)   (RSS stopping distance)
/// reach   = r_ped + v_ped_max · (t_pose + t_stop)      (grown reachable disc)
/// required = d_stop + reach + clearance + ego_reach    (F1: ego is a BODY, not a point)
/// ```
///
/// - `a_brake_mps2` is the ego's assured braking. The caller MUST pass the
///   POSTURE-COMPOSED brake (`kinematics.max_brake_mps2`), not the Nominal
///   service brake — under Degraded the vehicle is commanded to brake no harder
///   than the MRC contract, so the Nominal value would understate `d_stop` in
///   exactly the posture where a subsystem is already faulted (#779 F3).
/// - `a_max_mps2` is the ego's max acceleration — the RSS response-phase term
///   (#779 F2): during ρ the plan/actuator may still be executing acceleration,
///   which is the whole reason RSS's `d_min` carries an `a_max·ρ` term. The prior
///   constant-speed-during-ρ model understated the boundary by metres at ODD speed.
/// - `ego_reach_m` is the max distance from the pose (rear axle) to any point of
///   the ego footprint — `max(wheelbase+overhang_front, overhang_rear).hypot(half_width)`
///   (#779 F1). Direction-independent, matching the omnidirectional model. Without
///   it the distance was rear-axle-to-pedestrian and the ~3.8 m robotaxi nose swept
///   past the pedestrian before the disc growth even counted.
///
/// Returns `f64::INFINITY` — a guaranteed breach once applied — for ANY invalid
/// input: a non-positive/non-finite brake (an unbrakeable ego can never prove VRU
/// safety), a non-finite/negative accel or ego-reach, a non-finite speed/time, or
/// a corrupt parameter set (fail closed; a NaN here would otherwise poison
/// `dist < required` into admitting an unsafe trajectory).
#[must_use]
pub fn required_pedestrian_clearance_m(
    ego_speed_mps: f64,
    pose_time_s: f64,
    a_brake_mps2: f64,
    a_max_mps2: f64,
    ego_reach_m: f64,
    params: &VruRssParams,
) -> f64 {
    let pos_finite = |x: f64| x.is_finite() && x > 0.0;
    let nonneg_finite = |x: f64| x.is_finite() && x >= 0.0;
    if !pos_finite(a_brake_mps2)      // an unbrakeable ego can never prove VRU safety
        || !nonneg_finite(a_max_mps2) // #779 F2
        || !nonneg_finite(ego_reach_m) // #779 F1
        || !ego_speed_mps.is_finite()
        || !pose_time_s.is_finite()
        || !params_valid(params)
    {
        return f64::INFINITY;
    }
    let v = ego_speed_mps.abs();
    let rho = params.reaction_time_s;
    // F2 — RSS response phase: the ego may still be accelerating during ρ, so it
    // brakes from `v_after`, not `v` (Shalev-Shwartz Def. 1 / Lemma 2; IEEE 2846).
    let v_after = v + a_max_mps2 * rho;
    let t_stop = rho + v_after / a_brake_mps2;
    let d_stop = v * rho + 0.5 * a_max_mps2 * rho * rho + v_after * v_after / (2.0 * a_brake_mps2);
    let reach = params.ped_radius_m + params.v_ped_max_mps * (pose_time_s.max(0.0) + t_stop);
    // F1 — the ego is a body: the pose is the rear axle, so the front corner is
    // `ego_reach_m` ahead; add it so the STOPPING ENVELOPE (not the axle) must
    // clear the disc.
    d_stop + reach + params.clearance_m + ego_reach_m
}

/// Euclidean distance between two points.
fn euclid(a: Point, b: Point) -> f64 {
    let dx = a.x_m - b.x_m;
    let dy = a.y_m - b.y_m;
    (dx * dx + dy * dy).sqrt()
}

/// Signed orientation of `c` relative to the directed line `a→b` (2D cross
/// product). `> 0` left, `< 0` right, `0` collinear; non-finite input → NaN,
/// which makes every downstream sign comparison `false` (no crossing detected).
fn orient(a: Point, b: Point, c: Point) -> f64 {
    (b.x_m - a.x_m) * (c.y_m - a.y_m) - (b.y_m - a.y_m) * (c.x_m - a.x_m)
}

/// Do segments `p1→p2` and `q1→q2` **properly** cross (interiors intersect at a
/// single point)? STRICT by design (#789 F6): collinear overlap and mere
/// endpoint-touching return `false`. The soundness direction is asymmetric — a
/// FALSE POSITIVE (reporting a crossing where the straight path is actually
/// clear) would apply an unjustified detour and make `dist_geo > true_geodesic`
/// → UNSOUND; a false negative merely leaves the euclidean distance (no
/// relaxation, conservative). So we admit a crossing ONLY on strictly-opposite
/// orientations on both segments; any non-finite vertex → NaN orientations →
/// no crossing (the euclidean bound stands).
fn segments_properly_cross(p1: Point, p2: Point, q1: Point, q2: Point) -> bool {
    let d1 = orient(q1, q2, p1);
    let d2 = orient(q1, q2, p2);
    let d3 = orient(p1, p2, q1);
    let d4 = orient(p1, p2, q2);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

/// Every vertex of a barrier is finite (a well-formed segment). A barrier that
/// fails this is a corrupt map input; the breach gate fails the WHOLE set closed
/// to the pure disc (see the barrier gate in [`pedestrian_breach`]) — mirroring
/// the over-bound [`MAX_BARRIERS`] rule — so a single malformed barrier can never
/// fabricate a distance-shrinking detour, and (unlike a silent per-barrier skip)
/// the fail-closed action is observable.
fn barrier_finite(bar: &BarrierSegment) -> bool {
    bar.a.x_m.is_finite() && bar.a.y_m.is_finite() && bar.b.x_m.is_finite() && bar.b.y_m.is_finite()
}

/// A **lower bound on the geodesic distance** from `ped` to `pose` that respects
/// impassable `barriers` (#789 F6, `PEDESTRIAN_RSS_F6_DESIGN.md` §3). Guarantees
/// `euclidean ≤ dist_geo ≤ true_geodesic`:
///
/// - If the straight `ped→pose` segment crosses no impassable barrier →
///   the euclidean distance (no relaxation).
/// - If it crosses ≥1 barrier → the **minimum** over all crossed barriers and
///   their two endpoints `e` of `‖ped−e‖ + ‖e−pose‖` (the shortest
///   single-endpoint detour). `≥ euclidean` by the triangle inequality (so the
///   bound only ever RELAXES); `≤ true_geodesic` because the true shortest path
///   avoiding *all* barriers is at least the shortest path avoiding any single
///   crossed one, whose endpoint-bend the `min` under-cuts (so the bound stays
///   SOUND — the `min` is load-bearing for this half).
///
/// PRECONDITION: every barrier vertex is finite ([`barrier_finite`]) — the caller
/// (the breach gate) has already failed a malformed set closed to `&[]`, so this
/// hot path carries no per-barrier validation branch. `O(B)`, allocation-free.
fn barrier_aware_distance(ped: Point, pose: Point, barriers: &[BarrierSegment]) -> f64 {
    let e = euclid(ped, pose);
    if barriers.is_empty() {
        return e;
    }
    let mut best = f64::INFINITY;
    for bar in barriers {
        if segments_properly_cross(ped, pose, bar.a, bar.b) {
            let d_a = euclid(ped, bar.a) + euclid(bar.a, pose);
            let d_b = euclid(ped, bar.b) + euclid(bar.b, pose);
            best = best.min(d_a).min(d_b);
        }
    }
    // No crossing barrier → euclidean. `best` (a detour) is ≥ euclidean, so the
    // clip only relaxes; the `max(e, ..)` is belt-and-braces against float noise
    // making a detour appear a hair below euclidean.
    if best.is_finite() {
        best.max(e)
    } else {
        e
    }
}

/// **The WS-2 primitive**: does `trajectory` breach the omnidirectional
/// pedestrian bound for any (pose, pedestrian) pair?
///
/// Per pose with effective speed `> stop_epsilon`, per pedestrian: breach if the
/// euclidean pose→pedestrian distance is below the pose's
/// [`required_pedestrian_clearance_m`] plus the pedestrian's measurement-age disc
/// growth (`v_ped_max · age_s`, #789 F8). No lateral filter and no behind-ego
/// filter — omnidirectionality is the point (a VRU beside or behind the path can
/// enter it; distance + the disc's time growth keep far pedestrians from binding).
///
/// **Effective speed (#789 F4).** The stop-epsilon skip and the `d_stop` term use
/// `max(|v_declared|, displacement-implied speed of the adjacent pose pair)`, so a
/// doer emitting translating poses that DECLARE `v = 0` cannot skip the bound.
/// **Params (#789 F5)** are sanitized once here; **the pedestrian count (#789 F9)**
/// is bounded at [`MAX_PEDESTRIANS`] and the per-pose `required` is hoisted out of
/// the pedestrian loop. A non-finite pedestrian / pose, a negative-or-non-finite
/// age, an over-bound scene, or a malformed segment is a breach (fail closed).
// SAFETY: SG1 | REQ: vru-pedestrian-reachable-set-bound | TEST: pedestrian_ahead_within_stopping_envelope_breaches,pedestrian_far_ahead_is_clear,safe_stop_next_to_pedestrian_is_admitted,kerbside_pedestrian_outside_lateral_band_still_binds,non_finite_pedestrian_breaches,empty_scene_is_noop,declared_zero_velocity_but_translating_pose_still_binds,loose_params_cannot_weaken_the_bound,measurement_age_grows_the_reachable_disc,too_many_pedestrians_fails_closed,corrupt_v_ped_max_is_not_laundered_and_fails_closed
#[must_use]
pub fn pedestrian_breach(
    trajectory: &[TrajectoryPoint],
    scene: &PedestrianScene<'_>,
    a_brake_mps2: f64,
    a_max_mps2: f64,
    ego_reach_m: f64,
) -> bool {
    // Absent pedestrians → no-op (byte-identical to the empty double-loop).
    if scene.pedestrians.is_empty() {
        return false;
    }
    // F9 — fail-closed input bound: an over-bound scene is a malformed perception
    // input, never an unbounded per-tick loop.
    if scene.pedestrians.len() > MAX_PEDESTRIANS {
        return true;
    }
    // F5 — sanitize the caller-supplied params ONCE, at this single choke point, so
    // a loose param set can never weaken the bound regardless of how it was built.
    let params = scene.params.sanitized();
    // F6 — the barrier set is bounded (WCET) and fail-closed to the pure disc: an
    // over-bound OR malformed (non-finite vertex) barrier set is a corrupt map
    // input, so we IGNORE ALL barriers (euclidean, the more conservative bound)
    // rather than run an unbounded loop or relax on corrupt input. Whole-set
    // invalidation (not a per-barrier skip) keeps the fail-closed action
    // observable and consistent with the over-bound rule. `&[]` (the only case
    // today) is the pure disc.
    let barriers: &[BarrierSegment] = if scene.barriers.len() > MAX_BARRIERS
        || scene.barriers.iter().any(|b| !barrier_finite(b))
    {
        &[]
    } else {
        scene.barriers
    };
    // Validate every pedestrian up front (fail closed on non-finite fields or a
    // negative / non-finite measurement age, #789 F8).
    for ped in scene.pedestrians {
        if !pedestrian_fields_valid(ped) {
            return true;
        }
    }
    // F9 hoist — `required_base` depends only on the POSE (effective speed, time),
    // not on the pedestrian, so evaluate it ONCE per pose in the outer loop; the
    // inner pedestrian loop is a distance comparison plus the cheap per-pedestrian
    // age term. One `required` evaluation per pose, not per (pose, pedestrian).
    for (i, tp) in trajectory.iter().enumerate() {
        // Self-contained fail-closed: a non-finite pose would NaN the distance and
        // fail OPEN below. The validator's containment pass rejects such poses
        // first, but this helper is public and must not depend on that ordering.
        if !(tp.velocity_mps.is_finite()
            && tp.time_from_start_s.is_finite()
            && tp.pose.x_m.is_finite()
            && tp.pose.y_m.is_finite()
            && tp.pose.heading_rad.is_finite())
        {
            return true;
        }
        // F4 — the EFFECTIVE speed: the max of the declared speed and the
        // displacement-implied speed of the adjacent segments. A malformed segment
        // (teleport / time reversal) fails closed. This is used for BOTH the
        // stop-epsilon skip and the `d_stop` term, so a translating pose that
        // declares `v = 0` neither skips the bound nor understates the stop distance.
        let mut v_eff = tp.velocity_mps.abs();
        if i > 0 {
            match segment_implied_speed(&trajectory[i - 1], tp) {
                Ok(s) => v_eff = v_eff.max(s),
                Err(()) => return true,
            }
        }
        if i + 1 < trajectory.len() {
            match segment_implied_speed(tp, &trajectory[i + 1]) {
                Ok(s) => v_eff = v_eff.max(s),
                Err(()) => return true,
            }
        }
        if v_eff <= params.stop_epsilon_mps {
            continue;
        }
        let required_base = required_pedestrian_clearance_m(
            v_eff,
            tp.time_from_start_s,
            a_brake_mps2,
            a_max_mps2,
            ego_reach_m,
            &params,
        );
        // Fail closed on a non-finite base BEFORE adding the age term (Copilot #799):
        // `required_base` is ∞ on any fault (invalid params / brake / accel / geometry),
        // and `∞ + v_ped_max·age_s` becomes NaN when `v_ped_max` is non-finite and
        // `age_s = 0` (NaN·0 = NaN) — then `dist < NaN` is false and the check would
        // FAIL OPEN. A moving pose with an infinite requirement is an unconditional
        // breach.
        if !required_base.is_finite() {
            return true;
        }
        let pose_pt = Point {
            x_m: tp.pose.x_m,
            y_m: tp.pose.y_m,
        };
        for ped in scene.pedestrians {
            // F8 — the disc has already been growing for `age_s` since the
            // measurement was taken; extend the required clearance accordingly.
            let required = required_base + params.v_ped_max_mps * ped.age_s;
            // F6 — barrier-aware distance (a lower bound on the corridor-geodesic;
            // `barriers` is empty in every deployment today → euclidean, unchanged).
            let dist = barrier_aware_distance(ped.pos, pose_pt, barriers);
            if dist < required {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests — the primitive in isolation (integration tests live in
// kirra-ros2-adapter/tests/validation_tests.rs with the rest of the
// slow-checker suite).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::trajectory::Pose;

    fn pt(x: f64, v: f64, t: f64) -> TrajectoryPoint {
        TrajectoryPoint {
            pose: Pose {
                x_m: x,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: v,
            time_from_start_s: t,
        }
    }

    fn ped(x: f64, y: f64) -> PerceivedPedestrian {
        ped_age(x, y, 0.0)
    }

    fn ped_age(x: f64, y: f64, age_s: f64) -> PerceivedPedestrian {
        PerceivedPedestrian {
            id: 1,
            pos: Point { x_m: x, y_m: y },
            vel: Point { x_m: 0.0, y_m: 0.0 },
            age_s,
        }
    }

    const BRAKE: f64 = 4.5; // default_urban service brake (Nominal)
    const A_MAX: f64 = 2.5; // default_urban max accel (#779 F2 response phase)

    /// default_urban (robotaxi) rear-axle→front-corner reach (#779 F1):
    /// `max(wheelbase+overhang_front, overhang_rear).hypot(half_width)` =
    /// `max(2.8+0.9, 1.1).hypot(0.925)` = `3.7.hypot(0.925)` ≈ 3.814 m.
    fn ego_reach() -> f64 {
        3.7_f64.hypot(0.925)
    }

    fn required(v: f64, t: f64, p: &VruRssParams) -> f64 {
        required_pedestrian_clearance_m(v, t, BRAKE, A_MAX, ego_reach(), p)
    }

    fn breaches(traj: &[TrajectoryPoint], sc: &PedestrianScene<'_>) -> bool {
        pedestrian_breach(traj, sc, BRAKE, A_MAX, ego_reach())
    }

    fn scene(peds: &[PerceivedPedestrian]) -> PedestrianScene<'_> {
        PedestrianScene {
            pedestrians: peds,
            params: VruRssParams::default(),
            barriers: &[], // F6 default-off: pure euclidean disc
        }
    }

    fn seg(ax: f64, ay: f64, bx: f64, by: f64) -> BarrierSegment {
        BarrierSegment {
            a: Point { x_m: ax, y_m: ay },
            b: Point { x_m: bx, y_m: by },
        }
    }

    /// The formula at a worked point (doc §4.1) with the F1 ego-body + F2
    /// response-phase terms: v=2, t=0, ρ=0.5, a_brake=4.5, a_max=2.5 →
    /// v_after = 3.25; t_stop = 0.5 + 3.25/4.5 = 1.2222 s;
    /// d_stop = 1.0 + 0.3125 + 3.25²/9 = 2.4861 m;
    /// reach = 0.3 + 2.0·1.2222 = 2.7444 m;
    /// ego_reach ≈ 3.8139 m; required = 2.4861+2.7444+0.5+3.8139 = 9.5444 m.
    #[test]
    fn worked_reference_point_matches_the_doc() {
        let r = required(2.0, 0.0, &VruRssParams::default());
        assert!((r - 9.5444).abs() < 1e-3, "got {r}");
    }

    /// #779 F1 — the ego-body (footprint) term is present: the pose is the rear
    /// axle, so a pedestrian that clears the AXLE-only bound but not the front
    /// corner must still breach. Before the fix (point ego) it admitted.
    #[test]
    fn ego_footprint_term_binds_the_body_not_the_axle() {
        let p = VruRssParams::default();
        let req = required(2.0, 0.0, &p);
        // What the old point-ego formula would have demanded (no ego_reach term).
        let axle_only = req - ego_reach();
        let ped_x = axle_only + 0.1; // clears the axle bound, INSIDE the body bound
        assert!(
            ped_x < req,
            "precondition: still inside the full requirement"
        );
        let traj = [pt(0.0, 2.0, 0.0)];
        assert!(
            breaches(&traj, &scene(&[ped(ped_x, 0.0)])),
            "a pedestrian inside the ego-body envelope must breach (F1)"
        );
        // Just OUTSIDE the full requirement still admits (no over-rejection).
        assert!(!breaches(
            &[pt(0.0, 2.0, 0.0)],
            &scene(&[ped(req + 0.1, 0.0)])
        ));
    }

    /// #779 F2 — the response-phase acceleration term raises the requirement vs a
    /// constant-speed-during-ρ model: with a_max=0 the ego coasts through ρ and
    /// the requirement is strictly smaller than with the real a_max.
    #[test]
    fn response_phase_accel_term_raises_the_requirement() {
        let p = VruRssParams::default();
        let with_accel = required_pedestrian_clearance_m(4.0, 0.0, BRAKE, A_MAX, ego_reach(), &p);
        let no_accel = required_pedestrian_clearance_m(4.0, 0.0, BRAKE, 0.0, ego_reach(), &p);
        assert!(
            with_accel > no_accel,
            "the a_max·ρ response phase must add distance"
        );
    }

    /// #779 F1 / Copilot #788 — `ego_reach_m` fails closed (NaN → downstream ∞) on
    /// ANY non-finite or negative geometry. The `overhang_rear = NaN` case is the
    /// one a naive `f64::max` would MASK (max ignores NaN, returning the finite
    /// front reach) — the explicit validation must catch it.
    #[test]
    fn ego_reach_fails_closed_on_corrupt_geometry() {
        // Sound robotaxi geometry → finite, equals the worked-reference reach.
        let r = ego_reach_m(2.8, 0.9, 1.1, 1.85);
        assert!((r - ego_reach()).abs() < 1e-9, "sound geometry: got {r}");
        // A NaN in ANY field → NaN (fail closed).
        assert!(ego_reach_m(f64::NAN, 0.9, 1.1, 1.85).is_nan());
        assert!(ego_reach_m(2.8, f64::NAN, 1.1, 1.85).is_nan());
        assert!(
            ego_reach_m(2.8, 0.9, f64::NAN, 1.85).is_nan(),
            "the overhang_rear=NaN case f64::max would MASK must fail closed"
        );
        assert!(ego_reach_m(2.8, 0.9, 1.1, f64::NAN).is_nan());
        // Negative geometry also fails closed.
        assert!(ego_reach_m(2.8, 0.9, -1.0, 1.85).is_nan());
        // And the NaN reach makes the requirement ∞ (breach) downstream.
        assert_eq!(
            required_pedestrian_clearance_m(
                2.0,
                0.0,
                BRAKE,
                A_MAX,
                f64::NAN,
                &VruRssParams::default()
            ),
            f64::INFINITY
        );
    }

    /// #779 F3 — the posture-composed brake matters: the weaker Degraded MRC
    /// brake (3.0) demands MORE clearance than the Nominal service brake (4.5),
    /// and a pedestrian between the two verdicts admits under Nominal but breaches
    /// under Degraded. The validator passes `kinematics.max_brake_mps2` (3.0 under
    /// Degraded) so a faulted-posture ego is held to its actual stopping power.
    #[test]
    fn weaker_degraded_brake_demands_more_clearance() {
        let p = VruRssParams::default();
        const NOMINAL_BRAKE: f64 = 4.5;
        const DEGRADED_BRAKE: f64 = 3.0;
        let nominal =
            required_pedestrian_clearance_m(5.0, 0.0, NOMINAL_BRAKE, A_MAX, ego_reach(), &p);
        let degraded =
            required_pedestrian_clearance_m(5.0, 0.0, DEGRADED_BRAKE, A_MAX, ego_reach(), &p);
        assert!(
            degraded > nominal,
            "the weaker Degraded brake must demand MORE clearance"
        );
        // A pedestrian between the two boundaries: Nominal admits, Degraded breaches.
        let d = 0.5 * (nominal + degraded);
        let traj = [pt(0.0, 5.0, 0.0)];
        assert!(
            !pedestrian_breach(
                &traj,
                &scene(&[ped(d, 0.0)]),
                NOMINAL_BRAKE,
                A_MAX,
                ego_reach()
            ),
            "the Nominal brake admits a pedestrian at the mid-distance"
        );
        assert!(
            pedestrian_breach(
                &traj,
                &scene(&[ped(d, 0.0)]),
                DEGRADED_BRAKE,
                A_MAX,
                ego_reach()
            ),
            "the Degraded brake breaches the same pedestrian (F3)"
        );
    }

    #[test]
    fn pedestrian_ahead_within_stopping_envelope_breaches() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(breaches(&traj, &scene(&[ped(3.0, 0.0)])));
    }

    #[test]
    fn pedestrian_far_ahead_is_clear() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(!breaches(&traj, &scene(&[ped(40.0, 0.0)])));
    }

    /// THE STOP-PROPOSAL INVARIANT: a stopped/stopping trajectory next to a
    /// pedestrian is admitted — the gate must never make `safe_stop`
    /// inadmissible (that would deadlock the doer↔checker loop).
    #[test]
    fn safe_stop_next_to_pedestrian_is_admitted() {
        let stop = [pt(0.0, 0.0, 0.0), pt(0.0, 0.0, 1.0)];
        assert!(!breaches(&stop, &scene(&[ped(0.5, 0.5)])));
    }

    /// Omnidirectionality: a kerbside pedestrian OUTSIDE the vehicle-RSS
    /// lateral band still binds — they can step in. (The vehicle-object RSS
    /// would lateral-filter this away; the VRU bound must not.)
    #[test]
    fn kerbside_pedestrian_outside_lateral_band_still_binds() {
        let traj = [pt(0.0, 4.0, 0.0), pt(4.0, 4.0, 1.0)];
        // 2.5 m lateral, 4 m ahead: well inside required (~13 m at v=4).
        assert!(breaches(&traj, &scene(&[ped(4.0, 2.5)])));
    }

    #[test]
    fn non_finite_pedestrian_breaches() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(breaches(&traj, &scene(&[ped(f64::NAN, 0.0)])));
    }

    #[test]
    fn empty_scene_is_noop() {
        let traj = [pt(0.0, 10.0, 0.0), pt(10.0, 10.0, 1.0)];
        assert!(!breaches(&traj, &scene(&[])));
    }

    /// Fail-closed on an unbrakeable ego, AND on a corrupt accel / ego-reach
    /// (the new #779 F1/F2 inputs): zero/negative/non-finite brake, non-finite
    /// or negative a_max, non-finite or negative ego_reach → infinite requirement.
    #[test]
    fn non_positive_brake_and_bad_geometry_fail_closed() {
        let p = VruRssParams::default();
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, 0.0, A_MAX, ego_reach(), &p),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, f64::NAN, A_MAX, ego_reach(), &p),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, BRAKE, f64::NAN, ego_reach(), &p),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, BRAKE, -1.0, ego_reach(), &p),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, BRAKE, A_MAX, f64::NAN, &p),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, BRAKE, A_MAX, -1.0, &p),
            f64::INFINITY
        );
    }

    /// Fail-closed on corrupt inputs/params: a NaN speed, time, or ANY
    /// parameter field yields an infinite requirement — never a NaN that
    /// would fail OPEN in `dist < required`.
    #[test]
    fn corrupt_inputs_and_params_fail_closed_not_open() {
        let p = VruRssParams::default();
        assert_eq!(required(f64::NAN, 0.0, &p), f64::INFINITY);
        assert_eq!(required(1.0, f64::NAN, &p), f64::INFINITY);
        for corrupt in [
            VruRssParams {
                v_ped_max_mps: f64::NAN,
                ..p
            },
            VruRssParams {
                ped_radius_m: -1.0,
                ..p
            },
            VruRssParams {
                clearance_m: f64::INFINITY,
                ..p
            },
            VruRssParams {
                reaction_time_s: -0.5,
                ..p
            },
            VruRssParams {
                stop_epsilon_mps: f64::NAN,
                ..p
            },
        ] {
            let r = required(1.0, 0.0, &corrupt);
            assert_eq!(r, f64::INFINITY, "corrupt {corrupt:?} must fail closed");
        }
        // And through the breach predicate: corrupt params → a moving pose
        // near ANY pedestrian breaches (never admits).
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        let sc = PedestrianScene {
            pedestrians: &[ped(1000.0, 0.0)],
            params: VruRssParams {
                reaction_time_s: f64::NAN,
                ..p
            },
            barriers: &[],
        };
        assert!(breaches(&traj, &sc));
    }

    /// Fail-closed on a non-finite trajectory POSE (the distance would NaN
    /// and fail open) — self-contained, not dependent on containment order.
    #[test]
    fn non_finite_pose_breaches() {
        let mut traj = vec![pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        traj[1].pose.x_m = f64::NAN;
        assert!(breaches(&traj, &scene(&[ped(50.0, 0.0)])));
    }

    /// Monotonicity (the safety shape): the requirement never DECREASES
    /// with ego speed or pose time — faster/later can only demand more room.
    #[test]
    fn requirement_is_monotone_in_speed_and_time() {
        let p = VruRssParams::default();
        let mut prev = 0.0;
        for i in 0..50 {
            let v = f64::from(i) * 0.5;
            let r = required(v, 0.0, &p);
            assert!(r >= prev, "requirement must not decrease with speed");
            prev = r;
        }
        let early = required(3.0, 0.5, &p);
        let late = required(3.0, 4.0, &p);
        assert!(late > early, "a later pose faces a larger reachable disc");
    }

    /// #789 F4 — the stop-epsilon skip must NOT trust the declared `velocity_mps`.
    /// A trajectory whose poses TRANSLATE (x: 0 → 2 → 4 over t 0,1,2) while
    /// declaring `v = 0` is really moving at 2 m/s; pre-fix every pose was
    /// stop-epsilon-skipped and a pedestrian in the path was missed. The
    /// displacement-implied speed must bind it.
    #[test]
    fn declared_zero_velocity_but_translating_pose_still_binds() {
        let traj = [pt(0.0, 0.0, 0.0), pt(2.0, 0.0, 1.0), pt(4.0, 0.0, 2.0)];
        assert!(
            breaches(&traj, &scene(&[ped(3.0, 0.0)])),
            "a translating trajectory declaring v=0 must not bypass the VRU bound (F4)"
        );
        // Control: a genuinely stationary trajectory (no translation, v=0) is still
        // admitted next to the same pedestrian — the stop-proposal invariant holds.
        let stopped = [pt(0.0, 0.0, 0.0), pt(0.0, 0.0, 1.0)];
        assert!(!breaches(&stopped, &scene(&[ped(3.0, 0.0)])));
    }

    /// #789 F4 — a teleport / time-reversal (nonzero displacement over a
    /// non-positive `dt`) is a malformed trajectory and fails closed.
    #[test]
    fn malformed_segment_fails_closed() {
        // Two poses at the SAME time but different positions → instantaneous
        // translation. No finite implied speed is definable → breach.
        let teleport = [pt(0.0, 0.0, 0.0), pt(5.0, 0.0, 0.0)];
        assert!(breaches(&teleport, &scene(&[ped(1000.0, 0.0)])));
    }

    /// #789 F5 — caller-supplied params cannot WEAKEN the bound. A params set with
    /// `stop_epsilon = 5.0` (would skip a 2 m/s pose) and `v_ped_max = 0.0` (would
    /// zero the disc growth) is sanitized: the epsilon is clamped to the kernel
    /// stop-and-hold value and `v_ped_max` is floored at 2.0, so the pedestrian
    /// still binds. Pre-fix the loose epsilon skipped the pose and it admitted.
    #[test]
    fn loose_params_cannot_weaken_the_bound() {
        let loose = VruRssParams {
            v_ped_max_mps: 0.0,
            stop_epsilon_mps: 5.0,
            ..VruRssParams::default()
        };
        // The sanitizer floors/clamps both fields (monotone-tightening).
        let s = loose.sanitized();
        assert_eq!(s.v_ped_max_mps, V_PED_MAX_FLOOR_MPS);
        assert!(s.stop_epsilon_mps <= VRU_STOP_EPSILON_CEILING_MPS);
        // And through the predicate: the 2 m/s pose is NOT skipped despite eps=5.0.
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        let sc = PedestrianScene {
            pedestrians: &[ped(3.0, 0.0)],
            params: loose,
            barriers: &[],
        };
        assert!(
            pedestrian_breach(&traj, &sc, BRAKE, A_MAX, ego_reach()),
            "a loose stop_epsilon must be clamped so the pose is not skipped (F5)"
        );
    }

    /// #789 F8 — a stale measurement grows the reachable disc by `v_ped_max · age`.
    /// A pedestrian just OUTSIDE the fresh (age 0) requirement is admitted; the same
    /// pedestrian measured 2 s ago (disc grown by 2·2 = 4 m) now binds. A negative
    /// or non-finite age fails closed.
    #[test]
    fn measurement_age_grows_the_reachable_disc() {
        let p = VruRssParams::default();
        // Single distinct pose (duplicated at t=0 so the implied speed is 0 and the
        // required is the age-free base at v=2).
        let traj = [pt(0.0, 2.0, 0.0), pt(0.0, 2.0, 0.0)];
        let req = required(2.0, 0.0, &p);
        assert!(
            !breaches(&traj, &scene(&[ped_age(req + 1.0, 0.0, 0.0)])),
            "fresh: admits"
        );
        assert!(
            breaches(&traj, &scene(&[ped_age(req + 1.0, 0.0, 2.0)])),
            "a 2 s stale measurement grows the disc by v_ped_max·age = 4 m → binds (F8)"
        );
        // Fail-closed on a bad age even for a distant pedestrian.
        assert!(breaches(&traj, &scene(&[ped_age(1000.0, 0.0, -0.1)])));
        assert!(breaches(&traj, &scene(&[ped_age(1000.0, 0.0, f64::NAN)])));
    }

    /// #789 (Copilot #799) — a corrupt `v_ped_max` must fail closed, not be
    /// LAUNDERED by `sanitized()` into a valid 2.0, and the split age term must not
    /// NaN-poison the requirement into failing OPEN. A negative value stays negative
    /// (→ `params_valid` breach); a non-finite value forces `required_base = ∞`,
    /// whose `∞ + v_ped_max·age` (NaN at age 0) is caught by the finite guard.
    #[test]
    fn corrupt_v_ped_max_is_not_laundered_and_fails_closed() {
        // A negative v_ped_max is NOT floored to 2.0.
        let neg = VruRssParams {
            v_ped_max_mps: -1.0,
            ..VruRssParams::default()
        };
        assert!(
            neg.sanitized().v_ped_max_mps < 0.0,
            "a negative v_ped_max must NOT be floored"
        );
        // Through the predicate: a moving pose with a corrupt v_ped_max breaches even
        // a FRESH (age 0) pedestrian placed arbitrarily far — no NaN fail-open. Each
        // of these silently ADMITTED before the fix (negative → laundered to 2.0 with
        // a finite requirement; NaN/∞ → `∞ + NaN` → `dist < NaN` false).
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let sc = PedestrianScene {
                pedestrians: &[ped_age(1000.0, 0.0, 0.0)],
                params: VruRssParams {
                    v_ped_max_mps: bad,
                    ..VruRssParams::default()
                },
                barriers: &[],
            };
            assert!(
                pedestrian_breach(&traj, &sc, BRAKE, A_MAX, ego_reach()),
                "corrupt v_ped_max={bad} must fail closed, not NaN-poison to fail open"
            );
        }
    }

    /// #789 F9 — the pedestrian count is a fail-closed input bound: a scene AT the
    /// bound with all pedestrians far is admitted; one OVER the bound breaches
    /// regardless of placement (a malformed / over-bound perception input).
    #[test]
    fn too_many_pedestrians_fails_closed() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        let ok: Vec<_> = (0..MAX_PEDESTRIANS)
            .map(|i| ped(1000.0 + i as f64, 0.0))
            .collect();
        assert!(
            !breaches(&traj, &scene(&ok)),
            "a scene at the bound with far VRUs admits"
        );
        let over: Vec<_> = (0..=MAX_PEDESTRIANS)
            .map(|i| ped(1000.0 + i as f64, 0.0))
            .collect();
        assert!(
            breaches(&traj, &scene(&over)),
            "an over-bound scene must fail closed (F9)"
        );
    }

    /// An INDEPENDENT reference for the breach predicate: pedestrian-outer /
    /// pose-inner, recomputing `required` per (pedestrian, pose) pair — the
    /// structure the F9 hoist replaced. Used by the proptest to prove the hoisted
    /// implementation is verdict-identical to the per-pair spec.
    fn naive_reference_breach(
        traj: &[TrajectoryPoint],
        scene: &PedestrianScene<'_>,
        brake: f64,
        amax: f64,
        reach: f64,
    ) -> bool {
        if scene.pedestrians.is_empty() {
            return false;
        }
        if scene.pedestrians.len() > MAX_PEDESTRIANS {
            return true;
        }
        let params = scene.params.sanitized();
        let barriers: &[BarrierSegment] = if scene.barriers.len() > MAX_BARRIERS
            || scene.barriers.iter().any(|b| !barrier_finite(b))
        {
            &[]
        } else {
            scene.barriers
        };
        for ped in scene.pedestrians {
            if !pedestrian_fields_valid(ped) {
                return true;
            }
        }
        for ped in scene.pedestrians {
            for (i, tp) in traj.iter().enumerate() {
                if !(tp.velocity_mps.is_finite()
                    && tp.time_from_start_s.is_finite()
                    && tp.pose.x_m.is_finite()
                    && tp.pose.y_m.is_finite()
                    && tp.pose.heading_rad.is_finite())
                {
                    return true;
                }
                let mut v_eff = tp.velocity_mps.abs();
                if i > 0 {
                    match segment_implied_speed(&traj[i - 1], tp) {
                        Ok(s) => v_eff = v_eff.max(s),
                        Err(()) => return true,
                    }
                }
                if i + 1 < traj.len() {
                    match segment_implied_speed(tp, &traj[i + 1]) {
                        Ok(s) => v_eff = v_eff.max(s),
                        Err(()) => return true,
                    }
                }
                if v_eff <= params.stop_epsilon_mps {
                    continue;
                }
                // Mirror the fail-closed guard in `pedestrian_breach` (Copilot #799):
                // an infinite base must breach before the age term can NaN-poison it.
                let required_base = required_pedestrian_clearance_m(
                    v_eff,
                    tp.time_from_start_s,
                    brake,
                    amax,
                    reach,
                    &params,
                );
                if !required_base.is_finite() {
                    return true;
                }
                let required = required_base + params.v_ped_max_mps * ped.age_s;
                let pose_pt = Point {
                    x_m: tp.pose.x_m,
                    y_m: tp.pose.y_m,
                };
                let dist = barrier_aware_distance(ped.pos, pose_pt, barriers);
                if dist < required {
                    return true;
                }
            }
        }
        false
    }

    proptest::proptest! {
        /// #789 — the F9-hoisted [`pedestrian_breach`] is verdict-identical to the
        /// independent per-pair reference over random finite trajectories and
        /// scenes (the review-recommended `breach ⟺ disc/envelope overlap` cross
        /// check, phrased as hoisted-vs-naive equivalence so it also guards F4/F8).
        #[test]
        fn hoisted_breach_matches_naive_reference(
            xs in proptest::collection::vec(-5.0f64..50.0, 2..6),
            vs in proptest::collection::vec(0.0f64..12.0, 2..6),
            pxs in proptest::collection::vec(-5.0f64..60.0, 0..5),
            pys in proptest::collection::vec(-5.0f64..5.0, 0..5),
            ages in proptest::collection::vec(0.0f64..3.0, 0..5),
        ) {
            let n = xs.len().min(vs.len());
            let traj: Vec<_> = (0..n).map(|i| pt(xs[i], vs[i], i as f64)).collect();
            let m = pxs.len().min(pys.len()).min(ages.len());
            let peds: Vec<_> = (0..m).map(|i| ped_age(pxs[i], pys[i], ages[i])).collect();
            let sc = scene(&peds);
            let got = pedestrian_breach(&traj, &sc, BRAKE, A_MAX, ego_reach());
            let want = naive_reference_breach(&traj, &sc, BRAKE, A_MAX, ego_reach());
            proptest::prop_assert_eq!(got, want);
        }
    }

    // -----------------------------------------------------------------------
    // #789 F6 — the default-off impassable-barrier corridor-clip seam.
    // -----------------------------------------------------------------------

    /// The default (`barriers: &[]`) is byte-identical to the pre-F6 pure disc:
    /// the same pedestrian breaches with an empty barrier set exactly as it did
    /// before the seam existed (the whole existing suite runs on `scene()`,
    /// which is `&[]`; this pins the property by name).
    #[test]
    fn empty_barriers_are_the_pure_euclidean_disc() {
        let traj = [pt(0.0, 2.0, 0.0)];
        let peds = [ped(8.0, 0.0)]; // 8 m < required(2) ≈ 9.54 → breaches
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &[],
        };
        assert!(breaches(&traj, &sc));
    }

    /// `euclid` computes the exact 2-D distance. Fully-asymmetric operands (both
    /// coords distinct and non-zero) so every arithmetic mutation diverges.
    #[test]
    fn euclid_is_the_exact_2d_distance() {
        // (5,9)→(1,2): dx=4, dy=7 → √(16+49)=√65.
        let d = euclid(Point { x_m: 5.0, y_m: 9.0 }, Point { x_m: 1.0, y_m: 2.0 });
        assert!((d - 65.0_f64.sqrt()).abs() < 1e-12, "got {d}");
    }

    /// `orient` computes the exact signed area (2-D cross). A single asymmetric
    /// triple pins every term — any `+`/`-`/`*`/`/` flip changes the value.
    #[test]
    fn orient_is_the_exact_signed_area() {
        // a=(1,2) b=(4,6) c=(3,9): (4-1)(9-2) − (6-2)(3-1) = 21 − 8 = 13.
        let o = orient(
            Point { x_m: 1.0, y_m: 2.0 },
            Point { x_m: 4.0, y_m: 6.0 },
            Point { x_m: 3.0, y_m: 9.0 },
        );
        assert!((o - 13.0).abs() < 1e-12, "got {o}");
        // Sign flips with orientation (c on the other side of a→b).
        let o_rev = orient(
            Point { x_m: 1.0, y_m: 2.0 },
            Point { x_m: 4.0, y_m: 6.0 },
            Point {
                x_m: 3.0,
                y_m: -9.0,
            },
        );
        assert!(o_rev < 0.0, "opposite side → negative, got {o_rev}");
    }

    /// `barrier_aware_distance` is bracketed by euclidean below and a real
    /// single-endpoint detour: no barriers → euclidean exactly; a crossing wall
    /// → the `‖ped−e‖+‖e−pose‖` detour (never below euclidean). ASYMMETRIC
    /// endpoints (`d_a ≠ d_b`) in both orders so the `min` cannot mask a mutation
    /// of either leg's `+`.
    #[test]
    fn barrier_aware_distance_is_bracketed_by_euclidean_and_a_detour() {
        let ped_p = Point { x_m: 8.0, y_m: 0.0 };
        let pose_p = Point { x_m: 0.0, y_m: 0.0 };
        let e = euclid(ped_p, pose_p); // 8.0
        assert_eq!(barrier_aware_distance(ped_p, pose_p, &[]), e);

        // Symmetric wall y∈[-3,3]: each endpoint detour is 5+5 = 10 m.
        let d = barrier_aware_distance(ped_p, pose_p, &[seg(4.0, -3.0, 4.0, 3.0)]);
        assert!(d >= e, "the clip only ever relaxes (≥ euclidean)");
        assert!((d - 10.0).abs() < 1e-9, "single-detour 5+5, got {d}");

        // Asymmetric wall y∈[-1,10], endpoint a=(4,-1) is the nearer bend:
        // d_a = 2√17 ≈ 8.246 (the min), d_b = 2√116 ≈ 21.54. Pins leg-a's `+`.
        let d_lo_a = barrier_aware_distance(ped_p, pose_p, &[seg(4.0, -1.0, 4.0, 10.0)]);
        let want_a = 2.0 * 17.0_f64.sqrt();
        assert!(
            (d_lo_a - want_a).abs() < 1e-9,
            "min via endpoint a, got {d_lo_a}"
        );

        // Endpoints swapped → endpoint b=(4,-1) is now the nearer bend. Pins
        // leg-b's `+` (which the previous config's `min` would otherwise mask).
        let d_lo_b = barrier_aware_distance(ped_p, pose_p, &[seg(4.0, 10.0, 4.0, -1.0)]);
        assert!(
            (d_lo_b - want_a).abs() < 1e-9,
            "min via endpoint b, got {d_lo_b}"
        );
    }

    /// `segments_properly_cross` agrees with an INDEPENDENT parametric
    /// line-intersection reference (exact integer arithmetic) over an exhaustive
    /// small grid. The two derivations differ (double orientation-sign test vs a
    /// solved `(t,u)` interior check), so any mutation of `orient` or the sign
    /// logic diverges on some grid configuration.
    #[test]
    fn properly_cross_matches_independent_parametric_reference() {
        // Exact integer parametric reference: P(t)=p1+t·(p2−p1), Q(u)=q1+u·(q2−q1);
        // a PROPER crossing iff a unique solution with 0<t<1 and 0<u<1.
        fn ref_cross(p1: (i64, i64), p2: (i64, i64), q1: (i64, i64), q2: (i64, i64)) -> bool {
            let (rx, ry) = (p2.0 - p1.0, p2.1 - p1.1);
            let (sx, sy) = (q2.0 - q1.0, q2.1 - q1.1);
            let denom = rx * sy - ry * sx;
            if denom == 0 {
                return false; // parallel or collinear (or a degenerate point)
            }
            let (qpx, qpy) = (q1.0 - p1.0, q1.1 - p1.1);
            let mut tn = qpx * sy - qpy * sx; // t = tn/denom
            let mut un = qpx * ry - qpy * rx; // u = un/denom
            let mut d = denom;
            if d < 0 {
                d = -d;
                tn = -tn;
                un = -un;
            }
            tn > 0 && tn < d && un > 0 && un < d
        }
        let pt_of = |v: (i64, i64)| Point {
            x_m: v.0 as f64,
            y_m: v.1 as f64,
        };
        // A compact asymmetric grid; exhaustive over all 4 segment endpoints.
        let coords: [(i64, i64); 6] = [(0, 0), (2, 0), (0, 2), (2, 2), (1, 3), (3, 1)];
        for &p1 in &coords {
            for &p2 in &coords {
                for &q1 in &coords {
                    for &q2 in &coords {
                        let got =
                            segments_properly_cross(pt_of(p1), pt_of(p2), pt_of(q1), pt_of(q2));
                        let want = ref_cross(p1, p2, q1, q2);
                        assert_eq!(got, want, "cross({p1:?},{p2:?},{q1:?},{q2:?})");
                    }
                }
            }
        }
    }

    /// An impassable barrier BETWEEN ego and pedestrian relaxes the bound: a
    /// pedestrian that breaches the pure disc is admitted once the straight path
    /// must detour around a wall and that detour exceeds the requirement.
    #[test]
    fn impassable_barrier_admits_a_pedestrian_it_forces_around() {
        let traj = [pt(0.0, 2.0, 0.0)]; // pose at origin, v=2 → required ≈ 9.54
        let peds = [ped(8.0, 0.0)];
        // Pure disc: 8 m < 9.54 → breaches.
        assert!(breaches(&traj, &scene(&peds)));
        // Wall at x=4 spanning y∈[-3,3] → detour 10 m > 9.54 → admits.
        let barriers = [seg(4.0, -3.0, 4.0, 3.0)];
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &barriers,
        };
        assert!(
            !breaches(&traj, &sc),
            "the 10 m barrier detour exceeds the requirement → admit"
        );
    }

    /// NEGATIVE CONTROL for the load-bearing input gate (§2): a merely-painted
    /// lane edge is NEVER supplied in `barriers` (only ODD-classified impassable
    /// segments are), so at the SAME geometry the bound stays the pure disc and
    /// the pedestrian still breaches — a paint line can never launder a
    /// pedestrian out of the reachable set.
    #[test]
    fn a_passable_painted_edge_is_never_supplied_and_still_binds() {
        let traj = [pt(0.0, 2.0, 0.0)];
        let peds = [ped(8.0, 0.0)];
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &[], // passable edge → not classified impassable → absent
        };
        assert!(breaches(&traj, &sc));
    }

    /// A barrier that does NOT separate the pedestrian from the ego (the straight
    /// path never crosses it) does not relax — the euclidean bound stands.
    #[test]
    fn a_non_crossing_barrier_does_not_relax_the_bound() {
        let traj = [pt(0.0, 2.0, 0.0)];
        let peds = [ped(8.0, 0.0)];
        // A wall well off to the side (y∈[10,13]); the ped→pose segment on y=0
        // never crosses it.
        let barriers = [seg(4.0, 10.0, 4.0, 13.0)];
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &barriers,
        };
        assert!(
            breaches(&traj, &sc),
            "a non-crossing barrier must not relax the bound"
        );
    }

    /// An over-bound barrier set (a malformed map input) fails closed to the pure
    /// disc — the whole set is IGNORED, never partially applied. The inclusive
    /// boundary (exactly `MAX_BARRIERS`) still clips.
    #[test]
    fn an_over_bound_barrier_set_fails_closed_to_the_pure_disc() {
        let traj = [pt(0.0, 2.0, 0.0)];
        let peds = [ped(8.0, 0.0)];
        // MAX_BARRIERS+1 crossing walls → ignored → euclidean → breaches.
        let too_many = vec![seg(4.0, -3.0, 4.0, 3.0); MAX_BARRIERS + 1];
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &too_many,
        };
        assert!(
            breaches(&traj, &sc),
            "an over-bound barrier set must fail closed to the pure disc"
        );
        // Exactly MAX_BARRIERS (inclusive) DOES clip → admits.
        let at_cap = vec![seg(4.0, -3.0, 4.0, 3.0); MAX_BARRIERS];
        let sc_ok = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &at_cap,
        };
        assert!(
            !breaches(&traj, &sc_ok),
            "MAX_BARRIERS (inclusive) still clips"
        );
    }

    /// A non-finite barrier fails the WHOLE set closed to the pure disc: a scene
    /// pairing a VALID crossing wall (which alone would admit) with one corrupt
    /// (NaN-vertex) barrier breaches — the valid wall's relaxation is discarded
    /// along with the corrupt one, so a corrupt barrier can never launder a
    /// pedestrian out of the reachable set (and, unlike a silent per-barrier skip,
    /// this is observable — the valid wall's admit is withdrawn).
    #[test]
    fn a_non_finite_barrier_fails_the_whole_set_closed() {
        let traj = [pt(0.0, 2.0, 0.0)];
        let peds = [ped(8.0, 0.0)];
        // The valid wall ALONE admits (proven by `impassable_barrier_admits…`).
        let valid_wall = seg(4.0, -3.0, 4.0, 3.0);
        // Pairing it with a NaN-vertex barrier invalidates the whole set → the
        // bound reverts to the pure disc → 8 m < 9.54 → breaches.
        let barriers = [valid_wall, seg(4.0, f64::NAN, 4.0, 3.0)];
        let sc = PedestrianScene {
            pedestrians: &peds,
            params: VruRssParams::default(),
            barriers: &barriers,
        };
        assert!(
            breaches(&traj, &sc),
            "one corrupt barrier must fail the whole set closed to the pure disc"
        );
        // Each vertex coordinate matters: the same corrupt barrier with the NaN in
        // any single position still invalidates the set (guards `barrier_finite`).
        for corrupt in [
            seg(f64::NAN, 0.0, 4.0, 3.0),
            seg(4.0, f64::NAN, 4.0, 3.0),
            seg(4.0, -3.0, f64::INFINITY, 3.0),
            seg(4.0, -3.0, 4.0, f64::NAN),
        ] {
            let bars = [valid_wall, corrupt];
            let sc = PedestrianScene {
                pedestrians: &peds,
                params: VruRssParams::default(),
                barriers: &bars,
            };
            assert!(
                breaches(&traj, &sc),
                "a non-finite vertex in any position must invalidate the set"
            );
        }
    }

    proptest::proptest! {
        /// #789 F6 — barriers only ever RELAX: a scene that breaches WITH a
        /// barrier set must also breach WITHOUT it (the clip can turn a breach
        /// into an admit, never the reverse — the soundness half of §3). Also
        /// re-checks hoisted-vs-naive verdict equivalence over a NON-empty
        /// barrier set. Random finite vertical walls over the shared grid.
        #[test]
        fn barriers_only_relax_and_stay_hoist_equivalent(
            xs in proptest::collection::vec(-5.0f64..50.0, 2..5),
            vs in proptest::collection::vec(0.0f64..12.0, 2..5),
            pxs in proptest::collection::vec(-5.0f64..60.0, 0..4),
            pys in proptest::collection::vec(-5.0f64..5.0, 0..4),
            bxs in proptest::collection::vec(-5.0f64..60.0, 0..4),
            bys in proptest::collection::vec(-5.0f64..5.0, 0..4),
        ) {
            let n = xs.len().min(vs.len());
            let traj: Vec<_> = (0..n).map(|i| pt(xs[i], vs[i], i as f64)).collect();
            let m = pxs.len().min(pys.len());
            let peds: Vec<_> = (0..m).map(|i| ped(pxs[i], pys[i])).collect();
            let k = bxs.len().min(bys.len());
            let barriers: Vec<_> = (0..k)
                .map(|i| seg(bxs[i], bys[i] - 2.0, bxs[i], bys[i] + 2.0))
                .collect();
            let with_b = PedestrianScene {
                pedestrians: &peds,
                params: VruRssParams::default(),
                barriers: &barriers,
            };
            let no_b = scene(&peds);
            let breach_with = pedestrian_breach(&traj, &with_b, BRAKE, A_MAX, ego_reach());
            let breach_without = pedestrian_breach(&traj, &no_b, BRAKE, A_MAX, ego_reach());
            // Relaxation-only: a barrier-set breach implies a pure-disc breach.
            if breach_with {
                proptest::prop_assert!(breach_without);
            }
            // Hoisted-vs-naive equivalence holds WITH barriers too.
            let naive_with = naive_reference_breach(&traj, &with_b, BRAKE, A_MAX, ego_reach());
            proptest::prop_assert_eq!(breach_with, naive_with);
        }
    }
}
