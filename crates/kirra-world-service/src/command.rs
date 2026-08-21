//! **The one sanctioned way to write to Kirra World** — Tier 5 box 5c.1.
//!
//! [`query`](crate::query) gave reads a single door. This is the write half,
//! and it is deliberately the same shape: a sealed trait, one entry point,
//! compile-time outcome types.
//!
//! # What this box is NOT allowed to be
//!
//! `KIRRA-WM-TIER5-CQRS-001` states the constraint this module is written
//! against:
//!
//! > **No new semantics hidden inside command wrappers**: a command that
//! > quietly decides something the domain layer does not is a second
//! > adjudication path, and the tier that discovers it will be paying Tier 2's
//! > bill again.
//!
//! Three structural decisions carry that, rather than a promise to be careful:
//!
//! 1. **A command carries an already-constructed domain value, never its
//!    parts.** [`RecordMerge`] takes a
//!    [`MergeEntities`](kirra_world::adjudication::MergeEntities), not
//!    `sources` and `into`. `MergeEntities::new` is where a merge is judged
//!    well-formed — empty source list, duplicate source, merge-into-self — and
//!    a command taking the raw parts would have to make that judgement a second
//!    time. Two judgements are two places to get it wrong, and the second one
//!    is the one nobody tests. Here the command **cannot** validate differently,
//!    because it never sees anything to validate.
//!
//! 2. **No `CommandError`.** The blueprint's signatures end in `| Refused`, and
//!    the tempting reading is a new flattened refusal enum. That would DESTROY
//!    information: [`StoreError`] already distinguishes *no such candidate*
//!    from *that row is not a candidate* from *unauthorized adjudicator*, and
//!    an operator told the wrong one looks in the wrong place. So commands
//!    return `StoreError` unchanged. "Refused" is the store's existing typed
//!    refusal, not a new vocabulary laid over it.
//!
//! 3. **Authority is carried, never re-checked.**
//!    [`AdjudicationAuthority`](kirra_world::same_as_adjudication::AdjudicationAuthority)
//!    refuses any class but `Operator` at CONSTRUCTION, so a held authority is
//!    already an authorized one. A check here would be a second place authority
//!    is decided — which is the defect this module exists to avoid, not a
//!    belt-and-braces improvement.
//!
//! # The asymmetry this module surfaces rather than papers over
//!
//! Building the surface required auditing every write door, and the audit found
//! **two routes to canonical identity change, only one of them gated**:
//!
//! | Door | Authority |
//! |---|---|
//! | `append_same_as_candidate` | pins `Derivation`/`Candidate` — structurally cannot promote |
//! | `adjudicate_same_as` | requires `AdjudicationAuthority` — `Operator` only, per `KIRRA-WM-PROMOTION-001` |
//! | `append_adjudication` (Merge / Split / Forget) | **none** |
//!
//! A `Merge` reaches `Lifecycle::Merged { into }`, the entity projection, and
//! identity resolution — the same canonical-identity effect a promoted
//! `same_as` has, by a route carrying no adjudicator at all.
//!
//! **This module does not fix that, and the restraint is the point.** Requiring
//! an authority token here that the domain does not require would be precisely
//! the "new semantics hidden inside a command wrapper" the box forbids — a
//! tightening invented at the wrapper, where the domain still permits the
//! ungated call to anyone holding `&mut WorldStore`. The fix belongs in
//! `kirra_world::adjudication`, and it needs a ruling: *may an identity
//! adjudication be recorded without an authorized adjudicator?* Recorded in
//! `WM_SCOPE.md`; pinned here by
//! `a_merge_command_carries_no_authority_and_that_is_the_open_finding`.
//!
//! # Why `AssertEntity` is absent
//!
//! [`IdentityAdjudication`] has four verbs and this module wraps three.
//! `Assert` is operator teaching — box **5d**, BLOCKED ON RULING pending what
//! writer class an operator assertion carries and whether it may outrank sensed
//! evidence. One `RecordIdentityAdjudication` command taking any variant would
//! have carried `Assert` in as a side effect and unblocked 5d by accident, so
//! there are three narrow commands instead of one wide one. Pinned by
//! `no_command_can_record_an_assert_because_5d_is_unruled`.
//!
//! # What is deliberately not wrapped
//!
//! `WORLD_MODEL_ARCHITECTURE.md` §14.1 lists nine commands. Five have a domain
//! operation behind them today and are wrapped here. The other four are not
//! omissions to be filled in later without thought:
//!
//! * `AssertEntity` — blocked on 5d, above.
//! * `RecordObservation(ObservationDraft)` — the underlying `WorldStore::append`
//!   is the RAW door: it takes `writer_class` and `claim_status` as free
//!   parameters. A command wrapping it either passes them through (adding
//!   nothing, so it is not a command, it is an alias) or chooses them (deciding
//!   the two fields the whole trust model rests on, inside a wrapper). Neither
//!   is this box's to do.
//! * `ConfirmEntity`, `CorrectObservation`, `RetractAssertion` — **no domain
//!   operation exists**. Writing the command first would put the semantics in
//!   the wrapper by definition, since there would be nothing else for them to
//!   live in.
//! * `ImportMapLayer` — map layers are unstarted and gated on a consumer.

use kirra_world::adjudication::{ForgetEntity, IdentityAdjudication, MergeEntities, SplitEntity};
use kirra_world::same_as_adjudication::SameAsAdjudication;
use kirra_world::same_as_candidate::SameAsCandidate;
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::candidate_record::CandidateRow;
use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;
use kirra_world_store::{StoreError, WorldStore};

mod sealed {
    /// Closed by construction. See [`super::WorldCommand`].
    pub trait Sealed {}
}

/// **One sanctioned write.**
///
/// Sealed for the reason [`WorldQuery`](crate::query::WorldQuery) is: an open
/// trait would let a caller write its own implementation, close over a
/// `&mut WorldStore`, and route an arbitrary mutation through [`CommandEngine`]
/// while looking like sanctioned use. A write surface that can be extended from
/// outside is a write surface that decides nothing.
pub trait WorldCommand: sealed::Sealed {
    /// What performing this command produces.
    ///
    /// Associated rather than a shared union, for [`WorldQuery`]'s reason:
    /// [`AdjudicateSameAs`] yields a [`SameAsAdjudication`] and the others a
    /// generation, checked at compile time, with no runtime arm to get wrong.
    ///
    /// [`WorldQuery`]: crate::query::WorldQuery
    type Outcome;

    /// Perform this command against `engine`.
    ///
    /// Prefer [`CommandEngine::execute`] at call sites. This is the dispatch
    /// target, not the intended spelling.
    ///
    /// # Errors
    ///
    /// Whatever the underlying store operation raises, unchanged — see this
    /// module's note on why there is no `CommandError`.
    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError>;
}

/// **The write entry point.**
///
/// Holds `&mut WorldStore` and offers no reads. The asymmetry with
/// [`QueryEngine`](crate::query::QueryEngine) is deliberate: a command that
/// could also read would invite read-modify-write inside a wrapper, and a
/// decision made from a read the caller never saw is the definition of a
/// semantic hidden in the command layer.
pub struct CommandEngine<'a> {
    store: &'a mut WorldStore,
}

impl<'a> CommandEngine<'a> {
    /// Bind an engine to a store.
    #[must_use]
    pub fn new(store: &'a mut WorldStore) -> Self {
        Self { store }
    }

    /// **Perform one typed command.**
    ///
    /// ```ignore
    /// let mut engine = CommandEngine::new(&mut store);
    /// let generation = engine.execute(RecordMerge { row, merge })?;
    /// ```
    ///
    /// # Errors
    ///
    /// Whatever the underlying store operation raises.
    pub fn execute<C: WorldCommand>(&mut self, command: C) -> Result<C::Outcome, StoreError> {
        command.execute(self)
    }

    /// The store the commands dispatch into.
    ///
    /// `pub(crate)` and no wider: this is the thing the module exists to stop
    /// callers from reaching.
    pub(crate) fn store(&mut self) -> &mut WorldStore {
        self.store
    }
}

/// **Propose that two entities are the same.** Blueprint: part of `RecordObservation`.
///
/// Wraps `WorldStore::append_same_as_candidate`, which pins
/// `WriterClass::Derivation` and `ClaimStatus::Candidate` itself — so this
/// command structurally cannot propose something confirmed, and does not need
/// to be trusted not to.
pub struct ProposeSameAs<'a> {
    /// Where this proposal sits in the log.
    pub row: CandidateRow<'a>,
    /// The proposal itself, already judged well-formed by the domain.
    pub candidate: SameAsCandidate,
}

impl sealed::Sealed for ProposeSameAs<'_> {}

impl WorldCommand for ProposeSameAs<'_> {
    type Outcome = i64;

    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError> {
        engine
            .store()
            .append_same_as_candidate(&self.row, &self.candidate)
    }
}

/// **Judge a proposed `same_as`.** Blueprint: `ConfirmEntity`, narrowed to the
/// one relation v1 promotes.
///
/// The request carries its own `AdjudicationAuthority`, which is why this
/// command performs no authority check of its own. See the module docs.
pub struct AdjudicateSameAs<'a> {
    /// Who decided what, about which persisted candidate.
    pub request: SameAsAdjudicationRequest<'a>,
}

impl sealed::Sealed for AdjudicateSameAs<'_> {}

impl WorldCommand for AdjudicateSameAs<'_> {
    type Outcome = SameAsAdjudication;

    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError> {
        engine.store().adjudicate_same_as(&self.request)
    }
}

/// **Several entities were one.** Blueprint: `MergeEntities`.
pub struct RecordMerge<'a> {
    /// Where this judgement sits in the log.
    pub row: AdjudicationRow<'a>,
    /// The merge, already judged well-formed by `MergeEntities::new`.
    pub merge: MergeEntities,
}

impl sealed::Sealed for RecordMerge<'_> {}

impl WorldCommand for RecordMerge<'_> {
    type Outcome = i64;

    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError> {
        let adjudication = IdentityAdjudication::Merge(self.merge);
        engine.store().append_adjudication(&self.row, &adjudication)
    }
}

/// **One entity was several.** Blueprint: `SplitEntity`.
pub struct RecordSplit<'a> {
    /// Where this judgement sits in the log.
    pub row: AdjudicationRow<'a>,
    /// The split, already judged well-formed by the domain.
    pub split: SplitEntity,
}

impl sealed::Sealed for RecordSplit<'_> {}

impl WorldCommand for RecordSplit<'_> {
    type Outcome = i64;

    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError> {
        let adjudication = IdentityAdjudication::Split(self.split);
        engine.store().append_adjudication(&self.row, &adjudication)
    }
}

/// **An entity was retired.** Blueprint: `ForgetEntity`.
///
/// Retirement, never erasure — `WORLD_MODEL_ARCHITECTURE.md` §14.1 is explicit
/// that genuine erasure would be a distinct audited `Redact` with its own ADR
/// and a tombstone. Nothing here deletes.
pub struct RecordForget<'a> {
    /// Where this judgement sits in the log.
    pub row: AdjudicationRow<'a>,
    /// The retirement, already judged well-formed by the domain.
    pub forget: ForgetEntity,
}

impl sealed::Sealed for RecordForget<'_> {}

impl WorldCommand for RecordForget<'_> {
    type Outcome = i64;

    fn execute(self, engine: &mut CommandEngine<'_>) -> Result<Self::Outcome, StoreError> {
        let adjudication = IdentityAdjudication::Forget(self.forget);
        engine.store().append_adjudication(&self.row, &adjudication)
    }
}
