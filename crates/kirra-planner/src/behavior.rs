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

/// A traffic-signal indication (the longitudinally-relevant set). A solid `Green` is
/// **permissive** — for a turn it means "proceed *if clear*", so the turn-maneuver layer still
/// gap-accepts oncoming traffic. `ProtectedGreen` (a green turn arrow) is the **protected** movement
/// — the conflicting streams hold a red, so the turn proceeds with priority. The protected/permitted
/// distinction is consumed by the maneuver-intent layer (`mick`'s `TurnAt` grounding), not this
/// longitudinal layer, where a `ProtectedGreen` is simply "proceed" like `Green`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalState {
    /// Proceed. **Permissive** for a turn — yield to oncoming (gap-accept).
    Green,
    /// Protected green turn arrow — proceed with **priority** (conflicting streams are stopped).
    /// Longitudinally identical to `Green`; the difference is the turn-maneuver right-of-way.
    ProtectedGreen,
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
    /// **Occluded junction approach** — RSS Rule 4 (caution under limited visibility)
    /// applied LATERALLY at a junction. The ego is approaching the conflict point at
    /// `conflict_line_x_m` but a building / parked car / hedge blocks its view of cross
    /// traffic, so it has assured-clear sight of only `sight_distance_m` toward it. Unobserved
    /// space is treated as possibly occupied by an emerging vehicle, so the approach speed is
    /// capped to the **assured-clear-distance speed** — the most the ego may carry and still
    /// brake to a stop within what it can see ([`assured_clear_distance_speed_cap`]). The ego
    /// therefore CREEPS into a blind junction, fast where the view is open and slow where it is
    /// not; as it nears the corner and perception reports more sight, the cap relaxes. The cap
    /// applies only while the conflict is still ahead (once passed, the junction is cleared).
    /// This is the behavioral/legal "slow for a blind corner" rule; KIRRA's RSS still bounds
    /// any cross vehicle that actually becomes visible.
    OccludedApproach { conflict_line_x_m: f64, sight_distance_m: f64 },
}

/// RSS reaction time (s) for the occlusion speed bound — the conservative SAE-L4 value, matched
/// to the checker's `RSS_REACTION_TIME_S` so the doer's cap composes with the checker's bound.
const OCCLUSION_REACTION_TIME_S: f64 = 0.5;

/// RSS Rule 4 — the **assured-clear-distance** speed bound: the maximum speed (m/s) from which
/// the ego can brake to a stop within `visible_m`, treating unobserved space beyond as
/// potentially occupied. Includes the reaction distance ([`OCCLUSION_REACTION_TIME_S`]).
/// Solves `v·t + v²/(2a) = visible` for `v`: `v = sqrt((a·t)² + 2·a·visible) − a·t`, clamped at
/// 0. Mirrors the checker's `assured_clear_distance_speed_cap` so a doer plan capped here
/// (with a comfortable decel ≤ the checker's brake decel) is checker-admissible, not just
/// fail-closed.
// SAFETY: SG1 SG9 | REQ: occlusion-assured-clear-distance-speed-bound | TEST: occluded_approach_caps_speed_to_the_assured_clear_distance,occluded_approach_stops_capping_once_the_conflict_is_passed,occlusion_cap_composes_with_other_speed_caps_taking_the_lowest,the_ego_creeps_into_a_blind_junction_but_cruises_an_open_one,occlusion_creep_composes_with_a_stop_sign_at_the_same_blind_junction
#[must_use]
pub fn assured_clear_distance_speed_cap(visible_m: f64, brake_decel_mps2: f64) -> f64 {
    let a = brake_decel_mps2.max(0.0);
    let d = visible_m.max(0.0);
    let t = OCCLUSION_REACTION_TIME_S;
    (((a * t).powi(2) + 2.0 * a * d).sqrt() - a * t).max(0.0)
}

/// Critical gap (s) for accepting an **unprotected turn** — the time the ego needs to clear the
/// junction conflict zone before a vehicle it must yield to arrives, plus a reaction/safety margin.
/// Standard gap-acceptance (HCM left-turn critical gap ≈ 4–4.5 s). The doer BEGINS the turn only if
/// every conflicting approach is at least this far away in time; otherwise it HOLDs for a gap.
pub const DEFAULT_TURN_CRITICAL_GAP_S: f64 = 4.0;

/// One conflicting vehicle's approach to a turn's conflict point, reduced to the single quantity
/// gap-acceptance needs: the **time** until it reaches the conflict (`distance / closing-speed`).
/// The caller computes it from perception (only vehicles actually CLOSING on the conflict and which
/// the ego must yield to — i.e. NOT on its right-of-way cede list — are passed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConflictApproach {
    /// Seconds until this vehicle reaches the turn conflict point (`> 0`, finite, when closing).
    pub time_to_conflict_s: f64,
}

/// Gap-acceptance: may the ego BEGIN an unprotected turn now? It proceeds only if **every**
/// conflicting approach is more than `critical_gap_s` away in time (the smallest gap clears the
/// critical gap); any closer conflict → HOLD and wait for a gap. No conflicts → proceed.
///
/// Fail-closed by construction: the test is `time > critical_gap_s`, so a NaN time (bad data) is
/// `false` ⇒ HOLD; a `critical_gap_s` that is non-finite/≤0 (misconfig) makes no approach pass
/// unless genuinely clear. A non-closing vehicle is excluded UPSTREAM (it is not a conflict), so it
/// is never represented as a `ConflictApproach`. KIRRA's head-on / crossing RSS independently
/// backstops a misjudged acceptance.
// SAFETY: SG5 | REQ: unprotected-turn-gap-acceptance | TEST: no_conflicts_accepts_the_turn,an_ample_gap_accepts_the_turn,a_tight_gap_holds_the_turn,the_critical_gap_boundary_is_strict,a_nonfinite_gap_fails_closed_to_hold,a_vehicle_already_at_the_conflict_holds,a_tight_gap_holds_the_turn,asserting_right_of_way_proceeds_through_the_same_tight_gap
#[must_use]
pub fn accept_turn_gap(approaches: &[ConflictApproach], critical_gap_s: f64) -> bool {
    approaches.iter().all(|a| a.time_to_conflict_s > critical_gap_s)
}

/// Tunables for the behavioral layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BehaviorConfig {
    /// Comfortable deceleration used for the amber dilemma-zone test: on amber,
    /// stop only if the required decel to the line is at/below this.
    pub amber_comfortable_decel_mps2: f64,
    /// Speed approaching a YIELD / flashing-amber control (slow, ready to stop).
    pub yield_speed_mps: f64,
    /// Comfortable deceleration used to derive the occluded-junction approach speed cap (the
    /// assured-clear-distance bound). Kept at/below a vehicle's hard brake so the doer's
    /// occlusion cap is conservative w.r.t. the checker's RSS Rule 4 — the doer slows enough
    /// that the resulting plan is checker-admissible.
    pub occlusion_brake_decel_mps2: f64,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self { amber_comfortable_decel_mps2: 3.0, yield_speed_mps: 4.0, occlusion_brake_decel_mps2: 3.0 }
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
                // Both proceed longitudinally; the protected/permitted turn distinction is the
                // maneuver layer's (`mick` TurnAt gap-acceptance), not this longitudinal one.
                SignalState::Green | SignalState::ProtectedGreen => {}
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
            TrafficControl::OccludedApproach { conflict_line_x_m, sight_distance_m } => {
                // While the blind junction is still ahead, cap the speed to what the ego can
                // stop within its assured-clear sight — it creeps in, ready for emergent
                // cross-traffic. Once the conflict is passed, the junction is cleared (no cap).
                if ahead(conflict_line_x_m) {
                    out.add_cap(assured_clear_distance_speed_cap(
                        sight_distance_m,
                        cfg.occlusion_brake_decel_mps2,
                    ));
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
// Lane-line crossing rules — the LATERAL behavioral constraint — moved to the shared
// `kirra-map` crate (de-monolith Stage 6b: `kirra_map::lane_lines`). They are lane-map
// substrate (typed lane lines at real positions ride on the lane map), distinct from
// this module's longitudinal traffic-control logic, which stays here. Re-exported so
// `crate::behavior::{LaneBoundary, LineType, lateral_move_permitted}` (and the planner's
// `kirra_planner::{LaneBoundary, LineType}` re-exports) keep the SAME types.
// ===========================================================================
pub use kirra_map::lane_lines::{lateral_move_permitted, LaneBoundary, LineType};

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: BehaviorConfig = BehaviorConfig { amber_comfortable_decel_mps2: 3.0, yield_speed_mps: 4.0, occlusion_brake_decel_mps2: 3.0 };

    #[test]
    fn red_light_requires_stop_green_does_not() {
        let red = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Red }];
        assert_eq!(evaluate_controls(&red, 5.0, 8.0, &CFG).stop_x_m, Some(20.0));
        let green = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Green }];
        assert_eq!(evaluate_controls(&green, 5.0, 8.0, &CFG).stop_x_m, None);
        // A protected green arrow is longitudinally identical to green — proceed, no stop/cap (the
        // protected/permitted right-of-way distinction is the maneuver layer's, not this one's).
        let prot = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::ProtectedGreen }];
        let b = evaluate_controls(&prot, 5.0, 8.0, &CFG);
        assert_eq!(b.stop_x_m, None);
        assert_eq!(b.speed_cap_mps, None);
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

    #[test]
    fn occluded_approach_caps_speed_to_the_assured_clear_distance() {
        // 5 m of assured-clear sight toward a conflict 30 m ahead: cap = ACD(5, a=3, t=0.5) =
        // sqrt(2.25 + 30) − 1.5 ≈ 4.18 m/s. A short-sight blind corner forces a creep.
        let occ = [TrafficControl::OccludedApproach { conflict_line_x_m: 30.0, sight_distance_m: 5.0 }];
        let cap = evaluate_controls(&occ, 5.0, 8.0, &CFG).speed_cap_mps.expect("occlusion caps speed");
        let expect = assured_clear_distance_speed_cap(5.0, 3.0);
        assert!((cap - expect).abs() < 1e-9, "cap is the ACD speed, got {cap} expect {expect}");
        assert!(cap < 8.0, "the blind approach is slower than cruise, got {cap}");
    }

    #[test]
    fn shorter_sight_caps_lower_more_blind_means_slower() {
        let near = evaluate_controls(&[TrafficControl::OccludedApproach { conflict_line_x_m: 30.0, sight_distance_m: 3.0 }], 5.0, 8.0, &CFG).speed_cap_mps.unwrap();
        let far = evaluate_controls(&[TrafficControl::OccludedApproach { conflict_line_x_m: 30.0, sight_distance_m: 12.0 }], 5.0, 8.0, &CFG).speed_cap_mps.unwrap();
        assert!(near < far, "less sight → lower cap (creep more), got near {near} far {far}");
        // A wide-open view imposes effectively no creep (the cap exceeds a normal cruise).
        let open = evaluate_controls(&[TrafficControl::OccludedApproach { conflict_line_x_m: 30.0, sight_distance_m: 80.0 }], 5.0, 8.0, &CFG).speed_cap_mps.unwrap();
        assert!(open > 12.0, "a clear view does not meaningfully cap, got {open}");
    }

    #[test]
    fn occluded_approach_stops_capping_once_the_conflict_is_passed() {
        // Conflict behind the ego (already through the junction) → no cap.
        let passed = [TrafficControl::OccludedApproach { conflict_line_x_m: 10.0, sight_distance_m: 4.0 }];
        assert_eq!(evaluate_controls(&passed, 15.0, 8.0, &CFG).speed_cap_mps, None);
    }

    #[test]
    fn occlusion_cap_composes_with_other_speed_caps_taking_the_lowest() {
        // A 4 m/s yield-like school-zone cap vs the ACD cap; the binding (lower) one wins.
        let controls = [
            TrafficControl::OccludedApproach { conflict_line_x_m: 40.0, sight_distance_m: 20.0 }, // ~9 m/s
            TrafficControl::SchoolZone { from_x_m: 0.0, to_x_m: 50.0, limit_mps: 4.0, active: true },
        ];
        assert_eq!(evaluate_controls(&controls, 10.0, 10.0, &CFG).speed_cap_mps, Some(4.0));
    }

    // The lane-line crossing-rule tests moved with their types to
    // `kirra_map::lane_lines` (de-monolith Stage 6b).

    // ---- Unprotected-turn gap-acceptance --------------------------------------------------

    fn approach(t: f64) -> ConflictApproach {
        ConflictApproach { time_to_conflict_s: t }
    }

    #[test]
    fn no_conflicts_accepts_the_turn() {
        assert!(accept_turn_gap(&[], DEFAULT_TURN_CRITICAL_GAP_S), "nothing to yield to ⇒ proceed");
    }

    #[test]
    fn an_ample_gap_accepts_the_turn() {
        // Both conflicting vehicles are well over the critical gap away.
        assert!(accept_turn_gap(&[approach(6.0), approach(8.0)], DEFAULT_TURN_CRITICAL_GAP_S));
    }

    #[test]
    fn a_tight_gap_holds_the_turn() {
        // One vehicle is closer than the critical gap → the smallest gap binds → HOLD.
        assert!(!accept_turn_gap(&[approach(6.0), approach(2.5)], DEFAULT_TURN_CRITICAL_GAP_S));
    }

    #[test]
    fn the_critical_gap_boundary_is_strict() {
        // Exactly at the critical gap is NOT enough (strict >): the ego waits for clearly more.
        assert!(!accept_turn_gap(&[approach(DEFAULT_TURN_CRITICAL_GAP_S)], DEFAULT_TURN_CRITICAL_GAP_S));
        assert!(accept_turn_gap(&[approach(DEFAULT_TURN_CRITICAL_GAP_S + 0.01)], DEFAULT_TURN_CRITICAL_GAP_S));
    }

    #[test]
    fn a_nonfinite_gap_fails_closed_to_hold() {
        // Bad perception data (NaN time) must not be read as "clear" — `NaN > c` is false ⇒ HOLD.
        assert!(!accept_turn_gap(&[approach(f64::NAN)], DEFAULT_TURN_CRITICAL_GAP_S));
        assert!(!accept_turn_gap(&[approach(10.0), approach(f64::NAN)], DEFAULT_TURN_CRITICAL_GAP_S));
    }

    #[test]
    fn a_vehicle_already_at_the_conflict_holds() {
        // time_to_conflict ≤ 0 (a vehicle in/through the conflict zone now) → HOLD.
        assert!(!accept_turn_gap(&[approach(0.0)], DEFAULT_TURN_CRITICAL_GAP_S));
        assert!(!accept_turn_gap(&[approach(-1.0)], DEFAULT_TURN_CRITICAL_GAP_S));
    }
}
