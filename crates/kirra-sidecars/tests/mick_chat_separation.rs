//! Structural + behavioral separation fence for the Mick CHAT path.
//!
//! The crate-level actuation fence (`ci/check_mick_actuation_fence.py`)
//! already proves kirra-sidecars has no dependency route to actuation. This
//! test adds the FINER invariant: the chat path (`src/chat.rs` +
//! `src/bin/mick_chat_service.rs`) must not even touch the typed-intent
//! machinery this crate legitimately uses elsewhere — no intent parse, no
//! intent schema, no latch, no planner brain. Chat is conversation; the
//! governed motion door is `/intent`, in a different binary.
//!
//! Source scanning is a ratchet against the convenient edge, not a proof —
//! the behavioral halves below (guard + wire) are the semantics.

use std::cell::Cell;
use std::rc::Rc;

use kirra_sidecars::chat::{
    handle_request, looks_like_motion_intent_json, ChatConfig, ChatMessage, ChatModelClient,
    ChatRequest, ChatService, CHAT_SYSTEM_PROMPT, MOTION_SHAPE_REFUSAL,
};

// --- 1. the source-level fence -----------------------------------------------

/// Identifiers of the motion/intent machinery (and actuation seams) that must
/// never appear in the chat sources. Every needle is built by CONCATENATION so
/// neither this file's own text nor `ci/check_mick_actuation_fence.py`'s
/// crate-wide symbol scan can match the list itself — the tokens only exist
/// at runtime.
fn forbidden_identifiers() -> Vec<String> {
    let join = |a: &str, b: &str| format!("{a}{b}");
    vec![
        // the typed-intent machinery (legitimately used by mick.rs, forbidden here)
        join("Mick", "Intent"),
        join("from_llm", "_json"),
        join("Intent", "Service"),
        join("Intent", "Outcome"),
        join("Llm", "Brain"),
        join("intent_", "schema"),
        join("kirra_", "planner"),
        join("World", "Context"),
        join("decide_", "request"),
        // actuation seams (also caught crate-wide by the CI fence)
        join("Release", "Token"),
        join("issue_ros", "_release"),
        join("write_", "twist"),
        join("Motor", "Serial"),
        join("cmd_", "vel"),
        join("serial", "port"),
    ]
}

#[test]
fn chat_sources_never_reference_intent_or_actuation_machinery() {
    for (name, src) in [
        ("src/chat.rs", include_str!("../src/chat.rs")),
        (
            "src/bin/mick_chat_service.rs",
            include_str!("../src/bin/mick_chat_service.rs"),
        ),
    ] {
        // Scan CODE, not commentary — the docs may (and do) explain what the
        // chat path is fenced FROM; only code touching it is a breach. Same
        // comment-stripping convention as ci/check_mick_actuation_fence.py.
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for ident in forbidden_identifiers() {
            assert!(
                !code.contains(&ident),
                "{name} references {ident:?} — the chat path must stay \
                 structurally separate from the motion/intent machinery"
            );
        }
    }
}

#[test]
fn chat_binary_does_not_mention_the_intent_routes() {
    // The chat binary must not even name the motion endpoints, so a future
    // "convenient" fallback from chat to /intent shows up as a fence break.
    let bin = include_str!("../src/bin/mick_chat_service.rs");
    for line in bin.lines() {
        let code = line.split("//").next().unwrap_or("");
        assert!(
            !code.contains("/intent"),
            "chat binary carries an /intent route outside comments: {line:?}"
        );
    }
}

// --- 2 + 3. prompt and response behavior --------------------------------------

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

#[test]
fn prompt_denies_motion_authority_and_lists_no_intent_forms() {
    assert!(CHAT_SYSTEM_PROMPT.contains("conversational only"));
    assert!(CHAT_SYSTEM_PROMPT.contains("no authority to move or control the robot"));
    for form in [
        "cruise",
        "go_to",
        "lane_change",
        "target_speed_mps",
        "{\"intent\"",
    ] {
        assert!(
            !CHAT_SYSTEM_PROMPT.contains(form),
            "prompt teaches a motion form: {form}"
        );
    }
}

#[test]
fn the_live_dangerous_reply_is_guarded_not_surfaced_as_a_result() {
    // The exact model output from the live /intent failure. On the CHAT
    // surface it must never appear as if it were an actionable result.
    let live = r#"{"intent":"cruise","target_speed_mps":10}"#;
    assert!(looks_like_motion_intent_json(live));

    let (mut svc, calls) = service(live);
    let (status, body) = handle_request(
        &mut svc,
        "POST",
        "/chat",
        br#"{"text":"Drive forward one meter."}"#,
        10_000,
    );
    assert_eq!(status, "200 OK");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["reply"], MOTION_SHAPE_REFUSAL);
    assert!(
        !body.contains("target_speed_mps"),
        "motion-shaped JSON leaked to the chat wire: {body}"
    );
    assert_eq!(calls.get(), 1, "the model was consulted exactly once");
    assert_eq!(svc.motion_shape_guarded(), 1);
}

// --- 4. endpoint separation ----------------------------------------------------

#[test]
fn chat_serves_no_intent_surface_at_the_wire() {
    let (mut svc, _) = service("hello");
    for (method, path) in [
        ("GET", "/intent/last"),
        ("POST", "/intent"),
        ("GET", "/narration/last"),
    ] {
        let (status, _) = handle_request(&mut svc, method, path, b"{}", 10_000);
        assert_eq!(
            status, "404 Not Found",
            "{method} {path} must not exist on chat"
        );
    }
}

#[test]
fn chat_requests_touch_no_intent_state_because_none_exists() {
    // The type-level fact, asserted as behavior: ChatService owns sessions
    // and a guard counter — nothing else. Drive many turns and observe the
    // only state that exists is conversational.
    let (mut svc, _) = service("fine");
    for i in 0..5u64 {
        let req = ChatRequest {
            text: format!("turn {i}"),
            session_id: None,
        };
        svc.handle_chat(&req, 10_000 + i * 2_000).unwrap();
    }
    assert_eq!(svc.session_count(), 1);
    assert_eq!(svc.history_len("default"), 5);
    assert_eq!(svc.motion_shape_guarded(), 0);
}
