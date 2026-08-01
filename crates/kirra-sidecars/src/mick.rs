//! The Mick typed-intent endpoint core — **typed text in, typed intent out,
//! never a command.**
//!
//! `POST /intent {"text": "..."}` runs the existing `LlmBrain` →
//! `MickIntent::parse_llm_json` fail-closed path (`LlmBrain::decide_request`)
//! and, ONLY on success, latches the accepted intent for the doer to read on
//! `GET /intent/last`. The latched artifact is the exact JSON slice that
//! passed the parse, so every consumer (the occy_doer bridge, the planner
//! seam) re-parses the same bytes with the same parse — one parser, no drift.
//!
//! **Fail-closed to NO MOTION (Part 2.4):** an unparseable / out-of-schema /
//! non-finite model reply, a transport error, or an empty request all return
//! `Err` — the latch is NOT updated, the doer sees no new intent, and with no
//! intent there is no goal, no plan, no proposal. There is no default goal
//! and no "proceed cautiously" arm anywhere on this path.
//!
//! **Deterministic non-motion fence:** obvious conversational and read-only
//! text (`hello rabbit`, `what do you see`) is classified BEFORE the model runs
//! and returns [`IntentOutcome::NonMotion`] — no LLM call, no intent, and
//! critically no latch or `seq` change, so `GET /intent/last` cannot present an
//! older motion command as newly requested. Defense in depth only; the deployed
//! model returns valid-looking MOTION json for those utterances and the parse
//! correctly admits it, because the parse checks shape, not whether motion was
//! asked for. See [`crate::mick_fence`].
//!
//! Mick can only ever SLOW the system down: the intent is advice to the doer;
//! Occy grounds it, KIRRA bounds it, and the verifying consumer enforces it —
//! all in other processes, across the actuation fence.

use kirra_planner::{LlmBrain, MickIntent, ModelClient, WorldContext};
use serde::Deserialize;

use crate::mick_fence::{classify_non_motion, NonMotionKind};
use crate::net::RateLimiter;

/// LLM-call rate bound (burst / steady-state per second). A plumbing bound:
/// Mick is the slow System-2 loop and each request is a full model
/// completion, so a runaway caller is shed cheaply before the LLM call.
pub const MICK_RATE_BURST: f64 = 3.0;
pub const MICK_RATE_PER_S: f64 = 1.0;

/// One typed-text request. `context` is optional — an absent context is the
/// benign standing-still view (speed 0, NOMINAL); posture and speed only
/// shape the prompt's conservatism, never the enforcement.
#[derive(Deserialize)]
pub struct IntentRequest {
    pub text: String,
    #[serde(default)]
    pub context: Option<ContextReq>,
}

/// The caller-supplied slice of [`WorldContext`]. Everything defaults to the
/// benign zero view; an unknown posture token is REJECTED (fail-closed) —
/// never coerced to Nominal.
#[derive(Deserialize, Default)]
pub struct ContextReq {
    #[serde(default)]
    pub ego_speed_mps: f64,
    #[serde(default)]
    pub posture: Option<String>,
    #[serde(default)]
    pub goal_ahead_m: f64,
    #[serde(default)]
    pub goal_left_m: f64,
    #[serde(default)]
    pub may_change_left: bool,
    #[serde(default)]
    pub may_change_right: bool,
}

fn world_context(ctx: &ContextReq) -> Result<WorldContext, &'static str> {
    let posture = match ctx.posture.as_deref() {
        None | Some("NOMINAL") => "NOMINAL",
        Some("DEGRADED") => "DEGRADED",
        Some("LOCKED_OUT") => "LOCKED_OUT",
        Some(_) => return Err("MICK_BAD_CONTEXT"),
    };
    if !(ctx.ego_speed_mps.is_finite()
        && ctx.goal_ahead_m.is_finite()
        && ctx.goal_left_m.is_finite())
    {
        return Err("MICK_BAD_CONTEXT");
    }
    Ok(WorldContext {
        ego_speed_mps: ctx.ego_speed_mps,
        posture,
        goal_ahead_m: ctx.goal_ahead_m,
        goal_left_m: ctx.goal_left_m,
        may_change_left: ctx.may_change_left,
        may_change_right: ctx.may_change_right,
        objects: Vec::new(),
        available_turns: Vec::new(),
    })
}

/// An accepted intent, as latched for `GET /intent/last`.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptedIntent {
    /// Monotonic per-process sequence — the doer applies an intent at most
    /// once by tracking the last seq it consumed.
    pub seq: u64,
    /// Wall clock (ms) when the intent was accepted.
    pub at_ms: u64,
    /// The exact JSON object slice that passed the fail-closed parse.
    pub intent_json: String,
}

impl AcceptedIntent {
    /// The `GET /intent/last` wire form. Embeds `intent_json` VERBATIM —
    /// valid by construction: [`IntentService::handle_text`] verified the
    /// slice stands alone as a JSON object BEFORE latching (a re-embed
    /// failure there is a loud `MICK_INTENT_REEMBED_FAILED`, never a latched
    /// artifact), so there is no silent-null fallback path here (review:
    /// Copilot on #894).
    #[must_use]
    pub fn to_wire(&self) -> String {
        format!(
            r#"{{"intent":{},"seq":{},"at_ms":{}}}"#,
            self.intent_json, self.seq, self.at_ms
        )
    }

    /// The `POST /intent` success wire form (same verbatim-embed guarantee).
    #[must_use]
    pub fn to_post_wire(&self) -> String {
        format!(
            r#"{{"ok":true,"intent":{},"seq":{},"at_ms":{}}}"#,
            self.intent_json, self.seq, self.at_ms
        )
    }
}

/// The outcome of one typed-text request.
///
/// Three states, not two: a request can be accepted, REFUSED (an `Err`,
/// fail-closed), or recognised as deterministically non-motion. The last is a
/// success that carries no intent — an enum rather than an `Option` so a caller
/// cannot silently treat "the operator said hello" as "the model failed".
#[derive(Clone, Debug, PartialEq)]
pub enum IntentOutcome {
    /// The model produced an intent that passed the fail-closed parse; it is
    /// latched and `seq` has advanced.
    Accepted(MickIntent, AcceptedIntent),
    /// Deterministically non-motion text. No model call was made, no intent
    /// exists, and the latch and `seq` are UNCHANGED.
    NonMotion(NonMotionKind),
}

/// The service core: brain + latch + rate limit. Single-threaded, like the
/// serve loop that drives it.
pub struct IntentService<M: ModelClient> {
    brain: LlmBrain<M>,
    limiter: RateLimiter,
    last: Option<AcceptedIntent>,
    seq: u64,
    /// How many requests the deterministic fence has absorbed this process.
    non_motion_fenced: u64,
    /// Last kind announced, so a greeting arriving at voice rate logs once per
    /// episode rather than once per utterance.
    announced_kind: Option<NonMotionKind>,
}

impl<M: ModelClient> IntentService<M> {
    #[must_use]
    pub fn new(brain: LlmBrain<M>) -> Self {
        Self {
            brain,
            limiter: RateLimiter::new(MICK_RATE_BURST, MICK_RATE_PER_S),
            last: None,
            seq: 0,
            non_motion_fenced: 0,
            announced_kind: None,
        }
    }

    /// Handle one typed-text request at `now_ms`. On success the accepted
    /// intent is latched and returned; on ANY failure the latch is untouched
    /// (fail-closed: no new intent → no motion downstream). The error token
    /// `MICK_RATE_LIMITED` maps to 429 at the wire; everything else to 422.
    pub fn handle_text(
        &mut self,
        req: &IntentRequest,
        now_ms: u64,
    ) -> Result<IntentOutcome, &'static str> {
        if !self.limiter.admit(now_ms) {
            return Err("MICK_RATE_LIMITED");
        }
        let ctx = world_context(req.context.as_ref().unwrap_or(&ContextReq::default()))?;
        // Deterministic fence, BEFORE the model and before any state moves.
        // Ordering is deliberate: after the rate limit (so the fence cannot be
        // used to bypass shedding) and after context validation (so a caller's
        // malformed context still surfaces), but before the LLM call — which is
        // the only thing this needs to prevent.
        if let Some(kind) = classify_non_motion(&req.text) {
            self.non_motion_fenced += 1;
            // Announce once per episode, not once per utterance: a greeting can
            // arrive at voice rate. No transcript is logged — the kind and the
            // running count carry the signal without the speech.
            if self.announced_kind != Some(kind) {
                self.announced_kind = Some(kind);
                eprintln!(
                    "mick: non-motion fence engaged (kind={}, fenced={}) — no model call, \
                     no intent, latch unchanged",
                    kind.as_str(),
                    self.non_motion_fenced
                );
            }
            // NOTHING mutates: not `last`, not `seq`. An unchanged `seq` is what
            // stops a consumer re-reading the previous motion intent as fresh —
            // the doer applies an intent at most once by tracking the last seq
            // it consumed, so a stalled seq is a no-op there by construction.
            return Ok(IntentOutcome::NonMotion(kind));
        }
        let (intent, slice) = self.brain.decide_request(&ctx, &req.text)?;
        // The slice must stand alone as a JSON object for verbatim
        // re-publication (`to_wire`). `parse_llm_json` guarantees this; if
        // that invariant ever breaks, refuse LOUDLY here — an unpublishable
        // artifact must never latch (review: Copilot on #894).
        let is_object = serde_json::from_str::<serde_json::Value>(&slice)
            .map(|v| v.is_object())
            .unwrap_or(false);
        if !is_object {
            return Err("MICK_INTENT_REEMBED_FAILED");
        }
        self.seq += 1;
        let accepted = AcceptedIntent {
            seq: self.seq,
            at_ms: now_ms,
            intent_json: slice,
        };
        self.last = Some(accepted.clone());
        self.announced_kind = None; // a real intent ends the fenced episode
        Ok(IntentOutcome::Accepted(intent, accepted))
    }

    /// How many requests the deterministic fence has absorbed this process.
    #[must_use]
    pub fn non_motion_fenced(&self) -> u64 {
        self.non_motion_fenced
    }

    /// The last accepted intent, if any (the `GET /intent/last` source).
    #[must_use]
    pub fn last(&self) -> Option<&AcceptedIntent> {
        self.last.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_planner::MockModel;

    fn service(reply: &str) -> IntentService<MockModel> {
        IntentService::new(LlmBrain::new(MockModel::replying(reply)))
    }

    fn req(text: &str) -> IntentRequest {
        IntentRequest {
            text: text.to_string(),
            context: None,
        }
    }

    #[test]
    fn accepted_intent_is_latched_and_wire_round_trips_through_the_one_parse() {
        let mut svc = service(r#"{"intent":"go_to","x_m":12.0,"y_m":-2.0}"#);
        let IntentOutcome::Accepted(intent, accepted) = svc
            .handle_text(&req("take me to the dock"), 10_000)
            .unwrap()
        else {
            panic!("a motion request must be accepted, not fenced");
        };
        assert_eq!(
            intent,
            MickIntent::GoTo {
                x_m: 12.0,
                y_m: -2.0
            }
        );
        assert_eq!(svc.last(), Some(&accepted));
        // The published artifact re-parses with the SAME parse to the SAME intent.
        let wire: serde_json::Value = serde_json::from_str(&accepted.to_wire()).unwrap();
        let republished = wire["intent"].to_string();
        assert_eq!(MickIntent::from_llm_json(&republished).unwrap(), intent);
        assert_eq!(wire["seq"], 1);
        // Verbatim embed: the wire carries the EXACT accepted slice, and the
        // POST form is the same artifact behind ok:true.
        assert!(accepted.to_wire().contains(&accepted.intent_json));
        let post: serde_json::Value = serde_json::from_str(&accepted.to_post_wire()).unwrap();
        assert_eq!(post["ok"], true);
        assert_eq!(post["intent"], wire["intent"]);
    }

    /// Part 2.4 — the proof: unparseable LLM output fails closed to NO
    /// intent. The latch is untouched, so the doer has nothing to ground —
    /// no default goal, no "proceed cautiously".
    #[test]
    fn unparseable_llm_output_fails_closed_and_never_latches() {
        let mut svc = service("just floor it, trust me");
        let err = svc
            .handle_text(&req("go as fast as you can"), 10_000)
            .unwrap_err();
        assert_eq!(err, "MICK_JSON_PARSE_ERROR");
        assert!(
            svc.last().is_none(),
            "a rejected reply must not become an intent"
        );

        // Same for a schema-valid-but-nonfinite reply.
        let mut svc = service(r#"{"intent":"go_to","x_m":1e999,"y_m":0.0}"#);
        assert!(svc.handle_text(&req("dock please"), 10_000).is_err());
        assert!(svc.last().is_none());
    }

    #[test]
    fn a_failure_never_clobbers_the_previous_good_intent() {
        // First a good intent latches; then a garbage reply must neither
        // replace nor clear it (the doer keeps the standing goal).
        let mut svc = service(r#"{"intent":"hold"}"#);
        svc.handle_text(&req("stop"), 10_000).unwrap();
        // Swap the brain's reply by rebuilding — MockModel is fixed-reply, so
        // emulate with a second service sharing the latch semantics: instead,
        // drive the SAME service with an empty request (fails pre-model).
        let before = svc.last().cloned();
        assert!(svc.handle_text(&req("   "), 11_000).is_err());
        assert_eq!(
            svc.last().cloned(),
            before,
            "failure leaves the latch as-was"
        );
    }

    #[test]
    fn unknown_posture_token_is_rejected_not_coerced() {
        let mut svc = service(r#"{"intent":"hold"}"#);
        let r = IntentRequest {
            text: "hold".into(),
            context: Some(ContextReq {
                posture: Some("YOLO".into()),
                ..ContextReq::default()
            }),
        };
        assert_eq!(svc.handle_text(&r, 10_000).unwrap_err(), "MICK_BAD_CONTEXT");
    }

    // --- deterministic non-motion fence ------------------------------------

    /// A model that counts calls, so "never reached the LLM" is an assertion
    /// about a counter rather than about a log line.
    struct CountingModel {
        reply: String,
        calls: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl kirra_planner::ModelClient for CountingModel {
        fn complete(&self, _prompt: &str) -> Result<String, kirra_planner::ModelError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.reply.clone())
        }
    }

    /// A service whose model reports how many times it was asked. The reply is
    /// the MOTION json gemma3:4b actually returned for "hello rabbit" — so if
    /// the fence ever stops working, these tests latch a cruise intent and fail
    /// loudly rather than passing on a coincidence.
    /// Returns the service and a HANDLE to the call counter — the test holds
    /// its own reference rather than reaching into `LlmBrain`'s private model,
    /// so proving "no LLM call" costs no API surface in another crate.
    fn counting_service(
        reply: &str,
    ) -> (
        IntentService<CountingModel>,
        std::rc::Rc<std::cell::Cell<u32>>,
    ) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let svc = IntentService::new(LlmBrain::new(CountingModel {
            reply: reply.to_string(),
            calls: std::rc::Rc::clone(&calls),
        }));
        (svc, calls)
    }

    const LIVE_CRUISE_REPLY: &str = r#"{"intent":"cruise","target_speed_mps":5.0}"#;

    #[test]
    fn the_observed_live_non_motion_inputs_never_reach_the_model() {
        for text in ["hello rabbit", "hello parker", "what do you see"] {
            let (mut svc, calls) = counting_service(LIVE_CRUISE_REPLY);
            let out = svc.handle_text(&req(text), 10_000).unwrap();
            assert!(
                matches!(out, IntentOutcome::NonMotion(_)),
                "{text:?} must be fenced, got {out:?}"
            );
            assert_eq!(calls.get(), 0, "{text:?} reached the LLM");
            assert!(svc.last().is_none(), "{text:?} latched an intent");
        }
    }

    /// The compositional live regression (2026-08): `Hello Mr. Parker, how are
    /// you?` fell through the exact-match fence, and the model answered with a
    /// dangerous, schema-valid cruise. Five facts, each asserted: the outcome
    /// is `NonMotion`, zero model calls, nothing latched, `seq` does not
    /// advance, and `non_motion_fenced` increments.
    #[test]
    fn the_live_compositional_greeting_is_fenced_with_no_state_change() {
        // Fresh service: fenced, no call, no latch, counter moves.
        let (mut svc, calls) = counting_service(r#"{"intent":"cruise","target_speed_mps":10}"#);
        let out = svc
            .handle_text(&req("Hello Mr. Parker, how are you?"), 10_000)
            .unwrap();
        assert!(matches!(out, IntentOutcome::NonMotion(_)), "got {out:?}");
        assert_eq!(calls.get(), 0, "the live phrase reached the LLM");
        assert!(svc.last().is_none(), "the live phrase latched an intent");
        assert_eq!(svc.non_motion_fenced(), 1);

        // With a prior motion intent latched: seq must NOT advance.
        let (mut svc, calls) = counting_service(r#"{"intent":"go_to","x_m":1.0,"y_m":0.0}"#);
        svc.handle_text(&req("drive forward one meter"), 10_000)
            .unwrap();
        let before = svc.last().cloned();
        assert_eq!(before.as_ref().unwrap().seq, 1);
        let out = svc
            .handle_text(&req("Hello Mr. Parker, how are you?"), 12_000)
            .unwrap();
        assert!(matches!(out, IntentOutcome::NonMotion(_)));
        assert_eq!(svc.last().cloned(), before, "latch changed on a greeting");
        assert_eq!(svc.last().unwrap().seq, 1, "seq advanced on a greeting");
        assert_eq!(calls.get(), 1, "only the real command called the model");
        assert_eq!(svc.non_motion_fenced(), 1);
    }

    #[test]
    fn a_real_motion_request_still_takes_the_model_path() {
        let (mut svc, calls) = counting_service(r#"{"intent":"go_to","x_m":1.0,"y_m":0.0}"#);
        let out = svc
            .handle_text(&req("drive forward one meter"), 10_000)
            .unwrap();
        let IntentOutcome::Accepted(intent, accepted) = out else {
            panic!("an explicit motion request must not be fenced");
        };
        assert_eq!(intent, MickIntent::GoTo { x_m: 1.0, y_m: 0.0 });
        assert_eq!(calls.get(), 1);
        assert_eq!(accepted.seq, 1);
        assert_eq!(svc.last(), Some(&accepted));
    }

    #[test]
    fn a_wake_prefix_with_a_command_still_reaches_the_model() {
        for text in [
            "hello rabbit, drive forward one meter",
            "hey parker turn left",
            "rabbit, stop",
        ] {
            let (mut svc, calls) = counting_service(r#"{"intent":"hold"}"#);
            let out = svc.handle_text(&req(text), 10_000).unwrap();
            assert!(
                matches!(out, IntentOutcome::Accepted(..)),
                "{text:?} must reach the model, got {out:?}"
            );
            assert_eq!(calls.get(), 1, "{text:?}");
        }
    }

    // --- the state rule: a greeting must not re-present an old command ------

    #[test]
    fn a_greeting_after_a_motion_intent_advances_nothing() {
        // The hazard: the operator says "go to the dock", the doer consumes
        // seq 1, then the operator says "hello rabbit". If the greeting bumped
        // seq (or relatched), the doer would see a "new" intent whose payload
        // is the OLD drive command and move again, unasked.
        let (mut svc, calls) = counting_service(r#"{"intent":"go_to","x_m":12.0,"y_m":-2.0}"#);
        let IntentOutcome::Accepted(_, first) = svc
            .handle_text(&req("take me to the dock"), 10_000)
            .unwrap()
        else {
            panic!("setup: the motion request must be accepted");
        };
        let before = svc.last().cloned();
        assert_eq!(first.seq, 1);

        for (i, text) in ["hello rabbit", "what do you see", "thanks"]
            .iter()
            .enumerate()
        {
            let out = svc
                .handle_text(&req(text), 11_000 + i as u64 * 1_000)
                .unwrap();
            assert!(matches!(out, IntentOutcome::NonMotion(_)), "{text:?}");
            assert_eq!(
                svc.last().cloned(),
                before,
                "{text:?} changed the latch — a consumer would re-read the drive command"
            );
        }
        // seq is still 1: the doer's apply-once check (`seq <= last_consumed`)
        // makes the stale latch a structural no-op, not a matter of timing.
        assert_eq!(svc.last().unwrap().seq, 1);
        assert_eq!(calls.get(), 1, "only the real request called the model");

        // And a genuine follow-up command still advances normally.
        let IntentOutcome::Accepted(_, next) = svc
            .handle_text(&req("take me to the dock"), 20_000)
            .unwrap()
        else {
            panic!("a later real request must still be accepted");
        };
        assert_eq!(next.seq, 2, "the fence must not stall real intents");
    }

    #[test]
    fn a_greeting_before_any_intent_leaves_intent_last_empty() {
        // `GET /intent/last` renders `{"intent":null}` when nothing is latched;
        // a greeting must not conjure a first intent out of nothing.
        let (mut svc, calls) = counting_service(LIVE_CRUISE_REPLY);
        assert!(matches!(
            svc.handle_text(&req("hello rabbit"), 10_000).unwrap(),
            IntentOutcome::NonMotion(_)
        ));
        assert!(svc.last().is_none());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn the_fence_counter_tracks_absorbed_requests() {
        let (mut svc, calls) = counting_service(LIVE_CRUISE_REPLY);
        assert_eq!(svc.non_motion_fenced(), 0);
        for (i, text) in ["hello", "hi", "thanks"].iter().enumerate() {
            svc.handle_text(&req(text), 10_000 + i as u64 * 2_000)
                .unwrap();
        }
        assert_eq!(svc.non_motion_fenced(), 3);
        assert_eq!(calls.get(), 0, "no fenced request may reach the LLM");
    }

    #[test]
    fn over_rate_requests_are_shed_before_the_model() {
        let mut svc = service(r#"{"intent":"hold"}"#);
        let mut shed = 0;
        for _ in 0..10 {
            if svc.handle_text(&req("hold"), 10_000) == Err("MICK_RATE_LIMITED") {
                shed += 1;
            }
        }
        assert!(
            shed >= 6,
            "the burst bound must shed a same-instant flood: {shed}"
        );
    }
}
