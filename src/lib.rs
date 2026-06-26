// src/lib.rs

// These two doc lints fire on intentionally column-aligned ASCII derivation
// tables in safety doc-comments (e.g. the SG2 lateral-margin budget in
// `gateway/containment.rs`, the perception kinematic-ceiling budget in
// `gateway/perception_monitor.rs`). Satisfying them would misalign those tables,
// which are read as evidence — so the alignment wins over the markdown-nesting
// pedantry.
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

pub mod kirra_core;
pub mod governor_guard;
pub mod modbus_adapter;
pub mod config;
pub mod kinematics_contract;
pub mod ros2_adapter;
pub mod action_filter;
pub mod action_policy;
pub mod security;
pub mod telemetry;
pub mod metrics;
pub mod health;
pub mod output;
pub mod gateway;
pub mod robotics_alignment;
pub mod dds_bridge;
pub mod ffi;
#[cfg(feature = "tpm")]
pub mod tpm;
pub mod tpm_quote;
pub mod startup_sentinel;
pub mod attestation;
pub mod verifier;
pub mod verifier_store;
pub mod store_handle;
pub mod key_registry;
pub mod posture_cache;
pub mod posture_engine;
pub mod posture_engine_v2;
pub mod posture_tracker;
pub mod recovery_hysteresis;
pub mod telemetry_watchdog;
pub mod clock;
pub mod scenario_runner;
pub mod audit_chain;
pub mod audit_writer;
// #104 — post-incident forensic sequence instrumentation (observability-only;
// emits a correlation-id'd, signed, hash-chained sequence into the audit log).
pub mod post_incident;
// #111/#112 — command-source provenance (audit/ingress layer; the verdict stays
// source-blind per SG7). Emits COMMAND_SOURCE_HANDOFF into the signed chain.
pub mod command_source;
// Learning-loop capture channel (Phase 1, #190) — sibling of audit_writer;
// non-blocking, default-OFF side channel recording the verdict/correction.
pub mod capture;
pub mod wcet_gate;
pub mod traceability_gate;
pub mod federation;
pub mod federation_reconciliation;
pub mod protocol_adapter;
pub mod adapters;
pub mod fabric;
pub mod standby_monitor;
/// Background-task supervisor (review finding C2): re-spawns dead safety loops and
/// escalates the fleet to fail-closed LockedOut if a critical loop is wedged.
pub mod supervisor;
pub mod kinematics_sim;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustMode {
    FullAutonomy,
    ConstrainedAdvisory,
    ShadowMode,
    LockedOut,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GovernorInterceptResult {
    pub sanitized_scalar: f64,
    pub asset_in_safe_control_state: bool,
    /// `Cow` so the common/failsafe branches carry a `&'static str` discriminant
    /// with ZERO allocation (the FFI scalar path discards this every tick), while
    /// the off-nominal clamp branches still embed their numeric detail via an owned
    /// `format!` — preserving the exact audit-narrative content.
    pub mitigation_narrative: std::borrow::Cow<'static, str>,
    pub was_unsafe_attempt: bool,
    pub was_rate_breached: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentAction {
    MoveLinear { velocity: f64 },
    Rotate { angular_velocity: f64 },
    SetPumpRate { gpm: f64 },
    HoldPosition,
    EmergencyStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionResolution {
    Approved,
    Mutated,
    Rejected,
    Failsafe,
}

pub trait SafetyContract {
    fn min_bound(&self) -> f64;
    fn max_bound(&self) -> f64;
    fn max_angular_rate(&self) -> f64;
    fn max_rate(&self) -> f64;
    fn fallback(&self) -> f64;
    fn scale_factor(&self) -> f64;
}

pub trait SafetyGovernor {
    fn evaluate(&mut self, demand: f64, dt: f64) -> GovernorInterceptResult;
    fn trust_mode(&self) -> TrustMode;
    fn last_output(&self) -> f64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterError {
    FrameTruncated,
    InvalidProtocolIdentifier,
    IllegalFunctionCode,
    UnmonitoredRegisterTarget,
    MalformedPayload,
}

pub trait ProtocolAdapter {
    fn decode_demand(&self, frame: &[u8]) -> Result<f64, AdapterError>;
    fn encode_response(&self, sanitized: f64, original: &[u8]) -> Vec<u8>;
    fn encode_exception(&self, original: &[u8], exception_code: u8) -> Vec<u8>;
}
