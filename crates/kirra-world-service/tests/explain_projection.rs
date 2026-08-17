//! **Tier 4 box 4c.1 — the World-side explanation projection.**
//!
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001` puts the explanation on the Kirra World side
//! of a process boundary and sends a presentation-only artifact across. These
//! tests are about the crossing, and the property they exist for is one
//! sentence:
//!
//! > **Nothing that could ask Kirra World another question survives the
//! > projection.**
//!
//! The type-level half is `ci/check_explain_artifact_neutral.py`, which gives
//! the artifact nowhere to put a coordinate. That is the stronger guard and it
//! is not sufficient on its own: it constrains the SHAPE, and these constrain
//! the CONTENT — that every semantic distinction box 4b produces still arrives,
//! and that the identities feeding them do not.
//!
//! # Why a fake label source rather than a store
//!
//! The interesting inputs — a claim deleted by compaction, a claim below the
//! coverage floor, a citation whose carriers are plural — are awkward to
//! produce in SQLite and trivial to describe directly. The store-backed path is
//! exercised where it belongs, in box 4b's own suite; what is under test here is
//! the narrowing, and it is a pure function of the tree.

use kirra_explain_types::{
    BranchContinuation, BranchState, DanglingReason, DisplayLabel, EvidenceDigest, NodeEvidence,
    StopReason, Tense, EXPLANATION_ARTIFACT_VERSION,
};
use kirra_world_service::explain::{project_explanation, ClaimLabels, DELETED_CLAIM_LABEL};
use kirra_world_store::provenance_graph::{
    Branch, BranchContinuation as TreeContinuation, CitationResolution,
    DanglingReason as TreeDangling, GraphOutcome, NodeCitations, NotWalkedReason, ProvenanceNode,
    ProvenanceTree,
};

/// A label source that describes anything except the generations it is told are
/// gone.
struct Labels {
    missing: Vec<i64>,
}

impl Labels {
    fn all_present() -> Self {
        Self {
            missing: Vec::new(),
        }
    }
    fn without(missing: &[i64]) -> Self {
        Self {
            missing: missing.to_vec(),
        }
    }
}

impl ClaimLabels for Labels {
    type Error = std::convert::Infallible;

    fn claim_label(&self, generation: i64) -> Result<Option<DisplayLabel>, Self::Error> {
        if self.missing.contains(&generation) {
            return Ok(None);
        }
        Ok(Some(DisplayLabel::new(format!(
            "package 17 was at dock A (claim {generation})"
        ))))
    }

    fn evidence(
        &self,
        generation: i64,
    ) -> Result<Option<(DisplayLabel, EvidenceDigest)>, Self::Error> {
        if self.missing.contains(&generation) {
            return Ok(None);
        }
        Ok(Some((
            DisplayLabel::new(format!("a dock-A scan (evidence {generation})")),
            EvidenceDigest::new(format!("digest-{generation}")),
        )))
    }

    fn pin_label(&self, generation: i64) -> Result<DisplayLabel, Self::Error> {
        Ok(DisplayLabel::new(format!("09:1{generation} on 17 August")))
    }
}

fn node(generation: i64, depth: usize, citations: NodeCitations) -> ProvenanceNode {
    ProvenanceNode {
        generation,
        depth,
        parent: if depth == 0 { None } else { Some(depth - 1) },
        via_ordinal: if depth == 0 { None } else { Some(0) },
        citations,
    }
}

fn branch(cited: &str, resolution: CitationResolution, cont: TreeContinuation) -> Branch {
    Branch {
        ordinal: 0,
        cited_observation_id: cited.to_string(),
        resolution,
        continuation: cont,
    }
}

fn indexed(branches: Vec<Branch>) -> NodeCitations {
    NodeCitations::Indexed {
        branches,
        truncated: false,
    }
}

fn tree(nodes: Vec<ProvenanceNode>, outcome: GraphOutcome) -> ProvenanceTree {
    ProvenanceTree {
        root_generation: nodes.first().map_or(0, |n| n.generation),
        at_generation: 40,
        nodes,
        outcome,
        rule_version: 1,
    }
}

/// Every string the artifact carries, so a coordinate hiding in one is visible.
fn all_text(a: &kirra_explain_types::ExplanationArtifact) -> Vec<String> {
    let mut out = Vec::new();
    if let Tense::Historical { pinned_at } = &a.tense {
        out.push(pinned_at.as_str().to_string());
    }
    for n in &a.nodes {
        out.push(n.claim.as_str().to_string());
        if let NodeEvidence::Recorded { branches, .. } = &n.evidence {
            for b in branches {
                match &b.state {
                    BranchState::Resolved { evidence, digest } => {
                        out.push(evidence.as_str().to_string());
                        out.push(digest.as_str().to_string());
                    }
                    BranchState::Plural { cited, .. } | BranchState::Dangling { cited, .. } => {
                        out.push(cited.as_str().to_string());
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 1. The narrowing — what must not survive
// ---------------------------------------------------------------------------

/// **The clause that makes the boundary real rather than nominal.**
///
/// A plural citation is where the temptation lives: the tree knows exactly which
/// events carried the id, and passing them on would be strictly more
/// informative. It would also hand a renderer the material to name one, which is
/// the collapse box 4b refused a layer down — and hand it generations, which are
/// precisely the arguments `provenance_tree(root, at, spec)` takes.
///
/// So the count crosses and the identities do not, and this asserts the second
/// half rather than trusting the first.
#[test]
fn a_plural_citations_carriers_are_counted_and_their_identities_dropped() {
    let t = tree(
        vec![node(
            10,
            0,
            indexed(vec![branch(
                "obs-x",
                CitationResolution::Plural {
                    // Distinctive values, so a leak is unmistakable in the text.
                    target_generations: vec![7717, 8823],
                    truncated: false,
                },
                TreeContinuation::NotWalked(NotWalkedReason::Plural),
            )]),
        )],
        GraphOutcome::default(),
    );

    let artifact = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    let NodeEvidence::Recorded { branches, .. } = &artifact.nodes[0].evidence else {
        panic!("expected recorded evidence");
    };
    assert_eq!(
        branches[0].state,
        BranchState::Plural {
            cited: kirra_world_service::explain::cited_label("obs-x"),
            carriers: 2,
            more_than_counted: false,
        },
        "the FACT — several, and Kirra cannot say which — crosses; the identities do not"
    );

    for text in all_text(&artifact) {
        for leaked in ["7717", "8823"] {
            assert!(
                !text.contains(leaked),
                "carrier generation {leaked} survived into {text:?} — a renderer \
                 holding it could name one carrier, and could ask Kirra World \
                 about it"
            );
        }
    }
}

/// The same narrowing at the other coordinate-bearing state: a cycle says WHERE
/// it closes, and where is a store question.
#[test]
fn a_cycles_return_generation_does_not_cross() {
    let t = tree(
        vec![node(
            10,
            0,
            indexed(vec![branch(
                "obs-loop",
                CitationResolution::Resolved {
                    target_generation: 6631,
                },
                TreeContinuation::NotWalked(NotWalkedReason::CycleDetected {
                    back_to_generation: 6631,
                }),
            )]),
        )],
        GraphOutcome {
            cycle_detected: true,
            ..GraphOutcome::default()
        },
    );

    let artifact = project_explanation(&t, &Labels::without(&[6631]), 99).expect("project");
    let NodeEvidence::Recorded { branches, .. } = &artifact.nodes[0].evidence else {
        panic!("expected recorded evidence");
    };
    assert_eq!(
        branches[0].continuation,
        BranchContinuation::Stopped(StopReason::Cycle)
    );
    assert!(artifact.completeness.cycle_detected);
    for text in all_text(&artifact) {
        assert!(
            !text.contains("6631"),
            "the generation a cycle returns to survived into {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Every semantic distinction survives
// ---------------------------------------------------------------------------

/// The narrowing must not become a flattening. Each of these is a distinction
/// box 4b went to trouble to keep, and a projection that lost one would hand
/// Mick an artifact it could render honestly and still be wrong.
#[test]
fn the_three_resolution_outcomes_arrive_distinguishable() {
    let t = tree(
        vec![node(
            10,
            0,
            indexed(vec![
                branch(
                    "obs-one",
                    CitationResolution::Resolved {
                        target_generation: 5,
                    },
                    TreeContinuation::NotWalked(NotWalkedReason::DepthLimit),
                ),
                branch(
                    "obs-many",
                    CitationResolution::Plural {
                        target_generations: vec![6, 7, 8],
                        truncated: true,
                    },
                    TreeContinuation::NotWalked(NotWalkedReason::Plural),
                ),
                branch(
                    "obs-never",
                    CitationResolution::Dangling {
                        reason: TreeDangling::NeverVisible,
                    },
                    TreeContinuation::NotWalked(NotWalkedReason::Nothing),
                ),
                branch(
                    "obs-gone",
                    CitationResolution::Dangling {
                        reason: TreeDangling::PossiblyCompacted { spans: vec![2] },
                    },
                    TreeContinuation::NotWalked(NotWalkedReason::Nothing),
                ),
            ]),
        )],
        GraphOutcome::default(),
    );

    let artifact = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    let NodeEvidence::Recorded { branches, .. } = &artifact.nodes[0].evidence else {
        panic!("expected recorded evidence");
    };
    assert_eq!(branches.len(), 4, "order and count preserved");

    assert!(matches!(branches[0].state, BranchState::Resolved { .. }));
    assert!(matches!(
        branches[1].state,
        BranchState::Plural {
            carriers: 3,
            more_than_counted: true,
            ..
        }
    ));
    assert_eq!(
        branches[2].state,
        BranchState::Dangling {
            cited: kirra_world_service::explain::cited_label("obs-never"),
            reason: DanglingReason::NeverRecorded,
        }
    );
    assert_eq!(
        branches[3].state,
        BranchState::Dangling {
            cited: kirra_world_service::explain::cited_label("obs-gone"),
            reason: DanglingReason::PossiblyDeleted,
        },
        "never-recorded and possibly-deleted must not merge — they are different \
         facts, and only one of them means nothing was ever there"
    );
}

/// The three node states, likewise. *Cited nothing*, *record deleted* and *not
/// indexed* look identical from a renderer that only counts children.
#[test]
fn the_three_node_evidence_states_arrive_distinguishable() {
    let t = tree(
        vec![
            node(10, 0, indexed(vec![])),
            node(11, 1, NodeCitations::EvidenceCompacted),
            node(12, 2, NodeCitations::BelowCoverageFloor),
        ],
        GraphOutcome {
            degraded: true,
            coverage_limited: true,
            ..GraphOutcome::default()
        },
    );

    let artifact = project_explanation(&t, &Labels::without(&[11, 12]), 99).expect("project");
    assert!(matches!(
        artifact.nodes[0].evidence,
        NodeEvidence::Recorded {
            ref branches,
            more_citations: false
        } if branches.is_empty()
    ));
    assert_eq!(
        artifact.nodes[1].evidence,
        NodeEvidence::DeletedByCompaction
    );
    assert_eq!(artifact.nodes[2].evidence, NodeEvidence::NotIndexed);
    assert!(artifact.completeness.degraded);
    assert!(artifact.completeness.coverage_limited);
}

/// A node whose record is gone still needs something to call it, and the
/// substitute must not read as *"cited nothing"*.
#[test]
fn a_deleted_claim_is_labelled_rather_than_left_blank() {
    let t = tree(
        vec![node(10, 0, NodeCitations::EvidenceCompacted)],
        GraphOutcome {
            degraded: true,
            ..GraphOutcome::default()
        },
    );
    let artifact = project_explanation(&t, &Labels::without(&[10]), 99).expect("project");
    assert_eq!(artifact.nodes[0].claim.as_str(), DELETED_CLAIM_LABEL);
    assert!(!artifact.nodes[0].claim.as_str().is_empty());
}

/// All four completeness flags are independent, and a projection that ORed them
/// into one would let a cycle be reported as an ordinary bound.
#[test]
fn each_completeness_flag_crosses_on_its_own() {
    for (outcome, expect) in [
        (
            GraphOutcome {
                truncated: true,
                ..GraphOutcome::default()
            },
            (true, false, false, false),
        ),
        (
            GraphOutcome {
                cycle_detected: true,
                ..GraphOutcome::default()
            },
            (false, true, false, false),
        ),
        (
            GraphOutcome {
                degraded: true,
                ..GraphOutcome::default()
            },
            (false, false, true, false),
        ),
        (
            GraphOutcome {
                coverage_limited: true,
                ..GraphOutcome::default()
            },
            (false, false, false, true),
        ),
    ] {
        let t = tree(vec![node(10, 0, indexed(vec![]))], outcome);
        let c = project_explanation(&t, &Labels::all_present(), 99)
            .expect("project")
            .completeness;
        assert_eq!(
            (
                c.truncated,
                c.cycle_detected,
                c.degraded,
                c.coverage_limited
            ),
            expect,
            "{outcome:?} did not cross as itself"
        );
        assert!(c.requires_disclosure());
    }
}

// ---------------------------------------------------------------------------
// 3. Tense — the field that keeps a historical answer from being narrated now
// ---------------------------------------------------------------------------

/// A tree pinned behind the head describes the past, and says so.
///
/// This is the artifact-level counterpart of box 4b's T1/T2 property: 4b makes
/// the ANSWER historically honest, and this makes the honesty legible to
/// something that cannot see the coordinate. An explanation whose citations all
/// resolve looks exactly like a current one, so without this a renderer would
/// narrate a week-old answer in the present tense and be wrong in a way nothing
/// in the text would reveal.
#[test]
fn a_pinned_tree_is_historical_and_a_head_pinned_one_is_current() {
    let t = tree(vec![node(10, 0, indexed(vec![]))], GraphOutcome::default());

    let historical = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    assert!(
        matches!(historical.tense, Tense::Historical { .. }),
        "pinned at 40 with head 99 — this describes the past"
    );

    let current = project_explanation(&t, &Labels::all_present(), 40).expect("project");
    assert_eq!(
        current.tense,
        Tense::Current,
        "pinned at the head — this describes the present"
    );

    // And beyond the head, which a caller can legitimately ask for: still
    // current, never a Historical whose rendered instant is in the future.
    let beyond = project_explanation(&t, &Labels::all_present(), 3).expect("project");
    assert_eq!(beyond.tense, Tense::Current);
}

/// The pin crosses as rendered text, not as the coordinate.
#[test]
fn the_pin_crosses_as_an_instant_and_not_as_a_generation() {
    let mut t = tree(vec![node(10, 0, indexed(vec![]))], GraphOutcome::default());
    t.at_generation = 7;
    let artifact = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    let Tense::Historical { pinned_at } = &artifact.tense else {
        panic!("expected a historical artifact");
    };
    assert_eq!(
        pinned_at.as_str(),
        "09:17 on 17 August",
        "what crosses is what the label source rendered — the projection never \
         formats a coordinate itself"
    );
}

// ---------------------------------------------------------------------------
// 4. Totality
// ---------------------------------------------------------------------------

/// The projection runs on the answering side of a process boundary, so a
/// malformed tree must narrow an explanation rather than take down the answerer.
#[test]
fn an_empty_tree_projects_to_an_artifact_with_nothing_to_render() {
    let t = tree(vec![], GraphOutcome::default());
    let artifact = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    assert_eq!(artifact.version, EXPLANATION_ARTIFACT_VERSION);
    assert!(artifact.root().is_none());
    assert!(artifact.nodes.is_empty());
}

/// Structure survives: depth, parent and branch order are what a renderer
/// indents and reads by.
#[test]
fn depth_parent_and_expansion_targets_survive() {
    let t = tree(
        vec![
            node(
                10,
                0,
                indexed(vec![branch(
                    "obs-a",
                    CitationResolution::Resolved {
                        target_generation: 11,
                    },
                    TreeContinuation::Walked { node: 1 },
                )]),
            ),
            node(11, 1, indexed(vec![])),
        ],
        GraphOutcome::default(),
    );
    let artifact = project_explanation(&t, &Labels::all_present(), 99).expect("project");
    assert_eq!(artifact.nodes[0].depth, 0);
    assert_eq!(artifact.nodes[0].parent, None);
    assert_eq!(artifact.nodes[1].depth, 1);
    assert_eq!(artifact.nodes[1].parent, Some(0));

    let NodeEvidence::Recorded { branches, .. } = &artifact.nodes[0].evidence else {
        panic!("expected recorded evidence");
    };
    assert_eq!(
        branches[0].continuation,
        BranchContinuation::Expanded { node: 1 }
    );
    assert!(
        artifact.node(1).is_some(),
        "an expanded branch must address a node that exists in the artifact"
    );
}
