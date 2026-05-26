// src/lib.rs

pub mod kirra_core;
pub mod modbus_adapter;
pub mod config;
pub mod kinematics_contract;
pub mod ros2_adapter;
pub mod action_filter;
pub mod action_policy;
pub mod security;
pub mod audit_log;
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
pub mod startup_sentinel;
pub mod verifier;
pub mod verifier_store;
pub mod posture_cache;
pub mod posture_engine;
pub mod posture_engine_v2;
pub mod recovery_hysteresis;
pub mod telemetry_watchdog;
pub mod clock;
pub mod scenario_runner;
pub mod audit_chain;
pub mod federation;
pub mod federation_reconciliation;
pub mod protocol_adapter;
pub mod adapters;
pub mod fabric;
pub mod standby_monitor;
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
    pub mitigation_narrative: String,
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
