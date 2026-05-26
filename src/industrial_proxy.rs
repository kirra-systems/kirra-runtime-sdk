// src/industrial_proxy.rs
use crate::kirra_core::{KirraUnifiedGovernor, CausalFlightRecorder, GlobalSystemState, SafetyContractProfile};
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
pub trait ModbusTcpFrame {
    fn extract_transaction_id(&self) -> u16;
    fn extract_protocol_id(&self) -> u16;
    fn extract_length_field(&self) -> u16;
    fn extract_unit_id(&self) -> u8;
    fn extract_function_code(&self) -> u8;
    fn extract_target_register(&self) -> u32;
    fn extract_payload_demand(&self, profile: &SafetyContractProfile) -> Result<f64, &'static str>;
    fn construct_mutated_frame(&self, scaled_demand: f64, profile: &SafetyContractProfile) -> Vec<u8>;
    fn construct_exception_frame(&self, exception_code: u8) -> Vec<u8>;
}

impl ModbusTcpFrame for &[u8] {
    fn extract_transaction_id(&self) -> u16 { u16::from_be_bytes([self[0], self[1]]) }
    fn extract_protocol_id(&self) -> u16 { u16::from_be_bytes([self[2], self[3]]) }
    fn extract_length_field(&self) -> u16 { u16::from_be_bytes([self[4], self[5]]) }
    fn extract_unit_id(&self) -> u8 { self[6] }
    fn extract_function_code(&self) -> u8 { self[7] }
    fn extract_target_register(&self) -> u32 { u16::from_be_bytes([self[8], self[9]]) as u32 }

    fn extract_payload_demand(&self, profile: &SafetyContractProfile) -> Result<f64, &'static str> {
        if self.len() < 12 { return Err("FRAME_TRUNCATED"); }
        if self.extract_protocol_id() != 0 { return Err("INVALID_PROTOCOL_ID"); }
        if self.extract_function_code() != 6 { return Err("ILLEGAL_FUNCTION_CODE"); }

        if self.extract_target_register() != profile.asset_identifier {
            return Err("UNMONITORED_REGISTER_TARGET");
        }

        let raw_val = u16::from_be_bytes([self[10], self[11]]) as f64;
        let scale = if profile.engineering_scale_factor <= 0.0 { 1.0 } else { profile.engineering_scale_factor };

        Ok(raw_val / scale)
    }

    fn construct_mutated_frame(&self, scaled_demand: f64, profile: &SafetyContractProfile) -> Vec<u8> {
        let mut out = self.to_vec();
        let scale = if profile.engineering_scale_factor <= 0.0 { 1.0 } else { profile.engineering_scale_factor };

        let raw_counts = (scaled_demand * scale).round();
        let safe_u16 = (raw_counts.clamp(0.0, 65535.0) as u16).to_be_bytes();
        out[10] = safe_u16[0];
        out[11] = safe_u16[1];
        out
    }

    fn construct_exception_frame(&self, exception_code: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(10);
        let tx = self.extract_transaction_id().to_be_bytes();
        out.extend_from_slice(&tx);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&[0, 3]);
        out.push(self.extract_unit_id());
        out.push(self.extract_function_code() | 0x80);
        out.push(exception_code);
        out
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RealTimeTimingAnalytics {
    pub real_dt_ms: f64,
    pub target_dt_ms: f64,
    pub jitter_ms: f64,
    pub expected_allowed_delta: f64,
    pub actual_output_delta: f64,
    pub tolerance_epsilon: f64,
    pub within_tolerance: bool,
    pub timing_check_applicable: bool,
}

pub struct LiveProtocolSimulator {
    pub unified_governor: KirraUnifiedGovernor,
    pub recorder: CausalFlightRecorder,
}

pub struct ProxyResolutionSummary {
    pub outbound_bytes: Vec<u8>,
    pub was_mitigated: bool,
    pub was_unsafe_attempt: bool,
    pub was_rate_breached: bool,
    pub asset_in_safe_control_state: bool,
}

impl LiveProtocolSimulator {
    pub fn new(governor: KirraUnifiedGovernor) -> Self {
        Self {
            unified_governor: governor,
            recorder: CausalFlightRecorder::new(),
        }
    }

    pub fn process_wire_packet(&mut self, packet: &[u8], sim_time_ms: u64, dt: f64) -> ProxyResolutionSummary {
        let raw_demand = match packet.extract_payload_demand(&self.unified_governor.contract_config) {
            Ok(val) => val,
            Err(err) => {
                let exception_code = match err {
                    "ILLEGAL_FUNCTION_CODE" => 0x01,
                    "UNMONITORED_REGISTER_TARGET" => 0x02,
                    _ => 0x04,
                };
                return ProxyResolutionSummary {
                    outbound_bytes: packet.construct_exception_frame(exception_code),
                    was_mitigated: true,
                    was_unsafe_attempt: true,
                    was_rate_breached: false,
                    asset_in_safe_control_state: false,
                };
            }
        };

        let result = self.unified_governor.evaluate_demand_scalar(raw_demand, dt);
        let outbound_bytes = packet.construct_mutated_frame(result.sanitized_scalar, &self.unified_governor.contract_config);
        let was_mitigated = (result.sanitized_scalar - raw_demand).abs() > 0.001;

        let active_trust = self.unified_governor.trust_evaluator.mode;
        let system_state = match active_trust {
            crate::kirra_core::TrustMode::FullAutonomy => GlobalSystemState::Normal,
            crate::kirra_core::TrustMode::ConstrainedAdvisory | crate::kirra_core::TrustMode::ShadowMode => GlobalSystemState::Degraded,
            crate::kirra_core::TrustMode::LockedOut => GlobalSystemState::Failsafe,
        };

        self.recorder.log(
            sim_time_ms,
            "INDUSTRIAL_MASTER",
            &format!("TX_{}", packet.extract_transaction_id()),
            &format!("WRITE_REG_{}", packet.extract_target_register()),
            if was_mitigated { "MUTATED_CLAMP" } else { "TRANSPARENT" },
            system_state,
            active_trust,
            self.unified_governor.trust_evaluator.current_score,
            result.mitigation_narrative.clone(),
        );

        ProxyResolutionSummary {
            outbound_bytes,
            was_mitigated,
            was_unsafe_attempt: result.was_unsafe_attempt,
            was_rate_breached: result.was_rate_breached,
            asset_in_safe_control_state: result.asset_in_safe_control_state,
        }
    }
}
