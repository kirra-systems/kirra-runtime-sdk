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
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DanglingReason {
    /// No compacted window could have held it.
    NeverRecorded,
    /// A compacted window could have. Necessary condition, not proof — it may
    /// over-report and may never under-report.
    PossiblyDeleted,
}

/// Whether an explanation continues below a branch, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExplanationBranch {
    /// What it resolved to.
    pub state: BranchState,
    /// Whether the artifact continues below it.
    pub continuation: BranchContinuation,
}

/// What is known about one claim's own citations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

pub mod wire;
pub use wire::{
    ExplainCurrentSubject, ExplainOutcome, ALL_SEMANTICS, EXPLAIN_CURRENT_SUBJECT_PATH,
};

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

    /// Every semantic this artifact distinguishes survives a real codec.
    ///
    /// SCOPE, stated so it is not mistaken for more than it is: this proves
    /// SERIALIZATION. It does not prove the World-side producer and the
    /// Mick-side renderer are connected — a hand-built value round-tripping
    /// would stay green with either endpoint deleted. That control belongs to
    /// the transport box, and runs the real projection through the wire into
    /// the real renderer.
    ///
    /// What it does buy is that the transport box never has to wonder whether a
    /// missing caveat came from the wire: the states that oblige a caveat are
    /// checked here, at the type, where the failure would be cheapest to find.
    #[test]
    fn every_distinguishable_state_survives_a_round_trip() {
        let artifact = ExplanationArtifact {
            version: EXPLANATION_ARTIFACT_VERSION,
            tense: Tense::Historical {
                pinned_at: DisplayLabel::new("last Tuesday"),
            },
            nodes: vec![
                ExplanationNode {
                    depth: 0,
                    parent: None,
                    claim: DisplayLabel::new("package 17 was last seen at dock A"),
                    evidence: NodeEvidence::Recorded {
                        branches: vec![
                            ExplanationBranch {
                                state: BranchState::Resolved {
                                    evidence: DisplayLabel::new("a dock-A scan"),
                                    digest: EvidenceDigest::new("9f86d081"),
                                },
                                continuation: BranchContinuation::Expanded { node: 1 },
                            },
                            ExplanationBranch {
                                state: BranchState::Plural {
                                    cited: DisplayLabel::new("a dock-A scan"),
                                    carriers: 3,
                                    more_than_counted: true,
                                },
                                continuation: BranchContinuation::Stopped(StopReason::Ambiguous),
                            },
                            ExplanationBranch {
                                state: BranchState::Dangling {
                                    cited: DisplayLabel::new("an unrecorded scan"),
                                    reason: DanglingReason::PossiblyDeleted,
                                },
                                continuation: BranchContinuation::Stopped(StopReason::Cycle),
                            },
                        ],
                        more_citations: true,
                    },
                },
                ExplanationNode {
                    depth: 1,
                    parent: Some(0),
                    claim: DisplayLabel::new("the scan was recorded by the dock reader"),
                    evidence: NodeEvidence::DeletedByCompaction,
                },
                ExplanationNode {
                    depth: 1,
                    parent: Some(0),
                    claim: DisplayLabel::new("the reader was calibrated"),
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

        // Non-vacuity: a fixture that had quietly lost its distinguishing
        // states would round-trip perfectly and prove nothing.
        assert!(matches!(artifact.tense, Tense::Historical { .. }));
        assert!(artifact.completeness.requires_disclosure());
        assert_eq!(artifact.nodes.len(), 3);

        let wire = serde_json::to_string(&artifact).expect("artifact must serialize");
        let back: ExplanationArtifact =
            serde_json::from_str(&wire).expect("artifact must deserialize");

        assert_eq!(
            back, artifact,
            "a semantic changed across the wire — the renderer would narrate a \
             different answer from the one Kirra World projected"
        );
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

// ---------------------------------------------------------------------------
// Relationship status — the read-only identity view.
// ---------------------------------------------------------------------------

/// The contract version for the relationship view.
///
/// Versioned SEPARATELY from [`EXPLANATION_ARTIFACT_VERSION`]. They describe
/// different things and will change for different reasons; one number for both
/// would force a lockstep bump on every consumer of either.
pub const RELATIONS_VIEW_VERSION: u32 = 1;

/// The route prefix the relationship view is served on.
///
/// A PREFIX rather than a whole path, because the subject travels in the path.
/// Shared so the producer's route table and any client agree by construction
/// rather than by two string literals that match today.
pub const RELATIONS_PATH_PREFIX: &str = "/relations/";

/// How well the evidence behind a relation still resolves.
///
/// The four cases are not a severity scale and must not be collapsed into one.
/// `KIRRA-WM-EVIDENCE-RETENTION-001` ruled that compaction may degrade the
/// ability to explain WHY a promotion was made but never WHETHER it holds — so
/// every variant here describes the EXPLANATION, and none of them qualifies the
/// relation itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceStanding {
    /// Exactly one visible record carries the cited evidence.
    Resolved,
    /// The evidence is gone and a compacted span could have held it. The
    /// relation stands; its explanation has decayed.
    ///
    /// Distinct from [`Self::Dangling`], and ADR-0041 §11.3 forbids collapsing
    /// them: *whatever carried this was deleted* and *nothing ever carried
    /// this* are different findings about the record.
    Degraded,
    /// No visible record carries it, and no compaction could explain that.
    /// Something is wrong with the citation itself.
    Dangling,
    /// Several records carry the cited id, so the evidence cannot be attributed
    /// to one. Reported rather than reduced — picking the newest would name one
    /// record as the source of something the store cannot attribute.
    Plural,
}

/// One pair this subject is currently adjudicated the same as.
///
/// # What is deliberately absent
///
/// **No outcome field.** Every pair in this view is related BY PROMOTION —
/// `KIRRA-WM-ADJUDICATION-PRECEDENCE-001` means a withdrawn pair is simply not
/// here. An `outcome` could therefore only ever read `promoted`, and a constant
/// field invites a consumer to branch on something that cannot vary.
///
/// **No adjudicator class.** For the same reason: `KIRRA-WM-PROMOTION-001` v1
/// authorizes `SourceClass::Operator` and nothing else, so a class field is a
/// second constant. Every `adjudicator` below is a human operator, and that is
/// a property of the ruling rather than of the row.
///
/// **No queryable handle.** No `AnswerRef`, no cursor, no generation as a
/// number — see [`Self::decision_marker`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelatedPair {
    /// The canonical pair's low entity.
    pub low: String,
    /// The canonical pair's high entity.
    pub high: String,
    /// The OTHER entity — the one that is not the subject asked about.
    ///
    /// Derivable from the pair, and provided anyway: deriving it means asking
    /// *was my subject the low or the high* at every call site, and getting
    /// that backwards silently answers with the entity the caller already had.
    pub other: String,
    /// Who decided. An operator identity, never a service account: see the
    /// type's note on the absent class field.
    pub adjudicator: String,
    /// **An opaque marker for the decision that put this row here.**
    ///
    /// Stated precisely, because "opaque" is easy to overclaim. It is derived
    /// from the deciding record's log coordinate and is stable for a given
    /// decision, so an operator CAN correlate it with the audit record — which
    /// is the point of serving it at all.
    ///
    /// What the contract does not promise: that it is a number, that it orders,
    /// or that any endpoint accepts it. No route on this service takes a
    /// decision marker as input, which is what keeps it from becoming the
    /// queryable handle `KIRRA-WM-EXPLAIN-NEUTRALITY` forbids — a client cannot
    /// use it to ask for anything.
    pub decision_marker: String,
    /// How well the evidence behind this relation still resolves.
    pub provenance: ProvenanceStanding,
}

/// What one subject is currently adjudicated the same as.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelationsView {
    /// The subject asked about, echoed so a stored response is self-describing.
    pub subject: String,
    /// The pairs currently in effect, ascending by the other entity.
    pub related: Vec<RelatedPair>,
    /// Whether more relations exist than one page carries.
    ///
    /// Carried rather than inferred from the length: a full page and a
    /// cut-short page are the same length and mean different things.
    pub truncated: bool,
}

/// The tagged outcome of a relationship request.
///
/// Always the thing in the body, for [`ExplainOutcome`]'s reason: a client
/// decodes exactly one type, and *the service is down* never looks like *this
/// subject is related to nothing*.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RelationsOutcome {
    /// The question was answered. `related` may be empty — that is an answer.
    Related {
        /// The view.
        view: RelationsView,
    },
    /// The subject is not an askable entity identity. A REFUSAL, distinct from
    /// an empty answer: told the latter, a caller concludes the entity exists.
    NotAnEntity {
        /// Why it was not admissible.
        reason: String,
    },
    /// The producer could not answer. Never confusable with "related to
    /// nothing".
    Unavailable {
        /// What went wrong, in operator-readable terms.
        reason: String,
    },
}
