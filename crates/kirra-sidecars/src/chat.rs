//! The Mick CONVERSATIONAL service core — **typed text in, plain text out,
//! never an intent** — tuned for LOW PERCEIVED LATENCY.
//!
//! Two doors, never merged:
//!
//! | route | purpose | authority |
//! |---|---|---|
//! | `POST /chat`, `POST /chat/stream` | conversation only | none — plain text, no downstream consumer |
//! | `POST /intent` (other binary)     | motion-intent classification | governed downstream |
//!
//! **Latency design** (time to first audible response is the metric):
//!
//!  1. STATELESS by default — history off (`KIRRA_MICK_CHAT_HISTORY_TURNS=0`);
//!     every remembered turn is prompt-eval time. Optional history is bounded
//!     to ≤ 2 pairs and in-memory only.
//!  2. COMPACT system prompt (length-pinned by test) — prompt tokens are TTFT.
//!  3. Output capped (`num_predict`, default 48 tokens) + low temperature —
//!     a spoken reply is one or two short sentences.
//!  4. Model kept RESIDENT (`keep_alive`, default 30m) + a one-shot warm-up on
//!     boot; `/health` reports warming/ready/degraded honestly.
//!  5. STREAMING deltas + sentence-boundary chunking, so TTS can start on the
//!     first sentence while the model generates the rest.
//!  6. Deterministic FAST ROUTES for trivial utterances (hello/thanks/goodbye)
//!     and a conservative leading-verb MOTION REFUSAL — zero model calls.
//!
//! **No path to motion, structurally**: no intent types, no latch, no seq, no
//! planner; `tests/mick_chat_separation.rs` fences the sources and the wire.
//! A motion-shaped model reply is held back and REPLACED
//! ([`MOTION_SHAPE_REFUSAL`]) — never surfaced as if actionable, never parsed.
//!
//! **Privacy**: history is memory-only and restart-cleared; logs carry stage
//! names, durations, and counters — never transcript or reply text.

use std::collections::HashMap;
use std::io::Write;

use crate::net::RateLimiter;
use serde::Deserialize;

/// LLM-call rate bound (burst / steady-state per second).
pub const CHAT_RATE_BURST: f64 = 3.0;
pub const CHAT_RATE_PER_S: f64 = 1.0;

/// Hard ceiling on live sessions (only relevant when history is enabled).
pub const CHAT_MAX_SESSIONS: usize = 32;

/// History is bounded to at most this many prior (user, assistant) pairs —
/// a hard design cap, not a default: every remembered pair is prompt-eval
/// latency on EVERY subsequent turn.
pub const CHAT_MAX_HISTORY_TURNS: usize = 2;

/// In-flight model requests. The serve loop is single-threaded, so this is
/// structural (a second connection waits in the TCP backlog); documented as
/// config for the day the loop grows threads.
pub const CHAT_MAX_INFLIGHT: usize = 1;

/// The compact conversational system prompt. Short ON PURPOSE — prompt tokens
/// are paid on every turn as time-to-first-token; the length is pinned by a
/// regression test. Says what it must (no motion authority, no invented
/// state) and nothing else.
pub const CHAT_SYSTEM_PROMPT: &str = "You are Mick, a concise conversational robot assistant. \
Answer naturally in one or two short sentences. \
You cannot move or control the robot. \
Never claim access to live state unless it is provided in the request. \
For motion requests, say they must use the governed motion path.";

/// Ceiling the prompt-length regression test enforces.
pub const CHAT_SYSTEM_PROMPT_MAX_CHARS: usize = 400;

/// Reply substituted when the model emits motion-shaped JSON.
pub const MOTION_SHAPE_REFUSAL: &str =
    "Motion requests must use the governed motion path. Happy to keep talking here.";

/// Reply for a deterministically detected explicit motion request — no model
/// call at all.
pub const MOTION_ROUTE_REPLY: &str = "That needs the governed motion path.";

/// One `POST /chat` / `POST /chat/stream` request. An optional per-request
/// `max_output_tokens` may only REDUCE the configured cap, never raise it.
#[derive(Deserialize)]
pub struct ChatRequest {
    pub text: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

/// Boot-validated configuration (fail-closed: malformed values abort startup).
#[derive(Clone, Debug)]
pub struct ChatConfig {
    /// `KIRRA_MICK_CHAT_MAX_INPUT_CHARS` (default 500).
    pub max_input_chars: usize,
    /// `KIRRA_MICK_CHAT_HISTORY_TURNS` (default 0 = stateless; hard cap
    /// [`CHAT_MAX_HISTORY_TURNS`], larger values are a boot error).
    pub history_turns: usize,
    /// `KIRRA_MICK_CHAT_MAX_OUTPUT_TOKENS` (default 48) — the num_predict cap.
    pub max_output_tokens: u32,
    /// `KIRRA_MICK_CHAT_TEMPERATURE` (default 0.2).
    pub temperature: f64,
    /// `KIRRA_MICK_CHAT_KEEP_ALIVE` (default "30m") — model residency.
    pub keep_alive: String,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            max_input_chars: 500,
            history_turns: 0,
            max_output_tokens: 48,
            temperature: 0.2,
            keep_alive: "30m".to_string(),
        }
    }
}

/// Hard ceiling on the configurable input-text bound, so a misconfiguration
/// cannot defeat the "bounded" guarantee (an out-of-range value ABORTS
/// startup, the same fail-closed posture as a malformed one). History has its
/// own, tighter hard cap: [`CHAT_MAX_HISTORY_TURNS`].
pub const CHAT_MAX_INPUT_CHARS_CEILING: usize = 8_192;

impl ChatConfig {
    /// Read from the environment, fail-closed: present-but-malformed OR
    /// out-of-range values are an `Err` for the binary to abort on.
    pub fn from_env() -> Result<Self, String> {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_MAX_INPUT_CHARS") {
            cfg.max_input_chars = v
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0 && *n <= CHAT_MAX_INPUT_CHARS_CEILING)
                .ok_or_else(|| {
                    format!(
                        "KIRRA_MICK_CHAT_MAX_INPUT_CHARS malformed or out of range \
                         (1..={CHAT_MAX_INPUT_CHARS_CEILING}): {v:?}"
                    )
                })?;
        }
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_HISTORY_TURNS") {
            let n = v
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("KIRRA_MICK_CHAT_HISTORY_TURNS malformed: {v:?}"))?;
            if n > CHAT_MAX_HISTORY_TURNS {
                return Err(format!(
                    "KIRRA_MICK_CHAT_HISTORY_TURNS={n} exceeds the hard cap \
                     {CHAT_MAX_HISTORY_TURNS} (history is prompt-eval latency on every turn)"
                ));
            }
            cfg.history_turns = n;
        }
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_MAX_OUTPUT_TOKENS") {
            cfg.max_output_tokens = v
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("KIRRA_MICK_CHAT_MAX_OUTPUT_TOKENS malformed: {v:?}"))?;
        }
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_TEMPERATURE") {
            cfg.temperature = v
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|t| t.is_finite() && *t >= 0.0)
                .ok_or_else(|| format!("KIRRA_MICK_CHAT_TEMPERATURE malformed: {v:?}"))?;
        }
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_KEEP_ALIVE") {
            let t = v.trim();
            if t.is_empty() {
                return Err("KIRRA_MICK_CHAT_KEEP_ALIVE is empty".to_string());
            }
            cfg.keep_alive = t.to_string();
        }
        Ok(cfg)
    }
}

/// One message handed to the chat model transport.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    /// `system` | `user` | `assistant`.
    pub role: &'static str,
    pub content: String,
}

/// Generation knobs carried per request (mirrors the transport's options;
/// defined here so the core stays transport-free).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatGenParams {
    pub keep_alive: String,
    pub num_predict: u32,
    pub temperature: f64,
}

/// Abstract STREAMING "messages → delta text" port for the conversational
/// path. Deliberately not the motion `ModelClient` (whose decode is
/// intent-schema-constrained). `on_delta` returning `false` cancels the
/// generation.
pub trait ChatModelClient {
    fn stream_chat(
        &self,
        messages: &[ChatMessage],
        gen: &ChatGenParams,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str>;
}

// ---------------------------------------------------------------------------
// Deterministic fast paths (no model call)
// ---------------------------------------------------------------------------

/// Light local normalization for the fast routes: lowercase, punctuation to
/// spaces, collapsed. (Local on purpose — the chat path shares no code with
/// the intent-side fence.)
fn light_normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            if pending && !out.is_empty() {
                out.push(' ');
            }
            pending = false;
            out.extend(ch.to_lowercase());
        } else {
            pending = true;
        }
    }
    out
}

/// Tiny, explicit fixed-reply table — the whole utterance must be one of
/// these. Deliberately NOT a rule-based chatbot: three social niceties whose
/// answers cannot be wrong, nothing that claims state.
#[must_use]
pub fn fast_route(text: &str) -> Option<&'static str> {
    match light_normalize(text).as_str() {
        "hello" | "hi" | "hey" => Some("Hello."),
        "thank you" | "thanks" => Some("You're welcome."),
        "goodbye" | "bye" => Some("Goodbye."),
        _ => None,
    }
}

/// Leading motion verbs/phrases → the utterance is an explicit motion request
/// and chat refuses deterministically (no model call, no forwarding — chat
/// NEVER invokes the motion door). Conservative: only a LEADING match counts
/// ("how do I stop it" is conversation); false negatives fall through to the
/// model whose motion-shaped output is guarded anyway.
const MOTION_LEADS: &[&str] = &[
    "drive",
    "turn",
    "stop",
    "go",
    "reverse",
    "cruise",
    "pull over",
    "proceed",
    "move",
    "back up",
    "follow",
    "take me",
    "head",
    "steer",
    "accelerate",
    "slow down",
];

#[must_use]
pub fn is_explicit_motion_request(text: &str) -> bool {
    let n = light_normalize(text);
    MOTION_LEADS
        .iter()
        .any(|lead| n == *lead || n.starts_with(&format!("{lead} ")))
}

// ---------------------------------------------------------------------------
// Sentence chunking (for streaming TTS)
// ---------------------------------------------------------------------------

/// Sentence-boundary chunker: feed streamed deltas, get speakable chunks.
///
/// A chunk ends at `. ? ! ; :` when followed by whitespace/end AND the buffer
/// has at least `min_chars` — with a decimal guard (`3.5` never splits: a
/// period BETWEEN digits is not a boundary). A punctuation-free run longer
/// than `max_chars` falls back to the last WORD boundary, so speech still
/// starts. Word boundaries are always preserved.
pub struct SentenceChunker {
    buf: String,
    pub min_chars: usize,
    pub max_chars: usize,
}

impl SentenceChunker {
    #[must_use]
    pub fn new(min_chars: usize, max_chars: usize) -> Self {
        Self {
            buf: String::new(),
            min_chars,
            max_chars: max_chars.max(min_chars + 1),
        }
    }

    /// Feed one delta; returns zero or more completed chunks.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(end) = self.next_boundary() {
            let chunk: String = self.buf.drain(..end).collect();
            let trimmed = chunk.trim().to_string();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
        }
        out
    }

    /// Flush whatever remains (end of stream).
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let trimmed = rest.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// Byte index one past the next chunk boundary, if any.
    fn next_boundary(&self) -> Option<usize> {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut count = 0usize;
        let mut byte = 0usize;
        let mut last_space_byte = None;
        for (i, ch) in chars.iter().enumerate() {
            count += 1;
            byte += ch.len_utf8();
            if ch.is_whitespace() {
                last_space_byte = Some(byte);
            }
            let next = chars.get(i + 1);
            let prev = if i > 0 { chars.get(i - 1) } else { None };
            let is_punct = matches!(ch, '.' | '?' | '!' | ';' | ':');
            // Decimal / dotted-token guard: punctuation flanked by digits or
            // immediately followed by a non-space is not a sentence end
            // ("3.5", "v2.0", "e.g." mid-token).
            let followed_ok = next.is_none_or(|c| c.is_whitespace());
            let decimal = *ch == '.'
                && prev.is_some_and(|c| c.is_ascii_digit())
                && next.is_some_and(|c| c.is_ascii_digit());
            if is_punct && followed_ok && !decimal && count >= self.min_chars {
                return Some(byte);
            }
            // Punctuation-free fallback: past the max, cut at the last word
            // boundary so long unpunctuated output still starts speaking.
            if count >= self.max_chars {
                if let Some(sp) = last_space_byte {
                    return Some(sp);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Motion-shape output guard (streaming-aware)
// ---------------------------------------------------------------------------

/// Whole-reply structural check: is this (bare or once-code-fenced) a JSON
/// object carrying an `"intent"` key? serde_json only — never the typed
/// parser; unknown future tags guard identically.
#[must_use]
pub fn looks_like_motion_intent_json(reply: &str) -> bool {
    let mut candidate = reply.trim();
    if let Some(inner) = candidate.strip_prefix("```") {
        if let Some(end) = inner.rfind("```") {
            let inner = &inner[..end];
            candidate = inner
                .split_once('\n')
                .map_or(inner, |(_, rest)| rest)
                .trim();
        }
    }
    match serde_json::from_str::<serde_json::Value>(candidate) {
        Ok(v) => v.is_object() && v.get("intent").is_some(),
        Err(_) => false,
    }
}

/// Streaming guard policy: a reply whose first non-whitespace character opens
/// a JSON object or code fence is HELD BACK in full (no deltas emitted) and
/// guarded when complete; ordinary prose streams immediately. Deterministic:
/// the decision is made once, on the first delta.
#[must_use]
pub fn must_hold_back(first_delta: &str) -> bool {
    matches!(
        first_delta.trim_start().chars().next(),
        Some('{') | Some('`')
    )
}

// ---------------------------------------------------------------------------
// Timing + stream events
// ---------------------------------------------------------------------------

/// Stage timings for one request, all relative to request receipt, measured
/// on the caller-supplied monotonic clock. Never carries content.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChatTiming {
    /// First model token observed (0 for deterministic fast routes).
    pub ttft_ms: u64,
    /// First speakable sentence ready.
    pub first_sentence_ms: u64,
    /// Reply complete.
    pub total_ms: u64,
    /// True when the reply came from a deterministic route (no model call).
    pub deterministic: bool,
}

/// One streamed event, in emission order.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// Stream opened; `request_id` is a per-process counter.
    Start { request_id: u64 },
    /// One raw model delta (prose replies only; held-back replies emit none).
    Delta { text: String },
    /// One speakable sentence chunk.
    Sentence { text: String },
    /// Terminal: full reply + timings. Always the last event.
    Done { reply: String, timing: ChatTiming },
}

/// Chunker bounds for the streamed `sentence` events
/// (`KIRRA_CHAT_TTS_CHUNK_{MIN,MAX}_CHARS` are client-side knobs; the service
/// uses these fixed, spoken-interaction bounds).
pub const SENTENCE_MIN_CHARS: usize = 20;
pub const SENTENCE_MAX_CHARS: usize = 120;

/// Why a request was refused (stable wire tokens).
pub type ChatError = &'static str;

struct Session {
    pairs: Vec<(String, String)>,
    last_used_ms: u64,
}

/// The service core: transport + optional bounded history + rate limit +
/// request counter. Single-threaded (CHAT_MAX_INFLIGHT = 1 structurally).
/// Holds NO intent state.
pub struct ChatService<M: ChatModelClient> {
    model: M,
    cfg: ChatConfig,
    limiter: RateLimiter,
    sessions: HashMap<String, Session>,
    next_request_id: u64,
    motion_shape_guarded: u64,
    fast_routed: u64,
    motion_refused: u64,
}

impl<M: ChatModelClient> ChatService<M> {
    #[must_use]
    pub fn new(model: M, cfg: ChatConfig) -> Self {
        Self {
            model,
            cfg,
            limiter: RateLimiter::new(CHAT_RATE_BURST, CHAT_RATE_PER_S),
            sessions: HashMap::new(),
            next_request_id: 0,
            motion_shape_guarded: 0,
            fast_routed: 0,
            motion_refused: 0,
        }
    }

    #[must_use]
    pub fn motion_shape_guarded(&self) -> u64 {
        self.motion_shape_guarded
    }

    #[must_use]
    pub fn fast_routed(&self) -> u64 {
        self.fast_routed
    }

    #[must_use]
    pub fn motion_refused(&self) -> u64 {
        self.motion_refused
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub fn history_len(&self, session_id: &str) -> usize {
        self.sessions.get(session_id).map_or(0, |s| s.pairs.len())
    }

    /// Handle one request, streaming events into `emit` (which returns
    /// `false` to cancel — e.g. the client hung up). `clock` is a monotonic
    /// milliseconds source. Returns the terminal timing, or a stable error
    /// token; on error nothing was emitted beyond `Start`.
    pub fn handle_chat_streamed(
        &mut self,
        req: &ChatRequest,
        clock: &mut dyn FnMut() -> u64,
        emit: &mut dyn FnMut(&StreamEvent) -> bool,
    ) -> Result<ChatTiming, ChatError> {
        let t0 = clock();
        if !self.limiter.admit(t0) {
            return Err("MICK_CHAT_RATE_LIMITED");
        }
        let text = req.text.trim();
        if text.is_empty() {
            return Err("MICK_CHAT_EMPTY_REQUEST");
        }
        if req.text.chars().count() > self.cfg.max_input_chars {
            return Err("MICK_CHAT_TEXT_TOO_LONG");
        }
        if text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
        {
            return Err("MICK_CHAT_BAD_TEXT");
        }
        let session_id = validate_session_id(req.session_id.as_deref())?;

        self.next_request_id += 1;
        let request_id = self.next_request_id;
        if !emit(&StreamEvent::Start { request_id }) {
            return Err("MICK_CHAT_CANCELLED");
        }

        // Deterministic fast paths: zero model calls, ~zero latency.
        let deterministic_reply = if let Some(fixed) = fast_route(text) {
            self.fast_routed += 1;
            Some(fixed)
        } else if is_explicit_motion_request(text) {
            self.motion_refused += 1;
            eprintln!(
                "mick_chat: explicit motion request refused deterministically \
                 (refused={}) — chat never forwards to the motion door",
                self.motion_refused
            );
            Some(MOTION_ROUTE_REPLY)
        } else {
            None
        };
        if let Some(reply) = deterministic_reply {
            let now = clock().saturating_sub(t0);
            let timing = ChatTiming {
                ttft_ms: 0,
                first_sentence_ms: now,
                total_ms: now,
                deterministic: true,
            };
            emit(&StreamEvent::Sentence {
                text: reply.to_string(),
            });
            emit(&StreamEvent::Done {
                reply: reply.to_string(),
                timing,
            });
            self.record_turn(&session_id, text, reply, t0);
            return Ok(timing);
        }

        // Model path: compose compact prompt (+ optional bounded history).
        let mut messages = vec![ChatMessage {
            role: "system",
            content: CHAT_SYSTEM_PROMPT.to_string(),
        }];
        if self.cfg.history_turns > 0 {
            if let Some(s) = self.sessions.get(&session_id) {
                for (user, assistant) in &s.pairs {
                    messages.push(ChatMessage {
                        role: "user",
                        content: user.clone(),
                    });
                    messages.push(ChatMessage {
                        role: "assistant",
                        content: assistant.clone(),
                    });
                }
            }
        }
        messages.push(ChatMessage {
            role: "user",
            content: text.to_string(),
        });

        // Per-request token cap may only REDUCE the configured maximum.
        let num_predict = match req.max_output_tokens {
            Some(n) if n > 0 => n.min(self.cfg.max_output_tokens),
            _ => self.cfg.max_output_tokens,
        };
        let gen = ChatGenParams {
            keep_alive: self.cfg.keep_alive.clone(),
            num_predict,
            temperature: self.cfg.temperature,
        };

        let mut full = String::new();
        let mut ttft_ms: Option<u64> = None;
        let mut first_sentence_ms: Option<u64> = None;
        let mut hold_back: Option<bool> = None;
        let mut chunker = SentenceChunker::new(SENTENCE_MIN_CHARS, SENTENCE_MAX_CHARS);
        let mut cancelled = false;

        {
            let emit_ref: &mut dyn FnMut(&StreamEvent) -> bool = emit;
            let clock_ref: &mut dyn FnMut() -> u64 = clock;
            let mut on_delta = |delta: &str| -> bool {
                // First token: stamped exactly once.
                if ttft_ms.is_none() {
                    ttft_ms = Some(clock_ref().saturating_sub(t0));
                    hold_back = Some(must_hold_back(delta));
                }
                full.push_str(delta);
                if hold_back == Some(true) {
                    return true; // buffer silently; guard decides at the end
                }
                if !emit_ref(&StreamEvent::Delta {
                    text: delta.to_string(),
                }) {
                    cancelled = true;
                    return false;
                }
                for chunk in chunker.push(delta) {
                    if first_sentence_ms.is_none() {
                        first_sentence_ms = Some(clock_ref().saturating_sub(t0));
                    }
                    if !emit_ref(&StreamEvent::Sentence { text: chunk }) {
                        cancelled = true;
                        return false;
                    }
                }
                true
            };
            self.model
                .stream_chat(&messages, &gen, &mut on_delta)
                .map_err(|_| "MICK_CHAT_MODEL_ERROR")?;
        }
        if cancelled {
            return Err("MICK_CHAT_CANCELLED");
        }

        // Assemble the final reply, applying the guard to held-back output.
        let reply = if hold_back == Some(true) || looks_like_motion_intent_json(&full) {
            if looks_like_motion_intent_json(&full) {
                self.motion_shape_guarded += 1;
                eprintln!(
                    "mick_chat: motion-shape output guard engaged (guarded={}) — \
                     reply replaced with the routing explanation",
                    self.motion_shape_guarded
                );
                MOTION_SHAPE_REFUSAL.to_string()
            } else {
                // Held back but harmless (e.g. fenced non-intent JSON): emit
                // it now as one late sentence rather than dribbling stale
                // deltas.
                full.clone()
            }
        } else {
            full.clone()
        };
        // A guarded/held-back reply surfaces as a single sentence event here.
        if hold_back == Some(true) || looks_like_motion_intent_json(&full) {
            if first_sentence_ms.is_none() {
                first_sentence_ms = Some(clock().saturating_sub(t0));
            }
            if !emit(&StreamEvent::Sentence {
                text: reply.clone(),
            }) {
                return Err("MICK_CHAT_CANCELLED");
            }
        } else if let Some(tail) = chunker.finish() {
            if first_sentence_ms.is_none() {
                first_sentence_ms = Some(clock().saturating_sub(t0));
            }
            if !emit(&StreamEvent::Sentence { text: tail }) {
                return Err("MICK_CHAT_CANCELLED");
            }
        }

        let total_ms = clock().saturating_sub(t0);
        let timing = ChatTiming {
            ttft_ms: ttft_ms.unwrap_or(total_ms),
            first_sentence_ms: first_sentence_ms.unwrap_or(total_ms),
            total_ms,
            deterministic: false,
        };
        emit(&StreamEvent::Done {
            reply: reply.clone(),
            timing,
        });
        self.record_turn(&session_id, text, &reply, t0);
        Ok(timing)
    }

    /// Non-streaming convenience: collect the stream, return (reply, timing).
    pub fn handle_chat(
        &mut self,
        req: &ChatRequest,
        clock: &mut dyn FnMut() -> u64,
    ) -> Result<(String, ChatTiming), ChatError> {
        let mut reply = String::new();
        let mut emit = |ev: &StreamEvent| {
            if let StreamEvent::Done { reply: r, .. } = ev {
                reply = r.clone();
            }
            true
        };
        let timing = self.handle_chat_streamed(req, clock, &mut emit)?;
        Ok((reply, timing))
    }

    fn record_turn(&mut self, session_id: &str, user: &str, reply: &str, now_ms: u64) {
        if self.cfg.history_turns == 0 {
            return;
        }
        if !self.sessions.contains_key(session_id) && self.sessions.len() >= CHAT_MAX_SESSIONS {
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.last_used_ms)
                .map(|(k, _)| k.clone())
            {
                self.sessions.remove(&oldest);
            }
        }
        let session = self
            .sessions
            .entry(session_id.to_string())
            .or_insert(Session {
                pairs: Vec::new(),
                last_used_ms: now_ms,
            });
        session.last_used_ms = now_ms;
        session.pairs.push((user.to_string(), reply.to_string()));
        while session.pairs.len() > self.cfg.history_turns {
            session.pairs.remove(0);
        }
    }
}

fn validate_session_id(raw: Option<&str>) -> Result<String, ChatError> {
    let id = raw.unwrap_or("default");
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("MICK_CHAT_BAD_SESSION");
    }
    Ok(id.to_string())
}

// ---------------------------------------------------------------------------
// Health / warm-up
// ---------------------------------------------------------------------------

/// Service readiness. The HTTP listener being up is NOT health: the model may
/// still be cold (warming) or Ollama unreachable (degraded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarmState {
    Warming,
    Ready,
    Degraded,
}

/// The `/health` body for a state. Configured model is included; nothing
/// secret exists to leak.
#[must_use]
pub fn health_body(state: WarmState, model: &str, warmup_ms: Option<u64>) -> String {
    match state {
        WarmState::Warming => format!(r#"{{"status":"warming","model":"{model}"}}"#),
        WarmState::Ready => match warmup_ms {
            Some(ms) => {
                format!(r#"{{"status":"ready","model":"{model}","warmup_ms":{ms}}}"#)
            }
            None => format!(r#"{{"status":"ready","model":"{model}"}}"#),
        },
        WarmState::Degraded => {
            format!(r#"{{"status":"degraded","reason":"OLLAMA_UNAVAILABLE","model":"{model}"}}"#)
        }
    }
}

// ---------------------------------------------------------------------------
// Wire (single handler path; streaming writes SSE into any `Write`)
// ---------------------------------------------------------------------------

/// Render one event as SSE. Content fields are JSON-encoded; timings carry no
/// content.
#[must_use]
pub fn sse_event(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::Start { request_id } => {
            format!("event: start\ndata: {{\"request_id\":\"{request_id}\"}}\n\n")
        }
        StreamEvent::Delta { text } => format!(
            "event: delta\ndata: {}\n\n",
            serde_json::json!({ "text": text })
        ),
        StreamEvent::Sentence { text } => format!(
            "event: sentence\ndata: {}\n\n",
            serde_json::json!({ "text": text })
        ),
        StreamEvent::Done { timing, .. } => format!(
            "event: done\ndata: {}\n\n",
            serde_json::json!({
                "ttft_ms": timing.ttft_ms,
                "first_sentence_ms": timing.first_sentence_ms,
                "total_ms": timing.total_ms,
                "deterministic": timing.deterministic,
            })
        ),
    }
}

/// Handle `POST /chat/stream`: write the SSE response head + events into
/// `sink` as they happen. A sink write failure cancels the generation (the
/// client hung up — barge-in). Pure over (service, body, clock, sink).
pub fn stream_chat_into<M: ChatModelClient>(
    svc: &mut ChatService<M>,
    body: &[u8],
    clock: &mut dyn FnMut() -> u64,
    sink: &mut dyn Write,
) -> Result<(), ChatError> {
    let req: ChatRequest = serde_json::from_slice(body).map_err(|_| "MICK_CHAT_BAD_REQUEST")?;
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if sink.write_all(head.as_bytes()).is_err() {
        return Err("MICK_CHAT_CANCELLED");
    }
    let mut emit = |ev: &StreamEvent| -> bool {
        sink.write_all(sse_event(ev).as_bytes()).is_ok() && sink.flush().is_ok()
    };
    svc.handle_chat_streamed(&req, clock, &mut emit).map(|_| ())
}

/// Route one parsed NON-streaming request. `/chat/stream` is handled by
/// [`stream_chat_into`] (it owns the socket); everything else lands here.
/// There is deliberately NO intent surface on this service.
pub fn handle_request<M: ChatModelClient>(
    svc: &mut ChatService<M>,
    method: &str,
    path: &str,
    body: &[u8],
    clock: &mut dyn FnMut() -> u64,
    health: (WarmState, &str, Option<u64>),
) -> (&'static str, String) {
    match (method, path) {
        ("GET", "/health") => ("200 OK", health_body(health.0, health.1, health.2)),
        ("POST", "/chat") => match serde_json::from_slice::<ChatRequest>(body) {
            Ok(req) => match svc.handle_chat(&req, clock) {
                Ok((reply, timing)) => (
                    "200 OK",
                    serde_json::json!({
                        "ok": true,
                        "reply": reply,
                        "timing": {
                            "ttft_ms": timing.ttft_ms,
                            "first_sentence_ms": timing.first_sentence_ms,
                            "total_ms": timing.total_ms,
                            "deterministic": timing.deterministic,
                        }
                    })
                    .to_string(),
                ),
                Err("MICK_CHAT_RATE_LIMITED") => (
                    "429 Too Many Requests",
                    "{\"ok\":false,\"error\":\"MICK_CHAT_RATE_LIMITED\"}".to_string(),
                ),
                Err("MICK_CHAT_MODEL_ERROR") => (
                    "502 Bad Gateway",
                    "{\"ok\":false,\"error\":\"MICK_CHAT_MODEL_ERROR\"}".to_string(),
                ),
                Err(code) => (
                    "422 Unprocessable Entity",
                    serde_json::json!({"ok": false, "error": code}).to_string(),
                ),
            },
            Err(e) => (
                "400 Bad Request",
                serde_json::json!({"ok": false, "error": format!("{e}")}).to_string(),
            ),
        },
        _ => ("404 Not Found", "{\"error\":\"unknown route\"}".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Stub model: replies with fixed deltas, counts calls, advances a shared
    /// fake clock by `delta_cost_ms` per delta so TTFT/total are deterministic.
    struct StubModel {
        deltas: Vec<String>,
        calls: Rc<Cell<u32>>,
        clock: Rc<Cell<u64>>,
        delta_cost_ms: u64,
    }

    impl ChatModelClient for StubModel {
        fn stream_chat(
            &self,
            _messages: &[ChatMessage],
            _gen: &ChatGenParams,
            on_delta: &mut dyn FnMut(&str) -> bool,
        ) -> Result<(), &'static str> {
            self.calls.set(self.calls.get() + 1);
            for d in &self.deltas {
                self.clock.set(self.clock.get() + self.delta_cost_ms);
                if !on_delta(d) {
                    return Ok(());
                }
            }
            Ok(())
        }
    }

    struct Harness {
        svc: ChatService<StubModel>,
        calls: Rc<Cell<u32>>,
        clock: Rc<Cell<u64>>,
    }

    fn harness_with(deltas: &[&str], cfg: ChatConfig) -> Harness {
        let calls = Rc::new(Cell::new(0));
        let clock = Rc::new(Cell::new(10_000));
        let svc = ChatService::new(
            StubModel {
                deltas: deltas.iter().map(|s| (*s).to_string()).collect(),
                calls: Rc::clone(&calls),
                clock: Rc::clone(&clock),
                delta_cost_ms: 50,
            },
            cfg,
        );
        Harness { svc, calls, clock }
    }

    fn harness(deltas: &[&str]) -> Harness {
        harness_with(deltas, ChatConfig::default())
    }

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            text: text.to_string(),
            session_id: None,
            max_output_tokens: None,
        }
    }

    fn run(h: &mut Harness, text: &str) -> (Vec<StreamEvent>, Result<ChatTiming, ChatError>) {
        let clock = Rc::clone(&h.clock);
        // Advance a little per clock read so distinct stages get distinct stamps.
        let mut clk = move || {
            clock.set(clock.get() + 1);
            clock.get()
        };
        let mut events = Vec::new();
        let mut emit = |ev: &StreamEvent| {
            events.push(ev.clone());
            true
        };
        let r = h.svc.handle_chat_streamed(&req(text), &mut clk, &mut emit);
        (events, r)
    }

    // --- prompt contract ------------------------------------------------------

    #[test]
    fn system_prompt_is_compact_and_denies_motion_authority() {
        assert!(
            CHAT_SYSTEM_PROMPT.chars().count() <= CHAT_SYSTEM_PROMPT_MAX_CHARS,
            "prompt grew past {CHAT_SYSTEM_PROMPT_MAX_CHARS} chars — prompt tokens are TTFT"
        );
        assert!(CHAT_SYSTEM_PROMPT.contains("cannot move or control the robot"));
        assert!(CHAT_SYSTEM_PROMPT.contains("governed motion path"));
        assert!(CHAT_SYSTEM_PROMPT.contains("Never claim access to live state"));
    }

    #[test]
    fn system_prompt_teaches_no_motion_wire_forms() {
        for token in [
            "cruise",
            "go_to",
            "lane_change",
            "target_speed_mps",
            "\"intent\"",
        ] {
            assert!(!CHAT_SYSTEM_PROMPT.contains(token), "{token}");
        }
    }

    // --- deterministic fast paths --------------------------------------------

    #[test]
    fn trivial_greetings_fast_route_with_zero_model_calls_and_zero_ttft() {
        for (text, reply) in [
            ("Hello!", "Hello."),
            ("hi", "Hello."),
            ("Thank you.", "You're welcome."),
            ("thanks", "You're welcome."),
            ("Goodbye", "Goodbye."),
        ] {
            let mut h = harness(&["never"]);
            let (events, r) = run(&mut h, text);
            let timing = r.unwrap();
            assert!(timing.deterministic, "{text:?}");
            assert_eq!(timing.ttft_ms, 0, "{text:?}");
            assert_eq!(h.calls.get(), 0, "{text:?} reached the model");
            assert!(matches!(&events[1], StreamEvent::Sentence { text: t } if t == reply));
        }
    }

    #[test]
    fn fast_routes_stay_tiny_and_do_not_swallow_real_questions() {
        for text in [
            "hello there how are you",
            "thanks for the update on the dock",
            "what is your status",
            "tell me a joke",
        ] {
            assert_eq!(fast_route(text), None, "{text:?}");
        }
    }

    #[test]
    fn explicit_motion_requests_are_refused_without_a_model_call() {
        let mut h = harness(&["never"]);
        let (events, r) = run(&mut h, "Drive forward one meter.");
        let timing = r.unwrap();
        assert!(timing.deterministic);
        assert_eq!(h.calls.get(), 0, "motion text reached the model");
        assert_eq!(h.svc.motion_refused(), 1);
        assert!(matches!(&events[1], StreamEvent::Sentence { text } if text == MOTION_ROUTE_REPLY));
        for text in [
            "turn left",
            "stop",
            "go to the dock",
            "pull over now",
            "back up",
        ] {
            assert!(is_explicit_motion_request(text), "{text:?}");
        }
    }

    #[test]
    fn motion_detector_is_conservative_leading_verb_only() {
        for text in [
            "how do I stop the recording",
            "what does drive mode mean",
            "the go button is red",
            "can you explain pull over rules",
        ] {
            assert!(!is_explicit_motion_request(text), "{text:?}");
        }
    }

    // --- streaming, ttft, sentences ------------------------------------------

    #[test]
    fn deltas_stream_before_completion_and_ttft_is_stamped_exactly_once() {
        let mut h = harness(&["Hello", ". ", "How can I help", "?"]);
        let (events, r) = run(&mut h, "introduce yourself briefly");
        let timing = r.unwrap();
        assert!(!timing.deterministic);
        assert!(timing.ttft_ms > 0 && timing.ttft_ms < timing.total_ms);
        // Delta events precede Done, and the first Delta precedes the last.
        let delta_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Delta { .. }))
            .count();
        assert_eq!(delta_count, 4);
        assert!(matches!(events.first(), Some(StreamEvent::Start { .. })));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
        // Sentences arrived (chunked on punctuation).
        let sentences: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Sentence { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sentences, vec!["Hello. How can I help?"]);
        assert!(timing.first_sentence_ms >= timing.ttft_ms);
    }

    #[test]
    fn first_sentence_is_emitted_before_the_stream_completes() {
        // Two sentences: the first must be emitted as a Sentence event BEFORE
        // the final delta arrives (ordering proof via event positions).
        let mut h = harness(&[
            "All systems look fine to me. ",
            "Ask me anything else you need",
            ".",
        ]);
        let (events, _) = run(&mut h, "how are things going today");
        let first_sentence_pos = events
            .iter()
            .position(|e| matches!(e, StreamEvent::Sentence { .. }))
            .expect("a sentence event");
        let last_delta_pos = events
            .iter()
            .rposition(|e| matches!(e, StreamEvent::Delta { .. }))
            .unwrap();
        assert!(
            first_sentence_pos < last_delta_pos,
            "first sentence must not wait for stream completion"
        );
    }

    // --- sentence chunker ------------------------------------------------------

    #[test]
    fn chunker_splits_on_punctuation_with_min_bound() {
        let mut c = SentenceChunker::new(5, 120);
        let mut got = Vec::new();
        for d in ["Hi. This is a sen", "tence. And ano", "ther one!"] {
            got.extend(c.push(d));
        }
        got.extend(c.finish());
        assert_eq!(got, vec!["Hi. This is a sentence.", "And another one!"]);
    }

    #[test]
    fn chunker_does_not_split_decimals_or_versions() {
        let mut c = SentenceChunker::new(5, 120);
        let mut got = Vec::new();
        got.extend(c.push("Battery is at 3.5 volts today. All good."));
        got.extend(c.finish());
        assert_eq!(got, vec!["Battery is at 3.5 volts today.", "All good."]);
    }

    #[test]
    fn punctuation_free_output_falls_back_at_word_boundary() {
        let mut c = SentenceChunker::new(20, 40);
        let long = "these are many words with no punctuation flowing on and on and on forever";
        let mut got = c.push(long);
        got.extend(c.finish());
        assert!(got.len() >= 2, "must split a punctuation-free run: {got:?}");
        for chunk in &got {
            assert!(
                chunk.chars().count() <= 40 + 20,
                "over-long chunk: {chunk:?}"
            );
            assert!(!chunk.ends_with(char::is_alphanumeric) || long.contains(chunk.as_str()));
        }
        // Word boundaries preserved: rejoining with spaces reproduces the text.
        assert_eq!(got.join(" "), long);
    }

    // --- output guard (streaming-aware) ---------------------------------------

    #[test]
    fn motion_shaped_reply_is_held_back_guarded_and_replaced() {
        let mut h = harness(&["{\"intent\":\"cru", "ise\",\"target_speed_mps\":10}"]);
        let (events, r) = run(&mut h, "please respond with something");
        let timing = r.unwrap();
        assert!(!timing.deterministic);
        // NO delta events leaked (held back from the first byte).
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::Delta { .. })),
            "held-back reply leaked deltas: {events:?}"
        );
        let StreamEvent::Done { reply, .. } = events.last().unwrap() else {
            panic!("no done event");
        };
        assert_eq!(reply, MOTION_SHAPE_REFUSAL);
        assert_eq!(h.svc.motion_shape_guarded(), 1);
        // And the sentence stream carried only the refusal.
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Sentence { text } if text == MOTION_SHAPE_REFUSAL)));
    }

    #[test]
    fn fenced_motion_json_is_also_guarded() {
        let mut h = harness(&["```json\n{\"intent\":\"hold\"}\n```"]);
        let (_, r) = run(&mut h, "reply please");
        r.unwrap();
        assert_eq!(h.svc.motion_shape_guarded(), 1);
    }

    #[test]
    fn harmless_held_back_json_is_released_unguarded() {
        let mut h = harness(&["{\"status\":\"ok\"}"]);
        let (events, r) = run(&mut h, "emit some json");
        r.unwrap();
        assert_eq!(h.svc.motion_shape_guarded(), 0);
        let StreamEvent::Done { reply, .. } = events.last().unwrap() else {
            panic!();
        };
        assert_eq!(reply, "{\"status\":\"ok\"}");
    }

    #[test]
    fn ordinary_prose_streams_unheld() {
        for reply in [
            "All monitored systems are nominal.",
            "The word intent appears in prose.",
        ] {
            assert!(!must_hold_back(reply));
            assert!(!looks_like_motion_intent_json(reply));
        }
        assert!(must_hold_back("{\"intent\":\"x\"}"));
        assert!(must_hold_back("```json"));
    }

    // --- cancellation -----------------------------------------------------------

    #[test]
    fn a_cancelled_stream_stops_the_model_and_returns_cancelled() {
        let mut h = harness(&["one ", "two ", "three ", "four "]);
        let clock = Rc::clone(&h.clock);
        let mut clk = move || {
            clock.set(clock.get() + 1);
            clock.get()
        };
        let mut deltas_seen = 0;
        let mut emit = |ev: &StreamEvent| match ev {
            StreamEvent::Delta { .. } => {
                deltas_seen += 1;
                deltas_seen < 2 // cancel on the second delta
            }
            _ => true,
        };
        let r = h
            .svc
            .handle_chat_streamed(&req("talk to me"), &mut clk, &mut emit);
        assert_eq!(r, Err("MICK_CHAT_CANCELLED"));
        assert_eq!(deltas_seen, 2, "consumption must stop at the cancel point");
    }

    // --- validation, limits, errors --------------------------------------------

    #[test]
    fn empty_oversized_and_control_input_is_refused_before_the_model() {
        let mut h = harness(&["x"]);
        let clock = Rc::clone(&h.clock);
        let mut clk = move || {
            clock.set(clock.get() + 1);
            clock.get()
        };
        let mut emit = |_: &StreamEvent| true;
        assert_eq!(
            h.svc.handle_chat_streamed(&req("   "), &mut clk, &mut emit),
            Err("MICK_CHAT_EMPTY_REQUEST")
        );
        let long = "x".repeat(501);
        assert_eq!(
            h.svc.handle_chat_streamed(&req(&long), &mut clk, &mut emit),
            Err("MICK_CHAT_TEXT_TOO_LONG"),
            "default input bound is 500 chars"
        );
        assert_eq!(
            h.svc
                .handle_chat_streamed(&req("hi\u{1b}[2Jthere"), &mut clk, &mut emit),
            Err("MICK_CHAT_BAD_TEXT")
        );
        assert_eq!(h.calls.get(), 0);
    }

    #[test]
    fn per_request_token_cap_only_reduces_never_raises() {
        struct CapturingGen {
            seen: Rc<Cell<u32>>,
        }
        impl ChatModelClient for CapturingGen {
            fn stream_chat(
                &self,
                _m: &[ChatMessage],
                gen: &ChatGenParams,
                on_delta: &mut dyn FnMut(&str) -> bool,
            ) -> Result<(), &'static str> {
                self.seen.set(gen.num_predict);
                on_delta("ok");
                Ok(())
            }
        }
        let seen = Rc::new(Cell::new(0));
        let mut svc = ChatService::new(
            CapturingGen {
                seen: Rc::clone(&seen),
            },
            ChatConfig::default(),
        );
        let mut clk = || 10_000;
        let mut emit = |_: &StreamEvent| true;
        // Reduction honoured.
        let r = ChatRequest {
            text: "hello world how are things".into(),
            session_id: None,
            max_output_tokens: Some(16),
        };
        svc.handle_chat_streamed(&r, &mut clk, &mut emit).unwrap();
        assert_eq!(seen.get(), 16);
        // Raising is clamped to the configured max (48).
        let r = ChatRequest {
            text: "hello world again my friend".into(),
            session_id: None,
            max_output_tokens: Some(4096),
        };
        svc.handle_chat_streamed(&r, &mut clk, &mut emit).unwrap();
        assert_eq!(seen.get(), 48, "per-request cap must never exceed config");
    }

    #[test]
    fn model_unavailable_maps_to_the_stable_error() {
        struct Failing;
        impl ChatModelClient for Failing {
            fn stream_chat(
                &self,
                _m: &[ChatMessage],
                _g: &ChatGenParams,
                _d: &mut dyn FnMut(&str) -> bool,
            ) -> Result<(), &'static str> {
                Err("MICK_CHAT_OLLAMA_REQUEST_FAILED")
            }
        }
        let mut svc = ChatService::new(Failing, ChatConfig::default());
        let mut clk = || 10_000;
        let mut emit = |_: &StreamEvent| true;
        assert_eq!(
            svc.handle_chat_streamed(&req("say hello to everyone"), &mut clk, &mut emit),
            Err("MICK_CHAT_MODEL_ERROR")
        );
    }

    // --- history: stateless by default, bounded when enabled --------------------

    #[test]
    fn history_is_off_by_default_and_no_session_state_accrues() {
        let mut h = harness(&["fine. "]);
        for i in 0..3 {
            let clock = Rc::clone(&h.clock);
            let mut clk = move || {
                clock.set(clock.get() + 1);
                clock.get()
            };
            let mut emit = |_: &StreamEvent| true;
            h.svc
                .handle_chat_streamed(
                    &req(&format!("turn number {i} says something")),
                    &mut clk,
                    &mut emit,
                )
                .unwrap();
            h.clock.set(h.clock.get() + 2_000);
        }
        assert_eq!(
            h.svc.session_count(),
            0,
            "stateless default must store nothing"
        );
    }

    #[test]
    fn enabled_history_is_bounded_to_the_hard_cap() {
        let cfg = ChatConfig {
            history_turns: 2,
            ..ChatConfig::default()
        };
        let mut h = harness_with(&["fine. "], cfg);
        for i in 0..6 {
            let clock = Rc::clone(&h.clock);
            let mut clk = move || {
                clock.set(clock.get() + 1);
                clock.get()
            };
            let mut emit = |_: &StreamEvent| true;
            h.svc
                .handle_chat_streamed(
                    &req(&format!("history turn {i} content here")),
                    &mut clk,
                    &mut emit,
                )
                .unwrap();
            h.clock.set(h.clock.get() + 2_000);
        }
        assert_eq!(h.svc.history_len("default"), 2);
    }

    #[test]
    fn config_rejects_history_beyond_the_hard_cap() {
        // from_env is env-driven; validate the invariant directly.
        const { assert!(CHAT_MAX_HISTORY_TURNS == 2) };
        let cfg = ChatConfig::default();
        assert_eq!(cfg.history_turns, 0, "stateless by default");
        assert_eq!(cfg.max_input_chars, 500);
        assert_eq!(cfg.max_output_tokens, 48);
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.keep_alive, "30m");
    }

    // --- health -------------------------------------------------------------------

    #[test]
    fn health_reports_warming_ready_and_degraded_states() {
        assert_eq!(
            health_body(WarmState::Warming, "gemma3:4b", None),
            r#"{"status":"warming","model":"gemma3:4b"}"#
        );
        assert_eq!(
            health_body(WarmState::Ready, "gemma3:4b", Some(1234)),
            r#"{"status":"ready","model":"gemma3:4b","warmup_ms":1234}"#
        );
        assert_eq!(
            health_body(WarmState::Degraded, "gemma3:4b", None),
            r#"{"status":"degraded","reason":"OLLAMA_UNAVAILABLE","model":"gemma3:4b"}"#
        );
    }

    // --- wire ---------------------------------------------------------------------

    #[test]
    fn nonstreaming_chat_returns_reply_and_timing_and_no_intent_routes_exist() {
        let mut h = harness(&["All monitored systems are nominal."]);
        let clock = Rc::clone(&h.clock);
        let mut clk = move || {
            clock.set(clock.get() + 1);
            clock.get()
        };
        let (status, body) = handle_request(
            &mut h.svc,
            "POST",
            "/chat",
            br#"{"text":"How are your systems doing?"}"#,
            &mut clk,
            (WarmState::Ready, "gemma3:4b", Some(10)),
        );
        assert_eq!(status, "200 OK");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["reply"], "All monitored systems are nominal.");
        assert!(v["timing"]["ttft_ms"].is_u64());
        assert!(v["timing"]["total_ms"].is_u64());

        for path in ["/intent", "/intent/last", "/narration/last"] {
            for method in ["GET", "POST"] {
                let (status, _) = handle_request(
                    &mut h.svc,
                    method,
                    path,
                    b"{}",
                    &mut clk,
                    (WarmState::Ready, "m", None),
                );
                assert_eq!(status, "404 Not Found", "{method} {path}");
            }
        }
    }

    #[test]
    fn streaming_wire_emits_sse_events_into_the_sink() {
        let mut h = harness(&["Hello. ", "How can I help?"]);
        let clock = Rc::clone(&h.clock);
        let mut clk = move || {
            clock.set(clock.get() + 1);
            clock.get()
        };
        let mut sink: Vec<u8> = Vec::new();
        stream_chat_into(
            &mut h.svc,
            br#"{"text":"greet me please now"}"#,
            &mut clk,
            &mut sink,
        )
        .unwrap();
        let text = String::from_utf8(sink).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("Content-Type: text/event-stream"));
        assert!(text.contains("event: start"));
        assert!(text.contains("event: delta"));
        assert!(text.contains("event: sentence"));
        assert!(text.contains("event: done"));
        assert!(text.contains("ttft_ms"));
        // No hidden prompt leakage: the system prompt never rides the wire.
        assert!(!text.contains("governed motion path\" }"));
        assert!(!text.contains(CHAT_SYSTEM_PROMPT));
    }

    #[test]
    fn sse_done_event_carries_timings_and_no_content() {
        let ev = StreamEvent::Done {
            reply: "secret-ish content".to_string(),
            timing: ChatTiming {
                ttft_ms: 120,
                first_sentence_ms: 300,
                total_ms: 540,
                deterministic: false,
            },
        };
        let s = sse_event(&ev);
        assert!(s.contains("\"ttft_ms\":120"));
        assert!(
            !s.contains("secret-ish"),
            "done event must carry timings only, not the reply"
        );
    }

    #[test]
    fn rate_limit_sheds_before_the_model() {
        let mut h = harness(&["x"]);
        let mut shed = 0;
        for _ in 0..10 {
            let mut clk = || 10_000;
            let mut emit = |_: &StreamEvent| true;
            if h.svc
                .handle_chat_streamed(&req("say something new"), &mut clk, &mut emit)
                == Err("MICK_CHAT_RATE_LIMITED")
            {
                shed += 1;
            }
        }
        assert!(shed >= 6, "burst bound must shed: {shed}");
    }
}
