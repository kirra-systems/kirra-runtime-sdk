//! The entity taxonomy's pure half — `KIRRA-WM-ARCH-001` §6, Tier 1.
//!
//! §6 has three parts. **Structure and kinds are here**; *identity adjudication*
//! (§6.3 — candidate clustering, merge and split events) is `WM_SCOPE.md`
//! **Tier 2**, and this module deliberately stops at the boundary.
//!
//! # Two rules made structural
//!
//! **1. An unknown kind degrades to `Unknown`; it never guesses a supertype.**
//! §6.2: *"A consumer that does not know a kind must degrade to `Unknown`, not
//! guess a supertype."* So [`EntityKind::group`] returns `Option<EntityGroup>`
//! and gives `None` for [`EntityKind::Unknown`] — there is no supertype to read,
//! rather than a plausible one to misread. [`EntityKind::from_token`] maps every
//! unrecognised token to `Unknown`, which is what makes new kinds *additive*
//! (§6.2) instead of breaking old consumers.
//!
//! **2. Resolution confidence cannot be confused with attribute confidence.**
//! §6.1 flags them as distinct — resolution confidence is *"how sure we are this
//! is **one** thing"*, not how sure we are of any of its attributes. They would
//! otherwise be the same [`Confidence`] type and silently interchangeable, so
//! [`ResolutionConfidence`] is a newtype and the compiler keeps them apart.
//!
//! # A tension inside §6, flagged rather than resolved
//!
//! §6.1's field table lists `kind` as a field every entity carries. §6.2 says
//! **the opposite**: *"Kind is a **classification observation**, not an intrinsic
//! property. A detector saying 'package' is evidence. The kind shown in a
//! projection is the adjudicated result."*
//!
//! Those cannot both be literally true of a stored field. This module follows
//! **§6.2**, for the same reason §6.1 already gives about attributes — *"they
//! are projections over the observations bound to it, so an attribute always
//! retains its own source, time, and confidence"* — and kind is exactly such an
//! attribute. [`Entity`] therefore has **no `kind` field**; kind is produced by
//! [`adjudicated_kind`] from classification evidence, so it cannot be set to
//! something the evidence does not support.
//!
//! Recorded as an open question for the World Model owner: **is §6.1's `kind`
//! row meant as a cached projection, or as an independently settable field?**
//! Under the first reading these agree and this module is right. Under the
//! second they conflict, and the conflict should be resolved in the blueprint
//! rather than here.
//!
//! # What is missing, and why
//!
//! `entity_id` generation (monotonic, never reused), `first_observed` /
//! `last_observed`, and `provenance_head` (a hash-chain head) all need the
//! store: ULID generation and hashing are dependencies, and `kirra-world` has
//! **zero by design** — the same argument that scoped [`crate::observation`].
//! [`EntityId`] here is an opaque validated wrapper, not a generator.

use crate::observation::{Confidence, SourceClass};
use crate::reference::EntityId;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Why an entity value could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityError {
    /// An alias name was empty or whitespace.
    EmptyAliasName,
    /// The lifecycle transition is not permitted — see [`Lifecycle::advance_to`].
    IllegalTransition {
        /// The state moved from.
        from: LifecycleState,
        /// The state moved to.
        to: LifecycleState,
    },
}

// ---------------------------------------------------------------------------
// Kinds — §6.2
// ---------------------------------------------------------------------------

/// The four kind groups. **Closed at the root** (§6.2).
///
/// Note there is no `Unknown` group: an unrecognised kind has no supertype, and
/// [`EntityKind::group`] returns `None` rather than inventing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityGroup {
    /// Things with agency.
    Agents,
    /// Things in the world with a physical extent.
    PhysicalObjects,
    /// Locations.
    Places,
    /// Things with no physical extent.
    Abstract,
}

/// A semantic entity type — §6.2's grouped, extensible, root-closed taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    // Agents
    /// A robot.
    Robot,
    /// A person.
    Person,
    /// An animal.
    Animal,
    // Physical objects
    /// A physical object with no more specific kind.
    Object,
    /// A tool.
    Tool,
    /// A package.
    Package,
    /// A vehicle.
    Vehicle,
    /// A door.
    Door,
    /// A charging dock.
    ChargingDock,
    // Places
    /// A room.
    Room,
    /// A zone.
    Zone,
    /// A waypoint.
    Waypoint,
    /// A landmark.
    Landmark,
    /// A surface.
    Surface,
    // Abstract
    /// A mission.
    Mission,
    /// A task.
    Task,
    /// A route.
    Route,
    /// A skill.
    Skill,
    /// A capability.
    Capability,
    /// **A kind this build does not recognise.**
    ///
    /// The degrade target §6.2 requires. Reached by [`EntityKind::from_token`]
    /// for any unrecognised token, which is what lets a newer producer add a
    /// kind without breaking an older consumer.
    Unknown,
}

impl EntityKind {
    /// The group this kind belongs to, or `None` if the kind is unrecognised.
    ///
    /// **`None` is the rule, not an omission.** §6.2 requires a consumer that
    /// does not know a kind to *"degrade to `Unknown`, not guess a supertype"*.
    /// Returning `Option` is how that becomes unavailable rather than merely
    /// discouraged — there is no group to read, so none can be guessed.
    #[must_use]
    pub fn group(self) -> Option<EntityGroup> {
        Some(match self {
            Self::Robot | Self::Person | Self::Animal => EntityGroup::Agents,
            Self::Object
            | Self::Tool
            | Self::Package
            | Self::Vehicle
            | Self::Door
            | Self::ChargingDock => EntityGroup::PhysicalObjects,
            Self::Room | Self::Zone | Self::Waypoint | Self::Landmark | Self::Surface => {
                EntityGroup::Places
            }
            Self::Mission | Self::Task | Self::Route | Self::Skill | Self::Capability => {
                EntityGroup::Abstract
            }
            Self::Unknown => return None,
        })
    }

    /// The stable wire token.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Robot => "robot",
            Self::Person => "person",
            Self::Animal => "animal",
            Self::Object => "object",
            Self::Tool => "tool",
            Self::Package => "package",
            Self::Vehicle => "vehicle",
            Self::Door => "door",
            Self::ChargingDock => "charging_dock",
            Self::Room => "room",
            Self::Zone => "zone",
            Self::Waypoint => "waypoint",
            Self::Landmark => "landmark",
            Self::Surface => "surface",
            Self::Mission => "mission",
            Self::Task => "task",
            Self::Route => "route",
            Self::Skill => "skill",
            Self::Capability => "capability",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a wire token. **Anything unrecognised becomes [`Self::Unknown`].**
    ///
    /// This is what makes §6.2's *"new kinds are additive and versioned, never
    /// repurposed"* survivable in a mixed-version fleet: an older consumer meets
    /// a newer kind and degrades, rather than failing or guessing.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "robot" => Self::Robot,
            "person" => Self::Person,
            "animal" => Self::Animal,
            "object" => Self::Object,
            "tool" => Self::Tool,
            "package" => Self::Package,
            "vehicle" => Self::Vehicle,
            "door" => Self::Door,
            "charging_dock" => Self::ChargingDock,
            "room" => Self::Room,
            "zone" => Self::Zone,
            "waypoint" => Self::Waypoint,
            "landmark" => Self::Landmark,
            "surface" => Self::Surface,
            "mission" => Self::Mission,
            "task" => Self::Task,
            "route" => Self::Route,
            "skill" => Self::Skill,
            "capability" => Self::Capability,
            _ => Self::Unknown,
        }
    }
}

/// One classification claim about an entity — §6.2's *"a detector saying
/// 'package' is evidence"*.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// What the classifier says this is.
    pub kind: EntityKind,
    /// How confident it is, and on what basis.
    pub confidence: Confidence,
    /// What kind of producer said so.
    pub source: SourceClass,
}

/// The outcome of adjudicating a kind — three distinct answers, kept distinct.
///
/// Deliberately **not** a bare [`EntityKind`]. ADR-0040's reasoning about
/// `ResolutionOutcome` applies exactly: collapsing "no evidence", "settled" and
/// "evidence I cannot rank" into one value loses a distinction the caller must
/// act on differently, and the loss is unrecoverable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindAdjudication {
    /// No classification evidence at all.
    NoEvidence,
    /// The evidence settles on one kind.
    Settled(EntityKind),
    /// **Evidence disagrees and cannot be ranked.**
    ///
    /// Either the contenders' confidences rest on different bases — which §7.3
    /// forbids comparing, since *"a detector's 0.9 and a geometric residual's
    /// 0.9 are not comparable"* — or a score is absent, and an absent score is
    /// not a low one.
    ///
    /// **This is not a kind, and the caller must not pick one from it.** It is
    /// the honest report that the system holds classification evidence it has no
    /// grounds to rank.
    Unrankable {
        /// The distinct kinds in contention, in first-seen order.
        contenders: Vec<EntityKind>,
    },
}

/// Adjudicate a kind from classification evidence — §6.2.
///
/// Kind is *"the adjudicated result"* of classification observations, never an
/// intrinsic property, so this is the only way to obtain one.
///
/// # The order of resolution
///
/// 1. **No evidence** → [`KindAdjudication::NoEvidence`]. Never a guess.
/// 2. **An operator claim settles it** — transition rule 4's adjudication half.
///    An operator may settle *what a thing is*, which is classification, not
///    geometry. Two operators who disagree are themselves unrankable.
/// 3. **Unanimous evidence settles it**, whatever the bases. If every claim says
///    `Package`, no comparison is needed and none is made — so differing bases
///    among agreeing sources are harmless, which is the common case.
/// 4. **Otherwise rank by confidence** — through [`Confidence::compare`], which
///    enforces §7.3's cross-basis guard. Any comparison it refuses makes the
///    whole adjudication [`KindAdjudication::Unrankable`].
///
/// # Ties keep the earlier claim
///
/// Only a strictly-greater confidence displaces, so an exact tie on the same
/// basis resolves by input order. That residual order-dependence is real and
/// pinned by a test; ranking equal-confidence disagreement properly needs the
/// corroboration counting only Tier 2 entity resolution can supply.
#[must_use]
pub fn adjudicated_kind(classifications: &[Classification]) -> KindAdjudication {
    if classifications.is_empty() {
        return KindAdjudication::NoEvidence;
    }

    /// Distinct kinds, in first-seen order.
    fn contenders(cs: &[Classification]) -> Vec<EntityKind> {
        let mut out: Vec<EntityKind> = Vec::new();
        for c in cs {
            if !out.contains(&c.kind) {
                out.push(c.kind);
            }
        }
        out
    }

    // (2) Rule 4: an operator settles classification.
    let operator: Vec<&Classification> = classifications
        .iter()
        .filter(|c| matches!(c.source, SourceClass::Operator))
        .collect();
    if !operator.is_empty() {
        let first = operator[0].kind;
        return if operator.iter().all(|c| c.kind == first) {
            KindAdjudication::Settled(first)
        } else {
            // Operators outrank sensors, but not each other.
            KindAdjudication::Unrankable {
                contenders: contenders(&operator.iter().map(|c| (*c).clone()).collect::<Vec<_>>()),
            }
        };
    }

    // (3) Unanimous evidence needs no comparison — so mismatched bases among
    //     agreeing sources never block the answer.
    let all = contenders(classifications);
    if all.len() == 1 {
        return KindAdjudication::Settled(all[0]);
    }

    // (4) Disagreement: rank THROUGH the §7.3 guard, never around it.
    let mut best = &classifications[0];
    for c in &classifications[1..] {
        match c.confidence.compare(&best.confidence) {
            Ok(core::cmp::Ordering::Greater) => best = c,
            // Less or Equal: the incumbent holds, so ties keep the earlier claim.
            Ok(_) => {}
            // Bases differ, or a score is absent. Either way there is no ground
            // to rank these, and inventing one is the silent over-trust §7.3
            // exists to prevent.
            Err(_) => return KindAdjudication::Unrankable { contenders: all },
        }
    }
    KindAdjudication::Settled(best.kind)
}

// ---------------------------------------------------------------------------
// Lifecycle — §6.1
// ---------------------------------------------------------------------------

/// Which lifecycle state an entity is in, without the merge/split payload.
///
/// Exists so [`EntityError::IllegalTransition`] can name states without
/// carrying ids into an error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// Newly created, not yet corroborated.
    Provisional,
    /// A thing we believe in.
    Established,
    /// Not observed lately, not retired.
    Dormant,
    /// Gone. Terminal.
    Retired,
    /// Merged into another entity. Terminal, but still resolvable.
    Merged,
    /// Produced by splitting another entity. A live state.
    Split,
}

/// An entity's lifecycle — §6.1.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    /// Newly created, not yet corroborated.
    Provisional,
    /// A thing we believe in.
    Established,
    /// Not observed lately. Reversible — see [`Lifecycle::advance_to`].
    Dormant,
    /// Gone. **Terminal.**
    Retired,
    /// **Merged into another entity — terminal, and still resolvable.**
    ///
    /// §6.3: *"Both original IDs remain resolvable forever and answer with a
    /// redirect."* The same shape as [`crate::trust::Adjudication::Rejected`]:
    /// terminal for this record, not for the subject. Merging never deletes.
    Merged {
        /// The entity this one is now an alias for.
        into: EntityId,
    },
    /// **Produced by splitting another entity.** A *live* state carrying its
    /// origin, not a terminal one.
    Split {
        /// The entity this one was split out of.
        from: EntityId,
    },
}

impl Lifecycle {
    /// The payload-free state.
    #[must_use]
    pub fn state(&self) -> LifecycleState {
        match self {
            Self::Provisional => LifecycleState::Provisional,
            Self::Established => LifecycleState::Established,
            Self::Dormant => LifecycleState::Dormant,
            Self::Retired => LifecycleState::Retired,
            Self::Merged { .. } => LifecycleState::Merged,
            Self::Split { .. } => LifecycleState::Split,
        }
    }

    /// Whether this state admits any further transition.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Retired | Self::Merged { .. })
    }

    /// Move to a new lifecycle state.
    ///
    /// # The permitted moves, and which of them the blueprint actually states
    ///
    /// §6.1 gives `Provisional → Established → Dormant → Retired`, *"plus
    /// `Merged(into)` / `Split(from)`"*, and says no more. Three readings had to
    /// be supplied, and each is called out here rather than buried:
    ///
    /// * **`Dormant → Established` is permitted.** The arrow chain is written
    ///   one-directionally, but an entity observed again after a quiet period is
    ///   the ordinary case, and forbidding it would force a new identity for a
    ///   thing that never stopped existing — precisely the identity-loss failure
    ///   §6.3 exists to prevent.
    /// * **`Retired` and `Merged` are terminal.** Neither admits any move.
    /// * **`Split` is live, not terminal**, on the reading that `Split(from)`
    ///   marks an entity *produced by* a split. It therefore behaves like
    ///   `Established`.
    ///
    /// Recorded as an open question: **is `Split(from)` a live origin marker or
    /// a terminal marker on the entity that was split?** The two readings differ
    /// in whether the *original* survives a split.
    ///
    /// # Errors
    ///
    /// [`EntityError::IllegalTransition`], naming both states.
    pub fn advance_to(&self, next: Lifecycle) -> Result<Lifecycle, EntityError> {
        let from = self.state();
        let to = next.state();

        let permitted = match from {
            // Terminal states admit nothing.
            LifecycleState::Retired | LifecycleState::Merged => false,
            LifecycleState::Provisional => matches!(
                to,
                LifecycleState::Established
                    | LifecycleState::Dormant
                    | LifecycleState::Retired
                    | LifecycleState::Merged
                    | LifecycleState::Split
            ),
            LifecycleState::Established | LifecycleState::Split => matches!(
                to,
                LifecycleState::Dormant
                    | LifecycleState::Retired
                    | LifecycleState::Merged
                    | LifecycleState::Split
            ),
            LifecycleState::Dormant => matches!(
                to,
                LifecycleState::Established
                    | LifecycleState::Retired
                    | LifecycleState::Merged
                    | LifecycleState::Split
            ),
        };

        if permitted {
            Ok(next)
        } else {
            Err(EntityError::IllegalTransition { from, to })
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution confidence — §6.1
// ---------------------------------------------------------------------------

/// How sure we are that an entity is **one thing**.
///
/// §6.1 marks this *"distinct from attribute confidence"*, and the newtype is
/// what makes the distinction hold: without it both would be [`Confidence`] and
/// a caller could pass "I am 0.9 sure this box is red" where "I am 0.9 sure
/// these two sightings are the same box" was wanted. Those are different claims
/// and conflating them misstates what the system knows.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionConfidence(Confidence);

impl ResolutionConfidence {
    /// Mark a confidence as being about identity.
    #[must_use]
    pub fn new(confidence: Confidence) -> Self {
        Self(confidence)
    }

    /// The underlying confidence. Explicit, so unwrapping is visible at the
    /// call site rather than implicit through a `Deref`.
    #[must_use]
    pub fn inner(&self) -> &Confidence {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Aliases and the entity spine — §6.1
// ---------------------------------------------------------------------------

/// A name something is known by, **with its own provenance** (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alias {
    name: String,
    source: SourceClass,
}

impl Alias {
    /// Build an alias.
    ///
    /// # Errors
    ///
    /// [`EntityError::EmptyAliasName`] if the name is empty or whitespace.
    pub fn new(name: impl Into<String>, source: SourceClass) -> Result<Self, EntityError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(EntityError::EmptyAliasName);
        }
        Ok(Self { name, source })
    }

    /// The name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the name came from. §6.1 requires each alias to carry this — an
    /// operator's name for a thing and a detector's label are not the same kind
    /// of claim.
    #[must_use]
    pub fn source(&self) -> SourceClass {
        self.source
    }
}

/// The entity **spine** — §6.1.
///
/// > *"Attributes are **not** stored on the entity. They are projections over
/// > the observations bound to it, so an attribute always retains its own
/// > source, time, and confidence. An entity is a spine; observations hang from
/// > it."*
///
/// **There is no `kind` field**, deliberately — see the module docs. Kind comes
/// from [`adjudicated_kind`] over classification evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    id: EntityId,
    lifecycle: Lifecycle,
    aliases: Vec<Alias>,
    resolution_confidence: ResolutionConfidence,
}

impl Entity {
    /// Create a provisional entity — the state everything starts in.
    #[must_use]
    pub fn provisional(id: EntityId, resolution_confidence: ResolutionConfidence) -> Self {
        Self {
            id,
            lifecycle: Lifecycle::Provisional,
            aliases: Vec::new(),
            resolution_confidence,
        }
    }

    /// The identifier.
    #[must_use]
    pub fn id(&self) -> &EntityId {
        &self.id
    }

    /// The lifecycle state.
    #[must_use]
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// Every name this is known by.
    #[must_use]
    pub fn aliases(&self) -> &[Alias] {
        &self.aliases
    }

    /// How sure we are this is one thing.
    #[must_use]
    pub fn resolution_confidence(&self) -> &ResolutionConfidence {
        &self.resolution_confidence
    }

    /// Add a name.
    #[must_use]
    pub fn with_alias(mut self, alias: Alias) -> Self {
        self.aliases.push(alias);
        self
    }

    /// Move the lifecycle on.
    ///
    /// # Errors
    ///
    /// [`EntityError::IllegalTransition`] — see [`Lifecycle::advance_to`].
    pub fn advance_to(mut self, next: Lifecycle) -> Result<Self, EntityError> {
        self.lifecycle = self.lifecycle.advance_to(next)?;
        Ok(self)
    }

    /// Where a merged entity redirects to — §6.3's *"answer with a redirect"*.
    ///
    /// `None` unless this entity was merged.
    #[must_use]
    pub fn redirects_to(&self) -> Option<&EntityId> {
        match &self.lifecycle {
            Lifecycle::Merged { into } => Some(into),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::ConfidenceBasis;

    fn conf(score: f32) -> Confidence {
        Confidence::new(Some(score), ConfidenceBasis::ModelScore, None).unwrap()
    }

    fn res_conf() -> ResolutionConfidence {
        ResolutionConfidence::new(Confidence::unspecified())
    }

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).unwrap()
    }

    // -- §6.2: degrade, never guess a supertype ---------------------------

    #[test]
    fn an_unknown_kind_has_no_supertype_to_guess() {
        // THE §6.2 rule. There is no group to read, so none can be invented.
        assert_eq!(EntityKind::Unknown.group(), None);
    }

    #[test]
    fn an_unrecognised_token_degrades_rather_than_failing() {
        // A newer producer's kind meeting an older consumer.
        assert_eq!(
            EntityKind::from_token("quadruped_drone"),
            EntityKind::Unknown
        );
        assert_eq!(EntityKind::from_token(""), EntityKind::Unknown);
        assert_eq!(EntityKind::from_token("ROBOT"), EntityKind::Unknown);
    }

    #[test]
    fn every_known_kind_round_trips_and_has_a_group() {
        let all = [
            EntityKind::Robot,
            EntityKind::Person,
            EntityKind::Animal,
            EntityKind::Object,
            EntityKind::Tool,
            EntityKind::Package,
            EntityKind::Vehicle,
            EntityKind::Door,
            EntityKind::ChargingDock,
            EntityKind::Room,
            EntityKind::Zone,
            EntityKind::Waypoint,
            EntityKind::Landmark,
            EntityKind::Surface,
            EntityKind::Mission,
            EntityKind::Task,
            EntityKind::Route,
            EntityKind::Skill,
            EntityKind::Capability,
        ];
        assert_eq!(all.len(), 19, "the blueprint's §6.2 table has 19 kinds");
        for k in all {
            assert_eq!(EntityKind::from_token(k.as_token()), k, "{k:?}");
            assert!(k.group().is_some(), "{k:?} must have a group");
        }
    }

    #[test]
    fn unknown_round_trips_but_stays_groupless() {
        assert_eq!(
            EntityKind::from_token(EntityKind::Unknown.as_token()),
            EntityKind::Unknown
        );
        assert_eq!(EntityKind::Unknown.group(), None);
    }

    // -- §6.2: kind is evidence, not an intrinsic property -----------------

    #[test]
    fn no_evidence_yields_unknown_rather_than_a_guess() {
        assert_eq!(adjudicated_kind(&[]), KindAdjudication::NoEvidence);
    }

    #[test]
    fn an_operator_settles_the_classification() {
        // Rule 4's adjudication half: an operator may settle WHAT a thing is.
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: conf(0.99),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.10),
                source: SourceClass::Operator,
            },
        ];
        // The operator wins despite far lower confidence.
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Tool)
        );
    }

    #[test]
    fn without_an_operator_the_strongest_sensor_claim_wins() {
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: conf(0.3),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.8),
                source: SourceClass::Sensor,
            },
        ];
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Tool)
        );
    }

    #[test]
    fn a_tie_keeps_the_earlier_claim() {
        // Documented limitation: the result depends on input order. Asserted so
        // the behaviour is pinned rather than accidental.
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: conf(0.5),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.5),
                source: SourceClass::Sensor,
            },
        ];
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Package)
        );
    }

    #[test]
    fn disagreement_across_bases_is_unrankable_not_silently_ranked() {
        // REGRESSION. The earlier version pulled raw .score() out and compared
        // floats, reaching straight PAST the §7.3 cross-basis guard built in
        // slice 2 — a detector's 0.9 and a geometric residual's 0.9 are not the
        // same quantity, and ranking them is the silent over-trust §7.3 names.
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: Confidence::new(Some(0.9), ConfidenceBasis::ModelScore, None).unwrap(),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: Confidence::new(Some(0.95), ConfidenceBasis::GeometricResidual, None)
                    .unwrap(),
                source: SourceClass::Sensor,
            },
        ];
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Unrankable {
                contenders: vec![EntityKind::Package, EntityKind::Tool],
            }
        );
    }

    #[test]
    fn agreeing_sources_settle_even_with_mismatched_bases() {
        // The common case, and why the basis guard does not block it: if every
        // claim says the same thing, no comparison is needed and none is made.
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: Confidence::new(Some(0.4), ConfidenceBasis::ModelScore, None).unwrap(),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Package,
                confidence: Confidence::new(Some(0.9), ConfidenceBasis::GeometricResidual, None)
                    .unwrap(),
                source: SourceClass::Import,
            },
        ];
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Package)
        );
    }

    #[test]
    fn an_unscored_first_claim_no_longer_freezes_the_result() {
        // REGRESSION for the second half of the finding. The old loop gated
        // displacement on `if let (Some, Some)`, so an unscored claim at index 0
        // meant NOTHING could ever displace it — a later 0.99 claim could not
        // win, contradicting "highest confidence wins".
        //
        // The honest answer is not "the scored one wins" either: an absent score
        // is not a low score (asserted in the observation module). So this is
        // Unrankable — which is at least no longer silently order-frozen.
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: Confidence::unspecified(),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: Confidence::new(Some(0.99), ConfidenceBasis::Unspecified, None)
                    .unwrap(),
                source: SourceClass::Sensor,
            },
        ];
        assert_ne!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Package),
            "the unscored first claim must not win by freezing the loop"
        );
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Unrankable {
                contenders: vec![EntityKind::Package, EntityKind::Tool],
            }
        );
    }

    #[test]
    fn operators_outrank_sensors_but_not_each_other() {
        let claims = [
            Classification {
                kind: EntityKind::Package,
                confidence: conf(0.99),
                source: SourceClass::Sensor,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.5),
                source: SourceClass::Operator,
            },
            Classification {
                kind: EntityKind::Vehicle,
                confidence: conf(0.5),
                source: SourceClass::Operator,
            },
        ];
        // The sensor claim is out of contention entirely; the two operators are
        // not rankable against each other.
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Unrankable {
                contenders: vec![EntityKind::Tool, EntityKind::Vehicle],
            }
        );
    }

    #[test]
    fn agreeing_operators_settle() {
        let claims = [
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.1),
                source: SourceClass::Operator,
            },
            Classification {
                kind: EntityKind::Tool,
                confidence: conf(0.2),
                source: SourceClass::Operator,
            },
        ];
        assert_eq!(
            adjudicated_kind(&claims),
            KindAdjudication::Settled(EntityKind::Tool)
        );
    }

    #[test]
    fn reclassification_does_not_need_a_new_entity() {
        // §6.2: "the box you thought was a package turning out to be a tool is a
        // reclassification, not a new object." The entity has no kind field, so
        // this is true by construction — there is nothing on it to contradict.
        let e = Entity::provisional(eid("e-1"), res_conf());
        let before = [Classification {
            kind: EntityKind::Package,
            confidence: conf(0.9),
            source: SourceClass::Sensor,
        }];
        let after = [Classification {
            kind: EntityKind::Tool,
            confidence: conf(0.9),
            source: SourceClass::Operator,
        }];
        assert_eq!(
            adjudicated_kind(&before),
            KindAdjudication::Settled(EntityKind::Package)
        );
        assert_eq!(
            adjudicated_kind(&after),
            KindAdjudication::Settled(EntityKind::Tool)
        );
        assert_eq!(
            e.id(),
            &eid("e-1"),
            "identity unchanged by reclassification"
        );
    }

    // -- Lifecycle ---------------------------------------------------------

    #[test]
    fn the_ordinary_progression_is_permitted() {
        let l = Lifecycle::Provisional
            .advance_to(Lifecycle::Established)
            .unwrap()
            .advance_to(Lifecycle::Dormant)
            .unwrap()
            .advance_to(Lifecycle::Retired)
            .unwrap();
        assert_eq!(l.state(), LifecycleState::Retired);
    }

    #[test]
    fn a_dormant_entity_can_be_observed_again() {
        // The supplied reading: forbidding this would force a new identity for a
        // thing that never stopped existing.
        assert!(Lifecycle::Dormant
            .advance_to(Lifecycle::Established)
            .is_ok());
    }

    #[test]
    fn retired_and_merged_are_terminal() {
        assert!(Lifecycle::Retired.is_terminal());
        assert!(Lifecycle::Merged { into: eid("e-2") }.is_terminal());

        assert_eq!(
            Lifecycle::Retired.advance_to(Lifecycle::Established),
            Err(EntityError::IllegalTransition {
                from: LifecycleState::Retired,
                to: LifecycleState::Established,
            })
        );
        assert!(Lifecycle::Merged { into: eid("e-2") }
            .advance_to(Lifecycle::Established)
            .is_err());
    }

    #[test]
    fn a_split_entity_is_live_not_terminal() {
        let l = Lifecycle::Split { from: eid("e-0") };
        assert!(!l.is_terminal());
        assert!(l.advance_to(Lifecycle::Dormant).is_ok());
    }

    #[test]
    fn a_merged_entity_still_resolves_by_redirect() {
        // §6.3: "Both original IDs remain resolvable forever and answer with a
        // redirect." Merging never deletes.
        let e = Entity::provisional(eid("e-1"), res_conf())
            .advance_to(Lifecycle::Merged { into: eid("e-2") })
            .unwrap();
        assert_eq!(e.redirects_to(), Some(&eid("e-2")));
        assert_eq!(e.id(), &eid("e-1"), "the merged-away id is still its own");
    }

    #[test]
    fn an_unmerged_entity_redirects_nowhere() {
        let e = Entity::provisional(eid("e-1"), res_conf());
        assert_eq!(e.redirects_to(), None);
    }

    // -- Validation --------------------------------------------------------

    #[test]
    fn empty_identifiers_are_refused() {
        // `EntityId` moved to `crate::reference` on 2026-08-08 and now reports
        // `ReferenceError`; its own emptiness/length/control-character cases are
        // covered there. What this test still owns is the *alias* half.
        assert_eq!(
            Alias::new("", SourceClass::Operator),
            Err(EntityError::EmptyAliasName)
        );
    }

    #[test]
    fn an_alias_carries_where_the_name_came_from() {
        // §6.1: an operator's name and a detector's label are not the same claim.
        let e = Entity::provisional(eid("e-1"), res_conf())
            .with_alias(Alias::new("the loading bay", SourceClass::Operator).unwrap())
            .with_alias(Alias::new("zone_04", SourceClass::Import).unwrap());
        assert_eq!(e.aliases().len(), 2);
        assert_eq!(e.aliases()[0].source(), SourceClass::Operator);
        assert_eq!(e.aliases()[1].source(), SourceClass::Import);
    }

    #[test]
    fn resolution_confidence_is_not_interchangeable_with_attribute_confidence() {
        // The newtype is the whole point; this test documents the intent, since
        // the real enforcement is that the wrong type does not compile.
        let rc = ResolutionConfidence::new(conf(0.9));
        assert_eq!(rc.inner().score(), Some(0.9));
    }
}
