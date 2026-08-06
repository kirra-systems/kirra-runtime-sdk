//! The relationship model's pure half — `KIRRA-WM-ARCH-001` §8, Tier 1.
//!
//! Relationships are *"first-class, directed, typed, and time-bounded."*
//!
//! # Three of §8's four design notes are made structural
//!
//! **1. Inferred relationships must cite their derivation.** §8: *"`inside`
//! derived from a pose and a room polygon carries the polygon version and the
//! pose observation. When the map changes, dependent inferences are invalidated
//! by provenance rather than left silently wrong."*
//!
//! [`Origination`] is an enum, not a `source` field beside an
//! `Option<derivation>`. An inferred relationship **carries its
//! [`DerivationRef`] in the variant**, so "inferred, derivation missing" is
//! unrepresentable rather than merely discouraged. The obvious hole — routing
//! an inference through `Direct(SourceClass::Derivation)` — is refused at the
//! constructor.
//!
//! **2. `caused_by` is deliberately weak.** §8 admits it *"for
//! operator-asserted causality and for system events… **not** for inferred
//! physical causation."* [`Predicate::CausedBy`] combined with
//! [`Origination::Inferred`] is therefore **refused**. That is what
//! "deliberately weak" means when written as a type rather than a warning.
//!
//! **3. Supersession, not update.** §8: *"Moving a toolbox closes the old
//! `last_seen_at` interval and opens a new relationship. The history is the
//! point."* There is no setter for `valid_time` and no `update`. The only way
//! forward is [`Relationship::supersede`], which **returns both** the closed
//! predecessor and the new relationship — so the history cannot be dropped by
//! forgetting to keep it.
//!
//! # The fourth note is only half-doable here
//!
//! *"Symmetry and inverses are derived, not stored twice. `contains` implies
//! `inside`; storing both invites divergence."*
//!
//! A pure module cannot stop a store writing both rows. What it can do is
//! define **one canonical direction**, so the two spellings normalize to the
//! same value and a store's dedupe has something to compare — see
//! [`Relationship::canonical`].
//!
//! **The relation algebra is deliberately sparse.** §8 states exactly one
//! implication (`contains` → `inside`) and says nothing about the other
//! thirteen predicates. [`Predicate::symmetry`] therefore returns
//! [`Symmetry::Unspecified`] for everything else rather than guessing — see its
//! docs for the specific candidates, recorded as an open question.

use crate::entity::EntityId;
use crate::observation::{Confidence, DomainInstant, SourceClass, ValidInterval};

// ---------------------------------------------------------------------------
// Predicates — §8's four groups
// ---------------------------------------------------------------------------

/// Which family a predicate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredicateGroup {
    /// Containment and connection.
    Topological,
    /// Physical arrangement.
    Spatial,
    /// Ownership and duty.
    Assignment,
    /// Ordering and causality.
    Temporal,
}

/// A typed, directed relation — §8's predicate table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Predicate {
    /// Subject is inside object.
    Inside,
    /// Subject contains object. Inverse of [`Self::Inside`].
    Contains,
    /// Subject connects to object.
    ConnectedTo,
    /// Subject is adjacent to object.
    AdjacentTo,
    /// Subject is a part of object.
    PartOf,
    /// Subject is near object.
    Near,
    /// Subject supports object.
    Supports,
    /// Subject rests on object.
    OnTopOf,
    /// Subject was last seen at object.
    LastSeenAt,
    /// Subject belongs to object.
    BelongsTo,
    /// Subject is assigned to object.
    AssignedTo,
    /// Subject charges object.
    ChargingFor,
    /// Subject is operated by object.
    OperatedBy,
    /// Subject preceded object.
    Preceded,
    /// Subject was caused by object.
    ///
    /// **Deliberately weak** (§8). Admitted for operator-asserted causality and
    /// system events; **never** for inferred physical causation — which
    /// [`Relationship::new`] enforces.
    CausedBy,
}

/// What a predicate implies about its reverse direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symmetry {
    /// `p(a,b)` implies `p(b,a)`.
    Symmetric,
    /// `p(a,b)` implies the named predicate holding `(b,a)`.
    Inverse(Predicate),
    /// **The blueprint does not say.** Treated as implying nothing — no
    /// reverse-direction fact is derived, which is the fail-closed reading.
    Unspecified,
}

impl Predicate {
    /// The group this predicate belongs to.
    #[must_use]
    pub fn group(self) -> PredicateGroup {
        match self {
            Self::Inside | Self::Contains | Self::ConnectedTo | Self::AdjacentTo | Self::PartOf => {
                PredicateGroup::Topological
            }
            Self::Near | Self::Supports | Self::OnTopOf | Self::LastSeenAt => {
                PredicateGroup::Spatial
            }
            Self::BelongsTo | Self::AssignedTo | Self::ChargingFor | Self::OperatedBy => {
                PredicateGroup::Assignment
            }
            Self::Preceded | Self::CausedBy => PredicateGroup::Temporal,
        }
    }

    /// The stable wire token.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Inside => "inside",
            Self::Contains => "contains",
            Self::ConnectedTo => "connected_to",
            Self::AdjacentTo => "adjacent_to",
            Self::PartOf => "part_of",
            Self::Near => "near",
            Self::Supports => "supports",
            Self::OnTopOf => "on_top_of",
            Self::LastSeenAt => "last_seen_at",
            Self::BelongsTo => "belongs_to",
            Self::AssignedTo => "assigned_to",
            Self::ChargingFor => "charging_for",
            Self::OperatedBy => "operated_by",
            Self::Preceded => "preceded",
            Self::CausedBy => "caused_by",
        }
    }

    /// What holds in the reverse direction, if anything.
    ///
    /// # Only one implication is stated, so only one is encoded
    ///
    /// §8 gives exactly one: *"`contains` implies `inside`"*. Everything else
    /// returns [`Symmetry::Unspecified`], because asserting a relation algebra
    /// the blueprint did not state would put inferred facts into an evidence
    /// store on this module's authority.
    ///
    /// Recorded as an open question for the World Model owner, with the
    /// candidates named so the ruling is cheap:
    ///
    /// * **Likely symmetric:** `near`, `adjacent_to`. Physical proximity reads
    ///   both ways.
    /// * **Likely symmetric, but not certainly:** `connected_to` — a one-way
    ///   corridor or a directed conveyor would break it, and a robot map can
    ///   contain both.
    /// * **Likely an inverse pair:** `supports` / `on_top_of`.
    /// * **Possibly missing an inverse:** `part_of` has no `has_part` in the
    ///   table at all.
    ///
    /// Until ruled, each is `Unspecified` — which derives nothing, rather than
    /// deriving something plausible and wrong.
    #[must_use]
    pub fn symmetry(self) -> Symmetry {
        match self {
            Self::Contains => Symmetry::Inverse(Self::Inside),
            Self::Inside => Symmetry::Inverse(Self::Contains),
            _ => Symmetry::Unspecified,
        }
    }
}

// ---------------------------------------------------------------------------
// Endpoints and origination
// ---------------------------------------------------------------------------

/// What a relationship points at — §8's `EntityRef | LiteralRef`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelObject {
    /// Another entity.
    Entity(EntityId),
    /// A value rather than a thing. A literal can never be a *subject*, which
    /// is why [`Relationship::canonical`] leaves literal-object relationships
    /// alone — there is nothing to flip them into.
    Literal(String),
}

/// A reference to what an inference was derived from.
///
/// §8 requires the *"polygon version and the pose observation"* — this crate
/// stores the reference opaquely; resolving it is the store's job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DerivationRef(String);

impl DerivationRef {
    /// Wrap a derivation reference.
    ///
    /// # Errors
    ///
    /// [`RelationshipError::EmptyDerivationRef`] if empty or whitespace — an
    /// empty reference claims a derivation exists while naming nothing, which
    /// defeats the invalidate-by-provenance rule it is there to serve.
    pub fn new(r: impl Into<String>) -> Result<Self, RelationshipError> {
        let r = r.into();
        if r.trim().is_empty() {
            return Err(RelationshipError::EmptyDerivationRef);
        }
        Ok(Self(r))
    }

    /// The reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a relationship came from — **the derivation is in the variant**.
///
/// This shape is the point. With `source` and `Option<derivation>` as separate
/// fields, "inferred but citing nothing" is representable and has to be caught
/// by a check somebody remembers to write. Here it does not typecheck.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origination {
    /// Stated by a producer directly — sensed, asserted, imported.
    Direct(SourceClass),
    /// **Inferred**, and carrying what it was inferred from.
    Inferred(DerivationRef),
}

// ---------------------------------------------------------------------------
// Relationship
// ---------------------------------------------------------------------------

/// Why a relationship could not be built or moved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelationshipError {
    /// A derivation reference was empty.
    EmptyDerivationRef,
    /// A relationship id was empty.
    EmptyRelationshipId,
    /// **§8: `caused_by` is admitted for operator-asserted causality and system
    /// events, never for inferred physical causation.**
    InferredCausalityRefused,
    /// [`SourceClass::Derivation`] was passed to [`Origination::Direct`], which
    /// would route an inference around the derivation requirement. Use
    /// [`Origination::Inferred`] and cite what it came from.
    DerivationMustCiteItsSource,
    /// The relationship has already been superseded. Supersession is terminal
    /// for the record — §8's *"the history is the point"*.
    AlreadySuperseded,
}

/// An opaque relationship identifier. Generation lives in the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationshipId(String);

impl RelationshipId {
    /// Wrap an identifier.
    ///
    /// # Errors
    ///
    /// [`RelationshipError::EmptyRelationshipId`] if empty or whitespace.
    pub fn new(id: impl Into<String>) -> Result<Self, RelationshipError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(RelationshipError::EmptyRelationshipId);
        }
        Ok(Self(id))
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A directed, typed, time-bounded relation — §8.
#[derive(Debug, Clone, PartialEq)]
pub struct Relationship {
    id: RelationshipId,
    subject: EntityId,
    predicate: Predicate,
    object: RelObject,
    valid_time: ValidInterval,
    transaction_time: DomainInstant,
    origination: Origination,
    confidence: Confidence,
    superseded_by: Option<RelationshipId>,
}

impl Relationship {
    /// Build a relationship.
    ///
    /// # Errors
    ///
    /// * [`RelationshipError::InferredCausalityRefused`] — `caused_by` may not
    ///   be inferred (§8's "deliberately weak").
    /// * [`RelationshipError::DerivationMustCiteItsSource`] — a
    ///   `Direct(SourceClass::Derivation)` would let an inference skip its
    ///   derivation reference.
    // Eight arguments, one over clippy's default. Allowed deliberately: these
    // are §8's own record fields, each a distinct type, and bundling them into
    // invented sub-structs would break the 1:1 reading against the blueprint
    // for no safety gain — the type distinctions already prevent transposition.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: RelationshipId,
        subject: EntityId,
        predicate: Predicate,
        object: RelObject,
        valid_time: ValidInterval,
        transaction_time: DomainInstant,
        origination: Origination,
        confidence: Confidence,
    ) -> Result<Self, RelationshipError> {
        if matches!(origination, Origination::Direct(SourceClass::Derivation)) {
            return Err(RelationshipError::DerivationMustCiteItsSource);
        }
        if predicate == Predicate::CausedBy && matches!(origination, Origination::Inferred(_)) {
            return Err(RelationshipError::InferredCausalityRefused);
        }
        Ok(Self {
            id,
            subject,
            predicate,
            object,
            valid_time,
            transaction_time,
            origination,
            confidence,
            superseded_by: None,
        })
    }

    /// The identifier.
    #[must_use]
    pub fn id(&self) -> &RelationshipId {
        &self.id
    }
    /// The subject.
    #[must_use]
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }
    /// The predicate.
    #[must_use]
    pub fn predicate(&self) -> Predicate {
        self.predicate
    }
    /// The object.
    #[must_use]
    pub fn object(&self) -> &RelObject {
        &self.object
    }
    /// When it held. **No setter** — see [`Self::supersede`].
    #[must_use]
    pub fn valid_time(&self) -> &ValidInterval {
        &self.valid_time
    }
    /// **When we learned it** — §8's `transaction_time`, the transaction half of
    /// the bitemporal pair.
    ///
    /// Deliberately **not** required to share a clock domain with
    /// [`Self::valid_time`]: transaction time is a fact about the recorder,
    /// valid time is a fact about the world, and they can legitimately be read
    /// from different clocks. Nothing is lost by allowing it —
    /// [`DomainInstant::compare`] already refuses to compare across domains, so
    /// a caller cannot accidentally order one against the other.
    #[must_use]
    pub fn transaction_time(&self) -> DomainInstant {
        self.transaction_time
    }

    /// Where it came from.
    #[must_use]
    pub fn origination(&self) -> &Origination {
        &self.origination
    }
    /// How confident, and on what basis.
    #[must_use]
    pub fn confidence(&self) -> &Confidence {
        &self.confidence
    }
    /// What replaced it, if anything.
    #[must_use]
    pub fn superseded_by(&self) -> Option<&RelationshipId> {
        self.superseded_by.as_ref()
    }

    /// Whether this relationship was derived rather than stated.
    #[must_use]
    pub fn is_inferred(&self) -> bool {
        matches!(self.origination, Origination::Inferred(_))
    }

    /// What this was derived from, if it was derived.
    ///
    /// The handle §8's invalidate-by-provenance rule needs: *"when the map
    /// changes, dependent inferences are invalidated by provenance rather than
    /// left silently wrong."* An inferred relationship **always** has one.
    #[must_use]
    pub fn derivation(&self) -> Option<&DerivationRef> {
        match &self.origination {
            Origination::Inferred(d) => Some(d),
            Origination::Direct(_) => None,
        }
    }

    /// **Supersession, not update** — §8.
    ///
    /// Returns **both** halves: this relationship with its interval closed and
    /// its `superseded_by` set, and the replacement. There is no `update`, and
    /// no setter for `valid_time`, so the only way to change what is believed is
    /// to produce a record of what was believed before.
    ///
    /// Returning a pair rather than mutating is deliberate: *"the history is the
    /// point"*, and a caller that keeps only the second half has to discard the
    /// first explicitly rather than by omission.
    ///
    /// # Errors
    ///
    /// [`RelationshipError::AlreadySuperseded`] — terminal, like
    /// [`crate::trust::Adjudication::Rejected`] and
    /// [`crate::entity::Lifecycle::Merged`]. A record that has already been
    /// closed is not closed again.
    pub fn supersede(
        self,
        closed_at: ValidInterval,
        replacement: Relationship,
    ) -> Result<(Self, Self), RelationshipError> {
        if self.superseded_by.is_some() {
            return Err(RelationshipError::AlreadySuperseded);
        }
        let closed = Self {
            valid_time: closed_at,
            superseded_by: Some(replacement.id.clone()),
            ..self
        };
        Ok((closed, replacement))
    }

    /// Normalize to the canonical direction, so the two spellings of one fact
    /// compare equal — §8's *"symmetry and inverses are derived, not stored
    /// twice."*
    ///
    /// A pure module cannot stop a store writing both rows; it can make them
    /// **the same value** so a dedupe has something to compare.
    ///
    /// # Which direction is canonical, and why it is arbitrary on purpose
    ///
    /// The predicate whose token sorts first. `contains` < `inside`, so
    /// `contains` is canonical. That rule is **mechanical rather than
    /// semantic** — deliberately, because choosing on meaning would be a domain
    /// judgement, and any stable choice serves the dedupe equally well.
    ///
    /// Returns `self` unchanged when the predicate's symmetry is
    /// [`Symmetry::Unspecified`], or when the object is a
    /// [`RelObject::Literal`] — a literal cannot become a subject, so there is
    /// no other direction to point in.
    #[must_use]
    pub fn canonical(self) -> Self {
        let Symmetry::Inverse(inverse) = self.predicate.symmetry() else {
            return self;
        };
        let RelObject::Entity(obj) = self.object.clone() else {
            return self;
        };
        if self.predicate.as_token() <= inverse.as_token() {
            return self;
        }
        Self {
            subject: obj,
            predicate: inverse,
            object: RelObject::Entity(self.subject.clone()),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ClockDomain, ConfidenceBasis, DomainInstant};

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).unwrap()
    }
    fn rid(s: &str) -> RelationshipId {
        RelationshipId::new(s).unwrap()
    }
    fn interval(from: u64) -> ValidInterval {
        ValidInterval::open_from(DomainInstant {
            ms: from,
            domain: ClockDomain::System,
        })
    }
    fn txn() -> DomainInstant {
        DomainInstant {
            ms: 1_000,
            domain: ClockDomain::System,
        }
    }
    fn conf() -> Confidence {
        Confidence::new(Some(0.8), ConfidenceBasis::ModelScore, None).unwrap()
    }
    fn rel(
        id: &str,
        subject: &str,
        p: Predicate,
        object: &str,
        o: Origination,
    ) -> Result<Relationship, RelationshipError> {
        Relationship::new(
            rid(id),
            eid(subject),
            p,
            RelObject::Entity(eid(object)),
            interval(0),
            txn(),
            o,
            conf(),
        )
    }

    // -- §8: inferred relationships must cite their derivation -------------

    #[test]
    fn an_inference_always_has_a_derivation() {
        let r = rel(
            "r-1",
            "e-1",
            Predicate::Inside,
            "e-2",
            Origination::Inferred(DerivationRef::new("poly-v3+obs-77").unwrap()),
        )
        .unwrap();
        assert!(r.is_inferred());
        assert_eq!(r.derivation().unwrap().as_str(), "poly-v3+obs-77");
    }

    #[test]
    fn a_derivation_cannot_skip_its_reference_via_source_class() {
        // The hole the enum would otherwise leave: Direct(Derivation) is an
        // inference with no citation, which defeats invalidate-by-provenance.
        assert_eq!(
            rel(
                "r-1",
                "e-1",
                Predicate::Inside,
                "e-2",
                Origination::Direct(SourceClass::Derivation)
            ),
            Err(RelationshipError::DerivationMustCiteItsSource)
        );
    }

    #[test]
    fn an_empty_derivation_reference_is_refused() {
        assert_eq!(
            DerivationRef::new("   "),
            Err(RelationshipError::EmptyDerivationRef)
        );
    }

    #[test]
    fn a_direct_relationship_has_no_derivation() {
        let r = rel(
            "r-1",
            "e-1",
            Predicate::Near,
            "e-2",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        assert!(!r.is_inferred());
        assert_eq!(r.derivation(), None);
    }

    // -- §8: caused_by is deliberately weak --------------------------------

    #[test]
    fn inferred_physical_causation_is_refused() {
        // THE §8 rule: caused_by is for operator-asserted causality and system
        // events, never for a robot's inference.
        assert_eq!(
            rel(
                "r-1",
                "e-1",
                Predicate::CausedBy,
                "e-2",
                Origination::Inferred(DerivationRef::new("d-1").unwrap())
            ),
            Err(RelationshipError::InferredCausalityRefused)
        );
    }

    #[test]
    fn operator_asserted_causality_is_admitted() {
        assert!(rel(
            "r-1",
            "e-1",
            Predicate::CausedBy,
            "e-2",
            Origination::Direct(SourceClass::Operator)
        )
        .is_ok());
        // ...and a system event.
        assert!(rel(
            "r-2",
            "e-1",
            Predicate::CausedBy,
            "e-2",
            Origination::Direct(SourceClass::Configuration)
        )
        .is_ok());
    }

    #[test]
    fn only_caused_by_is_restricted_this_way() {
        // Every other predicate may be inferred.
        for p in [Predicate::Inside, Predicate::Near, Predicate::Preceded] {
            assert!(
                rel(
                    "r-1",
                    "e-1",
                    p,
                    "e-2",
                    Origination::Inferred(DerivationRef::new("d-1").unwrap())
                )
                .is_ok(),
                "{p:?} should be inferable"
            );
        }
    }

    // -- §8: supersession, not update --------------------------------------

    #[test]
    fn supersession_returns_both_halves() {
        // "The history is the point." A caller that wants only the new value
        // has to discard the old one explicitly.
        let old = rel(
            "r-1",
            "toolbox",
            Predicate::LastSeenAt,
            "bench",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        let new = rel(
            "r-2",
            "toolbox",
            Predicate::LastSeenAt,
            "shelf",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();

        let closed_window = ValidInterval::new(
            DomainInstant {
                ms: 0,
                domain: ClockDomain::System,
            },
            Some(DomainInstant {
                ms: 500,
                domain: ClockDomain::System,
            }),
        )
        .unwrap();

        let (closed, replacement) = old.supersede(closed_window, new).unwrap();

        assert_eq!(closed.superseded_by(), Some(&rid("r-2")));
        assert_eq!(closed.valid_time().end().unwrap().ms, 500);
        assert_eq!(replacement.id(), &rid("r-2"));
        assert_eq!(replacement.superseded_by(), None);
        // The old fact is still fully readable — "where was it yesterday?"
        assert_eq!(closed.object(), &RelObject::Entity(eid("bench")));
    }

    #[test]
    fn superseding_twice_is_refused() {
        let a = rel(
            "r-1",
            "e-1",
            Predicate::Near,
            "e-2",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        let b = rel(
            "r-2",
            "e-1",
            Predicate::Near,
            "e-3",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        let c = rel(
            "r-3",
            "e-1",
            Predicate::Near,
            "e-4",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();

        let (closed, _) = a.supersede(interval(0), b).unwrap();
        assert_eq!(
            closed.supersede(interval(0), c),
            Err(RelationshipError::AlreadySuperseded)
        );
    }

    // -- §8: inverses are derived, not stored twice ------------------------

    #[test]
    fn the_one_stated_implication_is_encoded() {
        assert_eq!(
            Predicate::Contains.symmetry(),
            Symmetry::Inverse(Predicate::Inside)
        );
        assert_eq!(
            Predicate::Inside.symmetry(),
            Symmetry::Inverse(Predicate::Contains)
        );
    }

    #[test]
    fn unstated_relation_algebra_derives_nothing() {
        // near/adjacent_to/connected_to LOOK symmetric and supports/on_top_of
        // LOOK like an inverse pair — but §8 does not say so, and asserting it
        // would put inferred facts in an evidence store on this module's say-so.
        for p in [
            Predicate::Near,
            Predicate::AdjacentTo,
            Predicate::ConnectedTo,
            Predicate::Supports,
            Predicate::OnTopOf,
            Predicate::PartOf,
        ] {
            assert_eq!(p.symmetry(), Symmetry::Unspecified, "{p:?}");
        }
    }

    #[test]
    fn both_spellings_of_one_fact_normalize_to_the_same_value() {
        // "storing both invites divergence" — canonical() is what lets a store
        // notice they are the same fact.
        let a = rel(
            "r-1",
            "room",
            Predicate::Contains,
            "toolbox",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap()
        .canonical();
        let b = rel(
            "r-1",
            "toolbox",
            Predicate::Inside,
            "room",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap()
        .canonical();

        assert_eq!(a, b);
        assert_eq!(a.predicate(), Predicate::Contains, "contains < inside");
        assert_eq!(a.subject(), &eid("room"));
    }

    #[test]
    fn canonicalisation_is_idempotent() {
        let r = rel(
            "r-1",
            "toolbox",
            Predicate::Inside,
            "room",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        let once = r.canonical();
        let twice = once.clone().canonical();
        assert_eq!(once, twice);
    }

    #[test]
    fn a_literal_object_is_never_flipped() {
        // A literal cannot become a subject, so there is no other direction.
        let r = Relationship::new(
            rid("r-1"),
            eid("toolbox"),
            Predicate::Inside,
            RelObject::Literal("bay-4".into()),
            interval(0),
            txn(),
            Origination::Direct(SourceClass::Import),
            conf(),
        )
        .unwrap();
        let c = r.clone().canonical();
        assert_eq!(c, r);
        assert_eq!(c.predicate(), Predicate::Inside);
    }

    #[test]
    fn unspecified_symmetry_leaves_the_relationship_alone() {
        let r = rel(
            "r-1",
            "e-2",
            Predicate::Near,
            "e-1",
            Origination::Direct(SourceClass::Sensor),
        )
        .unwrap();
        assert_eq!(r.clone().canonical(), r);
    }

    // -- Predicate table ---------------------------------------------------

    #[test]
    fn the_predicate_table_matches_the_blueprint() {
        let all = [
            (Predicate::Inside, PredicateGroup::Topological),
            (Predicate::Contains, PredicateGroup::Topological),
            (Predicate::ConnectedTo, PredicateGroup::Topological),
            (Predicate::AdjacentTo, PredicateGroup::Topological),
            (Predicate::PartOf, PredicateGroup::Topological),
            (Predicate::Near, PredicateGroup::Spatial),
            (Predicate::Supports, PredicateGroup::Spatial),
            (Predicate::OnTopOf, PredicateGroup::Spatial),
            (Predicate::LastSeenAt, PredicateGroup::Spatial),
            (Predicate::BelongsTo, PredicateGroup::Assignment),
            (Predicate::AssignedTo, PredicateGroup::Assignment),
            (Predicate::ChargingFor, PredicateGroup::Assignment),
            (Predicate::OperatedBy, PredicateGroup::Assignment),
            (Predicate::Preceded, PredicateGroup::Temporal),
            (Predicate::CausedBy, PredicateGroup::Temporal),
        ];
        assert_eq!(all.len(), 15, "§8's table has 15 predicates");
        for (p, g) in all {
            assert_eq!(p.group(), g, "{p:?}");
            assert!(!p.as_token().is_empty());
        }
    }

    #[test]
    fn tokens_are_distinct() {
        let all = [
            Predicate::Inside,
            Predicate::Contains,
            Predicate::ConnectedTo,
            Predicate::AdjacentTo,
            Predicate::PartOf,
            Predicate::Near,
            Predicate::Supports,
            Predicate::OnTopOf,
            Predicate::LastSeenAt,
            Predicate::BelongsTo,
            Predicate::AssignedTo,
            Predicate::ChargingFor,
            Predicate::OperatedBy,
            Predicate::Preceded,
            Predicate::CausedBy,
        ];
        let mut toks: Vec<&str> = all.iter().map(|p| p.as_token()).collect();
        toks.sort_unstable();
        let before = toks.len();
        toks.dedup();
        assert_eq!(toks.len(), before, "predicate tokens must be distinct");
    }

    // -- Composition: clock domains still cannot mix -----------------------

    #[test]
    fn a_relationship_interval_still_cannot_span_two_clocks() {
        // Inherited from ValidInterval — the guard is not re-implemented here,
        // and this test pins that the composition actually carries it.
        let bad = ValidInterval::new(
            DomainInstant {
                ms: 0,
                domain: ClockDomain::Boundary,
            },
            Some(DomainInstant {
                ms: 10,
                domain: ClockDomain::System,
            }),
        );
        assert!(bad.is_err());
    }

    #[test]
    fn valid_and_transaction_time_may_use_different_clocks() {
        // The bitemporal pair: valid time is about the world, transaction time
        // is about the recorder. Forcing one domain would be over-constraining,
        // and DomainInstant::compare already refuses to order across them.
        let r = Relationship::new(
            rid("r-1"),
            eid("e-1"),
            Predicate::Near,
            RelObject::Entity(eid("e-2")),
            ValidInterval::open_from(DomainInstant {
                ms: 5,
                domain: ClockDomain::Boundary,
            }),
            DomainInstant {
                ms: 9,
                domain: ClockDomain::System,
            },
            Origination::Direct(SourceClass::Sensor),
            conf(),
        )
        .unwrap();
        assert_eq!(r.valid_time().domain(), ClockDomain::Boundary);
        assert_eq!(r.transaction_time().domain, ClockDomain::System);
        // ...and they still cannot be ordered against each other.
        assert!(r
            .transaction_time()
            .compare(&r.valid_time().start())
            .is_err());
    }

    #[test]
    fn empty_identifiers_are_refused() {
        assert_eq!(
            RelationshipId::new(" "),
            Err(RelationshipError::EmptyRelationshipId)
        );
    }
}
