//! The retention policy — ADR-0040's Tier 1 exit criterion, pure half.
//!
//! `kirra-world-store` already implements *how* to compact: `compact_range`
//! removes a span and leaves a citation, refusing any window that holds a
//! protected class or a projection head (ADR-0041 §11.3, and
//! `kirra_world_store::compaction`'s header for why those two refusals are not
//! alike). What has never existed is *when* — nothing in the workspace calls
//! it, which is why the horizons OQ2 ruled have never been enforced and the
//! store fills in the 15.79 days D-20/D-21 measured.
//!
//! This module is the deciding half: pure, clock-injected, and dependency-free
//! like the rest of the crate. The scheduler that acts on it is the store side's.
//!
//! # Why the answer is not `Option<Range>`
//!
//! The same reasoning [`crate::ResolutionOutcome`] records for query results,
//! and here it is load-bearing rather than tidy. A retention pass has four
//! outcomes an operator must be able to tell apart:
//!
//! * nothing is old enough yet,
//! * something is old enough but a **protected class** pins it,
//! * something is old enough but a **projection head** pins it,
//! * compact this range.
//!
//! `Option<(lo, hi)>` collapses the first three into `None`. That matters
//! because the second one is the failure mode worth watching: a long-lived
//! protected event sits in front of a large raw span and holds it, so the store
//! keeps growing while retention reports, truthfully and uselessly, that it did
//! nothing. Making the blocked cases distinct values is what lets the driver
//! observe that instead of discovering it as a full disk.
//!
//! # The two refusals are not alike, and that is encoded
//!
//! Taken verbatim from the store's own analysis, which pre-agreed the
//! escalation rather than leaving it to be argued later:
//!
//! * A **protected class** makes the window a policy unit — refusing it whole
//!   is the *requirement*, because compacting around it would make "how much of
//!   this span survives" a question about arrival order.
//! * A **projection head** only requires that the head itself survive.
//!   Refusing the whole window is a conservative simplification, and the agreed
//!   escalation, should measurement ever justify it, is to compact *around*
//!   heads.
//!
//! [`Blocker::may_compact_around`] is that distinction as a value rather than a
//! paragraph, so a future driver cannot apply the escalation to the refusal it
//! was never agreed for.

use crate::observation::{ClockDomain, DomainInstant};

/// Milliseconds in a day.
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

/// OQ2's ruled horizon for the `raw` retention class, in days.
pub const RAW_HORIZON_DAYS: u64 = 30;

/// OQ2's ruled horizon for the protected classes, in days.
///
/// The classes are `safety`, `incident`, `calibration`, `adjudication` and
/// `operator` — enumerated in `kirra_world_store::compaction::PROTECTED_CLASSES`
/// and deliberately not duplicated here, since two copies of one list is how
/// they come to disagree.
pub const PROTECTED_HORIZON_DAYS: u64 = 365;

/// Why a retention policy could not be built, or a decision could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionError {
    /// A horizon of zero days would make every event immediately eligible.
    /// Refused rather than honoured: a policy that deletes on arrival is far
    /// more likely to be a units mistake than an intent.
    HorizonZero,
    /// **The protected horizon was shorter than the raw one.** Refused, because
    /// it inverts the entire point: protected classes would age out *before*
    /// the ordinary traffic they exist to outlive.
    ProtectedShorterThanRaw {
        /// The raw horizon offered, in days.
        raw_days: u64,
        /// The protected horizon offered, in days.
        protected_days: u64,
    },
    /// A retention decision was asked of a non-wall clock.
    ///
    /// Retention is a question about calendar durations. The
    /// [`ClockDomain::Boundary`] clock is the safety timing domain, and a
    /// 30-day horizon read against it is not merely imprecise but meaningless —
    /// the same non-mixing rule [`DomainInstant::compare`] enforces, applied to
    /// the one decision here that is irreversible.
    NotWallClock(ClockDomain),
    /// A survey described a log that cannot exist — see
    /// [`RetentionSurvey::new`].
    IncoherentSurvey,
}

/// How long each retention class is kept before it may be compacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetentionPolicy {
    raw_ms: u64,
    protected_ms: u64,
}

impl RetentionPolicy {
    /// The horizons OQ2 ruled: 30 days `raw`, 365 days protected.
    #[must_use]
    pub fn oq2() -> Self {
        Self {
            raw_ms: RAW_HORIZON_DAYS * MS_PER_DAY,
            protected_ms: PROTECTED_HORIZON_DAYS * MS_PER_DAY,
        }
    }

    /// Build a policy from horizons in **days**.
    ///
    /// Days rather than milliseconds on purpose: the horizons were ruled in
    /// days, and the one unit conversion happens here, once, instead of at
    /// every call site where a factor of 1000 could go missing.
    ///
    /// # Errors
    ///
    /// * [`RetentionError::HorizonZero`] if either horizon is zero.
    /// * [`RetentionError::ProtectedShorterThanRaw`] if protected data would
    ///   age out before raw data.
    pub fn new(raw_days: u64, protected_days: u64) -> Result<Self, RetentionError> {
        if raw_days == 0 || protected_days == 0 {
            return Err(RetentionError::HorizonZero);
        }
        if protected_days < raw_days {
            return Err(RetentionError::ProtectedShorterThanRaw {
                raw_days,
                protected_days,
            });
        }
        Ok(Self {
            raw_ms: raw_days.saturating_mul(MS_PER_DAY),
            protected_ms: protected_days.saturating_mul(MS_PER_DAY),
        })
    }

    /// The instant before which `raw` events are eligible for compaction.
    ///
    /// Saturating: on a clock reading less than one horizon since the epoch the
    /// cutoff is 0, which makes **nothing** eligible. That is the fail-closed
    /// direction — an underflow that wrapped would make *everything* eligible,
    /// and this operation is not reversible.
    ///
    /// # Errors
    ///
    /// [`RetentionError::NotWallClock`] if `now` is not on the system clock.
    pub fn raw_cutoff_ms(&self, now: DomainInstant) -> Result<u64, RetentionError> {
        Self::require_wall_clock(now)?;
        Ok(now.ms.saturating_sub(self.raw_ms))
    }

    /// The instant before which protected events are eligible.
    ///
    /// Nothing in this crate acts on it yet — the store refuses protected
    /// windows outright, so this horizon is currently a *statement of the
    /// policy*, not a trigger. It is here because leaving it out would make the
    /// invariant in [`Self::new`] unverifiable from the outside.
    ///
    /// # Errors
    ///
    /// [`RetentionError::NotWallClock`] if `now` is not on the system clock.
    pub fn protected_cutoff_ms(&self, now: DomainInstant) -> Result<u64, RetentionError> {
        Self::require_wall_clock(now)?;
        Ok(now.ms.saturating_sub(self.protected_ms))
    }

    fn require_wall_clock(now: DomainInstant) -> Result<(), RetentionError> {
        match now.domain {
            ClockDomain::System => Ok(()),
            other => Err(RetentionError::NotWallClock(other)),
        }
    }
}

/// Which of the store's two refusals stopped a compactable prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blocker {
    /// A protected-class event. Refusing the window whole is the requirement.
    ProtectedClass,
    /// A projection head — the event defining a `(subject, predicate)`'s
    /// current claim, which a rebuild must still be able to reproduce.
    ProjectionHead,
}

impl Blocker {
    /// Whether §11.3's **pre-agreed** escalation — compacting around the
    /// blocker instead of refusing the window whole — applies to this one.
    ///
    /// True only for [`Blocker::ProjectionHead`]. The store's analysis agreed
    /// that escalation in advance *for heads*, and explicitly did not for
    /// protected classes, where refusing whole is what makes survival a
    /// question about policy rather than about interleaving.
    ///
    /// Encoded rather than documented so that a driver implementing the
    /// escalation cannot reach for it on the refusal it was never agreed for.
    #[must_use]
    pub fn may_compact_around(self) -> bool {
        matches!(self, Self::ProjectionHead)
    }
}

/// What the store observed about its own log during one retention pass.
///
/// Every field is something the store can answer cheaply; nothing here is
/// derived. Keeping the observation and the decision apart is what lets the
/// decision be tested without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetentionSurvey {
    oldest_generation: u64,
    newest_past_cutoff: Option<u64>,
    compactable_through: Option<u64>,
    blocked_by: Option<Blocker>,
}

impl RetentionSurvey {
    /// Record a survey.
    ///
    /// * `oldest_generation` — the oldest generation still present.
    /// * `newest_past_cutoff` — the newest generation whose event is older than
    ///   the cutoff, or `None` if none are. This is the *age* question.
    /// * `compactable_through` — `largest_compactable_prefix` over that range,
    ///   or `None` if even the first generation is refused.
    /// * `blocked_by` — which refusal stopped it, if one did.
    ///
    /// # Errors
    ///
    /// [`RetentionError::IncoherentSurvey`] for a log that cannot exist:
    /// eligibility or a compactable prefix below the oldest surviving
    /// generation, a compactable prefix beyond what age made eligible, a
    /// blocker with nothing eligible to block, or a prefix that reached the end
    /// of the eligible range while still claiming a blocker. Refused rather
    /// than normalized — a survey this wrong means the caller's queries
    /// disagree with each other, and compacting on it would act on a log
    /// nobody actually observed.
    pub fn new(
        oldest_generation: u64,
        newest_past_cutoff: Option<u64>,
        compactable_through: Option<u64>,
        blocked_by: Option<Blocker>,
    ) -> Result<Self, RetentionError> {
        if let Some(n) = newest_past_cutoff {
            if n < oldest_generation {
                return Err(RetentionError::IncoherentSurvey);
            }
        }
        match (newest_past_cutoff, compactable_through) {
            // A prefix of nothing.
            (None, Some(_)) => return Err(RetentionError::IncoherentSurvey),
            (Some(n), Some(c)) => {
                if c < oldest_generation || c > n {
                    return Err(RetentionError::IncoherentSurvey);
                }
                // Reached the end of the eligible range, so nothing blocked it.
                if c == n && blocked_by.is_some() {
                    return Err(RetentionError::IncoherentSurvey);
                }
            }
            _ => {}
        }
        // A blocker with nothing eligible is a refusal of a window that was
        // never a candidate.
        if newest_past_cutoff.is_none() && blocked_by.is_some() {
            return Err(RetentionError::IncoherentSurvey);
        }
        Ok(Self {
            oldest_generation,
            newest_past_cutoff,
            compactable_through,
            blocked_by,
        })
    }

    /// The oldest generation still present.
    #[must_use]
    pub fn oldest_generation(&self) -> u64 {
        self.oldest_generation
    }

    /// The newest generation older than the cutoff, if any.
    #[must_use]
    pub fn newest_past_cutoff(&self) -> Option<u64> {
        self.newest_past_cutoff
    }

    /// How far a compactable prefix reached, if at all.
    #[must_use]
    pub fn compactable_through(&self) -> Option<u64> {
        self.compactable_through
    }

    /// Which refusal stopped the prefix, if one did.
    #[must_use]
    pub fn blocked_by(&self) -> Option<Blocker> {
        self.blocked_by
    }
}

/// What a retention pass should do — **four outcomes, never three**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionDecision {
    /// Nothing has aged past the horizon. The ordinary state of a young store,
    /// and the only one of the three no-op outcomes that is uneventful.
    NothingOldEnough,
    /// Events aged out, but the very first one is refused, so no prefix exists.
    ///
    /// Carries its blocker because the two mean different things operationally:
    /// [`Blocker::ProtectedClass`] here means retention is pinned and the store
    /// will keep growing until that event itself ages out, which is the
    /// condition worth alerting on.
    FullyBlocked {
        /// Which refusal.
        blocker: Blocker,
        /// The generation that was refused — the one to look at first.
        at_generation: u64,
    },
    /// A prefix is compactable, and something refused the rest.
    ///
    /// Not a failure: this is the designed shape, since
    /// `largest_compactable_prefix` exists precisely so a refusal is a redirect
    /// rather than a dead end. The blocker rides along so the driver can tell
    /// a partial pass that is progressing from one that is stuck.
    CompactPartial {
        /// First generation to compact.
        lo: u64,
        /// Last generation to compact.
        hi: u64,
        /// What stopped it going further.
        blocker: Blocker,
    },
    /// Everything eligible by age is compactable.
    Compact {
        /// First generation to compact.
        lo: u64,
        /// Last generation to compact.
        hi: u64,
    },
}

impl RetentionDecision {
    /// The range to hand to `compact_range`, if there is one.
    ///
    /// A convenience for a driver that only wants to act; it deliberately does
    /// **not** replace matching on the variant, because the two no-op cases it
    /// flattens are the ones worth reporting differently.
    #[must_use]
    pub fn range(self) -> Option<(u64, u64)> {
        match self {
            Self::Compact { lo, hi } | Self::CompactPartial { lo, hi, .. } => Some((lo, hi)),
            Self::NothingOldEnough | Self::FullyBlocked { .. } => None,
        }
    }

    /// Whether retention made no progress **and** something is pinning it.
    ///
    /// The signal a driver should surface. Distinct from "did nothing", which
    /// on a store younger than the horizon is correct and expected.
    #[must_use]
    pub fn is_pinned(self) -> bool {
        matches!(self, Self::FullyBlocked { .. })
    }
}

/// Decide what one retention pass should compact.
///
/// Pure: every input is observed, `now` is injected, and the same survey always
/// yields the same decision — the property that lets a retention run be
/// replayed during incident reconstruction rather than reasoned about.
///
/// # Errors
///
/// [`RetentionError::IncoherentSurvey`] if the survey reports a blocker at a
/// generation outside the eligible range. Every other coherence rule is already
/// enforced by [`RetentionSurvey::new`].
pub fn decide(survey: &RetentionSurvey) -> Result<RetentionDecision, RetentionError> {
    let Some(newest) = survey.newest_past_cutoff else {
        return Ok(RetentionDecision::NothingOldEnough);
    };
    let lo = survey.oldest_generation;

    match (survey.compactable_through, survey.blocked_by) {
        (None, Some(blocker)) => Ok(RetentionDecision::FullyBlocked {
            blocker,
            at_generation: lo,
        }),
        // No prefix and no blocker cannot happen: something refused generation
        // `lo`, or the prefix would have reached at least that far.
        (None, None) => Err(RetentionError::IncoherentSurvey),
        (Some(hi), Some(blocker)) => Ok(RetentionDecision::CompactPartial { lo, hi, blocker }),
        (Some(hi), None) => {
            debug_assert_eq!(hi, newest, "survey construction guarantees this");
            Ok(RetentionDecision::Compact { lo, hi })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(ms: u64) -> DomainInstant {
        DomainInstant {
            ms,
            domain: ClockDomain::System,
        }
    }

    // -- horizons ----------------------------------------------------------

    #[test]
    fn the_oq2_horizons_are_the_ruled_ones() {
        let p = RetentionPolicy::oq2();
        assert_eq!(p, RetentionPolicy::new(30, 365).unwrap());
        // 30 days back from an instant exactly 100 days after the epoch.
        let hundred_days = 100 * MS_PER_DAY;
        assert_eq!(p.raw_cutoff_ms(now(hundred_days)).unwrap(), 70 * MS_PER_DAY);
    }

    #[test]
    fn protected_data_can_never_age_out_before_raw_data() {
        // The inversion that would defeat the entire point of the classes.
        assert_eq!(
            RetentionPolicy::new(30, 29),
            Err(RetentionError::ProtectedShorterThanRaw {
                raw_days: 30,
                protected_days: 29,
            })
        );
        // Equal is admitted — degenerate but not inverted.
        assert!(RetentionPolicy::new(30, 30).is_ok());
    }

    #[test]
    fn a_zero_horizon_is_refused_rather_than_honoured() {
        assert_eq!(
            RetentionPolicy::new(0, 365),
            Err(RetentionError::HorizonZero)
        );
        assert_eq!(
            RetentionPolicy::new(30, 0),
            Err(RetentionError::HorizonZero)
        );
    }

    #[test]
    fn a_cutoff_saturates_toward_deleting_nothing() {
        // A clock reading less than one horizon since the epoch. Wrapping here
        // would make EVERY event eligible, and compaction is irreversible.
        let p = RetentionPolicy::oq2();
        assert_eq!(p.raw_cutoff_ms(now(0)).unwrap(), 0);
        assert_eq!(p.raw_cutoff_ms(now(MS_PER_DAY)).unwrap(), 0);
        // A cutoff of 0 leaves nothing older than it: no event has a negative
        // timestamp, so nothing is eligible. Fail-closed, not fail-open.
        assert_eq!(p.protected_cutoff_ms(now(MS_PER_DAY)).unwrap(), 0);
    }

    #[test]
    fn retention_cannot_be_decided_on_the_safety_clock() {
        // A 30-day horizon read against the boundary timing domain is
        // meaningless, and this is the one decision that cannot be undone.
        let p = RetentionPolicy::oq2();
        let boundary = DomainInstant {
            ms: 100 * MS_PER_DAY,
            domain: ClockDomain::Boundary,
        };
        assert_eq!(
            p.raw_cutoff_ms(boundary),
            Err(RetentionError::NotWallClock(ClockDomain::Boundary))
        );
        assert_eq!(
            p.protected_cutoff_ms(boundary),
            Err(RetentionError::NotWallClock(ClockDomain::Boundary))
        );
    }

    // -- the two refusals are not alike ------------------------------------

    #[test]
    fn only_the_head_refusal_carries_a_pre_agreed_escalation() {
        // §11.3 agreed compacting AROUND heads in advance, and explicitly did
        // not agree it for protected classes, where refusing whole is what
        // keeps survival a question about policy rather than arrival order.
        assert!(Blocker::ProjectionHead.may_compact_around());
        assert!(!Blocker::ProtectedClass.may_compact_around());
    }

    // -- the four outcomes -------------------------------------------------

    #[test]
    fn nothing_old_enough_is_distinct_from_blocked() {
        // THE reason this is not Option<Range>. Both do nothing; only one is
        // worth waking someone for.
        let young = RetentionSurvey::new(1, None, None, None).unwrap();
        let pinned =
            RetentionSurvey::new(1, Some(500), None, Some(Blocker::ProtectedClass)).unwrap();

        let a = decide(&young).unwrap();
        let b = decide(&pinned).unwrap();

        assert_eq!(a, RetentionDecision::NothingOldEnough);
        assert_eq!(
            b,
            RetentionDecision::FullyBlocked {
                blocker: Blocker::ProtectedClass,
                at_generation: 1,
            }
        );
        // Both compact nothing...
        assert_eq!(a.range(), None);
        assert_eq!(b.range(), None);
        // ...and they are still tellable apart, which is the whole point.
        assert!(!a.is_pinned());
        assert!(b.is_pinned());
        assert_ne!(a, b);
    }

    #[test]
    fn a_clear_range_compacts_whole() {
        let s = RetentionSurvey::new(1, Some(500), Some(500), None).unwrap();
        assert_eq!(
            decide(&s).unwrap(),
            RetentionDecision::Compact { lo: 1, hi: 500 }
        );
        assert_eq!(decide(&s).unwrap().range(), Some((1, 500)));
    }

    #[test]
    fn a_partial_prefix_is_progress_and_says_what_stopped_it() {
        // The designed shape: largest_compactable_prefix exists so a refusal is
        // a redirect, not a dead end.
        let s =
            RetentionSurvey::new(1, Some(500), Some(310), Some(Blocker::ProjectionHead)).unwrap();
        let d = decide(&s).unwrap();
        assert_eq!(
            d,
            RetentionDecision::CompactPartial {
                lo: 1,
                hi: 310,
                blocker: Blocker::ProjectionHead,
            }
        );
        assert_eq!(d.range(), Some((1, 310)));
        // Progress, so not pinned — the distinction a driver alerts on.
        assert!(!d.is_pinned());
    }

    #[test]
    fn the_decision_is_a_pure_function_of_the_survey() {
        // Replayability: the property that lets a retention run be reconstructed
        // during an incident rather than argued about.
        let s =
            RetentionSurvey::new(7, Some(900), Some(412), Some(Blocker::ProtectedClass)).unwrap();
        let first = decide(&s).unwrap();
        for _ in 0..5 {
            assert_eq!(decide(&s).unwrap(), first);
        }
    }

    // -- surveys that describe an impossible log ---------------------------

    #[test]
    fn a_survey_that_contradicts_itself_is_refused() {
        // Eligibility below the oldest surviving generation.
        assert_eq!(
            RetentionSurvey::new(10, Some(5), None, None),
            Err(RetentionError::IncoherentSurvey)
        );
        // A compactable prefix past what age made eligible.
        assert_eq!(
            RetentionSurvey::new(1, Some(100), Some(101), None),
            Err(RetentionError::IncoherentSurvey)
        );
        // A prefix of nothing.
        assert_eq!(
            RetentionSurvey::new(1, None, Some(50), None),
            Err(RetentionError::IncoherentSurvey)
        );
        // A blocker with nothing eligible to block.
        assert_eq!(
            RetentionSurvey::new(1, None, None, Some(Blocker::ProtectedClass)),
            Err(RetentionError::IncoherentSurvey)
        );
        // Reached the end of the eligible range while still claiming a blocker.
        assert_eq!(
            RetentionSurvey::new(1, Some(500), Some(500), Some(Blocker::ProjectionHead)),
            Err(RetentionError::IncoherentSurvey)
        );
    }

    #[test]
    fn a_prefix_below_the_oldest_generation_is_refused() {
        // Compacting generations that are already gone.
        assert_eq!(
            RetentionSurvey::new(10, Some(100), Some(9), None),
            Err(RetentionError::IncoherentSurvey)
        );
        assert!(
            RetentionSurvey::new(10, Some(100), Some(10), Some(Blocker::ProjectionHead)).is_ok()
        );
    }
}
