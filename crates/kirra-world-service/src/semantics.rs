//! **The answer boundary's own versioned rule, and the composition — box 3b.**
//!
//! `kirra_world_store::semantics` versions the *reducers*. An answer is not
//! produced by a reducer alone: the boundary applies its own rule on the way
//! out, deciding which folded claims are servable at all. That rule can change
//! what an answer says without any reducer moving, so it is versioned here on
//! the same terms — declared, corpus-pinned, source-pinned.
//!
//! # Why a query carries a SET of versions rather than one number
//!
//! A single composite version would have to move whenever *any* rule moved,
//! including rules the query never consults. That is not merely inelegant; it
//! makes every recorded reference refuse for reasons unrelated to it, and a
//! refusal that fires constantly is one people learn to route around.
//!
//! So [`SemanticVersions::for_query`] names exactly the rules a query family
//! depends on. For [`QueryKind::CurrentSubject`] that is three of the four rules
//! in the system, and the membership test is always the same question — *can
//! this rule change what the answer says?*
//!
//! | Rule | In? | Why |
//! |---|---|---|
//! | `world_current_fold` | yes | the pinned replay folds claims with it |
//! | `entity_fold` | yes **since 3h** | the pinned replay folds the identity graph the answer resolves objects against |
//! | `answer_admissibility` | yes | it decides which folded claims are served |
//! | `subject_summary_fold` | **no** | a different query family entirely |
//!
//! # `entity_fold` entered this set because the composition changed
//!
//! Box 3b shipped with `entity_fold` **excluded**, and the exclusion was
//! correct at the time: a resolved ref reported
//! `ObjectIdentity::NotResolvedInReplay`, so the identity fold could not reach
//! the answer and including it would have refused references for a rule they
//! never consulted.
//!
//! Box 3h made refs compose identity at the pinned generation. That is a change
//! in what an answer is *derived from*, so the rule joined the set — and the
//! test that pinned the exclusion failed, which is how it was supposed to
//! happen. A dependency set edited by hand to match the code is a dependency set
//! that will one day not match it; this one moved because a red test said the
//! old claim was no longer true.
//!
//! [`QueryKind::CurrentSubject`]: crate::answer_ref::QueryKind::CurrentSubject

use kirra_world_store::projection::ProjectedClaim;
use kirra_world_store::semantics::{self as store_semantics, RuleId};

use crate::answer_ref::QueryKind;
use crate::read_view::is_admissible_for_ref;

/// A versioned rule owned by the answer boundary.
///
/// Declared in the same shape as [`kirra_world_store::semantics::RuleId`] —
/// enum, stable `as_str`, one row in a `…RuleSpec` table — so
/// `ci/check_world_semantics.py` reads both crates' declarations with one
/// parser and holds them to one contract. A boundary rule checked by a
/// second, differently-shaped gate is a boundary rule checked more weakly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BoundaryRuleId {
    /// [`crate::read_view::is_admissible_for_ref`] — which folded claims are
    /// servable at all.
    Admissibility,
}

impl BoundaryRuleId {
    /// The stable name used in declarations, baselines and recorded references.
    ///
    /// `const` so [`ADMISSIBILITY`] can be defined from it rather than beside
    /// it — two spellings of one rule name is exactly how a baseline key drifts
    /// from the thing it keys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admissibility => "answer_admissibility",
        }
    }

    /// Every versioned rule the boundary owns.
    #[must_use]
    pub fn all() -> &'static [BoundaryRuleId] {
        &[BoundaryRuleId::Admissibility]
    }
}

/// The boundary's stable rule name, as it appears in a recorded reference.
pub const ADMISSIBILITY: &str = BoundaryRuleId::Admissibility.as_str();

/// One boundary rule's declaration. Mirrors
/// [`kirra_world_store::semantics::RuleSpec`] field for field.
#[derive(Debug, Clone, Copy)]
pub struct BoundaryRuleSpec {
    /// Which rule.
    pub rule: BoundaryRuleId,
    /// The declared semantic version.
    pub version: u32,
    /// SHA-256 of this rule's corpus rendering, at this version.
    pub corpus_digest: &'static str,
    /// SHA-256 of the comment-stripped source span named by `span`.
    pub source_pin: &'static str,
    /// The file holding the rule.
    pub source_file: &'static str,
    /// The marker name delimiting the rule's span in that file.
    pub span: &'static str,
}

/// **The declared semantics of every rule the answer boundary owns.**
pub const BOUNDARY_SEMANTICS: &[BoundaryRuleSpec] = &[BoundaryRuleSpec {
    rule: BoundaryRuleId::Admissibility,
    version: 1,
    corpus_digest: "c6458e88ed93ce2498a8b02f97e04bdfe3f2c666a1146ed34c081f4e56bcc33f",
    source_pin: "93d1c69a10f75e8df9e39c9d3cee1a89733b76b8fac7f0b0a7203b25370e04ae",
    source_file: "crates/kirra-world-service/src/read_view.rs",
    span: "answer_admissibility",
}];

/// The declared version of one boundary rule.
///
/// # Panics
///
/// If [`BOUNDARY_SEMANTICS`] has no row for `rule` — an undeclared rule is an
/// unversioned one, which is the state box 3b exists to make unrepresentable.
#[must_use]
pub fn version_of(rule: BoundaryRuleId) -> u32 {
    BOUNDARY_SEMANTICS
        .iter()
        .find(|s| s.rule == rule)
        .unwrap_or_else(|| panic!("boundary rule {} has no declaration", rule.as_str()))
        .version
}

/// One rule's version, as recorded on a reference.
///
/// A `String` name rather than a borrowed one so a reference decoded from
/// storage — carrying a rule this build may no longer declare — is
/// representable. A version set that could only hold names this build knows
/// could not describe the reference it is meant to refuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleVersion {
    /// The rule's stable name.
    pub rule: String,
    /// The version it was recorded at.
    pub version: u32,
}

/// **The semantics one answer was produced under.**
///
/// Sorted and deduplicated at construction, so equality and hashing are
/// properties of the *content* rather than of the order a caller happened to
/// supply — which is what makes "the same query at the same coordinate produces
/// the same reference" true of the type rather than of a convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SemanticVersions {
    entries: Vec<RuleVersion>,
}

impl SemanticVersions {
    /// Build from an arbitrary set — for tests, and for decoding a stored
    /// reference recorded by another build.
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = RuleVersion>) -> Self {
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort();
        entries.dedup_by(|a, b| a.rule == b.rule);
        Self { entries }
    }

    /// **The versions this build implements for a query family.**
    ///
    /// Read from the live declarations rather than restated, so a bumped
    /// reducer version reaches recorded references without anyone remembering
    /// to update a second table.
    #[must_use]
    pub fn for_query(kind: QueryKind) -> Self {
        match kind {
            // Both families depend on the SAME three rules, and that is a
            // finding rather than a coincidence: they differ in which
            // COORDINATE they cut on, not in which rules produce the answer.
            // Still derived per-family, because "identical today" is not
            // "identical by construction" — a temporal-resolution rule would
            // belong to one and not the other, and a shared arm that had to be
            // split later is a shared arm nobody remembers to split.
            QueryKind::CurrentSubject | QueryKind::AsOfSubject => Self::new([
                RuleVersion {
                    rule: RuleId::WorldCurrentFold.as_str().to_string(),
                    version: store_semantics::version_of(RuleId::WorldCurrentFold),
                },
                // Box 3h added this for `CurrentSubject`: a resolved ref
                // composes the identity graph as it stood at the pinned
                // generation, so the fold that BUILDS that graph can change
                // what an answer says — the whole test for membership in this
                // set. `ask_as_of` joined when its composition landed on the
                // transaction-time axis.
                RuleVersion {
                    rule: RuleId::EntityFold.as_str().to_string(),
                    version: store_semantics::version_of(RuleId::EntityFold),
                },
                RuleVersion {
                    rule: ADMISSIBILITY.to_string(),
                    version: version_of(BoundaryRuleId::Admissibility),
                },
            ]),
        }
    }

    /// The recorded rules, sorted by name.
    #[must_use]
    pub fn entries(&self) -> &[RuleVersion] {
        &self.entries
    }

    /// This rule's version, if the set names it.
    #[must_use]
    pub fn version_of(&self, rule: &str) -> Option<u32> {
        self.entries
            .iter()
            .find(|e| e.rule == rule)
            .map(|e| e.version)
    }

    /// **Every way `self` (as recorded) differs from `current`.**
    ///
    /// Returns differences rather than a boolean because a refusal that cannot
    /// name the rule that moved is a refusal an operator cannot act on. A rule
    /// present on one side only is a difference too — `None` on the missing
    /// side — since a query family gaining or losing a dependency changes what
    /// the answer is derived from just as surely as a version bump does.
    #[must_use]
    pub fn differences(&self, current: &Self) -> Vec<VersionDifference> {
        let mut names: Vec<&str> = self
            .entries
            .iter()
            .chain(current.entries.iter())
            .map(|e| e.rule.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();

        names
            .into_iter()
            .filter_map(|rule| {
                let recorded = self.version_of(rule);
                let now = current.version_of(rule);
                (recorded != now).then(|| VersionDifference {
                    rule: rule.to_string(),
                    recorded,
                    current: now,
                })
            })
            .collect()
    }
}

/// One rule whose version moved between a recorded reference and this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDifference {
    /// The rule's stable name.
    pub rule: String,
    /// The version the reference was recorded under, or `None` if the reference
    /// did not depend on this rule.
    pub recorded: Option<u32>,
    /// The version this build implements, or `None` if it no longer declares
    /// the rule.
    pub current: Option<u32>,
}

// ---------------------------------------------------------------------------
// The admissibility corpus
// ---------------------------------------------------------------------------

/// The admissibility rule's corpus input: `(label, claim, clock, budget)`.
///
/// One bullet per row, in order, because a list that does not match the rows is
/// a list a reader cannot check coverage against — and checking coverage is the
/// only reason to write it down:
///
/// | Row | Verdict | The branch it holds open |
/// |---|---|---|
/// | `unbounded_unlabelled` | served | the base case, and the **unlabelled** one: a claim with no axes has no trust to grade, and refusing it would invent a judgement from the absence of one |
/// | `expired` | refused | refused on **time** |
/// | `stale` | **served** | past the budget, not past `valid_to` |
/// | `rejected_adjudication` | refused | refused on **trust**, not on time |
/// | `ambiguous_adjudication` | refused | the other half of `trust_grade`'s `Rejected \| Ambiguous` or-pattern — with only one row present, narrowing that pattern to a single variant would not move the digest |
/// | `confirmed_adjudication` | served | **labelled** and served, which is what distinguishes "served because unlabelled" from "served despite being graded" |
///
/// The `stale` row is the one a careless corpus omits, and its absence is the
/// expensive kind. That the boundary reports staleness rather than swallowing
/// it is a documented property of this rule, so a corpus that could not tell
/// "stale" from "expired" would let the two be merged without moving the digest
/// — see `the_boundary_corpus_catches_refusing_a_stale_claim`.
///
/// The pairing of `unbounded_unlabelled` with `confirmed_adjudication` is what
/// gives `the_boundary_corpus_catches_refusing_an_unlabelled_claim` its teeth:
/// a rule that served only labelled claims still serves the second row, so the
/// two rows together locate the change rather than merely detecting one.
#[must_use]
pub fn admissibility_corpus() -> Vec<(&'static str, ProjectedClaim, u64, Option<u64>)> {
    use kirra_world_store::{Adjudication, Corroboration, Origin, TrustAxes};

    let claim =
        |valid_from_ms: i64, valid_to_ms: Option<i64>, trust: Option<TrustAxes>| ProjectedClaim {
            subject: "package_17".to_string(),
            predicate: Some("last_seen_at".to_string()),
            object: Some("dock_alpha".to_string()),
            kind: "mission".to_string(),
            payload: "{}".to_string(),
            frame_id: None,
            map_id: None,
            source: "sensor".to_string(),
            valid_from_ms,
            valid_to_ms,
            txn_time_ms: valid_from_ms,
            generation: 1,
            event_id: "ev-1".to_string(),
            chain_digest: "chain-1".to_string(),
            trust,
        };
    let axes = |adjudication| {
        TrustAxes::new(
            Origin::Observed,
            Corroboration::Uncorroborated,
            adjudication,
        )
        .expect("corpus axes are storable")
    };

    vec![
        (
            "unbounded_unlabelled",
            claim(1_000, None, None),
            2_000,
            None,
        ),
        ("expired", claim(1_000, Some(1_500), None), 2_000, None),
        // Past the budget but not past `valid_to`: stale, and servable.
        ("stale", claim(1_000, None, None), 2_000, Some(100)),
        (
            "rejected_adjudication",
            claim(1_000, None, Some(axes(Adjudication::Rejected))),
            2_000,
            None,
        ),
        (
            "ambiguous_adjudication",
            claim(1_000, None, Some(axes(Adjudication::Ambiguous))),
            2_000,
            None,
        ),
        (
            "confirmed_adjudication",
            claim(1_000, None, Some(axes(Adjudication::Confirmed))),
            2_000,
            None,
        ),
    ]
}

/// Render the admissibility rule's verdict over its corpus.
#[must_use]
pub fn admissibility_rendering() -> String {
    let mut out = String::new();
    for (label, claim, clock, budget) in admissibility_corpus() {
        out.push_str(label);
        out.push('\u{1f}');
        out.push_str(if is_admissible_for_ref(&claim, clock, budget) {
            "served"
        } else {
            "refused"
        });
        out.push('\u{1e}');
    }
    out
}

/// The digest of [`admissibility_rendering`], in the form
/// [`BoundaryRuleSpec::corpus_digest`] declares.
#[must_use]
pub fn admissibility_corpus_digest() -> String {
    store_semantics::digest(&admissibility_rendering())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_set_is_order_independent() {
        let a = SemanticVersions::new([
            RuleVersion {
                rule: "b".into(),
                version: 2,
            },
            RuleVersion {
                rule: "a".into(),
                version: 1,
            },
        ]);
        let b = SemanticVersions::new([
            RuleVersion {
                rule: "a".into(),
                version: 1,
            },
            RuleVersion {
                rule: "b".into(),
                version: 2,
            },
        ]);
        assert_eq!(a, b);
        assert!(a.differences(&b).is_empty());
    }

    #[test]
    fn a_moved_version_is_reported_by_name() {
        let recorded = SemanticVersions::new([RuleVersion {
            rule: "a".into(),
            version: 1,
        }]);
        let current = SemanticVersions::new([RuleVersion {
            rule: "a".into(),
            version: 2,
        }]);
        assert_eq!(
            recorded.differences(&current),
            vec![VersionDifference {
                rule: "a".into(),
                recorded: Some(1),
                current: Some(2),
            }]
        );
    }

    /// A dependency appearing or disappearing changes what an answer is derived
    /// from, so it must be a difference rather than silently tolerated.
    #[test]
    fn an_added_or_removed_rule_is_a_difference() {
        let none = SemanticVersions::new([]);
        let one = SemanticVersions::new([RuleVersion {
            rule: "a".into(),
            version: 1,
        }]);
        assert_eq!(
            none.differences(&one),
            vec![VersionDifference {
                rule: "a".into(),
                recorded: None,
                current: Some(1),
            }]
        );
        assert_eq!(
            one.differences(&none),
            vec![VersionDifference {
                rule: "a".into(),
                recorded: Some(1),
                current: None,
            }]
        );
    }

    /// The membership of this set is a claim about today's code, so it is
    /// asserted rather than only written down.
    ///
    /// # This test previously asserted TWO rules, and box 3h broke it
    ///
    /// That is the intended lifecycle, and worth recording because a dependency
    /// set is exactly the kind of thing that rots quietly. Under 3b a pinned ref
    /// reported `ObjectIdentity::NotResolvedInReplay`, so `entity_fold` could
    /// not reach the answer and its exclusion was correct. 3h made refs compose
    /// the identity graph at the pinned generation — a change in what the answer
    /// is *derived from* — and this assertion went red, naming the stale claim.
    ///
    /// The set was then widened because a test said the old claim was false, not
    /// because someone edited a list to match the code. A dependency set
    /// maintained the other way round is one that will eventually describe a
    /// composition that no longer exists.
    #[test]
    fn the_current_subject_query_depends_on_exactly_three_rules() {
        let v = SemanticVersions::for_query(QueryKind::CurrentSubject);
        let names: Vec<&str> = v.entries().iter().map(|e| e.rule.as_str()).collect();
        assert_eq!(
            names,
            vec![ADMISSIBILITY, "entity_fold", "world_current_fold"]
        );
        assert!(
            v.version_of("entity_fold").is_some(),
            "since 3h a resolved ref looks its objects up in the identity graph \
             the entity fold builds, so that fold can change what the answer says"
        );
        assert!(
            v.version_of("subject_summary_fold").is_none(),
            "subject summaries are a different query family; including them would \
             refuse references for a rule they never consulted"
        );
    }
}
