//! **Tier 2 — identity adjudication as recorded events** (§6.3, §14.1).
//!
//! `WM_SCOPE.md` §5: *"Merge and split are **events, never destructive edits**.
//! This is what makes an `EntityId` revisable, and it is precisely what a store
//! built on a bare opaque key can never retrofit — the key would have already
//! lost its own history."*
//!
//! [`mod@entity`](crate::entity) already carries the *states* an adjudication
//! produces — `Merged { into }`, `Split { from }`, `Retired`. What it has never
//! carried is the **act**: the thing that has a time, a justification, and an
//! author, and that can be replayed. A lifecycle field alone answers *"what is
//! this entity now"* and cannot answer *"why, and on what evidence"*, which is
//! the question §6.3 says every system that gets identity wrong has destroyed by
//! the time anyone asks it.
//!
//! # What is refused, and why each refusal is here rather than remembered
//!
//! Every rule below is a constructor refusal, so the malformed adjudication is
//! unrepresentable rather than merely discouraged:
//!
//! * **A merge into one of its own sources** would make an entity its own
//!   redirect target. §6.3 promises the old ids *"answer with a redirect"*
//!   forever; a self-redirect is a resolution loop, and the failure would appear
//!   at read time, in a projection, long after the event that caused it.
//! * **A split into fewer than two** is not a *partition*. Into one it is a
//!   no-op that nonetheless records an identity change; into zero it is
//!   destruction, which is what [`ForgetEntity`] is for and what §14.1 reserves
//!   for a `Redact` operation with *"its own ADR"*. **This rule scopes the type
//!   to partition-shaped splits** — see the constructor-neutrality note below.
//! * **Duplicate sources or destinations** leave it ambiguous whether one event
//!   or two occurred, which is unanswerable afterwards from the record.
//! * **No supporting evidence** makes the adjudication an assertion. The whole
//!   point of recording it is that someone can later ask what it rested on.
//!
//! # `Evidence` is a supplied reading, not an inherited one
//!
//! §14.1 writes `MergeEntities(from[], into, Evidence)` and never says what
//! `Evidence` is; it appears as an unelaborated parameter name in three verb
//! signatures and nowhere else in the blueprint. Same shape as the observation
//! kinds — a *specification* gap, so it is ruled here and labelled rather than
//! quietly invented.
//!
//! **The reading: evidence is the set of observations that justify the
//! judgement**, carried as [`Justification`]. Not an [`EvidenceDigest`](crate::evidence::EvidenceDigest)
//! — a digest is the adjudication's own position in the chain, which the store
//! computes *after* appending it, so it cannot be a construction input without
//! inventing a value that does not exist yet.
//!
//! Operator teaching is covered without a second path: an operator's ruling is
//! itself recorded as an observation with
//! [`SourceClass::Operator`](crate::observation::SourceClass::Operator), so
//! "because the operator said so" is a justification with a real
//! [`ObservationId`] behind it rather than an exemption from the rule.
//!
//! # The consequence a split cannot state
//!
//! An adjudication reports the lifecycle transitions it implies
//! ([`IdentityAdjudication::resulting_lifecycles`]) so the two halves of the
//! model cannot drift apart — every transition it names is one
//! [`Lifecycle::advance_to`] permits.
//!
//! # What became of a split's source — **ruled**
//!
//! `entity.rs` carried this open for a while: *"is `Split(from)` a live origin
//! marker or a terminal marker on the entity that was split? The two readings
//! differ in whether the original survives a split."*
//!
//! **`KIRRA-WM-SPLIT-SURVIVAL-001`, adopted 2026-08-08**, answers it by
//! admitting that the two readings answer *different questions*:
//!
//! * [`SplitEntity::partition`] — the source **was** its successors. It is
//!   [`Lifecycle::Superseded`]: terminal, still resolvable, redirecting to all
//!   of them.
//! * [`SplitEntity::subtract`] — pieces were carved off a source that
//!   **survives**. It takes no transition, because nothing moved.
//!
//! ## The finding that produced the ruling, kept because it is the useful part
//!
//! The module used to say the source's fate was "not stated, because it is not
//! decided". That was true of `resulting_lifecycles` and **false as a claim
//! about this module**: the constructor had already chosen. `SplitTooNarrow`
//! (fewer than two destinations) and `SplitIntoSelf` together refused **both
//! spellings of a surviving original** — `into = [piece]` and
//! `into = [source, piece]` — so the ordinary subtraction case (you believed one
//! pallet; there is a pallet with a box on it) was *unrepresentable*, while the
//! prose claimed neutrality.
//!
//! An open question the implementation has quietly closed is worse than one
//! still open, because the next reader takes the constraint as considered. That
//! is what `subtract` fixes, and the record of it stays here rather than
//! disappearing when the box was ticked.
//!
//! `unresolved_consequence` is **gone**. It was honest while the question was
//! open and became the hiding place the proposal predicted the moment a real
//! answer existed.
//!
//! # ADR-0042 condition (1)
//!
//! Nothing here may become a required safety input: no `CorridorSource`, no
//! actuator path, no release token. Pure data and pure functions, zero
//! dependencies, same as the rest of this crate.

use crate::entity::Lifecycle;
use crate::observation::DomainInstant;
use crate::reference::EntityId;
use crate::reference::ObservationId;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why an adjudication was refused.
///
/// Each variant names the offending value, because an identity event is
/// reviewed long after it was rejected and *"invalid merge"* sends the reader
/// back to the payload to work out which id was the problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationError {
    /// No supporting observation was supplied.
    ///
    /// An adjudication with nothing behind it is an assertion. It may still be
    /// *correct*; it is not **evidence**, and the record could never say so.
    NoJustification,

    /// The same observation was cited twice in one justification.
    ///
    /// Refused rather than deduplicated: a caller who cited a thing twice may
    /// have meant two different observations and mistyped one of them, and
    /// silently collapsing that hides the mistake at the only moment it is
    /// cheap to catch.
    DuplicateJustification {
        /// The observation cited more than once.
        observation: ObservationId,
    },

    /// A merge listed no source entities.
    EmptyMerge,

    /// A merge's destination is also one of its sources.
    ///
    /// The resulting `Merged { into }` would point the entity at itself.
    MergeIntoSelf {
        /// The entity that appeared on both sides.
        entity: EntityId,
    },

    /// The same entity appeared twice among a merge's sources.
    DuplicateSource {
        /// The repeated entity.
        entity: EntityId,
    },

    /// A split named fewer than two destinations.
    ///
    /// Into one it is a no-op recorded as an identity change; into zero it is
    /// destruction, which this module deliberately cannot express.
    ///
    /// **This is a partition rule, not a universal one.** Together with
    /// [`Self::SplitIntoSelf`] it makes a *surviving* original unrepresentable,
    /// which is a position on the open question rather than neutrality about it
    /// — see the module docs and `KIRRA-WM-SPLIT-SURVIVAL-001`.
    SplitTooNarrow {
        /// How many destinations were supplied.
        found: usize,
    },

    /// The same entity appeared twice among a split's destinations.
    DuplicateDestination {
        /// The repeated entity.
        entity: EntityId,
    },

    /// A split named its own source among its destinations.
    ///
    /// The piece would carry `Split { from }` pointing at itself.
    ///
    /// Correct for a partition, and **the other half of what makes subtraction
    /// unrepresentable** — a caller spelling "the source is one of the pieces"
    /// lands here. See `KIRRA-WM-SPLIT-SURVIVAL-001`.
    SplitIntoSelf {
        /// The entity that appeared on both sides.
        entity: EntityId,
    },

    /// A retirement carried an empty or whitespace-only reason.
    ///
    /// `ForgetEntity` suppresses an entity from default projections. An
    /// operator meeting that absence later has only the reason to go on.
    EmptyReason,
}

impl core::fmt::Display for AdjudicationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoJustification => {
                write!(f, "an adjudication must cite at least one observation")
            }
            Self::DuplicateJustification { observation } => write!(
                f,
                "observation {:?} was cited twice in one justification",
                observation.as_str()
            ),
            Self::EmptyMerge => write!(f, "a merge must have at least one source"),
            Self::MergeIntoSelf { entity } => write!(
                f,
                "entity {:?} is both a source and the destination of the merge",
                entity.as_str()
            ),
            Self::DuplicateSource { entity } => write!(
                f,
                "entity {:?} was listed twice as a merge source",
                entity.as_str()
            ),
            Self::SplitTooNarrow { found } => {
                write!(f, "a split needs at least two destinations, got {found}")
            }
            Self::DuplicateDestination { entity } => write!(
                f,
                "entity {:?} was listed twice as a split destination",
                entity.as_str()
            ),
            Self::SplitIntoSelf { entity } => write!(
                f,
                "entity {:?} is both the source and a destination of the split",
                entity.as_str()
            ),
            Self::EmptyReason => write!(f, "a retirement must carry a reason"),
        }
    }
}

// ---------------------------------------------------------------------------
// Justification — the supplied reading of `Evidence`
// ---------------------------------------------------------------------------

/// The observations an identity judgement rests on.
///
/// One type shared by all three adjudications, so the rule is stated once. Two
/// copies of "non-empty and duplicate-free" is two chances to get it right in
/// one place and wrong in another, and the wrong one would be the one nobody
/// wrote a test for.
///
/// **Order is preserved, not sorted.** The caller's ordering is part of what it
/// recorded — typically the order the evidence arrived — and a constructor that
/// tidied it would be editing the record. Same "validate, never normalize"
/// discipline as [`EvidenceDigest`](crate::evidence::EvidenceDigest), for the
/// same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Justification(Vec<ObservationId>);

// No `is_empty` here, and its absence is deliberate rather than an oversight
// -- hence the `allow`. The constructor refuses the empty case, so any
// `is_empty` would return a CONSTANT `false`, and `!j.is_empty()` would be a
// tautology that reads exactly like a real assertion. Offering it invites the
// vacuous check; not offering it makes the check unwriteable. Anything meaning
// "this really cites something" reads `observations()`.
#[allow(clippy::len_without_is_empty)]
impl Justification {
    /// Admit a non-empty, duplicate-free set of supporting observations.
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::NoJustification`] if empty;
    /// [`AdjudicationError::DuplicateJustification`] naming the first
    /// observation cited twice.
    pub fn new(
        observations: impl IntoIterator<Item = ObservationId>,
    ) -> Result<Self, AdjudicationError> {
        let observations: Vec<ObservationId> = observations.into_iter().collect();
        if observations.is_empty() {
            return Err(AdjudicationError::NoJustification);
        }
        if let Some(dup) = first_duplicate(&observations) {
            return Err(AdjudicationError::DuplicateJustification {
                observation: dup.clone(),
            });
        }
        Ok(Self(observations))
    }

    /// The supporting observations, in the order they were cited.
    #[must_use]
    pub fn observations(&self) -> &[ObservationId] {
        &self.0
    }

    /// How many observations support this judgement.
    ///
    /// Never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// The first value that appears more than once, if any.
///
/// Quadratic, and deliberately so: these are hand-authored identity events with
/// a handful of members, not a hot path, and the alternative (sorting into a
/// set) would need `Ord` on every id type for no gain at this size.
fn first_duplicate<T: PartialEq>(items: &[T]) -> Option<&T> {
    items
        .iter()
        .enumerate()
        .find(|(i, item)| items[..*i].contains(item))
        .map(|(_, item)| item)
}

// ---------------------------------------------------------------------------
// Retirement reason
// ---------------------------------------------------------------------------

/// Why an entity was retired.
///
/// Free text rather than an enum, because the blueprint enumerates no reasons
/// and inventing a closed set here would force real retirements into whichever
/// variant fit worst. Validated non-empty, never normalized — the operator's
/// words are the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirementReason(String);

impl RetirementReason {
    /// Admit a non-empty reason.
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::EmptyReason`] if empty or whitespace-only.
    pub fn new(reason: impl Into<String>) -> Result<Self, AdjudicationError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(AdjudicationError::EmptyReason);
        }
        Ok(Self(reason))
    }

    /// The reason as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// The three adjudications
// ---------------------------------------------------------------------------

/// **`MergeEntities(from[], into, Evidence)`** — §14.1.
///
/// Several entities are judged to have been one thing all along. Each source
/// becomes [`Lifecycle::Merged`] pointing at the destination, and — §6.3 —
/// *"both original IDs remain resolvable forever and answer with a redirect."*
///
/// The destination gets **no** transition. It was already an entity and still
/// is; a merge tells you what the *others* were, not that the survivor changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEntities {
    sources: Vec<EntityId>,
    into: EntityId,
    justification: Justification,
    at: DomainInstant,
}

impl MergeEntities {
    /// Adjudicate that `sources` and `into` are one entity.
    ///
    /// `at` is supplied rather than read from a clock: this crate has no
    /// dependencies and no clock, and the layer that appends the event is the
    /// one that knows what time it recorded it. Same core/adapter split as
    /// identity minting.
    ///
    /// It is a [`DomainInstant`] rather than a bare integer for the reason
    /// [`crate::relationship`] already uses one for its transaction time: the
    /// instant has to say **which clock it came from**, or two adjudications
    /// stamped on unsynchronized clocks order confidently and wrongly. A bare
    /// `i64` would also make a negative timestamp representable, and would be
    /// the store's SQLite spelling leaking up into the domain core.
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::EmptyMerge`] with no sources;
    /// [`AdjudicationError::DuplicateSource`] naming a repeated source;
    /// [`AdjudicationError::MergeIntoSelf`] if the destination is also a source.
    pub fn new(
        sources: impl IntoIterator<Item = EntityId>,
        into: EntityId,
        justification: Justification,
        at: DomainInstant,
    ) -> Result<Self, AdjudicationError> {
        let sources: Vec<EntityId> = sources.into_iter().collect();
        if sources.is_empty() {
            return Err(AdjudicationError::EmptyMerge);
        }
        if let Some(dup) = first_duplicate(&sources) {
            return Err(AdjudicationError::DuplicateSource {
                entity: dup.clone(),
            });
        }
        if sources.contains(&into) {
            return Err(AdjudicationError::MergeIntoSelf { entity: into });
        }
        Ok(Self {
            sources,
            into,
            justification,
            at,
        })
    }

    /// The entities being folded in. Never empty.
    #[must_use]
    pub fn sources(&self) -> &[EntityId] {
        &self.sources
    }

    /// The surviving entity.
    #[must_use]
    pub fn into_entity(&self) -> &EntityId {
        &self.into
    }

    /// What this judgement rests on.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        &self.justification
    }

    /// When the adjudication was made, on a clock that names itself.
    #[must_use]
    pub fn at(&self) -> DomainInstant {
        self.at
    }
}

/// Which **shape** of split an event records.
///
/// `KIRRA-WM-SPLIT-SURVIVAL-001` §5, adopted 2026-08-08: *"partition and
/// subtraction are separate constructors with separate rules rather than one
/// constructor with a flag. A flag invites a caller to pass the wrong one; two
/// constructors make the wrong call not compile."*
///
/// So this type is what an event **reports**, read back off a constructed
/// value. It is not an argument any constructor takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitShape {
    /// The source **was** these successors, and is superseded by them.
    ///
    /// "You believed there was one thing; it was really these N." The source
    /// stops corresponding to anything and answers with a redirect to all of
    /// them — [`crate::entity::Lifecycle::Superseded`].
    Partition,
    /// Pieces were **separated out** of a source that survives.
    ///
    /// "You believed there was one pallet; there is a pallet with a box on it."
    /// The pallet did not stop existing. This is the shape
    /// `KIRRA-WM-SPLIT-SURVIVAL-001` §1 found was *unrepresentable*: both
    /// spellings of a surviving original were refused, by `SplitTooNarrow` and
    /// by `SplitIntoSelf`.
    Subtraction,
}

/// **`SplitEntity(from, into[], Evidence)`** — §14.1.
///
/// One entity is judged to have been several things, or to have had pieces
/// carved off it. Each destination becomes [`Lifecycle::Split`] carrying its
/// origin; what happens to the SOURCE depends on the shape.
///
/// **Both shapes are now representable.** This type modelled partition only
/// until 2026-08-08, and said so — but the neutrality was in the prose and not
/// in the type: `SplitTooNarrow` and `SplitIntoSelf` between them refused both
/// spellings of a surviving original. `KIRRA-WM-SPLIT-SURVIVAL-001` §1 recorded
/// that finding and §5 supplied the fix — two constructors, [`Self::partition`]
/// and [`Self::subtract`], with separate rules.
///
/// The source's fate is no longer open: a partition supersedes it
/// ([`crate::entity::Lifecycle::Superseded`]), a subtraction leaves it live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitEntity {
    source: EntityId,
    shape: SplitShape,
    into: Vec<EntityId>,
    justification: Justification,
    at: DomainInstant,
}

impl SplitEntity {
    /// **Partition** — `source` was really the entities in `into`.
    ///
    /// The source is superseded by them and answers with a redirect to all;
    /// see [`crate::entity::Lifecycle::Superseded`].
    ///
    /// This is what `SplitEntity::new` was, renamed rather than reinterpreted:
    /// its rules already implemented partition and only prose claimed
    /// neutrality (`KIRRA-WM-SPLIT-SURVIVAL-001` §1).
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::SplitTooNarrow`] with fewer than two destinations —
    /// a partition into one is not a partition;
    /// [`AdjudicationError::DuplicateDestination`] naming a repeated
    /// destination; [`AdjudicationError::SplitIntoSelf`] if the source is also a
    /// destination, which would be the surviving-original spelling that belongs
    /// to [`Self::subtract`].
    pub fn partition(
        source: EntityId,
        into: impl IntoIterator<Item = EntityId>,
        justification: Justification,
        at: DomainInstant,
    ) -> Result<Self, AdjudicationError> {
        let into: Vec<EntityId> = into.into_iter().collect();
        if into.len() < 2 {
            return Err(AdjudicationError::SplitTooNarrow { found: into.len() });
        }
        Self::build(source, SplitShape::Partition, into, justification, at)
    }

    /// **Subtraction** — pieces were separated out of a source that survives.
    ///
    /// The shape §1 found unrepresentable: `into = [piece]` was refused by
    /// `SplitTooNarrow` and `into = [source, piece]` by `SplitIntoSelf`, so
    /// both spellings of a surviving original were impossible.
    ///
    /// The source is **not** named in `separated` and takes no lifecycle
    /// consequence — it survives with its identity intact, which is the whole
    /// difference from a partition.
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::SplitTooNarrow`] with no pieces at all — separating
    /// nothing is not a subtraction;
    /// [`AdjudicationError::DuplicateDestination`] naming a repeated piece;
    /// [`AdjudicationError::SplitIntoSelf`] if the source is listed among the
    /// pieces, which would say it was separated from itself.
    pub fn subtract(
        source: EntityId,
        separated: impl IntoIterator<Item = EntityId>,
        justification: Justification,
        at: DomainInstant,
    ) -> Result<Self, AdjudicationError> {
        let separated: Vec<EntityId> = separated.into_iter().collect();
        if separated.is_empty() {
            return Err(AdjudicationError::SplitTooNarrow { found: 0 });
        }
        Self::build(
            source,
            SplitShape::Subtraction,
            separated,
            justification,
            at,
        )
    }

    /// The rules both shapes share: no duplicates, and the source is never one
    /// of its own destinations.
    fn build(
        source: EntityId,
        shape: SplitShape,
        into: Vec<EntityId>,
        justification: Justification,
        at: DomainInstant,
    ) -> Result<Self, AdjudicationError> {
        if let Some(dup) = first_duplicate(&into) {
            return Err(AdjudicationError::DuplicateDestination {
                entity: dup.clone(),
            });
        }
        if into.contains(&source) {
            return Err(AdjudicationError::SplitIntoSelf { entity: source });
        }
        Ok(Self {
            source,
            shape,
            into,
            justification,
            at,
        })
    }

    /// Which shape of split this is.
    #[must_use]
    pub fn shape(&self) -> SplitShape {
        self.shape
    }

    /// The entity being split.
    #[must_use]
    pub fn source(&self) -> &EntityId {
        &self.source
    }

    /// The entities it turned out to be — or, under
    /// [`SplitShape::Subtraction`], the ones separated *off* it.
    ///
    /// **The cardinality depends on the shape**, so read [`Self::shape`] before
    /// relying on it: a [`SplitShape::Partition`] has at least two (fewer is not
    /// a partition), a `Subtraction` at least one. Neither is ever empty, and
    /// neither ever names the source.
    #[must_use]
    pub fn destinations(&self) -> &[EntityId] {
        &self.into
    }

    /// What this judgement rests on.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        &self.justification
    }

    /// When the adjudication was made, on a clock that names itself.
    #[must_use]
    pub fn at(&self) -> DomainInstant {
        self.at
    }
}

/// **An identity was asserted** — a new entity is a distinct thing.
///
/// The fourth verb, added 2026-08-08. The blueprint's identity pipeline names
/// three steps and annotates two of them:
///
/// ```text
/// observations ──► candidate clustering ──► identity assertion ──► entity
///                        (pure)              (recorded Event)
/// ```
///
/// The *recorded event* it names is this one, and until now it did not exist:
/// the module had `Merge`, `Split` and `Forget` — three verbs that all **revise**
/// an identity — and nothing for the act of first deciding there is one.
/// `WM_SCOPE.md` §5 puts minting at Tier 2 on exactly that reading: *"minting an
/// id is deciding that something is a distinct thing, which is adjudication"*.
///
/// # This records the judgement; it does not generate the id
///
/// The `EntityId` arrives already minted. Generation lives in the store —
/// `WM_SCOPE.md` §4: *"the core owns the type and the layer with a clock mints
/// the value"* — because a monotonic, never-reused id needs a clock and durable
/// state, and this crate has no dependencies by ratification criterion.
///
/// The split is not merely mechanical. A minted id that no `AssertIdentity`
/// records is an id with no justification behind it, which is the failure the
/// other three verbs each carry a [`Justification`] to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertIdentity {
    entity: EntityId,
    justification: Justification,
    at: DomainInstant,
}

impl AssertIdentity {
    /// Record that an identity was asserted.
    ///
    /// # Why there is no refusal here, stated rather than left to be noticed
    ///
    /// The other three constructors refuse a self-redirect, a too-narrow split,
    /// duplicates. This one refuses nothing beyond what its own field types
    /// already do: [`Justification`] is non-empty and duplicate-free by
    /// construction, and [`EntityId`] is validated by
    /// [`crate::reference`].
    ///
    /// That is a real difference, not an oversight. Those refusals are all
    /// *relational* — they compare an entity against others named in the same
    /// event. An assertion names ONE entity and nothing to be inconsistent
    /// with. The one check worth wanting — that this id was never asserted
    /// before — is not answerable here: it is a question about every other
    /// event in the log, which is the store's to answer and is enforced there
    /// by the mint's never-reuse guarantee.
    #[must_use]
    pub fn new(entity: EntityId, justification: Justification, at: DomainInstant) -> Self {
        Self {
            entity,
            justification,
            at,
        }
    }

    /// The entity being asserted into existence.
    #[must_use]
    pub fn entity(&self) -> &EntityId {
        &self.entity
    }

    /// The observations that justify the judgement.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        &self.justification
    }

    /// When the assertion was made.
    #[must_use]
    pub fn at(&self) -> DomainInstant {
        self.at
    }
}

/// **`ForgetEntity(EntityId, Reason)`** — §14.1.
///
/// *"'forget this place' is an operator-facing lifecycle transition to `Retired`
/// plus suppression from default projections. It is **not** deletion."*
///
/// # There is no `Redact` here, and its absence is the point
///
/// §14.1 reserves genuine erasure for *"a distinct, audited `Redact` operation
/// with its own ADR — and it must leave a tombstone proving something was
/// redacted, or the chain breaks."* No such ADR exists, so this module cannot
/// express erasure at all. A caller reaching for deletion finds nothing to
/// reach for, which is a stronger guarantee than a comment saying not to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetEntity {
    entity: EntityId,
    reason: RetirementReason,
    justification: Justification,
    at: DomainInstant,
}

impl ForgetEntity {
    /// Retire an entity, suppressing it from default projections.
    ///
    /// A justification is required here for the same reason as the other two,
    /// and the uniformity is deliberate: an entity that vanished from every
    /// projection with nothing recorded behind it is the exact failure the
    /// operator hits at the worst moment. An operator's instruction is
    /// recordable as an `Operator`-class observation, so this costs a real path
    /// nothing.
    #[must_use]
    pub fn new(
        entity: EntityId,
        reason: RetirementReason,
        justification: Justification,
        at: DomainInstant,
    ) -> Self {
        Self {
            entity,
            reason,
            justification,
            at,
        }
    }

    /// The entity being retired.
    #[must_use]
    pub fn entity(&self) -> &EntityId {
        &self.entity
    }

    /// Why, in the words that were recorded.
    #[must_use]
    pub fn reason(&self) -> &RetirementReason {
        &self.reason
    }

    /// What this judgement rests on.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        &self.justification
    }

    /// When the adjudication was made, on a clock that names itself.
    #[must_use]
    pub fn at(&self) -> DomainInstant {
        self.at
    }
}

// ---------------------------------------------------------------------------
// The recorded event
// ---------------------------------------------------------------------------

/// One recorded identity adjudication.
///
/// Closed over the three verbs §14.1 defines, and closed deliberately: a fourth
/// way to change identity should be a blueprint change first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityAdjudication {
    /// A new entity was asserted to be a distinct thing.
    Assert(AssertIdentity),
    /// Several entities were one.
    Merge(MergeEntities),
    /// One entity was several.
    Split(SplitEntity),
    /// An entity was retired. Not deleted.
    Forget(ForgetEntity),
}

impl IdentityAdjudication {
    /// When this adjudication was made.
    #[must_use]
    pub fn at(&self) -> DomainInstant {
        match self {
            Self::Assert(a) => a.at(),
            Self::Merge(m) => m.at(),
            Self::Split(s) => s.at(),
            Self::Forget(f) => f.at(),
        }
    }

    /// What this adjudication rests on. Never empty, whichever verb it is.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        match self {
            Self::Assert(a) => a.justification(),
            Self::Merge(m) => m.justification(),
            Self::Split(s) => s.justification(),
            Self::Forget(f) => f.justification(),
        }
    }

    /// The lifecycle transitions this adjudication implies.
    ///
    /// Every transition named here is one [`Lifecycle::advance_to`] permits from
    /// a live state, which is what stops the event model and the state model
    /// drifting apart: a consequence this function invents that the lifecycle
    /// algebra rejects is a contradiction, and there is a test that walks every
    /// verb — and both split shapes — against `advance_to` to catch exactly
    /// that.
    ///
    /// **Exhaustive since `KIRRA-WM-SPLIT-SURVIVAL-001` (2026-08-08).** It was
    /// not: a split's source was missing because its fate was unruled, and
    /// `unresolved_consequence` named the gap. Every verb now states every
    /// consequence it has — which for two of them is *none*, and that is a
    /// statement rather than an omission:
    ///
    /// * [`AssertIdentity`] creates, and creation is not a transition.
    /// * A [`SplitShape::Subtraction`] source survives, so nothing moved.
    ///
    /// The two are different claims and the docs at each arm say which.
    #[must_use]
    pub fn resulting_lifecycles(&self) -> Vec<(EntityId, Lifecycle)> {
        match self {
            // **No lifecycle consequence, and that is not an omission.**
            //
            // Every entry this function returns is walked through
            // `Lifecycle::advance_to` from every live state by
            // `every_stated_consequence_is_a_transition_the_lifecycle_permits`.
            // An assertion CREATES an entity: there is no prior state, so there
            // is no transition. Its initial state comes from
            // `Entity::provisional`, which is a constructor rather than a move.
            //
            // Stating `Provisional` here would be a lie the seam test correctly
            // catches -- an established entity cannot go back to provisional,
            // and `an_assertion_states_no_transition_because_it_creates` proves
            // that refusal is what makes the empty vec necessary rather than
            // convenient.
            Self::Assert(_) => Vec::new(),
            Self::Merge(m) => m
                .sources()
                .iter()
                .map(|s| {
                    (
                        s.clone(),
                        Lifecycle::Merged {
                            into: m.into_entity().clone(),
                        },
                    )
                })
                .collect(),
            Self::Split(s) => {
                let mut out: Vec<(EntityId, Lifecycle)> = s
                    .destinations()
                    .iter()
                    .map(|d| {
                        (
                            d.clone(),
                            Lifecycle::Split {
                                from: s.source().clone(),
                            },
                        )
                    })
                    .collect();
                // The SOURCE's fate, which this function refused to state until
                // KIRRA-WM-SPLIT-SURVIVAL-001 supplied one.
                //
                // A partition supersedes it: it was those successors and stops
                // corresponding to anything, answering with a redirect to all
                // of them. A subtraction leaves it ALIVE and unchanged, so
                // there is no transition to state -- the same "no entry"
                // meaning as `Assert`, and for the same reason: nothing moved.
                if s.shape() == SplitShape::Partition {
                    out.push((
                        s.source().clone(),
                        Lifecycle::Superseded {
                            by: s.destinations().to_vec(),
                        },
                    ));
                }
                out
            }
            Self::Forget(f) => vec![(f.entity().clone(), Lifecycle::Retired)],
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ClockDomain, TimeError};

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).expect("test id")
    }

    fn oid(s: &str) -> ObservationId {
        ObservationId::new(s).expect("test id")
    }

    fn just() -> Justification {
        Justification::new([oid("obs-1")]).expect("one observation")
    }

    const T0_MS: u64 = 1_700_000_000_000;

    /// A system-clock instant. Adjudication is knowledge-layer bookkeeping, not
    /// safety timing, so `System` is the honest domain -- and the caller has to
    /// say so rather than inherit a default.
    fn at(ms: u64) -> DomainInstant {
        DomainInstant {
            ms,
            domain: ClockDomain::System,
        }
    }

    const T0: DomainInstant = DomainInstant {
        ms: T0_MS,
        domain: ClockDomain::System,
    };

    // -- Justification --------------------------------------------------

    #[test]
    fn an_adjudication_with_nothing_behind_it_is_refused() {
        assert_eq!(
            Justification::new([]).expect_err("refused"),
            AdjudicationError::NoJustification
        );
    }

    #[test]
    fn a_repeated_citation_is_refused_rather_than_deduplicated() {
        let err =
            Justification::new([oid("obs-1"), oid("obs-2"), oid("obs-1")]).expect_err("refused");
        assert_eq!(
            err,
            AdjudicationError::DuplicateJustification {
                observation: oid("obs-1")
            },
            "and it names WHICH observation, since the caller may have mistyped \
             the second one rather than meant the first twice"
        );
    }

    #[test]
    fn citation_order_is_preserved_because_it_is_part_of_the_record() {
        let j = Justification::new([oid("obs-9"), oid("obs-1"), oid("obs-5")]).expect("admitted");
        let seen: Vec<&str> = j.observations().iter().map(ObservationId::as_str).collect();
        assert_eq!(
            seen,
            ["obs-9", "obs-1", "obs-5"],
            "not sorted -- the caller's ordering is what it recorded"
        );
        assert_eq!(j.len(), 3);
        assert_eq!(
            j.len(),
            j.observations().len(),
            "len() reports the real slice, not a stored count that could drift"
        );
    }

    // -- Merge ----------------------------------------------------------

    #[test]
    fn a_merge_folds_every_source_into_the_survivor() {
        let m = MergeEntities::new([eid("a"), eid("b")], eid("keep"), just(), T0).expect("valid");
        let adj = IdentityAdjudication::Merge(m);

        assert_eq!(
            adj.resulting_lifecycles(),
            vec![
                (eid("a"), Lifecycle::Merged { into: eid("keep") }),
                (eid("b"), Lifecycle::Merged { into: eid("keep") }),
            ]
        );
    }

    #[test]
    fn the_survivor_of_a_merge_gets_no_transition() {
        let m = MergeEntities::new([eid("a")], eid("keep"), just(), T0).expect("valid");
        let adj = IdentityAdjudication::Merge(m);
        assert!(
            !adj.resulting_lifecycles()
                .iter()
                .any(|(id, _)| id == &eid("keep")),
            "a merge says what the OTHERS were; it does not change the survivor"
        );
    }

    #[test]
    fn a_merge_into_one_of_its_own_sources_is_refused() {
        let err = MergeEntities::new([eid("a"), eid("keep")], eid("keep"), just(), T0)
            .expect_err("refused");
        assert_eq!(
            err,
            AdjudicationError::MergeIntoSelf {
                entity: eid("keep")
            }
        );
    }

    #[test]
    fn a_merge_with_no_sources_is_refused() {
        assert_eq!(
            MergeEntities::new([], eid("keep"), just(), T0).expect_err("refused"),
            AdjudicationError::EmptyMerge
        );
    }

    #[test]
    fn a_source_listed_twice_is_refused() {
        assert_eq!(
            MergeEntities::new([eid("a"), eid("a")], eid("keep"), just(), T0).expect_err("refused"),
            AdjudicationError::DuplicateSource { entity: eid("a") }
        );
    }

    // -- Split ----------------------------------------------------------

    #[test]
    fn a_split_marks_every_piece_with_its_origin() {
        let s = SplitEntity::partition(eid("pallet"), [eid("box-1"), eid("box-2")], just(), T0)
            .expect("valid");
        let adj = IdentityAdjudication::Split(s);

        assert_eq!(
            adj.resulting_lifecycles(),
            vec![
                (
                    eid("box-1"),
                    Lifecycle::Split {
                        from: eid("pallet")
                    }
                ),
                (
                    eid("box-2"),
                    Lifecycle::Split {
                        from: eid("pallet")
                    }
                ),
                // ...and the SOURCE, since KIRRA-WM-SPLIT-SURVIVAL-001. A
                // partition supersedes it; before the ruling this list stopped
                // at the pieces and the source's fate was unstated.
                (
                    eid("pallet"),
                    Lifecycle::Superseded {
                        by: vec![eid("box-1"), eid("box-2")]
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_split_into_one_is_refused_because_it_is_not_a_split() {
        assert_eq!(
            SplitEntity::partition(eid("pallet"), [eid("box-1")], just(), T0).expect_err("refused"),
            AdjudicationError::SplitTooNarrow { found: 1 }
        );
    }

    #[test]
    fn a_split_into_nothing_is_refused_because_that_would_be_destruction() {
        assert_eq!(
            SplitEntity::partition(eid("pallet"), [], just(), T0).expect_err("refused"),
            AdjudicationError::SplitTooNarrow { found: 0 },
            "erasure is a Redact with its own ADR, and this module cannot express it"
        );
    }

    #[test]
    fn a_split_naming_its_own_source_as_a_piece_is_refused() {
        let err = SplitEntity::partition(eid("pallet"), [eid("box-1"), eid("pallet")], just(), T0)
            .expect_err("refused");
        assert_eq!(
            err,
            AdjudicationError::SplitIntoSelf {
                entity: eid("pallet")
            }
        );
    }

    #[test]
    fn a_destination_listed_twice_is_refused() {
        assert_eq!(
            SplitEntity::partition(eid("pallet"), [eid("b"), eid("b")], just(), T0)
                .expect_err("refused"),
            AdjudicationError::DuplicateDestination { entity: eid("b") }
        );
    }

    /// **The scope this type actually has**, asserted rather than described.
    ///
    /// The module documents that it models *partition* and not *subtraction*.
    /// A documented scope is a remembered one; this makes it a checked one, and
    /// the check is what will FAIL when `KIRRA-WM-SPLIT-SURVIVAL-001` is ruled
    /// in favour of admitting subtraction — which is the intended outcome, not
    /// a regression.
    ///
    /// Both spellings of "the source survives as one of the pieces" are walked,
    /// because a reader who tries one and gives up would conclude the other
    /// works.
    #[test]
    fn a_split_where_the_source_survives_is_unrepresentable_today() {
        // You believed one pallet. There is a pallet with a box on it. The
        // pallet did not stop existing.
        let carve_off_a_piece =
            SplitEntity::partition(eid("pallet"), [eid("box")], just(), T0).expect_err("refused");
        assert_eq!(
            carve_off_a_piece,
            AdjudicationError::SplitTooNarrow { found: 1 },
            "naming only the new piece is refused for being too narrow"
        );

        let name_the_survivor =
            SplitEntity::partition(eid("pallet"), [eid("pallet"), eid("box")], just(), T0)
                .expect_err("refused");
        assert_eq!(
            name_the_survivor,
            AdjudicationError::SplitIntoSelf {
                entity: eid("pallet")
            },
            "and naming the survivor explicitly is refused too -- so there is \
             no third spelling that works"
        );
    }

    /// Non-vacuity for the test above: a **partition** of the same source is
    /// admitted.
    ///
    /// Without this, `a_split_where_the_source_survives_is_unrepresentable_today`
    /// would pass just as happily against a constructor that refused every
    /// split, which would make it evidence of nothing.
    #[test]
    fn the_partition_shape_of_the_same_split_is_admitted() {
        SplitEntity::partition(eid("pallet"), [eid("pallet-deck"), eid("box")], just(), T0)
            .expect("two successors, neither of them the source");
    }

    /// **The undecided consequence is reported, not omitted.**
    /// **A partition supersedes its source**, and says so.
    ///
    /// This test replaces `a_split_does_not_pretend_to_know_what_became_of_the
    /// _source`, which asserted the opposite: that no consequence was stated
    /// and `unresolved_consequence` named the gap. That was the honest answer
    /// while the question was open. `KIRRA-WM-SPLIT-SURVIVAL-001` closed it, so
    /// the placeholder became the hiding place §5 item 3 predicted and was
    /// deleted with it.
    ///
    /// Recorded rather than silently rewritten: a reader comparing against the
    /// proposal should find the old test's disappearance explained here.
    #[test]
    fn a_partition_supersedes_its_source() {
        let s = SplitEntity::partition(eid("pallet"), [eid("b1"), eid("b2")], just(), T0)
            .expect("valid");
        let adj = IdentityAdjudication::Split(s);

        let fate = adj
            .resulting_lifecycles()
            .into_iter()
            .find(|(id, _)| id == &eid("pallet"))
            .map(|(_, l)| l)
            .expect("the source's fate is stated now");
        assert_eq!(
            fate,
            Lifecycle::Superseded {
                by: vec![eid("b1"), eid("b2")]
            },
            "superseded BY its successors -- all of them, not one"
        );
    }

    /// **A subtraction leaves its source alive**, which is the shape that was
    /// unrepresentable before the ruling.
    ///
    /// Both spellings were refused: `into = [piece]` by `SplitTooNarrow` and
    /// `into = [source, piece]` by `SplitIntoSelf`. Now it has its own
    /// constructor, and the source takes NO transition — it did not move.
    #[test]
    fn a_subtraction_leaves_its_source_alive() {
        let s = SplitEntity::subtract(eid("pallet"), [eid("box-1")], just(), T0)
            .expect("one piece is a valid subtraction");
        assert_eq!(s.shape(), SplitShape::Subtraction);
        let adj = IdentityAdjudication::Split(s);

        assert!(
            !adj.resulting_lifecycles()
                .iter()
                .any(|(id, _)| id == &eid("pallet")),
            "a surviving source takes no transition, because nothing moved"
        );
        assert_eq!(
            adj.resulting_lifecycles().len(),
            1,
            "only the separated piece has a consequence"
        );
    }

    /// The source is still never one of its own destinations, in either shape.
    #[test]
    fn neither_shape_admits_the_source_among_its_destinations() {
        for r in [
            SplitEntity::partition(eid("p"), [eid("p"), eid("b")], just(), T0),
            SplitEntity::subtract(eid("p"), [eid("p")], just(), T0),
        ] {
            assert!(
                matches!(r, Err(AdjudicationError::SplitIntoSelf { .. })),
                "a source separated from itself is not a split"
            );
        }
        // ...and a partition still needs two, while a subtraction needs one.
        assert!(matches!(
            SplitEntity::partition(eid("p"), [eid("b")], just(), T0),
            Err(AdjudicationError::SplitTooNarrow { found: 1 })
        ));
        assert!(matches!(
            SplitEntity::subtract(eid("p"), [], just(), T0),
            Err(AdjudicationError::SplitTooNarrow { found: 0 })
        ));
    }

    #[test]
    fn a_merge_and_a_retirement_leave_nothing_undecided() {
        let m = IdentityAdjudication::Merge(
            MergeEntities::new([eid("a")], eid("keep"), just(), T0).expect("valid"),
        );
        let f = IdentityAdjudication::Forget(ForgetEntity::new(
            eid("gone"),
            RetirementReason::new("shelf removed").expect("reason"),
            just(),
            T0,
        ));
        // `unresolved_consequence` is gone -- KIRRA-WM-SPLIT-SURVIVAL-001 §5
        // item 3. What it asserted for these two is now structural: every verb
        // states every consequence it has, so there is no "undecided" accessor
        // left to return None from.
        assert!(!m.resulting_lifecycles().is_empty());
        assert!(!f.resulting_lifecycles().is_empty());
    }

    // -- Forget ---------------------------------------------------------

    #[test]
    fn forgetting_retires_and_does_not_delete() {
        let f = IdentityAdjudication::Forget(ForgetEntity::new(
            eid("old-dock"),
            RetirementReason::new("bay decommissioned").expect("reason"),
            just(),
            T0,
        ));
        assert_eq!(
            f.resulting_lifecycles(),
            vec![(eid("old-dock"), Lifecycle::Retired)],
            "Retired is a lifecycle state, and the entity remains in the model"
        );
    }

    #[test]
    fn a_retirement_without_a_reason_is_refused() {
        assert_eq!(
            RetirementReason::new("   ").expect_err("refused"),
            AdjudicationError::EmptyReason
        );
        assert_eq!(
            RetirementReason::new("").expect_err("refused"),
            AdjudicationError::EmptyReason
        );
    }

    #[test]
    fn a_reason_is_recorded_verbatim() {
        let r = RetirementReason::new("  bay 4 decommissioned  ").expect("reason");
        assert_eq!(
            r.as_str(),
            "  bay 4 decommissioned  ",
            "not trimmed -- the operator's words are the record"
        );
    }

    // -- The seam to the lifecycle algebra -------------------------------

    /// **Every consequence an adjudication states is a transition the lifecycle
    /// algebra permits.**
    ///
    /// This is the assertion that keeps the two halves of the identity model
    /// from drifting. Without it `resulting_lifecycles` could name a move
    /// `advance_to` rejects, and the contradiction would surface as a failed
    /// write in the store rather than as a broken invariant here.
    ///
    /// Walked from every live state, because an adjudication does not get to
    /// choose what state it finds an entity in.
    ///
    /// **Every verb is in `cases`, and both split shapes are** — including
    /// `Assert`, which states no consequence at all and so contributes nothing
    /// to the count. That is the point of listing it: if it ever grows one, it
    /// is checked here the moment it does rather than the day someone remembers
    /// to add it to this list.
    #[test]
    fn every_stated_consequence_is_a_transition_the_lifecycle_permits() {
        let cases = [
            IdentityAdjudication::Assert(AssertIdentity::new(eid("new"), just(), T0)),
            IdentityAdjudication::Merge(
                MergeEntities::new([eid("a"), eid("b")], eid("keep"), just(), T0).expect("valid"),
            ),
            IdentityAdjudication::Split(
                SplitEntity::partition(eid("p"), [eid("b1"), eid("b2")], just(), T0)
                    .expect("valid"),
            ),
            IdentityAdjudication::Split(
                SplitEntity::subtract(eid("s"), [eid("off")], just(), T0).expect("valid"),
            ),
            IdentityAdjudication::Forget(ForgetEntity::new(
                eid("gone"),
                RetirementReason::new("decommissioned").expect("reason"),
                just(),
                T0,
            )),
        ];

        let live = [
            Lifecycle::Provisional,
            Lifecycle::Established,
            Lifecycle::Dormant,
            Lifecycle::Split {
                from: eid("origin"),
            },
        ];

        let mut checked = 0usize;
        for adj in &cases {
            for (_, next) in adj.resulting_lifecycles() {
                for from in &live {
                    from.advance_to(next.clone()).unwrap_or_else(|e| {
                        panic!(
                            "adjudication states a consequence the lifecycle refuses: \
                             {from:?} -> {next:?} ({e:?})"
                        )
                    });
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked,
            7 * live.len(),
            "2 merge sources + 2 partition pieces + 1 SUPERSEDED SOURCE + 1 \
             subtracted piece + 1 retirement, each from every live state. \
             Assert contributes 0 -- it creates rather than transitions."
        );
    }

    /// An assertion states **no** transition, because it creates.
    ///
    /// The non-obvious half: this is not a gap left open like `SplitEntity`'s
    /// source. `Provisional` is where a new entity starts, and it is reached by
    /// `Entity::provisional` — a constructor — not by moving from somewhere.
    ///
    /// Proved rather than asserted: `Provisional` is *unreachable* as a
    /// transition target from every live state, so stating it as a consequence
    /// would make `every_stated_consequence_is_a_transition_the_lifecycle_permits`
    /// fail. The empty vec is the only honest answer, and the walk below is why.
    #[test]
    fn an_assertion_states_no_transition_because_it_creates() {
        let adj = IdentityAdjudication::Assert(AssertIdentity::new(eid("new-1"), just(), T0));
        assert!(
            adj.resulting_lifecycles().is_empty(),
            "creation is not a transition"
        );

        // ...and here is why it cannot state `Provisional`.
        for from in [
            Lifecycle::Provisional,
            Lifecycle::Established,
            Lifecycle::Dormant,
        ] {
            assert!(
                from.clone().advance_to(Lifecycle::Provisional).is_err(),
                "{from:?} -> Provisional must be refused, or the empty vec above \
                 would be a choice rather than a necessity"
            );
        }
    }

    /// The fourth verb carries a justification like the other three.
    ///
    /// Uniform on purpose: a minted id with nothing recorded behind it is an id
    /// whose existence no one can account for, which is the failure the whole
    /// module exists to prevent.
    #[test]
    fn an_assertion_rests_on_evidence_like_every_other_verb() {
        let adj = IdentityAdjudication::Assert(AssertIdentity::new(eid("new-1"), just(), T0));
        assert!(
            !adj.justification().observations().is_empty(),
            "never empty, whichever verb it is"
        );
        assert_eq!(adj.at(), T0);
    }

    /// The lifecycle algebra's terminal states are still terminal — the
    /// non-vacuity anchor for the test above.
    ///
    /// Without this, `every_stated_consequence_is_a_transition_the_lifecycle_permits`
    /// would pass just as happily against an `advance_to` that permitted
    /// everything, which would make it evidence of nothing.
    /// **`Superseded` is terminal**, and this is its non-vacuity anchor.
    ///
    /// `KIRRA-WM-SPLIT-SURVIVAL-001` §8 constraint 2: *"No new `Lifecycle`
    /// state without a negative control. The existing terminality tests pass
    /// against an `advance_to` that permits everything; any new terminal state
    /// needs the same anchor before it is claimed to be terminal."*
    ///
    /// Without this,
    /// `every_stated_consequence_is_a_transition_the_lifecycle_permits` would
    /// be satisfied by an `advance_to` that permitted every move, and the claim
    /// that a partitioned source is finished would rest on the doc comment.
    #[test]
    fn a_superseded_entity_cannot_be_adjudicated_again() {
        let superseded = Lifecycle::Superseded {
            by: vec![eid("b1"), eid("b2")],
        };
        for next in [
            Lifecycle::Established,
            Lifecycle::Dormant,
            Lifecycle::Retired,
            Lifecycle::Merged { into: eid("other") },
            Lifecycle::Split { from: eid("x") },
            Lifecycle::Superseded {
                by: vec![eid("c1"), eid("c2")],
            },
        ] {
            assert!(
                superseded.advance_to(next.clone()).is_err(),
                "a partitioned entity is finished; {next:?} would lose the redirect \
                 to its successors"
            );
        }
    }

    #[test]
    fn an_already_merged_entity_cannot_be_adjudicated_again() {
        let merged = Lifecycle::Merged { into: eid("keep") };
        let adj = IdentityAdjudication::Merge(
            MergeEntities::new([eid("a")], eid("other"), just(), T0).expect("valid"),
        );
        for (_, next) in adj.resulting_lifecycles() {
            assert!(
                merged.advance_to(next).is_err(),
                "a merged entity is terminal; re-merging it would lose the first redirect"
            );
        }
        assert!(
            Lifecycle::Retired.advance_to(Lifecycle::Retired).is_err(),
            "and a retired entity cannot be retired again"
        );
    }

    // -- Records, not commands ------------------------------------------

    /// Fields are private and there is no setter, so an adjudication cannot be
    /// edited after the fact.
    ///
    /// The point of this module is that identity changes are **events**. An
    /// event you can amend in place is an edit wearing an event's name, which is
    /// the failure §6.3 describes.
    #[test]
    fn an_adjudication_is_immutable_once_constructed() {
        let m = MergeEntities::new([eid("a")], eid("keep"), just(), T0).expect("valid");
        assert_eq!(m.sources(), &[eid("a")]);
        assert_eq!(m.into_entity(), &eid("keep"));
        assert_eq!(m.at(), T0);
        assert_eq!(m.justification().len(), 1);
        // Only accessors exist. Any mutation would need a field, and the
        // compiler is what enforces that rather than this comment.
    }

    #[test]
    fn every_verb_carries_a_justification_and_a_time() {
        let cases = [
            IdentityAdjudication::Merge(
                MergeEntities::new([eid("a")], eid("keep"), just(), T0).expect("valid"),
            ),
            IdentityAdjudication::Split(
                SplitEntity::partition(eid("p"), [eid("b1"), eid("b2")], just(), at(T0_MS + 1))
                    .expect("valid"),
            ),
            IdentityAdjudication::Forget(ForgetEntity::new(
                eid("gone"),
                RetirementReason::new("decommissioned").expect("reason"),
                just(),
                at(T0_MS + 2),
            )),
        ];
        for (n, adj) in cases.iter().enumerate() {
            assert!(
                !adj.justification().observations().is_empty(),
                "no verb is exempt from citing its evidence"
            );
            assert_eq!(adj.at(), at(T0_MS + n as u64));
        }
    }

    /// **The stamp names its own clock**, which is the whole reason this is a
    /// `DomainInstant` and not an integer.
    ///
    /// Two adjudications recorded against unsynchronized clocks must not order.
    /// With a bare `i64` they would compare fine and be confidently wrong; here
    /// the comparison is refused. `TimeError`'s own words: a cross-domain
    /// ordering *"is not merely imprecise — it is meaningless"*.
    #[test]
    fn an_adjudication_time_carries_the_clock_it_came_from() {
        let boundary = DomainInstant {
            ms: T0_MS,
            domain: ClockDomain::Boundary,
        };
        let stamped = IdentityAdjudication::Merge(
            MergeEntities::new([eid("a")], eid("keep"), just(), boundary).expect("valid"),
        );
        assert_eq!(stamped.at().domain, ClockDomain::Boundary, "verbatim");

        let on_system = IdentityAdjudication::Merge(
            MergeEntities::new([eid("b")], eid("keep"), just(), at(T0_MS)).expect("valid"),
        );
        assert_eq!(
            stamped.at().compare(&on_system.at()),
            Err(TimeError::DomainsDiffer {
                left: ClockDomain::Boundary,
                right: ClockDomain::System,
            }),
            "two clocks that were never synchronized do not order"
        );

        // Non-vacuity: within ONE domain the comparison does work, so the
        // refusal above is about the domains and not about comparison being
        // broken for every adjudication.
        assert_eq!(
            on_system.at().compare(&at(T0_MS + 1)),
            Ok(core::cmp::Ordering::Less)
        );
    }

    #[test]
    fn the_errors_name_the_offending_value() {
        let err = MergeEntities::new([eid("dup"), eid("dup")], eid("keep"), just(), T0)
            .expect_err("refused");
        let shown = err.to_string();
        assert!(
            shown.contains("dup"),
            "an identity event is reviewed long after it was refused: {shown}"
        );
    }
}
