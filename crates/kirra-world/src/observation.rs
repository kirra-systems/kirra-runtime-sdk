//! The observation model's pure half — `KIRRA-WM-ARCH-001` §7, Tier 1.
//!
//! §7.1 specifies an eighteen-field immutable `Observation` record. **This
//! module implements the part of it that can be pure**, and deliberately stops
//! short of the rest — see *What is missing and why* below.
//!
//! # Two rules made structural rather than remembered
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
//! # What is missing, and why — the dependency argument
//!
//! §7.1's record also carries `observation_id: Ulid`, `evidence_digest: Hash`,
//! `prev_hash: Hash`, `frame: FrameRef`, `map: Option<MapVersion>` and a
//! per-kind versioned `TypedPayload`. **None of them are here**, because each
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

    // -- Evidence-first ----------------------------------------------------

    #[test]
    fn an_observation_subject_may_be_unbound() {
        // Why this module does not depend on the entity taxonomy: evidence can
        // be recorded before anything decides what it is about.
        let s = SubjectRef::Unbound;
        assert_eq!(s, SubjectRef::Unbound);
        assert_ne!(s, SubjectRef::Candidate("c-1".into()));
    }
}
