#![no_main]
//! Fuzz the LLM-output JSON parser
//! (`kirra_verifier::action_policy::UnstructuredTextParser::parse_llm_json_intent`).
//!
//! Parses UNTRUSTED model output (raw JSON from an LLM) into a typed, internally
//! tagged `AgentAction`. This is the seam where free-form model text becomes a
//! safety-relevant action, so it must be robust to arbitrary/adversarial JSON:
//! malformed / wrong-shape / deeply-nested input must return `Err` — never panic
//! or hang. The bytes are interpreted as UTF-8 (non-UTF-8 input is skipped).
use kirra_verifier::action_policy::UnstructuredTextParser;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let _ = UnstructuredTextParser.parse_llm_json_intent(s);
});
