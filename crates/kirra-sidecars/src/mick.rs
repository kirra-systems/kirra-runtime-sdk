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
//! Mick can only ever SLOW the system down: the intent is advice to the doer;
//! Occy grounds it, KIRRA bounds it, and the verifying consumer enforces it —
//! all in other processes, across the actuation fence.

use kirra_planner::{LlmBrain, MickIntent, ModelClient, WorldContext};
use serde::Deserialize;

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
    /// The `GET /intent/last` wire form.
    #[must_use]
    pub fn to_wire(&self) -> String {
        // The slice parsed as an object moments ago; re-embedding it as a
        // Value cannot fail for an accepted intent.
        let intent: serde_json::Value =
            serde_json::from_str(&self.intent_json).unwrap_or(serde_json::Value::Null);
        serde_json::json!({ "intent": intent, "seq": self.seq, "at_ms": self.at_ms }).to_string()
    }
}

/// The service core: brain + latch + rate limit. Single-threaded, like the
/// serve loop that drives it.
pub struct IntentService<M: ModelClient> {
    brain: LlmBrain<M>,
    limiter: RateLimiter,
    last: Option<AcceptedIntent>,
    seq: u64,
}

impl<M: ModelClient> IntentService<M> {
    #[must_use]
    pub fn new(brain: LlmBrain<M>) -> Self {
        Self {
            brain,
            limiter: RateLimiter::new(MICK_RATE_BURST, MICK_RATE_PER_S),
            last: None,
            seq: 0,
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
    ) -> Result<(MickIntent, AcceptedIntent), &'static str> {
        if !self.limiter.admit(now_ms) {
            return Err("MICK_RATE_LIMITED");
        }
        let ctx = world_context(req.context.as_ref().unwrap_or(&ContextReq::default()))?;
        let (intent, slice) = self.brain.decide_request(&ctx, &req.text)?;
        self.seq += 1;
        let accepted = AcceptedIntent {
            seq: self.seq,
            at_ms: now_ms,
            intent_json: slice,
        };
        self.last = Some(accepted.clone());
        Ok((intent, accepted))
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
        let (intent, accepted) = svc.handle_text(&req("take me to the dock"), 10_000).unwrap();
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
    }

    /// Part 2.4 — the proof: unparseable LLM output fails closed to NO
    /// intent. The latch is untouched, so the doer has nothing to ground —
    /// no default goal, no "proceed cautiously".
    #[test]
    fn unparseable_llm_output_fails_closed_and_never_latches() {
        let mut svc = service("just floor it, trust me");
        let err = svc.handle_text(&req("go as fast as you can"), 10_000).unwrap_err();
        assert_eq!(err, "MICK_JSON_PARSE_ERROR");
        assert!(svc.last().is_none(), "a rejected reply must not become an intent");

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
        assert_eq!(svc.last().cloned(), before, "failure leaves the latch as-was");
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

    #[test]
    fn over_rate_requests_are_shed_before_the_model() {
        let mut svc = service(r#"{"intent":"hold"}"#);
        let mut shed = 0;
        for _ in 0..10 {
            if svc.handle_text(&req("hold"), 10_000) == Err("MICK_RATE_LIMITED") {
                shed += 1;
            }
        }
        assert!(shed >= 6, "the burst bound must shed a same-instant flood: {shed}");
    }
}
