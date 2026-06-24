//! **kirra-mick** — the live LLM transport for Mick.
//!
//! [`OllamaClient`] is a [`kirra_planner::ModelClient`] that drives a LOCAL model (Gemma
//! by default) served by [Ollama](https://ollama.com) over HTTP. Drop it into
//! `kirra_planner::LlmBrain` and Mick is driven by a real model:
//!
//! ```no_run
//! use kirra_mick::OllamaClient;
//! use kirra_planner::{LlmBrain, MickBrain, WorldContext};
//!
//! let mut mick = LlmBrain::new(OllamaClient::new()); // reads KIRRA_OLLAMA_URL / KIRRA_MICK_MODEL
//! let ctx = WorldContext { ego_speed_mps: 3.0, posture: "NOMINAL", goal_ahead_m: 40.0,
//!     goal_left_m: 0.0, may_change_left: true, may_change_right: false, objects: Vec::new(),
//!     available_turns: Vec::new() };
//! match mick.decide(&ctx) {
//!     Ok(intent) => { /* Occy grounds it, KIRRA bounds it */ }
//!     Err(_)     => { /* model unreachable / hallucinated → HOLD (fail-closed) */ }
//! }
//! ```
//!
//! **Safety is unchanged by the transport.** Every failure mode here — connection refused,
//! timeout, non-200, empty/garbage completion — returns a `ModelError`, so `LlmBrain`
//! fails closed and the caller HOLDs. The model can never reach the actuator: Occy grounds
//! the intent and KIRRA bounds it downstream. The transport just decides *whether there is
//! a fresh intent this tick*, never whether a command is safe.

use kirra_planner::{ModelClient, ModelError};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default Ollama base URL (override with `KIRRA_OLLAMA_URL`).
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Default model id (override with `KIRRA_MICK_MODEL`). A 4B-class instruct model is a
/// good fit for the tight typed-intent vocabulary; pull it with `ollama pull gemma3:4b`.
pub const DEFAULT_MODEL: &str = "gemma3:4b";
/// Per-request timeout. Mick is the slow System-2 loop, but a hung backend must NOT wedge
/// the planning tick — on timeout we fail closed and HOLD.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A [`ModelClient`] backed by a local Ollama server running Gemma (or any Ollama model).
pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::blocking::Client,
}

impl OllamaClient {
    /// Construct from the environment: `KIRRA_OLLAMA_URL` (default [`DEFAULT_OLLAMA_URL`])
    /// and `KIRRA_MICK_MODEL` (default [`DEFAULT_MODEL`]).
    #[must_use]
    pub fn new() -> Self {
        let base_url = std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
        let model = std::env::var("KIRRA_MICK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self::with(base_url, model)
    }

    /// Construct with an explicit base URL + model id.
    #[must_use]
    pub fn with(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { base_url: base_url.into(), model: model.into(), http }
    }

    /// The configured model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl Default for OllamaClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Ollama `/api/generate` request (non-streaming: one prompt → one completion).
#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    /// Schema-constrained decoding: Ollama grammar-constrains the output to this JSON
    /// **schema** (`kirra_planner::intent_schema`), so Gemma can ONLY emit a JSON object
    /// whose `intent` is a known tag — no prose, no code fence, no hallucinated tag. This
    /// is strictly tighter than the previous `"json"` (any-JSON) form. `from_llm_json`
    /// still validates the per-variant fields + finiteness on top (a schema cannot express
    /// finiteness, and the binding safety decision must stay in our fail-closed parse, not
    /// the model's decoder), so a schema-valid-but-wrong reply (e.g. `target_speed_mps` =
    /// `Inf`) still fails closed → HOLD.
    format: serde_json::Value,
}

/// The relevant slice of the `/api/generate` response.
#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

impl ModelClient for OllamaClient {
    fn complete(&self, prompt: &str) -> Result<String, ModelError> {
        let url = format!("{}/api/generate", self.base_url);
        let body = GenerateRequest {
            model: &self.model,
            prompt,
            stream: false,
            format: kirra_planner::intent_schema(),
        };

        // Every failure maps to a stable ModelError → LlmBrain fails closed → HOLD.
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .map_err(|_| "MICK_OLLAMA_REQUEST_FAILED")?;
        if !resp.status().is_success() {
            return Err("MICK_OLLAMA_HTTP_STATUS");
        }
        let parsed: GenerateResponse = resp.json().map_err(|_| "MICK_OLLAMA_DECODE_FAILED")?;
        if parsed.response.trim().is_empty() {
            return Err("MICK_OLLAMA_EMPTY_COMPLETION");
        }
        Ok(parsed.response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_planner::{LlmBrain, MickBrain, WorldContext};

    fn ctx() -> WorldContext {
        WorldContext {
            ego_speed_mps: 3.0,
            posture: "NOMINAL",
            goal_ahead_m: 40.0,
            goal_left_m: 0.0,
            may_change_left: true,
            may_change_right: false,
            objects: Vec::new(),
            available_turns: Vec::new(),
        }
    }

    /// CI-safe: with no Ollama listening, the transport fails closed (connection refused),
    /// and `LlmBrain` therefore HOLDs. No model required — this pins the fail-closed path.
    #[test]
    fn unreachable_ollama_fails_closed() {
        // Port 1 has nothing listening → immediate connection refusal.
        let client = OllamaClient::with("http://127.0.0.1:1", DEFAULT_MODEL);
        assert!(client.complete("hi").is_err(), "an unreachable Ollama must fail closed");

        let mut mick = LlmBrain::new(OllamaClient::with("http://127.0.0.1:1", DEFAULT_MODEL));
        assert!(mick.decide(&ctx()).is_err(), "LlmBrain HOLDs when the model is unreachable");
    }

    #[test]
    fn config_defaults_and_overrides() {
        let c = OllamaClient::with("http://example:1234", "llama3.2:3b");
        assert_eq!(c.model(), "llama3.2:3b");
    }

    /// The request sends the intent schema as Ollama's `format` (an OBJECT, not the string
    /// `"json"`), so decoding is schema-constrained. CI-safe — no network; just serializes
    /// the body and inspects the wire shape.
    #[test]
    fn request_body_carries_the_intent_schema_as_format() {
        let body = GenerateRequest {
            model: "gemma3:4b",
            prompt: "hi",
            stream: false,
            format: kirra_planner::intent_schema(),
        };
        let wire: serde_json::Value = serde_json::to_value(&body).expect("body serializes");
        assert_eq!(wire["format"]["type"], "object", "format is a schema object, not \"json\"");
        let tags = wire["format"]["properties"]["intent"]["enum"].as_array().expect("intent enum present");
        assert!(
            tags.iter().any(|t| t == "pull_over") && tags.iter().any(|t| t == "go_to"),
            "the constrained tag set is carried on the wire"
        );
    }

    /// Live round-trip — requires `ollama pull gemma3:4b` and a running server. Ignored in
    /// CI; run locally with `cargo test -p kirra-mick -- --ignored`.
    #[test]
    #[ignore = "requires a local Ollama serving the configured model"]
    fn live_ollama_returns_a_typed_intent() {
        let mut mick = LlmBrain::new(OllamaClient::new());
        let intent = mick.decide(&ctx()).expect("a running Ollama should yield a typed intent");
        // Any of the typed intents is acceptable — we only assert it parsed to one.
        println!("live Gemma chose: {intent:?}");
    }
}
