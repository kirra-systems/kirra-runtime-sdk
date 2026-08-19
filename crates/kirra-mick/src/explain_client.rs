//! **Mick's half of the explanation seam — Tier 4 box 3b.**
//!
//! `KIRRA-WM-EXPLAIN-PLACEMENT-001`:
//!
//! > Explanation is computed on the Kirra World side of a process boundary and
//! > exported as a bounded, immutable, presentation-only artifact through a
//! > World-independent transport/type contract. Mick renders that artifact but
//! > does not query Kirra World, resolve provenance, or alter its evidentiary
//! > meaning.
//!
//! This module is the "through a World-independent transport" clause, made
//! real. It holds a base URL and a subject name and it can produce nothing
//! else: there is no store handle here, no query type, no coordinate, and no
//! second request it could make with what came back.
//!
//! ```text
//! subject name ──HTTP──▶ world_explain_service ──▶ ExplanationArtifact
//!                                                        │
//!                                          explain_render::render_explanation
//!                                                        ▼
//!                                                    sentences
//! ```
//!
//! # Why the fetch and the rendering live together
//!
//! Because the CAPABILITY is "say why, out loud", and splitting it across
//! crates would leave `explain_render` reachable only from tests — which is
//! exactly the "tested but never invoked" state `ci/check_orphan_cores.py`
//! exists to catch. [`ExplainClient::narrate`] is the renderer's first and only
//! production caller, and that is deliberate rather than incidental.
//!
//! # Unavailable is never silence
//!
//! Every failure — unconfigured, unreachable, timed out, non-200 with an
//! undecodable body, garbage on the wire — becomes
//! [`ExplainOutcome::Unavailable`] carrying a reason. It never becomes an empty
//! artifact and never becomes `NothingRecorded`.
//!
//! That distinction is the whole point of the type. *"I have no record of
//! that"* and *"I could not reach the part of me that remembers"* are different
//! sentences, and an operator who hears the first when the second is true has
//! been told something false about Kirra World by a component with no authority
//! to say anything about it. So the client's failure path produces the
//! unavailable case, and the renderer is never handed something it could
//! narrate as evidence.
//!
//! # This is not a route to Kirra World
//!
//! `kirra-mick` depends on `kirra-explain-types` — which
//! `ci/check_explain_artifact_neutral.py` proves carries no World types and no
//! query handles — and on `reqwest`. It does not depend on `kirra-world*`, and
//! `ci/check_kirra_world_bidirectional_fence.py` is what keeps that true. A URL
//! is not a dependency: the producer answers one question and hands back
//! resolved text, so there is nothing here to ask a second question WITH.

use std::time::Duration;

use kirra_explain_types::{ExplainCurrentSubject, ExplainOutcome, EXPLAIN_CURRENT_SUBJECT_PATH};

use crate::explain_render::{render_explanation, Narration};

/// Base URL of the explanation producer, e.g. `http://127.0.0.1:8120`.
pub const EXPLAIN_URL_ENV: &str = "KIRRA_WORLD_EXPLAIN_URL";

/// Fetch timeout. An explanation is narration: a slow producer must not wedge
/// the caller's loop, and a timeout is an honest `Unavailable` rather than a
/// stall.
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// A client for the one explanation operation.
pub struct ExplainClient {
    base_url: String,
    http: reqwest::blocking::Client,
}

impl ExplainClient {
    /// Construct against an explicit base URL.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    /// Construct from [`EXPLAIN_URL_ENV`]. `None` → explanation is not
    /// configured on this host.
    ///
    /// `None` rather than a default URL: a default would make an unconfigured
    /// deployment indistinguishable from a broken one, and both would surface
    /// as connection-refused noise every time anyone asked why.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        match std::env::var(EXPLAIN_URL_ENV) {
            Ok(url) if !url.trim().is_empty() => Some(Self::new(url.trim())),
            _ => None,
        }
    }

    /// The configured base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// **Ask the producer to explain the current answer about `subject_id`.**
    ///
    /// Infallible by return type, which is the design: there is no `Err` a
    /// caller could handle differently from [`ExplainOutcome::Unavailable`],
    /// and a `Result` would let a caller `unwrap_or_default()` its way to a
    /// fabricated empty explanation.
    #[must_use]
    pub fn explain(&self, subject_id: &str) -> ExplainOutcome {
        let url = format!(
            "{}{EXPLAIN_CURRENT_SUBJECT_PATH}",
            self.base_url.trim_end_matches('/')
        );
        let request = ExplainCurrentSubject {
            subject_id: subject_id.to_string(),
        };
        let response = match self.http.post(&url).json(&request).send() {
            Ok(r) => r,
            Err(_) => return ExplainOutcome::unavailable("the explanation service is unreachable"),
        };
        // A non-200 STILL carries an outcome when the producer wrote one — 503
        // with an `Unavailable` body is the producer telling us why, and
        // discarding it to substitute our own guess would lose the only
        // diagnosis anyone has. Only an undecodable body falls back.
        let status = response.status();
        match response.json::<ExplainOutcome>() {
            Ok(outcome) => outcome,
            Err(_) => ExplainOutcome::unavailable(format!(
                "the explanation service answered {status} with a body this \
                 client could not decode"
            )),
        }
    }

    /// **Ask, and render the answer as sentences.**
    ///
    /// The whole capability in one call, and the renderer's production caller.
    /// A caller that wants the artifact itself uses [`Self::explain`]; a caller
    /// that wants to SAY something uses this.
    ///
    /// Every case yields sentences, including both failures — a spoken surface
    /// that goes silent when Kirra World is unreachable has told the operator
    /// nothing, and *"nothing"* is indistinguishable from *"nothing is wrong"*.
    #[must_use]
    pub fn narrate(&self, subject_id: &str) -> Narration {
        Self::narrate_outcome(&self.explain(subject_id))
    }

    /// Render an outcome. Split out from [`Self::narrate`] so the mapping from
    /// the three cases to language is testable without a socket, and so the
    /// two failure sentences are written once.
    #[must_use]
    pub fn narrate_outcome(outcome: &ExplainOutcome) -> Narration {
        match outcome {
            ExplainOutcome::Explained { explanation } => render_explanation(explanation),
            // Deliberately the renderer's own phrase for an empty artifact:
            // "nothing is recorded" is one statement, and it should not have
            // two spellings depending on whether the emptiness was noticed on
            // this side of the wire or the other.
            ExplainOutcome::NothingRecorded => Narration {
                sentences: vec![crate::explain_render::PHRASE_NOTHING_TO_EXPLAIN.to_string()],
            },
            // NOT the phrase above. An operator must be able to tell "Kirra
            // World has no record" from "I could not ask Kirra World", and the
            // reason is carried so the second sentence is diagnosable.
            ExplainOutcome::Unavailable { reason } => Narration {
                sentences: vec![
                    "I could not retrieve an explanation, so I cannot say what this rests on."
                        .to_string(),
                    format!("The reason given was: {reason}."),
                ],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI-safe: nothing is listening on port 1, so the client fails closed with
    /// the unavailable case — never an empty artifact, and never the
    /// nothing-recorded sentence.
    #[test]
    fn an_unreachable_producer_is_unavailable_and_not_an_empty_explanation() {
        let client = ExplainClient::new("http://127.0.0.1:1");
        let outcome = client.explain("package_17");
        assert!(
            matches!(outcome, ExplainOutcome::Unavailable { .. }),
            "{outcome:?}"
        );
        assert!(outcome.explanation().is_none());
        assert_ne!(outcome, ExplainOutcome::NothingRecorded);
    }

    /// The two failures say DIFFERENT things out loud. This is the sentence-level
    /// version of the type-level distinction, and it is the one an operator
    /// actually hears.
    #[test]
    fn unavailable_and_nothing_recorded_are_not_the_same_sentence() {
        let unavailable = ExplainClient::narrate_outcome(&ExplainOutcome::unavailable("down"));
        let empty = ExplainClient::narrate_outcome(&ExplainOutcome::NothingRecorded);
        assert_ne!(unavailable.text(), empty.text());
        assert!(
            !unavailable
                .text()
                .contains(crate::explain_render::PHRASE_NOTHING_TO_EXPLAIN),
            "an unreachable producer must never claim Kirra World has no record: {}",
            unavailable.text()
        );
        assert!(
            unavailable.text().contains("down"),
            "the reason must survive into the spoken line: {}",
            unavailable.text()
        );
        assert_eq!(
            empty.text(),
            crate::explain_render::PHRASE_NOTHING_TO_EXPLAIN
        );
    }

    /// A URL is normalised once, so a trailing slash in configuration does not
    /// become a 404 that only appears on someone's robot.
    #[test]
    fn a_trailing_slash_in_the_configured_url_is_tolerated() {
        assert_eq!(
            ExplainClient::new("http://h:1/")
                .base_url()
                .trim_end_matches('/'),
            "http://h:1"
        );
    }
}
