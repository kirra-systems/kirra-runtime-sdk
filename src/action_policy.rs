// src/action_policy.rs

use crate::AgentAction;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "action")]
enum LlmSchemaPayload {
    #[serde(rename = "MOVE")]
    Move { velocity: f64 },
    #[serde(rename = "ROTATE")]
    Rotate { angular_velocity: f64 },
    #[serde(rename = "PUMP")]
    Pump { gpm: f64 },
    #[serde(rename = "STOP")]
    Stop,
}

pub struct UnstructuredTextParser;

impl UnstructuredTextParser {
    pub fn parse_llm_json_intent(&self, raw_json: &str) -> Result<AgentAction, &'static str> {
        let payload: LlmSchemaPayload =
            serde_json::from_str(raw_json).map_err(|_| "JSON_DESERIALIZE_ERROR")?;
        match payload {
            LlmSchemaPayload::Move { velocity } => Ok(AgentAction::MoveLinear { velocity }),
            LlmSchemaPayload::Rotate { angular_velocity } => {
                Ok(AgentAction::Rotate { angular_velocity })
            }
            LlmSchemaPayload::Pump { gpm } => Ok(AgentAction::SetPumpRate { gpm }),
            LlmSchemaPayload::Stop => Ok(AgentAction::EmergencyStop),
        }
    }
}
