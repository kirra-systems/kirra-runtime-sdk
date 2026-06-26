// src/adapters/ethernet_ip.rs

use serde::{Deserialize, Serialize};
use crate::gateway::policy::OperationalCommand;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EtherNetIpMessage {
    pub command_code: u16,
    pub session_handle: u32,
    pub status: u32,
    pub service_code: u8,
    pub class_id: u16,
    pub instance_id: u16,
    pub attribute_id: u16,
    pub data: Vec<u8>,
    pub source_node: String,
}

#[derive(Debug, Clone)]
pub struct EtherNetIpEvaluation {
    pub command: OperationalCommand,
    pub service_name: String,
    pub is_write: bool,
    pub target_description: String,
    pub safety_relevant: bool,
}

pub struct EtherNetIpAdapter;

impl EtherNetIpAdapter {
    pub fn evaluate(msg: &EtherNetIpMessage) -> EtherNetIpEvaluation {
        let (command, service_name, is_write) = match msg.service_code {
            0x0E => (OperationalCommand::ReadTelemetry, "Get_Attribute_Single", false),
            0x10 => (OperationalCommand::WriteState,    "Set_Attribute_Single", true),
            0x4B => (OperationalCommand::SystemMutation, "Execute_Service",     true),
            0x4C => (OperationalCommand::ReadTelemetry, "Read_Tag",             false),
            0x4D => (OperationalCommand::WriteState,    "Write_Tag",            true),
            _    => (OperationalCommand::Unknown,       "Unknown_Service",      false),
        };

        // Safety-relevant CIP classes: Safety Supervisor, Safety Validator,
        // Safety Analog Input Point, Safety Analog Output Point
        let safety_relevant = matches!(msg.class_id, 0x29 | 0x2A | 0x3B | 0x3C);

        let target_description = format!(
            "class=0x{:02X} instance=0x{:02X} attr=0x{:02X}",
            msg.class_id, msg.instance_id, msg.attribute_id
        );

        EtherNetIpEvaluation {
            command,
            service_name: service_name.to_string(),
            is_write,
            target_description,
            safety_relevant,
        }
    }
}

impl crate::adapters::IndustrialAdapter for EtherNetIpAdapter {
    type Message = EtherNetIpMessage;
    const PROTOCOL: &'static str = "ethernet_ip";

    fn verdict(msg: &EtherNetIpMessage) -> crate::adapters::AdapterVerdict {
        let e = EtherNetIpAdapter::evaluate(msg);
        crate::adapters::AdapterVerdict {
            command: e.command,
            details: serde_json::json!({
                "service_name": e.service_name,
                "is_write": e.is_write,
                "target_description": e.target_description,
                "safety_relevant": e.safety_relevant,
            }),
            triggers_recalculation: false,
        }
    }

    // POSTURE-ONLY (no override): a CIP write (Set_Attribute_Single / Write_Tag)
    // carries `data` whose DATA TYPE — and thus value width, signedness, and
    // scaling — is defined by the target attribute in the device's EDS/object
    // model, NOT in the frame. Decoding a scalar magnitude from `data` without
    // that per-attribute type config would be FABRICATION (#85), so EtherNet-IP
    // stays bounded by posture/classification only until a per-target CIP type
    // map exists (tracked follow-up). The trait's fail-closed default applies.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(service_code: u8, class_id: u16) -> EtherNetIpMessage {
        EtherNetIpMessage {
            command_code: 0x0065,
            session_handle: 1,
            status: 0,
            service_code,
            class_id,
            instance_id: 1,
            attribute_id: 1,
            data: vec![],
            source_node: "plc_01".to_string(),
        }
    }

    #[test]
    fn test_get_attribute_maps_to_read_telemetry() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x0E, 0x01));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        assert_eq!(e.service_name, "Get_Attribute_Single");
        assert!(!e.is_write);
    }

    #[test]
    fn test_set_attribute_maps_to_write_state() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x10, 0x01));
        assert_eq!(e.command, OperationalCommand::WriteState);
        assert!(e.is_write);
    }

    #[test]
    fn test_execute_service_maps_to_system_mutation() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x4B, 0x01));
        assert_eq!(e.command, OperationalCommand::SystemMutation);
    }

    #[test]
    fn test_read_tag_maps_to_read_telemetry() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x4C, 0x01));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        assert!(!e.is_write);
    }

    #[test]
    fn test_write_tag_maps_to_write_state() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x4D, 0x01));
        assert_eq!(e.command, OperationalCommand::WriteState);
        assert!(e.is_write);
    }

    #[test]
    fn test_unknown_service_code_maps_to_unknown() {
        let e = EtherNetIpAdapter::evaluate(&msg(0xFF, 0x01));
        assert_eq!(e.command, OperationalCommand::Unknown);
        assert_eq!(e.service_name, "Unknown_Service");
    }

    #[test]
    fn test_safety_relevant_classes_flagged() {
        for class in [0x29u16, 0x2A, 0x3B, 0x3C] {
            let e = EtherNetIpAdapter::evaluate(&msg(0x0E, class));
            assert!(e.safety_relevant, "class 0x{class:02X} must be safety_relevant");
        }
    }

    #[test]
    fn test_non_safety_class_not_flagged() {
        let e = EtherNetIpAdapter::evaluate(&msg(0x0E, 0x01));
        assert!(!e.safety_relevant);
    }

    #[test]
    fn test_empty_data_does_not_panic() {
        let mut m = msg(0x0E, 0x01);
        m.data = vec![];
        let _ = EtherNetIpAdapter::evaluate(&m);
    }

    #[test]
    fn test_target_description_format() {
        let mut m = msg(0x0E, 0x29);
        m.instance_id = 0x05;
        m.attribute_id = 0x0A;
        let e = EtherNetIpAdapter::evaluate(&m);
        assert!(e.target_description.contains("0x29"));
        assert!(e.target_description.contains("0x05"));
        assert!(e.target_description.contains("0x0A"));
    }
}
