// src/adapters/ethernet_ip.rs

use kirra_policy_types::OperationalCommand;
use serde::{Deserialize, Serialize};

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
            0x0E => (
                OperationalCommand::ReadTelemetry,
                "Get_Attribute_Single",
                false,
            ),
            0x10 => (OperationalCommand::WriteState, "Set_Attribute_Single", true),
            0x4B => (OperationalCommand::SystemMutation, "Execute_Service", true),
            0x4C => (OperationalCommand::ReadTelemetry, "Read_Tag", false),
            0x4D => (OperationalCommand::WriteState, "Write_Tag", true),
            _ => (OperationalCommand::Unknown, "Unknown_Service", false),
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

    /// A CIP `Set_Attribute_Single` write to a CONFIGURED `(class, instance,
    /// attribute)` target is faithfully decoded BY THE CONFIGURED TYPE (the CIP
    /// attribute's data type — the frame carries only bytes, never type, #85) and
    /// bounded. Unconfigured targets stay posture-only unless strict mode is on.
    fn bound_magnitude(msg: &EtherNetIpMessage) -> Result<(), &'static str> {
        EtherNetIpAdapter::bound_cip_write(msg, global_cip_bounds())
    }
}

// ---------------------------------------------------------------------------
// CIP attribute-write magnitude bounding (#85: faithful, config-typed)
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::adapters::BoundSpec;

/// CIP service code for `Set_Attribute_Single` — the write whose `data` is the
/// bare attribute value (no embedded CIP type header), so it is faithfully
/// decodable against a per-attribute type config.
const CIP_SET_ATTRIBUTE_SINGLE: u8 = 0x10;

/// Env var carrying per-attribute CIP write bounds. Format: comma-separated
/// `class:instance:attr=type:min:max`, e.g. `0x0A:1:3=i16:-500:500`. `class`,
/// `instance`, `attr` are `u16` (decimal or `0x`-hex); `type` ∈
/// {i8,u8,i16,u16,i32,u32,f32,f64}. Unset → CIP writes are posture-only. A
/// malformed entry is SKIPPED, never fabricated.
pub const CIP_ATTR_BOUNDS_ENV: &str = "KIRRA_CIP_ATTR_BOUNDS";

/// When `1`/`true`, a CIP `Set_Attribute_Single` to a target with NO configured
/// bound is DENIED (high-assurance mode) instead of falling through to
/// posture-only. Reads / other services are unaffected.
pub const CIP_STRICT_BOUNDS_ENV: &str = "KIRRA_CIP_STRICT_BOUNDS";

/// Per-`(class, instance, attribute)` CIP write bounds + the strict-mode flag.
#[derive(Debug, Clone, Default)]
pub struct CipAttrBounds {
    map: HashMap<(u16, u16, u16), BoundSpec>,
    strict: bool,
}

impl CipAttrBounds {
    /// Parse the env spec. Malformed entries are skipped (never fabricated).
    pub fn parse(spec: &str, strict: bool) -> Self {
        let mut map = HashMap::new();
        for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((target, bound)) = entry.split_once('=') else {
                continue;
            };
            let mut t = target.splitn(3, ':');
            let (Some(c), Some(i), Some(a)) = (t.next(), t.next(), t.next()) else {
                continue;
            };
            let (Some(class), Some(instance), Some(attr)) =
                (parse_u16_radix(c), parse_u16_radix(i), parse_u16_radix(a))
            else {
                continue;
            };
            let Some(b) = BoundSpec::parse(bound) else {
                continue;
            };
            map.insert((class, instance, attr), b);
        }
        Self { map, strict }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Parse a `u16` in decimal or `0x`-hex.
fn parse_u16_radix(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

impl EtherNetIpAdapter {
    /// MAGNITUDE BOUND — a CIP `Set_Attribute_Single` (0x10) write to a CONFIGURED
    /// `(class, instance, attribute)` target must carry a value within the declared
    /// envelope, decoded BY THE CONFIGURED TYPE (the CIP attribute's data type;
    /// the frame carries only bytes, #85). Fail-closed:
    ///   - a non-`Set_Attribute_Single` service → `Ok` (no faithfully-located scalar:
    ///     reads carry none; `Write_Tag`/`Execute_Service` embed a CIP type/count
    ///     header this simplified model does not separate, so they stay posture-only),
    ///   - a write to an UNCONFIGURED target → `Ok` unless strict mode (then denied),
    ///   - value bytes too short for the type → `Err`,
    ///   - value outside `[min, max]` → `Err`.
    pub fn bound_cip_write(
        msg: &EtherNetIpMessage,
        bounds: &CipAttrBounds,
    ) -> Result<(), &'static str> {
        // Only Set_Attribute_Single carries the bare attribute value in `data`.
        if msg.service_code != CIP_SET_ATTRIBUTE_SINGLE {
            return Ok(());
        }
        let key = (msg.class_id, msg.instance_id, msg.attribute_id);
        let Some(spec) = bounds.map.get(&key) else {
            return if bounds.strict {
                Err("CIP_UNCONFIGURED_TARGET_STRICT")
            } else {
                Ok(())
            };
        };
        spec.check(
            &msg.data,
            "CIP_VALUE_UNDECODABLE",
            "CIP_VALUE_NONFINITE",
            "CIP_VALUE_ENVELOPE_BREACH",
        )
    }
}

static GLOBAL_CIP_BOUNDS: OnceLock<CipAttrBounds> = OnceLock::new();

/// Read `KIRRA_CIP_ATTR_BOUNDS` (+ `KIRRA_CIP_STRICT_BOUNDS`) once into the
/// process-wide bounds, at startup. Mirrors the CANopen / DNP3 inits — kept out
/// of `AppState`/`ServiceState`. Idempotent.
pub fn init_cip_bounds_from_env() {
    let _ = GLOBAL_CIP_BOUNDS.set(load_cip_bounds_from_env());
}

/// The process-wide CIP attribute bounds, lazily loaded on first use if
/// `init_cip_bounds_from_env` was never called.
pub fn global_cip_bounds() -> &'static CipAttrBounds {
    GLOBAL_CIP_BOUNDS.get_or_init(load_cip_bounds_from_env)
}

fn load_cip_bounds_from_env() -> CipAttrBounds {
    let spec = std::env::var(CIP_ATTR_BOUNDS_ENV).unwrap_or_default();
    let strict = std::env::var(CIP_STRICT_BOUNDS_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    CipAttrBounds::parse(&spec, strict)
}

#[cfg(test)]
mod cip_bounds_tests {
    use super::*;

    /// A CIP write to (class, instance, attr) carrying `value` as the attribute data.
    fn cip_write(
        service: u8,
        class: u16,
        instance: u16,
        attr: u16,
        value: &[u8],
    ) -> EtherNetIpMessage {
        EtherNetIpMessage {
            command_code: 0x0065,
            session_handle: 1,
            status: 0,
            service_code: service,
            class_id: class,
            instance_id: instance,
            attribute_id: attr,
            data: value.to_vec(),
            source_node: "plc_01".into(),
        }
    }

    fn bounds(spec: &str, strict: bool) -> CipAttrBounds {
        CipAttrBounds::parse(spec, strict)
    }

    #[test]
    fn config_parses_decimal_and_hex_targets() {
        let b = bounds("0x0A:1:3=i16:-500:500, 100:2:4=u8:0:100", false);
        assert!(b.map.contains_key(&(0x0A, 1, 3)));
        assert!(b.map.contains_key(&(100, 2, 4)));
    }

    #[test]
    fn in_range_set_attribute_is_admitted() {
        let b = bounds("0x0A:1:3=i16:-500:500", false);
        let msg = cip_write(0x10, 0x0A, 1, 3, &100i16.to_le_bytes());
        assert_eq!(EtherNetIpAdapter::bound_cip_write(&msg, &b), Ok(()));
    }

    #[test]
    fn out_of_range_set_attribute_is_denied() {
        let b = bounds("0x0A:1:3=i16:-500:500", false);
        let msg = cip_write(0x10, 0x0A, 1, 3, &5000i16.to_le_bytes());
        assert_eq!(
            EtherNetIpAdapter::bound_cip_write(&msg, &b),
            Err("CIP_VALUE_ENVELOPE_BREACH")
        );
    }

    #[test]
    fn signedness_is_decided_by_the_configured_type() {
        // -200 (i16 LE) is in [-500,500]; the same bytes as u16 are 65336 (breach).
        let b_i16 = bounds("0x0A:1:3=i16:-500:500", false);
        let b_u16 = bounds("0x0A:1:3=u16:0:500", false);
        let msg = cip_write(0x10, 0x0A, 1, 3, &(-200i16).to_le_bytes());
        assert_eq!(EtherNetIpAdapter::bound_cip_write(&msg, &b_i16), Ok(()));
        assert_eq!(
            EtherNetIpAdapter::bound_cip_write(&msg, &b_u16),
            Err("CIP_VALUE_ENVELOPE_BREACH")
        );
    }

    #[test]
    fn truncated_value_for_configured_type_is_undecodable() {
        let b = bounds("0x0A:1:3=i32:-1:1", false);
        // Only 2 bytes for a configured 4-byte i32.
        let msg = cip_write(0x10, 0x0A, 1, 3, &[0x01, 0x00]);
        assert_eq!(
            EtherNetIpAdapter::bound_cip_write(&msg, &b),
            Err("CIP_VALUE_UNDECODABLE")
        );
    }

    #[test]
    fn unconfigured_target_is_posture_only_unless_strict() {
        let lax = bounds("0x0A:1:3=i16:-500:500", false);
        let strict = bounds("0x0A:1:3=i16:-500:500", true);
        // Different attribute → unconfigured.
        let msg = cip_write(0x10, 0x0A, 1, 9, &100i16.to_le_bytes());
        assert_eq!(EtherNetIpAdapter::bound_cip_write(&msg, &lax), Ok(()));
        assert_eq!(
            EtherNetIpAdapter::bound_cip_write(&msg, &strict),
            Err("CIP_UNCONFIGURED_TARGET_STRICT")
        );
    }

    #[test]
    fn non_set_attribute_services_are_not_bounded() {
        // Strict mode, configured target — but a READ (Get_Attribute_Single) and a
        // Write_Tag carry no faithfully-located scalar here, so both pass.
        let b = bounds("0x0A:1:3=i16:-500:500", true);
        let read = cip_write(0x0E, 0x0A, 1, 3, &9999i16.to_le_bytes());
        assert_eq!(EtherNetIpAdapter::bound_cip_write(&read, &b), Ok(()));
        let write_tag = cip_write(0x4D, 0x0A, 1, 3, &9999i16.to_le_bytes());
        assert_eq!(EtherNetIpAdapter::bound_cip_write(&write_tag, &b), Ok(()));
    }
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
            assert!(
                e.safety_relevant,
                "class 0x{class:02X} must be safety_relevant"
            );
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
