// src/telemetry.rs

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static CORRELATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StructuredLogEvent {
    pub timestamp_epoch_secs: u64,
    pub severity: String,
    pub node_id: String,
    pub correlation_id: String,
    pub transaction_id: u16,
    pub register_offset: u16,
    pub raw_demand: f64,
    pub sanitized_output: f64,
    pub trust_score: u32,
    pub trust_mode: String,
    pub event_narrative: String,
}

/// Bundled parameters for [`EnterpriseTelemetryGateway::emit_structured_event`].
#[derive(Debug, Clone)]
pub struct StructuredEventInput<'a> {
    pub severity: &'a str,
    pub tx_id: u16,
    pub offset: u16,
    pub raw: f64,
    pub sanitized: f64,
    pub score: u32,
    pub mode: &'a str,
    pub narrative: &'a str,
}

pub struct EnterpriseTelemetryGateway {
    node_identifier: String,
}

impl EnterpriseTelemetryGateway {
    pub fn new(node_id: &str) -> Self {
        Self {
            node_identifier: node_id.to_string(),
        }
    }

    #[inline]
    pub fn generate_correlation_id(&self, tx_id: u16) -> String {
        let seq = CORRELATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        format!(
            "KIRRA-NODE-{}-{:04X}-SEQ-{}",
            self.node_identifier, tx_id, seq
        )
    }

    pub fn emit_structured_event(&self, input: &StructuredEventInput<'_>) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = StructuredLogEvent {
            timestamp_epoch_secs: now,
            severity: input.severity.to_string(),
            node_id: self.node_identifier.clone(),
            correlation_id: self.generate_correlation_id(input.tx_id),
            transaction_id: input.tx_id,
            register_offset: input.offset,
            raw_demand: input.raw,
            sanitized_output: input.sanitized,
            trust_score: input.score,
            trust_mode: input.mode.to_string(),
            event_narrative: input.narrative.to_string(),
        };
        serde_json::to_string(&event)
            .unwrap_or_else(|_| r#"{"error":"TELEMETRY_SERIALIZATION_FAILED"}"#.to_string())
    }
}
