//! **The neutral explanation artifact — Tier 4 box 4c.1.**
//!
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001`:
//!
//! > Explanation is computed on the Kirra World side of a process boundary and
//! > exported as a bounded, immutable, presentation-only artifact through a
//! > World-independent transport/type contract. Mick renders that artifact but
//! > does not query Kirra World, resolve provenance, or alter its evidentiary
//! > meaning.
//!
//! ```text
//! Kirra World / world-service
//!         │  bounded ProvenanceTree
//!         ▼
//! Explanation projection (World-side, deterministic, no wording)
//!         │  THIS CRATE — no World types, no query handles
//!         ▼
//! Mick  ──▶  human-readable rendering
//! ```
//!
//! # Why a crate rather than a module
//!
//! Fence B refuses a dependency path from the doer side into `kirra-world*`, and
//! it is right to. A renderer living in `kirra-mick` that depended on the tree
//! type would rebuild `kirra-sidecars → kirra-mick → … → kirra-world*`, and the
//! honest response to that refusal is not to supersede the ADR — it is to put
//! the World-aware half behind a process boundary and share only types that
//! carry no World in them. This crate has **no dependencies at all**, so both
//! sides may depend on it and neither inherits anything from the other.
//!
//! # The clause that makes the boundary real: no query handles
//!
//! Mick receives **resolved presentation data**, never identifiers it could use
//! to ask Kirra World another question. The distinction is not stylistic:
//! `provenance_tree(root_generation, at_generation, spec)` takes exactly a pair
//! of generations, so an artifact carrying one hands Mick the argument it needs
//! to reconstruct the forbidden dependency dynamically — over IPC, at runtime,
//! with the Cargo graph still looking clean. The boundary would exist on paper
//! only.
//!
//! So **no type in this crate can hold a World coordinate**: no generation, no
//! event id, no observation id, no bitemporal instant. What crosses is
//! [`DisplayLabel`] (already-rendered text) and [`EvidenceDigest`] (citable,
//! opaque, and not a key to any query this system offers).
//!
//! Counts and depths ARE carried and are not coordinates — `depth` indents a
//! rendering, `carriers` says *how many* events a citation could not be narrowed
//! between. Neither can be fed back to a query. That is the line the gate draws,
//! and it is why the ban is on coordinates specifically rather than on numbers:
//! this is not `kirra-proposal-context`, where the dangerous thing is any
//! magnitude and the rule is a blanket numeric ban.
//!
//! The technique is that crate's, though, and deliberately: a seam is made safe
//! by having **nowhere to put** the dangerous thing, not by everyone remembering
//! not to put it there. `ci/check_explain_artifact_neutral.py` is the mechanical
//! half.
//!
//! # What the renderer may not do with this
//!
//! The artifact's states are evidentiary facts, and rendering may choose wording
//! but not meaning. Pinned in `WM_SCOPE.md` §7 and tested in 4c.2:
//!
//! | State | Rendering must not |
//! |---|---|
//! | [`BranchState::Dangling`] | become *"came from X"* |
//! | [`BranchState::Plural`] | pick one carrier |
//! | [`Completeness::degraded`] | omit the disclosure |
//! | [`StopReason::Cycle`] | look like ordinary truncation |
//! | [`Completeness::truncated`] | omit that more evidence exists |
//! | [`Tense::Historical`] | drift into the present tense |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// The artifact schema version.
///
/// Carried so a renderer built against an older shape refuses rather than
/// rendering fields it does not understand — the same reason a recorded answer
/// carries its rule versions. It is **not** a World coordinate: it identifies
/// this crate's contract, and nothing in Kirra World can be looked up with it.
pub const EXPLANATION_ARTIFACT_VERSION: u32 = 1;

/// Text that has already been rendered World-side, ready to show a human.
///
/// Deliberately opaque: a renderer may place it in a sentence and may not parse
/// it. That is what keeps the semantics on the World side of the boundary — a
/// label that Mick took apart and reinterpreted would be Mick resolving
/// provenance, which is exactly what the placement ruling forbids.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayLabel(String);

impl DisplayLabel {
    /// Wrap already-rendered text.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text, for placing in a rendering.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DisplayLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An opaque, citable digest of a piece of evidence.
///
/// Carried so an explanation can be *checked* — an auditor holding the digest
/// can ask Kirra World, through a channel that is entitled to, whether it
/// matches. Mick cannot: a digest is not an argument to any query this system
/// offers, which is precisely why it is admissible here where a generation is
/// not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceDigest(String);

impl std::fmt::Display for EvidenceDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl EvidenceDigest {
    /// Wrap a digest.
    #[must_use]
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// The digest, for display or comparison.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether the artifact describes the past or the present.
///
/// Carried explicitly rather than inferred, because the inference is exactly the
/// mistake: an explanation whose citations all resolve looks identical to a
/// current one, and a renderer with no tense marker will narrate it in the
/// present. A historical answer says what was knowable **then**, and the tense
/// is not decoration on that — it is the claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tense {
    /// Pinned to a past coordinate. `pinned_at` is a rendered instant, not the
    /// coordinate itself: the projection converts, and that conversion is the
    /// boundary doing its job.
    Historical {
        /// A human-facing rendering of when this was true.
        pinned_at: DisplayLabel,
    },
    /// The latest state.
    Current,
}

/// What one citation resolved to, in presentation terms.
///
/// The four states are the ones `provenance_graph::CitationResolution` produces,
/// carried across the boundary WITHOUT their coordinates. `Plural` keeps its
/// cardinality because that is the fact — *"several, and Kirra cannot say
/// which"* — and loses the generations because those are query handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchState {
    /// Exactly one recorded event carried the cited evidence.
    Resolved {
        /// What that evidence was.
        evidence: DisplayLabel,
        /// Its digest, for an auditor.
        digest: EvidenceDigest,
    },
    /// Several did, and the store cannot say which.
    ///
    /// `carriers` is a count, deliberately, and the renderer's obligation is to
    /// say *several* rather than choose. There is no "first" or "newest" field
    /// here because there is nowhere for a renderer to get one from.
    Plural {
        /// What was cited.
        cited: DisplayLabel,
        /// How many recorded events carried it. Always at least 2.
        carriers: u32,
        /// Whether more carriers exist than the walk examined.
        more_than_counted: bool,
    },
    /// None did.
    Dangling {
        /// What was cited, and could not be found.
        cited: DisplayLabel,
        /// Whether deletion could explain the absence.
        reason: DanglingReason,
    },
}

/// Which kind of absence a [`BranchState::Dangling`] is.
///
/// *Never recorded* and *deleted* are different facts about the world, and a
/// renderer that collapses them tells an investigator nothing was there when the
/// truth may be that it was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DanglingReason {
    /// No compacted window could have held it.
    NeverRecorded,
    /// A compacted window could have. Necessary condition, not proof — it may
    /// over-report and may never under-report.
    PossiblyDeleted,
}

/// Whether an explanation continues below a branch, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchContinuation {
    /// The cited evidence's own provenance is in this artifact.
    Expanded {
        /// Index into [`ExplanationArtifact::nodes`]. An artifact-local
        /// position, not a World coordinate — it addresses this document and
        /// nothing outside it.
        node: u32,
    },
    /// It is not.
    Stopped(StopReason),
}

/// Why an explanation stops at a branch.
///
/// Five reasons because they call for five different sentences, and two of them
/// are the ones the ruling names as never interchangeable: a bound being reached
/// invites *"there is more"*, and a cycle must not, because raising a limit
/// cannot help circular evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The citation dangles — there is nothing below it.
    NothingToFollow,
    /// The citation is plural, so following it would require choosing.
    Ambiguous,
    /// A depth bound was reached.
    DepthLimit,
    /// A size bound was reached.
    NodeLimit,
    /// The evidence loops back on itself.
    Cycle,
}

/// One recorded citation, as presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationBranch {
    /// What it resolved to.
    pub state: BranchState,
    /// Whether the artifact continues below it.
    pub continuation: BranchContinuation,
}

/// What is known about one claim's own citations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeEvidence {
    /// The claim is recorded and its citations are known.
    Recorded {
        /// Its citations, in the order the claim recorded them. Duplicates are
        /// preserved: a claim citing the same evidence twice said so twice.
        branches: Vec<ExplanationBranch>,
        /// Whether the claim has more citations than the artifact carries.
        more_citations: bool,
    },
    /// The claim's own statement was deleted by compaction, so what it cited is
    /// unknowable. **Not** "it cited nothing".
    DeletedByCompaction,
    /// The provenance index does not cover this claim, so its citations are
    /// unknown. Also **not** "it cited nothing" — and unlike deletion, a
    /// backfill would answer it.
    NotIndexed,
}

/// One claim in the explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationNode {
    /// Distance from the root, which is 0. Indentation, not a coordinate.
    pub depth: u16,
    /// Index of the node that cited this one, `None` for the root.
    pub parent: Option<u32>,
    /// What the claim says, rendered.
    pub claim: DisplayLabel,
    /// What it rested on.
    pub evidence: NodeEvidence,
}

/// The four ways an explanation can be less than the whole answer.
///
/// Independent flags rather than one enum, mirroring the tree they come from: an
/// explanation can be truncated AND circular AND degraded at once, and a shape
/// that forces a precedence hides whichever loses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Completeness {
    /// A legitimate bound was reached; more evidence exists.
    pub truncated: bool,
    /// The evidence loops. Malformed, not bounded.
    pub cycle_detected: bool,
    /// Compaction removed evidence this explanation would otherwise contain.
    pub degraded: bool,
    /// Some claims' citations are unknown because the index does not cover them.
    pub coverage_limited: bool,
}

impl Completeness {
    /// Whether this is the whole explanation.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.truncated && !self.cycle_detected && !self.degraded && !self.coverage_limited
    }

    /// Whether anything here obliges a caveat in the rendering.
    ///
    /// Provided so a renderer cannot reach the states without a single place
    /// that answers *"must I disclose something?"* — the same reasoning that put
    /// degradation in `TemporalAnswer`'s return type rather than behind a flag a
    /// caller may forget to read.
    #[must_use]
    pub fn requires_disclosure(&self) -> bool {
        !self.is_complete()
    }
}

/// **A claim's provenance, resolved and rendered ready.**
///
/// Immutable and self-contained: everything a renderer needs is here, and
/// nothing here can be used to ask for more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationArtifact {
    /// The contract version this artifact was built to.
    pub version: u32,
    /// Whether it describes the past or the present.
    pub tense: Tense,
    /// The nodes, root first, in the order the walk produced them.
    pub nodes: Vec<ExplanationNode>,
    /// Whether anything is missing, and why.
    pub completeness: Completeness,
}

impl ExplanationArtifact {
    /// The claim being explained.
    ///
    /// `None` only for an empty artifact, which the projection does not produce
    /// — an explanation of nothing is not an explanation, and a renderer given
    /// one should say so rather than render silence.
    #[must_use]
    pub fn root(&self) -> Option<&ExplanationNode> {
        self.nodes.first()
    }

    /// The node an expanded branch leads to.
    ///
    /// Returns `None` for an index outside this artifact rather than panicking:
    /// the artifact crosses a process boundary, so a renderer must be able to
    /// handle a malformed one without dying.
    #[must_use]
    pub fn node(&self, index: u32) -> Option<&ExplanationNode> {
        self.nodes.get(index as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The states carry no coordinate, checked by construction rather than by
    /// reading: every constructor below is exhaustive, so a coordinate field
    /// added to any of them fails to compile here.
    #[test]
    fn every_branch_state_is_expressible_without_a_coordinate() {
        let _resolved = BranchState::Resolved {
            evidence: DisplayLabel::new("a scan of dock A at 09:14"),
            digest: EvidenceDigest::new("abc123"),
        };
        let _plural = BranchState::Plural {
            cited: DisplayLabel::new("a dock-A scan"),
            carriers: 2,
            more_than_counted: false,
        };
        let _never = BranchState::Dangling {
            cited: DisplayLabel::new("an unrecorded scan"),
            reason: DanglingReason::NeverRecorded,
        };
        let _deleted = BranchState::Dangling {
            cited: DisplayLabel::new("an unrecorded scan"),
            reason: DanglingReason::PossiblyDeleted,
        };
    }

    #[test]
    fn completeness_answers_the_disclosure_question_in_one_place() {
        assert!(Completeness::default().is_complete());
        assert!(!Completeness::default().requires_disclosure());
        for flags in [
            Completeness {
                truncated: true,
                ..Completeness::default()
            },
            Completeness {
                cycle_detected: true,
                ..Completeness::default()
            },
            Completeness {
                degraded: true,
                ..Completeness::default()
            },
            Completeness {
                coverage_limited: true,
                ..Completeness::default()
            },
        ] {
            assert!(
                flags.requires_disclosure(),
                "{flags:?} must oblige a caveat"
            );
        }
    }

    /// A malformed artifact from across a process boundary must not kill the
    /// renderer.
    #[test]
    fn an_out_of_range_branch_target_is_none_rather_than_a_panic() {
        let artifact = ExplanationArtifact {
            version: EXPLANATION_ARTIFACT_VERSION,
            tense: Tense::Current,
            nodes: vec![ExplanationNode {
                depth: 0,
                parent: None,
                claim: DisplayLabel::new("package 17 was last seen at dock A"),
                evidence: NodeEvidence::Recorded {
                    branches: vec![],
                    more_citations: false,
                },
            }],
            completeness: Completeness::default(),
        };
        assert!(artifact.node(0).is_some());
        assert!(artifact.node(9).is_none());
        assert!(artifact.root().is_some());
    }

    #[test]
    fn an_empty_artifact_has_no_root_to_render() {
        let empty = ExplanationArtifact {
            version: EXPLANATION_ARTIFACT_VERSION,
            tense: Tense::Current,
            nodes: vec![],
            completeness: Completeness::default(),
        };
        assert!(empty.root().is_none());
    }
}
