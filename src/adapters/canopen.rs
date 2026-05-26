// src/adapters/canopen.rs

use serde::{Deserialize, Serialize};
use crate::gateway::policy::OperationalCommand;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CanOpenMessage {
    pub node_id: u8,
    pub function_code: u8,
    pub data: Vec<u8>,
    pub source_node: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanOpenMessageType {
    NMT,
    SYNC,
    Timestamp,
    Emergency,
    PDOTransmit,
    PDOReceive,
    SDOTransmit,
    SDOReceive,
    Heartbeat,
    LSS,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CanOpenEvaluation {
    pub command: OperationalCommand,
    pub message_type: CanOpenMessageType,
    pub node_id: u8,
    pub is_emergency: bool,
    pub emergency_code: Option<u16>,
    /// True if this NMT command transitions the node to stopped or pre-operational,
    /// indicating the caller should trigger a posture recalculation.
    pub triggers_recalculation: bool,
}

pub struct CanOpenAdapter;

impl CanOpenAdapter {
    pub fn evaluate(msg: &CanOpenMessage) -> CanOpenEvaluation {
        let (command, message_type) = match msg.function_code {
            0x0 => (OperationalCommand::SystemMutation, CanOpenMessageType::NMT),
            0x1 => (OperationalCommand::ReadTelemetry,  CanOpenMessageType::SYNC),
            0x2 => (OperationalCommand::ReadTelemetry,  CanOpenMessageType::Timestamp),
            0x3 => (OperationalCommand::ReadTelemetry,  CanOpenMessageType::Emergency),
            0x4 | 0x5 => (OperationalCommand::ReadTelemetry, CanOpenMessageType::PDOTransmit),
            0x6 | 0x7 => (OperationalCommand::WriteState,    CanOpenMessageType::PDOReceive),
            0x8 | 0x9 => (OperationalCommand::ReadTelemetry, CanOpenMessageType::SDOTransmit),
            0xA | 0xB => (OperationalCommand::WriteState,    CanOpenMessageType::SDOReceive),
            0xE => (OperationalCommand::ReadTelemetry, CanOpenMessageType::Heartbeat),
            _   => (OperationalCommand::Unknown,        CanOpenMessageType::Unknown),
        };

        let is_emergency = msg.function_code == 0x3;
        let emergency_code = if is_emergency && msg.data.len() >= 2 {
            Some(u16::from_le_bytes([msg.data[0], msg.data[1]]))
        } else {
            None
        };

        // NMT commands that take a node offline trigger posture recalculation.
        // NMT data[0] command specifier:
        //   0x02 = Stop Remote Node
        //   0x80 = Enter Pre-Operational
        //   0x81 = Reset Node
        //   0x82 = Reset Communication
        let triggers_recalculation = msg.function_code == 0x0
            && msg.data.first().map(|&b| matches!(b, 0x02 | 0x80 | 0x81 | 0x82)).unwrap_or(false);

        CanOpenEvaluation {
            command,
            message_type,
            node_id: msg.node_id,
            is_emergency,
            emergency_code,
            triggers_recalculation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(function_code: u8, data: Vec<u8>) -> CanOpenMessage {
        CanOpenMessage {
            node_id: 5,
            function_code,
            data,
            source_node: "can_bus_01".to_string(),
        }
    }

    #[test]
    fn test_nmt_maps_to_system_mutation() {
        let e = CanOpenAdapter::evaluate(&msg(0x0, vec![0x01, 0x00]));
        assert_eq!(e.command, OperationalCommand::SystemMutation);
        assert_eq!(e.message_type, CanOpenMessageType::NMT);
    }

    #[test]
    fn test_sync_maps_to_read_telemetry() {
        let e = CanOpenAdapter::evaluate(&msg(0x1, vec![]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        assert_eq!(e.message_type, CanOpenMessageType::SYNC);
    }

    #[test]
    fn test_emergency_sets_is_emergency_flag() {
        let e = CanOpenAdapter::evaluate(&msg(0x3, vec![0x10, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]));
        assert!(e.is_emergency);
        assert_eq!(e.emergency_code, Some(0x2010));
    }

    #[test]
    fn test_emergency_with_no_data_has_none_code() {
        let e = CanOpenAdapter::evaluate(&msg(0x3, vec![]));
        assert!(e.is_emergency);
        assert_eq!(e.emergency_code, None);
    }

    #[test]
    fn test_pdo_transmit_maps_to_read_telemetry() {
        for fc in [0x4u8, 0x5] {
            let e = CanOpenAdapter::evaluate(&msg(fc, vec![]));
            assert_eq!(e.command, OperationalCommand::ReadTelemetry);
            assert_eq!(e.message_type, CanOpenMessageType::PDOTransmit);
        }
    }

    #[test]
    fn test_pdo_receive_maps_to_write_state() {
        for fc in [0x6u8, 0x7] {
            let e = CanOpenAdapter::evaluate(&msg(fc, vec![]));
            assert_eq!(e.command, OperationalCommand::WriteState);
            assert_eq!(e.message_type, CanOpenMessageType::PDOReceive);
        }
    }

    #[test]
    fn test_sdo_transmit_maps_to_read_telemetry() {
        for fc in [0x8u8, 0x9] {
            let e = CanOpenAdapter::evaluate(&msg(fc, vec![]));
            assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        }
    }

    #[test]
    fn test_sdo_receive_maps_to_write_state() {
        for fc in [0xAu8, 0xB] {
            let e = CanOpenAdapter::evaluate(&msg(fc, vec![]));
            assert_eq!(e.command, OperationalCommand::WriteState);
        }
    }

    #[test]
    fn test_heartbeat_maps_to_read_telemetry() {
        let e = CanOpenAdapter::evaluate(&msg(0xE, vec![]));
        assert_eq!(e.command, OperationalCommand::ReadTelemetry);
        assert_eq!(e.message_type, CanOpenMessageType::Heartbeat);
    }

    #[test]
    fn test_unknown_function_code_maps_to_unknown() {
        let e = CanOpenAdapter::evaluate(&msg(0xF, vec![]));
        assert_eq!(e.command, OperationalCommand::Unknown);
        assert_eq!(e.message_type, CanOpenMessageType::Unknown);
    }

    #[test]
    fn test_canopen_nmt_stop_triggers_posture_recalculation() {
        // Stop Remote Node (0x02) must signal that posture recalculation is needed
        let e = CanOpenAdapter::evaluate(&msg(0x0, vec![0x02, 0x00]));
        assert!(e.triggers_recalculation, "NMT Stop must trigger recalculation");
    }

    #[test]
    fn test_nmt_pre_operational_triggers_recalculation() {
        let e = CanOpenAdapter::evaluate(&msg(0x0, vec![0x80, 0x00]));
        assert!(e.triggers_recalculation);
    }

    #[test]
    fn test_nmt_start_does_not_trigger_recalculation() {
        // Start Remote Node (0x01) = node coming online, not going offline
        let e = CanOpenAdapter::evaluate(&msg(0x0, vec![0x01, 0x00]));
        assert!(!e.triggers_recalculation);
    }

    #[test]
    fn test_max_node_id() {
        let m = CanOpenMessage { node_id: 127, function_code: 0xE, data: vec![], source_node: "n".to_string() };
        let e = CanOpenAdapter::evaluate(&m);
        assert_eq!(e.node_id, 127);
    }
}
