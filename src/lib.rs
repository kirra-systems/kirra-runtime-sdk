// src/lib.rs

// These two doc lints fire on intentionally column-aligned ASCII derivation
// tables in safety doc-comments (e.g. the SG2 lateral-margin budget in
// `gateway/containment.rs`, the perception kinematic-ceiling budget in
// `gateway/perception_monitor.rs`). Satisfying them would misalign those tables,
// which are read as evidence — so the alignment wins over the markdown-nesting
// pedantry.
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

//! # Kirra — runtime legitimacy engine & safety governor
//!
//! Kirra is a fail-closed safety governor for AI-driven robotic and edge systems.
//! Its load-bearing thesis is **doer / checker**: a planner (the DOER) PROPOSES a
//! command; Kirra (the CHECKER) BOUNDS it against hard invariants, regardless of
//! what an AI model, LLM output, or upstream orchestrator instructs. The doer is
//! swappable and never trusted for safety — the checker is the invariant.
//!
//! ## Where to start
//!
//! - [`kirra_core::KirraKernelGovernor`] — the scalar governor: clamp a proposed
//!   command to a [`kinematics_contract::KinematicContract`] envelope + rate limit,
//!   fail-closed on non-finite input. Implements the [`SafetyGovernor`] trait.
//! - [`GovernorInterceptResult`] / [`MitigationCode`] — a governor verdict and the
//!   structured, zero-alloc reason it mitigated.
//! - [`ffi`] — the C ABI (`include/kirra.h`) exposing the same governor to C/C++.
//! - `authz`, `verifier`, `federation`, `audit_chain` — the fleet-legitimacy /
//!   verifier-service side (trust attestation, scoped RBAC, hash-chained audit).
//!
//! ## Examples
//!
//! - `examples/governor_quickstart.rs` — the checker bounding a doer's proposals
//!   (`cargo run --example governor_quickstart`).
//! - `examples/c/kirra_ffi_demo.c` — the same over the C ABI
//!   (`examples/c/build_and_run.sh`).

pub mod kirra_core;
pub mod governor_guard;
pub mod modbus_adapter;
pub mod config;
pub mod kinematics_contract;
pub mod ros2_adapter;
pub mod action_filter;
pub mod action_policy;
pub mod security;
pub mod authz;
pub mod telemetry;
pub mod metrics;
pub mod health;
pub mod output;
pub mod gateway;
pub mod robotics_alignment;
pub mod dds_bridge;
#[cfg(feature = "cyclonedds")]
pub mod dds_cyclonedds;
pub mod ffi;
#[cfg(feature = "tpm")]
pub mod tpm;
pub mod tpm_quote;
pub mod startup_sentinel;
pub mod attestation;
pub mod attestation_quote;
pub mod verifier;
pub mod verifier_store;
pub mod store_handle;
pub mod key_registry;
pub mod posture_cache;
pub mod ota_campaign;
pub mod campaign_monitor;
pub mod posture_engine;
pub mod posture_engine_v2;
pub mod posture_tracker;
pub mod recovery_hysteresis;
pub mod telemetry_watchdog;
pub mod clock;
pub mod scenario_runner;
pub mod audit_chain;
pub mod audit_shipper;
pub mod audit_writer;
// Clause 2 release-token binding (ADR-0006 / HVCHAN-001 §3 steps 5-7): digest →
// Ed25519 release token → actuator verify-before-release, over the
// kirra-contract-channel GovernorContractView; reuses the existing crypto.
pub mod governor_release;
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
// R2: `impl FleetTrustStore for VerifierStore` — the narrow durable seam the QM
// fleet transport drives instead of depending on this crate's `VerifierStore`.
pub mod fleet_trust_store;
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernorInterceptResult {
    pub sanitized_scalar: f64,
    pub asset_in_safe_control_state: bool,
    /// Structured, `Copy` mitigation code (§8). Replaces the prior per-tick
    /// `Cow<'static, str>` narrative: EVERY branch — including the three
    /// off-nominal clamps that used to `format!` a `String` on each `evaluate`
    /// — now carries only a tag + the numeric detail as `f64` fields, so the
    /// governor's hot scalar/FFI path (which discards the narrative every tick)
    /// is ZERO-ALLOC for all verdicts. The human/audit string is formatted
    /// LAZILY at the record/log sink via `Display`.
    pub mitigation: MitigationCode,
    pub was_unsafe_attempt: bool,
    pub was_rate_breached: bool,
}

/// The reason a governor verdict mitigated (clamped / held / failsafed) a
/// proposed scalar. `Copy + 'static` — carries no heap allocation on the hot
/// per-tick path. `Display` reproduces the exact prior narrative string for the
/// audit/log sink, formatting any numeric detail lazily only when recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MitigationCode {
    /// Non-finite (`NaN`/`Inf`) input rejected; fallback commanded.
    NonfiniteInputRejectedFailsafe,
    /// Non-positive timestep rejected; fallback commanded.
    InvalidTimeDeltaRejectedFailsafe,
    /// Out-of-envelope demand clamped to the hard bound (envelope wins, INV-8).
    EnvelopeClampTakesPriority,
    /// Rate-of-change clamped to the contract maximum.
    RateClampEnforced { max_rate: f64 },
    /// In-envelope, in-rate demand passed through unchanged.
    PassthroughUnrestrictedNormal,
    /// Degraded posture: demand bounded inside the reduced operating cap.
    DegradedPostureClamp { cap_min: f64, cap_max: f64 },
    /// Degraded posture (#70/#410): the demand was a speed INCREASE, a
    /// re-initiation from a stop, or a reversal through zero, which the
    /// decel-to-stop-and-HOLD bound overrode. `held` is the emitted scalar —
    /// non-increasing in magnitude and keeping the current sign.
    DegradedDecelToStopHold { held: f64 },
    /// Shadow mode: the last validated scalar is held (no new motion authored).
    ShadowModeHoldEnforced { retained: f64 },
    /// LockedOut: the contract fallback state is commanded.
    CriticalLockoutFallback,
}

impl std::fmt::Display for MitigationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MitigationCode::NonfiniteInputRejectedFailsafe => {
                f.write_str("NONFINITE_INPUT_REJECTED_FAILSAFE")
            }
            MitigationCode::InvalidTimeDeltaRejectedFailsafe => {
                f.write_str("INVALID_TIME_DELTA_REJECTED_FAILSAFE")
            }
            MitigationCode::EnvelopeClampTakesPriority => f.write_str("ENVELOPE_CLAMP_TAKES_PRIORITY"),
            MitigationCode::RateClampEnforced { max_rate } => {
                // Unit-NEUTRAL: this generic verdict serves every scalar governor —
                // kinematic (m/s²) AND flow (GPM/s, e.g. the water-flow Modbus
                // gateway) — so the formatter cannot know the domain's unit. The
                // contract owns the unit; a record sink that knows its domain adds
                // it. Every other variant already prints a bare number, so this
                // matches the prevailing style (was a hardcoded "GPM/s", correct
                // only for the flow path and wrong for every kinematic caller).
                write!(f, "RATE_CLAMP_ENFORCED: Max {max_rate}")
            }
            MitigationCode::PassthroughUnrestrictedNormal => {
                f.write_str("PASSTHROUGH_UNRESTRICTED_NORMAL")
            }
            MitigationCode::DegradedPostureClamp { cap_min, cap_max } => {
                write!(f, "DEGRADED_POSTURE_CLAMP: Bounded inside [{cap_min} - {cap_max}]")
            }
            MitigationCode::DegradedDecelToStopHold { held } => {
                write!(f, "DEGRADED_DECEL_TO_STOP_HOLD: Non-increasing hold at {held}")
            }
            MitigationCode::ShadowModeHoldEnforced { retained } => {
                write!(f, "SHADOW_MODE_HOLD_ENFORCED: Fixed value retained: {retained:.1}")
            }
            MitigationCode::CriticalLockoutFallback => {
                f.write_str("CRITICAL_LOCKOUT: Active fallback state commanded")
            }
        }
    }
}

#[cfg(test)]
mod mitigation_code_tests {
    use super::MitigationCode;

    /// `Display` PINS the exact narrative strings — they are recorded into the
    /// audit / governor-verdict narrative (e.g. the gateway `kirra_replay.json`),
    /// so any change to one is a deliberate recorded-content change and MUST be made
    /// here on purpose (as the unit-neutral `RateClampEnforced` change was). Numeric
    /// detail formats lazily (and only when recorded).
    #[test]
    fn display_pins_the_recorded_narratives() {
        assert_eq!(
            MitigationCode::NonfiniteInputRejectedFailsafe.to_string(),
            "NONFINITE_INPUT_REJECTED_FAILSAFE"
        );
        assert_eq!(
            MitigationCode::InvalidTimeDeltaRejectedFailsafe.to_string(),
            "INVALID_TIME_DELTA_REJECTED_FAILSAFE"
        );
        assert_eq!(
            MitigationCode::EnvelopeClampTakesPriority.to_string(),
            "ENVELOPE_CLAMP_TAKES_PRIORITY"
        );
        assert_eq!(
            // Unit-neutral: the generic scalar verdict names no domain unit (the
            // contract owns it) — was "Max 1.5 GPM/s", wrong for kinematic callers.
            MitigationCode::RateClampEnforced { max_rate: 1.5 }.to_string(),
            "RATE_CLAMP_ENFORCED: Max 1.5"
        );
        assert_eq!(
            MitigationCode::PassthroughUnrestrictedNormal.to_string(),
            "PASSTHROUGH_UNRESTRICTED_NORMAL"
        );
        assert_eq!(
            MitigationCode::DegradedPostureClamp { cap_min: 0.0, cap_max: 5.0 }.to_string(),
            "DEGRADED_POSTURE_CLAMP: Bounded inside [0 - 5]"
        );
        assert_eq!(
            MitigationCode::ShadowModeHoldEnforced { retained: 2.0 }.to_string(),
            "SHADOW_MODE_HOLD_ENFORCED: Fixed value retained: 2.0"
        );
        assert_eq!(
            MitigationCode::CriticalLockoutFallback.to_string(),
            "CRITICAL_LOCKOUT: Active fallback state commanded"
        );
    }

    #[test]
    fn mitigation_code_is_copy_and_zero_alloc_on_the_hot_path() {
        // A trivially-copyable tag: the governor's per-tick result carries this by
        // value with no heap allocation (the whole point of §8).
        let c = MitigationCode::RateClampEnforced { max_rate: 3.25 };
        let copied = c; // Copy, not move
        assert_eq!(c, copied);
    }
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
