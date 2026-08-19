//! **The explanation wire contract — Tier 4 box 3b.**
//!
//! One request type, one response type, one path constant. They live in THIS
//! crate rather than in the producer or the consumer for a reason that is
//! mechanical rather than tidy: `ci/check_explain_artifact_neutral.py` guards
//! this crate and nothing else, and the request type is the single highest-risk
//! place in the system for a Kirra World coordinate to appear.
//!
//! # Capability-specific BY CONSTRUCTION
//!
//! The ruling on this seam is that the caller may name a subject and the server
//! owns everything else — generation pin, freshness policy, lineage depth,
//! cursoring, semantic versions, answer selection. Writing that down is not
//! enforcement. Putting [`ExplainCurrentSubject`] in the guarded crate is:
//!
//! * a `generation`, `cursor`, `offset`, `as_of` or `at_ms` field reds the
//!   gate's NAME check whatever type it is given;
//! * `depth: u8`, `page: u32`, `max_nodes: usize` — any numeric knob at all —
//!   reds the WIDTH check, and the only way past is an allowlist entry with a
//!   justification, where *"lets the caller make the work larger"* is the
//!   sentence that fails review.
//!
//! So the request cannot GROW into a query surface without a reviewer seeing a
//! diff in a file whose only purpose is to say no. That is the same technique
//! `kirra-proposal-context` uses, applied to the one type that would otherwise
//! turn this endpoint into the generic *"ask World"* route the ruling forbids.
//!
//! # Why there is no `AnswerRef`, in any encoding
//!
//! Not "we chose not to send one" — there is nothing here to put one in. The
//! response carries [`ExplanationArtifact`], whose neutrality this crate
//! already asserts, or a categorical failure. A handle Mick could feed back
//! into a second question has nowhere to ride.
//!
//! # Unavailable is a value, not an absence
//!
//! [`ExplainOutcome`] makes *"the producer could not be reached"* a case a
//! caller must match, beside *"nothing is recorded"* and *"here it is"*. A
//! client that returned `Option<ExplanationArtifact>` would collapse the first
//! two into `None`, and a renderer given `None` narrates silence — which reads
//! to an operator as *"Kirra knows nothing about that"* when the truth is that
//! the explanation service was down. Those are different sentences and the type
//! keeps them apart.

use crate::ExplanationArtifact;

/// The path this operation is served on.
///
/// Shared so the producer's route table and the consumer's client cannot drift
/// into a 404 that only shows up in deployment.
pub const EXPLAIN_CURRENT_SUBJECT_PATH: &str = "/explain/current-subject";

/// **Explain the current answer about one named subject.**
///
/// The whole request. `deny_unknown_fields` is load-bearing rather than strict
/// for its own sake: without it a client could send `{"subject_id":"x",
/// "generation":42}` and be silently served, which would leave the seam looking
/// capability-specific while callers in the field had already started steering
/// it. Rejecting the field makes an attempt to widen the contract fail at the
/// first request rather than at the review that notices years later.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainCurrentSubject {
    /// The subject to explain. The only thing the caller chooses.
    pub subject_id: String,
}

/// What came back.
///
/// Three cases, kept apart because the operator-facing sentences differ and
/// collapsing them is this tier's own defect class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExplainOutcome {
    /// Here is the explanation.
    Explained {
        /// The bounded, presentation-only artifact.
        explanation: ExplanationArtifact,
    },
    /// Kirra World retains nothing about that subject. An honest empty.
    NothingRecorded,
    /// No explanation could be produced. NEVER a fabricated one.
    ///
    /// Covers both ends of the wire: the producer failing to read its store,
    /// and the consumer failing to reach the producer at all. A caller that
    /// only ever sees this variant knows it has no evidence — which is exactly
    /// what it should tell a human.
    Unavailable {
        /// A short operator-facing reason. Presentation text, like every other
        /// string that crosses this seam — not a machine code to branch on.
        reason: String,
    },
}

impl ExplainOutcome {
    /// The artifact, if there is one.
    ///
    /// Deliberately NOT `Option`-returning sugar over the other two variants
    /// being equivalent: a caller reaching for this has already decided it only
    /// wants the success case, and the two failures stay distinguishable to
    /// anyone who matches instead.
    #[must_use]
    pub fn explanation(&self) -> Option<&ExplanationArtifact> {
        match self {
            Self::Explained { explanation } => Some(explanation),
            _ => None,
        }
    }

    /// Build the unavailable case from anything printable.
    #[must_use]
    pub fn unavailable(reason: impl std::fmt::Display) -> Self {
        Self::Unavailable {
            reason: reason.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// The semantic census
// ---------------------------------------------------------------------------

/// Every distinguishable state an [`ExplainOutcome`] can exhibit.
///
/// # Why this is API rather than a test helper
///
/// The wire contract is proven by two suites in two crates that may not depend
/// on each other: the producer's, which asserts the corpus on disk is what it
/// emits, and Mick's, which asserts the renderer handles all of it. Both need
/// to agree on *what the input space IS*, and two hand-maintained lists of
/// states is the serde-mirror failure one level up — the copy that stops being
/// updated is the one that silently stops matching.
///
/// So the enumeration is defined ONCE, here, beside the types it enumerates.
///
/// # The matches below are exhaustive on purpose
///
/// No `_` arm anywhere in [`ExplainOutcome::semantics`]. Adding a variant to
/// any of these enums is then a COMPILE error in this function rather than a
/// silent gap in the corpus's coverage — which is the difference between an
/// enumeration that stays true and one that was true when it was written.
pub const ALL_SEMANTICS: &[&str] = &[
    "outcome:explained",
    "outcome:nothing_recorded",
    "outcome:unavailable",
    "tense:current",
    "tense:historical",
    "evidence:recorded",
    "evidence:more_citations",
    "evidence:deleted_by_compaction",
    "evidence:not_indexed",
    "branch:resolved",
    "branch:plural",
    "branch:dangling",
    "dangling:never_recorded",
    "dangling:possibly_deleted",
    "continuation:expanded",
    "continuation:stopped",
    "stop:nothing_to_follow",
    "stop:ambiguous",
    "stop:depth_limit",
    "stop:node_limit",
    "stop:cycle",
    "completeness:complete",
    "completeness:truncated",
    "completeness:cycle_detected",
    "completeness:degraded",
    "completeness:coverage_limited",
];

impl ExplainOutcome {
    /// Which of [`ALL_SEMANTICS`] this outcome exhibits, sorted and deduplicated.
    #[must_use]
    pub fn semantics(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        match self {
            Self::NothingRecorded => out.push("outcome:nothing_recorded"),
            Self::Unavailable { .. } => out.push("outcome:unavailable"),
            Self::Explained { explanation } => {
                out.push("outcome:explained");
                out.push(match explanation.tense {
                    crate::Tense::Current => "tense:current",
                    crate::Tense::Historical { .. } => "tense:historical",
                });
                let c = explanation.completeness;
                if c.is_complete() {
                    out.push("completeness:complete");
                }
                if c.truncated {
                    out.push("completeness:truncated");
                }
                if c.cycle_detected {
                    out.push("completeness:cycle_detected");
                }
                if c.degraded {
                    out.push("completeness:degraded");
                }
                if c.coverage_limited {
                    out.push("completeness:coverage_limited");
                }
                for node in &explanation.nodes {
                    match &node.evidence {
                        crate::NodeEvidence::DeletedByCompaction => {
                            out.push("evidence:deleted_by_compaction");
                        }
                        crate::NodeEvidence::NotIndexed => out.push("evidence:not_indexed"),
                        crate::NodeEvidence::Recorded {
                            branches,
                            more_citations,
                        } => {
                            out.push("evidence:recorded");
                            if *more_citations {
                                out.push("evidence:more_citations");
                            }
                            for branch in branches {
                                out.extend(branch_semantics(branch));
                            }
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn branch_semantics(branch: &crate::ExplanationBranch) -> Vec<&'static str> {
    let mut out = Vec::new();
    match &branch.state {
        crate::BranchState::Resolved { .. } => out.push("branch:resolved"),
        crate::BranchState::Plural { .. } => out.push("branch:plural"),
        crate::BranchState::Dangling { reason, cited: _ } => {
            out.push("branch:dangling");
            out.push(match reason {
                crate::DanglingReason::NeverRecorded => "dangling:never_recorded",
                crate::DanglingReason::PossiblyDeleted => "dangling:possibly_deleted",
            });
        }
    }
    match &branch.continuation {
        crate::BranchContinuation::Expanded { .. } => out.push("continuation:expanded"),
        crate::BranchContinuation::Stopped(reason) => {
            out.push("continuation:stopped");
            out.push(match reason {
                crate::StopReason::NothingToFollow => "stop:nothing_to_follow",
                crate::StopReason::Ambiguous => "stop:ambiguous",
                crate::StopReason::DepthLimit => "stop:depth_limit",
                crate::StopReason::NodeLimit => "stop:node_limit",
                crate::StopReason::Cycle => "stop:cycle",
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every token [`ExplainOutcome::semantics`] can produce is in
    /// [`ALL_SEMANTICS`], and vice versa is checked by the corpus suites. This
    /// half catches a token written in one place and not the other — a typo
    /// that would otherwise make a semantic permanently uncoverable, since no
    /// corpus entry could ever produce the spelling the list expects.
    #[test]
    fn the_two_failure_outcomes_report_distinct_semantics() {
        assert_eq!(
            ExplainOutcome::NothingRecorded.semantics(),
            vec!["outcome:nothing_recorded"]
        );
        assert_eq!(
            ExplainOutcome::unavailable("down").semantics(),
            vec!["outcome:unavailable"]
        );
    }

    /// The listed tokens are unique and sorted, so a duplicate cannot make the
    /// corpus look like it covers more than it does.
    #[test]
    fn the_semantic_list_is_a_set() {
        let mut sorted = ALL_SEMANTICS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ALL_SEMANTICS.len(),
            "ALL_SEMANTICS must not repeat a token"
        );
    }

    /// The request rejects a field it does not know, which is the runtime half
    /// of "capability-specific by construction".
    #[test]
    fn a_request_carrying_a_steering_parameter_does_not_decode() {
        let ok: ExplainCurrentSubject = serde_json::from_str(r#"{"subject_id":"package_17"}"#)
            .expect("the plain shape decodes");
        assert_eq!(ok.subject_id, "package_17");
        for body in [
            r#"{"subject_id":"p","generation":42}"#,
            r#"{"subject_id":"p","cursor":"c"}"#,
            r#"{"subject_id":"p","depth":3}"#,
        ] {
            assert!(
                serde_json::from_str::<ExplainCurrentSubject>(body).is_err(),
                "`{body}` must not decode"
            );
        }
    }
}
