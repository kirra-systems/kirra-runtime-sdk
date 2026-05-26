// src/adapters/dnp3.rs

use serde::{Deserialize, Serialize};
use crate::gateway::policy::OperationalCommand;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dnp3Message {
    pub source_address: u16,
    pub dest_address: u16,
    pub function_code: u8,
    pub data_link_control: u8,
    pub objects: Vec<Dnp3Object>,
    pub source_node: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dnp3Object {
    pub group: u8,
    pub variation: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Dnp3Evaluation {
    pub command: OperationalCommand,
    pub function_name: String,
    pub is_control: bool,
    pub is_broadcast: bool,
    pub critical_infrastructure_relevant: bool,
}

pub struct Dnp3Adapter;

// DNP3 broadcast address per IEEE 1815
pub const DNP3_BROADCAST_ADDRESS: u16 = 0xFFFF;

impl Dnp3Adapter {
    pub fn evaluate(msg: &Dnp3Message) -> Dnp3Evaluation {
        let (command, function_name) = match msg.function_code {
            0x01 => (OperationalCommand::ReadTelemetry,  "Read"),
            0x02 => (OperationalCommand::WriteState,     "Write"),
            0x03 => (OperationalCommand::WriteState,     "Select"),
            0x04 => (OperationalCommand::SystemMutation, "Operate"),
            0x05 => (OperationalCommand::SystemMutation, "Direct_Operate"),
            0x06 => (OperationalCommand::SystemMutation, "Direct_Operate_NR"),
            0x07 => (OperationalCommand::WriteState,     "Freeze"),
            0x08 => (OperationalCommand::WriteState,     "Freeze_NR"),
            0x81 => (OperationalCommand::ReadTelemetry,  "Response"),
            0x82 => (OperationalCommand::ReadTelemetry,  "Unsolicited_Response"),
            _    => (OperationalCommand::Unknown,         "Unknown"),
        };

        let is_broadcast = msg.dest_address == DNP3_BROADCAST_ADDRESS;

        // CROB (Group 12), Analog Output (41-42), Octet String (110-113)
        let critical_infrastructure_relevant = msg.objects.iter().any(|obj| {
            matches!(obj.group, 12 | 41 | 42 | 110 | 111 | 112 | 113)
        });

        // Control commands with CROB or Analog Output objects are actuator writes
        let is_control = matches!(msg.function_code, 0x03..=0x06)
            && msg.objects.iter().any(|obj| matches!(obj.group, 12 | 41 | 42));

        Dnp3Evaluation {
            command,
            function_name: function_name.to_string(),
            is_control,
            is_broadcast,
            critical_infrastructure_relevant,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(function_code: u8, dest_address: u16, objects: Vec<Dnp3Object>) -> Dnp3Message {
        Dnp3Message {
            source_address: 0x0001,
            dest_address,
            function_code,
            data_link_control: 0,
            objects,
            source_node: "substation_01".to_string(),
        }
    }

    fn obj(group: u8) -> Dnp3Object {
        Dnp3Object { group, variation: 1, data: vec![] }
    }

    #[test]
    fn test_read_maps_to_read_telemetry() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        assert_eq!(e.function_name, "Read");
    }

    #[test]
    fn test_write_maps_to_write_state() {
        let e = Dnp3Adapter::evaluate(&msg(0x02, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::WriteState);
    }

    #[test]
    fn test_select_maps_to_write_state() {
        let e = Dnp3Adapter::evaluate(&msg(0x03, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::WriteState);
    }

    #[test]
    fn test_operate_maps_to_system_mutation() {
        let e = Dnp3Adapter::evaluate(&msg(0x04, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::SystemMutation);
    }

    #[test]
    fn test_direct_operate_maps_to_system_mutation() {
        let e = Dnp3Adapter::evaluate(&msg(0x05, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::SystemMutation);
        assert_eq!(e.function_name, "Direct_Operate");
    }

    #[test]
    fn test_direct_operate_nr_maps_to_system_mutation() {
        let e = Dnp3Adapter::evaluate(&msg(0x06, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::SystemMutation);
    }

    #[test]
    fn test_freeze_maps_to_write_state() {
        let e = Dnp3Adapter::evaluate(&msg(0x07, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::WriteState);
    }

    #[test]
    fn test_response_maps_to_read_telemetry() {
        let e = Dnp3Adapter::evaluate(&msg(0x81, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
    }

    #[test]
    fn test_unsolicited_response_maps_to_read_telemetry() {
        let e = Dnp3Adapter::evaluate(&msg(0x82, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
    }

    #[test]
    fn test_unknown_function_code_maps_to_unknown() {
        let e = Dnp3Adapter::evaluate(&msg(0xAB, 0x0001, vec![]));
        assert_eq!(e.command, OperationalCommand::Unknown);
    }

    #[test]
    fn test_broadcast_address_detected() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0xFFFF, vec![]));
        assert!(e.is_broadcast);
    }

    #[test]
    fn test_non_broadcast_address_not_flagged() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![]));
        assert!(!e.is_broadcast);
    }

    #[test]
    fn test_crob_group_12_flags_critical_infrastructure() {
        let e = Dnp3Adapter::evaluate(&msg(0x05, 0x0001, vec![obj(12)]));
        assert!(e.critical_infrastructure_relevant);
    }

    #[test]
    fn test_analog_output_group_41_flags_critical_infrastructure() {
        let e = Dnp3Adapter::evaluate(&msg(0x04, 0x0001, vec![obj(41)]));
        assert!(e.critical_infrastructure_relevant);
    }

    #[test]
    fn test_analog_output_event_group_42_flags_critical() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![obj(42)]));
        assert!(e.critical_infrastructure_relevant);
    }

    #[test]
    fn test_octet_string_groups_flag_critical() {
        for group in [110u8, 111, 112, 113] {
            let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![obj(group)]));
            assert!(e.critical_infrastructure_relevant, "group {group} must be critical");
        }
    }

    #[test]
    fn test_no_critical_objects_not_flagged() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![obj(1), obj(2)]));
        assert!(!e.critical_infrastructure_relevant);
    }

    #[test]
    fn test_is_control_with_crob_and_operate() {
        let e = Dnp3Adapter::evaluate(&msg(0x05, 0x0001, vec![obj(12)]));
        assert!(e.is_control);
    }

    #[test]
    fn test_read_with_crob_is_not_control() {
        // Read FC (0x01) with CROB objects is not a control
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![obj(12)]));
        assert!(!e.is_control);
    }

    #[test]
    fn test_zero_length_data_in_object() {
        let e = Dnp3Adapter::evaluate(&msg(0x01, 0x0001, vec![obj(1)]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
    }
}
