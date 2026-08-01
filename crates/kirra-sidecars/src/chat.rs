//! The Mick CONVERSATIONAL service core — **typed text in, plain text out,
//! never an intent.**
//!
//! Why this exists: casual conversation must not be sent to the motion-only
//! `POST /intent` endpoint. That endpoint is deliberately schema-constrained
//! to typed motion intents, and a live failure showed what happens when a
//! greeting reaches it ("Hello Mr. Parker, how are you?" →
//! `{"intent":"cruise","target_speed_mps":10}`). The deterministic non-motion
//! fence now blocks that class *at* `/intent`; this module is the long-term
//! half of the design — a SEPARATE place for conversation, so conversational
//! text has somewhere to go that structurally cannot move the platform.
//!
//! **The separation, in one table:**
//!
//! | route | purpose | authority |
//! |---|---|---|
//! | `POST /chat`   | conversation only | none — plain text, no downstream consumer |
//! | `POST /intent` | motion-intent classification | governed downstream (Occy grounds, KIRRA bounds, the consumer enforces) |
//!
//! **No path to motion, structurally.** This module never references the
//! typed-intent machinery: no intent parse, no intent schema, no latch, no
//! seq, no planner types. The model transport is a plain-text `/api/chat`
//! completion with NO schema constraint. The crate-level actuation fence
//! (`ci/check_mick_actuation_fence.py`) already proves kirra-sidecars has no
//! dependency route to actuation; `tests/mick_chat_separation.rs` adds the
//! finer source-level fence that the CHAT path does not even touch the
//! intent machinery this crate legitimately uses elsewhere.
//!
//! **Output guard.** Chat must not *masquerade* as the motion API either: if
//! the model emits a JSON object shaped like a motion intent, the reply is
//! REPLACED by a fixed routing explanation ([`MOTION_SHAPE_REFUSAL`]) — the
//! motion-looking JSON is never passed through as if it were actionable, and
//! it is never parsed into anything. Pure and unit-tested
//! ([`looks_like_motion_intent_json`]).
//!
//! **History.** Bounded, in-memory, per-session: capped turns, capped total
//! characters, capped session count with oldest-session eviction. Nothing is
//! persisted, nothing is logged (counters only), everything clears on process
//! restart.

use std::collections::HashMap;

use crate::net::RateLimiter;
use serde::Deserialize;

/// LLM-call rate bound (burst / steady-state per second) — the same plumbing
/// shape as the intent service: each request is a full model completion, so a
/// runaway caller is shed cheaply before the LLM call.
pub const CHAT_RATE_BURST: f64 = 3.0;
pub const CHAT_RATE_PER_S: f64 = 1.0;

/// Hard ceiling on live sessions; the oldest-touched session is evicted when
/// a new one would exceed it (memory bound, not a feature).
pub const CHAT_MAX_SESSIONS: usize = 32;

/// The conversational system prompt. Deliberately explicit that this surface
/// has NO motion authority — and deliberately free of any intent-JSON forms,
/// so the prompt cannot teach the model the motion wire shape
/// (`tests` pin both properties).
pub const CHAT_SYSTEM_PROMPT: &str = "You are Mick, the robot's conversational assistant.\n\
\n\
You may answer questions, explain robot status information supplied in the \
user's text, and engage in ordinary conversation.\n\
\n\
You are conversational only. You have no authority to move or control the \
robot. Never claim that you started, stopped, steered, navigated, or \
commanded the platform. Never emit motion-intent JSON. If asked to move the \
robot, explain that motion requests must go through the dedicated governed \
intent path.\n\
\n\
Do not claim access to live sensors, posture, diagnostics, or state unless \
that information is explicitly included in the request context. Do not \
invent nominal status.";

/// The fixed reply substituted when the model emits motion-shaped JSON: the
/// chat surface explains the governed path instead of passing through
/// something that could be mistaken for an actionable result.
pub const MOTION_SHAPE_REFUSAL: &str =
    "I can't move or command the robot from chat. Motion requests go through \
the dedicated governed intent path, where the planner grounds them and the \
KIRRA governor bounds them. Happy to keep talking here.";

/// One `POST /chat` request. `session_id` selects a bounded in-memory history
/// (optional; absent → the shared `\"default\"` session).
#[derive(Deserialize)]
pub struct ChatRequest {
    pub text: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Boot-validated configuration (the env_config convention: a malformed value
/// aborts startup, never a silent default at use).
#[derive(Clone, Copy, Debug)]
pub struct ChatConfig {
    /// Max characters of user text per request (`KIRRA_MICK_CHAT_MAX_CHARS`).
    pub max_chars: usize,
    /// Max retained PAIRS (user + reply) per session
    /// (`KIRRA_MICK_CHAT_HISTORY_TURNS`); 0 disables history.
    pub history_turns: usize,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            max_chars: 1000,
            history_turns: 6,
        }
    }
}

/// Hard ceilings on the configurable bounds, so a misconfiguration cannot
/// defeat the "bounded" guarantees (out-of-range values ABORT startup, the
/// same fail-closed posture as malformed ones).
pub const CHAT_MAX_INPUT_CHARS_CEILING: usize = 8_192;
pub const CHAT_HISTORY_TURNS_CEILING: usize = 32;

impl ChatConfig {
    /// Read from the environment, fail-closed: present-but-malformed OR
    /// out-of-range values are an `Err` for the binary to abort on.
    pub fn from_env() -> Result<Self, String> {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_MAX_CHARS") {
            cfg.max_chars = v
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0 && *n <= CHAT_MAX_INPUT_CHARS_CEILING)
                .ok_or_else(|| {
                    format!(
                        "KIRRA_MICK_CHAT_MAX_CHARS malformed or out of range \
                         (1..={CHAT_MAX_INPUT_CHARS_CEILING}): {v:?}"
                    )
                })?;
        }
        if let Ok(v) = std::env::var("KIRRA_MICK_CHAT_HISTORY_TURNS") {
            cfg.history_turns = v
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n <= CHAT_HISTORY_TURNS_CEILING)
                .ok_or_else(|| {
                    format!(
                        "KIRRA_MICK_CHAT_HISTORY_TURNS malformed or out of range \
                         (0..={CHAT_HISTORY_TURNS_CEILING}): {v:?}"
                    )
                })?;
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

/// Abstract "messages → completion text" port for the CONVERSATIONAL path.
/// Deliberately not the motion `ModelClient`: that abstraction carries the
/// schema-constrained intent decode, which chat must never inherit.
pub trait ChatModelClient {
    fn chat(&self, messages: &[ChatMessage]) -> Result<String, &'static str>;
}

/// Is `reply`, as a whole (after trimming and unwrapping one Markdown code
/// fence), a JSON OBJECT carrying an `"intent"` key? That is the structural
/// signature of the motion wire shape, and the chat surface must not emit it.
///
/// Deliberately structural, not semantic: the check uses only serde_json —
/// the reply is NEVER parsed by the typed-intent parser, and an unknown
/// `intent` tag is guarded all the same (a new intent added later must not
/// leak through an out-of-date guard). Prose that merely *mentions* intent
/// JSON is not guarded — it does not parse as a lone object — and that is
/// fine: the guard exists to stop the reply LOOKING LIKE the motion API's
/// output, not to censor discussion of it.
#[must_use]
pub fn looks_like_motion_intent_json(reply: &str) -> bool {
    let mut candidate = reply.trim();
    // Unwrap one ```...``` fence (Gemma habitually fences JSON).
    if let Some(inner) = candidate.strip_prefix("```") {
        if let Some(end) = inner.rfind("```") {
            let inner = &inner[..end];
            // Drop an optional language tag on the fence line.
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

/// Why a request was refused (stable wire tokens).
pub type ChatError = &'static str;

struct Session {
    /// (user, assistant) pairs, oldest first.
    pairs: Vec<(String, String)>,
    last_used_ms: u64,
}

/// The service core: transport + per-session bounded history + rate limit.
/// Single-threaded, like the serve loop that drives it. Holds NO intent
/// state — there is no latch, no seq, nothing for a consumer to poll.
pub struct ChatService<M: ChatModelClient> {
    model: M,
    cfg: ChatConfig,
    limiter: RateLimiter,
    sessions: HashMap<String, Session>,
    /// How many motion-shaped replies the output guard has replaced.
    motion_shape_guarded: u64,
}

impl<M: ChatModelClient> ChatService<M> {
    #[must_use]
    pub fn new(model: M, cfg: ChatConfig) -> Self {
        Self {
            model,
            cfg,
            limiter: RateLimiter::new(CHAT_RATE_BURST, CHAT_RATE_PER_S),
            sessions: HashMap::new(),
            motion_shape_guarded: 0,
        }
    }

    /// How many replies the motion-shape output guard has replaced.
    #[must_use]
    pub fn motion_shape_guarded(&self) -> u64 {
        self.motion_shape_guarded
    }

    /// Handle one chat request at `now_ms`. Returns the reply text, or a
    /// stable error token. Validation happens BEFORE the model call; the
    /// output guard happens after it; history records only what was actually
    /// returned to the caller (a guarded reply stores the refusal, never the
    /// motion-shaped text).
    pub fn handle_chat(&mut self, req: &ChatRequest, now_ms: u64) -> Result<String, ChatError> {
        if !self.limiter.admit(now_ms) {
            return Err("MICK_CHAT_RATE_LIMITED");
        }
        let text = req.text.trim();
        if text.is_empty() {
            return Err("MICK_CHAT_EMPTY_REQUEST");
        }
        if req.text.chars().count() > self.cfg.max_chars {
            return Err("MICK_CHAT_TEXT_TOO_LONG");
        }
        // Control characters (beyond ordinary whitespace) are rejected, not
        // stripped: terminal-escape smuggling into a reply a human will read
        // in a terminal is exactly the abuse this bound exists for.
        if text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
        {
            return Err("MICK_CHAT_BAD_TEXT");
        }
        let session_id = validate_session_id(req.session_id.as_deref())?;

        // Compose: system prompt + this session's bounded history + the turn.
        let mut messages = vec![ChatMessage {
            role: "system",
            content: CHAT_SYSTEM_PROMPT.to_string(),
        }];
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
        messages.push(ChatMessage {
            role: "user",
            content: text.to_string(),
        });

        let raw = self
            .model
            .chat(&messages)
            .map_err(|_| "MICK_CHAT_MODEL_ERROR")?;
        let reply = if looks_like_motion_intent_json(&raw) {
            self.motion_shape_guarded += 1;
            // Count + log the KIND only — never the transcript.
            eprintln!(
                "mick_chat: motion-shape output guard engaged (guarded={}) — \
                 reply replaced with the routing explanation",
                self.motion_shape_guarded
            );
            MOTION_SHAPE_REFUSAL.to_string()
        } else {
            raw
        };

        self.record_turn(&session_id, text, &reply, now_ms);
        Ok(reply)
    }

    /// Append a (user, reply) pair to the session, enforcing every bound:
    /// per-session turns, per-session characters, and the session-count cap
    /// (oldest-touched evicted). In-memory only; cleared on restart.
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
        // Character bound over the whole session: trim oldest pairs until the
        // total fits (2× max_chars per retained turn is the generous ceiling).
        let char_cap = self.cfg.history_turns.saturating_mul(self.cfg.max_chars) * 2;
        while session.pairs.len() > 1
            && session
                .pairs
                .iter()
                .map(|(u, a)| u.chars().count() + a.chars().count())
                .sum::<usize>()
                > char_cap
        {
            session.pairs.remove(0);
        }
    }

    /// Retained history pairs for a session (tests/observability; content is
    /// never logged by the service itself).
    #[must_use]
    pub fn history_len(&self, session_id: &str) -> usize {
        self.sessions.get(session_id).map_or(0, |s| s.pairs.len())
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

/// Session ids are short, filename-safe tokens; anything else is refused
/// rather than truncated or normalized. Absent → the shared default session.
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

/// Route one parsed HTTP request — the single handler path the binary's serve
/// loop delegates to, extracted pure so the wire contract is testable without
/// sockets. Returns `(status line, body)`.
///
/// Deliberately: there is NO `/intent` route here and NO `/intent/last` route
/// — a chat caller cannot reach, read, or mutate Mick intent state through
/// this service by construction (`tests/mick_chat_separation.rs` pins it).
pub fn handle_request<M: ChatModelClient>(
    svc: &mut ChatService<M>,
    method: &str,
    path: &str,
    body: &[u8],
    now_ms: u64,
) -> (&'static str, String) {
    match (method, path) {
        ("GET", "/health") => ("200 OK", "{\"status\":\"ok\"}".to_string()),
        ("POST", "/chat") => match serde_json::from_slice::<ChatRequest>(body) {
            Ok(req) => match svc.handle_chat(&req, now_ms) {
                Ok(reply) => (
                    "200 OK",
                    serde_json::json!({"ok": true, "reply": reply}).to_string(),
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

    struct StubModel {
        reply: String,
        calls: Rc<Cell<u32>>,
    }

    impl ChatModelClient for StubModel {
        fn chat(&self, _messages: &[ChatMessage]) -> Result<String, &'static str> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.reply.clone())
        }
    }

    struct FailingModel;
    impl ChatModelClient for FailingModel {
        fn chat(&self, _messages: &[ChatMessage]) -> Result<String, &'static str> {
            Err("MICK_CHAT_OLLAMA_REQUEST_FAILED")
        }
    }

    fn service(reply: &str) -> (ChatService<StubModel>, Rc<Cell<u32>>) {
        let calls = Rc::new(Cell::new(0));
        let svc = ChatService::new(
            StubModel {
                reply: reply.to_string(),
                calls: Rc::clone(&calls),
            },
            ChatConfig::default(),
        );
        (svc, calls)
    }

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            text: text.to_string(),
            session_id: None,
        }
    }

    // --- the system prompt contract -----------------------------------------

    #[test]
    fn system_prompt_declares_conversational_only_and_denies_motion_authority() {
        assert!(CHAT_SYSTEM_PROMPT.contains("conversational only"));
        assert!(CHAT_SYSTEM_PROMPT.contains("no authority to move or control the robot"));
        assert!(CHAT_SYSTEM_PROMPT.contains("Never emit motion-intent JSON"));
        assert!(CHAT_SYSTEM_PROMPT.contains("governed"));
        assert!(CHAT_SYSTEM_PROMPT.contains("Do not invent nominal status"));
    }

    #[test]
    fn system_prompt_does_not_teach_the_motion_wire_shape() {
        // The prompt must not list the MickIntent JSON forms — the chat model
        // must never learn the motion vocabulary from its own instructions.
        for token in [
            "cruise",
            "go_to",
            "lane_change",
            "pull_over",
            "route_to",
            "target_speed_mps",
            "\"intent\"",
        ] {
            assert!(
                !CHAT_SYSTEM_PROMPT.contains(token),
                "prompt leaks the motion wire shape: {token}"
            );
        }
    }

    // --- input validation ----------------------------------------------------

    #[test]
    fn empty_oversized_and_control_input_is_refused_before_the_model() {
        let (mut svc, calls) = service("hello there");
        assert_eq!(
            svc.handle_chat(&req("   "), 10_000),
            Err("MICK_CHAT_EMPTY_REQUEST")
        );
        let long = "x".repeat(1001);
        assert_eq!(
            svc.handle_chat(&req(&long), 12_000),
            Err("MICK_CHAT_TEXT_TOO_LONG")
        );
        assert_eq!(
            svc.handle_chat(&req("hi\u{1b}[2Jthere"), 14_000),
            Err("MICK_CHAT_BAD_TEXT")
        );
        assert_eq!(calls.get(), 0, "invalid input reached the model");
    }

    #[test]
    fn session_ids_are_validated_not_normalized() {
        let (mut svc, _) = service("ok");
        for (i, bad) in ["", "a b", "sess/../ion", "x".repeat(65).as_str()]
            .into_iter()
            .enumerate()
        {
            let r = ChatRequest {
                text: "hello".to_string(),
                session_id: Some(bad.to_string()),
            };
            // Spaced in time so the rate limiter never shadows the verdict.
            assert_eq!(
                svc.handle_chat(&r, 10_000 + i as u64 * 2_000),
                Err("MICK_CHAT_BAD_SESSION"),
                "{bad:?}"
            );
        }
        assert_eq!(
            validate_session_id(Some("terminal-1.a_b")).as_deref(),
            Ok("terminal-1.a_b")
        );
        assert_eq!(validate_session_id(None).as_deref(), Ok("default"));
    }

    #[test]
    fn rate_limit_sheds_before_the_model() {
        let (mut svc, calls) = service("ok");
        let mut shed = 0;
        for _ in 0..10 {
            if svc.handle_chat(&req("hello"), 10_000) == Err("MICK_CHAT_RATE_LIMITED") {
                shed += 1;
            }
        }
        assert!(
            shed >= 6,
            "burst bound must shed a same-instant flood: {shed}"
        );
        assert_eq!(calls.get() as usize, 10 - shed);
    }

    // --- the motion-shape output guard ---------------------------------------

    #[test]
    fn motion_shaped_json_is_detected_bare_and_fenced() {
        for reply in [
            r#"{"intent":"cruise","target_speed_mps":10}"#,
            r#"  {"intent":"go_to","x_m":1.0,"y_m":0.0}  "#,
            "```json\n{\"intent\":\"hold\"}\n```",
            "```\n{\"intent\":\"future_unknown_tag\",\"weird\":1}\n```",
        ] {
            assert!(looks_like_motion_intent_json(reply), "{reply:?}");
        }
    }

    #[test]
    fn ordinary_text_and_non_intent_json_pass_the_guard() {
        for reply in [
            "All monitored systems are nominal.",
            "The word intent appears in prose without JSON.",
            r#"{"status":"ok"}"#,
            r#"["intent"]"#,
            "I would answer {\"intent\":...} if asked, but here is prose around it.",
            "",
        ] {
            assert!(!looks_like_motion_intent_json(reply), "{reply:?}");
        }
    }

    #[test]
    fn a_motion_shaped_reply_is_replaced_never_passed_through() {
        let (mut svc, _) = service(r#"{"intent":"cruise","target_speed_mps":10}"#);
        let reply = svc
            .handle_chat(&req("Drive forward one meter."), 10_000)
            .unwrap();
        assert_eq!(reply, MOTION_SHAPE_REFUSAL);
        assert_eq!(svc.motion_shape_guarded(), 1);
        // And the HISTORY stores the refusal, not the motion-shaped text —
        // the guard must not smuggle the shape back in via the next prompt.
        let (mut svc2, _) = service(r#"{"intent":"hold"}"#);
        svc2.handle_chat(&req("go"), 10_000).unwrap();
        assert_eq!(svc2.history_len("default"), 1);
    }

    // --- history bounds -------------------------------------------------------

    #[test]
    fn history_is_bounded_by_turns_and_isolated_by_session() {
        let (mut svc, _) = service("reply");
        for i in 0..10 {
            let r = ChatRequest {
                text: format!("turn {i}"),
                session_id: Some("a".to_string()),
            };
            svc.handle_chat(&r, 10_000 + i * 2_000).unwrap();
        }
        assert_eq!(svc.history_len("a"), ChatConfig::default().history_turns);
        assert_eq!(svc.history_len("b"), 0, "sessions must be isolated");
    }

    #[test]
    fn session_count_is_capped_with_oldest_eviction() {
        let (mut svc, _) = service("r");
        for i in 0..(CHAT_MAX_SESSIONS + 5) {
            let r = ChatRequest {
                text: "hi".to_string(),
                session_id: Some(format!("s{i}")),
            };
            svc.handle_chat(&r, 10_000 + i as u64 * 2_000).unwrap();
        }
        assert_eq!(svc.session_count(), CHAT_MAX_SESSIONS);
        assert_eq!(svc.history_len("s0"), 0, "oldest session must be evicted");
        let newest = format!("s{}", CHAT_MAX_SESSIONS + 4);
        assert_eq!(svc.history_len(&newest), 1);
    }

    #[test]
    fn zero_history_turns_disables_history_cleanly() {
        let calls = Rc::new(Cell::new(0));
        let mut svc = ChatService::new(
            StubModel {
                reply: "r".to_string(),
                calls,
            },
            ChatConfig {
                history_turns: 0,
                ..ChatConfig::default()
            },
        );
        svc.handle_chat(&req("hello"), 10_000).unwrap();
        assert_eq!(svc.session_count(), 0);
    }

    // --- the model boundary ---------------------------------------------------

    #[test]
    fn a_model_failure_maps_to_the_stable_chat_error() {
        let mut svc = ChatService::new(FailingModel, ChatConfig::default());
        assert_eq!(
            svc.handle_chat(&req("hello"), 10_000),
            Err("MICK_CHAT_MODEL_ERROR")
        );
    }

    #[test]
    fn the_prompt_history_and_turn_are_composed_in_order() {
        /// One captured model call: the (role, content) list it was handed.
        type SeenCalls = Rc<std::cell::RefCell<Vec<Vec<(String, String)>>>>;
        struct CapturingModel {
            seen: SeenCalls,
        }
        impl ChatModelClient for CapturingModel {
            fn chat(&self, messages: &[ChatMessage]) -> Result<String, &'static str> {
                self.seen.borrow_mut().push(
                    messages
                        .iter()
                        .map(|m| (m.role.to_string(), m.content.clone()))
                        .collect(),
                );
                Ok("fine".to_string())
            }
        }
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut svc = ChatService::new(
            CapturingModel {
                seen: Rc::clone(&seen),
            },
            ChatConfig::default(),
        );
        svc.handle_chat(&req("first"), 10_000).unwrap();
        svc.handle_chat(&req("second"), 12_000).unwrap();
        let calls = seen.borrow();
        let second = &calls[1];
        assert_eq!(second[0].0, "system");
        assert_eq!(second[0].1, CHAT_SYSTEM_PROMPT);
        assert_eq!(
            &second[1..],
            &[
                ("user".to_string(), "first".to_string()),
                ("assistant".to_string(), "fine".to_string()),
                ("user".to_string(), "second".to_string()),
            ]
        );
    }

    // --- the wire (handler path, no sockets) ----------------------------------

    #[test]
    fn wire_contract_health_chat_and_no_intent_routes() {
        let (mut svc, _) = service("All monitored systems are nominal.");
        let (status, body) = handle_request(&mut svc, "GET", "/health", b"", 10_000);
        assert_eq!((status, body.as_str()), ("200 OK", "{\"status\":\"ok\"}"));

        let (status, body) = handle_request(
            &mut svc,
            "POST",
            "/chat",
            br#"{"text":"How are your systems doing?"}"#,
            12_000,
        );
        assert_eq!(status, "200 OK");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["reply"], "All monitored systems are nominal.");

        // The chat service exposes NO intent surface: it cannot read the
        // latch, cannot mutate it, and cannot accept a motion request.
        for path in ["/intent", "/intent/last"] {
            for method in ["GET", "POST"] {
                let (status, _) = handle_request(&mut svc, method, path, b"{}", 14_000);
                assert_eq!(status, "404 Not Found", "{method} {path}");
            }
        }

        let (status, _) = handle_request(&mut svc, "POST", "/chat", b"not json", 16_000);
        assert_eq!(status, "400 Bad Request");
    }

    #[test]
    fn config_defaults_are_the_documented_bounds() {
        let cfg = ChatConfig::default();
        assert_eq!(cfg.max_chars, 1000);
        assert_eq!(cfg.history_turns, 6);
    }
}
