// parko/crates/parko-ros2/src/containment_gate.rs
//
// ADR-0029 Phase 3a — live SG2 containment seam for the parko-ros2 ML node.
//
// The node's tick is sensor → inference → twist: it emits ONE `(v, ω)`
// `ControlCommand` per tick, not a planned trajectory, and ingests only opaque
// inference tensors — no map, no odometry, no corridor. So the SDK adapter's
// map-anchored containment (Lanelet2 + localization) does not transplant here.
//
// The Phase 3 approach (ego-frame containment): a lidar-derived corridor is
// ALREADY ego-relative, so we check containment in the EGO frame with no global
// localization. This module is the PURE, testable core:
//
//   1. Project a short diff-drive (unicycle) lookahead of the proposed command
//      forward from the ego origin `(0,0,0)` — "where this command takes us over
//      the next horizon if held".
//   2. Check that lookahead's footprint against an ego-relative `Corridor` via
//      the generic SG2 seam `validate_trajectory_containment` (footprint-driven,
//      drive-agnostic — the S-PK1c thesis).
//
// Frame trust is `Trusted`: an ego-relative (perception-derived) corridor carries
// NO localization lateral-error term, so the baseline 0.40 m containment margin
// (KIRRA-OCCY-SG2-MARGIN-001) applies directly; corridor HEALTH is governed by
// the `Corridor`'s own confidence/age gate, not by localization.
//
// Fail-closed: a non-finite command, or an absent/stale/unhealthy corridor,
// yields a `DenyBreach` → the tick MRCs (stopped twist). Opt-in: the tick only
// runs this when a corridor snapshot is supplied (Phase 3b wires the live
// lidar→`kirra-taj` source; until then the production node supplies none and the
// path is byte-identical).

use kirra_core::containment::{validate_trajectory_containment, Corridor, Pose, VehicleFootprint};
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::kinematics_contract::{DenyCode, EnforceAction};
use parko_core::commands::ControlCommand;

use crate::command_mapping::OutgoingTwist;
use crate::tick_pipeline::{TickError, TickOutcome};

/// Lookahead horizon (s) — how far ahead the proposed command is projected for
/// the containment check. ~0.5 s of motion at the courier's 1.5 m/s is ~0.75 m,
/// enough to catch a maneuver heading out of the corridor a single next-pose
/// check would miss.
pub const CONTAINMENT_HORIZON_S: f64 = 0.5;

/// Integration step (s) for the lookahead projection. `HORIZON / STEP` poses
/// (plus the origin) — kept well under `MAX_TRAJECTORY_HORIZON = 50`.
pub const CONTAINMENT_STEP_S: f64 = 0.1;

/// Project the proposed `(v, ω)` command forward from the ego origin under the
/// diff-drive unicycle model (`ẋ = v·cosθ`, `ẏ = v·sinθ`, `θ̇ = ω`), forward-Euler
/// at `step_s`, for `horizon_s`. Returns the ego-frame pose sequence starting at
/// `(0, 0, 0)`. The command is assumed HELD over the horizon (worst-case for a
/// turning command — it does not re-plan mid-horizon).
fn project_lookahead(command: &ControlCommand, horizon_s: f64, step_s: f64) -> Vec<Pose> {
    let v = command.linear_velocity;
    let w = command.angular_velocity;
    let steps = if step_s > 0.0 && horizon_s > 0.0 {
        (horizon_s / step_s).ceil() as usize
    } else {
        0
    };
    // Always at least the origin pose; cap defensively (containment caps at 50).
    let steps = steps.min(48);
    let mut poses = Vec::with_capacity(steps + 1);
    let (mut x, mut y, mut th) = (0.0_f64, 0.0_f64, 0.0_f64);
    poses.push(Pose { x_m: x, y_m: y, heading_rad: th });
    for _ in 0..steps {
        x += v * th.cos() * step_s;
        y += v * th.sin() * step_s;
        th += w * step_s;
        poses.push(Pose { x_m: x, y_m: y, heading_rad: th });
    }
    poses
}

/// Phase 3a SG2 gate: does the proposed command's lookahead stay inside the
/// ego-relative corridor? `Allow` → publish the command; any `DenyBreach` → the
/// caller MRCs. Fail-closed on a non-finite command (the projection would be NaN;
/// reject before geometry). `footprint` is the platform's footprint (e.g.
/// `CourierPlatformProfile::footprint()`), keeping the check drive-agnostic.
///
// SAFETY: SG2 | REQ: parko-ros2-egoframe-containment-gate | TEST: straight_command_inside_corridor_is_allowed,turning_command_out_of_corridor_is_denied,stale_corridor_fails_closed,nonfinite_command_fails_closed,stationary_command_inside_is_allowed,gate_matches_platform_containment_seam
pub fn command_stays_in_corridor(
    footprint: &VehicleFootprint,
    command: &ControlCommand,
    corridor: &Corridor,
    horizon_s: f64,
    step_s: f64,
) -> EnforceAction {
    if !command.linear_velocity.is_finite() || !command.angular_velocity.is_finite() {
        // Fail-closed: a non-finite command cannot be shown contained.
        return EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture);
    }
    let lookahead = project_lookahead(command, horizon_s, step_s);
    // Ego-frame: no localization term → Trusted (corridor health gates itself).
    validate_trajectory_containment(&lookahead, corridor, footprint, FrameTrust::Trusted)
}

/// Convenience over [`command_stays_in_corridor`] with the module-default
/// horizon/step.
pub fn command_stays_in_corridor_default(
    footprint: &VehicleFootprint,
    command: &ControlCommand,
    corridor: &Corridor,
) -> EnforceAction {
    command_stays_in_corridor(
        footprint,
        command,
        corridor,
        CONTAINMENT_HORIZON_S,
        CONTAINMENT_STEP_S,
    )
}

/// Compose the SG2 containment gate ONTO a [`TickOutcome`] — the node-side seam
/// (ADR-0029 Phase 3a). Called after [`run_pipeline_tick`](crate::tick_pipeline::run_pipeline_tick)
/// when a corridor snapshot is available; with no snapshot the node skips this
/// and the path is byte-identical (opt-in).
///
/// The governed command is carried by the outcome's `OutgoingTwist`
/// (`linear_x_mps` / `angular_z_rads`), so the gate checks exactly what would be
/// published. An already-stopped twist (the staleness / inference / LockedOut
/// MRC, or a genuine zero command) is passed through — a stop is always
/// contained. A containment breach replaces the twist with `stopped` and tags
/// [`TickError::ContainmentBreach`].
///
// SAFETY: SG2 | REQ: parko-ros2-egoframe-containment-gate | TEST: gate_passes_an_in_corridor_outcome,gate_mrcs_an_out_of_corridor_outcome,gate_passes_through_an_already_stopped_outcome
pub fn apply_containment_gate(
    outcome: TickOutcome,
    footprint: &VehicleFootprint,
    corridor: &Corridor,
    horizon_s: f64,
    step_s: f64,
) -> TickOutcome {
    // An already-stopped twist needs no gating (a stop is contained); this also
    // preserves any upstream MRC + its TickError.
    if outcome.twist.linear_x_mps == 0.0 && outcome.twist.angular_z_rads == 0.0 {
        return outcome;
    }
    let command = ControlCommand {
        linear_velocity: outcome.twist.linear_x_mps,
        angular_velocity: outcome.twist.angular_z_rads,
        timestamp_ms: outcome.twist.stamp_ms,
    };
    match command_stays_in_corridor(footprint, &command, corridor, horizon_s, step_s) {
        EnforceAction::Allow => outcome,
        _ => TickOutcome {
            twist: OutgoingTwist::stopped(outcome.twist.stamp_ms),
            error: Some(TickError::ContainmentBreach),
            degraded: outcome.degraded,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::containment::Point;

    /// An ego-relative straight corridor along +x of the given half-width,
    /// spanning x ∈ [-2, span_m]. Healthy (fresh, confident).
    fn corridor_points(half_width_m: f64, span_m: f64) -> (Vec<Point>, Vec<Point>) {
        let n = 12;
        let x0 = -2.0;
        let dx = (span_m - x0) / (n as f64 - 1.0);
        let left = (0..n)
            .map(|i| Point { x_m: x0 + i as f64 * dx, y_m: half_width_m })
            .collect();
        let right = (0..n)
            .map(|i| Point { x_m: x0 + i as f64 * dx, y_m: -half_width_m })
            .collect();
        (left, right)
    }

    fn healthy_corridor<'a>(left: &'a [Point], right: &'a [Point]) -> Corridor<'a> {
        Corridor { left, right, confidence: 0.95, age_ms: 10, min_confidence: 0.5, max_age_ms: 500 }
    }

    fn courier_footprint() -> VehicleFootprint {
        crate::platform_profile::CourierPlatformProfile::courier_reference().footprint()
    }

    fn cmd(v: f64, w: f64) -> ControlCommand {
        ControlCommand { linear_velocity: v, angular_velocity: w, timestamp_ms: 0 }
    }

    #[test]
    fn straight_command_inside_corridor_is_allowed() {
        // Drive straight up a 1 m half-width corridor: footprint half-width 0.3 +
        // 0.40 margin = 0.70 < 1.0 → contained for the whole lookahead.
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.0, 0.0), &corridor);
        assert!(matches!(verdict, EnforceAction::Allow),
            "a straight in-corridor command must be allowed; got {verdict:?}");
    }

    #[test]
    fn turning_command_out_of_corridor_is_denied() {
        // A hard yaw curves the lookahead (and swings the rotated footprint)
        // out of the 1 m half-width corridor within the horizon.
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.5, 3.0), &corridor);
        assert!(matches!(verdict, EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)),
            "a command curving out of the corridor must DenyBreach; got {verdict:?}");
    }

    #[test]
    fn stale_corridor_fails_closed() {
        // age_ms (1000) > max_age_ms (500) → unhealthy corridor → DenyBreach,
        // regardless of the (benign, straight) command.
        let (l, r) = corridor_points(1.0, 5.0);
        let stale = Corridor { left: &l, right: &r, confidence: 0.95, age_ms: 1000, min_confidence: 0.5, max_age_ms: 500 };
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(1.0, 0.0), &stale);
        assert!(matches!(verdict, EnforceAction::DenyBreach(_)),
            "a stale corridor must fail closed; got {verdict:?}");
    }

    #[test]
    fn nonfinite_command_fails_closed() {
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        for c in [cmd(f64::NAN, 0.0), cmd(1.0, f64::INFINITY)] {
            let verdict = command_stays_in_corridor_default(&courier_footprint(), &c, &corridor);
            assert!(matches!(verdict, EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)),
                "a non-finite command must fail closed; got {verdict:?}");
        }
    }

    #[test]
    fn stationary_command_inside_is_allowed() {
        // v=0, ω=0 — the lookahead is the stationary footprint at the origin,
        // centered in the corridor → allowed.
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let verdict = command_stays_in_corridor_default(&courier_footprint(), &cmd(0.0, 0.0), &corridor);
        assert!(matches!(verdict, EnforceAction::Allow),
            "a stationary in-corridor command must be allowed; got {verdict:?}");
    }

    #[test]
    fn gate_matches_platform_containment_seam() {
        // The footprint-level gate must agree with the platform-level
        // `validate_platform_containment` (S-PK1c) on the same lookahead — the
        // gate is that seam, just fed the projected poses.
        use kirra_core::platform_kinematics::{validate_platform_containment, PlatformKinematics};
        let platform = crate::platform_profile::CourierPlatformProfile::courier_reference().platform();
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let command = cmd(1.5, 3.0);
        let lookahead = project_lookahead(&command, CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S);

        let via_platform = validate_platform_containment(&platform, &lookahead, &corridor, FrameTrust::Trusted);
        let via_gate = command_stays_in_corridor_default(&platform.footprint(), &command, &corridor);
        assert_eq!(
            format!("{via_platform:?}"), format!("{via_gate:?}"),
            "the gate must equal the platform containment seam on the same lookahead"
        );
    }

    // ---- TickOutcome composition (the node-side seam) ----------------------

    fn outcome(linear: f64, angular: f64) -> TickOutcome {
        TickOutcome {
            twist: OutgoingTwist { linear_x_mps: linear, angular_z_rads: angular, stamp_ms: 7 },
            error: None,
            degraded: false,
        }
    }

    #[test]
    fn gate_passes_an_in_corridor_outcome() {
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let out = apply_containment_gate(
            outcome(1.0, 0.0), &courier_footprint(), &corridor, CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S);
        assert_eq!(out.twist.linear_x_mps, 1.0, "an in-corridor command must pass through");
        assert!(out.error.is_none());
    }

    #[test]
    fn gate_mrcs_an_out_of_corridor_outcome() {
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let out = apply_containment_gate(
            outcome(1.5, 3.0), &courier_footprint(), &corridor, CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S);
        assert_eq!(out.twist, OutgoingTwist::stopped(7),
            "an out-of-corridor command must be MRC'd to a stop");
        assert_eq!(out.error, Some(TickError::ContainmentBreach));
    }

    #[test]
    fn gate_passes_through_an_already_stopped_outcome() {
        // A prior MRC (e.g. stale sensor) is already stopped — the gate must not
        // run containment on it (a stop is contained) and must preserve the
        // upstream error.
        let (l, r) = corridor_points(1.0, 5.0);
        let corridor = healthy_corridor(&l, &r);
        let prior = TickOutcome {
            twist: OutgoingTwist::stopped(7),
            error: Some(TickError::InferenceError("upstream".into())),
            degraded: true,
        };
        let out = apply_containment_gate(
            prior.clone(), &courier_footprint(), &corridor, CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S);
        assert_eq!(out, prior, "an already-stopped outcome must pass through unchanged");
    }
}
