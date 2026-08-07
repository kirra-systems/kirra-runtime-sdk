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
//! * **A split into fewer than two** is not a split. Into one it is a no-op that
//!   nonetheless records an identity change; into zero it is destruction, which
//!   is what [`ForgetEntity`] is for and what §14.1 reserves for a `Redact`
//!   operation with *"its own ADR"*.
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
//! **A split's consequence for the entity that was split is not stated, because
//! it is not decided.** `entity.rs` records the open question in as many words:
//! *"is `Split(from)` a live origin marker or a terminal marker on the entity
//! that was split? The two readings differ in whether the original survives a
//! split."* Rather than pick one silently, the undecided entity is reported by
//! [`IdentityAdjudication::unresolved_consequence`], so a caller that ignores it
//! is making a choice it can be held to instead of reading a list that quietly
//! omitted an entity.
//!
//! # ADR-0042 condition (1)
//!
//! Nothing here may become a required safety input: no `CorridorSource`, no
//! actuator path, no release token. Pure data and pure functions, zero
//! dependencies, same as the rest of this crate.

use crate::entity::{EntityId, Lifecycle};
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
            Self::SplitTooNarrow { found } => write!(
                f,
                "a split needs at least two destinations, got {found}"
            ),
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

    /// Always `false`. The constructor refuses the empty case.
    ///
    /// Present because [`Self::len`] without it is a lint, and worth a warning:
    /// **it is a constant, so `!j.is_empty()` is a tautology.** A test that
    /// means "this really cites something" must read
    /// [`Self::observations`], not this — clippy will happily suggest
    /// rewriting `len() >= 1` into a check that can never fail.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
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
    at_ms: i64,
}

impl MergeEntities {
    /// Adjudicate that `sources` and `into` are one entity.
    ///
    /// `at_ms` is supplied rather than read from a clock: this crate has no
    /// dependencies and no clock, and the layer that appends the event is the
    /// one that knows what time it recorded it. Same core/adapter split as
    /// identity minting.
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
        at_ms: i64,
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
            at_ms,
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

    /// When the adjudication was made.
    #[must_use]
    pub fn at_ms(&self) -> i64 {
        self.at_ms
    }
}

/// **`SplitEntity(from, into[], Evidence)`** — §14.1.
///
/// One entity is judged to have been several things. Each destination becomes
/// [`Lifecycle::Split`] carrying its origin.
///
/// **The source's own fate is deliberately not stated here** — see
/// [`IdentityAdjudication::unresolved_consequence`] and the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitEntity {
    source: EntityId,
    into: Vec<EntityId>,
    justification: Justification,
    at_ms: i64,
}

impl SplitEntity {
    /// Adjudicate that `source` was really the entities in `into`.
    ///
    /// # Errors
    ///
    /// [`AdjudicationError::SplitTooNarrow`] with fewer than two destinations;
    /// [`AdjudicationError::DuplicateDestination`] naming a repeated
    /// destination; [`AdjudicationError::SplitIntoSelf`] if the source is also a
    /// destination.
    pub fn new(
        source: EntityId,
        into: impl IntoIterator<Item = EntityId>,
        justification: Justification,
        at_ms: i64,
    ) -> Result<Self, AdjudicationError> {
        let into: Vec<EntityId> = into.into_iter().collect();
        if into.len() < 2 {
            return Err(AdjudicationError::SplitTooNarrow { found: into.len() });
        }
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
            into,
            justification,
            at_ms,
        })
    }

    /// The entity being split.
    #[must_use]
    pub fn source(&self) -> &EntityId {
        &self.source
    }

    /// The entities it turned out to be. At least two.
    #[must_use]
    pub fn destinations(&self) -> &[EntityId] {
        &self.into
    }

    /// What this judgement rests on.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        &self.justification
    }

    /// When the adjudication was made.
    #[must_use]
    pub fn at_ms(&self) -> i64 {
        self.at_ms
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
    at_ms: i64,
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
        at_ms: i64,
    ) -> Self {
        Self {
            entity,
            reason,
            justification,
            at_ms,
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

    /// When the adjudication was made.
    #[must_use]
    pub fn at_ms(&self) -> i64 {
        self.at_ms
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
    pub fn at_ms(&self) -> i64 {
        match self {
            Self::Merge(m) => m.at_ms(),
            Self::Split(s) => s.at_ms(),
            Self::Forget(f) => f.at_ms(),
        }
    }

    /// What this adjudication rests on. Never empty, whichever verb it is.
    #[must_use]
    pub fn justification(&self) -> &Justification {
        match self {
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
    /// algebra rejects is a contradiction, and there is a test that walks all
    /// three verbs against `advance_to` to catch exactly that.
    ///
    /// **Not exhaustive by design.** A split's source is missing, because its
    /// fate is unruled — [`Self::unresolved_consequence`] names it.
    #[must_use]
    pub fn resulting_lifecycles(&self) -> Vec<(EntityId, Lifecycle)> {
        match self {
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
            Self::Split(s) => s
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
                .collect(),
            Self::Forget(f) => vec![(f.entity().clone(), Lifecycle::Retired)],
        }
    }

    /// The entity whose lifecycle consequence this adjudication **cannot**
    /// state, if there is one.
    ///
    /// Only a split has one: the entity that was split. `entity.rs` records the
    /// question — *"is `Split(from)` a live origin marker or a terminal marker
    /// on the entity that was split?"* — and the two readings disagree about
    /// whether the original survives, which is not a detail. Picking one here
    /// would bury a ruling inside a helper function.
    ///
    /// Returning it rather than omitting it means a caller that does not handle
    /// the source is visibly declining to, instead of consuming a list that
    /// silently dropped an entity.
    #[must_use]
    pub fn unresolved_consequence(&self) -> Option<&EntityId> {
        match self {
            Self::Merge(_) | Self::Forget(_) => None,
            Self::Split(s) => Some(s.source()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).expect("test id")
    }

    fn oid(s: &str) -> ObservationId {
        ObservationId::new(s).expect("test id")
    }

    fn just() -> Justification {
        Justification::new([oid("obs-1")]).expect("one observation")
    }

    const T0: i64 = 1_700_000_000_000;

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
        let err = Justification::new([oid("obs-1"), oid("obs-2"), oid("obs-1")])
            .expect_err("refused");
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
        assert!(!j.is_empty());
    }

    // -- Merge ----------------------------------------------------------

    #[test]
    fn a_merge_folds_every_source_into_the_survivor() {
        let m = MergeEntities::new([eid("a"), eid("b")], eid("keep"), just(), T0).expect("valid");
        let adj = IdentityAdjudication::Merge(m);

        assert_eq!(
            adj.resulting_lifecycles(),
            vec![
                (
                    eid("a"),
                    Lifecycle::Merged {
                        into: eid("keep")
                    }
                ),
                (
                    eid("b"),
                    Lifecycle::Merged {
                        into: eid("keep")
                    }
                ),
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
        assert_eq!(err, AdjudicationError::MergeIntoSelf { entity: eid("keep") });
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
            MergeEntities::new([eid("a"), eid("a")], eid("keep"), just(), T0)
                .expect_err("refused"),
            AdjudicationError::DuplicateSource { entity: eid("a") }
        );
    }

    // -- Split ----------------------------------------------------------

    #[test]
    fn a_split_marks_every_piece_with_its_origin() {
        let s = SplitEntity::new(eid("pallet"), [eid("box-1"), eid("box-2")], just(), T0)
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
            ]
        );
    }

    #[test]
    fn a_split_into_one_is_refused_because_it_is_not_a_split() {
        assert_eq!(
            SplitEntity::new(eid("pallet"), [eid("box-1")], just(), T0).expect_err("refused"),
            AdjudicationError::SplitTooNarrow { found: 1 }
        );
    }

    #[test]
    fn a_split_into_nothing_is_refused_because_that_would_be_destruction() {
        assert_eq!(
            SplitEntity::new(eid("pallet"), [], just(), T0).expect_err("refused"),
            AdjudicationError::SplitTooNarrow { found: 0 },
            "erasure is a Redact with its own ADR, and this module cannot express it"
        );
    }

    #[test]
    fn a_split_naming_its_own_source_as_a_piece_is_refused() {
        let err = SplitEntity::new(
            eid("pallet"),
            [eid("box-1"), eid("pallet")],
            just(),
            T0,
        )
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
            SplitEntity::new(eid("pallet"), [eid("b"), eid("b")], just(), T0)
                .expect_err("refused"),
            AdjudicationError::DuplicateDestination { entity: eid("b") }
        );
    }

    /// **The undecided consequence is reported, not omitted.**
    #[test]
    fn a_split_does_not_pretend_to_know_what_became_of_the_source() {
        let s = SplitEntity::new(eid("pallet"), [eid("b1"), eid("b2")], just(), T0)
            .expect("valid");
        let adj = IdentityAdjudication::Split(s);

        assert!(
            !adj.resulting_lifecycles()
                .iter()
                .any(|(id, _)| id == &eid("pallet")),
            "no fabricated transition for the entity that was split"
        );
        assert_eq!(
            adj.unresolved_consequence(),
            Some(&eid("pallet")),
            "and it is NAMED, so a caller ignoring it is choosing to"
        );
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
        assert_eq!(m.unresolved_consequence(), None);
        assert_eq!(f.unresolved_consequence(), None);
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
    #[test]
    fn every_stated_consequence_is_a_transition_the_lifecycle_permits() {
        let cases = [
            IdentityAdjudication::Merge(
                MergeEntities::new([eid("a"), eid("b")], eid("keep"), just(), T0).expect("valid"),
            ),
            IdentityAdjudication::Split(
                SplitEntity::new(eid("p"), [eid("b1"), eid("b2")], just(), T0).expect("valid"),
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
            Lifecycle::Split { from: eid("origin") },
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
            5 * live.len(),
            "2 merge sources + 2 split pieces + 1 retirement, each from every live state"
        );
    }

    /// The lifecycle algebra's terminal states are still terminal — the
    /// non-vacuity anchor for the test above.
    ///
    /// Without this, `every_stated_consequence_is_a_transition_the_lifecycle_permits`
    /// would pass just as happily against an `advance_to` that permitted
    /// everything, which would make it evidence of nothing.
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
            Lifecycle::Retired
                .advance_to(Lifecycle::Retired)
                .is_err(),
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
        assert_eq!(m.at_ms(), T0);
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
                SplitEntity::new(eid("p"), [eid("b1"), eid("b2")], just(), T0 + 1).expect("valid"),
            ),
            IdentityAdjudication::Forget(ForgetEntity::new(
                eid("gone"),
                RetirementReason::new("decommissioned").expect("reason"),
                just(),
                T0 + 2,
            )),
        ];
        for (n, adj) in cases.iter().enumerate() {
            assert!(
                !adj.justification().observations().is_empty(),
                "no verb is exempt from citing its evidence"
            );
            assert_eq!(adj.at_ms(), T0 + n as i64);
        }
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
