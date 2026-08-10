// crates/kirra-proposal-context/src/lib.rs
//
// Tier 2.5 — the sanctioned Kirra World consumer (KIRRA-WM-CONSUMER-PLACEMENT-001).
//
//   Kirra World → proposal-context → symbolic context → proposal producer
//   ─────────────────────────────────────────────────────────────────────
//   checker boundary: CorridorSource / contract inputs → governor / checker
//
// It may influence WHAT IS PROPOSED. It may not implement or feed
// `CorridorSource`, checker bounds, release authority, or actuation.
//
// § THE SEAM IS CAPABILITY-LIMITED, NOT MERELY WELL-BEHAVED
//
// The rule this crate exists to make true:
//
//   > World-derived proposal context is SYMBOLIC ONLY. Its public API may carry
//   > identities, relations, ordering, categorical state, and opaque references;
//   > it may not carry numeric quantities that could encode checker bounds.
//
// "Does not currently depend on `kirra-core`" would be a weaker property — true
// today, one import away from false, and invisible in review. This is stronger:
// every checker bound in this codebase is a magnitude with physical units
// (corridor width, max speed, stopping distance, lateral accel, wheelbase), and
// none of the types below can hold a magnitude at all. A speed cap cannot be
// smuggled through a seam that has nowhere to put a number.
//
// The ban covers INTEGERS too, not just floats. `speed_mm_s: u32` is a bound
// wearing a disguise, and it is the more likely accident: someone reaching for
// integer millimetres is usually trying to be careful. The mechanical gate is
// `ci/check_proposal_context_symbolic.py` — no primitive numeric field on a
// public type unless an allowlist entry justifies it as non-physical.
//
// FUNCTION PARAMETERS ARE OUT OF SCOPE, deliberately. `now_ms` below is a query
// instant, not a bound: the store is bitemporal and cannot be read without one.
// It is never carried ON the context, which is where the rule bites — a value
// that crosses the seam is a value a planner can act on; a value used to read
// the store and discarded is not.
//
// § WHAT THIS CRATE DOES NOT PROVE
//
// Nothing consumes its output in production yet. The differential harness in
// `tests/` shows that world knowledge changes the symbolic context, and that a
// proposal-producing function fed by it produces a different proposal. That is
// Tier 2.5 EVIDENCE, not Tier 2.5 closure: §5.5's Goal 1 requires a host whose
// removal changes observable proposal behaviour, and the production
// orchestration boundary has not been ruled yet. Until it is, this crate is a
// consumer with a test-local consumer of its own, and the honest claim is the
// narrow one.

#![forbid(unsafe_code)]

use kirra_world_store::{StoreError, WorldStore};

/// An opaque symbolic identity — a destination, a task, a package, a relation.
///
/// A newtype over `String` rather than a bare `String` so the seam's vocabulary
/// is nameable, and over `String` rather than anything numeric because an
/// identity is the one thing a bound can never be. Ordering is lexical on the
/// id, which is a total order over symbols and carries no magnitude.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextId(String);

impl ContextId {
    /// Build an id. Rejects empty and whitespace-only ids — an unnamed identity
    /// is not a weaker identity, it is a bug, and accepting one would let a
    /// context claim to prefer a destination it cannot name.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.trim().is_empty() {
            return None;
        }
        Some(Self(id))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One piece of world-derived, proposal-shaping context.
///
/// Every variant is symbolic. `CandidatePriority` is an ORDERING rather than a
/// score for exactly this reason: a score would be a number, a number would need
/// units to mean anything, and a unit-bearing number is the shape of a bound.
/// An ordering expresses a preference completely without ever saying how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextHint {
    /// Prefer this destination over others, all else equal.
    PreferDestination(ContextId),
    /// Avoid this task if the proposal has a choice.
    AvoidTask(ContextId),
    /// Candidates, most preferred first. Ordering only — never a score.
    CandidatePriority(Vec<ContextId>),
    /// A world fact the proposal may reason with, as a symbolic triple.
    MissionFact {
        /// What the fact is about.
        subject: ContextId,
        /// How subject and object relate.
        relation: ContextId,
        /// What it relates to.
        object: ContextId,
    },
}

/// The bundle of hints that crosses the seam.
///
/// Private field with an accessor, following the `WorldAnswer` precedent: a
/// public `Vec` would let a caller mutate the context in place and attribute the
/// result to Kirra World.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProposalContext {
    hints: Vec<ContextHint>,
}

impl ProposalContext {
    /// An empty context — what a proposal producer sees when Kirra World is
    /// unavailable or knows nothing relevant. Distinct from "no opinion about
    /// this candidate": empty means the seam carried nothing at all.
    #[must_use]
    pub fn empty() -> Self {
        Self { hints: Vec::new() }
    }

    /// The hints, in the order the producer emitted them.
    #[must_use]
    pub fn hints(&self) -> &[ContextHint] {
        &self.hints
    }

    /// True when the seam carried nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    /// The preferred destination, if the context expressed one.
    #[must_use]
    pub fn preferred_destination(&self) -> Option<&ContextId> {
        self.hints.iter().find_map(|h| match h {
            ContextHint::PreferDestination(id) => Some(id),
            _ => None,
        })
    }

    /// The candidate ordering, if the context expressed one.
    #[must_use]
    pub fn candidate_priority(&self) -> Option<&[ContextId]> {
        self.hints.iter().find_map(|h| match h {
            ContextHint::CandidatePriority(ids) => Some(ids.as_slice()),
            _ => None,
        })
    }
}

/// What can go wrong producing context. Reading the world can fail; producing
/// context cannot "partially succeed" — a caller that cannot read the world gets
/// an error and decides for itself, rather than an empty context that would be
/// indistinguishable from "the world knows nothing".
#[derive(Debug)]
pub enum ContextError {
    /// The world store could not be read.
    Store(StoreError),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "world store read failed: {e}"),
        }
    }
}

impl std::error::Error for ContextError {}

impl From<StoreError> for ContextError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// Derive proposal-shaping context from what Kirra World knows about `subject`.
///
/// The one behaviour, kept deliberately tiny (Tier 2.5 asks for ONE observable
/// difference, not a generally useful engine): if the world holds a claim
/// `subject --relation--> X` and `X` is among `candidates`, the returned context
/// prefers `X` and reorders the candidate list to put it first. If the world
/// holds no such claim, the context carries the candidates in the order given
/// and expresses no preference.
///
/// `now_ms` is the bitemporal query instant — see the module note on why a
/// parameter is not a seam value.
///
/// Note what is NOT consulted: nothing spatial, nothing metric. The producer
/// reads a symbolic triple and emits symbolic hints. There is no path from a
/// world claim's numeric payload to the returned context, because the returned
/// type has nowhere to put one.
pub fn mission_context(
    store: &WorldStore,
    subject: &ContextId,
    relation: &ContextId,
    candidates: &[ContextId],
    now_ms: i64,
) -> Result<ProposalContext, ContextError> {
    let claims = store.current(subject.as_str(), now_ms)?;

    let preferred = claims.iter().find_map(|c| {
        let predicate = c.predicate.as_deref()?;
        if predicate != relation.as_str() {
            return None;
        }
        let object = c.object.as_deref()?;
        candidates.iter().find(|cand| cand.as_str() == object)
    });

    let Some(preferred) = preferred else {
        // The world had nothing to say. Carry the candidates unchanged and
        // express no preference — an ordering the caller already had is not a
        // world-derived hint, but omitting it entirely would make "world silent"
        // and "world absent" produce different shapes for the same knowledge.
        return Ok(ProposalContext {
            hints: vec![ContextHint::CandidatePriority(candidates.to_vec())],
        });
    };

    let mut priority = Vec::with_capacity(candidates.len());
    priority.push(preferred.clone());
    priority.extend(candidates.iter().filter(|c| *c != preferred).cloned());

    Ok(ProposalContext {
        hints: vec![
            ContextHint::PreferDestination(preferred.clone()),
            ContextHint::CandidatePriority(priority),
            ContextHint::MissionFact {
                subject: subject.clone(),
                relation: relation.clone(),
                object: preferred.clone(),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_must_actually_name_something() {
        assert!(ContextId::new("dock_b").is_some());
        assert!(ContextId::new("").is_none());
        assert!(ContextId::new("   ").is_none());
    }

    #[test]
    fn an_empty_context_carries_nothing_and_says_so() {
        let c = ProposalContext::empty();
        assert!(c.is_empty());
        assert!(c.hints().is_empty());
        assert_eq!(c.preferred_destination(), None);
        assert_eq!(c.candidate_priority(), None);
    }

    #[test]
    fn accessors_find_the_hints_they_name() {
        let dock_a = ContextId::new("dock_a").expect("id");
        let dock_b = ContextId::new("dock_b").expect("id");
        let c = ProposalContext {
            hints: vec![
                ContextHint::PreferDestination(dock_b.clone()),
                ContextHint::CandidatePriority(vec![dock_b.clone(), dock_a.clone()]),
            ],
        };
        assert_eq!(c.preferred_destination(), Some(&dock_b));
        assert_eq!(c.candidate_priority(), Some([dock_b, dock_a].as_slice()));
        assert!(!c.is_empty());
    }
}
