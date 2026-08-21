//! **The relationship view — World-side semantics for the read-only endpoint.**
//!
//! ```text
//! relationships_projection
//!       ↓  QueryEngine::execute(Related { .. })      the SANCTIONED query
//! THIS MODULE                                        the projection to neutral
//!       ↓  RelationsView                             the wire contract
//! kirra-world-explain-service                        the PROCESS BOUNDARY
//! ```
//!
//! # Why the mapping lives here and not in the producer
//!
//! `ci/check_query_boundedness.py` rule 5 refuses a consumer that reaches past
//! the query engine, and the explain producer is a consumer by that gate's
//! definition. So every World-aware step — asking the engine, resolving
//! provenance, deciding which distinctions survive — happens behind this
//! boundary, and what is left in the producer is transport.
//!
//! That is the same split box 4c.1 arrived at for explanations, and it was not
//! a free choice there either: the gate forced it, and the result was better
//! than the version that had interpretation on both sides of a socket.
//!
//! # What crosses, and what cannot
//!
//! The output is [`RelationsView`], which lives in `kirra-explain-types` — the
//! crate `ci/check_explain_artifact_neutral.py` guards. It cannot name a World
//! type, so nothing on the wire is a handle into the store: no `AnswerRef`, no
//! cursor, no projection row. A consumer receives strings and a closed enum,
//! and there is nothing it could feed back to ask for more.

use kirra_explain_types::{ProvenanceStanding, RelatedPair, RelationsView};
use kirra_world::reference::EntityId;
use kirra_world_store::provenance_graph::{CitationResolution, DanglingReason};
use kirra_world_store::WorldStore;

use crate::freshness::FreshnessSource;
use crate::query::{QueryEngine, Related};
use crate::read_view::AskError;

/// Why a relationship view could not be produced.
#[derive(Debug)]
pub enum RelationsError {
    /// The subject is not an admissible entity identity.
    ///
    /// Kept apart from every other failure so the producer can answer
    /// `NotAnEntity` rather than `Unavailable`: *that is not a thing you can
    /// ask about* and *I could not answer* are different, and a caller told the
    /// second will retry forever.
    NotAnEntity {
        /// The domain constructor's reason.
        detail: String,
    },
    /// The World boundary could not serve the question.
    Ask(AskError),
}

impl std::fmt::Display for RelationsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnEntity { detail } => write!(f, "not an entity identity: {detail}"),
            Self::Ask(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RelationsError {}

impl From<AskError> for RelationsError {
    fn from(e: AskError) -> Self {
        Self::Ask(e)
    }
}

/// Map box 4b's citation resolution onto the wire's four cases.
///
/// The `Dangling` split is the load-bearing part and is NOT a severity
/// judgement: `PossiblyCompacted` becomes `Degraded` because
/// `KIRRA-WM-EVIDENCE-RETENTION-001` ruled that retention may take the
/// explanation while leaving the relation intact, and `NeverVisible` stays
/// `Dangling` because nothing was ever recorded there. ADR-0041 §11.3 forbids
/// collapsing those two, and a wire type with one "missing" case would collapse
/// them at the boundary no matter how carefully the store kept them apart.
///
/// Exhaustive with no wildcard: a new resolution variant is a compile error
/// here rather than silently taking whichever arm a `_` fell into.
#[must_use]
fn standing_of(resolution: &CitationResolution) -> ProvenanceStanding {
    match resolution {
        CitationResolution::Resolved { .. } => ProvenanceStanding::Resolved,
        CitationResolution::Plural { .. } => ProvenanceStanding::Plural,
        CitationResolution::Dangling { reason } => match reason {
            DanglingReason::PossiblyCompacted { .. } => ProvenanceStanding::Degraded,
            DanglingReason::NeverVisible => ProvenanceStanding::Dangling,
        },
    }
}

/// **What one subject is currently adjudicated the same as.**
///
/// # Errors
///
/// [`RelationsError::NotAnEntity`] if `subject` is not an admissible entity id
/// — refused rather than answered as "related to nothing", because a caller
/// told the latter concludes the entity exists. [`RelationsError::Ask`] for
/// anything the boundary raises.
pub fn current_relations(
    store: &WorldStore,
    subject: &str,
) -> Result<RelationsView, RelationsError> {
    let id = EntityId::new(subject).map_err(|e| RelationsError::NotAnEntity {
        detail: e.to_string(),
    })?;

    // Through the ONE sanctioned door. Not `store.related` directly: this crate
    // implements the boundary, so it *may* reach the store, and doing it anyway
    // would put a second answer next to the typed one and make rule 5 a rule
    // about other people's code.
    let engine = QueryEngine::new(store, FreshnessSource::Ruled);
    let answer = engine.execute(Related {
        entity: subject.to_owned(),
    })?;

    let mut related = Vec::with_capacity(answer.neighbours().len());
    for neighbour in answer.neighbours() {
        let pair = &neighbour.relationship.pair;
        // Provenance is asked PER PAIR and through the same bounded primitive
        // 5b classified. `Ok(None)` cannot arise here -- the pair came out of
        // the projection a moment ago -- but it is mapped rather than unwrapped
        // so a concurrent rebuild degrades to an honest `Dangling` instead of a
        // panic in a read-only endpoint.
        let standing = store
            .relationship_provenance(pair)
            .map_err(|e| RelationsError::Ask(AskError::Store(e)))?
            .as_ref()
            .map_or(ProvenanceStanding::Dangling, standing_of);

        related.push(RelatedPair {
            low: pair.low().as_str().to_owned(),
            high: pair.high().as_str().to_owned(),
            other: neighbour.other.as_str().to_owned(),
            adjudicator: neighbour.relationship.adjudicator.clone(),
            decision_marker: decision_marker(neighbour.relationship.decided_generation),
            provenance: standing,
        });
    }

    Ok(RelationsView {
        subject: id.as_str().to_owned(),
        related,
        truncated: answer.is_truncated(),
    })
}

/// Render a deciding log coordinate as the wire's opaque decision marker.
///
/// One function so the producer cannot invent a second spelling, and so the
/// "opaque" claim has a single place to be true. See
/// [`kirra_explain_types::RelatedPair::decision_marker`] for exactly what the
/// contract does and does not promise about it.
#[must_use]
fn decision_marker(generation: i64) -> String {
    format!("d-{generation}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **All four provenance cases map distinctly.**
    ///
    /// Two of them (`Resolved`, `Degraded`) are reachable end-to-end and are
    /// proven that way in the producer's tests. `Plural` and the
    /// `NeverVisible` half of dangling need a store shaped in ways an ordinary
    /// promotion cannot produce, so they are pinned here at the mapping instead
    /// — stated plainly rather than left as a gap someone discovers later.
    ///
    /// The load-bearing assertion is the LAST one: `PossiblyCompacted` and
    /// `NeverVisible` must not both become the same wire case. ADR-0041 §11.3
    /// forbids collapsing *whatever carried this was deleted* into *nothing
    /// ever carried this*, and a wire enum with one "missing" variant would do
    /// exactly that at the boundary, however carefully the store kept them
    /// apart upstream.
    #[test]
    fn every_citation_resolution_maps_to_a_distinct_standing() {
        assert_eq!(
            standing_of(&CitationResolution::Resolved {
                target_generation: 7
            }),
            ProvenanceStanding::Resolved
        );
        assert_eq!(
            standing_of(&CitationResolution::Plural {
                target_generations: vec![7, 9],
                truncated: false,
            }),
            ProvenanceStanding::Plural
        );
        let compacted = standing_of(&CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted {
                spans: vec![3],
                truncated: false,
            },
        });
        let never = standing_of(&CitationResolution::Dangling {
            reason: DanglingReason::NeverVisible,
        });
        assert_eq!(compacted, ProvenanceStanding::Degraded);
        assert_eq!(never, ProvenanceStanding::Dangling);
        assert_ne!(
            compacted, never,
            "compacted evidence and never-recorded evidence must not collapse \
             into one wire case"
        );
    }

    /// The marker is opaque in the one sense the contract claims: it is not
    /// the bare number, so a consumer cannot mistake it for an index and no
    /// route accepts it back.
    #[test]
    fn a_decision_marker_is_not_a_bare_generation() {
        let m = decision_marker(41);
        assert_ne!(m, "41");
        assert!(
            m.contains("41"),
            "it must still correlate with the record: {m}"
        );
    }
}
