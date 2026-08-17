//! **Tier 4 box 4c.2 — what Mick may and may not say.**
//!
//! The renderer owns language; it does not own evidence. These tests are the
//! mechanism that makes that a fact rather than an intention, and they are
//! organised around one question asked six ways:
//!
//! > Does the sentence assert something the artifact does not say — or fail to
//! > assert something the artifact does?
//!
//! # Why the assertions are on PHRASES, not on sentences
//!
//! Exact prose must not be load-bearing, or the suite becomes a change-detector
//! and the first reword retires it. So every distinguishing clause is a public
//! constant in the renderer, and the tests assert those constants appear or do
//! not. Rewording around a phrase is free; swapping which phrase a state gets is
//! a mutation, and a mutation is what these exist to catch.
//!
//! # The negative half is the load-bearing half
//!
//! *"Dangling says it could not be resolved"* is easy to satisfy and easy to
//! satisfy **while also** saying it rested on something. The collapse this box
//! exists to prevent is not silence, it is a confident sentence next to an
//! honest one. So each state asserts its own phrase present AND the other
//! states' phrases absent.

use kirra_explain_types::{
    BranchContinuation, BranchState, Completeness, DanglingReason, DisplayLabel, EvidenceDigest,
    ExplanationArtifact, ExplanationBranch, ExplanationNode, NodeEvidence, StopReason, Tense,
    EXPLANATION_ARTIFACT_VERSION,
};
use kirra_mick::explain_render::{
    narrate, PHRASE_AS_OF, PHRASE_CANNOT_SAY_WHICH, PHRASE_CIRCULAR, PHRASE_COVERAGE_REDUCED,
    PHRASE_EVIDENCE_REMOVED, PHRASE_LIMIT_REACHED, PHRASE_MAY_BE_DELETED, PHRASE_MORE_EVIDENCE,
    PHRASE_NOT_INDEXED, PHRASE_RECORD_DELETED, PHRASE_RESTED_ON, PHRASE_UNRESOLVED,
};

/// Labels deliberately free of digits, so the "introduces no quantity" test can
/// treat ANY digit in the output as a finding.
const CLAIM: &str = "package seventeen was at dock A";
const EVIDENCE: &str = "a dock-A scan";
const CITED: &str = "the observation recorded as obs-x";

fn label(s: &str) -> DisplayLabel {
    DisplayLabel::new(s)
}

fn artifact(
    evidence: NodeEvidence,
    tense: Tense,
    completeness: Completeness,
) -> ExplanationArtifact {
    ExplanationArtifact {
        version: EXPLANATION_ARTIFACT_VERSION,
        tense,
        nodes: vec![ExplanationNode {
            depth: 0,
            parent: None,
            claim: label(CLAIM),
            evidence,
        }],
        completeness,
    }
}

fn recorded(state: BranchState, continuation: BranchContinuation) -> NodeEvidence {
    NodeEvidence::Recorded {
        branches: vec![ExplanationBranch {
            state,
            continuation,
        }],
        more_citations: false,
    }
}

fn resolved() -> BranchState {
    BranchState::Resolved {
        evidence: label(EVIDENCE),
        digest: EvidenceDigest::new("deadbeef"),
    }
}

fn dangling(reason: DanglingReason) -> BranchState {
    BranchState::Dangling {
        cited: label(CITED),
        reason,
    }
}

fn plural(carriers: u32) -> BranchState {
    BranchState::Plural {
        cited: label(CITED),
        carriers,
        more_than_counted: false,
    }
}

fn stopped(reason: StopReason) -> BranchContinuation {
    BranchContinuation::Stopped(reason)
}

#[track_caller]
fn says(text: &str, phrase: &str) {
    assert!(
        text.contains(phrase),
        "the rendering must say {phrase:?}\n  got: {text}"
    );
}

#[track_caller]
fn never_says(text: &str, phrase: &str) {
    assert!(
        !text.contains(phrase),
        "the rendering must NOT say {phrase:?}\n  got: {text}"
    );
}

// ---------------------------------------------------------------------------
// The six states
// ---------------------------------------------------------------------------

/// `Resolved` is the ONE state allowed to attribute confidently.
#[test]
fn resolved_states_the_evidence_confidently() {
    let text = narrate(&artifact(
        recorded(resolved(), stopped(StopReason::NothingToFollow)),
        Tense::Current,
        Completeness::default(),
    ))
    .text();

    says(&text, PHRASE_RESTED_ON);
    says(&text, EVIDENCE);
    never_says(&text, PHRASE_UNRESOLVED);
    never_says(&text, PHRASE_CANNOT_SAY_WHICH);
}

/// **The obligation with the sharpest failure mode.** A dangling citation
/// rendered with the resolved template says the claim came from evidence Kirra
/// could not find — a confident sentence about something that is not there.
#[test]
fn dangling_says_it_could_not_be_resolved_and_never_attributes() {
    let text = narrate(&artifact(
        recorded(
            dangling(DanglingReason::NeverRecorded),
            stopped(StopReason::NothingToFollow),
        ),
        Tense::Current,
        Completeness::default(),
    ))
    .text();

    says(&text, PHRASE_UNRESOLVED);
    says(&text, CITED);
    never_says(&text, PHRASE_RESTED_ON);
    never_says(
        &text,
        PHRASE_MAY_BE_DELETED, // never-recorded must not claim deletion
    );
}

/// The qualification inside dangling survives into the language.
#[test]
fn a_possibly_deleted_dangle_discloses_that_deletion_could_explain_it() {
    let text = narrate(&artifact(
        recorded(
            dangling(DanglingReason::PossiblyDeleted),
            stopped(StopReason::NothingToFollow),
        ),
        Tense::Current,
        Completeness::default(),
    ))
    .text();

    says(&text, PHRASE_UNRESOLVED);
    says(&text, PHRASE_MAY_BE_DELETED);
    never_says(&text, PHRASE_RESTED_ON);
}

/// `Plural` must say several matched. Choosing one is the collapse box 4b
/// refused two layers down, and it would look like a better answer.
#[test]
fn plural_says_several_matched_and_never_picks_one() {
    let text = narrate(&artifact(
        recorded(plural(3), stopped(StopReason::Ambiguous)),
        Tense::Current,
        Completeness::default(),
    ))
    .text();

    says(&text, PHRASE_CANNOT_SAY_WHICH);
    says(&text, "3");
    never_says(&text, PHRASE_RESTED_ON);
}

/// Degradation must be disclosed. An explanation missing deleted evidence and
/// not saying so reads as complete.
#[test]
fn degraded_discloses_that_evidence_was_removed() {
    let text = narrate(&artifact(
        NodeEvidence::DeletedByCompaction,
        Tense::Current,
        Completeness {
            degraded: true,
            ..Completeness::default()
        },
    ))
    .text();

    says(&text, PHRASE_EVIDENCE_REMOVED);
    says(&text, PHRASE_RECORD_DELETED);
}

/// Coverage limits are a different admission from deletion: a backfill fixes
/// one and nothing fixes the other.
#[test]
fn coverage_limited_discloses_an_unindexed_gap_distinctly_from_deletion() {
    let text = narrate(&artifact(
        NodeEvidence::NotIndexed,
        Tense::Current,
        Completeness {
            coverage_limited: true,
            ..Completeness::default()
        },
    ))
    .text();

    says(&text, PHRASE_COVERAGE_REDUCED);
    says(&text, PHRASE_NOT_INDEXED);
    never_says(&text, PHRASE_EVIDENCE_REMOVED);
}

/// **A cycle must not read as a bound.** The two call for opposite responses:
/// one says raise the limit, the other says the evidence is malformed.
#[test]
fn a_cycle_is_named_malformed_and_never_reads_as_truncation() {
    let text = narrate(&artifact(
        recorded(resolved(), stopped(StopReason::Cycle)),
        Tense::Current,
        Completeness {
            cycle_detected: true,
            ..Completeness::default()
        },
    ))
    .text();

    says(&text, PHRASE_CIRCULAR);
    says(&text, "malformed");
    never_says(&text, PHRASE_LIMIT_REACHED);
    never_says(&text, PHRASE_MORE_EVIDENCE);
}

/// Truncation must say a bound was reached AND that more exists. Saying only
/// the first leaves a reader thinking they have the whole answer.
///
/// The branch here stops for a reason that emits NO sentence of its own
/// (`NothingToFollow`), so the phrases can only have come from the
/// artifact-level disclosure. An earlier draft used `DepthLimit`, whose stop
/// sentence carries the same two phrases — and that draft stayed GREEN when the
/// disclosure was suppressed entirely, because it was reading the other source.
/// The combined-artifact test caught it; this is the single-flag test doing its
/// own job again.
#[test]
fn truncation_says_a_bound_was_reached_and_that_more_evidence_exists() {
    let text = narrate(&artifact(
        recorded(resolved(), stopped(StopReason::NothingToFollow)),
        Tense::Current,
        Completeness {
            truncated: true,
            ..Completeness::default()
        },
    ))
    .text();

    says(&text, PHRASE_LIMIT_REACHED);
    says(&text, PHRASE_MORE_EVIDENCE);
    never_says(&text, PHRASE_CIRCULAR);
}

// ---------------------------------------------------------------------------
// Tense
// ---------------------------------------------------------------------------

/// A historical artifact renders historically, on the OPENING sentence — before
/// the reader has heard anything that sounds like a present-tense fact.
#[test]
fn a_historical_artifact_is_narrated_in_the_past() {
    let text = narrate(&artifact(
        recorded(resolved(), stopped(StopReason::NothingToFollow)),
        Tense::Historical {
            pinned_at: label("nine fourteen on the seventeenth"),
        },
        Completeness::default(),
    ))
    .text();

    says(&text, PHRASE_AS_OF);
    says(&text, "the record said");
    never_says(&text, "The record says");
}

#[test]
fn a_current_artifact_is_narrated_in_the_present() {
    let text = narrate(&artifact(
        recorded(resolved(), stopped(StopReason::NothingToFollow)),
        Tense::Current,
        Completeness::default(),
    ))
    .text();

    says(&text, "The record says");
    never_says(&text, PHRASE_AS_OF);
}

// ---------------------------------------------------------------------------
// The combined artifact — the enum-precedence mistake, one layer later
// ---------------------------------------------------------------------------

/// **Box 4b made the outcomes orthogonal on purpose, and this is where that
/// decision has to survive contact with a renderer.**
///
/// An explanation can be historical AND degraded AND truncated AND circular at
/// once. A renderer built as a match — or as a summary that picks the "most
/// important" caveat — handles whichever branch comes first and silently drops
/// the rest. That is exactly the precedence mistake `GraphOutcome` avoided by
/// being four independent booleans instead of an enum, arriving two layers later
/// wearing different clothes.
///
/// Nothing else in this suite would catch it: every single-flag test would pass.
#[test]
fn all_four_incompleteness_facts_survive_together_with_the_tense() {
    let artifact = ExplanationArtifact {
        version: EXPLANATION_ARTIFACT_VERSION,
        tense: Tense::Historical {
            pinned_at: label("nine fourteen on the seventeenth"),
        },
        nodes: vec![
            ExplanationNode {
                depth: 0,
                parent: None,
                claim: label(CLAIM),
                evidence: recorded(
                    dangling(DanglingReason::PossiblyDeleted),
                    stopped(StopReason::Cycle),
                ),
            },
            ExplanationNode {
                depth: 1,
                parent: Some(0),
                claim: label("a supporting claim"),
                evidence: NodeEvidence::NotIndexed,
            },
        ],
        completeness: Completeness {
            truncated: true,
            cycle_detected: true,
            degraded: true,
            coverage_limited: true,
        },
    };

    let text = narrate(&artifact).text();

    // All four, each in its own words.
    says(&text, PHRASE_EVIDENCE_REMOVED);
    says(&text, PHRASE_COVERAGE_REDUCED);
    says(&text, PHRASE_LIMIT_REACHED);
    says(&text, PHRASE_CIRCULAR);
    // And the tense, which a caveat-focused renderer would be most likely to
    // drop once it had four other things to say.
    says(&text, PHRASE_AS_OF);
    // And the branch's own honesty, unweakened by the surrounding caveats.
    says(&text, PHRASE_UNRESOLVED);
    never_says(&text, PHRASE_RESTED_ON);
}

// ---------------------------------------------------------------------------
// Introducing nothing
// ---------------------------------------------------------------------------

/// The renderer may not introduce a coordinate, a quantity, or a fact the
/// artifact does not carry.
///
/// Checked by making every label digit-free, so the ONLY digits that may appear
/// are the ones the artifact actually carries — here, the plural carrier count.
/// A renderer that invented a generation, a timestamp, a confidence percentage
/// or a distance would put a digit in the output with nowhere to have got it
/// from.
#[test]
fn the_rendering_introduces_no_quantity_the_artifact_does_not_carry() {
    let text = narrate(&artifact(
        recorded(plural(4), stopped(StopReason::Ambiguous)),
        Tense::Historical {
            pinned_at: label("nine fourteen on the seventeenth"),
        },
        Completeness {
            truncated: true,
            ..Completeness::default()
        },
    ))
    .text();

    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    assert_eq!(
        digits, "4",
        "the only number in the rendering must be the carrier count the artifact \
         carries — anything else was invented\n  got: {text}"
    );
}

/// Every phrase the renderer can emit traces to something in the artifact. The
/// converse of the test above: not "no invented numbers" but "no invented
/// claims" for the empty case, where there is nothing to say at all.
#[test]
fn an_empty_artifact_says_so_rather_than_narrating_nothing_as_something() {
    let empty = ExplanationArtifact {
        version: EXPLANATION_ARTIFACT_VERSION,
        tense: Tense::Current,
        nodes: vec![],
        completeness: Completeness::default(),
    };
    let text = narrate(&empty).text();

    says(&text, "nothing recorded to explain");
    never_says(&text, PHRASE_RESTED_ON);
    never_says(&text, PHRASE_UNRESOLVED);
}

/// A claim that genuinely cited nothing is not the same as one whose citations
/// are unknown, and the renderer must not blur them — the storage-layer
/// distinction box 4a's coverage floor exists for, surfacing as language.
#[test]
fn cited_nothing_reads_differently_from_citations_unknown() {
    let cited_nothing = narrate(&artifact(
        NodeEvidence::Recorded {
            branches: vec![],
            more_citations: false,
        },
        Tense::Current,
        Completeness::default(),
    ))
    .text();
    let unknown = narrate(&artifact(
        NodeEvidence::NotIndexed,
        Tense::Current,
        Completeness {
            coverage_limited: true,
            ..Completeness::default()
        },
    ))
    .text();

    says(&cited_nothing, "cited nothing");
    never_says(&cited_nothing, PHRASE_NOT_INDEXED);
    says(&unknown, PHRASE_NOT_INDEXED);
    never_says(&unknown, "cited nothing");
}
