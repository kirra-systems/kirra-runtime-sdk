//! **Multi-modal predictive-RSS mode producer** — roll each live perceived object forward into
//! one or more `PredictedMode` hypotheses, so the checker's multi-modal predictive RSS pass
//! (`predictive_rss_breach`, gap #3) runs against REAL perception instead of staying dormant
//! (the slow loop previously passed `predicted_modes = None`, leaving the snapshot RSS as the
//! sole bound).
//!
//! # Why this matters (what the snapshot pass misses)
//!
//! The snapshot RSS extrapolates an object from its instantaneous velocity but evaluates it at
//! its CURRENT position; an object that is laterally CLEAR now is filtered out (§4 lateral
//! alignment) even if its motion carries it INTO the ego's path. Rolling the position forward in
//! TIME and checking each step against the time-matched ego pose catches that cut-in.
//!
//! # The modes
//!
//! - **Constant-velocity (CV)** — always emitted, from the object's reported velocity vector.
//!   Catches the straight-line cut-in above.
//! - **Constant-turn-rate (CTRV)** — emitted when a per-object turn rate is known (the tracker's
//!   yaw estimate, supplied via `yaw_rates`). A genuinely DISTINCT hypothesis: a turning object's
//!   CV mode may stay clear while its CTRV mode curves into the path. The checker worst-cases
//!   over every mode, so one dangerous hypothesis is enough to refuse — the point of *multi-modal*
//!   prediction. A negligible turn rate adds no CTRV mode (it would duplicate CV).

use smallvec::SmallVec;

use crate::corridor::Point;
use crate::state::PerceivedObject;
use crate::validation::{PredictedMode, PredictedSample};

/// A predicted-sample run (§8). Inline up to 8 samples — a 3 s / 0.5 s horizon is
/// 7, so the common case never heap-allocates; spills to the heap beyond that.
type SampleVec = SmallVec<[PredictedSample; 8]>;

/// Turn rate (rad/s) below which CTRV ≈ CV — no distinct CTRV mode is emitted (it would just
/// duplicate the CV hypothesis and the worst-case is unchanged).
pub const CTRV_YAW_EPS_RAD_S: f64 = 0.02;

/// Plausibility ceiling on a turning object's lateral acceleration (m/s² ≈ 1 g). A CTRV
/// hypothesis implies a lateral accel of `speed·|yaw|` (= v²/R); a value beyond this is
/// PHYSICALLY IMPOSSIBLE for a road vehicle (tyre grip ≈ 1 g), so such a yaw is tracker NOISE,
/// not a real turn. Emitting its (sharp-curve) CTRV mode would only manufacture a spurious
/// breach → spurious MRC, so the yaw is rejected and the object degrades to CV-only. A genuine
/// sharp turn happens at LOW speed (small `speed·yaw`) and is never rejected.
pub const CTRV_MAX_LATERAL_ACCEL_MPS2: f64 = 10.0;

/// An owned predicted mode (owns its samples, unlike the borrowed [`PredictedMode`] the checker
/// consumes). Build the borrowed view with [`as_mode`](Self::as_mode) at the call site.
#[derive(Debug, Clone)]
pub struct OwnedPredictedMode {
    pub object_id: u64,
    pub samples: SampleVec,
}

impl OwnedPredictedMode {
    /// Borrow this owned mode as a [`PredictedMode`] for the checker.
    #[must_use]
    pub fn as_mode(&self) -> PredictedMode<'_> {
        PredictedMode { object_id: self.object_id, samples: &self.samples }
    }
}

/// Number of sample steps over `[0, horizon_s]` at `dt_s` (≥ 1).
fn step_count(horizon_s: f64, dt_s: f64) -> usize {
    (horizon_s.max(0.0) / dt_s).ceil().max(1.0) as usize
}

/// Roll `obj` forward on CONSTANT VELOCITY (its reported velocity vector) — a straight-line
/// hypothesis sampled at `dt_s` over `[0, horizon_s]`.
fn cv_samples(obj: &PerceivedObject, horizon_s: f64, dt_s: f64) -> SampleVec {
    let n = step_count(horizon_s, dt_s);
    (0..=n)
        .map(|i| {
            let t = i as f64 * dt_s;
            PredictedSample {
                pos: Point { x_m: obj.pos.x_m + obj.vel.x_m * t, y_m: obj.pos.y_m + obj.vel.y_m * t },
                time_from_start_s: t,
            }
        })
        .collect()
}

/// Roll `obj` forward on CONSTANT TURN RATE: travel at its current speed while the heading turns
/// at `yaw_rate_rad_s`. The divergent hypothesis for a turning object (curves where CV goes
/// straight). Euler-integrated at `dt_s`.
fn ctrv_samples(obj: &PerceivedObject, yaw_rate_rad_s: f64, horizon_s: f64, dt_s: f64) -> SampleVec {
    let n = step_count(horizon_s, dt_s);
    let speed = obj.velocity_mps;
    let mut heading = obj.vel.y_m.atan2(obj.vel.x_m);
    let (mut x, mut y) = (obj.pos.x_m, obj.pos.y_m);
    let mut out = SampleVec::with_capacity(n + 1);
    out.push(PredictedSample { pos: Point { x_m: x, y_m: y }, time_from_start_s: 0.0 });
    for i in 1..=n {
        heading += yaw_rate_rad_s * dt_s;
        x += speed * heading.cos() * dt_s;
        y += speed * heading.sin() * dt_s;
        out.push(PredictedSample { pos: Point { x_m: x, y_m: y }, time_from_start_s: i as f64 * dt_s });
    }
    out
}

/// Object speed (m/s) below which no lane-follow mode is emitted — a (near-)stationary object's
/// lane-follow mode degenerates to its CV mode.
const LANE_FOLLOW_MIN_SPEED_MPS: f64 = 0.1;

/// Position after travelling `dist_m` along a polyline (clamped to its end). The arc-length
/// walker the lane-follow rollout samples — mirrors the planner's `point_on_path`.
fn point_on_polyline(poly: &[Point], dist_m: f64) -> Point {
    match poly.first() {
        None => Point { x_m: 0.0, y_m: 0.0 },
        Some(first) if dist_m <= 0.0 || poly.len() == 1 => *first,
        Some(_) => {
            let mut acc = 0.0;
            for w in poly.windows(2) {
                let seg = (w[1].x_m - w[0].x_m).hypot(w[1].y_m - w[0].y_m);
                if acc + seg >= dist_m {
                    let f = if seg > 1e-9 { (dist_m - acc) / seg } else { 0.0 };
                    return Point {
                        x_m: w[0].x_m + f * (w[1].x_m - w[0].x_m),
                        y_m: w[0].y_m + f * (w[1].y_m - w[0].y_m),
                    };
                }
                acc += seg;
            }
            *poly.last().unwrap()
        }
    }
}

/// Roll an object along a geometric **lane-follow** `path` at `speed_mps` — the map-intention
/// hypothesis: position after travelling `speed*t` along the path. A vehicle on a CURVING lane
/// traces the curve (which CV/CTRV cannot), and one on a diverging lane stays clear.
fn lane_follow_samples(path: &[Point], speed_mps: f64, horizon_s: f64, dt_s: f64) -> SampleVec {
    let n = step_count(horizon_s, dt_s);
    (0..=n)
        .map(|i| {
            let t = i as f64 * dt_s;
            PredictedSample { pos: point_on_polyline(path, speed_mps * t), time_from_start_s: t }
        })
        .collect()
}

/// Produce the multi-modal predicted-mode set for `objects` over `[0, horizon_s]` sampled at
/// `dt_s`. Per object: a **CV** mode (always); a **CTRV** mode for any object whose turn rate
/// (looked up in `yaw_rates` as `(object_id, yaw_rate_rad_s)`) exceeds [`CTRV_YAW_EPS_RAD_S`]; and
/// a **lane-follow** mode for any moving object with a geometric path in `lane_paths` (the
/// map-intention hypothesis — `(object_id, &[Point])`, supplied from the lane map). Pass either
/// extra slice empty to omit that mode; CV alone is the kinematic snapshot bound.
///
/// Returns owned modes; borrow them as `&[PredictedMode]` via [`OwnedPredictedMode::as_mode`] for
/// `validate_trajectory_slow_capped`. A non-positive `dt_s` is floored to a small step.
// SAFETY: SG1 | REQ: multi-modal-predicted-mode-producer | TEST: cv_mode_rolls_position_forward_on_velocity,a_turn_rate_adds_a_distinct_ctrv_mode,slow_loop_modes_adds_ctrv_only_when_the_yaw_feed_is_fresh,a_lane_path_adds_a_distinct_lane_follow_mode_that_traces_the_curve,produced_cv_mode_catches_a_cut_in_the_snapshot_rss_misses,produced_ctrv_mode_catches_a_turn_in_that_cv_misses_multimodal_payoff,produced_lane_follow_mode_catches_a_curving_in_object_that_cv_misses
#[must_use]
pub fn predicted_modes_from_objects(
    objects: &[PerceivedObject],
    yaw_rates: &[(u64, f64)],
    lane_paths: &[(u64, &[Point])],
    horizon_s: f64,
    dt_s: f64,
) -> Vec<OwnedPredictedMode> {
    let dt = dt_s.max(1e-3);
    let mut modes = Vec::with_capacity(objects.len());
    for obj in objects {
        modes.push(OwnedPredictedMode { object_id: obj.id, samples: cv_samples(obj, horizon_s, dt) });
        if let Some(&(_, yaw)) = yaw_rates.iter().find(|(id, _)| *id == obj.id) {
            // A distinct CTRV mode only when the yaw is BOTH non-negligible AND
            // CONSISTENT with the object's speed: `speed·|yaw|` (the implied
            // lateral accel) within a physical cornering ceiling. A yaw beyond it
            // is tracker noise — its sharp-curve CTRV would manufacture a spurious
            // breach — so the object degrades to CV-only (A2).
            let implied_lateral_accel = obj.velocity_mps * yaw.abs();
            if yaw.abs() > CTRV_YAW_EPS_RAD_S
                && implied_lateral_accel <= CTRV_MAX_LATERAL_ACCEL_MPS2
            {
                modes.push(OwnedPredictedMode { object_id: obj.id, samples: ctrv_samples(obj, yaw, horizon_s, dt) });
            }
        }
        if let Some(&(_, path)) = lane_paths.iter().find(|(id, _)| *id == obj.id) {
            if path.len() >= 2 && obj.velocity_mps > LANE_FOLLOW_MIN_SPEED_MPS {
                modes.push(OwnedPredictedMode { object_id: obj.id, samples: lane_follow_samples(path, obj.velocity_mps, horizon_s, dt) });
            }
        }
    }
    modes
}

/// Build the slow-loop predicted modes from live `objects` plus the tracker's per-object
/// `yaw_rates`, **gated on the yaw feed's freshness**. A FRESH yaw map adds the CTRV turn-in
/// hypothesis (genuinely multi-modal); a STALE / unconfigured one degrades to CV-only — a stale
/// estimate would keep predicting a turn-in after the object straightened, so the enhancement is
/// dropped rather than trusted. Dropping it is NOT a fault (unlike a lost redundancy channel):
/// the CV mode (and the snapshot RSS) still bound the object.
#[must_use]
pub fn slow_loop_modes(
    objects: &[PerceivedObject],
    yaw_rates: &[(u64, f64)],
    yaw_fresh: bool,
    horizon_s: f64,
    dt_s: f64,
) -> Vec<OwnedPredictedMode> {
    let yaw: &[(u64, f64)] = if yaw_fresh { yaw_rates } else { &[] };
    // No lane map in the slow loop yet (the adapter holds a CorridorSource, not a lane graph), so
    // no map-intention lane-follow modes here — CV/CTRV only. An integrator/map-side caller adds
    // them by calling `predicted_modes_from_objects` with the per-object lane paths.
    predicted_modes_from_objects(objects, yaw, &[], horizon_s, dt_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(id: u64, x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
        PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: vx.hypot(vy),
            heading_rad: vy.atan2(vx),
            vel: Point { x_m: vx, y_m: vy },
        }
    }

    #[test]
    fn cv_mode_rolls_position_forward_on_velocity() {
        let o = obj(1, 10.0, 0.0, 2.0, -1.0);
        let modes = predicted_modes_from_objects(&[o], &[], &[], 2.0, 1.0);
        assert_eq!(modes.len(), 1, "no yaw rate → CV mode only");
        let s = &modes[0].samples;
        // t=0 at the snapshot, t=1 advanced by (2,-1), t=2 by (4,-2).
        assert!((s[0].pos.x_m - 10.0).abs() < 1e-9 && (s[0].pos.y_m - 0.0).abs() < 1e-9);
        assert!((s[1].pos.x_m - 12.0).abs() < 1e-9 && (s[1].pos.y_m + 1.0).abs() < 1e-9);
        assert!((s[2].pos.x_m - 14.0).abs() < 1e-9 && (s[2].pos.y_m + 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_turn_rate_adds_a_distinct_ctrv_mode() {
        let o = obj(1, 10.0, 0.0, 3.0, 0.0); // moving +x
        let modes = predicted_modes_from_objects(&[o], &[(1, 0.4)], &[], 2.0, 0.5);
        assert_eq!(modes.len(), 2, "CV + CTRV for a turning object");
        // The CTRV mode curves (gains lateral y) where CV stays on the axis.
        let cv = &modes[0].samples;
        let ctrv = &modes[1].samples;
        assert!(cv.last().unwrap().pos.y_m.abs() < 1e-9, "CV stays on the x-axis");
        assert!(ctrv.last().unwrap().pos.y_m > 0.5, "CTRV curves off-axis (+y), got {}", ctrv.last().unwrap().pos.y_m);
    }

    #[test]
    fn a_negligible_turn_rate_adds_no_redundant_mode() {
        let o = obj(1, 10.0, 0.0, 3.0, 0.0);
        let modes = predicted_modes_from_objects(&[o], &[(1, 0.005)], &[], 2.0, 0.5);
        assert_eq!(modes.len(), 1, "sub-epsilon yaw → no duplicate CTRV mode");
    }

    #[test]
    fn an_implausible_yaw_for_the_speed_is_rejected_as_noise() {
        // 20 m/s with a 1.0 rad/s turn → implied lateral accel 20 m/s² (>1 g):
        // physically impossible for a road vehicle, so the yaw is tracker noise
        // and its CTRV mode is NOT emitted (CV-only) — no spurious sharp-curve
        // breach (A2).
        let fast = obj(1, 10.0, 0.0, 20.0, 0.0);
        let modes = predicted_modes_from_objects(&[fast], &[(1, 1.0)], &[], 2.0, 0.5);
        assert_eq!(modes.len(), 1, "implausible speed·yaw → CV-only, CTRV rejected");

        // The SAME turn rate at a LOW speed (2 m/s → 2 m/s² lateral) is a genuine
        // tight turn and IS emitted — the check filters noise, not real maneuvers.
        let slow = obj(2, 10.0, 0.0, 2.0, 0.0);
        let modes = predicted_modes_from_objects(&[slow], &[(2, 1.0)], &[], 2.0, 0.5);
        assert_eq!(modes.len(), 2, "plausible low-speed sharp turn → CV + CTRV");
    }

    #[test]
    fn slow_loop_modes_adds_ctrv_only_when_the_yaw_feed_is_fresh() {
        let o = obj(1, 10.0, 0.0, 3.0, 0.0);
        let yaw = [(1u64, 0.4)];
        // Fresh yaw → CV + CTRV (genuinely multi-modal).
        let fresh = slow_loop_modes(&[o], &yaw, true, 2.0, 0.5);
        assert_eq!(fresh.len(), 2, "fresh yaw adds the CTRV hypothesis");
        // Stale yaw → CV only (the turn estimate is dropped, not trusted) — never a fault.
        let stale = slow_loop_modes(&[o], &yaw, false, 2.0, 0.5);
        assert_eq!(stale.len(), 1, "stale yaw degrades to CV-only");
        // No yaw configured behaves like stale.
        let none = slow_loop_modes(&[o], &[], true, 2.0, 0.5);
        assert_eq!(none.len(), 1, "no yaw map → CV-only");
    }

    #[test]
    fn each_object_gets_its_own_modes() {
        let modes = predicted_modes_from_objects(&[obj(1, 0.0, 0.0, 1.0, 0.0), obj(2, 5.0, 0.0, 1.0, 0.0)], &[(2, 0.3)], &[], 1.0, 0.5);
        // obj1 CV only, obj2 CV+CTRV → 3 modes, ids preserved.
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0].object_id, 1);
        assert_eq!(modes[1].object_id, 2);
        assert_eq!(modes[2].object_id, 2);
    }

    #[test]
    fn a_lane_path_adds_a_distinct_lane_follow_mode_that_traces_the_curve() {
        // An object moving +x with a lane path that BENDS to +y. The lane-follow mode traces the
        // bend (gains +y) where the CV mode stays on the axis — a distinct hypothesis.
        let o = obj(1, 0.0, 0.0, 4.0, 0.0); // 4 m/s east
        let path = [
            Point { x_m: 0.0, y_m: 0.0 }, Point { x_m: 4.0, y_m: 0.0 },
            Point { x_m: 8.0, y_m: 4.0 }, Point { x_m: 8.0, y_m: 12.0 },
        ];
        let modes = predicted_modes_from_objects(&[o], &[], &[(1, &path[..])], 2.0, 0.5);
        assert_eq!(modes.len(), 2, "CV + lane-follow (no yaw → no CTRV)");
        assert!(modes[0].samples.last().unwrap().pos.y_m.abs() < 1e-9, "CV stays on the x-axis");
        assert!(modes[1].samples.last().unwrap().pos.y_m > 1.0, "lane-follow traces the bend into +y, got {}", modes[1].samples.last().unwrap().pos.y_m);
    }

    #[test]
    fn a_stationary_object_gets_no_lane_follow_mode() {
        let o = obj(1, 0.0, 0.0, 0.0, 0.0); // not moving
        let path = [Point { x_m: 0.0, y_m: 0.0 }, Point { x_m: 10.0, y_m: 0.0 }];
        let modes = predicted_modes_from_objects(&[o], &[], &[(1, &path[..])], 2.0, 0.5);
        assert_eq!(modes.len(), 1, "a stationary object's lane-follow degenerates to CV → omitted");
    }
}
