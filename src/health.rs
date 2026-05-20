// src/health.rs

use crate::TrustMode;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DeploymentLivenessStatus {
    pub process_alive: bool,
    pub allocation_state_stable: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DeploymentReadinessStatus {
    pub can_accept_ingress_traffic: bool,
    pub plc_egress_link_connected: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct FunctionalSafetyPosture {
    pub active_trust_mode: TrustMode,
    pub fail_closed_active: bool,
    pub tracking_metrics_observational_only: bool,
}

pub struct EnterpriseHAEngine { pub node_identifier: String }

impl EnterpriseHAEngine {
    pub fn new(node_id: &str) -> Self { Self { node_identifier: node_id.to_string() } }

    #[inline]
    pub fn evaluate_liveness_probe(&self) -> DeploymentLivenessStatus {
        DeploymentLivenessStatus { process_alive: true, allocation_state_stable: true }
    }

    #[inline]
    pub fn evaluate_readiness_probe(&self, network_stalled: bool, active_connections: u32) -> DeploymentReadinessStatus {
        DeploymentReadinessStatus {
            can_accept_ingress_traffic: !network_stalled && active_connections < 120,
            plc_egress_link_connected: !network_stalled,
        }
    }

    #[inline]
    pub fn inspect_safety_posture_state(&self, trust_mode: TrustMode) -> FunctionalSafetyPosture {
        FunctionalSafetyPosture {
            active_trust_mode: trust_mode,
            fail_closed_active: trust_mode == TrustMode::LockedOut,
            tracking_metrics_observational_only: true,
        }
    }
}
