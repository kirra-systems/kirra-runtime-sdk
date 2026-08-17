//! **The World-side explanation projection — Tier 4 box 4c.1.**
//!
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001`:
//!
//! > Explanation is computed on the Kirra World side of a process boundary and
//! > exported as a bounded, immutable, presentation-only artifact through a
//! > World-independent transport/type contract.
//!
//! This is that computation. It takes a [`ProvenanceTree`] — coordinates,
//! resolution outcomes, walk bounds — and produces a
//! [`ExplanationArtifact`] that a renderer with no access to Kirra World can
//! turn into language.
//!
//! # It lands here because this crate is the answer boundary
//!
//! `read_view` is already the one place Tier 3's *"no bare values"* rule is
//! structural, and an explanation is an **answer** — the same kind of thing,
//! about a different question. Putting the projection in `kirra-world-store`
//! would have made the store responsible for presentation, which is the
//! layering `WorldView::ask` exists to prevent.
//!
//! # What crosses, and what is deliberately consumed here
//!
//! The projection's real job is a **narrowing**, not a translation. Everything
//! that could address the store is used and dropped:
//!
//! | The tree has | The artifact gets |
//! |---|---|
//! | `root_generation`, `at_generation` | a rendered [`Tense`] |
//! | `target_generation` | a [`DisplayLabel`] + an [`EvidenceDigest`] |
//! | `target_generations` (plural) | a **count** |
//! | `NodeCitations::*` | [`NodeEvidence::*`] |
//! | `GraphOutcome`'s four flags | [`Completeness`]'s four |
//!
//! The `target_generations → count` row is the one worth pausing on. Dropping
//! the identities is not a simplification, it is the point: a renderer holding
//! them could name one, and *"pick the newest"* is the collapse box 4b refused
//! at the layer below. What survives is the fact — *several, and Kirra cannot
//! say which*.
//!
//! # A judgement about observation ids, stated because it is a judgement
//!
//! A dangling citation is only useful if the explanation says *what* dangled,
//! and the sentence `WM_SCOPE.md` §7 gives as the target — *"at that time, this
//! claim cited observation X, but Kirra could not resolve X to a recorded
//! event"* — names X. So the cited observation id is rendered INTO the label.
//!
//! That is a deliberate line, not an oversight: the ban that makes the boundary
//! real is on **generations**, because `provenance_tree(root, at, spec)` takes
//! exactly those and nothing else. An observation id is not an argument to any
//! query this system exposes. It is also the residual
//! `ci/check_explain_artifact_neutral.py` already documents — a label is text,
//! and text cannot be type-checked — so it is recorded here rather than
//! discovered later in the gate's fine print.

use kirra_explain_types::{
    BranchContinuation, BranchState, Completeness, DanglingReason, DisplayLabel, EvidenceDigest,
    ExplanationArtifact, ExplanationBranch, ExplanationNode, NodeEvidence, StopReason, Tense,
    EXPLANATION_ARTIFACT_VERSION,
};
use kirra_world_store::provenance_graph::{
    self, CitationResolution, NodeCitations, NotWalkedReason, ProvenanceTree,
};

/// Shown where a claim's own statement no longer exists.
///
/// A fixed phrase rather than an empty label: a renderer given nothing would
/// have to invent something, and what it invented would most likely read as
/// *"this claim cited nothing"* — the exact collapse `NodeEvidence` keeps apart.
pub const DELETED_CLAIM_LABEL: &str = "a claim whose record has since been deleted";

/// Shown where the citation index does not cover a claim.
pub const UNINDEXED_CLAIM_LABEL: &str = "a claim whose citations have not been indexed";

/// Shown where evidence resolved but could not be described.
///
/// Unreachable through the shipped label source — resolution means the event is
/// visible, so it can be read — and present because a `ClaimLabels`
/// implementation is a seam, and a seam that returns `None` must land somewhere
/// honest rather than in an `unwrap`.
pub const UNDESCRIBED_EVIDENCE_LABEL: &str = "evidence that could not be described";

/// **What the projection needs to turn coordinates into presentation.**
///
/// The seam exists for the reason `CitationLookup`'s does: every judgement about
/// *shape* stays in [`project_explanation`], and an implementation answers
/// narrow factual questions. It is also what lets the projection be tested
/// without a store, which matters here because the interesting cases —
/// compacted claims, unindexed claims — are awkward to produce and trivial to
/// describe.
pub trait ClaimLabels {
    /// Whatever the backing store fails with.
    type Error;

    /// A rendered description of the claim recorded at `generation`, or `None`
    /// when no such claim is retained.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn claim_label(&self, generation: i64) -> Result<Option<DisplayLabel>, Self::Error>;

    /// A rendered description and citable digest of the evidence at
    /// `generation`, or `None` when it is not retained.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn evidence(
        &self,
        generation: i64,
    ) -> Result<Option<(DisplayLabel, EvidenceDigest)>, Self::Error>;

    /// A rendered instant for a pinned coordinate — *"09:14 on 17 August"*, not
    /// the generation.
    ///
    /// This single method is where the boundary is actually enforced: it is the
    /// only route by which a coordinate becomes something the artifact carries,
    /// and what it returns is text.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn pin_label(&self, generation: i64) -> Result<DisplayLabel, Self::Error>;
}

/// Render a cited observation id for a human.
///
/// Deliberately verbose. `obs-4417` alone in a sentence reads as a typo; *"the
/// observation recorded as obs-4417"* reads as a reference, which is what it is.
#[must_use]
pub fn cited_label(observation_id: &str) -> DisplayLabel {
    DisplayLabel::new(format!("the observation recorded as {observation_id}"))
}

/// **Project a provenance tree into a presentation-only artifact.**
///
/// Total over every tree box 4b can produce, and deterministic: the same tree
/// and the same labels give the same artifact, with no wording chosen here that
/// a renderer could not have chosen itself.
///
/// `head_generation` is the store's latest coordinate, and it decides [`Tense`]:
/// a tree pinned at or beyond the head describes the present, and anything
/// earlier describes the past. Passed in rather than inferred, because the
/// projection has no store and guessing would put the most consequential field
/// in the artifact — the one that keeps a historical answer from being narrated
/// as current — behind an assumption.
///
/// # Errors
///
/// Whatever the [`ClaimLabels`] implementation fails with.
pub fn project_explanation<L: ClaimLabels>(
    tree: &ProvenanceTree,
    labels: &L,
    head_generation: i64,
) -> Result<ExplanationArtifact, L::Error> {
    let tense = if tree.at_generation >= head_generation {
        Tense::Current
    } else {
        Tense::Historical {
            pinned_at: labels.pin_label(tree.at_generation)?,
        }
    };

    let mut nodes = Vec::with_capacity(tree.nodes.len());
    for node in &tree.nodes {
        let claim = match labels.claim_label(node.generation)? {
            Some(label) => label,
            // Absent means the event is gone. Which of the two "no statement"
            // cases it is comes from the tree, not from this absence — the
            // store cannot tell an unindexed claim from a deleted one by
            // failing to describe it.
            None => DisplayLabel::new(match node.citations {
                NodeCitations::BelowCoverageFloor => UNINDEXED_CLAIM_LABEL,
                _ => DELETED_CLAIM_LABEL,
            }),
        };

        let evidence = match &node.citations {
            NodeCitations::EvidenceCompacted => NodeEvidence::DeletedByCompaction,
            NodeCitations::BelowCoverageFloor => NodeEvidence::NotIndexed,
            NodeCitations::Indexed {
                branches,
                truncated,
            } => {
                let mut out = Vec::with_capacity(branches.len());
                for branch in branches {
                    out.push(project_branch(branch, labels)?);
                }
                NodeEvidence::Recorded {
                    branches: out,
                    more_citations: *truncated,
                }
            }
        };

        nodes.push(ExplanationNode {
            // `usize`/`i64` -> `u16`/`u32`: both are bounded far below the cast
            // by `GraphSpec`, which refuses a depth above `MAX_PROVENANCE_DEPTH`
            // (32) and a node count above `MAX_PROVENANCE_NODES` (256). A
            // saturating cast is used anyway rather than a panic, because this
            // runs on the answering side of a boundary and a malformed input
            // should narrow an explanation, never take down the answerer.
            depth: u16::try_from(node.depth).unwrap_or(u16::MAX),
            parent: node.parent.map(|p| u32::try_from(p).unwrap_or(u32::MAX)),
            claim,
            evidence,
        });
    }

    Ok(ExplanationArtifact {
        version: EXPLANATION_ARTIFACT_VERSION,
        tense,
        nodes,
        completeness: Completeness {
            truncated: tree.outcome.truncated,
            cycle_detected: tree.outcome.cycle_detected,
            degraded: tree.outcome.degraded,
            coverage_limited: tree.outcome.coverage_limited,
        },
    })
}

fn project_branch<L: ClaimLabels>(
    branch: &provenance_graph::Branch,
    labels: &L,
) -> Result<ExplanationBranch, L::Error> {
    let cited = cited_label(&branch.cited_observation_id);
    let state = match &branch.resolution {
        CitationResolution::Resolved { target_generation } => {
            let (evidence, digest) = labels.evidence(*target_generation)?.unwrap_or_else(|| {
                (
                    DisplayLabel::new(UNDESCRIBED_EVIDENCE_LABEL),
                    EvidenceDigest::new(String::new()),
                )
            });
            BranchState::Resolved { evidence, digest }
        }
        CitationResolution::Plural {
            target_generations,
            truncated,
        } => BranchState::Plural {
            cited,
            // The narrowing. The identities are USED to count and then dropped:
            // a renderer holding them could name one, and naming one is the
            // collapse box 4b refused a layer down.
            carriers: u32::try_from(target_generations.len()).unwrap_or(u32::MAX),
            more_than_counted: *truncated,
        },
        CitationResolution::Dangling { reason } => BranchState::Dangling {
            cited,
            reason: match reason {
                provenance_graph::DanglingReason::NeverVisible => DanglingReason::NeverRecorded,
                provenance_graph::DanglingReason::PossiblyCompacted { .. } => {
                    // The spans are `lo_generation`s — coordinates — and they
                    // do not cross. The FACT that deletion could explain the
                    // absence does, which is what a renderer must disclose; an
                    // auditor who needs the span asks Kirra World, through a
                    // channel entitled to.
                    DanglingReason::PossiblyDeleted
                }
            },
        },
    };

    let continuation = match &branch.continuation {
        provenance_graph::BranchContinuation::Walked { node } => BranchContinuation::Expanded {
            node: u32::try_from(*node).unwrap_or(u32::MAX),
        },
        provenance_graph::BranchContinuation::NotWalked(reason) => {
            BranchContinuation::Stopped(match reason {
                NotWalkedReason::Nothing => StopReason::NothingToFollow,
                NotWalkedReason::Plural => StopReason::Ambiguous,
                NotWalkedReason::DepthLimit => StopReason::DepthLimit,
                NotWalkedReason::NodeLimit => StopReason::NodeLimit,
                // The generation the path returns to is dropped with every
                // other coordinate. That a cycle EXISTS is the renderable fact;
                // where it closes is a store question.
                NotWalkedReason::CycleDetected { .. } => StopReason::Cycle,
            })
        }
    };

    Ok(ExplanationBranch {
        state,
        continuation,
    })
}
