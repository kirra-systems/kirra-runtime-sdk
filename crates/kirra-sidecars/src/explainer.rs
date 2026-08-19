//! **The Mick-adjacent EXPLAINER — the spoken end of the Kirra World
//! explanation seam.**
//!
//! Tier 4 box 3b. The sibling of [`crate::narrator`], and the shapes are
//! deliberately the same: a configured-or-503 read-only relay that turns a
//! remote fact into a sentence a robot can say. The narrator answers *"why did
//! the governor refuse?"*; this answers *"why do you think that?"*.
//!
//! ```text
//! POST /explain {"subject_id":".."}
//!        │
//!        ▼   kirra_mick::explain_client (HTTP, no World types)
//! world_explain_service ──▶ ExplanationArtifact
//!        │
//!        ▼   kirra_mick::explain_render
//!    sentences
//! ```
//!
//! # Read-only, and with no authority of any kind
//!
//! Nothing here can move anything. It holds a base URL, and the one operation
//! behind it returns resolved presentation text. That is worth stating because
//! this crate is FENCED — `ci/check_mick_actuation_fence.py` refuses any
//! dependency route from these binaries to actuation — and a new outbound HTTP
//! client is exactly the kind of addition that deserves to be checked against
//! that fence rather than assumed harmless. It is: the producer's whole route
//! table is one explanation and a health probe.
//!
//! # Unconfigured is a 503, not a silent success
//!
//! An unset URL means explanation is not deployed on this host. The endpoint
//! says so with `EXPLAINER_NOT_CONFIGURED` rather than answering *"there is
//! nothing to explain"*, which would be a claim about Kirra World's contents
//! made by a component that never asked it anything.

use kirra_mick::explain_client::{ExplainClient, EXPLAIN_URL_ENV};

/// Where the explanation producer is, e.g. `http://127.0.0.1:8120`.
///
/// Re-exported from the client so the sidecar and the client cannot come to
/// disagree about which variable configures this.
pub const EXPLAIN_SERVICE_URL_ENV: &str = EXPLAIN_URL_ENV;

/// Resolve the explainer from the environment.
///
/// `None` → not configured on this host; the route answers 503. There is no
/// half-configured case to abort on (unlike the narrator, which needs a URL
/// AND an auditor token) because the producer serves presentation-only text
/// and carries no credential.
#[must_use]
pub fn explainer_from_env() -> Option<ExplainClient> {
    ExplainClient::from_env()
}

/// Render one explanation request as the sidecar's JSON body.
///
/// Sentences, not one blob: Rabbit's TTS path reads a line at a time, and
/// re-splitting prose downstream is how an abbreviation becomes a pause in the
/// wrong place.
#[must_use]
pub fn explain_reply(client: &ExplainClient, subject_id: &str) -> String {
    let narration = client.narrate(subject_id);
    serde_json::json!({
        "subject_id": subject_id,
        "sentences": narration.sentences,
        "text": narration.text(),
    })
    .to_string()
}

/// The body for `GET`/`POST` when explanation is not configured.
#[must_use]
pub fn not_configured_reply() -> String {
    serde_json::json!({
        "error": "EXPLAINER_NOT_CONFIGURED",
        "detail": format!(
            "set {EXPLAIN_SERVICE_URL_ENV} to the Kirra World explanation \
             producer's base URL"
        ),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unreachable producer still SAYS something, and what it says is that
    /// it could not retrieve an explanation — never that there is nothing to
    /// explain. Port 1 has nothing listening, so this needs no fixture server.
    #[test]
    fn an_unreachable_producer_is_narrated_as_unavailable_not_as_no_record() {
        let client = ExplainClient::new("http://127.0.0.1:1");
        let body = explain_reply(&client, "package_17");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(v["subject_id"], "package_17");
        let text = v["text"].as_str().expect("text");
        assert!(
            text.contains("could not retrieve an explanation"),
            "must disclose the failure: {text}"
        );
        assert!(
            !text.contains(kirra_mick::explain_render::PHRASE_NOTHING_TO_EXPLAIN),
            "an unreachable producer must never claim Kirra World has no record: {text}"
        );
        assert!(
            v["sentences"].as_array().map(Vec::len).unwrap_or(0) >= 2,
            "the reason must survive as its own sentence: {body}"
        );
    }

    /// The unconfigured reply names the variable to set, so an operator is not
    /// left grepping for it.
    #[test]
    fn the_unconfigured_reply_names_the_variable_that_fixes_it() {
        let body = not_configured_reply();
        assert!(body.contains("EXPLAINER_NOT_CONFIGURED"), "{body}");
        assert!(body.contains(EXPLAIN_SERVICE_URL_ENV), "{body}");
    }
}
