// src/adapters/dnp3.rs

use std::sync::OnceLock;

use crate::gateway::policy::OperationalCommand;
use serde::{Deserialize, Serialize};

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
            0x01 => (OperationalCommand::ReadTelemetry, "Read"),
            0x02 => (OperationalCommand::WriteState, "Write"),
            0x03 => (OperationalCommand::WriteState, "Select"),
            0x04 => (OperationalCommand::SystemMutation, "Operate"),
            0x05 => (OperationalCommand::SystemMutation, "Direct_Operate"),
            0x06 => (OperationalCommand::SystemMutation, "Direct_Operate_NR"),
            0x07 => (OperationalCommand::WriteState, "Freeze"),
            0x08 => (OperationalCommand::WriteState, "Freeze_NR"),
            0x81 => (OperationalCommand::ReadTelemetry, "Response"),
            0x82 => (OperationalCommand::ReadTelemetry, "Unsolicited_Response"),
            _ => (OperationalCommand::Unknown, "Unknown"),
        };

        let is_broadcast = msg.dest_address == DNP3_BROADCAST_ADDRESS;

        // CROB (Group 12), Analog Output (41-42), Octet String (110-113)
        let critical_infrastructure_relevant = msg
            .objects
            .iter()
            .any(|obj| matches!(obj.group, 12 | 41 | 42 | 110 | 111 | 112 | 113));

        // Control commands with CROB or Analog Output objects are actuator writes
        let is_control = matches!(msg.function_code, 0x03..=0x06)
            && msg
                .objects
                .iter()
                .any(|obj| matches!(obj.group, 12 | 41 | 42));

        Dnp3Evaluation {
            command,
            function_name: function_name.to_string(),
            is_control,
            is_broadcast,
            critical_infrastructure_relevant,
        }
    }

    /// MAGNITUDE BOUND — Analog Output (DNP3 group 41) setpoints on a CONTROL
    /// request must lie inside the integrator-configured envelope; otherwise the
    /// command is REFUSED. This closes the gap where the adapter classified a
    /// control's *type* (`SystemMutation`/`WriteState`) but never bounded the
    /// commanded *value* — a master could Direct_Operate an analog output to any
    /// magnitude and be admitted on posture alone.
    ///
    /// Fail-closed, mirroring `protocol_adapter`'s "faithfully decode or refuse,
    /// never fabricate" discipline (#85):
    ///   - a g41 object whose `data` does not match its variation's width →
    ///     `Err(DNP3_ANALOG_OUTPUT_UNDECODABLE)` (no fabricated value),
    ///   - a NaN/Inf float setpoint → `Err(DNP3_ANALOG_OUTPUT_NONFINITE)`,
    ///   - NO envelope configured → `Err(DNP3_ANALOG_OUTPUT_ENVELOPE_UNCONFIGURED)`
    ///     (an analog control write cannot be bounded, so it is denied — the
    ///     fail-closed default),
    ///   - a value outside `[min, max]` → `Err(DNP3_ANALOG_OUTPUT_ENVELOPE_BREACH)`.
    ///
    /// Only the control function codes (Select / Operate / `Direct_Operate[_NR]`,
    /// 0x03–0x06) carry a commanded setpoint; reads/responses are not bounded.
    /// A control with no g41 object (e.g. a CROB-only relay op) is `Ok` here —
    /// CROB is a relay control block, not a scalar magnitude.
    pub fn bound_analog_control(
        msg: &Dnp3Message,
        envelope: Option<&AnalogOutputEnvelope>,
    ) -> Result<(), &'static str> {
        // Only Select/Operate/Direct_Operate[_NR] carry a commanded setpoint.
        if !matches!(msg.function_code, 0x03..=0x06) {
            return Ok(());
        }
        for obj in &msg.objects {
            if obj.group != 41 {
                continue; // not an Analog Output Block command
            }
            let value = decode_analog_setpoint(obj.variation, &obj.data)
                .ok_or("DNP3_ANALOG_OUTPUT_UNDECODABLE")?;
            if !value.is_finite() {
                return Err("DNP3_ANALOG_OUTPUT_NONFINITE");
            }
            let env = envelope.ok_or("DNP3_ANALOG_OUTPUT_ENVELOPE_UNCONFIGURED")?;
            if !env.admits(value) {
                return Err("DNP3_ANALOG_OUTPUT_ENVELOPE_BREACH");
            }
        }
        Ok(())
    }
}

impl crate::adapters::IndustrialAdapter for Dnp3Adapter {
    type Message = Dnp3Message;
    const PROTOCOL: &'static str = "dnp3";

    fn verdict(msg: &Dnp3Message) -> crate::adapters::AdapterVerdict {
        let e = Dnp3Adapter::evaluate(msg);
        crate::adapters::AdapterVerdict {
            command: e.command,
            details: serde_json::json!({
                "function_name": e.function_name,
                "is_control": e.is_control,
                "is_broadcast": e.is_broadcast,
                "critical_infrastructure_relevant": e.critical_infrastructure_relevant,
            }),
            triggers_recalculation: false,
        }
    }

    /// Analog Output (group 41) setpoints are bounded against the process-wide
    /// envelope from `KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE`.
    fn bound_magnitude(msg: &Dnp3Message) -> Result<(), &'static str> {
        Dnp3Adapter::bound_analog_control(msg, global_analog_envelope().as_ref())
    }
}

/// Faithfully decode a DNP3 group-41 (Analog Output Block) setpoint from its
/// object `data`, per IEEE 1815 variations. Returns `None` (undecodable) when the
/// variation is unknown or `data` is too short for the variation's value width —
/// never a fabricated value. The trailing control-status octet (if present) is
/// ignored; only the leading value bytes are read (little-endian per IEEE 1815).
pub fn decode_analog_setpoint(variation: u8, data: &[u8]) -> Option<f64> {
    match variation {
        1 if data.len() >= 4 => {
            Some(i32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64)
        }
        2 if data.len() >= 2 => Some(i16::from_le_bytes([data[0], data[1]]) as f64),
        3 if data.len() >= 4 => {
            Some(f32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64)
        }
        4 if data.len() >= 8 => Some(f64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ])),
        _ => None,
    }
}

/// Inclusive `[min, max]` envelope an Analog Output setpoint must lie within.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalogOutputEnvelope {
    pub min: f64,
    pub max: f64,
}

impl AnalogOutputEnvelope {
    /// Parse a `"min:max"` spec (e.g. `"-100.0:100.0"`). Returns `None` on a
    /// malformed spec, non-finite bounds, or `min > max` — so a bad config never
    /// silently yields a permissive (or inverted) envelope; the caller then treats
    /// the envelope as unconfigured (fail-closed).
    pub fn parse(spec: &str) -> Option<Self> {
        let (min_s, max_s) = spec.trim().split_once(':')?;
        let min = min_s.trim().parse::<f64>().ok()?;
        let max = max_s.trim().parse::<f64>().ok()?;
        if !min.is_finite() || !max.is_finite() || min > max {
            return None;
        }
        Some(Self { min, max })
    }

    pub fn admits(&self, value: f64) -> bool {
        value.is_finite() && value >= self.min && value <= self.max
    }
}

/// Env var carrying the DNP3 Analog Output envelope as `"min:max"`.
/// Unset/malformed → no envelope → analog control writes are denied (fail-closed).
pub const DNP3_ANALOG_OUTPUT_ENVELOPE_ENV: &str = "KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE";

/// Process-wide DNP3 Analog Output envelope, initialized once at startup from the
/// environment. Kept out of `ServiceState`/`AppState` (mirrors the CANopen node
/// map) so this adapter-layer bound needs no change to their construction sites.
static GLOBAL_AO_ENVELOPE: OnceLock<Option<AnalogOutputEnvelope>> = OnceLock::new();

/// Initialize the global Analog Output envelope from
/// `KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE`. Idempotent; call once at startup.
pub fn init_analog_envelope_from_env() {
    let parsed = std::env::var(DNP3_ANALOG_OUTPUT_ENVELOPE_ENV)
        .ok()
        .and_then(|spec| AnalogOutputEnvelope::parse(&spec));
    if GLOBAL_AO_ENVELOPE.set(parsed).is_ok() {
        match parsed {
            Some(env) => tracing::info!(min = env.min, max = env.max,
                "DNP3 Analog Output envelope configured"),
            None => tracing::warn!(
                "DNP3 Analog Output envelope UNCONFIGURED/invalid — analog control writes will be denied (fail-closed)"
            ),
        }
    }
}

/// The configured global Analog Output envelope, or `None` (unconfigured →
/// fail-closed deny) — including before `init_analog_envelope_from_env` runs.
pub fn global_analog_envelope() -> Option<AnalogOutputEnvelope> {
    GLOBAL_AO_ENVELOPE.get().copied().flatten()
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
        Dnp3Object {
            group,
            variation: 1,
            data: vec![],
        }
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
            assert!(
                e.critical_infrastructure_relevant,
                "group {group} must be critical"
            );
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

    // --- Analog Output (group 41) magnitude bounding -----------------------

    fn ao(variation: u8, data: Vec<u8>) -> Dnp3Object {
        Dnp3Object {
            group: 41,
            variation,
            data,
        }
    }

    #[test]
    fn decode_setpoint_per_variation() {
        // v1: 32-bit signed int (LE); v2: 16-bit; v3: f32; v4: f64.
        assert_eq!(decode_analog_setpoint(1, &50i32.to_le_bytes()), Some(50.0));
        assert_eq!(
            decode_analog_setpoint(1, &(-7i32).to_le_bytes()),
            Some(-7.0)
        );
        assert_eq!(
            decode_analog_setpoint(2, &1234i16.to_le_bytes()),
            Some(1234.0)
        );
        assert_eq!(decode_analog_setpoint(3, &1.5f32.to_le_bytes()), Some(1.5));
        assert_eq!(
            decode_analog_setpoint(4, &(-2.25f64).to_le_bytes()),
            Some(-2.25)
        );
    }

    #[test]
    fn decode_setpoint_fails_closed_on_short_or_unknown() {
        assert_eq!(
            decode_analog_setpoint(1, &[0u8; 3]),
            None,
            "v1 needs 4 bytes"
        );
        assert_eq!(
            decode_analog_setpoint(2, &[0u8; 1]),
            None,
            "v2 needs 2 bytes"
        );
        assert_eq!(
            decode_analog_setpoint(4, &[0u8; 7]),
            None,
            "v4 needs 8 bytes"
        );
        assert_eq!(
            decode_analog_setpoint(9, &[0u8; 8]),
            None,
            "unknown variation undecodable"
        );
    }

    #[test]
    fn envelope_parse_rejects_bad_specs() {
        assert_eq!(
            AnalogOutputEnvelope::parse("-100:100"),
            Some(AnalogOutputEnvelope {
                min: -100.0,
                max: 100.0
            })
        );
        assert_eq!(
            AnalogOutputEnvelope::parse(" -5.5 : 5.5 "),
            Some(AnalogOutputEnvelope {
                min: -5.5,
                max: 5.5
            })
        );
        assert!(AnalogOutputEnvelope::parse("nocolon").is_none());
        assert!(AnalogOutputEnvelope::parse("a:b").is_none());
        assert!(
            AnalogOutputEnvelope::parse("10:5").is_none(),
            "min > max rejected"
        );
        assert!(
            AnalogOutputEnvelope::parse("inf:5").is_none(),
            "non-finite rejected"
        );
    }

    #[test]
    fn control_analog_in_envelope_is_admitted() {
        let env = AnalogOutputEnvelope {
            min: -100.0,
            max: 100.0,
        };
        let m = msg(0x05, 0x0001, vec![ao(1, 50i32.to_le_bytes().to_vec())]); // Direct_Operate, value 50
        assert_eq!(Dnp3Adapter::bound_analog_control(&m, Some(&env)), Ok(()));
    }

    #[test]
    fn control_analog_out_of_envelope_is_refused() {
        let env = AnalogOutputEnvelope {
            min: -100.0,
            max: 100.0,
        };
        let m = msg(0x05, 0x0001, vec![ao(1, 999i32.to_le_bytes().to_vec())]);
        assert_eq!(
            Dnp3Adapter::bound_analog_control(&m, Some(&env)),
            Err("DNP3_ANALOG_OUTPUT_ENVELOPE_BREACH")
        );
    }

    #[test]
    fn control_analog_without_envelope_is_fail_closed() {
        let m = msg(0x05, 0x0001, vec![ao(1, 50i32.to_le_bytes().to_vec())]);
        assert_eq!(
            Dnp3Adapter::bound_analog_control(&m, None),
            Err("DNP3_ANALOG_OUTPUT_ENVELOPE_UNCONFIGURED"),
            "an analog control write that cannot be bounded must be denied"
        );
    }

    #[test]
    fn control_analog_undecodable_is_refused() {
        let env = AnalogOutputEnvelope {
            min: -100.0,
            max: 100.0,
        };
        let m = msg(0x05, 0x0001, vec![ao(1, vec![0x01, 0x02])]); // v1 needs 4 bytes
        assert_eq!(
            Dnp3Adapter::bound_analog_control(&m, Some(&env)),
            Err("DNP3_ANALOG_OUTPUT_UNDECODABLE")
        );
    }

    #[test]
    fn control_analog_nonfinite_is_refused() {
        let env = AnalogOutputEnvelope {
            min: -100.0,
            max: 100.0,
        };
        let m = msg(0x05, 0x0001, vec![ao(3, f32::NAN.to_le_bytes().to_vec())]); // v3 NaN
        assert_eq!(
            Dnp3Adapter::bound_analog_control(&m, Some(&env)),
            Err("DNP3_ANALOG_OUTPUT_NONFINITE")
        );
    }

    #[test]
    fn non_control_and_crob_only_are_not_bounded() {
        let env = AnalogOutputEnvelope {
            min: -1.0,
            max: 1.0,
        };
        // A READ carrying a g41 object is not a command → not bounded.
        let read = msg(0x01, 0x0001, vec![ao(1, 999i32.to_le_bytes().to_vec())]);
        assert_eq!(Dnp3Adapter::bound_analog_control(&read, Some(&env)), Ok(()));
        // A control with only a CROB (g12) carries no scalar setpoint → Ok here.
        let crob = msg(0x05, 0x0001, vec![obj(12)]);
        assert_eq!(Dnp3Adapter::bound_analog_control(&crob, Some(&env)), Ok(()));
    }
}
