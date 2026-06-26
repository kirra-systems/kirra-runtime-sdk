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

// ---------------------------------------------------------------------------
// Shared, config-typed magnitude bounding (faithful, never fabricated)
//
// Frames for CANopen (SDO/PDO) and CIP carry the value's BYTES and (sometimes)
// its WIDTH, but NOT its type/signedness/scaling — those live in the device's
// object dictionary / CIP attribute model. So an integrator declares the type +
// envelope PER TARGET; the value is then faithfully decoded BY THAT TYPE (#85)
// and bounded. These primitives are protocol-agnostic; each adapter supplies the
// frame-specific value bytes + target key.
// ---------------------------------------------------------------------------

/// A scalar value type, declared per target by the integrator (from the device's
/// object dictionary / CIP attribute). Little-endian per CiA 301 / CIP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl ScalarType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "i8" => Self::I8,
            "u8" => Self::U8,
            "i16" => Self::I16,
            "u16" => Self::U16,
            "i32" => Self::I32,
            "u32" => Self::U32,
            "f32" => Self::F32,
            "f64" => Self::F64,
            _ => return None,
        })
    }

    pub fn width(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// Faithfully decode the leading `width()` little-endian bytes as this type.
    /// `None` when `data` is shorter than the type's width — never a fabricated
    /// value.
    pub fn decode_le(self, data: &[u8]) -> Option<f64> {
        let w = self.width();
        if data.len() < w {
            return None;
        }
        let b = &data[..w];
        Some(match self {
            Self::I8 => b[0] as i8 as f64,
            Self::U8 => b[0] as f64,
            Self::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Self::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Self::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        })
    }
}

/// A per-target magnitude bound: the declared scalar type + an inclusive
/// `[min, max]` envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundSpec {
    pub ty: ScalarType,
    pub min: f64,
    pub max: f64,
}

impl BoundSpec {
    /// Parse `"type:min:max"` (e.g. `"i16:-500:500"`). `None` on a malformed spec,
    /// unknown type, non-finite bounds, or `min > max` — so a bad config is
    /// treated as UNCONFIGURED (fail-closed for that target), never silently
    /// permissive or inverted.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut it = spec.trim().splitn(3, ':');
        let ty = ScalarType::parse(it.next()?)?;
        let min = it.next()?.trim().parse::<f64>().ok()?;
        let max = it.next()?.trim().parse::<f64>().ok()?;
        if !min.is_finite() || !max.is_finite() || min > max {
            return None;
        }
        Some(Self { ty, min, max })
    }

    /// Faithfully decode `value_bytes` per the declared type and check the
    /// envelope. Returns the stable failure reason (fail-closed) so the caller can
    /// surface it: `width` too short → `undecodable`; NaN/Inf → `nonfinite`;
    /// out of range → `breach`. `Ok` only when the decoded value is in `[min,max]`.
    pub fn check(
        &self,
        value_bytes: &[u8],
        undecodable: &'static str,
        nonfinite: &'static str,
        breach: &'static str,
    ) -> Result<(), &'static str> {
        let v = self.ty.decode_le(value_bytes).ok_or(undecodable)?;
        if !v.is_finite() {
            return Err(nonfinite);
        }
        if !(v >= self.min && v <= self.max) {
            return Err(breach);
        }
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

    #[test]
    fn scalar_type_decodes_le_by_type_and_rejects_short() {
        assert_eq!(ScalarType::I8.decode_le(&[0xFF]), Some(-1.0));
        assert_eq!(ScalarType::U8.decode_le(&[0xFF]), Some(255.0));
        assert_eq!(ScalarType::I16.decode_le(&[0x00, 0x80]), Some(-32768.0));
        assert_eq!(ScalarType::U16.decode_le(&[0x00, 0x80]), Some(32768.0));
        assert_eq!(ScalarType::I32.decode_le(&[0xFF, 0xFF, 0xFF, 0xFF]), Some(-1.0));
        assert_eq!(ScalarType::F32.decode_le(&1.5f32.to_le_bytes()), Some(1.5));
        assert_eq!(ScalarType::F64.decode_le(&2.5f64.to_le_bytes()), Some(2.5));
        // Too short for the declared width → None (never a fabricated value).
        assert_eq!(ScalarType::I32.decode_le(&[0x01, 0x02]), None);
        assert_eq!(ScalarType::F64.decode_le(&[0u8; 4]), None);
    }

    #[test]
    fn bound_spec_parse_rejects_bad_specs() {
        assert!(BoundSpec::parse("i16:-10:10").is_some());
        assert!(BoundSpec::parse("xx:0:1").is_none(), "unknown type");
        assert!(BoundSpec::parse("i16:10:-10").is_none(), "inverted range");
        assert!(BoundSpec::parse("i16:0").is_none(), "missing max");
        assert!(BoundSpec::parse("i16:nan:1").is_none(), "non-finite bound");
    }
}
