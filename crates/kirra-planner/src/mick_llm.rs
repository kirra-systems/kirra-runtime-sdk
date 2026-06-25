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
  {{\"intent\":\"turn_at\",\"direction\":\"left|right|straight\"}}  take the junction branch that way\n\
  {{\"intent\":\"route_to\",\"x_m\":<number>,\"y_m\":<number>}}  drive to a destination via the road network\n\
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
- a junction ahead and the goal is to your left → {{\"intent\":\"turn_at\",\"direction\":\"left\"}}\n\
- the goal is off to one side and reachable → {{\"intent\":\"go_to\",\"x_m\":20,\"y_m\":-4}}\n\
- a far destination reachable through several junctions → {{\"intent\":\"route_to\",\"x_m\":120,\"y_m\":40}}\n\
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
                "enum": ["go_to", "lane_change", "hold", "cruise", "overtake", "pull_over", "turn_at", "route_to",
                         "yield", "cross_when_clear", "creep_through"]
            },
            "x_m": { "type": "number" },
            "y_m": { "type": "number" },
            "target_offset_m": { "type": "number" },
            "target_speed_mps": { "type": "number" },
            "direction": { "type": "string", "enum": ["left", "right", "straight"] }
        },
        "required": ["intent"],
        "additionalProperties": false
    })
}

/// Render the world into a **sidewalk-courier** prompt (ADR-0027). Same strict typed-intent
/// contract and "a governor enforces the hard limits" framing as [`build_prompt`], but a
/// pedestrian-space persona offered ONLY the sidewalk intents — no road maneuvers (no
/// lane-change / overtake / junction turns). A courier follows its path, yields to people,
/// creeps through crowds, and crosses roads only at crosswalks when clear.
#[must_use]
pub fn build_courier_prompt(ctx: &WorldContext) -> String {
    let situation = serde_json::to_string_pretty(ctx).unwrap_or_else(|_| "{}".to_string());
    format!(
        "You are Mick, a careful sidewalk delivery courier — a small robot in pedestrian space \
(sidewalks, plazas, crosswalks). Choose the SINGLE best high-level intent for the situation.\n\
\n\
Respond with ONLY one JSON object (no prose, no code fence), in one of these forms:\n\
  {{\"intent\":\"go_to\",\"x_m\":<number>,\"y_m\":<number>}}        follow the path toward a point (ego frame)\n\
  {{\"intent\":\"yield\",\"x_m\":<number>,\"y_m\":<number>}}         give way to a pedestrian in your path, then go on\n\
  {{\"intent\":\"creep_through\",\"x_m\":<number>,\"y_m\":<number>}} inch gently through a crowd of pedestrians\n\
  {{\"intent\":\"cross_when_clear\",\"x_m\":<number>,\"y_m\":<number>}}  cross a road at a crosswalk, only when clear\n\
  {{\"intent\":\"hold\"}}                                          stop and hold\n\
\n\
You move at a slow, walking pace and ALWAYS give way to people — you never assert. A separate \
safety governor enforces collision limits and your speed/energy envelope, so you cannot hurt \
anyone; focus on being polite and making steady progress. Coordinates are ego-relative: \
+ahead is forward, +left is to your left.\n\
\n\
Examples (situation → intent):\n\
- clear path, goal ahead → {{\"intent\":\"go_to\",\"x_m\":8,\"y_m\":0}}\n\
- a pedestrian standing in your path ahead → {{\"intent\":\"yield\",\"x_m\":8,\"y_m\":0}}\n\
- a dense crowd of pedestrians around you → {{\"intent\":\"creep_through\",\"x_m\":8,\"y_m\":0}}\n\
- at a crosswalk, a car approaching on the road → {{\"intent\":\"cross_when_clear\",\"x_m\":9,\"y_m\":0}}\n\
- blocked, or off your route → {{\"intent\":\"hold\"}}\n\
\n\
Situation:\n{situation}\n\
\n\
Intent:"
    )
}

/// The **courier** intent schema — the constrained-decode subset for the sidewalk persona
/// ([`build_courier_prompt`]): only the sidewalk tags. Passed to a schema/grammar-constrained
/// backend so a courier model can ONLY emit a sidewalk intent — it cannot emit a road maneuver
/// (lane-change/overtake/turn) that does not apply on a sidewalk. A strict subset of
/// [`intent_schema`]'s enum, so every courier tag is still in the fail-closed parser.
#[must_use]
pub fn courier_intent_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "intent": {
                "type": "string",
                "enum": ["go_to", "yield", "cross_when_clear", "creep_through", "hold"]
            },
            "x_m": { "type": "number" },
            "y_m": { "type": "number" }
        },
        "required": ["intent"],
        "additionalProperties": false
    })
}

/// Which persona [`LlmBrain`] prompts as — a road chauffeur or a sidewalk courier (ADR-0027).
/// The persona selects the prompt + the constrained-decode schema; the fail-closed parse is the
/// same for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persona {
    Chauffeur,
    SidewalkCourier,
}

impl Persona {
    /// The prompt for this persona.
    #[must_use]
    pub fn prompt(self, ctx: &WorldContext) -> String {
        match self {
            Persona::Chauffeur => build_prompt(ctx),
            Persona::SidewalkCourier => build_courier_prompt(ctx),
        }
    }

    /// The constrained-decode schema for this persona (passed to Ollama's `format`).
    #[must_use]
    pub fn schema(self) -> serde_json::Value {
        match self {
            Persona::Chauffeur => intent_schema(),
            Persona::SidewalkCourier => courier_intent_schema(),
        }
    }
}

/// A [`MickBrain`] driven by any [`ModelClient`]: render the prompt, ask the model, parse
/// the reply into a typed intent. Fail-closed at every step — a transport error or an
/// unparseable / out-of-schema reply returns `Err`, on which the caller HOLDs.
pub struct LlmBrain<M: ModelClient> {
    model: M,
    persona: Persona,
}

impl<M: ModelClient> LlmBrain<M> {
    /// A road-chauffeur brain (the default persona).
    #[must_use]
    pub fn new(model: M) -> Self {
        Self { model, persona: Persona::Chauffeur }
    }

    /// A sidewalk-courier brain — prompts the courier persona, offered only sidewalk intents.
    #[must_use]
    pub fn courier(model: M) -> Self {
        Self { model, persona: Persona::SidewalkCourier }
    }

    /// This brain's persona.
    #[must_use]
    pub fn persona(&self) -> Persona {
        self.persona
    }
}

impl<M: ModelClient> MickBrain for LlmBrain<M> {
    fn decide(&mut self, ctx: &WorldContext) -> Result<MickIntent, MickError> {
        let prompt = self.persona.prompt(ctx);
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
            available_turns: Vec::new(),
        }
    }

    #[test]
    fn prompt_carries_the_schema_and_the_situation() {
        let p = build_prompt(&sample_ctx());
        // The typed-intent contract the model must follow.
        for tag in ["cruise", "go_to", "lane_change", "overtake", "pull_over", "turn_at", "route_to", "hold"] {
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
            (r#"{"intent":"turn_at","direction":"left"}"#, "turn_at"),
            (r#"{"intent":"route_to","x_m":120.0,"y_m":40.0}"#, "route_to"),
            (r#"{"intent":"yield","x_m":12.0,"y_m":0.0}"#, "yield"),
            (r#"{"intent":"cross_when_clear","x_m":12.0,"y_m":0.0}"#, "cross_when_clear"),
            (r#"{"intent":"creep_through","x_m":12.0,"y_m":0.0}"#, "creep_through"),
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

    // --- sidewalk-courier persona (ADR-0027 / D) ----------------------------

    #[test]
    fn courier_prompt_offers_the_sidewalk_intents_and_not_road_maneuvers() {
        let p = build_courier_prompt(&sample_ctx());
        for tag in ["go_to", "yield", "cross_when_clear", "creep_through", "hold"] {
            assert!(p.contains(tag), "courier prompt must offer the {tag} intent");
        }
        // A courier does not get road maneuvers.
        for tag in ["lane_change", "overtake", "turn_at", "route_to"] {
            assert!(!p.contains(tag), "courier prompt must NOT offer the road maneuver {tag}");
        }
        assert!(p.contains("courier") && p.contains("pedestrian"), "the sidewalk persona is present");
    }

    #[test]
    fn courier_schema_is_a_subset_of_the_parseable_tags() {
        let courier: Vec<String> = courier_intent_schema()["properties"]["intent"]["enum"]
            .as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let full: Vec<String> = intent_schema()["properties"]["intent"]["enum"]
            .as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(courier, ["go_to", "yield", "cross_when_clear", "creep_through", "hold"]);
        for tag in &courier {
            assert!(full.contains(tag), "courier tag {tag} must be in the full parseable set");
        }
    }

    #[test]
    fn courier_brain_authors_and_parses_a_sidewalk_intent() {
        // The courier persona round-trips a sidewalk intent through the brain.
        let mut brain = LlmBrain::courier(MockModel::replying(r#"{"intent":"yield","x_m":8.0,"y_m":0.0}"#));
        assert_eq!(brain.persona(), Persona::SidewalkCourier);
        assert_eq!(brain.decide(&sample_ctx()).unwrap(), MickIntent::Yield { x_m: 8.0, y_m: 0.0 });

        let mut creeper = LlmBrain::courier(MockModel::replying(r#"{"intent":"creep_through","x_m":8.0,"y_m":0.0}"#));
        assert_eq!(creeper.decide(&sample_ctx()).unwrap(), MickIntent::CreepThrough { x_m: 8.0, y_m: 0.0 });
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
