//! Occy behavioral rules layer — traffic signs & signals (#90 / Occy 1.C).
//!
//! # Scope and doctrine
//!
//! This layer makes Occy **obey road rules**. It is deliberately separate from
//! KIRRA: KIRRA is the *physical* safety governor (collision / kinematic
//! envelope) and never enforces traffic law; this layer is *behavioral*
//! (legal/regulatory) and lives in the planner. Running a red light is not a
//! kinematic violation — it is a legal one — so it belongs here, while the
//! collision shadow of a red light (cross-traffic) stays with KIRRA's RSS.
//!
//! # Grounding in the rules of the road
//!
//! Every behaviorally-relevant control reduces to a small set of **longitudinal
//! constraints** — a *stop/hold line* and/or a *speed cap*. The variants below
//! cover the regulatory + warning sign families that impose those constraints
//! (US **MUTCD** R/W/S series; **Vienna Convention** priority / prohibitory /
//! special-regulation signs), plus signal indications per the **UVC / MUTCD
//! Part 4**, including:
//!
//! - **Stop sign / flashing red** — *full stop, then proceed* (MUTCD R1-1; §4).
//! - **Yield / flashing amber** — *give way: slow, prepared to stop* (R1-2).
//! - **Steady red** — *hold at the line until released*.
//! - **Steady amber** — the lawful **dilemma-zone** rule: *stop if you can do so
//!   safely; otherwise clear the intersection*. Modeled as "stop only if the
//!   required deceleration to the line is within a comfortable bound".
//! - **Speed limit / school zone / work zone / advisory** — *regulatory or
//!   advisory speed cap* (R2-1, S-series, W13-1).
//! - **Do-not-enter / wrong-way / road-closed** — *prohibition: no-go* (R5-1).
//! - **Rail / level crossing** — *stop when a train/gate requires it* (R15-1).
//!
//! # Out of longitudinal scope (stated honestly)
//!
//! Purely informational **guide** signs impose no constraint (no-op).
//! Direction/maneuver-specific rules — right-turn-on-red, protected/permitted
//! turn **arrows**, lane-use control — need *maneuver intent*, which this
//! longitudinal layer does not model; they are noted, not silently mishandled.
//! "Satisfied" / signal **state** are caller-managed (the integrator tracks the
//! full-stop dwell and the live signal phase), keeping this layer pure.

/// A traffic-signal indication (the longitudinally-relevant set). Protected /
/// permitted **turn arrows** are intentionally omitted — they require maneuver
/// intent, out of this layer's longitudinal scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalState {
    /// Proceed.
    Green,
    /// Steady amber — stop if able (dilemma-zone rule), else clear.
    Amber,
    /// Steady red — hold at the line.
    Red,
    /// Flashing red — treat as a STOP sign (full stop, then proceed).
    FlashingRed,
    /// Flashing amber — proceed with caution (yield-like; no mandatory stop).
    FlashingAmber,
}

/// A traffic control Occy must obey. Positions are longitudinal distances along
/// the path (ego-frame world x, metres). Each variant maps to a [`Behavioral`]
/// constraint via [`evaluate_controls`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrafficControl {
    /// STOP sign (MUTCD R1-1): full stop at the line, then proceed. `satisfied`
    /// is set by the integrator once the mandatory full stop has been performed.
    StopSign { stop_line_x_m: f64, satisfied: bool },
    /// YIELD / give-way (R1-2): slow to the yield speed, prepared to stop. The
    /// actual give-way to conflicting traffic is a physical object → KIRRA's RSS.
    YieldSign { line_x_m: f64 },
    /// A traffic signal at `stop_line_x_m` showing `state`.
    TrafficLight { stop_line_x_m: f64, state: SignalState },
    /// Regulatory speed limit (R2-1) taking effect at `from_x_m`.
    SpeedLimit { from_x_m: f64, limit_mps: f64 },
    /// School zone (S-series): `limit_mps` over `[from_x_m, to_x_m]` while `active`
    /// (flashing beacon / posted hours).
    SchoolZone { from_x_m: f64, to_x_m: f64, limit_mps: f64, active: bool },
    /// Work / construction zone: `limit_mps` over `[from_x_m, to_x_m]`.
    WorkZone { from_x_m: f64, to_x_m: f64, limit_mps: f64 },
    /// Advisory (warning) speed, e.g. a curve plate (W13-1): a speed cap from
    /// `from_x_m`. Advisory, but treated as a cap for safe, lawful driving.
    AdvisorySpeed { from_x_m: f64, limit_mps: f64 },
    /// Prohibition — DO NOT ENTER / wrong-way / road-closed (R5-1): no-go; the
    /// plan must stop before the line and not pass it.
    DoNotEnter { line_x_m: f64 },
    /// Rail / level crossing (R15-1): stop at the line when `must_stop` (a train
    /// is approaching, the gate is down, or the crossing signal is active).
    RailCrossing { stop_line_x_m: f64, must_stop: bool },
}

/// Tunables for the behavioral layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BehaviorConfig {
    /// Comfortable deceleration used for the amber dilemma-zone test: on amber,
    /// stop only if the required decel to the line is at/below this.
    pub amber_comfortable_decel_mps2: f64,
    /// Speed approaching a YIELD / flashing-amber control (slow, ready to stop).
    pub yield_speed_mps: f64,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self { amber_comfortable_decel_mps2: 3.0, yield_speed_mps: 4.0 }
    }
}

/// The behavioral constraint Occy must apply this cycle: the nearest mandatory
/// **stop/hold line** ahead (if any) and the binding **speed cap** (if any).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Behavioral {
    /// Nearest longitudinal x at which the plan must come to a stop/hold. `None`
    /// = no mandatory stop from traffic controls.
    pub stop_x_m: Option<f64>,
    /// Speed cap in effect over the planned region (regulatory / advisory /
    /// yield). `None` = no behavioral speed cap.
    pub speed_cap_mps: Option<f64>,
}

impl Behavioral {
    fn add_stop(&mut self, x: f64) {
        self.stop_x_m = Some(self.stop_x_m.map_or(x, |c| c.min(x)));
    }
    fn add_cap(&mut self, v: f64) {
        self.speed_cap_mps = Some(self.speed_cap_mps.map_or(v, |c| c.min(v)));
    }
}

/// Evaluate the active traffic controls into a single [`Behavioral`] constraint,
/// given the ego's longitudinal position `ego_x` and speed `ego_v`.
#[must_use]
pub fn evaluate_controls(
    controls: &[TrafficControl],
    ego_x: f64,
    ego_v: f64,
    cfg: &BehaviorConfig,
) -> Behavioral {
    let mut out = Behavioral::default();
    // A stop line only constrains us if it is AHEAD.
    let ahead = |x: f64| x > ego_x;
    // The lawful amber dilemma test: can we stop at the line within a comfortable
    // deceleration? If not, clearing is the safe (and legal) action.
    let can_stop_safely = |line_x: f64| {
        let d = line_x - ego_x;
        if d <= 1e-3 {
            return false; // already at/over the line — cannot stop before it
        }
        let required_decel = (ego_v * ego_v) / (2.0 * d);
        required_decel <= cfg.amber_comfortable_decel_mps2
    };

    for c in controls {
        match *c {
            TrafficControl::StopSign { stop_line_x_m, satisfied } => {
                if !satisfied && ahead(stop_line_x_m) {
                    out.add_stop(stop_line_x_m);
                }
            }
            TrafficControl::YieldSign { line_x_m } => {
                if ahead(line_x_m) {
                    out.add_cap(cfg.yield_speed_mps);
                }
            }
            TrafficControl::TrafficLight { stop_line_x_m, state } => match state {
                SignalState::Green => {}
                SignalState::Red | SignalState::FlashingRed => {
                    if ahead(stop_line_x_m) {
                        out.add_stop(stop_line_x_m);
                    }
                }
                SignalState::Amber => {
                    // Dilemma zone: stop only if it can be done safely; else clear.
                    if ahead(stop_line_x_m) && can_stop_safely(stop_line_x_m) {
                        out.add_stop(stop_line_x_m);
                    }
                }
                SignalState::FlashingAmber => {
                    if ahead(stop_line_x_m) {
                        out.add_cap(cfg.yield_speed_mps);
                    }
                }
            },
            TrafficControl::DoNotEnter { line_x_m } => {
                if ahead(line_x_m) {
                    out.add_stop(line_x_m);
                }
            }
            TrafficControl::RailCrossing { stop_line_x_m, must_stop } => {
                if must_stop && ahead(stop_line_x_m) {
                    out.add_stop(stop_line_x_m);
                }
            }
            TrafficControl::SpeedLimit { from_x_m, limit_mps } => {
                // In effect once we are at/after the sign, OR approaching it (we
                // must already be at the limit by the time we reach it).
                if ego_x >= from_x_m || ahead(from_x_m) {
                    out.add_cap(limit_mps);
                }
            }
            TrafficControl::AdvisorySpeed { from_x_m, limit_mps } => {
                if ego_x >= from_x_m || ahead(from_x_m) {
                    out.add_cap(limit_mps);
                }
            }
            TrafficControl::SchoolZone { from_x_m, to_x_m, limit_mps, active } => {
                if active && ego_x < to_x_m && (ego_x >= from_x_m || ahead(from_x_m)) {
                    out.add_cap(limit_mps);
                }
            }
            TrafficControl::WorkZone { from_x_m, to_x_m, limit_mps } => {
                if ego_x < to_x_m && (ego_x >= from_x_m || ahead(from_x_m)) {
                    out.add_cap(limit_mps);
                }
            }
        }
    }
    out
}

// ===========================================================================
// Lane-line crossing rules — the LATERAL behavioral constraint
//
// Lane markings govern *when you may cross laterally* (the longitudinal layer
// above governs *when you stop / how fast*). Like signs, these are LEGAL, not
// physical — you *can* drive across a solid line; doing so is unlawful, and its
// collision shadow (oncoming traffic) is still KIRRA's RSS. So the rule lives in
// Occy: it gates the lateral-avoidance maneuver (route-around / lane offset).
//
// Boundaries are given as lateral offsets from the path centerline (+y left).
// Full typed boundaries with real positions ride on a lane map (the adapter's
// Lanelet2 seam); this is the rule logic + the route-around gate.
// ===========================================================================

/// A lane-marking type and its crossing permission (per the rules of the road).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineType {
    /// Broken / dashed — crossing permitted either direction (when safe).
    Broken,
    /// Single solid — no crossing either direction.
    Solid,
    /// Double solid — no crossing either direction.
    DoubleSolid,
    /// Combined (solid + broken) with the **broken** marking facing the +y (left)
    /// side: a vehicle on the +y side may cross; the -y (solid) side may not.
    BrokenOnLeft,
    /// Combined with the broken marking facing the -y (right) side: the -y side
    /// may cross; the +y (solid) side may not.
    BrokenOnRight,
    /// **Unmarked** — no painted line at all (an undivided road / dirt road
    /// centerline). Crossing is *permitted* either direction (like [`Broken`]),
    /// because the law does not forbid using the other half to pass when clear.
    /// The absence of paint does NOT remove the rule of the road, though: the
    /// **keep-right positional default** is a separate concern, modeled
    /// structurally by placing the ego's lane on the right half of the road (see
    /// `lanemap::LaneGraph::from_undivided_corridor`), not by this crossing flag.
    ///
    /// [`Broken`]: LineType::Broken
    Unmarked,
}

/// A lane boundary at lateral offset `y_m` from the path centerline (+y left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneBoundary {
    pub y_m: f64,
    pub line: LineType,
}

impl LaneBoundary {
    /// May the ego cross this boundary moving laterally from `from_y` to `to_y`?
    /// A move that does not cross `y_m` is unconstrained (returns `true`).
    #[must_use]
    pub fn may_cross(&self, from_y: f64, to_y: f64) -> bool {
        let from_side = from_y - self.y_m;
        let to_side = to_y - self.y_m;
        // Not crossing this line (same side, or starting exactly on it) → allowed.
        if from_side.abs() <= 1e-9 || from_side.signum() == to_side.signum() {
            return true;
        }
        match self.line {
            // Permissive: dashed paint, or no paint at all (undivided road).
            LineType::Broken | LineType::Unmarked => true,
            LineType::Solid | LineType::DoubleSolid => false,
            // Combined: only the side the broken marking faces may cross.
            LineType::BrokenOnLeft => from_side > 0.0,
            LineType::BrokenOnRight => from_side < 0.0,
        }
    }
}

/// Is a lateral move from `from_y` to `to_y` permitted by ALL lane boundaries?
#[must_use]
pub fn lateral_move_permitted(boundaries: &[LaneBoundary], from_y: f64, to_y: f64) -> bool {
    boundaries.iter().all(|b| b.may_cross(from_y, to_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: BehaviorConfig = BehaviorConfig { amber_comfortable_decel_mps2: 3.0, yield_speed_mps: 4.0 };

    #[test]
    fn red_light_requires_stop_green_does_not() {
        let red = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Red }];
        assert_eq!(evaluate_controls(&red, 5.0, 8.0, &CFG).stop_x_m, Some(20.0));
        let green = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Green }];
        assert_eq!(evaluate_controls(&green, 5.0, 8.0, &CFG).stop_x_m, None);
    }

    #[test]
    fn stop_sign_stops_until_satisfied() {
        let unsat = [TrafficControl::StopSign { stop_line_x_m: 15.0, satisfied: false }];
        assert_eq!(evaluate_controls(&unsat, 5.0, 6.0, &CFG).stop_x_m, Some(15.0));
        let sat = [TrafficControl::StopSign { stop_line_x_m: 15.0, satisfied: true }];
        assert_eq!(evaluate_controls(&sat, 5.0, 6.0, &CFG).stop_x_m, None);
    }

    #[test]
    fn amber_dilemma_zone_stops_when_safe_clears_when_not() {
        // Far away & slow → can stop safely → stop.
        let amber = [TrafficControl::TrafficLight { stop_line_x_m: 40.0, state: SignalState::Amber }];
        assert_eq!(evaluate_controls(&amber, 5.0, 8.0, &CFG).stop_x_m, Some(40.0));
        // Close & fast → required decel exceeds comfortable → clear (no stop).
        // d = 3 m, v = 12 → req_decel = 144/6 = 24 m/s² ≫ 3 → proceed.
        let amber_close = [TrafficControl::TrafficLight { stop_line_x_m: 8.0, state: SignalState::Amber }];
        assert_eq!(evaluate_controls(&amber_close, 5.0, 12.0, &CFG).stop_x_m, None);
    }

    #[test]
    fn flashing_red_is_a_stop_flashing_amber_is_yield() {
        let fr = [TrafficControl::TrafficLight { stop_line_x_m: 10.0, state: SignalState::FlashingRed }];
        assert_eq!(evaluate_controls(&fr, 2.0, 5.0, &CFG).stop_x_m, Some(10.0));
        let fa = [TrafficControl::TrafficLight { stop_line_x_m: 10.0, state: SignalState::FlashingAmber }];
        let b = evaluate_controls(&fa, 2.0, 8.0, &CFG);
        assert_eq!(b.stop_x_m, None);
        assert_eq!(b.speed_cap_mps, Some(4.0));
    }

    #[test]
    fn do_not_enter_is_a_hard_stop() {
        let dne = [TrafficControl::DoNotEnter { line_x_m: 12.0 }];
        assert_eq!(evaluate_controls(&dne, 3.0, 6.0, &CFG).stop_x_m, Some(12.0));
    }

    #[test]
    fn rail_crossing_stops_only_when_required() {
        let train = [TrafficControl::RailCrossing { stop_line_x_m: 18.0, must_stop: true }];
        assert_eq!(evaluate_controls(&train, 2.0, 6.0, &CFG).stop_x_m, Some(18.0));
        let clear = [TrafficControl::RailCrossing { stop_line_x_m: 18.0, must_stop: false }];
        assert_eq!(evaluate_controls(&clear, 2.0, 6.0, &CFG).stop_x_m, None);
    }

    #[test]
    fn speed_controls_take_the_lowest_cap() {
        let controls = [
            TrafficControl::SpeedLimit { from_x_m: 0.0, limit_mps: 13.0 },
            TrafficControl::SchoolZone { from_x_m: 10.0, to_x_m: 50.0, limit_mps: 7.0, active: true },
            TrafficControl::AdvisorySpeed { from_x_m: 5.0, limit_mps: 9.0 },
        ];
        assert_eq!(evaluate_controls(&controls, 12.0, 10.0, &CFG).speed_cap_mps, Some(7.0));
    }

    #[test]
    fn inactive_school_zone_and_passed_zone_do_not_cap() {
        let inactive = [TrafficControl::SchoolZone { from_x_m: 10.0, to_x_m: 50.0, limit_mps: 7.0, active: false }];
        assert_eq!(evaluate_controls(&inactive, 12.0, 10.0, &CFG).speed_cap_mps, None);
        // Already past the work zone end → no cap.
        let passed = [TrafficControl::WorkZone { from_x_m: 10.0, to_x_m: 20.0, limit_mps: 6.0 }];
        assert_eq!(evaluate_controls(&passed, 25.0, 10.0, &CFG).speed_cap_mps, None);
    }

    #[test]
    fn nearest_stop_line_binds() {
        let controls = [
            TrafficControl::TrafficLight { stop_line_x_m: 30.0, state: SignalState::Red },
            TrafficControl::StopSign { stop_line_x_m: 12.0, satisfied: false },
        ];
        assert_eq!(evaluate_controls(&controls, 2.0, 6.0, &CFG).stop_x_m, Some(12.0));
    }

    // --- lane-line crossing rules ---

    #[test]
    fn broken_line_allows_crossing_solid_forbids() {
        let broken = LaneBoundary { y_m: -0.5, line: LineType::Broken };
        assert!(broken.may_cross(0.0, -1.5), "broken: crossing OK");
        let solid = LaneBoundary { y_m: -0.5, line: LineType::Solid };
        assert!(!solid.may_cross(0.0, -1.5), "solid: no crossing");
        let double = LaneBoundary { y_m: 0.5, line: LineType::DoubleSolid };
        assert!(!double.may_cross(0.0, 2.0), "double solid: no crossing");
    }

    #[test]
    fn unmarked_line_allows_crossing_like_broken() {
        // No paint (undivided / dirt road centerline) → crossing permitted either
        // way, same as a broken line. (Keep-right is a positional default handled
        // elsewhere, not a crossing prohibition.)
        let unmarked = LaneBoundary { y_m: 0.0, line: LineType::Unmarked };
        assert!(unmarked.may_cross(-1.0, 1.0), "unmarked: cross left OK");
        assert!(unmarked.may_cross(1.0, -1.0), "unmarked: cross right OK");
    }

    #[test]
    fn not_crossing_a_line_is_unconstrained() {
        // Move stays on one side of the line → allowed even for a solid line.
        let solid = LaneBoundary { y_m: -3.0, line: LineType::Solid };
        assert!(solid.may_cross(0.0, -1.5), "did not reach the line → OK");
    }

    #[test]
    fn combined_line_crosses_from_the_broken_side_only() {
        // Broken faces +y: a vehicle on the +y side may cross down; the -y side may not.
        let bl = LaneBoundary { y_m: 0.0, line: LineType::BrokenOnLeft };
        assert!(bl.may_cross(1.0, -1.0), "+y (broken) side may cross");
        assert!(!bl.may_cross(-1.0, 1.0), "-y (solid) side may NOT cross");
        // Mirror image.
        let br = LaneBoundary { y_m: 0.0, line: LineType::BrokenOnRight };
        assert!(br.may_cross(-1.0, 1.0), "-y (broken) side may cross");
        assert!(!br.may_cross(1.0, -1.0), "+y (solid) side may NOT cross");
    }

    #[test]
    fn lateral_move_permitted_requires_all_boundaries() {
        let bounds = [
            LaneBoundary { y_m: -0.5, line: LineType::Broken },
            LaneBoundary { y_m: -1.0, line: LineType::Solid },
        ];
        // Crossing to -1.5 crosses BOTH; the solid one forbids it.
        assert!(!lateral_move_permitted(&bounds, 0.0, -1.5));
        // Crossing only to -0.7 crosses just the broken one → allowed.
        assert!(lateral_move_permitted(&bounds, 0.0, -0.7));
    }
}
