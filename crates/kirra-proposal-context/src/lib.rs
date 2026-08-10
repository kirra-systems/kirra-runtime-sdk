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
// § WHO CONSUMES THIS
//
// `kirra-mission-orchestrator` — the production orchestration host ruled by
// `KIRRA-WM-ORCHESTRATION-BOUNDARY-001` and built in #1426, which closed Tier
// 2.5's Goal 1. Its closure differential shows a proposal changing because of
// world knowledge while the checker's inputs stay the same borrow.
//
// (This paragraph read *"nothing consumes its output in production yet"* until
// the host landed. It is called out because a stale disclaimer is worse than
// none: it invites a reader to discount evidence that now exists.)
//
// § THE OTHER HALF, WHICH THIS CRATE OWES ITS CONSUMER
//
// Every answer here arrives from the sanctioned boundary with validity, trust
// axes, grade and provenance attached, so the seam can carry the world's own
// judgement about a fact rather than just the fact. What it deliberately does
// NOT carry is any of those as a number — see the capability rule above.

#![forbid(unsafe_code)]

use kirra_world::resolution::RefusalReason;
use kirra_world::trust::{TrustGrade, Validity};
use kirra_world_service::read_view::{
    AskError, ObjectIdentity, UnknownReason, WorldLookup, WorldView,
};
use kirra_world_store::WorldStore;

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
    /// How much the world's own boundary trusts the fact this context acted on.
    ///
    /// CATEGORICAL, never a score — a grade is a named tier, so it carries no
    /// magnitude and the symbolic-seam gate is unaffected. The variant exists
    /// because a proposal that acted on world knowledge should be able to say
    /// how well-founded that knowledge was without re-reading the world.
    FactTrust(FactGrade),
    /// How fresh the fact was, as the boundary judged it against the caller's
    /// staleness budget. Categorical for the same reason.
    FactFreshness(FactValidity),
}

/// The trust grade of a world fact, mirrored symbolically.
///
/// A mirror rather than a re-export so this crate's public surface stays
/// independent of the world's internal vocabulary — and so the seam carries a
/// closed set of named tiers rather than anything a future edit could widen into
/// a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactGrade {
    /// The world graded the claim, and the grade is this tier.
    Graded(&'static str),
    /// The claim carries no trust axes, so there is no grade to report.
    ///
    /// Distinct from a low grade: absent is not weak. Manufacturing a default
    /// here would invent a trust judgement from the absence of one.
    Ungraded,
}

/// The freshness of a world fact, mirrored symbolically.
///
/// **There is deliberately no variant for an EXPIRED fact.** An expired fact is
/// not fresh, not stale, and emphatically not time-independent, so any label
/// would be a misstatement; `mission_context` refuses such a fact as
/// [`WorldSilence::NoneAdmissible`] instead of describing it. The absent variant
/// is what makes that refusal the only expressible outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactValidity {
    /// Within the caller's staleness budget.
    Fresh,
    /// Past the caller's staleness budget, and served anyway — the boundary
    /// reports staleness rather than swallowing it, and so does this.
    Stale,
    /// The caller supplied no budget, so the world made no freshness judgement.
    ///
    /// Reported rather than silently treated as fresh: `Timeless` is a positive
    /// claim that age does not matter, and for a fact like "last seen at" that
    /// claim is false. Surfacing it is what lets a consumer notice.
    Timeless,
}

/// The bundle of hints that crosses the seam.
///
/// Private field with an accessor, following the `WorldAnswer` precedent: a
/// public `Vec` would let a caller mutate the context in place and attribute the
/// result to Kirra World.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProposalContext {
    hints: Vec<ContextHint>,
    silence: Option<WorldSilence>,
}

impl ProposalContext {
    /// An empty context — what a proposal producer sees when Kirra World is
    /// unavailable or knows nothing relevant. Distinct from "no opinion about
    /// this candidate": empty means the seam carried nothing at all.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            hints: Vec::new(),
            silence: None,
        }
    }

    /// The candidates unchanged, no preference, and the REASON the world
    /// expressed none.
    fn silent(candidates: &[ContextId], silence: WorldSilence) -> Self {
        Self {
            hints: vec![ContextHint::CandidatePriority(candidates.to_vec())],
            silence: Some(silence),
        }
    }

    /// Why the world expressed no preference, when it expressed none.
    ///
    /// `None` here means the world DID express a preference — not that it was
    /// silent for an unrecorded reason. The three silences stay distinct because
    /// collapsing them is precisely the information loss Tier 3 exists to
    /// prevent.
    #[must_use]
    pub fn silence(&self) -> Option<&WorldSilence> {
        self.silence.as_ref()
    }

    /// The trust grade of the fact this context acted on, if it acted on one.
    #[must_use]
    pub fn fact_trust(&self) -> Option<FactGrade> {
        self.hints.iter().find_map(|h| match h {
            ContextHint::FactTrust(g) => Some(*g),
            _ => None,
        })
    }

    /// The freshness of the fact this context acted on, if it acted on one.
    #[must_use]
    pub fn fact_freshness(&self) -> Option<FactValidity> {
        self.hints.iter().find_map(|h| match h {
            ContextHint::FactFreshness(v) => Some(*v),
            _ => None,
        })
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
    /// The answer boundary could not serve the question.
    Ask(AskError),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ask(e) => write!(f, "world read failed: {e}"),
        }
    }
}

impl std::error::Error for ContextError {}

impl From<AskError> for ContextError {
    fn from(e: AskError) -> Self {
        Self::Ask(e)
    }
}

/// Why the world had nothing usable to say — PRESERVED, not flattened.
///
/// Before this, both reasons collapsed into "no preference", which loses exactly
/// what Tier 3 exists to retain: *never heard of it* and *heard of it but not
/// servable* are different facts about the world, and a consumer that cannot
/// tell them apart cannot report which one it hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldSilence {
    /// No current claim for this subject at this clock.
    NoClaim,
    /// Claims exist, but none was admissible at this clock and budget.
    NoneAdmissible,
    /// A claim exists and is admissible, but its object is not among the
    /// candidates the caller offered. The world spoke; it just did not name
    /// anything on the menu.
    NoCandidateMatched,
    /// **A claim named this relation, and its object could not be resolved to
    /// something safe to compare** — box 3c's fail-closed arm.
    ///
    /// Distinct from [`Self::NoCandidateMatched`], which says the world named
    /// something not on the menu. This says the world named something and the
    /// identity graph could not tell us what it was: behind the log, recording a
    /// contradiction, or resolving the object to more than one entity.
    ObjectUnresolved(ObjectResolution),
}

/// Why a claim's object could not be turned into something safe to compare.
///
/// **A mirror of the world's `ObjectIdentity`, not a re-export**, for the reason
/// [`FactGrade`] is a mirror — and here it is load-bearing rather than
/// stylistic. The world's type carries `Resolved { hops: usize }`, and its
/// refusal reasons carry `TraversalBudgetExceeded { limit: usize }`.
/// Re-exporting it would put primitive numerics on this crate's public surface
/// and give the symbolic seam somewhere to put a number, which is the one thing
/// the seam rule exists to prevent. Those numbers are real and useful — they are
/// simply not this seam's to carry, and they remain available at the answer
/// boundary where an operator reads them.
///
/// Successors ARE carried: an entity id is a symbol, and naming which entities
/// an object turned out to be is the difference between a usable report and
/// "something went wrong".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectResolution {
    /// The identity graph has not consumed every adjudication the log holds, so
    /// any resolution would be known-stale.
    GraphStale,
    /// The object resolved to more than one live entity, and picking one would
    /// be a guess.
    Ambiguous {
        /// The entities the object turned out to be, as the events name them.
        successors: Vec<ContextId>,
    },
    /// The recorded history contradicts itself.
    ///
    /// CATEGORICAL — a named reason, never a magnitude, so the seam rule is
    /// unaffected.
    Contradictory(&'static str),
}

impl ObjectResolution {
    /// The categorical tag for one of the world's refusal reasons.
    ///
    /// Public so the lock-step test can walk the real enum and assert the tags
    /// are total and pairwise distinct. Exhaustive on purpose: adding a variant
    /// to `RefusalReason` must break this match rather than fall into a
    /// catch-all that silently reports the new reason as an old one — and the
    /// test catches the half a compiler cannot, two variants quietly sharing one
    /// tag, which compiles and then misreports forever.
    #[must_use]
    pub fn contradiction_tag(reason: &RefusalReason) -> &'static str {
        match reason {
            RefusalReason::RedirectCycle { .. } => "redirect_cycle",
            RefusalReason::TraversalBudgetExceeded { .. } => "traversal_budget_exceeded",
            RefusalReason::DanglingRedirect { .. } => "dangling_redirect",
            RefusalReason::EmptySupersession { .. } => "empty_supersession",
            RefusalReason::ContradictoryHistory { .. } => "contradictory_history",
        }
    }
}

/// Translate the world's `ObjectIdentity` into this seam's symbolic mirror.
///
/// Only the non-matchable variants can reach here — `ObjectIdentity::matchable`
/// has already returned `Some` for the rest — so the matchable arms map to a
/// nearest honest report rather than being unreachable-panicked. A wrong report
/// is recoverable; a panic in a proposal producer is not.
fn mirror_resolution(identity: &ObjectIdentity) -> ObjectResolution {
    match identity {
        ObjectIdentity::Unresolvable => ObjectResolution::GraphStale,
        ObjectIdentity::Ambiguous { successors } => ObjectResolution::Ambiguous {
            successors: successors.iter().filter_map(ContextId::new).collect(),
        },
        ObjectIdentity::Refused(reason) => {
            ObjectResolution::Contradictory(ObjectResolution::contradiction_tag(reason))
        }
        ObjectIdentity::NoObject
        | ObjectIdentity::Malformed
        | ObjectIdentity::NotAnEntity
        | ObjectIdentity::Resolved { .. } => ObjectResolution::Contradictory("unclassified"),
    }
}

/// Derive proposal-shaping context from what Kirra World knows about `subject`.
///
/// The one behaviour, kept deliberately tiny: if the world holds a claim
/// `subject --relation--> X` and `X` is among `candidates`, the returned context
/// prefers `X` and reorders the candidate list to put it first. Otherwise the
/// context carries the candidates in the order given and expresses no preference
/// — with [`ProposalContext::silence`] recording WHY, rather than flattening
/// three different reasons into one.
///
/// # The freshness contract is required, not defaulted
///
/// `staleness_budget_ms` is a parameter with no default because
/// `Validity::Timeless` — what the world returns when no budget is supplied — is
/// a positive claim that the fact's age does not matter. For "where was this
/// last seen", that claim is false. Tier 3 box 3e's *"no implicit default for
/// recency-sensitive semantics"* is enforced here by the signature: a caller
/// cannot avoid deciding. `None` is still expressible, and it means *I have
/// considered this and this fact is genuinely timeless*, which is a different
/// act from never having been asked.
///
/// # What this reads, and what it does not
///
/// It goes through the sanctioned answer boundary — `WorldView::ask` — so every
/// value it sees arrives with validity, trust axes, grade and provenance
/// attached. It does NOT touch `ProjectedClaim`'s public fields, which was the
/// 3a defect this migration closes.
///
/// It also does not resolve the object through the identity graph, even though
/// the object names an entity. That composes two projections with independent
/// checkpoints — 3c's question, and not one an answer this small may decide
/// silently.
pub fn mission_context(
    store: &WorldStore,
    subject: &ContextId,
    relation: &ContextId,
    candidates: &[ContextId],
    now_ms: i64,
    staleness_budget_ms: Option<u64>,
) -> Result<ProposalContext, ContextError> {
    let view = WorldView::new(store, staleness_budget_ms);

    let answers = match view.ask(subject.as_str(), now_ms)?.into_lookup() {
        WorldLookup::Answered(answers) => answers,
        WorldLookup::Unknown(reason) => {
            let silence = match reason {
                UnknownReason::NoClaim => WorldSilence::NoClaim,
                UnknownReason::NoneAdmissible => WorldSilence::NoneAdmissible,
            };
            return Ok(ProposalContext::silent(candidates, silence));
        }
    };

    // Set when a claim named this relation but its object could not be turned
    // into something safe to compare. Tracked separately from "nothing matched"
    // because they are different facts: the world DID answer, and the answer
    // could not be used.
    let mut unresolved: Option<ObjectResolution> = None;

    let matched = answers.iter().find_map(|a| {
        if a.predicate()? != relation.as_str() {
            return None;
        }
        // The OBJECT, not the payload: the fact is a relationship, and the
        // object is what it points at — resolved through the identity graph,
        // because an object that names an entity is not a string. `matchable`
        // is what makes the fail-closed cases fail closed; comparing the raw
        // object here would silently ignore every merge the world has recorded.
        let Some(object) = a.object_identity().matchable(a.object()) else {
            unresolved.get_or_insert_with(|| mirror_resolution(a.object_identity()));
            return None;
        };
        let candidate = candidates.iter().find(|c| c.as_str() == object)?;
        Some((candidate, a))
    });

    if matched.is_none() {
        if let Some(identity) = unresolved {
            return Ok(ProposalContext::silent(
                candidates,
                WorldSilence::ObjectUnresolved(identity),
            ));
        }
    }

    let Some((preferred, answer)) = matched else {
        return Ok(ProposalContext::silent(
            candidates,
            WorldSilence::NoCandidateMatched,
        ));
    };

    let mut priority = Vec::with_capacity(candidates.len());
    priority.push(preferred.clone());
    priority.extend(candidates.iter().filter(|c| *c != preferred).cloned());

    // Both mappings below FAIL CLOSED on the one state that has no honest
    // symbol, rather than reaching for the nearest one.
    //
    // Neither arm can be reached today: `WorldView::is_admissible` refuses
    // `Expired` and `Inadmissible` before an answer is ever built, and the
    // filters that guarantee it are pinned by
    // `kirra-world-store/tests/inadmissible_never_read.rs`. They are here
    // because that guarantee is emergent — it composes out of three mechanisms
    // added for unrelated reasons — and *"cannot happen"* is exactly when a
    // mapping gets written carelessly.
    //
    // An earlier draft folded `Expired` into `Timeless` while its own comment
    // said doing so would be a lie. It would have been: `Timeless` is a positive
    // claim that age does not matter, which is the strongest possible
    // misstatement about a fact that has run out. There is no honest
    // `FactValidity` for an expired fact — it is not fresh, not stale, and
    // emphatically not time-independent — so the correct move is not to label it
    // but to REFUSE it. The world's own `trust_grade` already rules an expired
    // claim `Inadmissible`, so refusing here agrees with the world rather than
    // inventing a policy.
    let grade = match answer.grade() {
        Some(TrustGrade::Strong) => FactGrade::Graded("strong"),
        Some(TrustGrade::Adequate) => FactGrade::Graded("adequate"),
        Some(TrustGrade::Weak) => FactGrade::Graded("weak"),
        Some(TrustGrade::Inadmissible) => {
            return Ok(ProposalContext::silent(
                candidates,
                WorldSilence::NoneAdmissible,
            ))
        }
        None => FactGrade::Ungraded,
    };
    let freshness = match answer.validity() {
        Validity::Fresh => FactValidity::Fresh,
        Validity::Stale => FactValidity::Stale,
        Validity::Timeless => FactValidity::Timeless,
        Validity::Expired => {
            return Ok(ProposalContext::silent(
                candidates,
                WorldSilence::NoneAdmissible,
            ))
        }
    };

    Ok(ProposalContext {
        hints: vec![
            ContextHint::PreferDestination(preferred.clone()),
            ContextHint::CandidatePriority(priority),
            ContextHint::MissionFact {
                subject: subject.clone(),
                relation: relation.clone(),
                object: preferred.clone(),
            },
            ContextHint::FactTrust(grade),
            ContextHint::FactFreshness(freshness),
        ],
        silence: None,
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
            silence: None,
        };
        assert_eq!(c.preferred_destination(), Some(&dock_b));
        assert_eq!(c.candidate_priority(), Some([dock_b, dock_a].as_slice()));
        assert!(!c.is_empty());
    }
}
