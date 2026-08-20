//! **Tier 2 box 2a — the production candidate-write path.**
//!
//! `KIRRA-WM-PROMOTION-001`: *clustering may PROPOSE co-reference; it may never
//! CONFIRM identity.* This crate is the propose half, running for real.
//!
//! # Why this crate exists at all
//!
//! Kirra World reached a mature read side — projections, as-of resolution,
//! provenance walks, explanations, retention — while having **no production
//! writer of any kind**. Every event in every test was test-authored. That was
//! found by tracing one `same_as` relationship from producer to projection and
//! discovering the chain broke at its first link, not its last.
//!
//! So this is deliberately the narrowest possible first write path: one rule,
//! one predicate, one claim status.
//!
//! # The shape: a pure proposer, a thin pass
//!
//! [`propose_from_claims`] is a pure function of the evidence it is given. It
//! reads nothing and writes nothing, so what a matcher WOULD propose is
//! testable without a store — and the tests that matter (all-pairs, the group
//! ceiling, the identifier rule) are written against it directly.
//!
//! [`run_ingest_pass`] is the thin part: survey, propose, write, report. The
//! same split as the retention sweeper's decide-then-act, for the same reason —
//! the judgement is the part worth testing exhaustively, and it should not need
//! a database to exercise.
//!
//! # It cannot confirm, structurally
//!
//! The only door this crate uses is
//! [`WorldStore::append_same_as_candidate`](kirra_world_store::WorldStore::append_same_as_candidate),
//! which takes no `writer_class` and no `claim_status` from its caller. There is
//! no argument here that could carry `Confirmed`, so "the matcher does not
//! self-confirm" is not a property of this code's care — it is a property of the
//! only API it can reach. Schema v8's trigger is the independent second half,
//! covering a producer written later against the same store.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use kirra_world::observation::{Confidence, ConfidenceBasis};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world::same_as_candidate::{
    CandidateError, CandidatePair, MatcherIdentity, SameAsCandidate,
};
use kirra_world_store::{candidate_record::CandidateRow, StoreError, WorldStore};

/// The most subjects an identifier value may group before the rule refuses it.
///
/// **A structural bound, not a post-hoc truncation.** All-pairs over a group of
/// `n` is `n(n-1)/2` proposals, so an unbounded group makes one pass's write
/// volume quadratic in how many entities happen to share a value.
///
/// The ceiling is also semantically right, which is why it refuses rather than
/// trims. A "strong identifier" shared by more than a few dozen entities is not
/// functioning as an identifier — it is a default, a placeholder, or a parse
/// failure (`"unknown"`, `""`, `"N/A"`). Proposing 500 co-reference candidates
/// from one such value would be worse than proposing none, because each would
/// carry the same honest-looking provenance as a real match.
///
/// An over-large group is REPORTED
/// ([`IngestPassReport::oversized_groups`]) rather than silently skipped: a
/// dropped group is a finding about the identifier, and hiding it would make a
/// misconfigured rule look like a quiet one.
pub const MAX_IDENTIFIER_GROUP: usize = 32;

/// Why an ingest pass could not run, or a proposal could not be built.
#[derive(Debug)]
pub enum IngestError {
    /// The store refused a read or a write.
    Store(StoreError),
    /// The domain refused a candidate this rule tried to build.
    ///
    /// Propagated rather than skipped: the rule builds pairs from values it read
    /// out of the store, so a refusal means the store holds something the rule's
    /// assumptions do not cover, and continuing would paper over it.
    Candidate(CandidateError),
    /// An entity id in the projection is not one the domain admits.
    ///
    /// Should be unreachable — ids are validated on the way in — so it is a
    /// signal that something wrote around the domain, not a routine outcome.
    Reference(String),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "store: {e}"),
            Self::Candidate(e) => write!(f, "candidate: {e}"),
            Self::Reference(d) => write!(f, "inadmissible id in the projection: {d}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<StoreError> for IngestError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<CandidateError> for IngestError {
    fn from(e: CandidateError) -> Self {
        Self::Candidate(e)
    }
}

/// The rule this producer runs: **exact agreement on one strong identifier**.
///
/// Deliberately not a learned matcher. Box 2a's job is to establish the write
/// PATH — writer identity, provenance, versioning, the citable observation — and
/// a model would make the first production writer's correctness a question about
/// the model instead of about the path. `MatcherIdentity` already carries
/// `model_or_rule` precisely so a rule is a first-class producer rather than a
/// placeholder for one.
#[derive(Debug, Clone)]
pub struct ExactIdentifierRule {
    predicate: String,
    matcher: MatcherIdentity,
}

impl ExactIdentifierRule {
    /// Build the rule for one identifying predicate.
    ///
    /// # Errors
    ///
    /// [`CandidateError`] if the matcher identity is incomplete — every part of
    /// *"which version of what proposed this"* is required.
    pub fn new(
        identifying_predicate: impl Into<String>,
        matcher: MatcherIdentity,
    ) -> Result<Self, CandidateError> {
        Ok(Self {
            predicate: identifying_predicate.into(),
            matcher,
        })
    }

    /// The predicate whose agreement this rule treats as evidence.
    #[must_use]
    pub fn predicate(&self) -> &str {
        &self.predicate
    }

    /// Who is proposing, and with which version of what.
    #[must_use]
    pub fn matcher(&self) -> &MatcherIdentity {
        &self.matcher
    }

    /// The confidence every proposal from this rule carries.
    ///
    /// **No score, basis `Unspecified`** — and that is the honest encoding, not
    /// an unfinished one. An exact-match rule does not compute a probability.
    /// §7.3 is explicit that the design *"must not force producers to invent
    /// precision they do not have"*, and a rule that stamped `0.95` on every
    /// proposal would be doing exactly that, in a field an adjudicator would
    /// later read as if it meant something.
    ///
    /// If a rule ever earns a calibrated score, it gets
    /// [`ConfidenceBasis::ModelScore`] and a [`CalibrationRef`] to back it —
    /// which is a different producer, with a different version.
    ///
    /// [`CalibrationRef`]: kirra_world::observation::CalibrationRef
    #[must_use]
    pub fn confidence(&self) -> Confidence {
        // `expect`: a `None` score cannot be out of range or non-finite, so the
        // only two failures `Confidence::new` has are both unreachable for this
        // argument. Not a fallible call dressed as an infallible one.
        Confidence::new(None, ConfidenceBasis::Unspecified, None)
            .expect("a scoreless confidence has nothing to reject")
    }
}

/// One row of surveyed evidence: a subject, the identifier value it carries, and
/// the **real** observation id of the row that carried it.
///
/// The third field is the reason this type exists rather than reusing
/// `ProjectedClaim`. A candidate's support list is cited by `ObservationId`
/// (`KIRRA-WM-PROMOTION-001`), and the current-state projection does not carry
/// one — a rule surveying it would have to reconstruct an id, which is
/// fabrication wearing provenance's clothes. So the survey reads the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreement {
    /// The entity carrying the value.
    pub subject: String,
    /// The identifier value.
    pub value: String,
    /// The observation that recorded it — the citable handle.
    pub observation_id: String,
}

/// One identifier value and the agreements that share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierGroup {
    /// The shared value.
    pub value: String,
    /// The agreeing rows, in the order the evidence presented them.
    pub members: Vec<Agreement>,
}

/// What one pass did, and what it declined to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestPassReport {
    /// Candidates written this pass.
    pub proposed: usize,
    /// Pairs skipped because this matcher version had already proposed them.
    pub already_proposed: usize,
    /// Groups refused for exceeding [`MAX_IDENTIFIER_GROUP`], with their sizes.
    ///
    /// Carried rather than logged so a caller can act on it. An identifier that
    /// groups too widely is a configuration finding, and a pass that silently
    /// dropped it would read as a pass that found nothing.
    pub oversized_groups: Vec<(String, usize)>,
    /// Whether the evidence survey hit its ceiling.
    ///
    /// `true` means the pass saw a PREFIX of the agreements, so its output is
    /// incomplete rather than final. Reported instead of inferred, because
    /// "found nothing more" and "stopped looking" are the same empty result to a
    /// caller that cannot tell them apart.
    pub survey_truncated: bool,
}

/// Group surveyed agreements by value.
///
/// A subject is recorded once per value however many rows repeat it — a subject
/// that restates a value has not corroborated itself. The FIRST observation of
/// that agreement is the one kept, so the citation names the evidence that
/// established the agreement rather than the most recent restatement of it.
#[must_use]
pub fn group_by_identifier(agreements: &[Agreement]) -> Vec<IdentifierGroup> {
    let mut groups: Vec<IdentifierGroup> = Vec::new();
    for a in agreements {
        match groups.iter_mut().find(|g| g.value == a.value) {
            Some(g) => {
                if !g.members.iter().any(|m| m.subject == a.subject) {
                    g.members.push(a.clone());
                }
            }
            None => groups.push(IdentifierGroup {
                value: a.value.clone(),
                members: vec![a.clone()],
            }),
        }
    }
    groups
}

/// What a rule would propose, and what it refused to propose from.
///
/// A named pair rather than a tuple: the two halves mean opposite things — one
/// is output, the other is a finding about the input — and a caller destructuring
/// positionally has nothing stopping it from treating the refusals as results.
#[derive(Debug, Clone, Default)]
pub struct Proposals {
    /// The candidates this rule proposes.
    pub candidates: Vec<SameAsCandidate>,
    /// Identifier values refused for grouping too widely, with their sizes.
    pub oversized_groups: Vec<(String, usize)>,
}

/// What this rule would propose from the surveyed evidence — pure.
///
/// # All pairs, not a spanning chain
///
/// For a group `{a, b, c}` this emits `(a,b)`, `(a,c)` and `(b,c)`, not a chain
/// of two. The extra pair is not redundancy: `KIRRA-WM-TRANSITIVITY-001` holds
/// that evidence is pairwise and **never transitively closed**, so a promoted
/// `a=b` and `b=c` do not yield `a=c`. A chain-emitting matcher would therefore
/// make the final identity depend on a closure the ruling forbids anyone to
/// compute — and would do it invisibly, by omission.
///
/// Every pair here IS directly evidenced: all three subjects carry the same
/// identifier value, so `(a,c)` rests on the same agreement `(a,b)` does. The
/// matcher proposes what it observed; it does not infer.
///
/// # Errors
///
/// [`IngestError`] if an id in the evidence is inadmissible, or the domain
/// refuses a pair this rule built.
pub fn propose_from_agreements(
    rule: &ExactIdentifierRule,
    agreements: &[Agreement],
) -> Result<Proposals, IngestError> {
    let mut candidates = Vec::new();
    let mut oversized = Vec::new();

    for group in group_by_identifier(agreements) {
        if group.members.len() > MAX_IDENTIFIER_GROUP {
            oversized.push((group.value, group.members.len()));
            continue;
        }
        for (i, a) in group.members.iter().enumerate() {
            for b in group.members.iter().skip(i + 1) {
                let pair = CandidatePair::new(entity(&a.subject)?, entity(&b.subject)?)?;
                // BOTH sides are cited. The evidence for co-reference is the
                // agreement, and an agreement has two halves -- a candidate
                // citing only one would name evidence that does not establish
                // the thing it claims.
                let support = vec![
                    observation(&a.observation_id)?,
                    observation(&b.observation_id)?,
                ];
                candidates.push(SameAsCandidate::propose(
                    pair,
                    rule.matcher().clone(),
                    rule.confidence(),
                    support,
                )?);
            }
        }
    }
    Ok(Proposals {
        candidates,
        oversized_groups: oversized,
    })
}

fn observation(id: &str) -> Result<ObservationId, IngestError> {
    ObservationId::new(id).map_err(|e| IngestError::Reference(format!("{e}")))
}

fn entity(id: &str) -> Result<EntityId, IngestError> {
    EntityId::new(id).map_err(|e| IngestError::Reference(format!("{e}")))
}

/// Run one production ingest pass against a real store.
///
/// Survey (the evidence log) → propose (pure) → write (the sanctioned door) →
/// report. The write is the only mutating step and it is the one door that
/// cannot express a confirmed claim.
///
/// `survey_limit` bounds the evidence read in SQL. A pass that fills it reports
/// `survey_truncated`, because a caller cannot otherwise tell a complete pass
/// from one that stopped early — and those two empty results mean opposite
/// things.
///
/// # Idempotence
///
/// A pair this matcher VERSION has already proposed is skipped rather than
/// re-proposed. Without that, every pass would append the whole proposal set
/// again and the log would grow with pass count rather than with evidence.
///
/// Keyed on the matcher's version deliberately: a NEW version re-proposing a
/// pair is a different claim — a different rule looked at the same evidence —
/// and collapsing the two would erase which version's judgement is on record.
///
/// # Errors
///
/// [`IngestError`] if the survey, the proposal or a write fails. A failed write
/// aborts the pass rather than continuing, so `proposed` never over-reports.
pub fn run_ingest_pass(
    store: &mut WorldStore,
    rule: &ExactIdentifierRule,
    now_ms: i64,
    survey_limit: usize,
    mut mint: impl FnMut() -> (EventId, ObservationId),
) -> Result<IngestPassReport, IngestError> {
    let rows = store.identifier_agreements(rule.predicate(), survey_limit)?;
    let survey_truncated = rows.len() == survey_limit;
    let agreements: Vec<Agreement> = rows
        .into_iter()
        .map(|(subject, value, observation_id)| Agreement {
            subject,
            value,
            observation_id,
        })
        .collect();

    let proposed = propose_from_agreements(rule, &agreements)?;

    let mut report = IngestPassReport {
        oversized_groups: proposed.oversized_groups,
        survey_truncated,
        ..IngestPassReport::default()
    };

    for candidate in proposed.candidates {
        if store.same_as_candidate_exists(
            candidate.pair().low().as_str(),
            candidate.pair().high().as_str(),
            rule.matcher().version(),
        )? {
            report.already_proposed += 1;
            continue;
        }
        let (event_id, observation_id) = mint();
        store.append_same_as_candidate(
            &CandidateRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: now_ms,
                valid_from_ms: now_ms,
                source: rule.matcher().producer(),
                source_version: rule.matcher().version(),
            },
            &candidate,
        )?;
        report.proposed += 1;
    }
    Ok(report)
}
