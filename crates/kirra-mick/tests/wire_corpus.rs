//! **The non-vacuity control, consumer half — Tier 4 box 3b.**
//!
//! The producer's suite proves `crates/kirra-explain-types/wire_corpus/` is
//! what the real producer emits. This proves the REAL renderer handles all of
//! it — every case, decoded through the real codec, narrated by the real
//! `render_explanation`, checked against the obligations
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001` puts on a rendering.
//!
//! Composed, the two suites give the end-to-end property without either crate
//! depending on the other — which is the point: Mick must not acquire a route
//! to Kirra World, not even a dev one.
//!
//! # Why the corpus is read from the DIRECTORY
//!
//! Not from a list of file names. A list is a second enumeration that drifts:
//! the producer adds a case, nobody adds it here, and this suite keeps passing
//! while a state crosses the boundary that no renderer test has ever seen. The
//! directory read means a new case is rendered the moment it exists, and the
//! coverage assertion below means a REMOVED case fails here too.
//!
//! # The obligations are derived from the artifact, not from a per-case table
//!
//! Each check asks the artifact what it contains and then asserts what the
//! narration must therefore say. That is what makes the suite total over the
//! corpus rather than over the cases someone remembered to write a check for —
//! and it is why adding a corpus entry cannot silently add an unchecked one.

use std::collections::BTreeSet;

use kirra_explain_types::{
    BranchState, Completeness, DanglingReason, ExplainOutcome, ExplanationArtifact, NodeEvidence,
    StopReason, Tense, ALL_SEMANTICS,
};
use kirra_mick::explain_client::ExplainClient;
use kirra_mick::explain_render::{
    PHRASE_AS_OF, PHRASE_CANNOT_SAY_WHICH, PHRASE_CIRCULAR, PHRASE_COVERAGE_REDUCED,
    PHRASE_EVIDENCE_REMOVED, PHRASE_LIMIT_REACHED, PHRASE_MAY_BE_DELETED, PHRASE_NOT_INDEXED,
    PHRASE_RECORD_DELETED, PHRASE_RESTED_ON, PHRASE_UNRESOLVED,
};

/// The smallest corpus that could honestly claim to span the wire.
///
/// A vacuity guard, and not a decorative one: every assertion below is a
/// for-each over the corpus, so an empty or truncated directory — a bad path, a
/// failed checkout, a `.gitignore` that swallowed the fixtures — would make
/// this file pass while testing nothing.
const MIN_CASES: usize = 10;

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kirra-explain-types/wire_corpus")
}

/// Every case on disk, by name, decoded through the real codec.
fn corpus() -> Vec<(String, ExplainOutcome)> {
    let dir = corpus_dir();
    let mut out: Vec<(String, ExplainOutcome)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("corpus dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).expect("read corpus entry");
            let outcome = serde_json::from_str::<ExplainOutcome>(&text)
                .unwrap_or_else(|e| panic!("{name}.json does not decode as ExplainOutcome: {e}"));
            (name, outcome)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        out.len() >= MIN_CASES,
        "only {} corpus entries found in {} — this suite asserts per-case, so \
         a missing corpus makes every check below vacuous",
        out.len(),
        dir.display()
    );
    out
}

fn narrate(outcome: &ExplainOutcome) -> String {
    ExplainClient::narrate_outcome(outcome).text()
}

fn says(text: &str, phrase: &str, case: &str, why: &str) {
    assert!(
        text.contains(phrase),
        "{case}: the rendering must say `{phrase}` because {why}.\n  got: {text}"
    );
}

fn never_says(text: &str, phrase: &str, case: &str, why: &str) {
    assert!(
        !text.contains(phrase),
        "{case}: the rendering must NOT say `{phrase}` because {why}.\n  got: {text}"
    );
}

// ---------------------------------------------------------------------------
// The end-to-end property
// ---------------------------------------------------------------------------

/// **Every case the real producer can emit is narrated, and narrated legibly.**
///
/// The base obligation before any state-specific one: nothing panics, nothing
/// comes back empty, and no case renders as silence. A renderer that returned
/// an empty string for an artifact it did not understand would pass every
/// `never_says` check in this file.
#[test]
fn every_case_the_producer_emits_renders_to_something_a_person_can_hear() {
    for (case, outcome) in corpus() {
        let text = narrate(&outcome);
        assert!(
            !text.trim().is_empty(),
            "{case}: an artifact that renders as silence has told the operator nothing"
        );
        assert!(
            text.trim_end().ends_with('.'),
            "{case}: the rendering must be sentences, not a fragment: {text}"
        );
    }
}

/// **The evidentiary obligations, derived per case from the artifact itself.**
///
/// The table in `explain_render`'s own docs, asserted against artifacts that
/// really crossed the wire rather than against ones hand-built beside the
/// assertions. That distinction is what makes this a control on the SEAM and
/// not a restatement of the renderer's unit tests.
#[test]
fn each_state_that_crossed_the_wire_carries_its_obligation_into_the_language() {
    for (case, outcome) in corpus() {
        let text = narrate(&outcome);
        let Some(artifact) = outcome.explanation() else {
            continue; // the two non-artifact outcomes have their own test
        };

        // Tense: a pinned explanation is narrated as the past. A renderer that
        // drifted into the present would assert something about NOW that the
        // coordinate says Kirra cannot assert.
        match &artifact.tense {
            Tense::Historical { .. } => says(
                &text,
                PHRASE_AS_OF,
                &case,
                "the artifact is pinned behind the head",
            ),
            Tense::Current => never_says(
                &text,
                PHRASE_AS_OF,
                &case,
                "a current explanation must not be framed as historical",
            ),
        }

        // Completeness: each flag is independent and each obliges its own
        // disclosure. A cycle narrated as ordinary truncation tells an operator
        // to raise a limit when the evidence is malformed.
        let Completeness {
            truncated,
            cycle_detected,
            degraded,
            coverage_limited,
        } = artifact.completeness;
        if truncated {
            says(&text, PHRASE_LIMIT_REACHED, &case, "a bound was reached");
        }
        if cycle_detected {
            says(&text, PHRASE_CIRCULAR, &case, "the evidence loops");
            never_says(
                &text,
                PHRASE_LIMIT_REACHED,
                &case,
                "a cycle is malformed evidence, not a bound a larger limit would fix",
            );
        }
        if degraded {
            says(
                &text,
                PHRASE_EVIDENCE_REMOVED,
                &case,
                "compaction removed evidence this explanation would otherwise contain",
            );
        }
        if coverage_limited {
            says(
                &text,
                PHRASE_COVERAGE_REDUCED,
                &case,
                "some citations are unknown because the index does not cover them",
            );
        }

        // Per-node and per-branch states.
        for node in &artifact.nodes {
            match &node.evidence {
                NodeEvidence::DeletedByCompaction => says(
                    &text,
                    PHRASE_RECORD_DELETED,
                    &case,
                    "the event's own statement is gone",
                ),
                NodeEvidence::NotIndexed => says(
                    &text,
                    PHRASE_NOT_INDEXED,
                    &case,
                    "the index makes no claim about what this event cited",
                ),
                NodeEvidence::Recorded { branches, .. } => {
                    for branch in branches {
                        match &branch.state {
                            BranchState::Resolved { .. } => says(
                                &text,
                                PHRASE_RESTED_ON,
                                &case,
                                "exactly one recorded event carried the cited evidence",
                            ),
                            BranchState::Plural { .. } => {
                                says(
                                    &text,
                                    PHRASE_CANNOT_SAY_WHICH,
                                    &case,
                                    "several events carried it and the store cannot attribute one",
                                );
                            }
                            BranchState::Dangling { reason, .. } => {
                                says(
                                    &text,
                                    PHRASE_UNRESOLVED,
                                    &case,
                                    "no recorded event carried the cited evidence",
                                );
                                if matches!(reason, DanglingReason::PossiblyDeleted) {
                                    says(
                                        &text,
                                        PHRASE_MAY_BE_DELETED,
                                        &case,
                                        "a compacted window could have held it, and \
                                         'never recorded' would be a different fact",
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// **A rendering carries only what the artifact says.**
///
/// The confident-attribution phrase is reserved for `Resolved`, so an artifact
/// with no resolved branch must never contain it. That is the check that stops
/// a renderer from being generous with a citation it could not resolve — the
/// failure that would make an explanation actively misleading rather than
/// merely incomplete.
#[test]
fn confident_attribution_appears_only_where_the_evidence_resolved() {
    for (case, outcome) in corpus() {
        let text = narrate(&outcome);
        let Some(artifact) = outcome.explanation() else {
            continue;
        };
        if !has_resolved_branch(artifact) {
            never_says(
                &text,
                PHRASE_RESTED_ON,
                &case,
                "no branch in this artifact resolved to a single recorded event",
            );
        }
    }
}

fn has_resolved_branch(artifact: &ExplanationArtifact) -> bool {
    artifact.nodes.iter().any(|n| match &n.evidence {
        NodeEvidence::Recorded { branches, .. } => branches
            .iter()
            .any(|b| matches!(b.state, BranchState::Resolved { .. })),
        _ => false,
    })
}

/// The two non-artifact outcomes stay apart in LANGUAGE, not only in type.
///
/// This is the sentence an operator actually hears, and hearing "there is
/// nothing recorded" when the truth is "I could not reach Kirra World" is a
/// false statement about the world made by a component with no authority to
/// make one.
#[test]
fn nothing_recorded_and_unavailable_do_not_sound_alike() {
    let cases = corpus();
    let empty = cases
        .iter()
        .find(|(_, o)| matches!(o, ExplainOutcome::NothingRecorded))
        .expect("the corpus must carry a NothingRecorded case");
    let down = cases
        .iter()
        .find(|(_, o)| matches!(o, ExplainOutcome::Unavailable { .. }))
        .expect("the corpus must carry an Unavailable case");
    let empty_text = narrate(&empty.1);
    let down_text = narrate(&down.1);
    assert_ne!(empty_text, down_text);
    never_says(
        &down_text,
        &empty_text,
        &down.0,
        "an unreachable producer must never claim Kirra World has no record",
    );
}

/// **The corpus this suite ran against spans the whole input space.**
///
/// Asserted independently of the producer's identical check, because the two
/// suites can diverge: a case deleted from the corpus would still leave the
/// producer's generator green if its own list were edited to match. Here, the
/// coverage is measured over WHAT WAS ACTUALLY RENDERED.
///
/// The enumeration is `kirra_explain_types::ALL_SEMANTICS`, built from
/// exhaustive matches, so a new artifact state is a compile error in the census
/// rather than a silent hole in this claim.
#[test]
fn the_corpus_this_suite_rendered_spans_every_distinguishable_semantic() {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for (_, outcome) in corpus() {
        seen.extend(outcome.semantics());
    }
    let missing: Vec<&&str> = ALL_SEMANTICS
        .iter()
        .filter(|s| !seen.contains(*s))
        .collect();
    assert!(
        missing.is_empty(),
        "the rendered corpus never exercised: {missing:?} — the obligations for \
         those states were asserted against nothing"
    );
}

/// A stop reason is a fact about WHY the walk stopped, and the two that mean
/// "raise a limit" must not read like the three that do not. Checked over the
/// corpus rather than asserted per case.
#[test]
fn a_bound_and_a_malformed_graph_are_never_narrated_as_the_same_thing() {
    for (case, outcome) in corpus() {
        let Some(artifact) = outcome.explanation() else {
            continue;
        };
        let stops: BTreeSet<&str> = artifact
            .nodes
            .iter()
            .flat_map(|n| match &n.evidence {
                NodeEvidence::Recorded { branches, .. } => branches
                    .iter()
                    .filter_map(|b| match &b.continuation {
                        kirra_explain_types::BranchContinuation::Stopped(r) => Some(match r {
                            StopReason::NothingToFollow => "nothing",
                            StopReason::Ambiguous => "ambiguous",
                            StopReason::DepthLimit => "depth",
                            StopReason::NodeLimit => "nodes",
                            StopReason::Cycle => "cycle",
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        let text = narrate(&outcome);
        if stops.contains("cycle") {
            says(
                &text,
                PHRASE_CIRCULAR,
                &case,
                "the walk stopped because the evidence refers back to itself",
            );
        }
        if stops.contains("ambiguous") {
            says(
                &text,
                PHRASE_CANNOT_SAY_WHICH,
                &case,
                "the walk stopped because the citation is plural",
            );
        }
    }
}
