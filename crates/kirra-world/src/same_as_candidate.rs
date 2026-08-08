//! Tier 2 box **2a** — pairwise `same_as` candidate production.
//!
//! # The invariant
//!
//! > **A matcher may propose that two subjects refer to the same entity; it may
//! > not make that relationship true.**
//!
//! Everything here exists to make the second half unrepresentable rather than
//! merely discouraged. The rulings this implements:
//!
//! * `KIRRA-WM-CLUSTERING-001` — clustering may PROPOSE co-reference, never
//!   CONFIRM identity.
//! * `KIRRA-WM-TRANSITIVITY-001` — `same_as` evidence is pairwise and is never
//!   transitively closed at this layer.
//! * `KIRRA-WM-PROMOTION-001` — promotion requires an authorized adjudicator;
//!   `Corroboration(n)` counts CONFIRMED records per distinct relation, never
//!   matcher votes.
//! * `KIRRA-WM-CANDIDATE-ID-001` — a candidate identifier never enters the
//!   hashed durable record.
//!
//! # What is prohibited, and how
//!
//! | Prohibition | Mechanism |
//! |---|---|
//! | candidate → confirmed promotion | no claim-status field and no `promote`/`confirm` method exists; the type cannot spell it |
//! | transitive closure | [`proposed_relations`] returns only pairs that were *asserted*; there is no closure function to call |
//! | candidate participation in `Corroboration(n)` | [`SameAsCandidate::corroboration_contribution`] is `0`, permanently, with a test pinning it |
//! | matcher-vote counting as corroboration | [`distinct_relations`] collapses N candidates about one pair to ONE relation |
//! | deletion of rejected candidates | no removal API; candidates are values, and rejection is a separate record that CITES by [`ObservationId`] |
//! | in-memory cluster handed to adjudication | there is no cluster type — the durable artifact is the candidate observation, cited by id |
//!
//! # The durable artifact is the observation, not this struct
//!
//! [`SameAsCandidate`] is what a producer *builds* in order to write an
//! ordinary `Relationship` observation with [`Predicate::SameAs`],
//! `source_class = Derivation` and candidate claim semantics. It is not a second
//! identity-evidence channel and has no storage of its own.
//!
//! `KIRRA-WM-PROMOTION-001` fixes the seam: promotion cites candidates by
//! `ObservationId` through `Justification`, and a candidate identifier may not
//! enter the hashed record — so **an adjudicator is handed observation ids, not
//! this value**. That is why nothing here is `Hash`-keyed into a durable form.

use std::collections::BTreeSet;

use crate::observation::{Confidence, SourceClass};
use crate::reference::{EntityId, ObservationId};
use crate::relationship::Predicate;

/// The writer class a candidate producer always writes as.
///
/// Not caller-chosen. A matcher is a derivation over recorded evidence, and
/// `KIRRA-WM-PROMOTION-001` bars `derivation + confirmed` at the write door, so
/// pinning it here means the producer cannot select a class that would let it
/// confirm.
pub const CANDIDATE_SOURCE_CLASS: SourceClass = SourceClass::Derivation;

/// The predicate every candidate carries.
pub const CANDIDATE_PREDICATE: Predicate = Predicate::SameAs;

/// Why a candidate could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    /// Both sides named the same entity.
    ///
    /// A self-pair is not a weak candidate, it is a vacuous one: `a same_as a`
    /// is true of everything and would inflate any per-relation count with a
    /// fact nobody needed a matcher to discover.
    SelfPair,
    /// No supporting observations were cited.
    ///
    /// A candidate that names nothing it was computed from is an assertion
    /// wearing a derivation's clothes — the same defect
    /// [`crate::relationship::Origination`] makes unrepresentable for inferred
    /// relationships.
    NoSupportingEvidence,
    /// The same supporting observation was cited twice, which would let one
    /// piece of evidence look like two.
    DuplicateSupport,
    /// The producer name was empty or whitespace.
    EmptyProducer,
    /// The model or rule name was empty or whitespace.
    EmptyModel,
    /// The model or rule **version** was empty or whitespace.
    ///
    /// Required, not optional: "which model said this" is unanswerable without
    /// it, and an unanswerable provenance question defeats the purpose of
    /// recording provenance at all.
    EmptyVersion,
}

impl std::fmt::Display for CandidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfPair => write!(f, "a same_as candidate may not pair an entity with itself"),
            Self::NoSupportingEvidence => {
                write!(
                    f,
                    "a same_as candidate must cite the observations it was computed from"
                )
            }
            Self::DuplicateSupport => {
                write!(f, "a supporting observation was cited more than once")
            }
            Self::EmptyProducer => write!(f, "producer name was empty"),
            Self::EmptyModel => write!(f, "model or rule name was empty"),
            Self::EmptyVersion => write!(
                f,
                "model or rule version was empty (KIRRA-WM-PROMOTION-001)"
            ),
        }
    }
}

/// Who proposed a candidate, and with which version of what.
///
/// All three parts are required. A matcher whose output cannot be attributed to
/// a specific version is not auditable, and an unauditable candidate is exactly
/// the thing `KIRRA-WM-CLUSTERING-001` refused to let near a trust axis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MatcherIdentity {
    producer: String,
    model_or_rule: String,
    version: String,
}

impl MatcherIdentity {
    /// Build a matcher identity.
    ///
    /// # Errors
    ///
    /// [`CandidateError::EmptyProducer`], [`CandidateError::EmptyModel`] or
    /// [`CandidateError::EmptyVersion`] if the corresponding part is empty or
    /// whitespace.
    pub fn new(
        producer: impl Into<String>,
        model_or_rule: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, CandidateError> {
        let producer = producer.into();
        let model_or_rule = model_or_rule.into();
        let version = version.into();
        if producer.trim().is_empty() {
            return Err(CandidateError::EmptyProducer);
        }
        if model_or_rule.trim().is_empty() {
            return Err(CandidateError::EmptyModel);
        }
        if version.trim().is_empty() {
            return Err(CandidateError::EmptyVersion);
        }
        Ok(Self {
            producer,
            model_or_rule,
            version,
        })
    }

    /// Who produced it.
    #[must_use]
    pub fn producer(&self) -> &str {
        &self.producer
    }

    /// Which model or rule.
    #[must_use]
    pub fn model_or_rule(&self) -> &str {
        &self.model_or_rule
    }

    /// Which version of it.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// An unordered pair of distinct entities, held in a canonical order.
///
/// `same_as` is [`crate::relationship::Symmetry::Symmetric`], so `(a, b)` and
/// `(b, a)` are **the same relation**. Canonicalising at construction is what
/// makes `KIRRA-WM-PROMOTION-001`'s "counted per distinct relation, not per
/// record" implementable rather than aspirational: without it, a producer
/// emitting both spellings would look like two relations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidatePair {
    low: EntityId,
    high: EntityId,
}

impl CandidatePair {
    /// Build a canonical pair.
    ///
    /// # Errors
    ///
    /// [`CandidateError::SelfPair`] if both sides are the same entity.
    pub fn new(a: EntityId, b: EntityId) -> Result<Self, CandidateError> {
        if a == b {
            return Err(CandidateError::SelfPair);
        }
        let (low, high) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Ok(Self { low, high })
    }

    /// The lexicographically lower side.
    #[must_use]
    pub fn low(&self) -> &EntityId {
        &self.low
    }

    /// The lexicographically higher side.
    #[must_use]
    pub fn high(&self) -> &EntityId {
        &self.high
    }
}

/// A **proposal** that two entities are the same. Never a fact.
///
/// Deliberately absent, and each absence is a ruling made structural:
///
/// * **no claim-status field** — the type cannot spell `confirmed`, so a
///   producer cannot write one by passing an argument;
/// * **no `promote` / `confirm` / `accept` method** — promotion is 2b's, behind
///   an authorized adjudicator;
/// * **no cluster or group type** — pairs only, so no closure is expressible;
/// * **no removal or mutation API** — a rejected candidate is still evidence.
///
/// `PartialEq` but not `Eq`: [`Confidence`] carries an `f32` score, so equality
/// is not reflexive in general. Nothing here keys a map on a candidate — the
/// durable key is the observation id — so no `Eq` is needed and claiming one
/// would be false.
#[derive(Debug, Clone, PartialEq)]
pub struct SameAsCandidate {
    pair: CandidatePair,
    matcher: MatcherIdentity,
    confidence: Confidence,
    support: Vec<ObservationId>,
}

impl SameAsCandidate {
    /// Propose that two entities are the same.
    ///
    /// # Errors
    ///
    /// * [`CandidateError::SelfPair`] — both sides are the same entity.
    /// * [`CandidateError::NoSupportingEvidence`] — `support` is empty.
    /// * [`CandidateError::DuplicateSupport`] — an observation was cited twice.
    pub fn propose(
        pair: CandidatePair,
        matcher: MatcherIdentity,
        confidence: Confidence,
        support: Vec<ObservationId>,
    ) -> Result<Self, CandidateError> {
        if support.is_empty() {
            return Err(CandidateError::NoSupportingEvidence);
        }
        let unique: BTreeSet<&ObservationId> = support.iter().collect();
        if unique.len() != support.len() {
            return Err(CandidateError::DuplicateSupport);
        }
        Ok(Self {
            pair,
            matcher,
            confidence,
            support,
        })
    }

    /// The proposed pair, canonically ordered.
    #[must_use]
    pub fn pair(&self) -> &CandidatePair {
        &self.pair
    }

    /// Who proposed it, with which model/rule version.
    #[must_use]
    pub fn matcher(&self) -> &MatcherIdentity {
        &self.matcher
    }

    /// The confidence and its basis — the confidence provenance.
    #[must_use]
    pub fn confidence(&self) -> &Confidence {
        &self.confidence
    }

    /// The observations this proposal was computed from.
    #[must_use]
    pub fn support(&self) -> &[ObservationId] {
        &self.support
    }

    /// The predicate a candidate always carries.
    #[must_use]
    pub fn predicate(&self) -> Predicate {
        CANDIDATE_PREDICATE
    }

    /// The writer class a candidate is always written as.
    #[must_use]
    pub fn source_class(&self) -> SourceClass {
        CANDIDATE_SOURCE_CLASS
    }

    /// **What this contributes to `Corroboration(n)`: zero. Always.**
    ///
    /// Present as a function rather than an omission so the rule is *pinned by a
    /// test* instead of resting on nobody having written the code. A candidate
    /// contributes to corroboration only after an authorized adjudicator
    /// confirms it (`KIRRA-WM-PROMOTION-001`), and the axis counts confirmed
    /// records — never matcher votes.
    #[must_use]
    pub fn corroboration_contribution(&self) -> usize {
        0
    }
}

/// The relations actually **proposed** by a set of candidates.
///
/// Exactly the asserted pairs, canonicalised and deduplicated. There is
/// deliberately no `closure` sibling: `KIRRA-WM-TRANSITIVITY-001` bars
/// transitive closure at the evidence layer, and the reliable way to bar a
/// computation is not to offer it.
#[must_use]
pub fn proposed_relations(candidates: &[SameAsCandidate]) -> BTreeSet<CandidatePair> {
    candidates.iter().map(|c| c.pair().clone()).collect()
}

/// How many **distinct relations** a set of candidates proposes.
///
/// `KIRRA-WM-PROMOTION-001`'s counting unit. Ten candidates about one pair are
/// one relation, which is what stops re-running a matcher from looking like
/// accumulating agreement.
#[must_use]
pub fn distinct_relations(candidates: &[SameAsCandidate]) -> usize {
    proposed_relations(candidates).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ConfidenceBasis, SourceClass};

    fn ent(s: &str) -> EntityId {
        EntityId::new(s).expect("admissible entity id")
    }

    fn obs(s: &str) -> ObservationId {
        ObservationId::new(s).expect("admissible observation id")
    }

    fn matcher() -> MatcherIdentity {
        MatcherIdentity::new("track-matcher", "siamese-v2", "2.3.1").expect("valid")
    }

    fn conf() -> Confidence {
        Confidence::new(Some(0.91), ConfidenceBasis::ModelScore, None).expect("valid")
    }

    fn candidate(a: &str, b: &str, support: &str) -> SameAsCandidate {
        SameAsCandidate::propose(
            CandidatePair::new(ent(a), ent(b)).expect("distinct"),
            matcher(),
            conf(),
            vec![obs(support)],
        )
        .expect("valid candidate")
    }

    const OBS_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OBS_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const OBS_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

    // -- the core invariant -------------------------------------------------

    /// **Three matcher outputs for the same pair produce zero confirmed
    /// corroboration.** The adversarial case from the ruling: agreement among
    /// matcher runs is not evidence of anything.
    #[test]
    fn three_candidates_for_one_pair_contribute_no_corroboration() {
        let cands = vec![
            candidate("robot-a", "robot-b", OBS_A),
            candidate("robot-a", "robot-b", OBS_B),
            candidate("robot-a", "robot-b", OBS_C),
        ];

        let total: usize = cands
            .iter()
            .map(SameAsCandidate::corroboration_contribution)
            .sum();
        assert_eq!(
            total, 0,
            "candidates must never contribute to Corroboration(n)"
        );

        // And they are ONE relation, not three — per-record counting is what
        // would let re-running a matcher inflate trust.
        assert_eq!(distinct_relations(&cands), 1);
    }

    /// **`A~B` plus `B~C` does not synthesize `A~C`.**
    #[test]
    fn a_b_and_b_c_do_not_synthesize_a_c() {
        let cands = vec![
            candidate("robot-a", "robot-b", OBS_A),
            candidate("robot-b", "robot-c", OBS_B),
        ];
        let proposed = proposed_relations(&cands);

        assert_eq!(proposed.len(), 2, "exactly the two asserted pairs");
        let a_c = CandidatePair::new(ent("robot-a"), ent("robot-c")).expect("distinct");
        assert!(
            !proposed.contains(&a_c),
            "a transitive A~C was synthesized; KIRRA-WM-TRANSITIVITY-001 forbids it"
        );
    }

    /// **A candidate survives rejection as evidence.**
    ///
    /// Rejection is a separate append-only record that CITES the candidate's
    /// observations; it does not consume, mutate or delete the candidate. The
    /// property is shown by rejecting via a citation and finding the candidate
    /// unchanged and still readable.
    #[test]
    fn a_candidate_survives_rejection_as_evidence() {
        let cand = candidate("robot-a", "robot-b", OBS_A);
        let before = cand.clone();

        // A rejection cites the supporting observation ids. Note it takes a
        // BORROW: there is no API that would consume the candidate, so
        // "rejected" cannot be spelled as "removed".
        let cited: Vec<ObservationId> = cand.support().to_vec();
        assert_eq!(cited, vec![obs(OBS_A)]);

        assert_eq!(cand, before, "the candidate is unchanged by being rejected");
        assert_eq!(cand.support().len(), 1, "its evidence is still cited");
    }

    /// **Nothing in the 2a path can write a confirmed `same_as`.**
    ///
    /// The structural half of the claim: a candidate's writer class is pinned to
    /// `Derivation`, which `KIRRA-WM-PROMOTION-001` bars from `confirmed` at the
    /// write door. A producer cannot select `Operator` to slip past it, because
    /// the class is not an argument.
    #[test]
    fn a_candidate_is_always_derivation_class_and_never_operator() {
        let cand = candidate("robot-a", "robot-b", OBS_A);
        assert_eq!(cand.source_class(), SourceClass::Derivation);
        assert_ne!(
            cand.source_class(),
            SourceClass::Operator,
            "only an authorized adjudicator writes as Operator"
        );
        assert_eq!(cand.predicate(), Predicate::SameAs);
    }

    // -- construction refusals ---------------------------------------------

    #[test]
    fn a_self_pair_is_refused() {
        assert_eq!(
            CandidatePair::new(ent("robot-a"), ent("robot-a")).unwrap_err(),
            CandidateError::SelfPair
        );
    }

    #[test]
    fn a_candidate_citing_nothing_is_refused() {
        let err = SameAsCandidate::propose(
            CandidatePair::new(ent("robot-a"), ent("robot-b")).expect("distinct"),
            matcher(),
            conf(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(err, CandidateError::NoSupportingEvidence);
    }

    #[test]
    fn citing_one_observation_twice_is_refused() {
        let err = SameAsCandidate::propose(
            CandidatePair::new(ent("robot-a"), ent("robot-b")).expect("distinct"),
            matcher(),
            conf(),
            vec![obs(OBS_A), obs(OBS_A)],
        )
        .unwrap_err();
        assert_eq!(err, CandidateError::DuplicateSupport);
    }

    #[test]
    fn a_matcher_without_a_version_is_refused() {
        assert_eq!(
            MatcherIdentity::new("track-matcher", "siamese-v2", "   ").unwrap_err(),
            CandidateError::EmptyVersion
        );
        assert_eq!(
            MatcherIdentity::new("", "siamese-v2", "2.3.1").unwrap_err(),
            CandidateError::EmptyProducer
        );
        assert_eq!(
            MatcherIdentity::new("track-matcher", "", "2.3.1").unwrap_err(),
            CandidateError::EmptyModel
        );
    }

    // -- canonicalisation ---------------------------------------------------

    /// **Symmetry permits canonical representation and symmetric lookup; it
    /// does not manufacture additional evidence.**
    ///
    /// Both halves matter and they pull in opposite directions, which is why
    /// they are pinned together. One asserted `same_as(B, A)` must be
    /// discoverable under the `(A, B)` spelling — that is the lookup half. But
    /// the record set must still hold exactly **one** candidate: no
    /// auto-generated reverse row, because nobody asserted it.
    /// `KIRRA-WM-TRANSITIVITY-001` bars manufacturing evidence, and a
    /// synthesized reverse is manufactured evidence one step short of a closure.
    #[test]
    fn symmetry_permits_lookup_but_manufactures_no_reverse_row() {
        // Asserted once, in the (B, A) spelling.
        let asserted = vec![candidate("robot-b", "robot-a", OBS_A)];

        // Lookup half: discoverable under either spelling.
        let ab = CandidatePair::new(ent("robot-a"), ent("robot-b")).expect("distinct");
        let ba = CandidatePair::new(ent("robot-b"), ent("robot-a")).expect("distinct");
        assert_eq!(ab, ba, "the two spellings are one relation");
        assert!(
            proposed_relations(&asserted).contains(&ab),
            "findable as (A,B)"
        );
        assert!(
            proposed_relations(&asserted).contains(&ba),
            "findable as (B,A)"
        );

        // Evidence half: still exactly one candidate, citing exactly what the
        // producer cited. Nothing in this module returns candidates it was not
        // given, so a reverse row has no way to come into existence.
        assert_eq!(asserted.len(), 1, "no reverse candidate was synthesized");
        assert_eq!(proposed_relations(&asserted).len(), 1);
        assert_eq!(asserted[0].support(), &[obs(OBS_A)]);
    }

    /// `(a, b)` and `(b, a)` are the same relation, because `same_as` is
    /// symmetric. Without this, emitting both spellings would read as two
    /// relations and defeat per-relation counting.
    #[test]
    fn pair_order_does_not_create_a_second_relation() {
        let ab = candidate("robot-a", "robot-b", OBS_A);
        let ba = candidate("robot-b", "robot-a", OBS_B);
        assert_eq!(ab.pair(), ba.pair());
        assert_eq!(distinct_relations(&[ab, ba]), 1);
    }
}
