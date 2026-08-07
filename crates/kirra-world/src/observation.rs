//! The observation model's pure half — `KIRRA-WM-ARCH-001` §7, Tier 1.
//!
//! §7.1 specifies an eighteen-field immutable `Observation` record. **This
//! module implements the part of it that can be pure**, and deliberately stops
//! short of the rest — see *What is missing and why* below.
//!
//! # Three rules made structural rather than remembered
//!
//! The same move that gave [`crate::trust`] its shape: where the blueprint
//! states a rule in prose, prefer a type that cannot express the violation.
//!
//! **1. Cross-modal confidence comparison is refused by default.** §7.3:
//!
//! > *"A bare float is nearly useless across modalities: a detector's 0.9 and a
//! > geometric residual's 0.9 are not comparable, and treating them as such is
//! > how fusion systems silently over-trust. Carrying the basis makes cross-modal
//! > comparison an explicit decision rather than an accident."*
//!
//! [`Confidence::compare`] therefore **errors** when the two bases differ.
//! Comparing across bases requires calling [`Confidence::compare_across_bases`],
//! whose name is the explicit decision the blueprint asks for. The accident is
//! not discouraged; it is unavailable.
//!
//! **2. Clock domains do not mix.** §5.1 and the hypervisor contract channel's
//! two-clock-domain model both say it, and `AOU-TIMESYNC-001` makes conversion
//! the integrator's obligation *at the producing edge*. [`DomainInstant`]
//! carries its domain, and [`DomainInstant::compare`] refuses a cross-domain
//! comparison rather than returning a confidently wrong ordering.
//!
//! **3. Language never supplies geometry (P10).** §9.2's transition rule 4 has
//! two halves; the *adjudication* half is [`crate::trust::TrustAxes::operator_confirm`]
//! and the *geometry* half is here. An operator correcting a pose builds a
//! [`Payload::correction`] — an associated function that never receives the
//! payload it corrects, and whose [`PayloadSource::Correction`] variant has no
//! source-class field to inherit. So the corrected record is unreachable from
//! the operation, and the correction cannot present as sensed.
//!
//! Worth noting where the enforcement point landed: **rule 4's geometry half
//! turned out not to need geometry.** The rule asks that an operator's payload
//! be *"visibly distinct from a sensed one"* — a claim about provenance, which
//! a crate with no pose type can keep in full. The `Payload` body is a type
//! parameter precisely so that stays true.
//!
//! # What is missing, and why — the dependency argument
//!
//! §7.1's record also carries `observation_id: Ulid`, `evidence_digest: Hash`,
//! `prev_hash: Hash`, `frame: FrameRef`, `map: Option<MapVersion>` and a
//! per-kind versioned `TypedPayload` **body** — [`Payload`] carries that body's
//! provenance but leaves the body itself a type parameter. **None of them are
//! here**, because each
//! needs a dependency — ULID generation, a hash implementation, frame and map
//! types — and `kirra-world` has **zero dependencies by design**.
//!
//! That is not fastidiousness; it is the argument ADR-0040's open question 1 was
//! decided on. The seam between this crate and `kirra-world-store` was retained
//! precisely so that `rusqlite`, `serde_json`, `sha2` and `hex` do not end up
//! beside the domain types. Pulling a hash in here to finish a struct would spend
//! that decision without revisiting it.
//!
//! So: identity, content-hashing and chaining belong to the **store**, which
//! already implements all three. Frames and maps belong to a later slice.
//!
//! **`ObservationKind` is also absent.** §7.1 names the field but the blueprint
//! never enumerates its variants, and inventing a taxonomy of observation kinds
//! would be a domain decision this module has no basis for.

use crate::trust::Origin;
use core::cmp::Ordering;

// ---------------------------------------------------------------------------
// Confidence — §7.3
// ---------------------------------------------------------------------------

/// What a confidence score is *made of*.
///
/// The axis that makes scores comparable-or-not. Two scores share a basis or
/// they do not, and [`Confidence::compare`] treats that as decisive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfidenceBasis {
    /// A model's own output score.
    ModelScore,
    /// Derived from geometric fit residual.
    GeometricResidual,
    /// A human's stated certainty.
    OperatorCertainty,
    /// Derived from how many independent sources agree. Pairs with
    /// [`crate::trust::Corroboration`].
    Corroboration,
    /// **Synthesized rather than measured.** The honest basis for a value the
    /// producer inferred from context rather than obtained from the datum.
    ///
    /// This variant is what makes ADR-0040's `PerceivedObject` condition
    /// satisfiable: that ruling requires any synthesized confidence to be
    /// *"visible in the store rather than indistinguishable from a measured
    /// value"*. A claim carrying `Assumed` is visibly synthesized — the
    /// distinction survives storage instead of being lost at the boundary.
    Assumed,
    /// No basis stated.
    ///
    /// **Valid and common, by explicit design.** §7.3: *"the design must not
    /// force producers to invent precision they do not have."* A producer with
    /// no confidence to report says so, rather than fabricating one.
    Unspecified,
}

/// How a score was calibrated, if it was.
///
/// An opaque reference; this crate does not interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CalibrationRef(String);

impl CalibrationRef {
    /// Wrap a calibration identifier.
    ///
    /// # Errors
    ///
    /// [`ConfidenceError::EmptyCalibrationRef`] if `id` is empty or whitespace —
    /// an empty reference reads as "calibrated" while naming nothing, which is
    /// worse than `None`.
    pub fn new(id: impl Into<String>) -> Result<Self, ConfidenceError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ConfidenceError::EmptyCalibrationRef);
        }
        Ok(Self(id))
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a confidence could not be built, or two could not be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfidenceError {
    /// The score was NaN or infinite. Fail-closed: a non-finite score is not a
    /// low score, it is an absent one, and the producer must say which.
    ScoreNotFinite,
    /// The score was outside `[0, 1]`.
    ScoreOutOfRange,
    /// A calibration reference was empty.
    EmptyCalibrationRef,
    /// **Cross-modal comparison, refused.** §7.3's whole point: a detector's 0.9
    /// and a geometric residual's 0.9 are not the same quantity. Both bases are
    /// named so the caller can decide, then call
    /// [`Confidence::compare_across_bases`] if the comparison is genuinely
    /// meant.
    BasesDiffer {
        /// The left operand's basis.
        left: ConfidenceBasis,
        /// The right operand's basis.
        right: ConfidenceBasis,
    },
    /// One or both sides carried no score, so there is nothing to order.
    ScoreAbsent,
}

/// A producer's confidence — **structured, never a bare float** (§7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Confidence {
    score: Option<f32>,
    basis: ConfidenceBasis,
    calibration: Option<CalibrationRef>,
}

impl Confidence {
    /// Build a confidence.
    ///
    /// # Errors
    ///
    /// [`ConfidenceError::ScoreNotFinite`] or [`ConfidenceError::ScoreOutOfRange`]
    /// if a score is present and not a finite value in `[0, 1]`.
    pub fn new(
        score: Option<f32>,
        basis: ConfidenceBasis,
        calibration: Option<CalibrationRef>,
    ) -> Result<Self, ConfidenceError> {
        if let Some(s) = score {
            if !s.is_finite() {
                return Err(ConfidenceError::ScoreNotFinite);
            }
            if !(0.0..=1.0).contains(&s) {
                return Err(ConfidenceError::ScoreOutOfRange);
            }
        }
        Ok(Self {
            score,
            basis,
            calibration,
        })
    }

    /// The honest zero-information confidence: no score, no basis, no
    /// calibration. What a producer with nothing to report should send.
    #[must_use]
    pub fn unspecified() -> Self {
        Self {
            score: None,
            basis: ConfidenceBasis::Unspecified,
            calibration: None,
        }
    }

    /// A confidence whose score was **synthesized from context, not measured**.
    ///
    /// The constructor an importer reaches for when the source datum carries no
    /// confidence of its own — see [`ConfidenceBasis::Assumed`].
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn assumed(score: f32) -> Result<Self, ConfidenceError> {
        Self::new(Some(score), ConfidenceBasis::Assumed, None)
    }

    /// The score, if the producer reported one.
    #[must_use]
    pub fn score(&self) -> Option<f32> {
        self.score
    }

    /// What the score is made of.
    #[must_use]
    pub fn basis(&self) -> ConfidenceBasis {
        self.basis
    }

    /// The calibration reference, if any.
    #[must_use]
    pub fn calibration(&self) -> Option<&CalibrationRef> {
        self.calibration.as_ref()
    }

    /// Whether this value was synthesized rather than measured.
    ///
    /// The predicate a store or auditor uses to answer "is any of this made up?"
    #[must_use]
    pub fn is_synthesized(&self) -> bool {
        matches!(self.basis, ConfidenceBasis::Assumed)
    }

    /// Order two confidences — **only when they share a basis**.
    ///
    /// # Errors
    ///
    /// * [`ConfidenceError::BasesDiffer`] if the bases are not identical. This
    ///   is the §7.3 guard, and it is the default path precisely so that the
    ///   silent over-trust it describes cannot happen by accident.
    /// * [`ConfidenceError::ScoreAbsent`] if either side has no score.
    pub fn compare(&self, other: &Self) -> Result<Ordering, ConfidenceError> {
        if self.basis != other.basis {
            return Err(ConfidenceError::BasesDiffer {
                left: self.basis,
                right: other.basis,
            });
        }
        self.compare_across_bases(other)
    }

    /// Order two confidences **ignoring their bases** — the explicit decision
    /// §7.3 asks for, named so it cannot be taken by accident.
    ///
    /// Call this only where the comparison is genuinely meant and its meaning is
    /// argued somewhere. [`Self::compare`] is the default for a reason.
    ///
    /// # Errors
    ///
    /// [`ConfidenceError::ScoreAbsent`] if either side has no score. Absence is
    /// not zero — a producer that reported nothing has not reported low
    /// confidence, and ordering it against a real score would invent a fact.
    pub fn compare_across_bases(&self, other: &Self) -> Result<Ordering, ConfidenceError> {
        let (a, b) = match (self.score, other.score) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(ConfidenceError::ScoreAbsent),
        };
        // Both are validated finite at construction, so partial_cmp is total
        // here; the fallback is unreachable and defensive rather than expected.
        Ok(a.partial_cmp(&b).unwrap_or(Ordering::Equal))
    }
}

// ---------------------------------------------------------------------------
// Source — §7.1 "WHO"
// ---------------------------------------------------------------------------

/// What kind of thing produced an observation — §7.1's `source_class`.
///
/// Distinct from [`Origin`], which is a **trust axis**. This says what the
/// producer *is*; `Origin` says what the resulting claim *counts as*. See
/// [`SourceClass::origin`] for the mapping between them, and why it is proposed
/// rather than inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceClass {
    /// A sensor or perception producer.
    Sensor,
    /// A human operator.
    Operator,
    /// Deployment configuration.
    Configuration,
    /// An external dataset.
    Import,
    /// Computed from other recorded evidence.
    Derivation,
    /// Received from a peer over a network.
    Network,
}

impl SourceClass {
    /// The [`Origin`] a claim from this producer carries.
    ///
    /// # This mapping is proposed, not inherited
    ///
    /// The blueprint defines both enums but never states the correspondence, and
    /// they are not the same size — six source classes, five origins (four
    /// storable). Two cases therefore required a judgement:
    ///
    /// * **`Configuration` → `Imported`.** Deployment config is authored by a
    ///   human, which argues for `Asserted`, but it arrives as data rather than
    ///   as a live ruling on a specific claim. `Imported` claims less.
    /// * **`Network` → `Imported`.** A peer's claim is external evidence. It is
    ///   emphatically **not** `Observed` — this instance did not measure it, and
    ///   treating a relayed claim as a local measurement is how a fleet launders
    ///   provenance across a hop.
    ///
    /// Both ambiguous cases resolve to `Imported` deliberately: of the storable
    /// origins it is the one that asserts least about how the claim was obtained.
    /// Recorded as an open question for the World Model owner: **is this
    /// mapping right, and should `Configuration` be `Asserted`?**
    #[must_use]
    pub fn origin(self) -> Origin {
        match self {
            Self::Sensor => Origin::Observed,
            Self::Operator => Origin::Asserted,
            Self::Derivation => Origin::Derived,
            Self::Import | Self::Configuration | Self::Network => Origin::Imported,
        }
    }
}

/// What an observation is *about* — §7.1's `subject`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubjectRef {
    /// A resolved entity, by opaque id.
    Entity(String),
    /// A candidate that entity resolution has not yet adjudicated.
    Candidate(String),
    /// A spatial frame.
    Frame(String),
    /// **Nothing yet.**
    ///
    /// Load-bearing, not a placeholder: the model is evidence-first, so an
    /// observation may be recorded before anything decides what it is about, and
    /// re-attribution later must not rewrite it. This variant is why the
    /// observation model does not depend on the entity taxonomy.
    Unbound,
}

/// Why a stored subject reference could not be re-admitted.
///
/// Every variant is a *refusal to reconstruct*, never a repair. A store reading
/// a subject it cannot classify must say so: guessing `Entity` because that is
/// the common case would silently promote a candidate — the one distinction the
/// discriminant exists to preserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectRefError {
    /// The token is not one of the four.
    ///
    /// Refused rather than treated as `Unbound`, because "we do not know what
    /// this is about" and "we cannot read what this says it is about" are
    /// different facts, and the second one is a corrupt row.
    UnknownKind {
        /// The token as stored.
        token: String,
    },
    /// A kind that names something was given nothing to name.
    KindNeedsId {
        /// Which kind.
        kind: &'static str,
    },
    /// [`SubjectRef::Unbound`] was given an id.
    ///
    /// `Unbound` means nothing has decided what the observation is about. An
    /// id attached to it is a contradiction, and admitting it would produce a
    /// value whose own `id()` disagrees with its variant.
    UnboundCarriesId,
    /// The id was empty or whitespace-only.
    ///
    /// Same rule as [`crate::reference`]: an empty identity is not a missing
    /// one, and `Some("")` reads as present at every call site that checks for
    /// presence.
    EmptyId,
}

impl core::fmt::Display for SubjectRefError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownKind { token } => {
                write!(f, "{token:?} is not a subject kind")
            }
            Self::KindNeedsId { kind } => write!(f, "a {kind} subject needs an id"),
            Self::UnboundCarriesId => {
                write!(f, "an unbound subject cannot carry an id")
            }
            Self::EmptyId => write!(f, "a subject id may not be empty"),
        }
    }
}

impl SubjectRef {
    /// The closed-vocabulary token for this reference's **kind**.
    ///
    /// Named `as_str` to match the store's existing convention, alongside
    /// [`crate::trust::Origin::as_str`] and `WriterClass`.
    ///
    /// This is the discriminant only — the *value* is [`Self::id`]. The two are
    /// separate because a store already keeps the value in a `subject` column;
    /// what it has never kept is which of the four cases that value is, so a
    /// projection cannot restrict itself to resolved entities. That gap is
    /// recorded in `kirra-world-store`'s `subject_projection`, which is named
    /// for what it actually computes rather than for what it looks like.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Entity(_) => "entity",
            Self::Candidate(_) => "candidate",
            Self::Frame(_) => "frame",
            Self::Unbound => "unbound",
        }
    }

    /// The id this reference names, if it names one.
    ///
    /// `None` **only** for [`Self::Unbound`] — and that asymmetry is the reason
    /// a storage layer cannot represent all four cases in a `NOT NULL` subject
    /// column without fabricating a value for the one case that has none.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Entity(id) | Self::Candidate(id) | Self::Frame(id) => Some(id),
            Self::Unbound => None,
        }
    }

    /// Re-admit a reference from a stored token and a stored id.
    ///
    /// The inverse of ([`Self::as_str`], [`Self::id`]), for a reader rebuilding
    /// a row. **Validates, never normalizes** — the id is carried verbatim, so
    /// a value stored with surrounding whitespace round-trips as written. A
    /// constructor that trimmed here would make an untampered row rehash
    /// differently from how it was written.
    ///
    /// # This is a re-admission path, not a gate
    ///
    /// Worth stating rather than implying: `SubjectRef`'s variants are **public
    /// tuple variants**, so `SubjectRef::Entity(String::new())` compiles and
    /// this function never sees it. The refusals below constrain what can be
    /// *read back*, which is the path a store needs; they do not make a
    /// malformed value unconstructible in memory. Same honest limit as
    /// `ProjectedClaim`'s public fields.
    ///
    /// # Errors
    ///
    /// [`SubjectRefError::UnknownKind`] for a token outside the four;
    /// [`SubjectRefError::KindNeedsId`] for a naming kind with no id;
    /// [`SubjectRefError::UnboundCarriesId`] for the reverse;
    /// [`SubjectRefError::EmptyId`] for an id that is empty or whitespace.
    pub fn from_stored_parts(token: &str, id: Option<&str>) -> Result<Self, SubjectRefError> {
        if let Some(v) = id {
            if v.trim().is_empty() {
                return Err(SubjectRefError::EmptyId);
            }
        }
        let need = |kind: &'static str| -> Result<String, SubjectRefError> {
            id.map(ToOwned::to_owned)
                .ok_or(SubjectRefError::KindNeedsId { kind })
        };
        match token {
            "entity" => Ok(Self::Entity(need("entity")?)),
            "candidate" => Ok(Self::Candidate(need("candidate")?)),
            "frame" => Ok(Self::Frame(need("frame")?)),
            "unbound" => {
                if id.is_some() {
                    return Err(SubjectRefError::UnboundCarriesId);
                }
                Ok(Self::Unbound)
            }
            other => Err(SubjectRefError::UnknownKind {
                token: other.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Time — §7.1 "WHEN", and the non-mixing rule
// ---------------------------------------------------------------------------

/// Which clock an instant was read from.
///
/// §5.1 and `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` §5 both state the
/// normative rule: **clock domains do not mix**, and conversion happens at the
/// producing edge (`AOU-TIMESYNC-001`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClockDomain {
    /// The safety/boundary timing domain.
    Boundary,
    /// General system timing.
    System,
}

/// An instant that knows which clock it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomainInstant {
    /// Milliseconds on `domain`'s clock.
    pub ms: u64,
    /// The clock this reading came from.
    pub domain: ClockDomain,
}

/// Why two instants could not be compared, or an interval could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeError {
    /// **Clock domains do not mix.** Refused rather than answered, because a
    /// cross-domain ordering is not merely imprecise — it is meaningless, and
    /// returning one would be confidently wrong.
    DomainsDiffer {
        /// The left operand's domain.
        left: ClockDomain,
        /// The right operand's domain.
        right: ClockDomain,
    },
    /// An interval ended before it began.
    IntervalEndsBeforeItStarts,
}

impl DomainInstant {
    /// Order two instants — **only within one clock domain**.
    ///
    /// # Errors
    ///
    /// [`TimeError::DomainsDiffer`] on a cross-domain comparison. There is
    /// deliberately no `compare_across_domains` escape hatch, unlike
    /// [`Confidence::compare_across_bases`]: comparing two confidences of
    /// different bases is *unwise*, while comparing two clocks that were never
    /// synchronized is *unsound*. Conversion is the producing edge's job.
    pub fn compare(&self, other: &Self) -> Result<Ordering, TimeError> {
        if self.domain != other.domain {
            return Err(TimeError::DomainsDiffer {
                left: self.domain,
                right: other.domain,
            });
        }
        Ok(self.ms.cmp(&other.ms))
    }
}

/// When a claim held in the world — §7.1's `valid_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidInterval {
    start: DomainInstant,
    end: Option<DomainInstant>,
}

impl ValidInterval {
    /// Build an interval.
    ///
    /// # Errors
    ///
    /// * [`TimeError::DomainsDiffer`] if `start` and `end` are on different
    ///   clocks — an interval spanning two unsynchronized clocks has no length.
    /// * [`TimeError::IntervalEndsBeforeItStarts`] if `end` precedes `start`.
    pub fn new(start: DomainInstant, end: Option<DomainInstant>) -> Result<Self, TimeError> {
        if let Some(e) = end {
            if start.compare(&e)? == Ordering::Greater {
                return Err(TimeError::IntervalEndsBeforeItStarts);
            }
        }
        Ok(Self { start, end })
    }

    /// An interval that has begun and has no stated end.
    #[must_use]
    pub fn open_from(start: DomainInstant) -> Self {
        Self { start, end: None }
    }

    /// When it began.
    #[must_use]
    pub fn start(&self) -> DomainInstant {
        self.start
    }

    /// When it ended, if it has.
    #[must_use]
    pub fn end(&self) -> Option<DomainInstant> {
        self.end
    }

    /// The clock domain this interval lives in.
    #[must_use]
    pub fn domain(&self) -> ClockDomain {
        self.start.domain
    }

    /// Project into a [`crate::trust::ValidityWindow`], so the trust model's
    /// read-time validity question can be asked about this observation.
    ///
    /// `ttl_ms` is §7.1's producer-declared freshness budget; `None` means the
    /// claim does not go stale.
    ///
    /// # The domain is dropped here, deliberately
    ///
    /// `ValidityWindow` carries bare `u64` milliseconds. That is safe **only**
    /// because every field of this interval is already proven to share one
    /// domain — `new` refuses a mixed one. The caller must pass a `now_ms` read
    /// from [`Self::domain`]'s clock; nothing downstream can check that for
    /// them, which is exactly why the check happens here instead.
    #[must_use]
    pub fn to_validity_window(&self, ttl_ms: Option<u64>) -> crate::trust::ValidityWindow {
        crate::trust::ValidityWindow {
            valid_from_ms: self.start.ms,
            valid_to_ms: self.end.map(|e| e.ms),
            staleness_budget_ms: ttl_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Payload provenance — §9.2 rule 4's geometry half, and P10
// ---------------------------------------------------------------------------

/// Why a payload could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PayloadError {
    /// A payload reference was empty.
    EmptyPayloadRef,
}

/// A reference to an earlier recorded payload.
///
/// Opaque here on the same terms as [`crate::relationship::DerivationRef`]:
/// this crate stores the reference, the store resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PayloadRef(String);

impl PayloadRef {
    /// Wrap a reference.
    ///
    /// # Errors
    ///
    /// [`PayloadError::EmptyPayloadRef`] if empty or whitespace.
    pub fn new(r: impl Into<String>) -> Result<Self, PayloadError> {
        let r = r.into();
        if r.trim().is_empty() {
            return Err(PayloadError::EmptyPayloadRef);
        }
        Ok(Self(r))
    }

    /// The reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a payload's **content** came from — the distinction §9.2 rule 4 turns on.
///
/// Rule 4: *"An operator correcting a pose creates a `Correction` observation
/// whose payload is itself an operator-sourced measurement, **visibly distinct
/// from a sensed one**."*
///
/// "Visibly distinct" is the whole requirement, and it is a statement about
/// provenance rather than about geometry — which is why this can be enforced
/// here, in a crate that has no pose type and wants none.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PayloadSource {
    /// Produced directly by this class of producer.
    Produced(SourceClass),
    /// **An operator correction of an earlier payload.**
    ///
    /// There is deliberately no [`SourceClass`] field. §7.2 lists `Correction`
    /// under the **Operator** source class, so a correction is operator-sourced
    /// by definition — and with no field to fill, "a correction that presents
    /// as sensed" cannot be spelled. Same move as
    /// [`crate::relationship::Origination::Inferred`] carrying its derivation:
    /// the thing that must never be dropped lives in the variant.
    Correction {
        /// What is being corrected. A correction that does not say what it
        /// corrects defeats the invalidate-by-provenance rule the same way an
        /// uncited inference does.
        of: PayloadRef,
    },
}

/// An observation's payload: content, plus **who produced it**, inseparably.
///
/// # The body is a type parameter, not a type this crate invents
///
/// §7.1's `TypedPayload` is per-kind and versioned, and belongs to the store
/// along with identity, hashing, frames and maps — see the module header's
/// dependency argument. Making the body generic keeps that seam intact while
/// still letting the rule below be enforced *here*, where the trust model is.
/// The store plugs in its own body type and inherits the guarantee.
///
/// # Rule 4's geometry half, and what enforces it
///
/// The failure the rule names is *"an operator assertion silently rewriting a
/// measured pose"*. Two things make it unavailable, neither of them a check:
///
/// 1. **There is no way to modify a payload.** No setter, no `&mut` accessor,
///    and no method anywhere that takes a `Payload` and returns a changed one.
///    A correction is built by [`Payload::correction`], an associated function
///    — it never receives the payload it corrects, only a reference to it. The
///    measured record is not merely protected from rewriting; it is not
///    reachable from the operation.
/// 2. **A correction cannot inherit the corrected payload's source class**,
///    because [`PayloadSource::Correction`] has no such field. That is the
///    laundering hole worth closing: had `correction` copied the original's
///    class, an operator's numbers would carry `Sensor` provenance and become
///    exactly the *invisible* rewrite the rule forbids.
///
/// # What this does NOT guarantee — read before relying on it
///
/// * **That `of` names the payload being corrected.** Identity is the store's
///   (§7.1's `observation_id` is a ULID, absent here by the dependency
///   argument), so this crate cannot check the reference resolves, let alone
///   that it resolves to the right record. It guarantees a correction *cites
///   something*, not that it cites truthfully.
/// * **That a producer told the truth about its own class.** A writer
///   constructing `Produced(SourceClass::Sensor)` for hand-typed numbers is
///   lying at the producing edge, where §7.2's per-source schemas live. No type
///   here can catch that, and pretending otherwise would be worse than saying so.
/// * **Anything about the numbers.** The body is opaque by construction, so
///   this module cannot tell a plausible corrected pose from an absurd one.
///   Rule 4 does not ask it to; the rule is about provenance being visible, and
///   that is the part being kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Payload<B> {
    body: B,
    source: PayloadSource,
}

impl<B> Payload<B> {
    /// A payload produced directly by `by`.
    #[must_use]
    pub fn produced(body: B, by: SourceClass) -> Self {
        Self {
            body,
            source: PayloadSource::Produced(by),
        }
    }

    /// **An operator's correction of an earlier payload.**
    ///
    /// Note what is absent: this does not take the payload being corrected, so
    /// there is no path by which the correction could copy its provenance, and
    /// no path by which the original could be altered. It takes a *reference*,
    /// and stamps the result operator-sourced itself.
    #[must_use]
    pub fn correction(of: PayloadRef, body: B) -> Self {
        Self {
            body,
            source: PayloadSource::Correction { of },
        }
    }

    /// The content.
    #[must_use]
    pub fn body(&self) -> &B {
        &self.body
    }

    /// Where the content came from.
    #[must_use]
    pub fn source(&self) -> &PayloadSource {
        &self.source
    }

    /// What this corrects, if it is a correction.
    #[must_use]
    pub fn corrects(&self) -> Option<&PayloadRef> {
        match &self.source {
            PayloadSource::Correction { of } => Some(of),
            PayloadSource::Produced(_) => None,
        }
    }

    /// **Whether an operator supplied these numbers** — the P10 read.
    ///
    /// True for a direct operator assertion *and* for a correction. A consumer
    /// that must not take geometry from language asks this one question, and
    /// the answer cannot be laundered by routing an assertion through a
    /// correction.
    #[must_use]
    pub fn is_operator_supplied(&self) -> bool {
        match &self.source {
            PayloadSource::Produced(c) => *c == SourceClass::Operator,
            PayloadSource::Correction { .. } => true,
        }
    }

    /// Whether this system **sensed** the content.
    ///
    /// [`SourceClass::Sensor`] only — deliberately narrow, and not the negation
    /// of [`Self::is_operator_supplied`]. An imported map layer and a derived
    /// fact are neither operator-supplied nor sensed, and collapsing the three
    /// into one boolean is how a relayed or inferred value comes to be read as
    /// a local measurement. Compare [`SourceClass::origin`]'s refusal to map
    /// `Network` to `Observed`, for the same reason.
    #[must_use]
    pub fn is_sensed(&self) -> bool {
        matches!(&self.source, PayloadSource::Produced(SourceClass::Sensor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{validity_at, Validity};

    // -- §7.3: cross-modal comparison is refused, not discouraged ---------

    #[test]
    fn comparing_across_bases_is_refused_by_default() {
        // THE §7.3 rule: "a detector's 0.9 and a geometric residual's 0.9 are
        // not comparable, and treating them as such is how fusion systems
        // silently over-trust."
        let detector = Confidence::new(Some(0.9), ConfidenceBasis::ModelScore, None).unwrap();
        let residual =
            Confidence::new(Some(0.9), ConfidenceBasis::GeometricResidual, None).unwrap();

        assert_eq!(
            detector.compare(&residual),
            Err(ConfidenceError::BasesDiffer {
                left: ConfidenceBasis::ModelScore,
                right: ConfidenceBasis::GeometricResidual,
            })
        );

        // ...and the explicit decision is available, by a name that shows up in
        // review.
        assert_eq!(
            detector.compare_across_bases(&residual),
            Ok(Ordering::Equal)
        );
    }

    #[test]
    fn same_basis_compares_normally() {
        let low = Confidence::new(Some(0.2), ConfidenceBasis::ModelScore, None).unwrap();
        let high = Confidence::new(Some(0.8), ConfidenceBasis::ModelScore, None).unwrap();
        assert_eq!(low.compare(&high), Ok(Ordering::Less));
        assert_eq!(high.compare(&low), Ok(Ordering::Greater));
    }

    #[test]
    fn an_absent_score_is_not_a_low_score() {
        let none = Confidence::unspecified();
        let some = Confidence::new(Some(0.0), ConfidenceBasis::Unspecified, None).unwrap();
        // Same basis, so the basis guard passes — but there is still nothing to
        // order, and 0.0 must not be invented for the absent side.
        assert_eq!(none.compare(&some), Err(ConfidenceError::ScoreAbsent));
    }

    #[test]
    fn a_non_finite_score_is_refused_rather_than_clamped() {
        assert_eq!(
            Confidence::new(Some(f32::NAN), ConfidenceBasis::ModelScore, None),
            Err(ConfidenceError::ScoreNotFinite)
        );
        assert_eq!(
            Confidence::new(Some(f32::INFINITY), ConfidenceBasis::ModelScore, None),
            Err(ConfidenceError::ScoreNotFinite)
        );
        assert_eq!(
            Confidence::new(Some(1.5), ConfidenceBasis::ModelScore, None),
            Err(ConfidenceError::ScoreOutOfRange)
        );
        assert_eq!(
            Confidence::new(Some(-0.1), ConfidenceBasis::ModelScore, None),
            Err(ConfidenceError::ScoreOutOfRange)
        );
    }

    #[test]
    fn the_boundaries_of_the_range_are_admitted() {
        assert!(Confidence::new(Some(0.0), ConfidenceBasis::ModelScore, None).is_ok());
        assert!(Confidence::new(Some(1.0), ConfidenceBasis::ModelScore, None).is_ok());
    }

    // -- The ADR-0040 PerceivedObject condition ---------------------------

    #[test]
    fn a_synthesized_confidence_is_visibly_synthesized() {
        // ADR-0040's tracked-object ruling requires synthesized confidence to be
        // "visible in the store rather than indistinguishable from a measured
        // value". `Assumed` is what makes that satisfiable.
        let synthesized = Confidence::assumed(0.7).unwrap();
        let measured = Confidence::new(Some(0.7), ConfidenceBasis::ModelScore, None).unwrap();

        assert!(synthesized.is_synthesized());
        assert!(!measured.is_synthesized());

        // Identical scores, and they still do not compare — the basis guard
        // catches exactly the confusion the ruling was worried about.
        assert!(matches!(
            synthesized.compare(&measured),
            Err(ConfidenceError::BasesDiffer { .. })
        ));
    }

    #[test]
    fn a_producer_with_nothing_to_report_says_so() {
        let c = Confidence::unspecified();
        assert_eq!(c.score(), None);
        assert_eq!(c.basis(), ConfidenceBasis::Unspecified);
        assert!(!c.is_synthesized());
    }

    #[test]
    fn an_empty_calibration_reference_is_refused() {
        assert_eq!(
            CalibrationRef::new("   "),
            Err(ConfidenceError::EmptyCalibrationRef)
        );
        assert!(CalibrationRef::new("platt-2026-01").is_ok());
    }

    // -- Clock domains do not mix -----------------------------------------

    #[test]
    fn cross_domain_comparison_is_refused() {
        let boundary = DomainInstant {
            ms: 100,
            domain: ClockDomain::Boundary,
        };
        let system = DomainInstant {
            ms: 200,
            domain: ClockDomain::System,
        };
        assert_eq!(
            boundary.compare(&system),
            Err(TimeError::DomainsDiffer {
                left: ClockDomain::Boundary,
                right: ClockDomain::System,
            })
        );
        // Within one domain it is an ordinary comparison.
        let later = DomainInstant {
            ms: 300,
            domain: ClockDomain::Boundary,
        };
        assert_eq!(boundary.compare(&later), Ok(Ordering::Less));
    }

    #[test]
    fn an_interval_cannot_span_two_clocks() {
        let start = DomainInstant {
            ms: 0,
            domain: ClockDomain::Boundary,
        };
        let end = DomainInstant {
            ms: 10,
            domain: ClockDomain::System,
        };
        assert!(matches!(
            ValidInterval::new(start, Some(end)),
            Err(TimeError::DomainsDiffer { .. })
        ));
    }

    #[test]
    fn an_interval_cannot_end_before_it_starts() {
        let start = DomainInstant {
            ms: 10,
            domain: ClockDomain::Boundary,
        };
        let end = DomainInstant {
            ms: 9,
            domain: ClockDomain::Boundary,
        };
        assert_eq!(
            ValidInterval::new(start, Some(end)),
            Err(TimeError::IntervalEndsBeforeItStarts)
        );
        // Zero-length is admitted: a claim can hold at an instant.
        let same = DomainInstant {
            ms: 10,
            domain: ClockDomain::Boundary,
        };
        assert!(ValidInterval::new(start, Some(same)).is_ok());
    }

    // -- Composition with the trust model ---------------------------------

    #[test]
    fn an_interval_projects_into_the_trust_validity_question() {
        let start = DomainInstant {
            ms: 1_000,
            domain: ClockDomain::Boundary,
        };
        let interval = ValidInterval::open_from(start);
        let window = interval.to_validity_window(Some(500));

        assert_eq!(validity_at(&window, 1_200), Validity::Fresh);
        assert_eq!(validity_at(&window, 1_600), Validity::Stale);
        // And with no TTL the claim is timeless, per rule 6's Timeless arm.
        assert_eq!(
            validity_at(&interval.to_validity_window(None), u64::MAX),
            Validity::Timeless
        );
    }

    #[test]
    fn a_closed_interval_carries_its_end_into_expiry() {
        let start = DomainInstant {
            ms: 0,
            domain: ClockDomain::System,
        };
        let end = DomainInstant {
            ms: 100,
            domain: ClockDomain::System,
        };
        let window = ValidInterval::new(start, Some(end))
            .unwrap()
            .to_validity_window(None);
        assert_eq!(validity_at(&window, 99), Validity::Timeless);
        assert_eq!(validity_at(&window, 100), Validity::Expired);
    }

    // -- Source class -> origin -------------------------------------------

    #[test]
    fn a_relayed_claim_is_never_treated_as_locally_observed() {
        // The laundering-across-a-hop case: this instance did not measure a
        // peer's claim, and Network must not map to Observed.
        assert_ne!(SourceClass::Network.origin(), Origin::Observed);
        assert_eq!(SourceClass::Network.origin(), Origin::Imported);
    }

    #[test]
    fn source_classes_map_to_their_origins() {
        assert_eq!(SourceClass::Sensor.origin(), Origin::Observed);
        assert_eq!(SourceClass::Operator.origin(), Origin::Asserted);
        assert_eq!(SourceClass::Derivation.origin(), Origin::Derived);
        assert_eq!(SourceClass::Import.origin(), Origin::Imported);
        assert_eq!(SourceClass::Configuration.origin(), Origin::Imported);
    }

    #[test]
    fn no_source_class_maps_to_the_unstorable_origin() {
        // Origin::Predicted never appears in the evidence store (blueprint §20),
        // and TrustAxes::new refuses it — so no producer may route to it.
        for sc in [
            SourceClass::Sensor,
            SourceClass::Operator,
            SourceClass::Configuration,
            SourceClass::Import,
            SourceClass::Derivation,
            SourceClass::Network,
        ] {
            assert_ne!(sc.origin(), Origin::Predicted, "{sc:?} routed to Predicted");
        }
    }

    // -- Subject discriminant ----------------------------------------------

    /// Every variant round-trips through its stored form.
    ///
    /// Walked over all four rather than a sample, because the point of the
    /// discriminant is that the cases are *distinguishable*, and a round-trip
    /// that skipped one would not notice it collapsing into another.
    #[test]
    fn every_subject_kind_round_trips_through_its_stored_form() {
        let cases = [
            SubjectRef::Entity("e-1".into()),
            SubjectRef::Candidate("c-1".into()),
            SubjectRef::Frame("map/base_link".into()),
            SubjectRef::Unbound,
        ];
        for original in cases {
            let back = SubjectRef::from_stored_parts(original.as_str(), original.id())
                .unwrap_or_else(|e| panic!("{original:?} did not re-admit: {e}"));
            assert_eq!(back, original);
        }
    }

    /// **The tokens are distinct**, so no two kinds collapse into one.
    ///
    /// This looks like it cannot fail until you notice these tokens are about
    /// to be written into a hash-chained evidence row. Two kinds sharing a
    /// token would make a candidate indistinguishable from a resolved entity
    /// *in the stored bytes* — unrecoverable afterwards, since the row would
    /// verify perfectly.
    #[test]
    fn the_four_kinds_have_four_distinct_tokens() {
        let tokens = [
            SubjectRef::Entity(String::from("x")).as_str(),
            SubjectRef::Candidate(String::from("x")).as_str(),
            SubjectRef::Frame(String::from("x")).as_str(),
            SubjectRef::Unbound.as_str(),
        ];
        let mut seen: Vec<&str> = Vec::new();
        for t in tokens {
            assert!(!seen.contains(&t), "token {t:?} is used by two kinds");
            seen.push(t);
        }
        assert_eq!(seen.len(), 4);
    }

    /// The tokens are pinned to their exact spellings.
    ///
    /// Not redundant with the round-trip: that would pass just as happily after
    /// a rename, because it reads the token back through the same function that
    /// wrote it. Once these are inside hashed bytes a rename is not a
    /// refactor — it changes the digest of every row that carried the old one.
    #[test]
    fn the_tokens_are_the_exact_spellings_a_store_will_hash() {
        assert_eq!(SubjectRef::Entity("x".into()).as_str(), "entity");
        assert_eq!(SubjectRef::Candidate("x".into()).as_str(), "candidate");
        assert_eq!(SubjectRef::Frame("x".into()).as_str(), "frame");
        assert_eq!(SubjectRef::Unbound.as_str(), "unbound");
    }

    /// Only `Unbound` has no id, and that is what a `NOT NULL` subject column
    /// cannot represent without inventing one.
    #[test]
    fn unbound_is_the_only_kind_without_an_id() {
        assert_eq!(SubjectRef::Entity("e-1".into()).id(), Some("e-1"));
        assert_eq!(SubjectRef::Candidate("c-1".into()).id(), Some("c-1"));
        assert_eq!(SubjectRef::Frame("f-1".into()).id(), Some("f-1"));
        assert_eq!(SubjectRef::Unbound.id(), None);
    }

    /// An unreadable kind is refused, **not** downgraded to `Unbound`.
    ///
    /// The tempting default is the wrong one in both directions: reading it as
    /// `Entity` silently promotes a candidate, and reading it as `Unbound`
    /// reports "nothing decided what this is about" when the truth is "this row
    /// is corrupt". Those send an investigator to different places.
    #[test]
    fn an_unreadable_kind_is_refused_rather_than_guessed() {
        assert_eq!(
            SubjectRef::from_stored_parts("Entity", Some("e-1")).expect_err("refused"),
            SubjectRefError::UnknownKind {
                token: "Entity".into()
            },
            "case matters -- the token is a stored byte string, not a label"
        );
        assert_eq!(
            SubjectRef::from_stored_parts("", Some("e-1")).expect_err("refused"),
            SubjectRefError::UnknownKind {
                token: String::new()
            }
        );
    }

    /// A kind that names something cannot be re-admitted naming nothing.
    #[test]
    fn a_naming_kind_without_an_id_is_refused() {
        for kind in ["entity", "candidate", "frame"] {
            assert_eq!(
                SubjectRef::from_stored_parts(kind, None).expect_err("refused"),
                SubjectRefError::KindNeedsId { kind },
                "and the error names WHICH kind was left empty"
            );
        }
    }

    /// ...and the reverse: `Unbound` carrying an id is a contradiction.
    #[test]
    fn an_unbound_subject_carrying_an_id_is_refused() {
        assert_eq!(
            SubjectRef::from_stored_parts("unbound", Some("e-1")).expect_err("refused"),
            SubjectRefError::UnboundCarriesId,
            "admitting it would produce a value whose own id() disagrees with \
             its variant"
        );
    }

    /// An empty id is refused, and checked BEFORE the kind is resolved.
    ///
    /// Ordering matters here: an empty id is the more fundamental defect, and
    /// reporting `KindNeedsId` for `("entity", Some(""))` would send a reader
    /// looking for a missing column rather than an empty one.
    #[test]
    fn an_empty_id_is_refused_and_reported_as_the_emptier_fault() {
        assert_eq!(
            SubjectRef::from_stored_parts("entity", Some("")).expect_err("refused"),
            SubjectRefError::EmptyId
        );
        assert_eq!(
            SubjectRef::from_stored_parts("entity", Some("   ")).expect_err("refused"),
            SubjectRefError::EmptyId,
            "whitespace-only is empty for this purpose, as in `reference`"
        );
        // ...and it outranks an unknown kind for the same reason.
        assert_eq!(
            SubjectRef::from_stored_parts("nonsense", Some("")).expect_err("refused"),
            SubjectRefError::EmptyId
        );
    }

    /// Ids are carried **verbatim**, not tidied.
    ///
    /// A store rehashes what it read. Trimming here would make an untampered
    /// row whose id legitimately holds surrounding whitespace re-admit as a
    /// different string, and report a broken chain.
    #[test]
    fn an_id_is_re_admitted_verbatim_rather_than_tidied() {
        let padded = " e-1 ";
        let back = SubjectRef::from_stored_parts("entity", Some(padded)).expect("admitted");
        assert_eq!(back.id(), Some(padded), "not trimmed");
    }

    // -- Evidence-first ----------------------------------------------------

    #[test]
    fn an_observation_subject_may_be_unbound() {
        // Why this module does not depend on the entity taxonomy: evidence can
        // be recorded before anything decides what it is about.
        let s = SubjectRef::Unbound;
        assert_eq!(s, SubjectRef::Unbound);
        assert_ne!(s, SubjectRef::Candidate("c-1".into()));
    }

    // -- §9.2 rule 4, geometry half: P10 -----------------------------------

    fn pref(s: &str) -> PayloadRef {
        PayloadRef::new(s).unwrap()
    }

    #[test]
    fn a_correction_is_visibly_distinct_from_the_pose_it_corrects() {
        // THE rule: "an operator correcting a pose creates a Correction
        // observation whose payload is itself an operator-sourced measurement,
        // visibly distinct from a sensed one."
        let measured = Payload::produced("pose@sensor", SourceClass::Sensor);
        let corrected = Payload::correction(pref("obs-77"), "pose@operator");

        assert!(measured.is_sensed());
        assert!(!measured.is_operator_supplied());

        // Visibly distinct, on the one question P10 asks.
        assert!(corrected.is_operator_supplied());
        assert!(!corrected.is_sensed());
        assert_eq!(corrected.corrects().unwrap().as_str(), "obs-77");
    }

    #[test]
    fn a_correction_cannot_inherit_the_corrected_payloads_source_class() {
        // The laundering hole: if `correction` copied the original's class, an
        // operator's numbers would carry Sensor provenance — the INVISIBLE
        // rewrite rule 4 forbids. The variant has no field to copy into.
        let corrected = Payload::correction(pref("obs-77"), "pose@operator");
        assert_eq!(
            corrected.source(),
            &PayloadSource::Correction { of: pref("obs-77") }
        );
        // Exhaustive over every producer class: none of them can be what a
        // correction reports as its source.
        for class in [
            SourceClass::Sensor,
            SourceClass::Operator,
            SourceClass::Configuration,
            SourceClass::Import,
            SourceClass::Derivation,
            SourceClass::Network,
        ] {
            assert_ne!(corrected.source(), &PayloadSource::Produced(class));
        }
    }

    #[test]
    fn the_measured_payload_is_untouched_by_a_correction() {
        // `correction` never receives the payload it corrects — the original is
        // not reachable from the operation, so "silently rewrite" has no path.
        let measured = Payload::produced("pose@sensor", SourceClass::Sensor);
        let before = measured.clone();
        let _correction = Payload::correction(pref("obs-77"), "pose@operator");
        assert_eq!(measured, before);
        assert_eq!(*measured.body(), "pose@sensor");
        assert!(measured.is_sensed());
    }

    #[test]
    fn routing_an_assertion_through_a_correction_does_not_launder_it() {
        // Both spellings of operator-supplied geometry answer P10's question the
        // same way. A consumer that must not take geometry from language cannot
        // be dodged by choosing the other variant.
        let asserted = Payload::produced("pose@operator", SourceClass::Operator);
        let corrected = Payload::correction(pref("obs-77"), "pose@operator");
        assert!(asserted.is_operator_supplied());
        assert!(corrected.is_operator_supplied());
    }

    #[test]
    fn sensed_is_narrower_than_not_operator_supplied() {
        // Deliberately NOT the negation of each other. An imported map layer and
        // a derived fact are neither — collapsing the three is how a relayed or
        // inferred value gets read as a local measurement.
        for class in [
            SourceClass::Configuration,
            SourceClass::Import,
            SourceClass::Derivation,
            SourceClass::Network,
        ] {
            let p = Payload::produced("x", class);
            assert!(!p.is_operator_supplied(), "{class:?}");
            assert!(!p.is_sensed(), "{class:?}");
        }
    }

    #[test]
    fn a_correction_must_cite_something() {
        // Same class of hole as an uncited inference: PayloadRef refuses empty,
        // and the variant has no None case, so "corrects nothing" is unspellable.
        assert_eq!(PayloadRef::new("  "), Err(PayloadError::EmptyPayloadRef));
        assert_eq!(PayloadRef::new(""), Err(PayloadError::EmptyPayloadRef));
        assert!(PayloadRef::new("obs-77").is_ok());
    }
}
