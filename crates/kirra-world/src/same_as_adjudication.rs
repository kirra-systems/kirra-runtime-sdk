//! Tier 2 box **2b** — authorized adjudication of `same_as` candidates.
//!
//! The narrow slice: take a persisted pairwise `same_as` candidate and record an
//! authorized adjudication that **promotes**, **rejects**, or leaves it
//! **unresolved** — without inventing transitive evidence.
//!
//! # The boundary
//!
//! ```text
//!   matcher / derivation producer        (2a)
//!           ↓
//!   pairwise same_as candidate
//!           ↓
//!      ✗ no trust-grade effect · ✗ no closure · ✗ no confirmed identity
//!           ↓
//!   authorized adjudicator               (here)
//!           ↓
//!   promotion record
//!           ↓
//!   confirmed same_as evidence
//!           ↓
//!   Corroboration(n)
//!           ↓
//!   deterministic identity resolution    (2c)
//! ```
//!
//! # What this module refuses
//!
//! | Rule | Mechanism |
//! |---|---|
//! | only the ruled class may promote | [`AdjudicationAuthority::new`] REFUSES any class but [`SourceClass::Operator`], so a held authority is always an authorized one |
//! | promotion cites the candidate | the constructor requires a non-empty, duplicate-free citation of [`ObservationId`]s |
//! | rejection never deletes | a rejection is another append-only record that cites the same candidate |
//! | promotion yields pairwise identity | the outcome carries a [`CandidatePair`]; there is no cluster type to produce |
//! | `A=B` + `B=C` never emits `A=C` | [`confirmed_relations`] returns promoted pairs only; no closure function exists |
//! | corroboration counts relations | [`corroboration_count`] deduplicates by pair, so re-adjudication cannot inflate it |
//!
//! # Reversal is not this module's
//!
//! Reversing a promotion is the existing `SplitEntity` in
//! [`crate::adjudication`], and `ForgetEntity` is **erasure, not reversal** —
//! neither is re-implemented here, and rejection is deliberately a *different*
//! thing from both (nothing was promoted, so there is nothing to reverse).
//!
//! Rulings: `KIRRA-WM-CLUSTERING-001`, `KIRRA-WM-TRANSITIVITY-001`,
//! `KIRRA-WM-PROMOTION-001`.

use std::collections::{BTreeMap, BTreeSet};

use crate::observation::{DomainInstant, SourceClass};
use crate::reference::ObservationId;
use crate::relationship::Predicate;
use crate::same_as_candidate::CandidatePair;

/// The one writer class authorized to promote, per `KIRRA-WM-PROMOTION-001`.
pub const AUTHORIZED_ADJUDICATOR_CLASS: SourceClass = SourceClass::Operator;

/// Why an adjudication could not be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationError {
    /// The writer class is not authorized to adjudicate.
    ///
    /// v1 admits `Operator` only. An automated adjudicator is not merely
    /// unimplemented — it is **unruled**, and `KIRRA-WM-PROMOTION-001` requires
    /// its own ruling before one exists.
    UnauthorizedAdjudicator(SourceClass),
    /// The adjudicator was not named.
    EmptyAdjudicator,
    /// The adjudication cited no candidate.
    ///
    /// A promotion that names nothing it acted on cannot be audited, and
    /// `KIRRA-WM-PROMOTION-001` requires the candidate observations be cited.
    NoCandidateCited,
    /// The same candidate observation was cited twice.
    DuplicateCitation,
}

impl std::fmt::Display for AdjudicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnauthorizedAdjudicator(c) => write!(
                f,
                "writer_class={c:?} may not adjudicate same_as; v1 admits operator only \
                 (KIRRA-WM-PROMOTION-001)"
            ),
            Self::EmptyAdjudicator => write!(f, "the adjudicator was not named"),
            Self::NoCandidateCited => {
                write!(
                    f,
                    "an adjudication must cite the candidate observation(s) it acted on"
                )
            }
            Self::DuplicateCitation => write!(f, "a candidate observation was cited twice"),
        }
    }
}

/// Proof that a writer is allowed to adjudicate.
///
/// **The check is at the constructor, not in the type signature**, and the
/// distinction is worth stating precisely because an earlier draft of this doc
/// overclaimed it. [`Self::new`] accepts any [`SourceClass`] and REFUSES every
/// one but [`SourceClass::Operator`] at runtime; nothing stops a caller
/// *passing* `Derivation`, only from getting an authority back.
///
/// What the type does buy is that the refusal cannot be forgotten downstream:
/// there is no public field and no other constructor, so **a value of this type
/// that exists is always an authorized one**, and every function taking it gets
/// that for free without re-checking. That is a runtime-checked invariant with
/// a type-level carrier — not a compile-time restriction, and it should not be
/// described as one.
///
/// **This constrains the class, never the credential.** `SourceClass` is
/// declared by the writer, so "operator only" is exactly as strong as the
/// authentication on the write path — the assumption of use recorded with
/// `KIRRA-WM-PROMOTION-001`. A deployment that lets an automated agent write as
/// `Operator` has bypassed this without violating it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdjudicationAuthority {
    adjudicator: String,
}

impl AdjudicationAuthority {
    /// Establish authority for a named adjudicator of a given class.
    ///
    /// # Errors
    ///
    /// * [`AdjudicationError::UnauthorizedAdjudicator`] — any class other than
    ///   [`SourceClass::Operator`].
    /// * [`AdjudicationError::EmptyAdjudicator`] — the name is empty.
    pub fn new(
        class: SourceClass,
        adjudicator: impl Into<String>,
    ) -> Result<Self, AdjudicationError> {
        if class != AUTHORIZED_ADJUDICATOR_CLASS {
            return Err(AdjudicationError::UnauthorizedAdjudicator(class));
        }
        let adjudicator = adjudicator.into();
        if adjudicator.trim().is_empty() {
            return Err(AdjudicationError::EmptyAdjudicator);
        }
        Ok(Self { adjudicator })
    }

    /// Who decided.
    #[must_use]
    pub fn adjudicator(&self) -> &str {
        &self.adjudicator
    }

    /// The class, which is fixed.
    #[must_use]
    pub fn class(&self) -> SourceClass {
        AUTHORIZED_ADJUDICATOR_CLASS
    }
}

/// What an adjudicator decided about one candidate.
///
/// `Unresolved` is a real outcome, not a missing one: "looked at and could not
/// decide" is different from "nobody has looked", and only the first is
/// evidence. Without it, an adjudicator with no opinion would have to either
/// reject (a judgement it did not make) or stay silent (indistinguishable from
/// an unreviewed backlog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The pair is confirmed to denote the same entity.
    Promoted,
    /// The pair is judged NOT to denote the same entity.
    Rejected,
    /// Looked at; not decided.
    Unresolved,
}

/// One recorded adjudication of one candidate pair.
///
/// Append-only by construction: no setters, and no API removes anything. A
/// rejection is a *new* record citing the same candidate — the candidate itself
/// survives as evidence, because deleting the thing a judgement is about
/// destroys the judgement's subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SameAsAdjudication {
    pair: CandidatePair,
    candidate_observation_id: ObservationId,
    cited: Vec<ObservationId>,
    authority: AdjudicationAuthority,
    outcome: Outcome,
    decided_at: DomainInstant,
}

impl SameAsAdjudication {
    /// Record a decision about a **persisted** candidate.
    ///
    /// # There is no `&SameAsCandidate` parameter, deliberately
    ///
    /// An earlier signature took the candidate by value-reference, which meant
    /// an adjudicator could be handed a caller-constructed struct that merely
    /// *looked* like evidence. Judging that is judging an assertion, not a
    /// record — and the whole point of `KIRRA-WM-PROMOTION-001` is that
    /// confirmed identity rests on recorded evidence.
    ///
    /// So this takes the `pair` and the **`candidate_observation_id`** of a
    /// candidate that exists in the log. The production caller is
    /// `WorldStore::adjudicate_same_as`, which obtains both by LOADING the row
    /// and validating it; nothing here can be satisfied by a value that was
    /// never written.
    ///
    /// The id is kept on the record rather than merely checked, because *which*
    /// persisted candidate was judged is part of what a later reader needs — a
    /// decision that cannot name its subject is not auditable.
    ///
    /// # Errors
    ///
    /// * [`AdjudicationError::NoCandidateCited`] — `cited` is empty.
    /// * [`AdjudicationError::DuplicateCitation`] — an id appears twice.
    pub fn record(
        pair: CandidatePair,
        candidate_observation_id: ObservationId,
        cited: Vec<ObservationId>,
        authority: AdjudicationAuthority,
        outcome: Outcome,
        decided_at: DomainInstant,
    ) -> Result<Self, AdjudicationError> {
        if cited.is_empty() {
            return Err(AdjudicationError::NoCandidateCited);
        }
        let unique: BTreeSet<&ObservationId> = cited.iter().collect();
        if unique.len() != cited.len() {
            return Err(AdjudicationError::DuplicateCitation);
        }
        Ok(Self {
            pair,
            candidate_observation_id,
            cited,
            authority,
            outcome,
            decided_at,
        })
    }

    /// The persisted candidate observation this decision judged.
    #[must_use]
    pub fn candidate_observation_id(&self) -> &ObservationId {
        &self.candidate_observation_id
    }

    /// The pair decided about.
    #[must_use]
    pub fn pair(&self) -> &CandidatePair {
        &self.pair
    }

    /// The candidate observations this decision cites.
    #[must_use]
    pub fn cited(&self) -> &[ObservationId] {
        &self.cited
    }

    /// Who decided, and under what authority.
    #[must_use]
    pub fn authority(&self) -> &AdjudicationAuthority {
        &self.authority
    }

    /// What was decided.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// When.
    #[must_use]
    pub fn decided_at(&self) -> DomainInstant {
        self.decided_at
    }

    /// The predicate a promotion confirms — pairwise, never a cluster.
    #[must_use]
    pub fn predicate(&self) -> Predicate {
        Predicate::SameAs
    }

    /// Whether this record confirms identity.
    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        self.outcome == Outcome::Promoted
    }
}

// PRECEDENCE-RULE-BEGIN
//
// `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`. Everything between these markers is
// the ONE definition of which adjudication governs a pair. Both the
// whole-history view below and `kirra_world_store::relationship_projection`'s
// incremental fold call into it, so the two cannot answer differently.

/// **Does the decision in effect leave the pair related?**
///
/// The effect half of `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`, and the one place
/// it is written. The store's incremental fold calls this rather than matching
/// on [`Outcome`] itself — which is what stops the two sides drifting, as they
/// had by 5c: this layer said *one `Promoted` anywhere confirms forever* while
/// the projection said *the latest decision governs*, and both claimed to be
/// reading the same operator history.
///
/// Exhaustive with no wildcard, deliberately: a new [`Outcome`] variant becomes
/// a COMPILE error here, in one place, instead of silently taking whichever arm
/// a `_` fell into in two.
#[must_use]
pub fn leaves_pair_related(outcome: Outcome) -> bool {
    match outcome {
        Outcome::Promoted => true,
        // Both WITHDRAW. `Unresolved` withdrawing rather than abstaining is the
        // choice box 5a recorded: the log does not say which the operator
        // meant, so abstaining would make the answer depend on a distinction
        // nothing wrote down while continuing to assert an identity the newest
        // authorized decision declined to affirm.
        Outcome::Rejected | Outcome::Unresolved => false,
    }
}

/// **The promotions currently supporting each related pair.**
///
/// The ordering half of `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`: decisions are
/// applied in **generation** order, and a pair is related when its most recent
/// run of decisions since the last withdrawal contains at least one promotion.
///
/// Only currently-related pairs appear. The `Vec` is that pair's **unbroken run
/// of promotions since the last withdrawal**, in generation order — which is
/// what lets a caller answer *when did this begin* and *on what evidence*
/// without walking the history a second time under a different rule.
///
/// # Two questions, one walk
///
/// *Which pairs are related* is decided by the latest decision. *When the
/// relation began* is the earliest promotion in the CURRENT run — so
/// re-affirming a pair does not move its start date, while a rejection resets
/// it. Both fall out of one pass, which is the point: the alternative is two
/// passes under two rules that can disagree, and that is the defect this ruling
/// exists to close.
///
/// Evidence follows the same line. A promotion that was later withdrawn does
/// not support the current relation, so its citations are dropped when the run
/// resets — citing them would be citing evidence for a decision an operator
/// un-made.
///
/// # Why generation and not `decided_at`
///
/// `decided_at` is on the record and is NOT the precedence key. That is
/// surprising enough to justify, and the reasons are of three different kinds —
/// worth separating, because one is an implementation constraint rather than a
/// semantic argument and should not be dressed as one.
///
/// **Semantic.** An adjudication is an ACT on the record by an authorized
/// party, not an observation of the world. `crate::projection`'s claim fold
/// orders on valid time precisely because a claim is about the world — when the
/// fact held matters more than when we heard it. A decision is the opposite: an
/// operator cannot retroactively un-make a decision already recorded and acted
/// upon, so a backdated decision arriving today must not silently override this
/// morning's.
///
/// **Totality.** Generation is a total order and always present.
/// [`DomainInstant`] refuses cross-domain comparison — correctly — so a
/// `decided_at` rule would have to refuse a pair whose decisions sit on
/// different clocks, turning an ordinary question into an error.
///
/// **Implementation, stated as such.** An incremental fold requires the
/// ordering key to BE the fold order. Under `decided_at`, an out-of-order
/// arrival would change the answer for a pair already folded, forcing a rebuild
/// from generation 0 every time — so `decided_at` precedence is not merely a
/// different rule, it is incompatible with the projection's design.
///
/// # The limitation this accepts, recorded rather than hidden
///
/// **AOU-ADJUDICATION-ORDER-001.** An adjudication reaching the log out of
/// decision order — an offline operator console syncing later — takes
/// precedence over decisions recorded before it, even though it was decided
/// earlier. A deployment that queues decisions offline must either record them
/// in decision order or accept that the record's order is the authority. A real
/// cost of the rule, not a defect in it.
pub fn promotions_in_effect<'a, I>(
    history: I,
) -> BTreeMap<CandidatePair, Vec<&'a SameAsAdjudication>>
where
    I: IntoIterator<Item = (i64, &'a SameAsAdjudication)>,
{
    let mut ordered: Vec<(i64, &'a SameAsAdjudication)> = history.into_iter().collect();
    // Sorted rather than assumed: this is the whole-history entry point and a
    // caller may hand it a log slice in any order. The store's fold takes its
    // ordering from `ORDER BY generation ASC` instead, and the two agreeing on
    // arbitrary histories is what the conformance corpus checks.
    ordered.sort_by_key(|(generation, _)| *generation);

    let mut runs: BTreeMap<CandidatePair, Vec<&'a SameAsAdjudication>> = BTreeMap::new();
    for (_, decision) in ordered {
        let run = runs.entry(decision.pair().clone()).or_default();
        if leaves_pair_related(decision.outcome()) {
            run.push(decision);
        } else {
            // The withdrawal resets the run. Kept as an EMPTY entry rather than
            // removed, so the two states stay distinguishable inside this walk;
            // the empty ones are dropped below.
            run.clear();
        }
    }
    runs.retain(|_, run| !run.is_empty());
    runs
}

// PRECEDENCE-RULE-END

/// The relations that **currently hold**.
///
/// Deduplicated, and governed by `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`: a pair
/// is related when the decision IN EFFECT for it is `Promoted`, not when any
/// historical decision was.
///
/// # What changed, and why the old reading was wrong
///
/// This used to be `filter(is_confirmed)` across every record — one `Promoted`
/// anywhere confirmed the pair however many rejections followed. That put it in
/// direct contradiction with the relationship projection, which has always let
/// the newest authorized decision govern. Two deterministic readings of one
/// operator history cannot both be authoritative, and this was the wrong one:
/// it made an operator's rejection unable to undo their own promotion.
///
/// There is deliberately no closure sibling:
/// `KIRRA-WM-TRANSITIVITY-001` permits resolution (2c) to traverse accepted
/// merges transitively, but forbids this layer from *emitting* the traversed
/// relation as evidence. `A=B` and `B=C` yield two confirmed relations here,
/// never three.
#[must_use]
pub fn confirmed_relations<'a, I>(history: I) -> BTreeSet<CandidatePair>
where
    I: IntoIterator<Item = (i64, &'a SameAsAdjudication)>,
{
    promotions_in_effect(history).into_keys().collect()
}

/// `Corroboration(n)` — **distinct relations currently in effect**.
///
/// Not adjudication records, and not candidate votes. Counting records would
/// let re-adjudicating one pair inflate trust; counting votes would let a
/// matcher do it, which `KIRRA-WM-PROMOTION-001` bars outright.
///
/// Under `KIRRA-WM-ADJUDICATION-PRECEDENCE-001` this counts relations that
/// currently HOLD — a promoted-then-rejected pair contributes nothing. Counting
/// "ever promoted" would let a withdrawn decision keep propping up a trust
/// grade, which is the same inflation by a slower route.
#[must_use]
pub fn corroboration_count<'a, I>(history: I) -> usize
where
    I: IntoIterator<Item = (i64, &'a SameAsAdjudication)>,
{
    confirmed_relations(history).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ClockDomain, Confidence, ConfidenceBasis};
    use crate::reference::EntityId;
    use crate::same_as_candidate::{MatcherIdentity, SameAsCandidate};

    /// Attach generations in slice order — the order a log would have recorded
    /// these decisions.
    ///
    /// Every test below reads as a HISTORY now rather than a bag, which is the
    /// visible consequence of `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`: the order
    /// decisions were recorded in is part of the question, so a caller cannot
    /// ask it without saying what that order was.
    fn in_order(records: &[SameAsAdjudication]) -> Vec<(i64, &SameAsAdjudication)> {
        records
            .iter()
            .enumerate()
            .map(|(i, a)| (i as i64 + 1, a))
            .collect()
    }

    const OBS_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OBS_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const OBS_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

    fn ent(s: &str) -> EntityId {
        EntityId::new(s).expect("admissible")
    }
    fn obs(s: &str) -> ObservationId {
        ObservationId::new(s).expect("admissible")
    }
    fn at(ms: u64) -> DomainInstant {
        DomainInstant {
            ms,
            domain: ClockDomain::System,
        }
    }
    fn operator() -> AdjudicationAuthority {
        AdjudicationAuthority::new(SourceClass::Operator, "op-jane").expect("authorized")
    }
    fn candidate(a: &str, b: &str, support: &str) -> SameAsCandidate {
        SameAsCandidate::propose(
            CandidatePair::new(ent(a), ent(b)).expect("distinct"),
            MatcherIdentity::new("track-matcher", "siamese-v2", "2.3.1").expect("valid"),
            Confidence::new(Some(0.9), ConfidenceBasis::ModelScore, None).expect("valid"),
            vec![obs(support)],
        )
        .expect("valid candidate")
    }
    fn decide(c: &SameAsCandidate, o: Outcome, cite: &str) -> SameAsAdjudication {
        SameAsAdjudication::record(
            c.pair().clone(),
            obs("cand-obs-1"),
            vec![obs(cite)],
            operator(),
            o,
            at(1),
        )
        .expect("recordable")
    }

    /// **Only the ruled class may promote.** A matcher writes as `Derivation`,
    /// which cannot even construct the authority.
    #[test]
    fn only_an_operator_may_adjudicate() {
        for refused in [
            SourceClass::Derivation,
            SourceClass::Sensor,
            SourceClass::Import,
            SourceClass::Network,
            SourceClass::Configuration,
        ] {
            assert_eq!(
                AdjudicationAuthority::new(refused, "someone").unwrap_err(),
                AdjudicationError::UnauthorizedAdjudicator(refused),
                "{refused:?} must not be able to adjudicate"
            );
        }
        assert!(AdjudicationAuthority::new(SourceClass::Operator, "op-jane").is_ok());
    }

    /// **Promotion cites the candidate observation.**
    #[test]
    fn an_adjudication_must_cite_the_candidate() {
        let c = candidate("robot-a", "robot-b", OBS_A);
        assert_eq!(
            SameAsAdjudication::record(
                c.pair().clone(),
                obs("cand-obs-1"),
                vec![],
                operator(),
                Outcome::Promoted,
                at(1)
            )
            .unwrap_err(),
            AdjudicationError::NoCandidateCited
        );
        assert_eq!(
            SameAsAdjudication::record(
                c.pair().clone(),
                obs("cand-obs-1"),
                vec![obs(OBS_A), obs(OBS_A)],
                operator(),
                Outcome::Promoted,
                at(1)
            )
            .unwrap_err(),
            AdjudicationError::DuplicateCitation
        );
        let ok = decide(&c, Outcome::Promoted, OBS_A);
        assert_eq!(ok.cited(), &[obs(OBS_A)]);
    }

    /// **Rejection is append-only and cites the candidate rather than deleting
    /// it.** The candidate is borrowed, so "rejected" cannot be spelled as
    /// "consumed".
    #[test]
    fn rejection_cites_the_candidate_and_leaves_it_intact() {
        let c = candidate("robot-a", "robot-b", OBS_A);
        let before = c.clone();
        let rejection = decide(&c, Outcome::Rejected, OBS_A);

        assert_eq!(rejection.outcome(), Outcome::Rejected);
        assert_eq!(rejection.cited(), &[obs(OBS_A)]);
        assert_eq!(c, before, "the candidate survives its own rejection");
        assert_eq!(c.support(), &[obs(OBS_A)], "and still carries its evidence");
        assert_eq!(corroboration_count(in_order(&[rejection])), 0);
    }

    /// **`A=B` and `B=C` do not produce `A=C`.**
    #[test]
    fn promoting_two_chained_pairs_does_not_emit_the_third() {
        let ab = decide(
            &candidate("robot-a", "robot-b", OBS_A),
            Outcome::Promoted,
            OBS_A,
        );
        let bc = decide(
            &candidate("robot-b", "robot-c", OBS_B),
            Outcome::Promoted,
            OBS_B,
        );
        let confirmed = confirmed_relations(in_order(&[ab, bc]));

        assert_eq!(confirmed.len(), 2, "exactly the two promoted pairs");
        let ac = CandidatePair::new(ent("robot-a"), ent("robot-c")).expect("distinct");
        assert!(
            !confirmed.contains(&ac),
            "a transitive A=C was emitted as evidence; KIRRA-WM-TRANSITIVITY-001 forbids it"
        );
    }

    /// **Re-adjudicating the same pair cannot inflate corroboration.**
    #[test]
    fn re_adjudicating_one_pair_counts_once() {
        let c = candidate("robot-a", "robot-b", OBS_A);
        let first = decide(&c, Outcome::Promoted, OBS_A);
        let again = decide(&c, Outcome::Promoted, OBS_B);
        let third = decide(&c, Outcome::Promoted, OBS_C);

        assert_eq!(
            corroboration_count(in_order(&[first, again, third])),
            1,
            "three promotions of one relation is one corroborated relation"
        );
    }

    /// Corroboration counts CONFIRMED relations — rejected and unresolved
    /// contribute nothing, and neither does a candidate on its own.
    #[test]
    fn only_promotions_corroborate() {
        let ab = candidate("robot-a", "robot-b", OBS_A);
        let cd = candidate("robot-c", "robot-d", OBS_B);
        let ef = candidate("robot-e", "robot-f", OBS_C);

        let records = vec![
            decide(&ab, Outcome::Promoted, OBS_A),
            decide(&cd, Outcome::Rejected, OBS_B),
            decide(&ef, Outcome::Unresolved, OBS_C),
        ];
        assert_eq!(corroboration_count(in_order(&records)), 1);

        // The candidates themselves still contribute nothing, whatever happened
        // to them — 2a's bar is not relaxed by 2b existing.
        assert_eq!(ab.corroboration_contribution(), 0);
        assert_eq!(cd.corroboration_contribution(), 0);
    }

    /// A promotion confirms a PAIRWISE relation. There is no cluster object to
    /// hand anyone, which is the seam `KIRRA-WM-PROMOTION-001` fixed.
    #[test]
    fn a_promotion_is_pairwise_and_carries_its_authority() {
        let c = candidate("robot-a", "robot-b", OBS_A);
        let p = decide(&c, Outcome::Promoted, OBS_A);

        assert_eq!(p.predicate(), Predicate::SameAs);
        assert!(p.is_confirmed());
        assert_eq!(p.pair(), c.pair());
        assert_eq!(p.authority().class(), SourceClass::Operator);
        assert_eq!(p.authority().adjudicator(), "op-jane");
        assert_eq!(p.decided_at(), at(1));
    }

    /// **Every relation key is canonical, whichever way it was asserted.**
    ///
    /// The dedupe is only as symmetric as its KEY. `same_as` being symmetric at
    /// the domain type buys nothing if something downstream counts a raw
    /// `(A, B)` tuple — the relation would be symmetric in the type system and
    /// still count twice in the tally, which is the trust inflation
    /// `KIRRA-WM-PROMOTION-001` bars, arriving one layer lower than expected.
    ///
    /// This pins the property rather than today's call sites: whatever order a
    /// caller asserted in, every key that reaches a set or a count has
    /// `low() <= high()`. `CandidatePair`'s fields are private with one
    /// constructor, so a non-canonical key is unconstructible — this asserts
    /// that stays true.
    #[test]
    fn every_confirmed_relation_key_is_canonical_whichever_order_was_asserted() {
        let records = vec![
            decide(
                &candidate("robot-b", "robot-a", OBS_A),
                Outcome::Promoted,
                OBS_A,
            ),
            decide(
                &candidate("robot-c", "robot-b", OBS_B),
                Outcome::Promoted,
                OBS_B,
            ),
            decide(
                &candidate("robot-a", "robot-c", OBS_C),
                Outcome::Promoted,
                OBS_C,
            ),
        ];
        let keys = confirmed_relations(in_order(&records));
        assert_eq!(
            keys.len(),
            3,
            "three distinct relations, none duplicated by order"
        );
        for k in &keys {
            assert!(
                k.low() <= k.high(),
                "a non-canonical relation key reached the dedupe: {k:?}"
            );
        }
        // And the count agrees with the set: no path counts records instead.
        assert_eq!(corroboration_count(in_order(&records)), keys.len());
    }

    /// Pair order does not create a second confirmed relation, since `same_as`
    /// is symmetric and the pair is canonical.
    #[test]
    fn promoting_both_spellings_is_one_confirmed_relation() {
        let ab = decide(
            &candidate("robot-a", "robot-b", OBS_A),
            Outcome::Promoted,
            OBS_A,
        );
        let ba = decide(
            &candidate("robot-b", "robot-a", OBS_B),
            Outcome::Promoted,
            OBS_B,
        );
        assert_eq!(corroboration_count(in_order(&[ab, ba])), 1);
    }
}
