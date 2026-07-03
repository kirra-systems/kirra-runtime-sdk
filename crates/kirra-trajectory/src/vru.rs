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

use kirra_core::corridor::Point;
use kirra_core::trajectory::TrajectoryPoint;

/// A perceived pedestrian / VRU. Deliberately minimal for v0: the
/// omnidirectional model needs only a position (velocity is accepted for
/// forward-compatibility with the directed refinement but does not weaken
/// the v0 bound).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerceivedPedestrian {
    pub id: u64,
    /// Position, ego-world frame (same frame as `PerceivedObject.pos`).
    pub pos: Point,
    /// Tracked velocity vector, m/s (informational in v0 — the reachable
    /// disc assumes `v_ped_max` in every direction regardless).
    pub vel: Point,
}

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

/// The pedestrian input to the slow checker: the perceived VRUs plus the
/// bound's parameters, carried together so the checker call site stays a
/// single optional argument (absent → no-op).
#[derive(Debug, Clone, Copy)]
pub struct PedestrianScene<'a> {
    pub pedestrians: &'a [PerceivedPedestrian],
    pub params: VruRssParams,
}

fn finite_point(p: &Point) -> bool {
    p.x_m.is_finite() && p.y_m.is_finite()
}

fn pedestrian_fields_finite(p: &PerceivedPedestrian) -> bool {
    finite_point(&p.pos) && finite_point(&p.vel)
}

/// The required center-to-center distance between a trajectory pose and a
/// pedestrian for the pose to be VRU-safe (doc §4):
///
/// ```text
/// t_stop  = ρ + v / a_brake                     (time to full stop)
/// d_stop  = v·ρ + v² / (2·a_brake)              (ego stopping distance)
/// reach   = r_ped + v_ped_max · (t_pose + t_stop)  (grown reachable disc)
/// required = d_stop + reach + clearance
/// ```
///
/// `a_brake_mps2` is the ego's assured service braking (the per-class
/// `VehicleConfig::max_decel_mps2`). Returns `f64::INFINITY` for a
/// non-positive/non-finite brake (an unbrakeable ego can never prove VRU
/// safety — fail closed).
#[must_use]
pub fn required_pedestrian_clearance_m(
    ego_speed_mps: f64,
    pose_time_s: f64,
    a_brake_mps2: f64,
    params: &VruRssParams,
) -> f64 {
    if !(a_brake_mps2.is_finite() && a_brake_mps2 > 0.0) {
        return f64::INFINITY;
    }
    let v = ego_speed_mps.abs();
    let rho = params.reaction_time_s;
    let t_stop = rho + v / a_brake_mps2;
    let d_stop = v * rho + v * v / (2.0 * a_brake_mps2);
    let reach = params.ped_radius_m + params.v_ped_max_mps * (pose_time_s.max(0.0) + t_stop);
    d_stop + reach + params.clearance_m
}

/// **The WS-2 primitive**: does `trajectory` breach the omnidirectional
/// pedestrian bound for any (pose, pedestrian) pair?
///
/// Per pose with `|v| > stop_epsilon`, per pedestrian: breach if the
/// euclidean pose→pedestrian distance is below
/// [`required_pedestrian_clearance_m`]. No lateral filter and no
/// behind-ego filter — omnidirectionality is the point (a VRU beside or
/// behind the path can enter it; distance + the disc's time growth keep
/// far pedestrians from binding). A non-finite pedestrian or a non-finite
/// pose time/speed is a breach (fail closed).
// SAFETY: SG1 | REQ: vru-pedestrian-reachable-set-bound | TEST: pedestrian_ahead_within_stopping_envelope_breaches,pedestrian_far_ahead_is_clear,safe_stop_next_to_pedestrian_is_admitted,kerbside_pedestrian_outside_lateral_band_still_binds,non_finite_pedestrian_breaches,empty_scene_is_noop
#[must_use]
pub fn pedestrian_breach(
    trajectory: &[TrajectoryPoint],
    scene: &PedestrianScene<'_>,
    a_brake_mps2: f64,
) -> bool {
    for ped in scene.pedestrians {
        if !pedestrian_fields_finite(ped) {
            return true;
        }
        for tp in trajectory {
            let v = tp.velocity_mps;
            let t = tp.time_from_start_s;
            if !(v.is_finite() && t.is_finite()) {
                return true;
            }
            if v.abs() <= scene.params.stop_epsilon_mps {
                continue;
            }
            let required = required_pedestrian_clearance_m(v, t, a_brake_mps2, &scene.params);
            let dx = ped.pos.x_m - tp.pose.x_m;
            let dy = ped.pos.y_m - tp.pose.y_m;
            let dist = (dx * dx + dy * dy).sqrt();
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
            pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: v,
            time_from_start_s: t,
        }
    }

    fn ped(x: f64, y: f64) -> PerceivedPedestrian {
        PerceivedPedestrian { id: 1, pos: Point { x_m: x, y_m: y }, vel: Point { x_m: 0.0, y_m: 0.0 } }
    }

    const BRAKE: f64 = 4.5; // default_urban service brake

    fn scene(peds: &[PerceivedPedestrian]) -> PedestrianScene<'_> {
        PedestrianScene { pedestrians: peds, params: VruRssParams::default() }
    }

    /// The formula at a worked point (doc §4.1): v=2, t=0, ρ=0.5, a=4.5 →
    /// t_stop = 0.5 + 0.444 = 0.944s; d_stop = 1.0 + 0.444 = 1.444m;
    /// reach = 0.3 + 2.0·0.944 = 2.189m; required = 1.444+2.189+0.5 = 4.133m.
    #[test]
    fn worked_reference_point_matches_the_doc() {
        let r = required_pedestrian_clearance_m(2.0, 0.0, BRAKE, &VruRssParams::default());
        assert!((r - 4.1333).abs() < 1e-3, "got {r}");
    }

    #[test]
    fn pedestrian_ahead_within_stopping_envelope_breaches() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(pedestrian_breach(&traj, &scene(&[ped(3.0, 0.0)]), BRAKE));
    }

    #[test]
    fn pedestrian_far_ahead_is_clear() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(!pedestrian_breach(&traj, &scene(&[ped(40.0, 0.0)]), BRAKE));
    }

    /// THE STOP-PROPOSAL INVARIANT: a stopped/stopping trajectory next to a
    /// pedestrian is admitted — the gate must never make `safe_stop`
    /// inadmissible (that would deadlock the doer↔checker loop).
    #[test]
    fn safe_stop_next_to_pedestrian_is_admitted() {
        let stop = [pt(0.0, 0.0, 0.0), pt(0.0, 0.0, 1.0)];
        assert!(!pedestrian_breach(&stop, &scene(&[ped(0.5, 0.5)]), BRAKE));
    }

    /// Omnidirectionality: a kerbside pedestrian OUTSIDE the vehicle-RSS
    /// lateral band still binds — they can step in. (The vehicle-object RSS
    /// would lateral-filter this away; the VRU bound must not.)
    #[test]
    fn kerbside_pedestrian_outside_lateral_band_still_binds() {
        let traj = [pt(0.0, 4.0, 0.0), pt(4.0, 4.0, 1.0)];
        // 2.5 m lateral, 4 m ahead: inside required (~8.9 m at v=4).
        assert!(pedestrian_breach(&traj, &scene(&[ped(4.0, 2.5)]), BRAKE));
    }

    #[test]
    fn non_finite_pedestrian_breaches() {
        let traj = [pt(0.0, 2.0, 0.0), pt(2.0, 2.0, 1.0)];
        assert!(pedestrian_breach(&traj, &scene(&[ped(f64::NAN, 0.0)]), BRAKE));
    }

    #[test]
    fn empty_scene_is_noop() {
        let traj = [pt(0.0, 10.0, 0.0), pt(10.0, 10.0, 1.0)];
        assert!(!pedestrian_breach(&traj, &scene(&[]), BRAKE));
    }

    /// Fail-closed on an unbrakeable ego: zero/negative/non-finite brake →
    /// infinite requirement → any moving pose near any pedestrian breaches.
    #[test]
    fn non_positive_brake_fails_closed() {
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, 0.0, &VruRssParams::default()),
            f64::INFINITY
        );
        assert_eq!(
            required_pedestrian_clearance_m(1.0, 0.0, f64::NAN, &VruRssParams::default()),
            f64::INFINITY
        );
    }

    /// Monotonicity (the safety shape): the requirement never DECREASES
    /// with ego speed or pose time — faster/later can only demand more room.
    #[test]
    fn requirement_is_monotone_in_speed_and_time() {
        let p = VruRssParams::default();
        let mut prev = 0.0;
        for i in 0..50 {
            let v = f64::from(i) * 0.5;
            let r = required_pedestrian_clearance_m(v, 0.0, BRAKE, &p);
            assert!(r >= prev, "requirement must not decrease with speed");
            prev = r;
        }
        let early = required_pedestrian_clearance_m(3.0, 0.5, BRAKE, &p);
        let late = required_pedestrian_clearance_m(3.0, 4.0, BRAKE, &p);
        assert!(late > early, "a later pose faces a larger reachable disc");
    }
}
