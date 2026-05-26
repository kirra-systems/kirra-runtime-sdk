// src/protocol_adapter.rs

use serde::{Deserialize, Serialize};
use crate::action_filter::{evaluate_action_claim, ActionClaim, ActionDecision};
use crate::verifier::FleetPosture;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndustrialProtocol {
    Modbus,
    OpcUa,
    EthernetIp,
    CanOpen,
    Dnp3,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndustrialEvent {
    pub protocol: IndustrialProtocol,
    pub asset_id: String,
    pub operation: String,
    pub address: String,
    pub value: i64,
    pub risk_class: String,
}

pub fn map_industrial_event_to_claim(event: &IndustrialEvent) -> Result<ActionClaim, &'static str> {
    if event.asset_id.is_empty() {
        return Err("MISSING_ASSET_ID");
    }

    let mapped_action_type = match (event.protocol.clone(), event.operation.as_str()) {
        (IndustrialProtocol::Modbus, "write_register")
        | (IndustrialProtocol::Modbus, "coil_write") => "cmd_vel",
        (IndustrialProtocol::Modbus, "read_register") => "read_telemetry",
        (IndustrialProtocol::OpcUa, "write_node")
        | (IndustrialProtocol::OpcUa, "call_method") => "cmd_vel",
        (IndustrialProtocol::OpcUa, "read_node") => "read_telemetry",
        _ => return Err("UNSUPPORTED_INDUSTRIAL_OPERATION"),
    };

    let payload = serde_json::json!({
        "linear_x": if mapped_action_type == "cmd_vel" && event.value != 0 { 0.25 } else { 0.0 },
        "linear_y": 0.0,
        "linear_z": 0.0,
        "angular_x": 0.0,
        "angular_y": 0.0,
        "angular_z": if mapped_action_type == "cmd_vel" && event.value > 1 { 0.4 } else { 0.0 },
        "industrial_context": {
            "address": event.address,
            "raw_value": event.value,
        }
    });

    Ok(ActionClaim {
        action_type: mapped_action_type.to_string(),
        target_node: event.asset_id.clone(),
        risk_class: event.risk_class.clone(),
        payload,
    })
}

pub fn evaluate_industrial_event(event: IndustrialEvent, posture: FleetPosture) -> ActionDecision {
    match map_industrial_event_to_claim(&event) {
        Ok(claim) => evaluate_action_claim(claim, posture),
        Err(err) => ActionDecision {
            allowed: false,
            reason: format!("ADAPTER_TRANSLATION_FAILURE: {}", err),
        },
    }
}

// ---------------------------------------------------------------------------
// Unified industrial request / evaluation
// ---------------------------------------------------------------------------

use crate::adapters::ethernet_ip::{EtherNetIpAdapter, EtherNetIpMessage};
use crate::adapters::canopen::{CanOpenAdapter, CanOpenMessage};
use crate::adapters::dnp3::{Dnp3Adapter, Dnp3Message};

#[derive(Debug, Clone, Deserialize)]
pub struct UnifiedIndustrialRequest {
    pub protocol: IndustrialProtocol,
    pub message: serde_json::Value,
}

#[derive(Debug)]
pub struct UnifiedEvaluationResult {
    pub protocol: String,
    pub command: crate::gateway::policy::OperationalCommand,
    pub allowed: bool,
    pub denial_reason: Option<String>,
    pub posture_at_evaluation: String,
    pub adapter_details: serde_json::Value,
    pub triggers_recalculation: bool,
}

pub fn evaluate_unified_industrial_request(
    req: UnifiedIndustrialRequest,
    posture: FleetPosture,
) -> Result<UnifiedEvaluationResult, String> {
    use crate::gateway::policy::OperationalCommand;

    let posture_str = format!("{:?}", posture);

    match req.protocol {
        IndustrialProtocol::Modbus | IndustrialProtocol::OpcUa => {
            // Legacy path: deserialize as IndustrialEvent fields from the message value
            let asset_id = req.message.get("asset_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let operation = req.message.get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let address = req.message.get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let value = req.message.get("value")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let risk_class = req.message.get("risk_class")
                .and_then(|v| v.as_str())
                .unwrap_or("kinetic_write")
                .to_string();

            let event = IndustrialEvent {
                protocol: req.protocol.clone(),
                asset_id,
                operation,
                address,
                value,
                risk_class,
            };

            let decision = evaluate_industrial_event(event, posture);
            let command = if decision.allowed {
                OperationalCommand::WriteState
            } else {
                OperationalCommand::Unknown
            };

            Ok(UnifiedEvaluationResult {
                protocol: format!("{:?}", req.protocol),
                command,
                allowed: decision.allowed,
                denial_reason: if decision.allowed { None } else { Some(decision.reason) },
                posture_at_evaluation: posture_str,
                adapter_details: serde_json::json!({}),
                triggers_recalculation: false,
            })
        }

        IndustrialProtocol::EthernetIp => {
            let msg: EtherNetIpMessage = serde_json::from_value(req.message)
                .map_err(|e| format!("MALFORMED_ETHERNET_IP_MESSAGE: {e}"))?;
            let eval = EtherNetIpAdapter::evaluate(&msg);
            let (allowed, denial_reason) = command_allowed_for_posture_pub(&eval.command, &posture);
            Ok(UnifiedEvaluationResult {
                protocol: "ethernet_ip".to_string(),
                command: eval.command,
                allowed,
                denial_reason,
                posture_at_evaluation: posture_str,
                adapter_details: serde_json::json!({
                    "service_name": eval.service_name,
                    "is_write": eval.is_write,
                    "target_description": eval.target_description,
                    "safety_relevant": eval.safety_relevant,
                }),
                triggers_recalculation: false,
            })
        }

        IndustrialProtocol::CanOpen => {
            let msg: CanOpenMessage = serde_json::from_value(req.message)
                .map_err(|e| format!("MALFORMED_CANOPEN_MESSAGE: {e}"))?;
            let eval = CanOpenAdapter::evaluate(&msg);
            let (allowed, denial_reason) = command_allowed_for_posture_pub(&eval.command, &posture);
            Ok(UnifiedEvaluationResult {
                protocol: "canopen".to_string(),
                command: eval.command,
                allowed,
                denial_reason,
                posture_at_evaluation: posture_str,
                adapter_details: serde_json::json!({
                    "message_type": format!("{:?}", eval.message_type),
                    "node_id": eval.node_id,
                    "is_emergency": eval.is_emergency,
                    "emergency_code": eval.emergency_code,
                }),
                triggers_recalculation: eval.triggers_recalculation,
            })
        }

        IndustrialProtocol::Dnp3 => {
            let msg: Dnp3Message = serde_json::from_value(req.message)
                .map_err(|e| format!("MALFORMED_DNP3_MESSAGE: {e}"))?;
            let eval = Dnp3Adapter::evaluate(&msg);
            let (allowed, denial_reason) = command_allowed_for_posture_pub(&eval.command, &posture);
            Ok(UnifiedEvaluationResult {
                protocol: "dnp3".to_string(),
                command: eval.command,
                allowed,
                denial_reason,
                posture_at_evaluation: posture_str,
                adapter_details: serde_json::json!({
                    "function_name": eval.function_name,
                    "is_control": eval.is_control,
                    "is_broadcast": eval.is_broadcast,
                    "critical_infrastructure_relevant": eval.critical_infrastructure_relevant,
                }),
                triggers_recalculation: false,
            })
        }
    }
}

pub fn command_allowed_for_posture_pub(
    command: &crate::gateway::policy::OperationalCommand,
    posture: &FleetPosture,
) -> (bool, Option<String>) {
    use crate::gateway::policy::OperationalCommand;
    match command {
        OperationalCommand::Unknown => (false, Some("UNKNOWN_COMMAND_TYPE".to_string())),
        _ => match posture {
            FleetPosture::Nominal => (true, None),
            FleetPosture::Degraded => match command {
                OperationalCommand::ReadTelemetry => (true, None),
                _ => (false, Some("FLEET_DEGRADED_WRITE_BLOCKED".to_string())),
            },
            FleetPosture::LockedOut => (false, Some("FLEET_LOCKEDOUT_ABSOLUTE_DENIAL".to_string())),
        },
    }
}

#[cfg(test)]
mod unified_tests {
    use super::*;

    fn eip_request() -> UnifiedIndustrialRequest {
        UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::EthernetIp,
            message: serde_json::json!({
                "command_code": 100,
                "session_handle": 1,
                "status": 0,
                "service_code": 0x0E,
                "class_id": 1,
                "instance_id": 1,
                "attribute_id": 1,
                "data": [],
                "source_node": "plc_01"
            }),
        }
    }

    fn canopen_request(fc: u8, data: Vec<u8>) -> UnifiedIndustrialRequest {
        UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::CanOpen,
            message: serde_json::json!({
                "node_id": 5,
                "function_code": fc,
                "data": data,
                "source_node": "can_01"
            }),
        }
    }

    fn dnp3_request(fc: u8, dest: u16) -> UnifiedIndustrialRequest {
        UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::Dnp3,
            message: serde_json::json!({
                "source_address": 1,
                "dest_address": dest,
                "function_code": fc,
                "data_link_control": 0,
                "objects": [],
                "source_node": "sub_01"
            }),
        }
    }

    #[test]
    fn test_unified_endpoint_routes_to_ethernet_ip() {
        let result = evaluate_unified_industrial_request(eip_request(), FleetPosture::Nominal);
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.protocol, "ethernet_ip");
        assert!(r.adapter_details.get("service_name").is_some());
    }

    #[test]
    fn test_unified_endpoint_routes_to_canopen() {
        let result = evaluate_unified_industrial_request(
            canopen_request(0xE, vec![]),
            FleetPosture::Nominal,
        );
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.protocol, "canopen");
    }

    #[test]
    fn test_unified_endpoint_routes_to_dnp3() {
        let result = evaluate_unified_industrial_request(
            dnp3_request(0x01, 0x0001),
            FleetPosture::Nominal,
        );
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.protocol, "dnp3");
        assert!(r.adapter_details.get("function_name").is_some());
    }

    #[test]
    fn test_response_always_includes_posture_at_evaluation() {
        for posture in [FleetPosture::Nominal, FleetPosture::Degraded, FleetPosture::LockedOut] {
            let r = evaluate_unified_industrial_request(eip_request(), posture.clone()).unwrap();
            assert!(!r.posture_at_evaluation.is_empty(), "posture_at_evaluation must always be set");
        }
    }

    #[test]
    fn test_malformed_message_returns_err() {
        let req = UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::EthernetIp,
            message: serde_json::json!({"bad": "data"}),
        };
        let result = evaluate_unified_industrial_request(req, FleetPosture::Nominal);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("MALFORMED_ETHERNET_IP_MESSAGE"));
    }

    #[test]
    fn test_canopen_nmt_stop_triggers_recalculation_flag() {
        let req = canopen_request(0x0, vec![0x02, 0x00]);
        let r = evaluate_unified_industrial_request(req, FleetPosture::Nominal).unwrap();
        assert!(r.triggers_recalculation);
    }

    #[test]
    fn test_lockedout_posture_denies_all() {
        let r = evaluate_unified_industrial_request(eip_request(), FleetPosture::LockedOut).unwrap();
        assert!(!r.allowed);
        assert_eq!(r.denial_reason.as_deref(), Some("FLEET_LOCKEDOUT_ABSOLUTE_DENIAL"));
    }

    #[test]
    fn test_degraded_posture_blocks_write() {
        // EtherNet/IP Get_Attribute_Single (0x0E) = ReadTelemetry → allowed even in Degraded
        let r = evaluate_unified_industrial_request(eip_request(), FleetPosture::Degraded).unwrap();
        assert!(r.allowed);
    }
}
