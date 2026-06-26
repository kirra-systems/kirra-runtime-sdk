// parko/crates/parko-ros2/src/taj_objects.rs
//
// ADR-0029 Phase 3b (object axis) — feed Taj's perceived OBJECTS into the
// parko-side RSS object-avoidance check, closing the one axis where the parko
// checker lagged the SDK adapter (which already runs full RSS + predictive RSS
// + occlusion). Phase 3b's corridor work (`taj_corridor` + `containment_gate`)
// consumed `TajPerception.corridor` but DROPPED `TajPerception.objects`; this
// module is the seam that stops dropping them.
//
// The thesis is unchanged: the planner/inference loop PROPOSES a `(v, ω)`; the
// trusted checker BOUNDS it. Here the bound is RSS: the proposed command's ego
// speed is checked against each perceived object via parko-kirra's vetted
// `compute_scene_rss` (the §4 longitudinal∧lateral conjunction — the SAME
// evaluator the comparator already trusts). An unsafe verdict MRCs the tick.
//
// Frame: Taj is ego-relative (angles from ego +X forward, +Y left) and objects
// carry ego-frame positions + an estimated ground-velocity vector — exactly the
// frame RSS wants, so no global localization is needed (mirrors the corridor
// gate's `FrameTrust::Trusted` rationale).
//
// This is the PURE, sim-testable core (object slice → `AgentScene` → verdict →
// gated `TickOutcome`). The ros2-gated half (storing the latest objects off the
// lidar task + applying the gate per tick) lives in `node.rs`, exactly like the
// corridor gate.
//
// Fail-closed throughout: no/stale perception → `AgentScene::Absent` → UNSAFE →
// MRC; a scene larger than `compute_scene_rss`'s WCET cap → UNSAFE → MRC; a
// non-finite object field cannot read as "safe" (the evaluator's NaN-safe
// branches). Opt-in: armed only when `object_rss_enabled` + lidar + footprint
// are configured, so an un-opted deployment is byte-identical.

use kirra_core::trajectory::PerceivedObject;
use parko_core::{AgentScene, RssAgent, RssParams};

use crate::command_mapping::OutgoingTwist;
use crate::platform_profile::CourierPlatformProfile;
use crate::tick_pipeline::{TickError, TickOutcome};

/// Courier RSS reaction time (s) — the response phase before the courier's
/// guaranteed braking takes effect (sense → decide → actuate). Matches the
/// parko-core RSS reference; deployment-tunable, VALIDATION-PENDING.
pub const COURIER_RSS_REACTION_TIME_S: f64 = 0.5;

/// Courier worst-case longitudinal acceleration during the response phase
/// (m/s²) — the ego may still be accelerating before it reacts. Conservative
/// for a 1.5 m/s sidewalk courier.
pub const COURIER_RSS_ACCEL_MAX_MPS2: f64 = 1.0;

/// Assumed maximum LEAD/object deceleration (m/s²). A sidewalk object (a person
/// stopping, a cart hitting a kerb) can shed speed fast; a HIGHER value shortens
/// the lead's assumed stopping distance, which only ENLARGES the gap the ego
/// must keep — i.e. it is the conservative (fail-safe) direction.
pub const COURIER_RSS_LEAD_BRAKE_MAX_MPS2: f64 = 4.0;

/// Courier maximum lateral acceleration (m/s²) for the side-RSS swerve term.
pub const COURIER_RSS_LAT_ACCEL_MAX_MPS2: f64 = 1.0;

/// Longitudinal velocity (m/s, ego frame) below which an object is treated as
/// CLOSING head-on (its `vel.x` points back toward the ego). Such an object is
/// routed through the head-on RSS bound (`oncoming = true`), never the
/// same-direction lead primitive — which would discard the closing sign and
/// UNDER-estimate the required gap (the #408 Obs-3 hazard). A small epsilon so a
/// near-stationary object is not spuriously flagged oncoming.
pub const ONCOMING_CLOSING_EPS_MPS: f64 = 0.10;

/// The courier's [`RssParams`], built from the profile's guaranteed braking plus
/// the documented courier RSS constants. One source of truth for the object
/// gate's bound (mirrors `CourierPlatformProfile::platform()` for the kinematic
/// side). `brake_min` is the ego's GUARANTEED deceleration (the profile's
/// `max_brake_mps2`); a non-positive value would make the RSS primitives return
/// their large finite failsafe (→ UNSAFE), so the gate fails closed by itself.
#[must_use]
pub fn courier_rss_params(profile: &CourierPlatformProfile) -> RssParams {
    RssParams {
        reaction_time: COURIER_RSS_REACTION_TIME_S,
        accel_max: COURIER_RSS_ACCEL_MAX_MPS2,
        brake_min: profile.max_brake_mps2,
        brake_max: COURIER_RSS_LEAD_BRAKE_MAX_MPS2,
        lat_accel_max: COURIER_RSS_LAT_ACCEL_MAX_MPS2,
    }
}

/// An OWNED snapshot of Taj's perceived objects — copied out of a
/// [`TajPerception`](kirra_taj::TajPerception) so the node can hold the latest in
/// an `Arc<Mutex<Option<_>>>` and re-derive freshness per tick (mirrors
/// [`CorridorSnapshot`](crate::taj_corridor::CorridorSnapshot)).
///
/// `stamp_ms` is the perception's wall-clock stamp; the gate recomputes `age =
/// now - stamp` at CHECK time (not a frozen age), so a snapshot left stale in
/// the slot — the lidar stream died — ages past the budget and fails closed.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSnapshot {
    objects: Vec<PerceivedObject>,
    stamp_ms: u64,
}

impl ObjectSnapshot {
    /// Copy a perception frame's objects + stamp into an owned snapshot.
    #[must_use]
    pub fn from_objects(objects: &[PerceivedObject], stamp_ms: u64) -> Self {
        Self { objects: objects.to_vec(), stamp_ms }
    }

    /// Age (ms) of this snapshot at `now_ms` (saturating; a clock that went
    /// backwards reads as age 0, never a panic).
    #[must_use]
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.stamp_ms)
    }

    #[must_use]
    pub fn objects(&self) -> &[PerceivedObject] {
        &self.objects
    }
}

/// Map ONE ego-frame perceived object to an [`RssAgent`] for the proposed ego
/// speed. Conventions (Taj ego frame: +X forward, +Y left):
/// - longitudinal gap = `pos.x_m` (the forward distance), lead speed = `vel.x_m`;
/// - lateral separation = `|pos.y_m|`, object lateral speed = `vel.y_m`;
/// - the ego is a unicycle that does not strafe → `ego_lat_vel = 0`;
/// - an object closing back toward the ego (`vel.x_m < -ε`) is `oncoming` (the
///   head-on bound), else a same-direction lead.
fn object_to_agent(object: &PerceivedObject, ego_vel_mps: f64) -> RssAgent {
    RssAgent {
        ego_vel: ego_vel_mps,
        lead_vel: object.vel.x_m,
        actual_longitudinal_gap_m: object.pos.x_m,
        ego_lat_vel: 0.0,
        obj_lat_vel: object.vel.y_m,
        actual_lateral_separation_m: object.pos.y_m.abs(),
        oncoming: object.vel.x_m < -ONCOMING_CLOSING_EPS_MPS,
    }
}

/// Convert a FRESH object slice into an [`AgentScene`] for the proposed ego
/// speed. Only objects in the ego's travel half-plane (`pos.x_m >= 0`, ahead or
/// abreast) are checked — a forward-driving courier cannot collide forward with
/// something already behind it, so including rear objects would only spuriously
/// MRC on a tailgater.
///
/// A fresh perception that sees nothing ahead is `KnownEmpty` (RSS-safe), NOT
/// `Absent` — the safety-critical distinction `compute_scene_rss` rests on:
/// "perception ran and the way is clear" must not be conflated with "no
/// perception this tick". `Absent` is reserved for the no/stale-perception case,
/// applied by [`apply_object_rss_gate`].
#[must_use]
pub fn objects_to_agent_scene(objects: &[PerceivedObject], ego_vel_mps: f64) -> AgentScene {
    let agents: Vec<RssAgent> = objects
        .iter()
        .filter(|o| o.pos.x_m >= 0.0)
        .map(|o| object_to_agent(o, ego_vel_mps))
        .collect();
    if agents.is_empty() {
        // Perception ran; nothing ahead to conflict with → verified clear.
        AgentScene::KnownEmpty
    } else {
        AgentScene::Agents(agents)
    }
}

/// Compose the RSS object-avoidance gate ONTO a [`TickOutcome`] — the node-side
/// seam, parallel to [`apply_containment_gate`](crate::containment_gate::apply_containment_gate).
/// Called after the tick (and after the containment gate) when object perception
/// is armed; with no snapshot the node skips this and the path is byte-identical.
///
/// The proposed ego speed is the outcome's `OutgoingTwist::linear_x_mps` — the
/// command about to be published — so the RSS check bounds exactly what would
/// drive the actuator. An already-stopped twist passes through (a stop is the
/// MRC itself; there is nothing stronger to impose), preserving any upstream
/// error. Fail-closed inputs → MRC:
/// - `None` snapshot (no perception) → `AgentScene::Absent` → UNSAFE;
/// - a snapshot older than `max_age_ms` → `Absent` → UNSAFE;
/// - any object pair RSS-unsafe (the §4 conjunction) → UNSAFE.
/// An unsafe verdict replaces the twist with `stopped` and tags
/// [`TickError::ObjectRssBreach`].
///
// SAFETY: SG1 | REQ: parko-ros2-object-rss-gate | TEST: clear_scene_passes_the_command,object_ahead_in_path_mrcs,object_clear_to_the_side_passes,absent_perception_fails_closed,stale_snapshot_fails_closed,rear_object_is_ignored,already_stopped_outcome_passes_through,oncoming_object_takes_the_head_on_bound
pub fn apply_object_rss_gate(
    outcome: TickOutcome,
    objects: Option<&ObjectSnapshot>,
    params: &RssParams,
    max_age_ms: u64,
    now_ms: u64,
) -> TickOutcome {
    // An already-stopped twist needs no gating (a stop is contained); this also
    // preserves any upstream MRC + its TickError.
    if outcome.twist.linear_x_mps == 0.0 && outcome.twist.angular_z_rads == 0.0 {
        return outcome;
    }

    let ego_vel = outcome.twist.linear_x_mps;
    // Fail-closed on missing/stale perception: ABSENT, never KnownEmpty.
    let scene = match objects {
        Some(snap) if snap.age_ms(now_ms) <= max_age_ms => {
            objects_to_agent_scene(snap.objects(), ego_vel)
        }
        _ => AgentScene::Absent,
    };

    let state = parko_kirra::compute_scene_rss(&scene, params);
    if state.safe {
        outcome
    } else {
        TickOutcome {
            twist: OutgoingTwist::stopped(outcome.twist.stamp_ms),
            error: Some(TickError::ObjectRssBreach),
            degraded: outcome.degraded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::corridor::Point;

    fn courier() -> CourierPlatformProfile {
        CourierPlatformProfile::courier_reference()
    }

    fn params() -> RssParams {
        courier_rss_params(&courier())
    }

    /// A perceived object at `(x, y)` with ground-velocity `(vx, vy)` (ego frame).
    fn object(id: u64, x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
        PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: (vx * vx + vy * vy).sqrt(),
            heading_rad: 0.0,
            vel: Point { x_m: vx, y_m: vy },
        }
    }

    fn outcome(linear: f64) -> TickOutcome {
        TickOutcome {
            twist: OutgoingTwist { linear_x_mps: linear, angular_z_rads: 0.0, stamp_ms: 7 },
            error: None,
            degraded: false,
        }
    }

    fn snapshot(objects: &[PerceivedObject], stamp_ms: u64) -> ObjectSnapshot {
        ObjectSnapshot::from_objects(objects, stamp_ms)
    }

    // ---- scene conversion --------------------------------------------------

    #[test]
    fn fresh_empty_perception_is_known_empty_not_absent() {
        // The safety-critical distinction: perception ran, nothing ahead → clear.
        assert!(matches!(objects_to_agent_scene(&[], 1.0), AgentScene::KnownEmpty));
        assert!(matches!(parko_kirra::compute_scene_rss(
            &objects_to_agent_scene(&[], 1.0), &params()).safe, true));
    }

    #[test]
    fn rear_object_is_ignored() {
        // An object 2 m BEHIND the ego (x = -2) is out of the forward half-plane;
        // a forward-driving courier cannot forward-collide with it → KnownEmpty.
        let objs = [object(1, -2.0, 0.0, 0.0, 0.0)];
        assert!(matches!(objects_to_agent_scene(&objs, 1.0), AgentScene::KnownEmpty));
    }

    // ---- the gate ----------------------------------------------------------

    #[test]
    fn clear_scene_passes_the_command() {
        // Nothing ahead → the governed command passes through untouched.
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&[], 100)), &params(), 500, 100);
        assert_eq!(out.twist.linear_x_mps, 1.0, "a clear scene must pass the command");
        assert!(out.error.is_none());
    }

    #[test]
    fn object_ahead_in_path_mrcs() {
        // A static object 0.5 m dead ahead, ego at 1 m/s — far inside the RSS
        // longitudinal gap → unsafe → MRC.
        let objs = [object(1, 0.5, 0.0, 0.0, 0.0)];
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&objs, 100)), &params(), 500, 100);
        assert_eq!(out.twist, OutgoingTwist::stopped(7), "an object in the path must MRC");
        assert_eq!(out.error, Some(TickError::ObjectRssBreach));
    }

    #[test]
    fn object_clear_to_the_side_passes() {
        // A static object 3 m to the side (y = 3), abreast (x = 0): laterally far
        // beyond the footprint+overlap band → the §4 conjunction is not met → safe.
        let objs = [object(1, 0.0, 3.0, 0.0, 0.0)];
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&objs, 100)), &params(), 500, 100);
        assert_eq!(out.twist.linear_x_mps, 1.0, "a laterally-clear object must not MRC");
        assert!(out.error.is_none());
    }

    #[test]
    fn absent_perception_fails_closed() {
        // No snapshot at all → AgentScene::Absent → UNSAFE → MRC.
        let out = apply_object_rss_gate(outcome(1.0), None, &params(), 500, 100);
        assert_eq!(out.twist, OutgoingTwist::stopped(7), "no perception must fail closed");
        assert_eq!(out.error, Some(TickError::ObjectRssBreach));
    }

    #[test]
    fn stale_snapshot_fails_closed() {
        // A snapshot stamped at 100, checked at 2000 with a 500 ms budget → stale
        // → Absent → MRC, even though it holds a clear (empty) scene.
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&[], 100)), &params(), 500, 2000);
        assert_eq!(out.twist, OutgoingTwist::stopped(7), "a stale snapshot must fail closed");
        assert_eq!(out.error, Some(TickError::ObjectRssBreach));
    }

    #[test]
    fn already_stopped_outcome_passes_through() {
        // A prior MRC (already stopped) is passed through with its error intact —
        // the gate imposes nothing stronger than a stop, and never runs RSS on it.
        let prior = TickOutcome {
            twist: OutgoingTwist::stopped(7),
            error: Some(TickError::InferenceError("upstream".into())),
            degraded: true,
        };
        // Even with an object dead ahead AND no perception, a stop passes through.
        let out = apply_object_rss_gate(prior.clone(), None, &params(), 500, 100);
        assert_eq!(out, prior, "an already-stopped outcome must pass through unchanged");
    }

    #[test]
    fn oncoming_object_takes_the_head_on_bound() {
        // An object 5 m ahead CLOSING at 3 m/s (vel.x = -3) is oncoming; the
        // head-on bound (sum of both closing stopping distances) exceeds the 5 m
        // gap at the courier's speed → MRC. The same object treated as a
        // same-direction lead (the bug the oncoming flag prevents) would have a
        // far smaller required gap and could read as safe.
        let agent = object_to_agent(&object(1, 5.0, 0.0, -3.0, 0.0), 1.0);
        assert!(agent.oncoming, "a back-closing object must be flagged oncoming");
        let objs = [object(1, 5.0, 0.0, -3.0, 0.0)];
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&objs, 100)), &params(), 500, 100);
        assert_eq!(out.error, Some(TickError::ObjectRssBreach),
            "a close oncoming object must MRC under the head-on bound");
    }

    #[test]
    fn over_cap_scene_fails_closed() {
        // A scene larger than compute_scene_rss's WCET cap is fail-closed (MRC),
        // not truncated — many forward objects (e.g. a noisy scan) → stop.
        let objs: Vec<PerceivedObject> =
            (0..256).map(|i| object(i, 1.0 + i as f64 * 0.01, 5.0, 0.0, 0.0)).collect();
        let out = apply_object_rss_gate(outcome(1.0), Some(&snapshot(&objs, 100)), &params(), 500, 100);
        assert_eq!(out.error, Some(TickError::ObjectRssBreach),
            "an over-cap scene must fail closed");
    }
}
