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
