pub mod canopen;
pub mod dnp3;
pub mod ethernet_ip;

use crate::gateway::policy::OperationalCommand;

// ---------------------------------------------------------------------------
// IndustrialAdapter — one trait for every binary-frame industrial protocol
// (DNP3 / CANopen / EtherNet-IP). It replaces the three near-identical arms of
// `evaluate_unified_industrial_request` with a single generic dispatch
// (`protocol_adapter::dispatch_adapter`), and gives the magnitude bound a
// uniform seam (`bound_magnitude`) so every protocol enforces it the same way —
// or documents, fail-closed, why it cannot. (The Modbus/OPC-UA legacy path maps
// through `IndustrialEvent` / action-claims and keeps its own arm.)
// ---------------------------------------------------------------------------

/// The protocol-agnostic result of classifying one industrial message. Each
/// adapter folds its protocol-specific detail into `details` (the JSON that was
/// previously built inline in `protocol_adapter`), so the dispatch is uniform.
#[derive(Debug, Clone)]
pub struct AdapterVerdict {
    /// The posture-gated command class (`ReadTelemetry` / `WriteState` /
    /// `SystemMutation` / `Unknown`).
    pub command: OperationalCommand,
    /// Protocol-specific observability detail for the evaluation result.
    pub details: serde_json::Value,
    /// True when this message takes a node offline / changes topology and the
    /// caller should trigger a posture recalculation.
    pub triggers_recalculation: bool,
}

/// One binary-frame industrial protocol adapter. Implemented as associated
/// functions (no `&self`) — adapters are zero-sized dispatch tags.
pub trait IndustrialAdapter {
    /// The wire message this adapter decodes (deserialized from the request JSON).
    type Message: serde::de::DeserializeOwned;

    /// Stable lower-snake protocol label (e.g. `"dnp3"`). Also drives the
    /// `MALFORMED_<PROTOCOL>_MESSAGE` deserialize-error reason (uppercased).
    const PROTOCOL: &'static str;

    /// Classify a message into a posture-gated command + observability detail.
    fn verdict(msg: &Self::Message) -> AdapterVerdict;

    /// MAGNITUDE BOUND — a posture-admitted WRITE/CONTROL must also have its
    /// commanded *value* bounded, not just its *type* classified. Fail-closed,
    /// per the "faithfully decode or refuse, never fabricate" discipline (#85).
    ///
    /// Default `Ok(())` = "this protocol carries no faithfully-decodable scalar
    /// magnitude in the message itself" (e.g. EtherNet-IP CIP, where the value's
    /// data type lives in the target attribute's device config, not the frame).
    /// Such a protocol stays posture-only until a per-target type config exists;
    /// overriding adapters (DNP3, CANopen) bound what the wire format self-describes.
    fn bound_magnitude(_msg: &Self::Message) -> Result<(), &'static str> {
        Ok(())
    }
}

#[cfg(test)]
mod adapter_trait_tests {
    use super::*;
    use crate::adapters::dnp3::{Dnp3Adapter, Dnp3Message};
    use crate::adapters::ethernet_ip::{EtherNetIpAdapter, EtherNetIpMessage};

    #[test]
    fn dnp3_verdict_via_trait_classifies_and_bound_is_noop_for_read() {
        let msg = Dnp3Message {
            source_address: 1,
            dest_address: 2,
            function_code: 0x01, // Read
            data_link_control: 0,
            objects: vec![],
            source_node: "rtu".into(),
        };
        let v = <Dnp3Adapter as IndustrialAdapter>::verdict(&msg);
        assert_eq!(v.command, OperationalCommand::ReadTelemetry);
        assert_eq!(Dnp3Adapter::PROTOCOL, "dnp3");
        // A read carries no commanded setpoint → no magnitude bound applies.
        assert!(<Dnp3Adapter as IndustrialAdapter>::bound_magnitude(&msg).is_ok());
    }

    #[test]
    fn ethernet_ip_write_is_posture_only_bound_default_ok() {
        // A CIP Set_Attribute_Single write classifies as WriteState, but its value
        // TYPE is in device config, not the frame — so bound_magnitude is the
        // fail-closed default no-op (posture-only; never fabricates a magnitude).
        let msg = EtherNetIpMessage {
            command_code: 0x65,
            session_handle: 1,
            status: 0,
            service_code: 0x10, // Set_Attribute_Single
            class_id: 0x04,
            instance_id: 1,
            attribute_id: 3,
            data: vec![0xFF, 0xFF, 0xFF, 0xFF],
            source_node: "plc".into(),
        };
        let v = <EtherNetIpAdapter as IndustrialAdapter>::verdict(&msg);
        assert_eq!(v.command, OperationalCommand::WriteState);
        assert_eq!(EtherNetIpAdapter::PROTOCOL, "ethernet_ip");
        assert!(<EtherNetIpAdapter as IndustrialAdapter>::bound_magnitude(&msg).is_ok());
    }
}
