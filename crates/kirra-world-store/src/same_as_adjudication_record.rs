//! Persisting a `same_as` **adjudication** — `WM_SCOPE.md` §5 box 2b.
//!
//! `KIRRA-WM-PROMOTION-001`: confirmed identity arrives only through an
//! **explicitly authorized adjudicator**, and v1 is `SourceClass::Operator`
//! only.
//!
//! # Where the authority rule is actually enforced
//!
//! In the TYPE, not here. `AdjudicationAuthority::new` refuses every class but
//! `Operator`, and the struct does not even store one — `class()` returns the
//! constant. An unauthorized adjudicator is therefore unrepresentable, and this
//! module deliberately does NOT re-check it: a comparison that can only ever be
//! `false` reads as the enforcement and stops a reader looking for the real one.
//! (An earlier draft of this module carried exactly that dead check, and its own
//! test found it.)
//!
//! # What box 2b actually closed
//!
//! Adjudication used to accept a `&SameAsCandidate` — a value the caller
//! constructed. That let an adjudicator judge something that merely LOOKED like
//! evidence, which is the opposite of resting confirmed identity on recorded
//! evidence. The API now takes the persisted candidate's `ObservationId` and the
//! store does the loading, so an in-memory candidate cannot enter at all.
//!
//! # Why the predicate is not `same_as`
//!
//! A row saying `(low, same_as, high)` at `claim_status = confirmed` would read
//! as *"these are the same"*. That is not what this row is. This row says *"an
//! authorized adjudicator reached a decision about that pair"* — and the
//! decision may be `Rejected`. Storing a rejection under the `same_as`
//! predicate would make the log assert the very thing the operator refused.
//!
//! So the predicate is [`ADJUDICATION_PREDICATE_TOKEN`], and the outcome lives
//! in the payload. Both entities stay in indexed columns, which is what lets a
//! reader find every decision about a pair without parsing payloads.

use kirra_world::observation::SourceClass;
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome, SameAsAdjudication};
use kirra_world::same_as_candidate::CandidatePair;

/// The `world_events.kind` token every `same_as` adjudication row carries.
pub const SAME_AS_ADJUDICATION_KIND: &str = "same_as_adjudication";

/// The `world_events.predicate` token every adjudication row carries.
///
/// **Not `same_as`** — see the module docs. This names the RELATION between the
/// row and the pair (*a decision was reached about it*), not the relation
/// between the two entities.
pub const ADJUDICATION_PREDICATE_TOKEN: &str = "same_as_adjudged";

/// The `world_events.retention_class` every adjudication row carries.
///
/// `"adjudication"`, and NOT the `"raw"` its candidate carries.
/// `crate::compaction::is_protected` holds for every class except `"raw"`, so
/// this is what keeps a decision from being compacted away.
///
/// The asymmetry with [`crate::candidate_record::CANDIDATE_RETENTION_CLASS`] is
/// the point rather than an inconsistency. A matcher proposes continuously and
/// most proposals are never judged; an *authorized adjudicator's decision* is
/// rare, deliberate, and the thing §6.3's "a merged id stays resolvable forever"
/// ultimately rests on.
pub const SAME_AS_ADJUDICATION_RETENTION_CLASS: &str = "adjudication";

/// Version of the payload encoding below.
pub const SAME_AS_ADJUDICATION_PAYLOAD_SCHEMA: i64 = 1;

/// The stored spelling of an outcome.
///
/// An exhaustive `match` with no wildcard: a new [`Outcome`] variant must be a
/// COMPILE error here rather than a row storing a decision this encoder never
/// considered.
#[must_use]
pub fn outcome_token(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Promoted => "promoted",
        Outcome::Rejected => "rejected",
        Outcome::Unresolved => "unresolved",
    }
}

/// The inverse of [`outcome_token`].
///
/// # Errors
///
/// Returns `None` for a token this build does not know — refused by the caller
/// rather than defaulted, because guessing which decision an operator made is
/// worse than admitting the row cannot be read.
#[must_use]
pub fn outcome_from_token(token: &str) -> Option<Outcome> {
    match token {
        "promoted" => Some(Outcome::Promoted),
        "rejected" => Some(Outcome::Rejected),
        "unresolved" => Some(Outcome::Unresolved),
        _ => None,
    }
}

/// The stored spelling of an authority's source class.
#[must_use]
pub fn source_class_token(class: SourceClass) -> &'static str {
    match class {
        SourceClass::Sensor => "sensor",
        SourceClass::Operator => "operator",
        SourceClass::Configuration => "configuration",
        SourceClass::Import => "import",
        SourceClass::Derivation => "derivation",
        SourceClass::Network => "network",
    }
}

/// Encode an adjudication's payload.
///
/// The pair is NOT in here — it travels in `subject`/`object`, where an index
/// can see it. The evidence is not here either: the judged candidate and the
/// cited observations are the row's `provenance`, because box 4a's citation
/// index is built from that column and a decision whose evidence lived only in
/// its payload would be invisible to the provenance walk.
///
/// `candidate_observation_id` DOES appear here as well, and the duplication is
/// deliberate: `provenance` is a flat list of ids, so it records *that* the
/// candidate was cited but not that it was the SUBJECT of the decision rather
/// than one input among several. The payload keeps that distinction; the
/// provenance column keeps the walk.
#[must_use]
pub fn encode_same_as_adjudication(a: &SameAsAdjudication) -> String {
    serde_json::json!({
        "outcome": outcome_token(a.outcome()),
        "candidate_observation_id": a.candidate_observation_id().as_str(),
        "authority": {
            "source_class": source_class_token(a.authority().class()),
            "identity": a.authority().adjudicator(),
        },
        "decided_at": {
            "ms": a.decided_at().ms,
            "domain": match a.decided_at().domain {
                kirra_world::observation::ClockDomain::Boundary => "boundary",
                kirra_world::observation::ClockDomain::System => "system",
            },
        },
    })
    .to_string()
}

/// The observation ids an adjudication row records as its `provenance`.
///
/// **The judged candidate comes first, then the cited observations.**
///
/// An earlier version returned only `cited()`, which left the
/// `candidate_observation_id` — the single most important citation, the thing
/// the decision is ABOUT — reachable only by parsing the payload. That
/// contradicted this module's own stated reason for putting citations in
/// `provenance`: box 4a's index is built from that column, so a provenance walk
/// from a decision could not reach the proposal it judged.
///
/// Deduplicated, because a caller may legitimately also name the candidate in
/// `cited` and the citation index keys on `(generation, ordinal)` — one
/// observation appearing twice would make one piece of evidence look like two,
/// the same defect `SameAsCandidate::propose` refuses for support lists.
#[must_use]
pub fn same_as_adjudication_provenance(a: &SameAsAdjudication) -> Vec<String> {
    let mut out = vec![a.candidate_observation_id().as_str().to_owned()];
    for o in a.cited() {
        let id = o.as_str().to_owned();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// What a caller must supply to adjudicate a persisted candidate.
///
/// Note what is **absent**: there is no candidate value and no pair. Both are
/// derived by the store from the row `candidate_observation_id` names, which is
/// what makes "adjudication acts on persisted evidence" a property of this
/// type rather than of the caller's discipline.
#[derive(Debug, Clone)]
pub struct SameAsAdjudicationRequest<'a> {
    /// This decision's identity in the log.
    pub event_id: &'a EventId,
    /// This decision's own observation identity.
    pub observation_id: &'a ObservationId,
    /// **The persisted candidate being judged.** The store loads it.
    pub candidate_observation_id: &'a str,
    /// The observations this decision rests on.
    pub cited: Vec<ObservationId>,
    /// Who decided.
    pub authority: AdjudicationAuthority,
    /// What they decided.
    pub outcome: Outcome,
    /// When they decided, on a named clock.
    pub decided_at: kirra_world::observation::DomainInstant,
    /// When the store learned it.
    pub txn_time_ms: i64,
    /// The operator console or tool that submitted it.
    pub source: &'a str,
    /// Its version.
    pub source_version: &'a str,
}

/// Why an adjudication could not be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicateError {
    /// No row in the log carries that observation id.
    ///
    /// Distinct from [`Self::NotACandidate`]: "there is no such evidence" and
    /// "that is not evidence of this kind" are different findings, and an
    /// adjudicator told the wrong one would look in the wrong place.
    NoSuchCandidate {
        /// The id that named nothing.
        observation_id: String,
    },
    /// The row exists but is not a derivation-class `same_as` candidate.
    NotACandidate {
        /// What disagreed, from the decoder.
        detail: String,
    },
    /// The domain refused the decision.
    Domain {
        /// The domain's own message.
        detail: String,
    },
}

impl std::fmt::Display for AdjudicateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchCandidate { observation_id } => write!(
                f,
                "no candidate observation `{observation_id}` in the log — \
                 an adjudication must judge evidence that exists"
            ),
            Self::NotACandidate { detail } => {
                write!(f, "that observation is not a same_as candidate: {detail}")
            }
            Self::Domain { detail } => write!(f, "decision refused: {detail}"),
        }
    }
}

impl std::error::Error for AdjudicateError {}

/// The pair a loaded candidate names, for the record's `subject`/`object`.
#[must_use]
pub fn adjudication_columns(pair: &CandidatePair) -> (String, String) {
    (
        pair.low().as_str().to_owned(),
        pair.high().as_str().to_owned(),
    )
}
