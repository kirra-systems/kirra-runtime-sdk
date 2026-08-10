// crates/kirra-mission-orchestrator/src/lib.rs
//
// Tier 2.5 — the production orchestration host
// (`KIRRA-WM-ORCHESTRATION-BOUNDARY-001`).
//
//   kirra-world → kirra-proposal-context → THIS CRATE → kirra-planner → proposal
//   ───────────────────────────────────────────────────────────────────────────
//   checker boundary: CorridorSource / contract inputs → governor / checker
//
// > A production orchestration layer may consume `kirra-proposal-context` and
// > pass symbolic preferences into proposal generation, but the proposal
// > producer itself must remain World-blind, and no type crossing the seam may
// > encode checker bounds.
//
// § WHY THIS CRATE EXISTS RATHER THAN A WORLD EDGE ON THE PLANNER
//
// `kirra-sidecars` depends on `kirra-planner` AND implements `CorridorSource`.
// A world edge on the planner would therefore make `kirra-sidecars` reach Kirra
// World, satisfying the fence's check-4 conjunction — the exact route the fence
// exists to refuse. Holding both edges HERE keeps the arrow pointing from this
// crate to the planner, so the planner gains nothing and nothing downstream of
// it changes.
//
// § THE SYMBOL→COORDINATE HOP, AND WHY THE TABLE IS NOT WORLD-DERIVED
//
// `Goal { target: Pose }` is coordinates, so somewhere a symbol must become
// numbers. That hop happens HERE, against a [`MissionTable`] built from mission /
// map configuration:
//
// > **Kirra World may say WHICH destination. It may not say WHERE that
// > destination is.**
//
// World knowledge selects among coordinates that already existed; it never
// authors one. Without that rule the symbolic seam would hold at the boundary
// and be lost one call past it — a world-authored `Pose` is a world-authored
// number sitting one type away from the planner's input.
//
// So `MissionTable` is constructed by the integrator and NEVER from a
// `WorldStore`. This crate does not depend on `kirra-world*` at all — only on
// `kirra-proposal-context`, whose types are symbolic by construction and whose
// symbolic-ness is gated by `ci/check_proposal_context_symbolic.py`.
//
// NOTE ON THAT GATE'S SCOPE: it guards the SEAM crate, not this one. This crate
// necessarily holds coordinates — that is its job. The gate would be wrong to
// cover it, and a reader expecting otherwise should read the boundary as: the
// numbers live on THIS side of the symbol→coordinate hop, and no number crosses
// backwards.
//
// § TWO THINGS THIS CRATE MAY NOT DO
//
//   * It may not construct, wrap, or modify the `CorridorSource` it forwards.
//     It re-borrows the caller's world untouched (`..world.clone()`), which is
//     the same idiom the production Mick bridge already uses.
//   * It may not read a checker verdict and re-plan against its NUMERIC content.
//     Reading *that* a proposal was refused is operational state; reading
//     "refused by 0.42 m/s" is consuming checker-bound information, and tuning
//     the next proposal against it would close exactly the feedback loop the
//     fence exists to prevent. There is deliberately no verdict input here.

#![forbid(unsafe_code)]

use kirra_planner::mick::{plan_for_intent, MickIntent};
use kirra_planner::{PlanInput, PlanOutput, Planner};
use kirra_proposal_context::{ContextId, ProposalContext};

/// Where a symbolic destination actually is.
///
/// Built from mission / map CONFIGURATION. Never from Kirra World — see the
/// module note: World says which, configuration says where.
#[derive(Debug, Clone, Default)]
pub struct MissionTable {
    entries: Vec<(ContextId, (f64, f64))>,
}

impl MissionTable {
    /// An empty table. A destination the table does not know is not a
    /// destination this host can propose — see [`Self::resolve`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record where a symbolic destination is. Later entries for the same id
    /// replace earlier ones, so a reconfiguration is a re-`insert`.
    pub fn insert(&mut self, id: ContextId, x_m: f64, y_m: f64) {
        self.entries.retain(|(k, _)| k != &id);
        self.entries.push((id, (x_m, y_m)));
    }

    /// Coordinates for a symbolic id, if configuration knows them.
    ///
    /// `None` for an unknown id — FAIL-CLOSED in the only sense available here:
    /// the host proposes nothing world-derived rather than inventing a location
    /// for a symbol it cannot place. Kirra World naming a destination that
    /// configuration does not know must not become a coordinate.
    #[must_use]
    pub fn resolve(&self, id: &ContextId) -> Option<(f64, f64)> {
        self.entries
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, xy)| *xy)
    }
}

/// What the host did with the context — returned alongside the plan so a caller
/// (and the differential harness) can see WHETHER world knowledge was applied,
/// without inspecting the plan's geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextApplication {
    /// The context expressed no preference: the caller's own goal was used
    /// unchanged.
    NoPreference,
    /// The context preferred a destination configuration could place, and the
    /// goal was replaced with it.
    Applied(ContextId),
    /// The context preferred a destination the mission table does not know. The
    /// caller's goal is used unchanged — a symbol configuration cannot place
    /// never becomes a coordinate.
    UnknownDestination(ContextId),
}

/// Plan with world-derived symbolic preference applied to the GOAL only.
///
/// `world` is forwarded untouched except for the goal — the same
/// `PlanInput { goal, ..world.clone() }` idiom the production Mick bridge uses,
/// which re-borrows the caller's corridor and objects rather than copying or
/// rebuilding them. That is what makes "the checker's inputs are unchanged"
/// structural here rather than merely asserted: across two runs there is ONE
/// corridor borrow and ONE object slice, not two equal ones.
///
/// The proposal producer is `plan_for_intent` — the unmodified production
/// bridge, which knows nothing about Kirra World.
pub fn plan_with_context(
    planner: &mut impl Planner,
    world: &PlanInput<'_>,
    context: &ProposalContext,
    missions: &MissionTable,
) -> (PlanOutput, ContextApplication) {
    let Some(preferred) = context.preferred_destination() else {
        return (
            plan_for_intent(
                planner,
                &MickIntent::GoTo {
                    x_m: world.goal.target.x_m,
                    y_m: world.goal.target.y_m,
                },
                world,
            ),
            ContextApplication::NoPreference,
        );
    };

    let Some((x_m, y_m)) = missions.resolve(preferred) else {
        return (
            plan_for_intent(
                planner,
                &MickIntent::GoTo {
                    x_m: world.goal.target.x_m,
                    y_m: world.goal.target.y_m,
                },
                world,
            ),
            ContextApplication::UnknownDestination(preferred.clone()),
        );
    };

    (
        plan_for_intent(planner, &MickIntent::GoTo { x_m, y_m }, world),
        ContextApplication::Applied(preferred.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ContextId {
        ContextId::new(s).expect("non-empty id")
    }

    #[test]
    fn the_table_resolves_only_what_configuration_recorded() {
        let mut t = MissionTable::new();
        t.insert(id("dock_b"), 12.0, 3.0);
        assert_eq!(t.resolve(&id("dock_b")), Some((12.0, 3.0)));
        assert_eq!(
            t.resolve(&id("dock_zzz")),
            None,
            "a symbol configuration cannot place must not become a coordinate"
        );
    }

    #[test]
    fn re_inserting_replaces_rather_than_shadows() {
        let mut t = MissionTable::new();
        t.insert(id("dock_b"), 1.0, 1.0);
        t.insert(id("dock_b"), 2.0, 2.0);
        assert_eq!(t.resolve(&id("dock_b")), Some((2.0, 2.0)));
        assert_eq!(t.entries.len(), 1, "reconfiguration replaces the entry");
    }
}
