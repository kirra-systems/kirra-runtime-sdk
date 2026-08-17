//! **Mick's explanation renderer — Tier 4 box 4c.2.**
//!
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001`:
//!
//! > Mick renders that artifact but does not query Kirra World, resolve
//! > provenance, or alter its evidentiary meaning.
//!
//! This module owns **language**. It does not own evidence, and the split is the
//! whole architecture:
//!
//! ```text
//! recorded citation → historical graph semantics → presentation-safe artifact → this
//!      (4a)                    (4b)                        (4c.1)                (4c.2)
//! ```
//!
//! Its only input is [`ExplanationArtifact`], whose crate has no dependencies at
//! all. There is no route from here to Kirra World, by construction rather than
//! by care.
//!
//! # Why deterministic templates rather than the model
//!
//! Mick has a local LLM and this does not use it. A paraphrase of a *proven*
//! artifact is a reasonable thing to want later; a model cannot be the mechanism
//! that **proves** `Dangling`, `Plural`, `Degraded`, `CycleDetected` and
//! `Truncated` stay semantically distinct, because the property under test is
//! *"this sentence does not assert something the artifact denies"* and there is
//! no way to assert that about a sampled distribution.
//!
//! So the templates are the contract and the mutation suite is what makes it
//! one. If a paraphrase layer is added, it goes **after** this, over text that
//! has already been proven, and it inherits the same tests.
//!
//! # The eight obligations
//!
//! Pinned in `WM_SCOPE.md` §7, restated here because this is where they are
//! actually kept:
//!
//! | State | The rendering must | and must never |
//! |---|---|---|
//! | `Resolved` | state the evidence confidently | — |
//! | `Dangling` | say it could not be resolved | imply it was observed successfully |
//! | `Plural` | say several matched | choose one |
//! | `degraded` | disclose evidence was removed | omit it |
//! | `cycle_detected` | name it malformed and recursive | call it ordinary truncation |
//! | `truncated` | say a bound was reached | omit that more exists |
//! | `Historical` | stay in past tense | drift into the present |
//! | any | carry only what the artifact says | introduce a coordinate, a quantity, or a fact |
//!
//! # Why the body is past-tense even in a current rendering
//!
//! Only the OPENING sentence switches on [`Tense`]. The body says *"it rested
//! on"* either way, because a citation is a past act in both cases: a claim
//! recorded today still cited its evidence when it was written. Making the body
//! agree with the opening would produce *"it rests on"*, which asserts something
//! about **now** — and for a pinned answer that is precisely the assertion the
//! coordinate says Kirra cannot make.
//!
//! # The phrases are public, deliberately
//!
//! Every distinguishing clause below is a named constant, and the tests assert
//! on **those** rather than on whole sentences. That is what keeps the suite
//! measuring semantics instead of punctuation: rewording a sentence around a
//! phrase is free, and swapping which phrase a state gets is a mutation the
//! suite reds on.

use kirra_explain_types::{
    BranchContinuation, BranchState, Completeness, DanglingReason, ExplanationArtifact,
    ExplanationNode, NodeEvidence, StopReason, Tense,
};

/// Opens a historical rendering. The obligation that a pinned explanation is
/// narrated in the past.
pub const PHRASE_AS_OF: &str = "As of";

/// Confident attribution — reserved for [`BranchState::Resolved`], and the one
/// phrase in this module that asserts a claim actually came from something.
///
/// **It appears nowhere else, and that is load-bearing rather than tidy.** An
/// earlier draft also used it neutrally — *"so what it rested on is unknown"* —
/// which meant every `never_says(PHRASE_RESTED_ON)` in the suite was measuring
/// an overloaded token instead of the property it named. The combined-artifact
/// test found it, by being the first case where a `NotIndexed` node and a
/// `Dangling` branch appeared in one rendering.
///
/// The unknown-evidence sentences now say *"what it cited is unknown"*, which
/// keeps this constant a true marker of confident attribution.
pub const PHRASE_RESTED_ON: &str = "rested on";

/// The dangling clause. Never accompanied by [`PHRASE_RESTED_ON`].
pub const PHRASE_UNRESOLVED: &str = "could not be resolved to any recorded event";

/// The deletion qualification on a dangling citation.
pub const PHRASE_MAY_BE_DELETED: &str = "may have been deleted";

/// The plural clause — the refusal to choose, in words.
pub const PHRASE_CANNOT_SAY_WHICH: &str = "cannot say which";

/// A claim whose own record is gone.
pub const PHRASE_RECORD_DELETED: &str = "record has been deleted";

/// A claim the citation index does not cover.
pub const PHRASE_NOT_INDEXED: &str = "citations have not been indexed";

/// Truncation — a bound, and the admission that more exists.
pub const PHRASE_LIMIT_REACHED: &str = "a limit was reached";

/// More evidence exists beyond the bound.
pub const PHRASE_MORE_EVIDENCE: &str = "more evidence exists";

/// A cycle. Deliberately shares no wording with [`PHRASE_LIMIT_REACHED`]: the
/// two must not read alike, because one invites raising a limit and the other
/// says the evidence is malformed.
pub const PHRASE_CIRCULAR: &str = "refers back to itself";

/// The disclosure that compaction removed evidence.
pub const PHRASE_EVIDENCE_REMOVED: &str = "some evidence has been deleted";

/// The disclosure that the index does not cover part of the answer.
pub const PHRASE_COVERAGE_REDUCED: &str = "some citations have not been indexed";

/// Shown when there is nothing to explain.
pub const PHRASE_NOTHING_TO_EXPLAIN: &str = "There is nothing recorded to explain.";

/// A rendered explanation, as sentences.
///
/// Sentences rather than one string so a caller can speak them one at a time —
/// Rabbit's TTS path reads a line at a time — without re-splitting prose and
/// getting it wrong on an abbreviation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narration {
    /// The sentences, in reading order.
    pub sentences: Vec<String>,
}

impl Narration {
    /// The whole rendering as one string.
    #[must_use]
    pub fn text(&self) -> String {
        self.sentences.join(" ")
    }
}

/// **Render an explanation artifact as language.**
///
/// Total: every artifact renders, including an empty one and one whose branches
/// address nodes that are not there. Deterministic: the same artifact always
/// produces the same sentences, which is what lets the obligations be tested at
/// all.
#[must_use]
pub fn narrate(artifact: &ExplanationArtifact) -> Narration {
    let mut sentences = Vec::new();

    let Some(root) = artifact.root() else {
        return Narration {
            sentences: vec![PHRASE_NOTHING_TO_EXPLAIN.to_string()],
        };
    };

    // Tense first, and framed on the OPENING sentence rather than added as a
    // trailing caveat. A reader who has already been told a confident-sounding
    // present-tense fact does not retroactively re-hear it as historical.
    match &artifact.tense {
        Tense::Historical { pinned_at } => sentences.push(format!(
            "{PHRASE_AS_OF} {pinned_at}, the record said: {}.",
            root.claim
        )),
        Tense::Current => sentences.push(format!("The record says: {}.", root.claim)),
    }

    for node in &artifact.nodes {
        sentences.extend(narrate_node(node));
    }

    sentences.extend(disclosures(&artifact.completeness));

    Narration { sentences }
}

fn narrate_node(node: &ExplanationNode) -> Vec<String> {
    let mut out = Vec::new();
    match &node.evidence {
        NodeEvidence::DeletedByCompaction => out.push(format!(
            "For {}, the {PHRASE_RECORD_DELETED}, so what it cited is unknown.",
            node.claim
        )),
        NodeEvidence::NotIndexed => out.push(format!(
            "For {}, the {PHRASE_NOT_INDEXED}, so what it cited is unknown.",
            node.claim
        )),
        NodeEvidence::Recorded {
            branches,
            more_citations,
        } => {
            if branches.is_empty() {
                out.push(format!("{} cited nothing.", node.claim));
            }
            for branch in branches {
                out.push(match &branch.state {
                    // The ONLY confident attribution in this module.
                    BranchState::Resolved { evidence, digest } => {
                        format!("It {PHRASE_RESTED_ON} {evidence} ({digest}).")
                    }
                    BranchState::Plural {
                        cited,
                        carriers,
                        more_than_counted,
                    } => format!(
                        "It cited {cited}, which matches {carriers}{} recorded events, and Kirra \
                         {PHRASE_CANNOT_SAY_WHICH} of them it was.",
                        if *more_than_counted { " or more" } else { "" }
                    ),
                    BranchState::Dangling { cited, reason } => {
                        let mut s = format!("It cited {cited}, which {PHRASE_UNRESOLVED}");
                        if matches!(reason, DanglingReason::PossiblyDeleted) {
                            s.push_str(&format!("; it {PHRASE_MAY_BE_DELETED}"));
                        }
                        s.push('.');
                        s
                    }
                });
                if let BranchContinuation::Stopped(reason) = &branch.continuation {
                    if let Some(s) = stop_sentence(*reason) {
                        out.push(s);
                    }
                }
            }
            if *more_citations {
                out.push(format!(
                    "It cited more than is shown here, because {PHRASE_LIMIT_REACHED}; \
                     {PHRASE_MORE_EVIDENCE}."
                ));
            }
        }
    }
    out
}

fn stop_sentence(reason: StopReason) -> Option<String> {
    match reason {
        // Nothing to add: the branch state already said the citation dangles or
        // is plural, and a second sentence saying "so Kirra stopped" would
        // narrate the walk rather than the evidence.
        StopReason::NothingToFollow | StopReason::Ambiguous => None,
        StopReason::DepthLimit | StopReason::NodeLimit => Some(format!(
            "Kirra did not follow that further because {PHRASE_LIMIT_REACHED}; \
             {PHRASE_MORE_EVIDENCE}."
        )),
        StopReason::Cycle => Some(format!(
            "Kirra did not follow that further because the provenance {PHRASE_CIRCULAR}, \
             which is malformed."
        )),
    }
}

/// The closing caveats.
///
/// Emitted per flag rather than as one summary, because the flags are
/// independent and a summary would have to choose an order and then a winner —
/// the enum-precedence mistake box 4b avoided, arriving one layer later.
fn disclosures(c: &Completeness) -> Vec<String> {
    let mut out = Vec::new();
    if c.degraded {
        out.push(format!(
            "This explanation is incomplete: {PHRASE_EVIDENCE_REMOVED}."
        ));
    }
    if c.coverage_limited {
        out.push(format!(
            "This explanation is incomplete: {PHRASE_COVERAGE_REDUCED}."
        ));
    }
    if c.truncated {
        out.push(format!(
            "This explanation is incomplete because {PHRASE_LIMIT_REACHED}; \
             {PHRASE_MORE_EVIDENCE}."
        ));
    }
    if c.cycle_detected {
        out.push(format!(
            "Part of this provenance {PHRASE_CIRCULAR}, which is malformed."
        ));
    }
    out
}
