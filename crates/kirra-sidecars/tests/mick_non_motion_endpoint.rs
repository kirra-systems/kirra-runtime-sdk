//! Endpoint-level contract for Mick's deterministic non-motion fence.
//!
//! Drives `IntentService::handle_text` — the exact call `mick_service`'s
//! `POST /intent` arm makes — and renders the SAME wire strings that arm
//! responds with, so what is asserted here is what a caller receives.
//!
//! The four inputs are the ones observed live against the deployed
//! `gemma3:4b`, three of which returned valid-looking MOTION json:
//!
//! ```text
//! "hello rabbit"           → {"intent":"cruise","target_speed_mps":5}
//! "hello parker"           → {"intent":"cruise","target_speed_mps":10}
//! "what do you see"        → {"intent":"route_to","x_m":120,"y_m":40}
//! "drive forward one meter"→ {"intent":"go_to","x_m":1,"y_m":0}      (correct)
//! ```
//!
//! The stub model here returns motion json for EVERY prompt, so a fence
//! regression cannot hide: it would latch a cruise intent and fail these
//! assertions rather than quietly passing.

use std::cell::Cell;
use std::rc::Rc;

use kirra_planner::{LlmBrain, ModelClient, ModelError};
use kirra_sidecars::mick::{IntentOutcome, IntentRequest, IntentService};

/// The wire body `POST /intent` returns for a deterministically non-motion
/// utterance — mirrors the `IntentOutcome::NonMotion` arm in `mick_service`.
const NON_MOTION_BODY: &str = r#"{"ok":true,"intent":null}"#;

struct AlwaysMotion {
    reply: String,
    calls: Rc<Cell<u32>>,
}

impl ModelClient for AlwaysMotion {
    fn complete(&self, _prompt: &str) -> Result<String, ModelError> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.reply.clone())
    }
}

fn service(reply: &str) -> (IntentService<AlwaysMotion>, Rc<Cell<u32>>) {
    let calls = Rc::new(Cell::new(0));
    let svc = IntentService::new(LlmBrain::new(AlwaysMotion {
        reply: reply.to_string(),
        calls: Rc::clone(&calls),
    }));
    (svc, calls)
}

fn text(t: &str) -> IntentRequest {
    IntentRequest {
        text: t.to_string(),
        context: None,
    }
}

/// The body `POST /intent` would return, and the body `GET /intent/last` would
/// return afterwards — rendered exactly as `mick_service` renders them.
fn post_and_last(svc: &mut IntentService<AlwaysMotion>, t: &str, now_ms: u64) -> (String, String) {
    let post = match svc.handle_text(&text(t), now_ms) {
        Ok(IntentOutcome::Accepted(_, accepted)) => accepted.to_post_wire(),
        Ok(IntentOutcome::NonMotion(_)) => NON_MOTION_BODY.to_string(),
        Err(code) => format!(r#"{{"ok":false,"error":"{code}"}}"#),
    };
    let last = match svc.last() {
        Some(a) => a.to_wire(),
        None => r#"{"intent":null}"#.to_string(),
    };
    (post, last)
}

#[test]
fn the_three_misclassified_utterances_return_no_intent_and_call_no_model() {
    for utterance in ["hello rabbit", "hello parker", "what do you see"] {
        // The stub replies with the cruise json gemma3:4b actually produced.
        let (mut svc, calls) = service(r#"{"intent":"cruise","target_speed_mps":5.0}"#);
        let (post, last) = post_and_last(&mut svc, utterance, 10_000);

        assert_eq!(post, NON_MOTION_BODY, "POST body for {utterance:?}");
        assert_eq!(
            last, r#"{"intent":null}"#,
            "GET /intent/last after {utterance:?} must still be empty"
        );
        assert_eq!(calls.get(), 0, "{utterance:?} reached the LLM");
    }
}

#[test]
fn an_explicit_motion_request_is_unaffected() {
    let (mut svc, calls) = service(r#"{"intent":"go_to","x_m":1.0,"y_m":0.0}"#);
    let (post, last) = post_and_last(&mut svc, "drive forward one meter", 10_000);

    assert_eq!(
        post,
        r#"{"ok":true,"intent":{"intent":"go_to","x_m":1.0,"y_m":0.0},"seq":1,"at_ms":10000}"#
    );
    assert_eq!(
        last,
        r#"{"intent":{"intent":"go_to","x_m":1.0,"y_m":0.0},"seq":1,"at_ms":10000}"#
    );
    assert_eq!(calls.get(), 1, "a real request must reach the model");
}

#[test]
fn a_greeting_does_not_re_present_an_earlier_drive_command() {
    // The state rule, at the wire. After a real command is latched and
    // consumed, a greeting must leave `GET /intent/last` byte-identical —
    // same payload AND same seq — so the doer's apply-once check keeps
    // ignoring it. A bumped seq here would make the robot drive on a hello.
    let (mut svc, calls) = service(r#"{"intent":"go_to","x_m":12.0,"y_m":-2.0}"#);
    let (_, after_command) = post_and_last(&mut svc, "take me to the dock", 10_000);
    assert!(after_command.contains(r#""seq":1"#));

    for (i, greeting) in ["hello rabbit", "thanks", "what do you see"]
        .iter()
        .enumerate()
    {
        let (post, last) = post_and_last(&mut svc, greeting, 12_000 + i as u64 * 2_000);
        assert_eq!(post, NON_MOTION_BODY, "{greeting:?}");
        assert_eq!(
            last, after_command,
            "{greeting:?} altered GET /intent/last — a consumer could re-read the drive command"
        );
    }
    assert_eq!(calls.get(), 1, "only the real command called the model");
}

#[test]
fn a_command_after_a_greeting_still_advances_the_sequence() {
    // The fence must not stall the real path: seq has to keep moving, or the
    // doer would ignore the next genuine intent as already-consumed.
    let (mut svc, _calls) = service(r#"{"intent":"go_to","x_m":3.0,"y_m":0.0}"#);
    post_and_last(&mut svc, "take me to the dock", 10_000);
    post_and_last(&mut svc, "hello rabbit", 12_000);
    let (post, _) = post_and_last(&mut svc, "take me to the dock", 14_000);
    assert!(
        post.contains(r#""seq":2"#),
        "the next real intent must advance seq: {post}"
    );
}

// --- the compositional live regression (2026-08) -----------------------------

/// The reply the deployed model actually produced for the live phrase — a
/// deliberately dangerous, schema-valid cruise. If the compositional fence
/// ever regresses, these tests latch it and fail loudly.
const LIVE_DANGEROUS_REPLY: &str = r#"{"intent":"cruise","target_speed_mps":10}"#;

#[test]
fn the_live_compositional_greeting_returns_no_intent_and_calls_no_model() {
    let (mut svc, calls) = service(LIVE_DANGEROUS_REPLY);
    let (post, last) = post_and_last(&mut svc, "Hello Mr. Parker, how are you?", 10_000);
    assert_eq!(post, NON_MOTION_BODY);
    assert_eq!(last, r#"{"intent":null}"#, "nothing may latch");
    assert_eq!(calls.get(), 0, "the live phrase reached the LLM");
    assert_eq!(
        svc.non_motion_fenced(),
        1,
        "the fence counter must record it"
    );
}

#[test]
fn compositional_non_motion_phrases_never_reach_the_model_even_after_a_command() {
    // Latch a real command first, then hold the line: every compositional
    // conversational phrase must leave the wire byte-identical (payload AND
    // seq) with zero further model calls.
    let (mut svc, calls) = service(r#"{"intent":"go_to","x_m":12.0,"y_m":-2.0}"#);
    let (_, after_command) = post_and_last(&mut svc, "take me to the dock", 10_000);
    assert!(after_command.contains(r#""seq":1"#));
    assert_eq!(calls.get(), 1);

    for (i, phrase) in [
        "Hello Mr. Parker, how are you?",
        "Hello Parker, how are your systems?",
        "Hey Rabbit, what do you see?",
        "Mr. Parker, are you there?",
        "Rabbit, what is your status?",
        "Good morning Parker, how are you?",
    ]
    .iter()
    .enumerate()
    {
        let (post, last) = post_and_last(&mut svc, phrase, 12_000 + i as u64 * 2_000);
        assert_eq!(post, NON_MOTION_BODY, "{phrase:?}");
        assert_eq!(last, after_command, "{phrase:?} altered GET /intent/last");
    }
    assert_eq!(calls.get(), 1, "a compositional phrase reached the LLM");
    assert_eq!(svc.non_motion_fenced(), 6);
}

#[test]
fn prefixed_motion_requests_each_call_the_model_exactly_once() {
    for phrase in [
        "Hello Parker, drive forward one meter",
        "Hey Rabbit, turn left",
        "Rabbit, stop",
        "Hello Mr. Parker, pull over",
        "Good morning, drive to the loading dock",
        "How are you going to reach the dock",
        "Are we okay to proceed to the ramp",
        "What do you see on the left, then go there",
    ] {
        let (mut svc, calls) = service(r#"{"intent":"hold"}"#);
        let (post, _) = post_and_last(&mut svc, phrase, 10_000);
        assert!(post.contains(r#""ok":true"#), "{phrase:?}: {post}");
        assert!(post.contains("hold"), "{phrase:?} did not reach the model");
        assert_eq!(calls.get(), 1, "{phrase:?}");
    }
}

#[test]
fn a_wake_prefix_carrying_a_command_reaches_the_model() {
    // "hello rabbit" alone is fenced; the same prefix followed by an explicit
    // request is not — otherwise the fence would swallow real commands.
    let (mut svc, calls) = service(r#"{"intent":"go_to","x_m":1.0,"y_m":0.0}"#);
    let (post, _) = post_and_last(&mut svc, "hello rabbit, drive forward one meter", 10_000);
    assert!(
        post.contains(r#""ok":true"#) && post.contains("go_to"),
        "{post}"
    );
    assert_eq!(calls.get(), 1);
}
