//! **kirra-mick** — the live LLM transport for Mick.
//!
//! [`OllamaClient`] is a [`kirra_planner::ModelClient`] that drives a LOCAL model
//! ([`DEFAULT_MODEL`] by default) served by [Ollama](https://ollama.com) over HTTP. Drop it into
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
/// Default model id (override with `KIRRA_MICK_MODEL`). A ~4B-class instruct model is a
/// good fit for the tight typed-intent vocabulary; pull it with `ollama pull phi3:3.8b`.
///
/// This is the SAME model the chat sidecar and the Rabbit speech layer default to:
/// one set of weights serves all three doer roles, so Ollama's
/// `OLLAMA_MAX_LOADED_MODELS=1` (see `robot/install/kirra-ollama-tuning.conf`) never
/// has to evict one role's model to serve another's. Vetted by
/// `robot/rabbit_model_smoketest.py`, which pins the weights' content digest.
pub const DEFAULT_MODEL: &str = "phi3:3.8b";
/// Per-request timeout. Mick is the slow System-2 loop, but a hung backend must NOT wedge
/// the planning tick — on timeout we fail closed and HOLD.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A [`ModelClient`] backed by a local Ollama server running [`DEFAULT_MODEL`] (or any
/// Ollama model).
pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::blocking::Client,
    persona: kirra_planner::Persona,
}

impl OllamaClient {
    /// Construct from the environment: `KIRRA_OLLAMA_URL` (default [`DEFAULT_OLLAMA_URL`])
    /// and `KIRRA_MICK_MODEL` (default [`DEFAULT_MODEL`]). Chauffeur persona.
    #[must_use]
    pub fn new() -> Self {
        let base_url =
            std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
        let model = std::env::var("KIRRA_MICK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self::with(base_url, model)
    }

    /// Construct with an explicit base URL + model id. Chauffeur persona.
    #[must_use]
    pub fn with(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http,
            persona: kirra_planner::Persona::Chauffeur,
        }
    }

    /// Set the persona — its constrained-decode schema gates the model's output tags (so a
    /// sidewalk courier is grammar-constrained to the sidewalk intents). Pair with
    /// `LlmBrain::courier(...)`, which selects the matching prompt.
    #[must_use]
    pub fn with_persona(mut self, persona: kirra_planner::Persona) -> Self {
        self.persona = persona;
        self
    }

    /// A sidewalk-courier Ollama client (constrained to sidewalk intents), from the environment.
    #[must_use]
    pub fn courier() -> Self {
        Self::new().with_persona(kirra_planner::Persona::SidewalkCourier)
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
    /// **schema** (`kirra_planner::intent_schema`), so the model can ONLY emit a JSON object
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
            format: self.persona.schema(),
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

// ---------------------------------------------------------------------------
// Conversational transport — deliberately SEPARATE from the motion path.
// ---------------------------------------------------------------------------

/// Default model id for the conversational endpoint (override with
/// `KIRRA_MICK_CHAT_MODEL`). Deliberately its own variable: the chat model can
/// be swapped without touching the motion model pin, and vice versa.
///
/// The DEFAULT is the same id as [`DEFAULT_MODEL`] — one model across all three
/// doer roles — but the separate variable is the point: an experiment on the
/// conversational surface must not silently become an experiment on the surface
/// that produces typed intents.
pub const DEFAULT_CHAT_MODEL: &str = "phi3:3.8b";

/// One message in an Ollama `/api/chat` conversation.
#[derive(Clone, Debug, Serialize)]
pub struct ChatWireMessage {
    /// `system` | `user` | `assistant`.
    pub role: &'static str,
    pub content: String,
}

/// Latency-bearing generation knobs, carried on every chat request.
///
/// - `keep_alive` holds the model RESIDENT between requests (Ollama's default
///   is ~5 minutes → the next turn pays a multi-second cold reload; the same
///   lever as the Rabbit path's `KIRRA_RABBIT_KEEP_ALIVE`).
/// - `num_predict` caps output tokens — a spoken reply should be one or two
///   short sentences, and every un-generated token is latency not spent.
/// - `temperature` low for consistent, terse conversational replies.
#[derive(Clone, Debug)]
pub struct ChatGenOptions {
    pub keep_alive: String,
    pub num_predict: u32,
    pub temperature: f64,
}

/// Ollama `/api/chat` request.
#[derive(Serialize)]
struct ChatRequestWire<'a> {
    model: &'a str,
    stream: bool,
    keep_alive: &'a str,
    options: ChatOptionsWire,
    messages: &'a [ChatWireMessage],
}

#[derive(Serialize)]
struct ChatOptionsWire {
    num_predict: u32,
    temperature: f64,
}

/// One NDJSON line of a streaming `/api/chat` response.
#[derive(Deserialize)]
struct ChatStreamLine {
    #[serde(default)]
    message: Option<ChatResponseMessage>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

/// Plain conversational Ollama transport for the `mick_chat_service` sidecar.
///
/// **NOT a [`ModelClient`], on purpose.** The motion transport
/// ([`OllamaClient`]) schema-constrains decoding to the typed-intent grammar —
/// exactly what a conversational reply must NOT be constrained to. Rather than
/// weaken or overload that abstraction, chat gets its own type and its own
/// wire shape (`/api/chat`, a messages array, **no `format` field**): free
/// text out, nothing downstream to parse it into an intent.
///
/// Same fail-closed posture as the motion transport: connection refused /
/// timeout / non-200 / undecodable / empty all collapse to a stable error
/// token, and the caller returns a chat error — never a fabricated reply.
pub struct OllamaChatClient {
    base_url: String,
    model: String,
    http: reqwest::blocking::Client,
}

impl OllamaChatClient {
    /// Construct from the environment: `KIRRA_OLLAMA_URL` (default
    /// [`DEFAULT_OLLAMA_URL`] — shared with the motion transport, one Ollama)
    /// and `KIRRA_MICK_CHAT_MODEL` (default [`DEFAULT_CHAT_MODEL`]).
    #[must_use]
    pub fn new() -> Self {
        let base_url =
            std::env::var("KIRRA_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
        let model = std::env::var("KIRRA_MICK_CHAT_MODEL")
            .unwrap_or_else(|_| DEFAULT_CHAT_MODEL.to_string());
        Self::with(base_url, model)
    }

    /// Construct with an explicit base URL + model id.
    #[must_use]
    pub fn with(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http,
        }
    }

    /// The configured model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// One STREAMING conversational completion. `on_delta` receives each
    /// incremental content fragment as it arrives off the wire (NDJSON lines);
    /// returning `false` CANCELS the generation — the response body is
    /// dropped, the connection closes, and Ollama aborts the remaining
    /// decode. Time-to-first-token is whenever the caller's first delta
    /// lands; this transport adds no buffering of its own.
    ///
    /// A completion that streams NOTHING before `done` is an error (an empty
    /// reply is never fabricated into a turn).
    pub fn stream_chat(
        &self,
        messages: &[ChatWireMessage],
        gen: &ChatGenOptions,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str> {
        use std::io::{BufRead, BufReader};
        let url = format!("{}/api/chat", self.base_url);
        let body = ChatRequestWire {
            model: &self.model,
            stream: true,
            keep_alive: &gen.keep_alive,
            options: ChatOptionsWire {
                num_predict: gen.num_predict,
                temperature: gen.temperature,
            },
            messages,
        };
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .map_err(|_| "MICK_CHAT_OLLAMA_REQUEST_FAILED")?;
        if !resp.status().is_success() {
            return Err("MICK_CHAT_OLLAMA_HTTP_STATUS");
        }
        let mut lines = BufReader::new(resp);
        let mut line = String::new();
        let mut streamed_any = false;
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(_) => return Err("MICK_CHAT_OLLAMA_STREAM_FAILED"),
            }
            if line.trim().is_empty() {
                continue;
            }
            let parsed: ChatStreamLine =
                serde_json::from_str(line.trim()).map_err(|_| "MICK_CHAT_OLLAMA_DECODE_FAILED")?;
            if let Some(m) = &parsed.message {
                if !m.content.is_empty() {
                    streamed_any = true;
                    if !on_delta(&m.content) {
                        return Ok(()); // caller cancelled; drop aborts Ollama
                    }
                }
            }
            if parsed.done {
                if !streamed_any {
                    return Err("MICK_CHAT_OLLAMA_EMPTY_COMPLETION");
                }
                return Ok(());
            }
        }
        // EOF before a `done` line: the stream was cut mid-generation.
        Err("MICK_CHAT_OLLAMA_STREAM_TRUNCATED")
    }
}

impl Default for OllamaChatClient {
    fn default() -> Self {
        Self::new()
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
        assert!(
            client.complete("hi").is_err(),
            "an unreachable Ollama must fail closed"
        );

        let mut mick = LlmBrain::new(OllamaClient::with("http://127.0.0.1:1", DEFAULT_MODEL));
        assert!(
            mick.decide(&ctx()).is_err(),
            "LlmBrain HOLDs when the model is unreachable"
        );
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
        assert_eq!(
            wire["format"]["type"], "object",
            "format is a schema object, not \"json\""
        );
        let tags = wire["format"]["properties"]["intent"]["enum"]
            .as_array()
            .expect("intent enum present");
        assert!(
            tags.iter().any(|t| t == "pull_over") && tags.iter().any(|t| t == "go_to"),
            "the constrained tag set is carried on the wire"
        );
    }

    // --- the conversational transport ---------------------------------------

    fn gen_opts() -> ChatGenOptions {
        ChatGenOptions {
            keep_alive: "30m".to_string(),
            num_predict: 48,
            temperature: 0.2,
        }
    }

    /// CI-safe: an unreachable Ollama fails the chat transport closed with a
    /// stable token — never a fabricated reply.
    #[test]
    fn unreachable_ollama_fails_chat_closed() {
        let client = OllamaChatClient::with("http://127.0.0.1:1", DEFAULT_CHAT_MODEL);
        let messages = [ChatWireMessage {
            role: "user",
            content: "hello".to_string(),
        }];
        let mut sink = |_: &str| true;
        assert_eq!(
            client.stream_chat(&messages, &gen_opts(), &mut sink),
            Err("MICK_CHAT_OLLAMA_REQUEST_FAILED")
        );
    }

    /// The chat wire shape: `/api/chat` messages, STREAMING, with the latency
    /// levers (keep_alive residency, num_predict output cap, temperature) and
    /// NO `format` field — the motion path's schema-constrained decoding must
    /// never leak into the conversational path. CI-safe: serializes the body
    /// and inspects the wire, no network.
    #[test]
    fn chat_request_carries_latency_levers_and_no_format_constraint() {
        let messages = [
            ChatWireMessage {
                role: "system",
                content: "you are conversational".to_string(),
            },
            ChatWireMessage {
                role: "user",
                content: "hi".to_string(),
            },
        ];
        let opts = gen_opts();
        let body = ChatRequestWire {
            model: "gemma3:4b",
            stream: true,
            keep_alive: &opts.keep_alive,
            options: ChatOptionsWire {
                num_predict: opts.num_predict,
                temperature: opts.temperature,
            },
            messages: &messages,
        };
        let wire: serde_json::Value = serde_json::to_value(&body).expect("body serializes");
        assert!(
            wire.get("format").is_none(),
            "chat must NOT schema-constrain decoding: {wire}"
        );
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["keep_alive"], "30m", "model residency lever missing");
        assert_eq!(wire["options"]["num_predict"], 48, "output cap missing");
        assert_eq!(wire["options"]["temperature"], 0.2);
        assert_eq!(wire["messages"][0]["role"], "system");
        assert_eq!(wire["messages"][1]["role"], "user");
    }

    /// Streaming parse over a canned NDJSON body, via a loopback TCP stub —
    /// deltas arrive incrementally, `done` ends cleanly, and cancellation
    /// (on_delta → false) stops consumption early. No Ollama needed.
    #[test]
    fn stream_chat_parses_ndjson_deltas_and_honours_cancellation() {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"lo. \"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"More.\"},\"done\":false}\n",
            "{\"done\":true}\n"
        );
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                // Drain the request head + body enough to reply.
                let _ = {
                    use std::io::Read;
                    let mut buf = [0u8; 4096];
                    s.read(&mut buf)
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        let client = OllamaChatClient::with(format!("http://{addr}"), "m");
        let messages = [ChatWireMessage {
            role: "user",
            content: "hi".to_string(),
        }];

        // Full consumption: all three deltas, clean end.
        let mut got = Vec::new();
        let mut sink = |d: &str| {
            got.push(d.to_string());
            true
        };
        client
            .stream_chat(&messages, &gen_opts(), &mut sink)
            .unwrap();
        assert_eq!(got, vec!["Hel", "lo. ", "More."]);

        // Cancellation after the first delta: consumption stops early, Ok.
        let mut seen = 0u32;
        let mut cancel_after_one = |_: &str| {
            seen += 1;
            false
        };
        client
            .stream_chat(&messages, &gen_opts(), &mut cancel_after_one)
            .unwrap();
        assert_eq!(seen, 1, "cancellation must stop consumption immediately");
        handle.join().unwrap();
    }

    /// Live round-trip — requires `ollama pull phi3:3.8b` and a running server. Ignored in
    /// CI; run locally with `cargo test -p kirra-mick -- --ignored`.
    #[test]
    #[ignore = "requires a local Ollama serving the configured model"]
    fn live_ollama_returns_a_typed_intent() {
        let mut mick = LlmBrain::new(OllamaClient::new());
        let intent = mick
            .decide(&ctx())
            .expect("a running Ollama should yield a typed intent");
        // Any of the typed intents is acceptable — we only assert it parsed to one.
        println!("live model chose: {intent:?}");
    }
}

pub mod explain_render;
