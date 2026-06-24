//! **Mick's LLM brain** — the model-agnostic prompt/parse layer behind the `MickBrain`
//! seam. This is the *pure, testable* 90% of "plug in Gemma": render the world into a
//! prompt, ask a model, parse the reply back into a typed [`MickIntent`]. It depends on
//! NO model and NO network — a [`ModelClient`] is an abstract "prompt → text" port, so the
//! whole path is exercised with a deterministic [`MockModel`]. A concrete client (a local
//! Gemma via Ollama, a cloud model) implements [`ModelClient`] behind a feature/crate and
//! plugs in unchanged.
//!
//! Safety is unchanged by *which* model runs: the reply is parsed by the existing
//! fail-closed [`MickIntent::from_llm_json`] (tolerant of Gemma-style fences/preamble,
//! strict on schema + finiteness), then Occy grounds it and KIRRA bounds it. A model
//! error, a refusal, or a hallucinated reply all collapse to an `Err` → the caller HOLDs.

use crate::mick::{MickBrain, MickError, MickIntent, WorldContext};

/// Error from a model backend (transport failure, timeout, empty completion, …). Any
/// failure collapses here and the brain fails closed — the model is never trusted.
pub type ModelError = &'static str;

/// Abstract "prompt → completion text" port. A real backend (local Gemma via Ollama,
/// llama.cpp, a cloud API) implements this; tests use [`MockModel`]. Sync + blocking on
/// purpose: Mick is the slow System-2 loop, so one blocking call per planning tick at low
/// rate is fine, and it keeps the `MickBrain` seam synchronous.
pub trait ModelClient {
    /// Return the model's raw completion for `prompt`, or a `ModelError`.
    fn complete(&self, prompt: &str) -> Result<String, ModelError>;
}

/// Render the world into a driving prompt: the chauffeur persona, the STRICT typed-intent
/// output contract (one JSON object, matching `MickIntent::from_llm_json`), and the
/// ego-relative situation (serialized [`WorldContext`]). The persona is told a governor
/// enforces the hard limits — so the model is freed to focus on *good* driving rather than
/// re-deriving collision/law/envelope rules it cannot be trusted to get right anyway.
#[must_use]
pub fn build_prompt(ctx: &WorldContext) -> String {
    let situation = serde_json::to_string_pretty(ctx).unwrap_or_else(|_| "{}".to_string());
    format!(
        "You are Mick, a careful, law-abiding chauffeur driving an autonomous vehicle. \
Choose the SINGLE best high-level driving intent for the current situation.\n\
\n\
Respond with ONLY one JSON object (no prose, no code fence), in one of these forms:\n\
  {{\"intent\":\"cruise\",\"target_speed_mps\":<number>}}    keep going, up to this speed\n\
  {{\"intent\":\"go_to\",\"x_m\":<number>,\"y_m\":<number>}}   head toward a point (ego frame)\n\
  {{\"intent\":\"lane_change\",\"target_offset_m\":<number>}}  shift laterally (+left, -right)\n\
  {{\"intent\":\"overtake\"}}                                   pass the slow/stopped lead ahead\n\
  {{\"intent\":\"pull_over\"}}                                  get to the road edge and stop\n\
  {{\"intent\":\"hold\"}}                                       stop and hold\n\
\n\
Drive smoothly and comfortably; ease off near objects and when posture is DEGRADED. You \
set only the high-level intent — a separate safety governor enforces collision limits, \
traffic law, and the speed envelope, so you cannot cause a crash. Focus on driving well. \
Coordinates are ego-relative: +ahead is forward, +left is to your left.\n\
\n\
Examples (situation → intent):\n\
- open road, no objects, goal far ahead → {{\"intent\":\"cruise\",\"target_speed_mps\":10}}\n\
- a stopped object close ahead in your path → {{\"intent\":\"hold\"}}\n\
- a slow object ahead, the goal is past it, a lane change to the left is allowed → \
{{\"intent\":\"lane_change\",\"target_offset_m\":3.5}}\n\
- a stopped car blocking your lane, the goal is past it, room to pass → \
{{\"intent\":\"overtake\"}}\n\
- an emergency vehicle (ambulance/police/fire) approaching → \
{{\"intent\":\"pull_over\"}}\n\
- the goal is off to one side and reachable → {{\"intent\":\"go_to\",\"x_m\":20,\"y_m\":-4}}\n\
\n\
Situation:\n{situation}\n\
\n\
Intent:"
    )
}

/// The intent **JSON Schema** — the machine-readable form of the output contract that
/// `build_prompt` states in prose and that [`MickIntent::from_llm_json`] parses. A backend
/// that supports schema-constrained / grammar-constrained decoding (e.g. Ollama's `format`)
/// passes this so the model can ONLY emit a JSON object whose `intent` is one of the known
/// tags — eliminating the unknown-tag and non-JSON failure modes at the decoder, before a
/// token is sampled, rather than catching them after the fact with a fail-closed HOLD.
///
/// Deliberately constrains the **tag + JSON validity**, not every per-variant field: it
/// admits the union of the typed fields and requires only `intent`. The remaining checks —
/// that `go_to` carries finite `x_m`/`y_m`, that a number is not `Inf`/`NaN`, etc. — stay
/// with [`MickIntent::from_llm_json`], because a schema/grammar cannot express finiteness
/// and the binding safety decision must remain in our fail-closed parse, never delegated to
/// the model's decoder. So this is a strict improvement over plain `"json"` that can never
/// regress: worst case it is exactly as permissive on fields, and strictly tighter on tags.
///
/// Source of truth: keep the `enum` below in lockstep with [`MickIntent::from_llm_json`]'s
/// tags and `build_prompt`'s listed forms (the `intent_schema_lists_every_parseable_tag`
/// test pins this).
#[must_use]
pub fn intent_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "intent": {
                "type": "string",
                "enum": ["go_to", "lane_change", "hold", "cruise", "overtake", "pull_over"]
            },
            "x_m": { "type": "number" },
            "y_m": { "type": "number" },
            "target_offset_m": { "type": "number" },
            "target_speed_mps": { "type": "number" }
        },
        "required": ["intent"],
        "additionalProperties": false
    })
}

/// A [`MickBrain`] driven by any [`ModelClient`]: render the prompt, ask the model, parse
/// the reply into a typed intent. Fail-closed at every step — a transport error or an
/// unparseable / out-of-schema reply returns `Err`, on which the caller HOLDs.
pub struct LlmBrain<M: ModelClient> {
    model: M,
}

impl<M: ModelClient> LlmBrain<M> {
    #[must_use]
    pub fn new(model: M) -> Self {
        Self { model }
    }
}

impl<M: ModelClient> MickBrain for LlmBrain<M> {
    fn decide(&mut self, ctx: &WorldContext) -> Result<MickIntent, MickError> {
        let prompt = build_prompt(ctx);
        let raw = self.model.complete(&prompt).map_err(|_| "MICK_MODEL_ERROR")?;
        // from_llm_json is already fail-closed + tolerant of small-model framing.
        MickIntent::from_llm_json(&raw)
    }
}

/// A deterministic stand-in for a model, for tests / sim: returns a fixed completion (or a
/// fixed error). Exercises the entire prompt → parse → intent path with no model present.
pub struct MockModel {
    response: Result<String, ModelError>,
}

impl MockModel {
    /// A model that always replies with `text`.
    #[must_use]
    pub fn replying(text: impl Into<String>) -> Self {
        Self { response: Ok(text.into()) }
    }

    /// A model whose backend always fails with `err`.
    #[must_use]
    pub fn failing(err: ModelError) -> Self {
        Self { response: Err(err) }
    }
}

impl ModelClient for MockModel {
    fn complete(&self, _prompt: &str) -> Result<String, ModelError> {
        self.response.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ctx() -> WorldContext {
        WorldContext {
            ego_speed_mps: 3.0,
            posture: "NOMINAL",
            goal_ahead_m: 40.0,
            goal_left_m: 0.0,
            may_change_left: true,
            may_change_right: false,
            objects: Vec::new(),
        }
    }

    #[test]
    fn prompt_carries_the_schema_and_the_situation() {
        let p = build_prompt(&sample_ctx());
        // The typed-intent contract the model must follow.
        for tag in ["cruise", "go_to", "lane_change", "overtake", "pull_over", "hold"] {
            assert!(p.contains(tag), "prompt must list the {tag} intent");
        }
        // The ego-relative situation is embedded (serialized WorldContext).
        assert!(p.contains("ego_speed_mps") && p.contains("posture"), "prompt must embed the situation");
        // Few-shot worked examples are present (small models lean on them heavily).
        assert!(p.contains("Examples"), "prompt must carry few-shot examples");
    }

    #[test]
    fn intent_schema_is_a_well_formed_object_schema_requiring_the_tag() {
        let s = intent_schema();
        assert_eq!(s["type"], "object", "the schema is an object schema");
        assert_eq!(s["required"], serde_json::json!(["intent"]), "the intent tag is required");
        assert_eq!(s["additionalProperties"], serde_json::json!(false), "no stray fields admitted");
        // It must serialize as a JSON object (this is what Ollama's `format` receives).
        assert!(serde_json::to_string(&s).is_ok(), "the schema serializes");
    }

    /// THE source-of-truth pin: every tag the schema constrains the model to MUST be one
    /// the fail-closed parser accepts (with that tag's fields), and vice-versa — so the
    /// decoder grammar and the typed parse can never drift apart.
    #[test]
    fn intent_schema_lists_every_parseable_tag() {
        let s = intent_schema();
        let enum_tags: Vec<String> = s["properties"]["intent"]["enum"]
            .as_array()
            .expect("intent.enum is an array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        // A minimal VALID object for each tag, and the typed intent it must parse to.
        let cases = [
            (r#"{"intent":"go_to","x_m":1.0,"y_m":2.0}"#, "go_to"),
            (r#"{"intent":"lane_change","target_offset_m":3.5}"#, "lane_change"),
            (r#"{"intent":"hold"}"#, "hold"),
            (r#"{"intent":"cruise","target_speed_mps":5.0}"#, "cruise"),
            (r#"{"intent":"overtake"}"#, "overtake"),
            (r#"{"intent":"pull_over"}"#, "pull_over"),
        ];
        for (json, tag) in cases {
            assert!(enum_tags.contains(&tag.to_string()), "schema enum must list {tag}");
            assert!(MickIntent::from_llm_json(json).is_ok(), "parser must accept the schema-valid {tag}");
        }
        // No extra tags the parser would reject (the enum is exactly the parseable set).
        assert_eq!(enum_tags.len(), cases.len(), "schema enum lists exactly the parseable tags");
    }

    #[test]
    fn llm_brain_parses_a_valid_model_reply() {
        let mut brain = LlmBrain::new(MockModel::replying(r#"{"intent":"cruise","target_speed_mps":4.0}"#));
        assert_eq!(brain.decide(&sample_ctx()).unwrap(), MickIntent::Cruise { target_speed_mps: 4.0 });
    }

    #[test]
    fn llm_brain_tolerates_gemma_framing() {
        // The tolerant extractor recovers the object from a fence + preamble.
        let mut brain = LlmBrain::new(MockModel::replying("Sure — here is the intent:\n```json\n{\"intent\":\"hold\"}\n```"));
        assert_eq!(brain.decide(&sample_ctx()).unwrap(), MickIntent::Hold);
    }

    #[test]
    fn llm_brain_fails_closed_on_a_hallucinated_reply() {
        let mut brain = LlmBrain::new(MockModel::replying("just floor it, trust me"));
        assert!(brain.decide(&sample_ctx()).is_err(), "unparseable reply must fail closed");
    }

    #[test]
    fn llm_brain_fails_closed_on_a_model_error() {
        let mut brain = LlmBrain::new(MockModel::failing("connection refused"));
        assert!(brain.decide(&sample_ctx()).is_err(), "a backend error must fail closed");
    }

    #[test]
    fn llm_brain_fails_closed_on_a_nonfinite_intent() {
        // The model emits a syntactically valid object with an overflowing number.
        let mut brain = LlmBrain::new(MockModel::replying(r#"{"intent":"go_to","x_m":1e400,"y_m":0.0}"#));
        assert!(brain.decide(&sample_ctx()).is_err(), "a non-finite intent must fail closed");
    }
}
