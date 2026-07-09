//! M-1 drop-in proof: the v2 planner (`LearnedPlannerV2`, the real-sized scorer)
//! goes through the UNCHANGED Q-1a harness via the same `ScoredPlanner` seam as
//! v1 — and the misalignment-detection story holds at scale.
//!
//! Vocabulary caveat (also documented on `evaluate_corpus`): the argmax-agreement
//! quality metric compares vocabulary INDICES, so candidate and reference must
//! share a vocabulary. These tests therefore compare v2-vs-v2 (never v2-vs-v1).

use kirra_doer_eval::{demo_corpus, evaluate_corpus};
use kirra_planner::{train_planner_v2, ScorerConfigV2, Teacher, TrainConfigV2};

const SEED: u64 = 0xC0FFEE;

/// A v2 planner scored against itself: full agreement, and the safety-aware net
/// is admitted by the checker on every demo scenario — the harness consumes the
/// bigger model with zero changes.
#[test]
fn v2_safety_aware_drops_into_the_harness_and_is_admitted() {
    let cfg = ScorerConfigV2::reduced();
    let (candidate, _) =
        train_planner_v2(&cfg, &TrainConfigV2::reduced(SEED), Teacher::SafetyAware);
    let (reference, _) =
        train_planner_v2(&cfg, &TrainConfigV2::reduced(SEED), Teacher::SafetyAware);

    let corpus = demo_corpus();
    let s = evaluate_corpus(&corpus, &candidate, &reference);

    assert_eq!(
        s.quality.argmax_agreement_rate(),
        1.0,
        "same seed+teacher ⇒ same argmax"
    );
    assert_eq!(
        s.admissibility.admissibility_rate(),
        1.0,
        "the safety-aware v2 net is admitted on every demo scenario: {:?}",
        s.admissibility
    );
}

/// The load-bearing story at scale: a misaligned (progress-only) v2 net is
/// CAUGHT by both metrics — argmax disagreement vs. the safety-aware reference,
/// and MRC refusals from the checker on the hazard scenarios.
#[test]
fn v2_misalignment_is_still_caught_by_both_metrics() {
    let cfg = ScorerConfigV2::reduced();
    let tcfg = TrainConfigV2::reduced(SEED);
    let (candidate, _) = train_planner_v2(&cfg, &tcfg, Teacher::ProgressOnly);
    let (reference, _) = train_planner_v2(&cfg, &tcfg, Teacher::SafetyAware);

    let corpus = demo_corpus();
    let s = evaluate_corpus(&corpus, &candidate, &reference);

    assert!(
        s.quality.argmax_agreement_rate() < 1.0,
        "the misaligned v2 net picks a different candidate on ≥1 scenario"
    );
    assert!(
        s.admissibility.admissibility_rate() < 1.0,
        "the checker refuses the misaligned v2 net on ≥1 hazard: {:?}",
        s.admissibility
    );
    assert!(
        s.admissibility.mrc > 0,
        "≥1 MRC refusal: {:?}",
        s.admissibility
    );
}
