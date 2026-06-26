// src/adapters/canopen.rs

use std::collections::HashMap;
use std::sync::OnceLock;

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

impl crate::adapters::IndustrialAdapter for CanOpenAdapter {
    type Message = CanOpenMessage;
    const PROTOCOL: &'static str = "canopen";

    fn verdict(msg: &CanOpenMessage) -> crate::adapters::AdapterVerdict {
        let e = CanOpenAdapter::evaluate(msg);
        crate::adapters::AdapterVerdict {
            command: e.command,
            details: serde_json::json!({
                "message_type": format!("{:?}", e.message_type),
                "node_id": e.node_id,
                "is_emergency": e.is_emergency,
                "emergency_code": e.emergency_code,
            }),
            triggers_recalculation: e.triggers_recalculation,
        }
    }

    // POSTURE-ONLY (no override): a CANopen PDO carries raw process data and an
    // SDO download's command byte self-describes only the value WIDTH (1/2/4
    // bytes) — never its TYPE (signed/unsigned, int/float) or scaling, which are
    // defined by the node's object dictionary, NOT the frame. Numerically
    // bounding a width-N byte run without that per-object type would FABRICATE the
    // value's interpretation (#85). So CANopen stays bounded by posture only until
    // a per-(node,index,subindex) object-dictionary type+envelope config exists
    // (tracked follow-up). Contrast DNP3 group-41, whose variation byte DOES carry
    // the type (i16/i32/f32/f64) — which is why only DNP3 overrides this today.
}

/// Env var carrying the CANopen-node-id → fleet-node-id map (#84).
/// Format: comma-separated `canid:fleet_node_id` pairs, e.g.
/// `5:robot-01,6:robot-02`. The map is CONFIG-sourced — there is no
/// hardcoded table; an unset/empty var yields an empty map (every offline
/// is then unattributed, handled fail-closed by the caller).
pub const CANOPEN_NODE_MAP_ENV: &str = "KIRRA_CANOPEN_NODE_MAP";

/// CANopen bus-address (`u8` node-id) → fleet-node-id (`String`) resolver.
///
/// The fleet-node-id is the key into the verifier's node registry
/// (`AppState::nodes`) and DAG — resolving it is what lets an NMT-offline
/// event mark the CORRECT asset so the posture recalc is effectful.
#[derive(Debug, Clone, Default)]
pub struct CanOpenNodeMap {
    map: HashMap<u8, String>,
}

impl CanOpenNodeMap {
    /// Parse the `canid:fleet_node` comma-separated config spec. Malformed
    /// or out-of-range entries are skipped with a warning (never panic);
    /// a later duplicate canid overrides an earlier one.
    pub fn from_spec(spec: &str) -> Self {
        let mut map = HashMap::new();
        for raw in spec.split(',') {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((id_str, node)) = entry.split_once(':') else {
                tracing::warn!(entry, "CANopen node-map: skipping malformed entry (expected `canid:fleet_node`)");
                continue;
            };
            let node = node.trim();
            match id_str.trim().parse::<u8>() {
                Ok(id) if !node.is_empty() => {
                    map.insert(id, node.to_string());
                }
                _ => {
                    tracing::warn!(entry, "CANopen node-map: skipping entry with invalid node-id (0-255) or empty fleet-node");
                }
            }
        }
        Self { map }
    }

    /// Resolve a CANopen node-id to its fleet-node-id, if mapped.
    pub fn resolve(&self, node_id: u8) -> Option<&str> {
        self.map.get(&node_id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Process-wide CANopen node map, initialized once at startup from the
/// environment. Kept out of `ServiceState`/`AppState` so this adapter-layer
/// fix needs no change to their (many) construction sites.
static GLOBAL_NODE_MAP: OnceLock<CanOpenNodeMap> = OnceLock::new();

/// Initialize the global CANopen node map from `KIRRA_CANOPEN_NODE_MAP`.
/// Idempotent — a second call is a no-op. Call once during startup.
pub fn init_node_map_from_env() {
    let spec = std::env::var(CANOPEN_NODE_MAP_ENV).unwrap_or_default();
    let map = CanOpenNodeMap::from_spec(&spec);
    let count = map.len();
    if GLOBAL_NODE_MAP.set(map).is_ok() {
        tracing::info!(entries = count, "CANopen node map initialized from env");
    }
}

/// Resolve a CANopen node-id against the global map. Returns `None` when the
/// map is uninitialized or has no entry for the id (fail-closed: the caller
/// treats `None` as an unattributed offline, never a silent no-op).
pub fn global_resolve(node_id: u8) -> Option<String> {
    GLOBAL_NODE_MAP
        .get()
        .and_then(|m| m.resolve(node_id))
        .map(str::to_string)
}

/// Why an NMT-offline could not be attributed to a fleet node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnattributedReason {
    /// No CANopen→fleet mapping exists for this bus node-id.
    NoMapping,
    /// The mapping exists but names a node not in the verifier registry.
    NodeNotRegistered,
}

/// Disposition of an NMT node-offline event after fleet-node resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NmtOfflineOutcome {
    /// Resolved to a registered fleet node — mark it offline (effectful recalc).
    Attributed { fleet_node_id: String },
    /// Could not be attributed — handle fail-closed (surface, never drop).
    Unattributed { canopen_node_id: u8, reason: UnattributedReason },
}

/// Classify an NMT-offline event given the resolved fleet node (if any) and
/// whether that node is registered in the verifier. Pure + total: a node-id
/// that resolves to a registered node is `Attributed`; everything else is
/// `Unattributed` with a reason — there is no silent-drop path.
pub fn classify_nmt_offline(
    canopen_node_id: u8,
    resolved: Option<String>,
    node_registered: bool,
) -> NmtOfflineOutcome {
    match resolved {
        Some(fleet_node_id) if node_registered => NmtOfflineOutcome::Attributed { fleet_node_id },
        Some(_) => NmtOfflineOutcome::Unattributed {
            canopen_node_id,
            reason: UnattributedReason::NodeNotRegistered,
        },
        None => NmtOfflineOutcome::Unattributed {
            canopen_node_id,
            reason: UnattributedReason::NoMapping,
        },
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

    // --- #84: CANopen-node-id → fleet-node mapping + fail-closed classification ---

    #[test]
    fn test_node_map_parses_spec_and_resolves() {
        let m = CanOpenNodeMap::from_spec("5:robot-01, 6 : robot-02 ,127:drone-09");
        assert_eq!(m.len(), 3);
        assert_eq!(m.resolve(5), Some("robot-01"));
        assert_eq!(m.resolve(6), Some("robot-02"));
        assert_eq!(m.resolve(127), Some("drone-09"));
        assert_eq!(m.resolve(9), None, "unmapped id resolves to None");
    }

    #[test]
    fn test_node_map_skips_malformed_entries_without_panicking() {
        // missing colon, non-numeric id, out-of-range id, empty node, blanks
        let m = CanOpenNodeMap::from_spec("garbage,xx:n,300:n,7:,, 8:robot-08 ,");
        assert_eq!(m.resolve(8), Some("robot-08"));
        assert_eq!(m.len(), 1, "only the one well-formed entry survives");
    }

    #[test]
    fn test_empty_spec_is_empty_map() {
        assert!(CanOpenNodeMap::from_spec("").is_empty());
        assert!(CanOpenNodeMap::from_spec("   ").is_empty());
    }

    #[test]
    fn test_classify_mapped_registered_is_attributed() {
        let out = classify_nmt_offline(5, Some("robot-01".to_string()), true);
        assert_eq!(out, NmtOfflineOutcome::Attributed { fleet_node_id: "robot-01".to_string() });
    }

    #[test]
    fn test_classify_unmapped_is_unattributed_no_mapping() {
        // FAIL-CLOSED: an unmapped offline is never dropped — it is classified
        // Unattributed(NoMapping) for the caller to surface.
        let out = classify_nmt_offline(99, None, false);
        assert_eq!(
            out,
            NmtOfflineOutcome::Unattributed { canopen_node_id: 99, reason: UnattributedReason::NoMapping }
        );
    }

    #[test]
    fn test_classify_mapped_but_unregistered_is_unattributed() {
        // A mapping that points at a node the verifier doesn't know is also
        // fail-closed — we cannot attribute the offline to a real asset.
        let out = classify_nmt_offline(5, Some("ghost".to_string()), false);
        assert_eq!(
            out,
            NmtOfflineOutcome::Unattributed { canopen_node_id: 5, reason: UnattributedReason::NodeNotRegistered }
        );
    }
}
