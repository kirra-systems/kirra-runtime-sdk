//! The four orthogonal trust axes — `KIRRA-WM-ARCH-001` §9, Tier 1.
//!
//! The blueprint's central claim about trust is that the familiar states —
//! Observed, Confirmed, Derived, Imported, Expired, Ambiguous, Rejected — are
//! **not one dimension**. `Imported` describes origin; `Expired` describes time;
//! `Ambiguous` describes adjudication; `Confirmed` conflates corroboration with
//! adjudication. Collapsing them is *"exactly why trust states in most systems
//! become mush after eighteen months — every new case forces either a wrong
//! assignment or a new variant"*.
//!
//! So four axes are stored separately and the familiar labels are **derived**:
//!
//! ```text
//! Origin        : Observed | Derived | Imported | Asserted | Predicted*
//! Corroboration : Uncorroborated | Corroborated(n) | Contradicted(n)
//! Adjudication  : Pending | Confirmed | Rejected | Ambiguous
//! Validity      : Fresh | Stale | Expired | Timeless
//! ```
//!
//! # Rule 6 is enforced structurally, not remembered
//!
//! Transition rule 6 says *"Validity is computed at read time, never stored.
//! `Fresh` is not a state the system enters; it is a question asked at the
//! instant of the query, with the clock passed in."*
//!
//! [`TrustAxes`] therefore has **three fields, not four**. There is nowhere to
//! put a [`Validity`], so no code can store one — the rule cannot be broken by
//! forgetting it. Validity is produced only by [`validity_at`], which takes the
//! clock as an argument.
//!
//! This is a deliberate echo of what makes Fence B strong and ADR-0042's
//! Decision 1 weak: an invariant a machine enforces outlives one a person must
//! remember.
//!
//! # What this module does NOT do
//!
//! **It does not populate `Corroborated(n)`.** Counting agreeing evidence
//! presupposes knowing that two observations are *about the same thing*, which
//! is entity resolution — `WM_SCOPE.md` Tier 2. The axis and its monotonic
//! transitions are here; the matcher that would drive them is not.
//!
//! **It carries no geometry.** Transition rule 4 has two halves: an operator
//! assertion outranks sensor evidence *for adjudication*, and never *for
//! geometry*. Only the adjudication half lives here — see
//! [`TrustAxes::operator_confirm`]. The geometry half is a constraint on the
//! observation payload, which Tier 1's observation model owns.

// ---------------------------------------------------------------------------
// Origin — rule 1: fixed at write, never changes
// ---------------------------------------------------------------------------

/// Where a claim came from. **Rule 1: origin never changes.** It is a property
/// of the record, fixed when the record is written.
///
/// There is deliberately no setter and no `&mut` accessor anywhere in this
/// module; the only way to obtain a [`TrustAxes`] with a different origin is to
/// construct a new one, which is what writing a new record means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Measured by a sensor or perception producer.
    Observed,
    /// Computed from other recorded evidence. Subject to rule 5.
    Derived,
    /// Brought in from an external dataset.
    Imported,
    /// Stated by a human operator.
    Asserted,
    /// **Never appears in the evidence store** (blueprint §20). Listed because
    /// the *axis* is shared with the predictive subsystem, which reuses this
    /// vocabulary for its own records.
    ///
    /// [`TrustAxes::new`] refuses it — see [`TrustError::PredictedNotStorable`].
    Predicted,
}

impl Origin {
    /// The stable token for this origin.
    ///
    /// Named `as_str` to match the store's existing convention. It is **not**
    /// yet claimed to be inside any hashed bytes — this crate has no storage,
    /// and binding a token to a hash chain is the store's decision to make.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Derived => "derived",
            Self::Imported => "imported",
            Self::Asserted => "asserted",
            Self::Predicted => "predicted",
        }
    }
}

// ---------------------------------------------------------------------------
// Corroboration — rule 2: monotonic in evidence, not in time
// ---------------------------------------------------------------------------

/// How much independent evidence agrees or disagrees.
///
/// **Rule 2: monotonic in evidence, not in time.** New agreeing evidence
/// increments; new disagreeing evidence moves to `Contradicted`. It never decays
/// on its own — decay is [`Validity`]'s job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Corroboration {
    /// One source, or none that can be cross-checked.
    Uncorroborated,
    /// `n` independent sources agree. `n >= 1`.
    Corroborated(u32),
    /// `n` independent sources disagree. `n >= 1`. Implies `Ambiguous`
    /// adjudication unless a rule resolves it — see rule 3.
    Contradicted(u32),
}

impl Corroboration {
    /// Fold in one piece of **agreeing** evidence.
    ///
    /// # A contradiction is not cleared by agreement
    ///
    /// `Contradicted(n).agreed()` stays `Contradicted(n)`. Agreement does not
    /// silently erase a recorded disagreement — rule 3 makes `Contradicted` mean
    /// `Ambiguous` *"unless an adjudication rule resolves it"*, so resolution is
    /// adjudication's job (see [`TrustAxes::operator_confirm`] and
    /// [`TrustAxes::reject`]), not arithmetic's.
    ///
    /// Reading it the other way would let a contradicted claim be quietly voted
    /// back to healthy by piling on agreeing sources, which is the laundering
    /// shape rule 5 exists to prevent, one axis over.
    #[must_use]
    pub fn agreed(self) -> Self {
        match self {
            Self::Uncorroborated => Self::Corroborated(1),
            Self::Corroborated(n) => Self::Corroborated(n.saturating_add(1)),
            Self::Contradicted(n) => Self::Contradicted(n),
        }
    }

    /// Fold in one piece of **disagreeing** evidence.
    ///
    /// Any prior agreement is superseded: `Corroborated(7).disagreed()` is
    /// `Contradicted(1)`, not `Corroborated(6)`. Disagreement is a change of
    /// kind, not a decrement — seven agreeing sources and one dissenter is a
    /// claim in conflict, and the count that matters afterwards is the count of
    /// conflicts.
    #[must_use]
    pub fn disagreed(self) -> Self {
        match self {
            Self::Uncorroborated | Self::Corroborated(_) => Self::Contradicted(1),
            Self::Contradicted(n) => Self::Contradicted(n.saturating_add(1)),
        }
    }

    /// Rank for the weakest-wins fold of rule 5. Lower is weaker.
    fn rank(self) -> u8 {
        match self {
            Self::Contradicted(_) => 0,
            Self::Uncorroborated => 1,
            Self::Corroborated(_) => 2,
        }
    }

    /// The weaker of two corroborations — rule 5's fold on this axis.
    ///
    /// Within a variant the conservative direction differs: **more**
    /// contradictions is weaker, **fewer** corroborations is weaker.
    #[must_use]
    pub fn weakest(self, other: Self) -> Self {
        match (self, other) {
            (Self::Contradicted(a), Self::Contradicted(b)) => Self::Contradicted(a.max(b)),
            (Self::Corroborated(a), Self::Corroborated(b)) => Self::Corroborated(a.min(b)),
            _ if self.rank() <= other.rank() => self,
            _ => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Adjudication — rules 3, 4, 7
// ---------------------------------------------------------------------------

/// Whether the claim has been ruled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Adjudication {
    /// Not yet ruled on.
    Pending,
    /// Ruled admissible.
    Confirmed,
    /// Ruled inadmissible. **Rule 7: terminal for the record.**
    Rejected,
    /// In conflict. **Rule 3 says this is a stable, reportable state, not an
    /// error** — Mick must be able to say "I have conflicting information about
    /// that."
    Ambiguous,
}

impl Adjudication {
    /// Rank for the weakest-wins fold of rule 5. Lower is weaker.
    fn rank(self) -> u8 {
        match self {
            Self::Rejected => 0,
            Self::Ambiguous => 1,
            Self::Pending => 2,
            Self::Confirmed => 3,
        }
    }

    /// The weaker of two adjudications — rule 5's fold on this axis.
    #[must_use]
    pub fn weakest(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

// ---------------------------------------------------------------------------
// Validity — rule 6: computed, never stored
// ---------------------------------------------------------------------------

/// Temporal admissibility, **computed at read time from a clock**.
///
/// This type is deliberately absent from [`TrustAxes`]. It is produced only by
/// [`validity_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Validity {
    /// Within its staleness budget.
    Fresh,
    /// Past its staleness budget but not expired.
    Stale,
    /// Past its explicit end of validity, or not yet in force — see
    /// [`validity_at`] for why those share a value.
    Expired,
    /// Carries no expiry. A surveyed building does not go stale.
    Timeless,
}

impl Validity {
    /// Rank for the weakest-wins fold of rule 5. Lower is weaker.
    fn rank(self) -> u8 {
        match self {
            Self::Expired => 0,
            Self::Stale => 1,
            Self::Fresh => 2,
            Self::Timeless => 3,
        }
    }

    /// The weaker of two validities — rule 5's fold on this axis.
    #[must_use]
    pub fn weakest(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

/// The stored temporal facts a validity question is asked against.
///
/// Note what is here: only *facts*, no verdict. The verdict is
/// [`validity_at`]'s return value and is never written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidityWindow {
    /// When the claim enters force (valid time, not transaction time).
    pub valid_from_ms: u64,
    /// When the claim leaves force, if it ever does.
    pub valid_to_ms: Option<u64>,
    /// How long after `valid_from_ms` the claim counts as `Fresh`.
    /// `None` means the claim does not go stale — see [`Validity::Timeless`].
    pub staleness_budget_ms: Option<u64>,
}

impl ValidityWindow {
    /// A window that never expires and never goes stale.
    #[must_use]
    pub fn timeless(valid_from_ms: u64) -> Self {
        Self {
            valid_from_ms,
            valid_to_ms: None,
            staleness_budget_ms: None,
        }
    }
}

/// Answer the validity question for `window` at instant `now_ms` — **rule 6**.
///
/// # Order of checks, and why
///
/// Expiry is checked before staleness so an expired claim is never reported as
/// merely `Stale`; the answer is the strongest *restriction* that applies, not
/// the first one found.
///
/// # A spec gap, flagged rather than silently resolved
///
/// A claim whose `valid_from_ms` is in the future is **not yet in force**. The
/// blueprint's four-value axis has no member for that case. This function
/// returns [`Validity::Expired`], chosen because it is the fail-closed answer —
/// both mean *not admissible now*, and the alternative of returning `Fresh` or
/// `Stale` would admit a claim that has not begun.
///
/// The naming is nonetheless wrong for the case, and this is recorded as an
/// open question for the World Model owner rather than settled here: **should
/// the Validity axis gain a `NotYetInForce` member?** Adding one is a blueprint
/// §9.1 change, which is not this module's to make.
#[must_use]
pub fn validity_at(window: &ValidityWindow, now_ms: u64) -> Validity {
    if now_ms < window.valid_from_ms {
        // Not yet in force. See the doc comment — fail-closed, flagged.
        return Validity::Expired;
    }
    if let Some(end) = window.valid_to_ms {
        if now_ms >= end {
            return Validity::Expired;
        }
    }
    match window.staleness_budget_ms {
        None => Validity::Timeless,
        Some(budget) => {
            let deadline = window.valid_from_ms.saturating_add(budget);
            if now_ms > deadline {
                Validity::Stale
            } else {
                Validity::Fresh
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TrustAxes
// ---------------------------------------------------------------------------

/// Why a set of trust axes could not be built or moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustError {
    /// [`Origin::Predicted`] never appears in the evidence store (blueprint
    /// §20). The predictive subsystem shares the vocabulary, not the store.
    PredictedNotStorable,
    /// **Rule 7**: `Rejected` is terminal for the record. Rejecting an
    /// observation never deletes it, and nothing moves it back out.
    RejectedIsTerminal,
    /// A derivation was attempted with no inputs. Rule 5 folds the weakest
    /// input; with nothing to fold there is no defensible answer, and inventing
    /// one (`Pending`/`Uncorroborated`) would manufacture trust from thin air.
    DerivationNeedsInputs,
}

/// The three **stored** trust axes. Validity is the fourth axis and is
/// deliberately not a field — see the module docs and [`validity_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrustAxes {
    origin: Origin,
    corroboration: Corroboration,
    adjudication: Adjudication,
}

impl TrustAxes {
    /// Build the axes for a newly written record.
    ///
    /// # Errors
    ///
    /// [`TrustError::PredictedNotStorable`] if `origin` is [`Origin::Predicted`].
    pub fn new(
        origin: Origin,
        corroboration: Corroboration,
        adjudication: Adjudication,
    ) -> Result<Self, TrustError> {
        if matches!(origin, Origin::Predicted) {
            return Err(TrustError::PredictedNotStorable);
        }
        Ok(Self {
            origin,
            corroboration,
            adjudication,
        })
    }

    /// A freshly observed, uncorroborated, unruled claim — the common case.
    #[must_use]
    pub fn observed() -> Self {
        Self {
            origin: Origin::Observed,
            corroboration: Corroboration::Uncorroborated,
            adjudication: Adjudication::Pending,
        }
    }

    /// Origin. **Rule 1** — there is no corresponding setter, by design.
    #[must_use]
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// Corroboration as stored, before rule 3 is applied.
    #[must_use]
    pub fn corroboration(&self) -> Corroboration {
        self.corroboration
    }

    /// Adjudication **with rule 3 applied**: `Contradicted` reads as `Ambiguous`
    /// unless an adjudication rule has resolved it.
    ///
    /// A `Pending` claim with contradicting evidence is `Ambiguous` — that is
    /// rule 3. An explicitly `Confirmed` or `Rejected` claim keeps its ruling,
    /// because that is what *"unless an adjudication rule resolves it"* means.
    #[must_use]
    pub fn adjudication(&self) -> Adjudication {
        match (self.corroboration, self.adjudication) {
            (Corroboration::Contradicted(_), Adjudication::Pending) => Adjudication::Ambiguous,
            _ => self.adjudication,
        }
    }

    /// Adjudication exactly as stored, **without** rule 3. For persistence and
    /// tests; consumers want [`Self::adjudication`].
    #[must_use]
    pub fn adjudication_stored(&self) -> Adjudication {
        self.adjudication
    }

    /// Fold in agreeing evidence — rule 2.
    ///
    /// # Errors
    ///
    /// [`TrustError::RejectedIsTerminal`] if the record was rejected.
    pub fn agreed(self) -> Result<Self, TrustError> {
        self.guard_not_rejected()?;
        Ok(Self {
            corroboration: self.corroboration.agreed(),
            ..self
        })
    }

    /// Fold in disagreeing evidence — rule 2.
    ///
    /// # Errors
    ///
    /// [`TrustError::RejectedIsTerminal`] if the record was rejected.
    pub fn disagreed(self) -> Result<Self, TrustError> {
        self.guard_not_rejected()?;
        Ok(Self {
            corroboration: self.corroboration.disagreed(),
            ..self
        })
    }

    /// An operator rules the claim admissible — the adjudication half of
    /// **rule 4**, and the resolution escape hatch of **rule 3**.
    ///
    /// This settles *which* entity is meant. It **cannot rewrite geometry**:
    /// nothing here touches a payload, and an operator correcting a measured
    /// pose must instead create a `Correction` observation whose payload is
    /// visibly operator-sourced (P10). That half is the observation model's to
    /// enforce, and this method's inability to reach a payload is the reason it
    /// cannot be used to sidestep it.
    ///
    /// # Errors
    ///
    /// [`TrustError::RejectedIsTerminal`] if the record was rejected.
    pub fn operator_confirm(self) -> Result<Self, TrustError> {
        self.guard_not_rejected()?;
        Ok(Self {
            adjudication: Adjudication::Confirmed,
            ..self
        })
    }

    /// Rule the claim inadmissible — **rule 7**.
    ///
    /// Rejecting never deletes: this marks the record inadmissible to
    /// projections. Terminal *for the record*, not for the subject — a later
    /// observation about the same thing is a different record and is unaffected.
    ///
    /// # Errors
    ///
    /// [`TrustError::RejectedIsTerminal`] if already rejected. Rejecting twice
    /// is refused rather than treated as a no-op, so a caller that believes it
    /// is making a fresh ruling is told otherwise.
    pub fn reject(self) -> Result<Self, TrustError> {
        self.guard_not_rejected()?;
        Ok(Self {
            adjudication: Adjudication::Rejected,
            ..self
        })
    }

    fn guard_not_rejected(self) -> Result<(), TrustError> {
        if matches!(self.adjudication, Adjudication::Rejected) {
            return Err(TrustError::RejectedIsTerminal);
        }
        Ok(())
    }

    /// **Rule 5 — derived inherits the weakest input on every axis.**
    ///
    /// The anti-laundering rule, and the single most load-bearing line in the
    /// trust model: it prevents a chain of plausible inferences producing a
    /// high-confidence conclusion from low-confidence roots.
    ///
    /// Origin is always [`Origin::Derived`] — rule 1 fixes origin at write, and
    /// what is being written is a derivation.
    ///
    /// # A deliberate reading of "every axis"
    ///
    /// Rule 5's text says the weakest input is inherited *"on every axis"*,
    /// while the §9.1 mapping table shows `—` for the Derived row's
    /// corroboration column. The two can be read as disagreeing.
    ///
    /// This implementation follows the **rule text**, folding corroboration too,
    /// because that is the conservative direction: a derivation from a
    /// contradicted input stays contradicted rather than emerging with the
    /// contradiction dropped. Reading `—` as "not folded" would let a derivation
    /// launder away a conflict, which is precisely what rule 5 exists to stop.
    ///
    /// Recorded as an open question for the World Model owner rather than
    /// treated as settled: **does the table's `—` mean "not applicable" or
    /// "undefined"?** Under either reading the conservative fold is safe; under
    /// the second it is also the only defensible one.
    ///
    /// # Errors
    ///
    /// [`TrustError::DerivationNeedsInputs`] if `inputs` is empty.
    pub fn derive_from(inputs: &[Self]) -> Result<Self, TrustError> {
        let (first, rest) = inputs
            .split_first()
            .ok_or(TrustError::DerivationNeedsInputs)?;

        // Fold the RULE-3-APPLIED adjudication, not the stored one: a
        // contradicted-and-pending input is Ambiguous to every reader, and a
        // derivation must inherit what the input actually means, not the
        // unresolved value behind it.
        let mut corroboration = first.corroboration;
        let mut adjudication = first.adjudication();
        for input in rest {
            corroboration = corroboration.weakest(input.corroboration);
            adjudication = adjudication.weakest(input.adjudication());
        }

        Ok(Self {
            origin: Origin::Derived,
            corroboration,
            adjudication,
        })
    }

    /// Rule 5's validity fold, which cannot live on [`Self`] because validity is
    /// never stored (rule 6). Ask each input's validity at the same instant,
    /// then fold.
    ///
    /// Returns [`Validity::Timeless`] for an empty slice — the identity of the
    /// fold. A derivation with no temporal inputs has nothing that can go stale;
    /// note that [`Self::derive_from`] separately refuses an empty *axes* slice,
    /// so this identity is only reachable for a derivation whose inputs are all
    /// atemporal.
    #[must_use]
    pub fn derived_validity(input_validities: &[Validity]) -> Validity {
        input_validities
            .iter()
            .copied()
            .fold(Validity::Timeless, Validity::weakest)
    }
}

// ---------------------------------------------------------------------------
// Derived labels — §9.1's mapping table, and §9.3's grade
// ---------------------------------------------------------------------------

/// The familiar single-word state, derived from the axes for display.
///
/// This is the §9.1 table read left-to-right. It is **lossy by construction** —
/// that is the point of deriving it rather than storing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestedState {
    /// Sensed once, unruled, in force.
    Observed,
    /// Sensed and ruled admissible, in force.
    Confirmed,
    /// Stated by an operator and ruled admissible.
    OperatorConfirmed,
    /// Computed from other evidence, carrying its inputs' weakest axes.
    Derived,
    /// Brought in from an external dataset, unruled.
    Imported,
    /// Outside its validity window — dominates origin.
    Expired,
    /// Evidence in conflict. A **reportable state, not an error** (rule 3).
    Ambiguous,
    /// Ruled inadmissible. Terminal for the record (rule 7), and dominates
    /// every other label.
    Rejected,
    /// **The §9.1 table names no state for these axes.**
    ///
    /// Reachable today for one combination: an `Asserted` claim whose
    /// adjudication is not `Confirmed` — an operator has stated something and
    /// nothing has ruled on it. `OperatorConfirmed` would overstate it,
    /// `Observed` would claim a sensor saw it, and the table offers nothing else.
    ///
    /// Reported rather than guessed, on ADR-0040's own reasoning about
    /// `ResolutionOutcome`: collapsing distinct answers into one value is
    /// unrecoverable once done. This is **not** the blueprint's `Unknown`, which
    /// means *no admissible evidence* — here there is evidence, it simply has no
    /// familiar name.
    Unlabelled,
}

/// How many agreeing sources the §9.1 `Confirmed` row's `Corroborated(≥k)`
/// requires.
///
/// **The blueprint does not define `k`.** Two is the smallest value for which
/// "corroborated" means anything — one source agreeing with itself is not
/// corroboration — so that is what this uses. Named rather than inlined so the
/// policy is greppable and changeable in one place, and flagged as an open
/// question for the World Model owner: **what is `k`, and is it per-kind?**
pub const CORROBORATION_THRESHOLD_K: u32 = 2;

/// Collapse the axes to the familiar label — §9.1.
///
/// # The table is not a partition, so precedence is this function's to state
///
/// §9.1 shows how each *named* state decomposes into axes; it does not claim
/// every axis combination has a name, and its rows overlap (a `Derived` claim
/// that is also `Corroborated(≥k)` and `Confirmed` matches two rows). Collapsing
/// arbitrary axes to one label is therefore this function's construction on top
/// of the table, and the precedence below is stated rather than inherited:
///
/// 1. `Rejected`, then `Expired`, then `Ambiguous` — the three rows the table
///    itself marks as applying to *any* origin, so they dominate.
/// 2. `OperatorConfirmed` before `Confirmed` — it is the **more specific** of
///    the two, pinning origin to `Asserted` where `Confirmed` accepts any.
/// 3. Otherwise the origin-only rows.
///
/// # Two corrections against the table
///
/// An earlier version of this function got both of these wrong, in opposite
/// directions, and they are called out because one of them was unsafe:
///
/// * **`OperatorConfirmed` requires `Confirmed` adjudication.** It previously
///   returned for *any* `Asserted` claim, so an operator assertion that had not
///   been adjudicated was reported as operator-confirmed. That **overstates
///   trust**, which is the direction that matters.
/// * **`Confirmed` is "any origin".** It previously returned only for
///   `Observed`, hiding confirmed status on `Imported` and `Derived` claims.
///   That understates, which is safe but still wrong.
#[must_use]
pub fn requested_state(axes: &TrustAxes, validity: Validity) -> RequestedState {
    // (1) The rows the table marks "any origin".
    if matches!(axes.adjudication(), Adjudication::Rejected) {
        return RequestedState::Rejected;
    }
    if matches!(validity, Validity::Expired) {
        return RequestedState::Expired;
    }
    if matches!(axes.adjudication(), Adjudication::Ambiguous) {
        return RequestedState::Ambiguous;
    }

    let confirmed = matches!(axes.adjudication(), Adjudication::Confirmed);

    // (2) More specific first.
    if confirmed && matches!(axes.origin(), Origin::Asserted) {
        return RequestedState::OperatorConfirmed;
    }
    if confirmed
        && matches!(axes.corroboration(), Corroboration::Corroborated(n)
            if n >= CORROBORATION_THRESHOLD_K)
    {
        return RequestedState::Confirmed;
    }

    // (3) Origin-only rows.
    match axes.origin() {
        Origin::Derived => RequestedState::Derived,
        Origin::Imported => RequestedState::Imported,
        Origin::Observed | Origin::Predicted => RequestedState::Observed,
        // `Asserted` without `Confirmed` adjudication. The table names no state
        // for it — see [`RequestedState::Unlabelled`].
        Origin::Asserted => RequestedState::Unlabelled,
    }
}

/// A single advisory number for consumers that insist on one — §9.3.
///
/// # This is never an input to a safety decision
///
/// Blueprint §18 and ADR-0039 both say it: Kirra World is non-authoritative, and
/// this grade is the most collapsible thing it produces. It exists so a
/// conversational surface can say "I'm fairly sure"; it must not reach a
/// checker. Fence B is what makes that structural rather than advisory — nothing
/// in the safety closure can depend on this crate at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustGrade {
    /// Rejected, expired, or in unresolved conflict.
    Inadmissible,
    /// Admissible but weakly supported.
    Weak,
    /// Ordinary single-source evidence, in force.
    Adequate,
    /// Corroborated or operator-confirmed, in force.
    Strong,
}

/// Collapse the axes and a validity answer to one advisory grade — §9.3.
#[must_use]
pub fn trust_grade(axes: &TrustAxes, validity: Validity) -> TrustGrade {
    match axes.adjudication() {
        Adjudication::Rejected | Adjudication::Ambiguous => return TrustGrade::Inadmissible,
        _ => {}
    }
    if matches!(validity, Validity::Expired) {
        return TrustGrade::Inadmissible;
    }
    let corroborated = matches!(axes.corroboration(), Corroboration::Corroborated(n) if n >= 2);
    let confirmed = matches!(axes.adjudication(), Adjudication::Confirmed);
    match (corroborated || confirmed, validity) {
        (true, Validity::Stale) => TrustGrade::Adequate,
        (true, _) => TrustGrade::Strong,
        (false, Validity::Stale) => TrustGrade::Weak,
        (false, _) => TrustGrade::Adequate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Rule 1: origin never changes ------------------------------------

    #[test]
    fn no_transition_changes_origin() {
        let a = TrustAxes::new(
            Origin::Imported,
            Corroboration::Uncorroborated,
            Adjudication::Pending,
        )
        .unwrap();
        assert_eq!(a.agreed().unwrap().origin(), Origin::Imported);
        assert_eq!(a.disagreed().unwrap().origin(), Origin::Imported);
        assert_eq!(a.operator_confirm().unwrap().origin(), Origin::Imported);
        assert_eq!(a.reject().unwrap().origin(), Origin::Imported);
    }

    #[test]
    fn predicted_is_refused_by_the_store_facing_constructor() {
        assert_eq!(
            TrustAxes::new(
                Origin::Predicted,
                Corroboration::Uncorroborated,
                Adjudication::Pending
            ),
            Err(TrustError::PredictedNotStorable)
        );
    }

    // -- Rule 2: monotonic in evidence -----------------------------------

    #[test]
    fn agreement_increments_and_disagreement_switches_kind() {
        let c = Corroboration::Uncorroborated.agreed().agreed();
        assert_eq!(c, Corroboration::Corroborated(2));
        // Not Corroborated(1): disagreement is a change of kind, not a decrement.
        assert_eq!(c.disagreed(), Corroboration::Contradicted(1));
    }

    #[test]
    fn agreement_does_not_clear_a_contradiction() {
        let c = Corroboration::Contradicted(1).agreed().agreed().agreed();
        assert_eq!(c, Corroboration::Contradicted(1));
    }

    #[test]
    fn corroboration_counts_saturate_rather_than_overflow() {
        assert_eq!(
            Corroboration::Corroborated(u32::MAX).agreed(),
            Corroboration::Corroborated(u32::MAX)
        );
        assert_eq!(
            Corroboration::Contradicted(u32::MAX).disagreed(),
            Corroboration::Contradicted(u32::MAX)
        );
    }

    // -- Rule 3: contradicted implies ambiguous --------------------------

    #[test]
    fn contradicted_and_pending_reads_as_ambiguous() {
        let a = TrustAxes::observed().disagreed().unwrap();
        assert_eq!(a.adjudication(), Adjudication::Ambiguous);
        // ...but the stored value is untouched, so the ruling is recoverable.
        assert_eq!(a.adjudication_stored(), Adjudication::Pending);
    }

    #[test]
    fn an_explicit_ruling_survives_contradiction() {
        let a = TrustAxes::observed()
            .disagreed()
            .unwrap()
            .operator_confirm()
            .unwrap();
        assert_eq!(a.adjudication(), Adjudication::Confirmed);
    }

    // -- Rule 5: the anti-laundering rule --------------------------------

    #[test]
    fn derivation_inherits_the_weakest_adjudication() {
        let strong = TrustAxes::observed().operator_confirm().unwrap();
        let weak = TrustAxes::observed(); // Pending
        let d = TrustAxes::derive_from(&[strong, weak]).unwrap();
        assert_eq!(d.origin(), Origin::Derived);
        assert_eq!(d.adjudication(), Adjudication::Pending);
    }

    #[test]
    fn derivation_cannot_launder_away_a_contradiction() {
        // THE rule. A contradicted input must not emerge from a derivation clean.
        let contradicted = TrustAxes::observed().disagreed().unwrap();
        let clean = TrustAxes::observed()
            .agreed()
            .unwrap()
            .agreed()
            .unwrap()
            .operator_confirm()
            .unwrap();

        let d = TrustAxes::derive_from(&[clean, contradicted]).unwrap();

        assert_eq!(d.corroboration(), Corroboration::Contradicted(1));
        assert_eq!(d.adjudication(), Adjudication::Ambiguous);
    }

    #[test]
    fn a_chain_of_derivations_cannot_climb() {
        // The pathology rule 5 names: plausible inferences producing a
        // high-confidence conclusion from low-confidence roots.
        //
        // Each step mixes the running value with a DELIBERATELY STRONG input.
        // That is load-bearing: with a single input the weakest-fold and a
        // strongest-fold are the same function, so a one-input chain would pass
        // even with the rule inverted. A negative control caught exactly that,
        // and this is the corrected shape.
        let strong = TrustAxes::observed()
            .agreed()
            .unwrap()
            .agreed()
            .unwrap()
            .operator_confirm()
            .unwrap();

        let root = TrustAxes::observed().disagreed().unwrap();
        let mut cur = TrustAxes::derive_from(&[root]).unwrap();
        for _ in 0..10 {
            cur = TrustAxes::derive_from(&[cur, strong]).unwrap();
        }

        // Ten rounds of laundering through a confirmed, doubly-corroborated
        // partner, and the conflict is still there.
        assert_eq!(cur.adjudication(), Adjudication::Ambiguous);
        assert_eq!(cur.corroboration(), Corroboration::Contradicted(1));
    }

    #[test]
    fn weakest_corroboration_takes_more_contradictions_and_fewer_agreements() {
        assert_eq!(
            Corroboration::Contradicted(2).weakest(Corroboration::Contradicted(5)),
            Corroboration::Contradicted(5)
        );
        assert_eq!(
            Corroboration::Corroborated(9).weakest(Corroboration::Corroborated(3)),
            Corroboration::Corroborated(3)
        );
        assert_eq!(
            Corroboration::Corroborated(9).weakest(Corroboration::Uncorroborated),
            Corroboration::Uncorroborated
        );
    }

    #[test]
    fn weakest_is_commutative_across_every_pair() {
        let all = [
            Corroboration::Uncorroborated,
            Corroboration::Corroborated(1),
            Corroboration::Corroborated(4),
            Corroboration::Contradicted(1),
            Corroboration::Contradicted(4),
        ];
        for a in all {
            for b in all {
                assert_eq!(a.weakest(b), b.weakest(a), "not commutative: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn derivation_with_no_inputs_is_refused_rather_than_invented() {
        assert_eq!(
            TrustAxes::derive_from(&[]),
            Err(TrustError::DerivationNeedsInputs)
        );
    }

    #[test]
    fn derived_validity_folds_to_the_weakest() {
        assert_eq!(
            TrustAxes::derived_validity(&[Validity::Timeless, Validity::Stale, Validity::Fresh]),
            Validity::Stale
        );
        assert_eq!(
            TrustAxes::derived_validity(&[Validity::Fresh, Validity::Expired]),
            Validity::Expired
        );
        assert_eq!(TrustAxes::derived_validity(&[]), Validity::Timeless);
    }

    // -- Rule 6: computed at read time -----------------------------------

    #[test]
    fn freshness_is_a_question_asked_with_a_clock() {
        let w = ValidityWindow {
            valid_from_ms: 1_000,
            valid_to_ms: None,
            staleness_budget_ms: Some(500),
        };
        // Same stored facts, three different answers — which is rule 6's point.
        assert_eq!(validity_at(&w, 1_200), Validity::Fresh);
        assert_eq!(validity_at(&w, 1_500), Validity::Fresh); // boundary: inclusive
        assert_eq!(validity_at(&w, 1_501), Validity::Stale);
    }

    #[test]
    fn expiry_beats_staleness() {
        let w = ValidityWindow {
            valid_from_ms: 0,
            valid_to_ms: Some(100),
            staleness_budget_ms: Some(10),
        };
        // Past both; must report the stronger restriction, not the first found.
        assert_eq!(validity_at(&w, 500), Validity::Expired);
    }

    #[test]
    fn no_staleness_budget_means_timeless() {
        assert_eq!(
            validity_at(&ValidityWindow::timeless(0), u64::MAX),
            Validity::Timeless
        );
    }

    #[test]
    fn a_claim_not_yet_in_force_is_never_fresh() {
        // The flagged spec gap: fail-closed is what matters, the NAME is open.
        let w = ValidityWindow {
            valid_from_ms: 1_000,
            valid_to_ms: None,
            staleness_budget_ms: Some(500),
        };
        assert_ne!(validity_at(&w, 999), Validity::Fresh);
        assert_eq!(validity_at(&w, 999), Validity::Expired);
    }

    #[test]
    fn staleness_deadline_saturates_rather_than_overflowing() {
        let w = ValidityWindow {
            valid_from_ms: u64::MAX,
            valid_to_ms: None,
            staleness_budget_ms: Some(u64::MAX),
        };
        assert_eq!(validity_at(&w, u64::MAX), Validity::Fresh);
    }

    // -- Rule 7: rejection is terminal -----------------------------------

    #[test]
    fn nothing_moves_a_rejected_record() {
        let r = TrustAxes::observed().reject().unwrap();
        assert_eq!(r.agreed(), Err(TrustError::RejectedIsTerminal));
        assert_eq!(r.disagreed(), Err(TrustError::RejectedIsTerminal));
        assert_eq!(r.operator_confirm(), Err(TrustError::RejectedIsTerminal));
        assert_eq!(r.reject(), Err(TrustError::RejectedIsTerminal));
    }

    #[test]
    fn a_rejected_input_poisons_a_derivation() {
        // Rule 7 is terminal for the RECORD; rule 5 then carries it downstream.
        let rejected = TrustAxes::observed().reject().unwrap();
        let good = TrustAxes::observed().operator_confirm().unwrap();
        let d = TrustAxes::derive_from(&[good, rejected]).unwrap();
        assert_eq!(d.adjudication(), Adjudication::Rejected);
    }

    // -- Derived labels ---------------------------------------------------

    #[test]
    fn requested_states_match_the_blueprint_table() {
        let observed = TrustAxes::observed();
        assert_eq!(
            requested_state(&observed, Validity::Fresh),
            RequestedState::Observed
        );

        // Corroborated(2) — the table's `Corroborated(>=k)`.
        let confirmed = TrustAxes::observed()
            .agreed()
            .unwrap()
            .agreed()
            .unwrap()
            .operator_confirm()
            .unwrap();
        assert_eq!(
            requested_state(&confirmed, Validity::Fresh),
            RequestedState::Confirmed
        );

        let asserted = TrustAxes::new(
            Origin::Asserted,
            Corroboration::Uncorroborated,
            Adjudication::Confirmed,
        )
        .unwrap();
        assert_eq!(
            requested_state(&asserted, Validity::Timeless),
            RequestedState::OperatorConfirmed
        );

        let imported = TrustAxes::new(
            Origin::Imported,
            Corroboration::Uncorroborated,
            Adjudication::Pending,
        )
        .unwrap();
        assert_eq!(
            requested_state(&imported, Validity::Timeless),
            RequestedState::Imported
        );
    }

    #[test]
    fn expiry_and_rejection_dominate_origin() {
        let imported = TrustAxes::new(
            Origin::Imported,
            Corroboration::Uncorroborated,
            Adjudication::Pending,
        )
        .unwrap();
        assert_eq!(
            requested_state(&imported, Validity::Expired),
            RequestedState::Expired
        );
        assert_eq!(
            requested_state(&imported.reject().unwrap(), Validity::Fresh),
            RequestedState::Rejected
        );
    }

    #[test]
    fn an_unadjudicated_operator_assertion_is_not_operator_confirmed() {
        // REGRESSION. The earlier version returned OperatorConfirmed for ANY
        // Asserted origin, so a claim nobody had ruled on was reported as
        // operator-confirmed — OVERSTATING trust, the direction that matters.
        // The table requires `Confirmed` adjudication for that row.
        let pending = TrustAxes::new(
            Origin::Asserted,
            Corroboration::Uncorroborated,
            Adjudication::Pending,
        )
        .unwrap();
        assert_ne!(
            requested_state(&pending, Validity::Fresh),
            RequestedState::OperatorConfirmed
        );
        assert_eq!(
            requested_state(&pending, Validity::Fresh),
            RequestedState::Unlabelled
        );

        // ...and the confirmed one still is.
        assert_eq!(
            requested_state(&pending.operator_confirm().unwrap(), Validity::Fresh),
            RequestedState::OperatorConfirmed
        );
    }

    #[test]
    fn confirmed_is_any_origin_not_just_observed() {
        // REGRESSION. The earlier version only reported Confirmed for Observed
        // origin, hiding confirmed status on Imported and Derived claims. The
        // table's Confirmed row reads "any origin".
        for origin in [Origin::Imported, Origin::Derived, Origin::Observed] {
            let axes = TrustAxes::new(
                origin,
                Corroboration::Corroborated(CORROBORATION_THRESHOLD_K),
                Adjudication::Confirmed,
            )
            .unwrap();
            assert_eq!(
                requested_state(&axes, Validity::Fresh),
                RequestedState::Confirmed,
                "origin {origin:?} should reach Confirmed"
            );
        }
    }

    #[test]
    fn confirmed_needs_k_agreeing_sources() {
        let below = TrustAxes::new(
            Origin::Observed,
            Corroboration::Corroborated(CORROBORATION_THRESHOLD_K - 1),
            Adjudication::Confirmed,
        )
        .unwrap();
        assert_eq!(
            requested_state(&below, Validity::Fresh),
            RequestedState::Observed
        );
    }

    #[test]
    fn operator_confirmed_outranks_confirmed_being_more_specific() {
        // Both rows match; the one that pins origin wins.
        let axes = TrustAxes::new(
            Origin::Asserted,
            Corroboration::Corroborated(CORROBORATION_THRESHOLD_K),
            Adjudication::Confirmed,
        )
        .unwrap();
        assert_eq!(
            requested_state(&axes, Validity::Fresh),
            RequestedState::OperatorConfirmed
        );
    }

    #[test]
    fn ambiguity_is_reportable_rather_than_an_error() {
        let a = TrustAxes::observed().disagreed().unwrap();
        assert_eq!(
            requested_state(&a, Validity::Fresh),
            RequestedState::Ambiguous
        );
    }

    // -- The advisory grade -----------------------------------------------

    #[test]
    fn the_grade_never_rates_an_inadmissible_claim_above_inadmissible() {
        let rejected = TrustAxes::observed().reject().unwrap();
        let ambiguous = TrustAxes::observed().disagreed().unwrap();
        assert_eq!(
            trust_grade(&rejected, Validity::Fresh),
            TrustGrade::Inadmissible
        );
        assert_eq!(
            trust_grade(&ambiguous, Validity::Fresh),
            TrustGrade::Inadmissible
        );
        assert_eq!(
            trust_grade(&TrustAxes::observed(), Validity::Expired),
            TrustGrade::Inadmissible
        );
    }

    #[test]
    fn corroboration_and_confirmation_lift_the_grade() {
        let plain = TrustAxes::observed();
        let corroborated = plain.agreed().unwrap().agreed().unwrap();
        assert_eq!(trust_grade(&plain, Validity::Fresh), TrustGrade::Adequate);
        assert_eq!(
            trust_grade(&corroborated, Validity::Fresh),
            TrustGrade::Strong
        );
        assert!(trust_grade(&corroborated, Validity::Fresh) > trust_grade(&plain, Validity::Fresh));
    }

    #[test]
    fn staleness_lowers_the_grade_without_making_it_inadmissible() {
        let plain = TrustAxes::observed();
        assert_eq!(trust_grade(&plain, Validity::Stale), TrustGrade::Weak);
        assert!(trust_grade(&plain, Validity::Stale) > TrustGrade::Inadmissible);
    }
}
