//! **The answer boundary's declared rule, and the controls behind it — box 3b.**
//!
//! The store's `tests/semantics_corpus.rs` does this for the three reducers.
//! This is the same discipline for the one rule the *boundary* owns: which
//! folded claims are servable at all.
//!
//! The sensitivity controls matter more here than they look. The admissibility
//! rule has exactly two ways to be wrong, and they are opposites:
//!
//! * **too permissive** — serving an expired or `Inadmissible` claim, which is
//!   the safety-relevant direction; and
//! * **too strict** — refusing a *stale* claim, which sounds conservative and is
//!   actually a regression. Staleness is reported, not swallowed: a caller with
//!   a budget gets the claim back carrying `Validity::Stale` and decides. A rule
//!   that silently dropped stale claims would turn a reported condition into an
//!   invisible one, and the answer would go from "here it is, it is old" to
//!   "nothing is known" — which is a different fact.
//!
//! A corpus blind to either direction would let that half of the rule be
//! rewritten under a fixed version, so both are challenged explicitly.

use kirra_world_service::answer_ref::QueryKind;
use kirra_world_service::semantics::{
    admissibility_corpus, admissibility_corpus_digest, admissibility_rendering, BoundaryRuleId,
    SemanticVersions, BOUNDARY_SEMANTICS,
};
use kirra_world_store::projection::ProjectedClaim;
use kirra_world_store::semantics::digest;
use kirra_world_store::{TrustGrade, Validity};

/// The declared digest must be the rule's actual digest.
#[test]
fn the_declared_boundary_corpus_digest_matches_the_rule() {
    for spec in BOUNDARY_SEMANTICS {
        assert_eq!(
            admissibility_corpus_digest(),
            spec.corpus_digest,
            "\n\n{} behaves differently from its declaration (version {}).\n\
             \n\
             If DELIBERATE: bump `version` AND re-pin `corpus_digest` in\n\
             `BOUNDARY_SEMANTICS`, and add the new version's row to\n\
             `ci/world_semantics_baseline.json`.\n\
             \n\
             rendering now:\n{}\n",
            spec.rule.as_str(),
            spec.version,
            admissibility_rendering().replace('\u{1e}', "\n"),
        );
    }
}

#[test]
fn no_boundary_declaration_carries_a_placeholder() {
    assert_eq!(BOUNDARY_SEMANTICS.len(), BoundaryRuleId::all().len());
    for spec in BOUNDARY_SEMANTICS {
        for (field, value) in [
            ("corpus_digest", spec.corpus_digest),
            ("source_pin", spec.source_pin),
        ] {
            assert_eq!(
                value.len(),
                64,
                "{}.{field} is not a sha256",
                spec.rule.as_str()
            );
            assert_ne!(
                value,
                "0".repeat(64),
                "{}.{field} is still the placeholder",
                spec.rule.as_str()
            );
        }
    }
}

/// A corpus in which every row lands the same way discriminates nothing: a rule
/// returning a constant would reproduce it exactly.
#[test]
fn the_boundary_corpus_exercises_both_verdicts() {
    let rendered = admissibility_rendering();
    assert!(rendered.contains("served"), "no row is served");
    assert!(rendered.contains("refused"), "no row is refused");
}

// ---------------------------------------------------------------------------
// Sensitivity controls
// ---------------------------------------------------------------------------

/// Render the corpus under a caller-supplied admissibility rule.
fn render_with(rule: impl Fn(&ProjectedClaim, u64, Option<u64>) -> bool) -> String {
    let mut out = String::new();
    for (label, claim, clock, budget) in admissibility_corpus() {
        out.push_str(label);
        out.push('\u{1f}');
        out.push_str(if rule(&claim, clock, budget) {
            "served"
        } else {
            "refused"
        });
        out.push('\u{1e}');
    }
    out
}

fn assert_variant_is_caught(variant: &str, rendered: &str) {
    assert_ne!(
        rendered,
        admissibility_rendering(),
        "\n\nthe admissibility corpus does NOT discriminate the `{variant}` \
         variant — the declared version would stay green through that change.\n"
    );
}

/// The faithfulness control: the harness must reproduce the shipped rule when
/// given the shipped rule, or a difference it reports is its own bug.
#[test]
fn the_boundary_variant_harness_reproduces_the_real_rule() {
    let real = |c: &ProjectedClaim, clock: u64, budget: Option<u64>| {
        c.validity_at(clock, budget) != Validity::Expired
            && !matches!(c.grade_at(clock, budget), Some(TrustGrade::Inadmissible))
    };
    assert_eq!(render_with(real), admissibility_rendering());
}

/// Dropping the expiry check serves a claim that has stopped holding.
#[test]
fn the_boundary_corpus_catches_serving_an_expired_claim() {
    assert_variant_is_caught(
        "expiry_check_dropped",
        &render_with(|c, clock, budget| {
            !matches!(c.grade_at(clock, budget), Some(TrustGrade::Inadmissible))
        }),
    );
}

/// Dropping the trust check serves a `Rejected` or `Ambiguous` claim.
#[test]
fn the_boundary_corpus_catches_serving_an_inadmissible_claim() {
    assert_variant_is_caught(
        "trust_check_dropped",
        &render_with(|c, clock, budget| c.validity_at(clock, budget) != Validity::Expired),
    );
}

/// **The over-strict direction.** Refusing stale claims looks conservative and
/// silently converts a reported condition into an absent answer.
#[test]
fn the_boundary_corpus_catches_refusing_a_stale_claim() {
    assert_variant_is_caught(
        "stale_refused",
        &render_with(|c, clock, budget| {
            !matches!(
                c.validity_at(clock, budget),
                Validity::Expired | Validity::Stale
            ) && !matches!(c.grade_at(clock, budget), Some(TrustGrade::Inadmissible))
        }),
    );
}

/// Refusing unlabelled claims invents a trust judgement from the absence of one.
#[test]
fn the_boundary_corpus_catches_refusing_an_unlabelled_claim() {
    assert_variant_is_caught(
        "unlabelled_refused",
        &render_with(|c, clock, budget| {
            c.trust.is_some()
                && c.validity_at(clock, budget) != Validity::Expired
                && !matches!(c.grade_at(clock, budget), Some(TrustGrade::Inadmissible))
        }),
    );
}

// ---------------------------------------------------------------------------
// The composition
// ---------------------------------------------------------------------------

/// The version set a query carries must be derived from the live declarations,
/// not restated. A second copy is a second thing to forget.
#[test]
fn a_querys_versions_come_from_the_live_declarations() {
    let v = SemanticVersions::for_query(QueryKind::CurrentSubject);
    assert_eq!(
        v.version_of("answer_admissibility"),
        Some(BOUNDARY_SEMANTICS[0].version),
    );
    assert_eq!(
        v.version_of("world_current_fold"),
        Some(kirra_world_store::semantics::version_of(
            kirra_world_store::semantics::RuleId::WorldCurrentFold
        )),
    );
}

/// Both crates' declarations must digest their corpora the same way, or the two
/// halves of one baseline file are not comparable.
#[test]
fn both_crates_declare_digests_in_the_same_form() {
    assert_eq!(
        digest(&admissibility_rendering()),
        admissibility_corpus_digest()
    );
}
