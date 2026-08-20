//! **Ruled freshness policy — Tier 3 box 3e.**
//!
//! `KIRRA-WM-FRESHNESS-POLICY-001`:
//!
//! > **Freshness semantics are centrally ruled by claim kind. `Timeless` must be
//! > explicitly granted. Bounded facts require an explicit age limit.
//! > Unclassified semantics refuse.**
//!
//! And the invariant that follows from it, which is the whole point:
//!
//! > **`Timeless` is an affirmative semantic classification, never the absence
//! > of a freshness policy.**
//!
//! # The defect this closes, which was live and documented
//!
//! `validity_at` maps `staleness_budget_ms: None` to [`Validity::Timeless`], and
//! `WorldView` accepted `None` from anyone. `WM_SCOPE.md`'s own FINDING 2
//! recorded what that meant in practice:
//!
//! > *"Where was the package last seen" is about as recency-sensitive as this
//! > domain gets, so a year-old observation is currently served with the same
//! > standing as a fresh one, under a label asserting that is fine.*
//!
//! `Timeless` is not "we did not check". It is a **positive claim that the
//! fact's age does not matter**. Serving it by default meant the engine asserted
//! that claim about every fact in the store, including the ones for which it is
//! false, and nobody had to decide anything for that to happen.
//!
//! # Why a ruled table rather than a caller flag
//!
//! Letting the caller declare a query recency-sensitive only moves the trust
//! decision outward: a careless caller says "insensitive" and manufactures
//! `Timeless` exactly as the engine did. The API would be satisfied and the
//! architectural rule defeated. So the disposition is **ruled centrally**, in
//! one reviewable place, keyed by semantics.
//!
//! # Keyed by SEMANTICS, not by storage representation
//!
//! [`SemanticClass`] is `(kind, predicate)`. `last_seen_at` and a floor plan can
//! both be perfectly valid confirmed claims with the same storage shape and
//! fundamentally different temporal meaning — so the key has to be the thing
//! that distinguishes them, which is what they MEAN. `kind` is carried as well
//! as `predicate` because two kinds may legitimately share a predicate name, and
//! a table that collided them would rule one of them by accident.
//!
//! # The state machine, and why there is no `Unknown` freshness
//!
//! ```text
//! policy = Timeless                  -> Validity::Timeless
//! policy = Bounded, age <= bound     -> Validity::Fresh
//! policy = Bounded, age  > bound     -> Validity::Stale
//! no policy                          -> the QUERY refuses
//! ```
//!
//! There is no successful answer for which freshness is unknown. A missing
//! policy is a **policy-resolution failure**, not a freshness state — so it
//! travels in the error channel, and a fourth `Validity::Unknown` variant would
//! be uninhabited. `WM_SCOPE.md` asked this question directly and said to decide
//! it by finding a reachable case or leaving it out; there is none, so it is
//! left out. If one is ever found — a claim whose age genuinely cannot be
//! determined, say from missing time provenance — the variant becomes justified
//! **then**, established by a reachable test rather than reserved speculatively.
//!
//! [`Validity::Timeless`]: kirra_world_store::Validity::Timeless

use crate::read_view::AskError;

/// **What a claim's age MEANS.**
///
/// Two variants and no third, because there are only two honest answers to
/// *"does this fact's age bear on whether it is usable"*. The absence of an
/// answer is not a third variant — it is a refusal, and it lives in
/// [`AskError::UnclassifiedFreshness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreshnessPolicy {
    /// Age is semantically irrelevant to this class.
    ///
    /// An **affirmative grant**. Reaching this variant means somebody ruled that
    /// a year-old instance of this fact is as good as a fresh one.
    Timeless,
    /// Age matters, and this is the limit past which the fact is `Stale`.
    Bounded {
        /// Milliseconds after `valid_from` at which the fact stops being fresh.
        max_age_ms: u64,
    },
}

impl FreshnessPolicy {
    /// The staleness budget this policy supplies to the read-time computation.
    ///
    /// The one place a policy becomes the `Option<u64>` that
    /// `kirra_world::trust::validity_at` consumes. `None` still means "timeless"
    /// *there* — what changed is that it can now only arise from an explicit
    /// [`Self::Timeless`] grant, never from a caller who supplied nothing.
    #[must_use]
    pub fn budget(self) -> Option<u64> {
        match self {
            Self::Timeless => None,
            Self::Bounded { max_age_ms } => Some(max_age_ms),
        }
    }
}

/// The semantic identity of a claim, for freshness purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticClass {
    /// The claim kind.
    pub kind: &'static str,
    /// The predicate, or `None` for a payload-only claim.
    pub predicate: Option<&'static str>,
}

/// **The ruled table.** Every semantic class this build serves, with its
/// disposition and the reason for it.
///
/// # These are rulings, and they are meant to be argued with
///
/// Each row is a decision about what a fact MEANS, not a technical fact about
/// the data, so each carries its reasoning. A row nobody can defend should be
/// removed — and removing it makes that class refuse, which is the safe
/// direction.
///
/// The table is deliberately **small**. A large speculative table would be the
/// decorative-metadata failure in another costume: entries nobody ruled, read as
/// though somebody had. Everything absent refuses, so the table can grow one
/// argued row at a time without any interim being unsafe.
pub const RULED: &[(SemanticClass, FreshnessPolicy)] = &[
    // `WM_SCOPE.md` FINDING 2, verbatim: "'Where was the package last seen' is
    // about as recency-sensitive as this domain gets, so a year-old observation
    // is currently served with the same standing as a fresh one, under a label
    // asserting that is fine." That finding is what this box exists to close, so
    // this row is the one the box was written for.
    //
    // Five minutes: a located package that has not been re-observed in five
    // minutes is a package whose location is a guess. The number is a ruling,
    // not a measurement — which is why it is HERE, in one reviewable place,
    // rather than inside a query.
    (
        SemanticClass {
            kind: "mission",
            predicate: Some("last_seen_at"),
        },
        FreshnessPolicy::Bounded {
            max_age_ms: 5 * 60 * 1_000,
        },
    ),
    // A position is the canonical recency-sensitive fact: it is a statement
    // about where something WAS, and everything that makes it useful decays.
    (
        SemanticClass {
            kind: "observation",
            predicate: Some("position"),
        },
        FreshnessPolicy::Bounded {
            max_age_ms: 30 * 1_000,
        },
    ),
    // An affirmative Timeless grant, and the argument for it: colour is a
    // property OF the object rather than a relationship between the object and
    // a moment. A year-old colour observation is not a stale fact, it is the
    // same fact observed a while ago — so ageing it would refuse valid answers
    // and buy no safety.
    (
        SemanticClass {
            kind: "observation",
            predicate: Some("colour"),
        },
        FreshnessPolicy::Timeless,
    ),
    // `KIRRA-WM-IDENTITY-FRESHNESS-001` (2026-08-20). An **adjudicated identity
    // decision** does not age.
    //
    // The distinction this ruling turns on, stated because it is the whole
    // argument and it is easy to collapse:
    //
    //   candidate evidence   — "I currently think A and B may be the same"
    //                          -> freshness may matter
    //   adjudicated decision — "an authorized adjudicator DECIDED A and B are
    //                          the same identity"
    //                          -> does not age out merely because time passed
    //
    // A matcher's proposal is a present-tense belief resting on observations
    // that may themselves be bounded. A decision recorded by an authorized
    // adjudicator is a different kind of fact: it remains valid until CHANGED by
    // later adjudication -- `SplitEntity`, `ForgetEntity`, or superseding
    // identity evidence -- not until a clock runs out. Ageing it would make
    // identity silently dissolve with nobody deciding anything, which is exactly
    // what §6.3's "a merged id stays resolvable forever" forbids.
    //
    // Note this rules the DECISION row (`same_as_adjudication`), not the
    // candidate. `(observation, "same_as")` -- the matcher's proposal -- stays
    // in `ci/freshness_unruled_baseline.json`, deliberately: whether a PROPOSAL
    // goes stale is a separate question this ruling does not answer.
    (
        SemanticClass {
            kind: "same_as_adjudication",
            predicate: Some("same_as_adjudged"),
        },
        FreshnessPolicy::Timeless,
    ),
];

/// The ruled disposition for a semantic class, if one exists.
///
/// `None` is not a default — it is the input to a refusal. See
/// [`resolve_policy`].
#[must_use]
pub fn ruled_policy(kind: &str, predicate: Option<&str>) -> Option<FreshnessPolicy> {
    RULED
        .iter()
        .find(|(class, _)| class.kind == kind && class.predicate == predicate)
        .map(|(_, policy)| *policy)
}

/// **Where a view's freshness dispositions come from.**
///
/// There is no variant meaning *"nothing supplied"*, which is the point: the
/// type makes the old `None` unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessSource {
    /// The ruled table decides, per claim, by semantic class. Unclassified
    /// semantics refuse.
    Ruled,
    /// **The caller classifies every claim in this view**, taking
    /// responsibility for the judgement the table would otherwise make.
    ///
    /// Retained deliberately rather than removed. `mission_context` already
    /// forces its caller to decide and documents `None` as *"I have considered
    /// this and this fact is genuinely timeless"* — an affirmative act, which is
    /// exactly what this variant carries. It is strictly safer than the global
    /// default it replaces, because the classification is now a value someone
    /// wrote rather than a branch nobody took.
    ///
    /// It is also the honest interim while [`RULED`] is small: a consumer whose
    /// semantics have not been ruled yet can still ask, provided it says what it
    /// believes and is greppable for having said it.
    Caller(FreshnessPolicy),
}

/// **Resolve the policy for one claim, or refuse.**
///
/// # Errors
///
/// [`AskError::UnclassifiedFreshness`] when the source is [`FreshnessSource::Ruled`]
/// and the class has no row. Fail-closed: not `Timeless`, not an infinite
/// budget, and not a hidden default.
pub fn resolve_policy(
    source: FreshnessSource,
    kind: &str,
    predicate: Option<&str>,
) -> Result<FreshnessPolicy, AskError> {
    match source {
        FreshnessSource::Caller(policy) => Ok(policy),
        FreshnessSource::Ruled => {
            ruled_policy(kind, predicate).ok_or_else(|| AskError::UnclassifiedFreshness {
                kind: kind.to_string(),
                predicate: predicate.map(str::to_string),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeless_supplies_no_budget_and_bounded_supplies_its_limit() {
        assert_eq!(FreshnessPolicy::Timeless.budget(), None);
        assert_eq!(FreshnessPolicy::Bounded { max_age_ms: 7 }.budget(), Some(7));
    }

    /// The load-bearing direction: an unclassified class REFUSES rather than
    /// falling back to the permissive reading.
    #[test]
    fn an_unclassified_class_refuses_under_the_ruled_source() {
        let err = resolve_policy(FreshnessSource::Ruled, "mission", Some("invented"))
            .expect_err("an unruled class must refuse");
        match err {
            AskError::UnclassifiedFreshness { kind, predicate } => {
                assert_eq!(kind, "mission");
                assert_eq!(predicate.as_deref(), Some("invented"));
            }
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    /// A payload-only claim is a distinct class, not a wildcard.
    #[test]
    fn a_predicate_less_claim_is_its_own_class_and_is_unruled_today() {
        assert!(ruled_policy("observation", None).is_none());
        assert!(resolve_policy(FreshnessSource::Ruled, "observation", None).is_err());
    }

    /// Two kinds sharing a predicate name must not collide — the reason `kind`
    /// is in the key at all.
    #[test]
    fn the_key_is_kind_and_predicate_together() {
        assert!(ruled_policy("observation", Some("colour")).is_some());
        assert!(
            ruled_policy("mission", Some("colour")).is_none(),
            "a ruling for one kind must not silently rule another"
        );
    }

    /// The caller source never consults the table — that is what makes it an
    /// override rather than a fallback.
    #[test]
    fn the_caller_source_answers_for_classes_the_table_does_not_hold() {
        assert_eq!(
            resolve_policy(
                FreshnessSource::Caller(FreshnessPolicy::Timeless),
                "anything",
                Some("unruled"),
            )
            .expect("the caller classified it"),
            FreshnessPolicy::Timeless,
        );
    }

    /// Every ruled row must be reachable by its own key, and no class may be
    /// ruled twice — a duplicate would make the table's meaning depend on order.
    #[test]
    fn the_table_is_well_formed() {
        for (class, expected) in RULED {
            assert_eq!(
                ruled_policy(class.kind, class.predicate),
                Some(*expected),
                "{class:?} is not reachable by its own key"
            );
        }
        let mut seen: Vec<&SemanticClass> = Vec::new();
        for (class, _) in RULED {
            assert!(
                !seen.contains(&class),
                "{class:?} is ruled twice; the table's meaning would depend on order"
            );
            seen.push(class);
        }
    }

    /// A `Bounded` row with a zero limit would make every instance stale the
    /// instant it became valid — almost certainly a typo rather than a ruling.
    #[test]
    fn no_bounded_row_has_a_zero_limit() {
        for (class, policy) in RULED {
            if let FreshnessPolicy::Bounded { max_age_ms } = policy {
                assert!(*max_age_ms > 0, "{class:?} is bounded at zero");
            }
        }
    }
}
