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

    // Operations fall into three classes:
    //   * `CmdVelSetpoint` — a register/node WRITE whose `value` IS the
    //     faithfully-decodable commanded setpoint. We pass it through verbatim.
    //   * `ReadTelemetry`  — a register/node READ.
    //   * UNMAPPABLE       — a motion-implying operation with NO decodable
    //     magnitude (a Modbus coil is a single boolean bit; an OPC-UA
    //     `call_method` is a generic RPC). We REFUSE to translate it rather than
    //     fabricate a magnitude — input-integrity fix (#85). Returning an error
    //     here makes `evaluate_industrial_event` fail closed
    //     (`ADAPTER_TRANSLATION_FAILURE`) in every posture, so the governor
    //     never evaluates an invented velocity as if it were real.
    enum Mapped {
        CmdVelSetpoint,
        ReadTelemetry,
    }
    let mapped = match (event.protocol.clone(), event.operation.as_str()) {
        (IndustrialProtocol::Modbus, "write_register") => Mapped::CmdVelSetpoint,
        (IndustrialProtocol::Modbus, "read_register") => Mapped::ReadTelemetry,
        (IndustrialProtocol::OpcUa, "write_node") => Mapped::CmdVelSetpoint,
        (IndustrialProtocol::OpcUa, "read_node") => Mapped::ReadTelemetry,
        // Motion-implying, but no faithfully-decodable velocity magnitude.
        (IndustrialProtocol::Modbus, "coil_write")
        | (IndustrialProtocol::OpcUa, "call_method") => {
            return Err("UNMAPPABLE_TO_KINEMATIC_CLAIM");
        }
        _ => return Err("UNSUPPORTED_INDUSTRIAL_OPERATION"),
    };

    let (action_type, payload) = match mapped {
        Mapped::ReadTelemetry => (
            "read_telemetry",
            serde_json::json!({
                "industrial_context": { "address": event.address, "raw_value": event.value }
            }),
        ),
        Mapped::CmdVelSetpoint => {
            // FAITHFUL WIDTH (#85, B15): a Modbus holding register is 16 bits on
            // the wire, so a faithful single-register `write_register` value
            // occupies the signed-OR-unsigned 16-bit range [-32768, 65535]. A
            // value outside that cannot have originated from one faithful register
            // — refuse rather than cast a corrupt `i64` into a setpoint (the
            // binary adapters reject width mismatches the same way; #85). The
            // kinematic envelope backstops this downstream, but refusing here
            // keeps the decode honest instead of fabricating a magnitude from an
            // unfaithful field. OPC-UA `write_node` carries its own (wider, typed)
            // value and is NOT constrained to 16 bits, so the guard is scoped to
            // Modbus.
            if event.protocol == IndustrialProtocol::Modbus
                && !(-32768..=65535).contains(&event.value)
            {
                return Err("MODBUS_REGISTER_VALUE_UNFAITHFUL_WIDTH");
            }

            // FAITHFUL DECODE: the written register/node value IS the commanded
            // linear setpoint — carry it through verbatim. No synthesized 0.25
            // crawl, no invented scaling. The governor evaluates the REAL
            // magnitude: a value beyond the envelope is denied, zero is a stop.
            //
            // `angular_z` is 0.0: the event carries a single scalar `value` with
            // no faithful angular source (the prior `0.4` was fabricated). A
            // turn-rate command would need a separate, decodable angular field.
            let linear_x = event.value as f64;
            (
                "cmd_vel",
                serde_json::json!({
                    "linear_x": linear_x,
                    "linear_y": 0.0,
                    "linear_z": 0.0,
                    "angular_x": 0.0,
                    "angular_y": 0.0,
                    "angular_z": 0.0,
                    "industrial_context": { "address": event.address, "raw_value": event.value }
                }),
            )
        }
    };

    Ok(ActionClaim {
        action_type: action_type.to_string(),
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

use crate::adapters::ethernet_ip::EtherNetIpAdapter;
use crate::adapters::canopen::CanOpenAdapter;
use crate::adapters::dnp3::Dnp3Adapter;

#[derive(Debug, Clone, Deserialize)]
pub struct UnifiedIndustrialRequest {
    pub protocol: IndustrialProtocol,
    pub message: serde_json::Value,
    /// Per-source replay key (e.g. the gateway/RTU id). Required for the
    /// handler-layer replay gate; unused by the pure `evaluate_*` logic.
    pub source_id: String,
    /// Monotonic per-source sequence (replay/regress gate).
    pub sequence: u64,
    /// Message timestamp (ms since epoch) for the freshness window.
    pub timestamp_ms: u64,
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

        // The three binary-frame protocols share ONE generic dispatch: classify,
        // posture-gate, then magnitude-bound (DNP3 enforces its g41 envelope; the
        // others are posture-only pending per-target type config — see their
        // `bound_magnitude`). `dispatch_adapter` formats its own posture string.
        IndustrialProtocol::EthernetIp => dispatch_adapter::<EtherNetIpAdapter>(req.message, posture),
        IndustrialProtocol::CanOpen => dispatch_adapter::<CanOpenAdapter>(req.message, posture),
        IndustrialProtocol::Dnp3 => dispatch_adapter::<Dnp3Adapter>(req.message, posture),
    }
}

/// Generic dispatch for every binary-frame industrial adapter (DNP3 / CANopen /
/// EtherNet-IP). Deserialize the protocol message, classify it into a
/// posture-gated command, then — only for a posture-ADMITTED command — apply the
/// adapter's magnitude bound (fail-closed on a breach). Collapses three
/// formerly-duplicated arms into one path and routes the magnitude bound
/// uniformly through `IndustrialAdapter::bound_magnitude`.
fn dispatch_adapter<A: crate::adapters::IndustrialAdapter>(
    message: serde_json::Value,
    posture: FleetPosture,
) -> Result<UnifiedEvaluationResult, String> {
    let msg: A::Message = serde_json::from_value(message)
        .map_err(|e| format!("MALFORMED_{}_MESSAGE: {e}", A::PROTOCOL.to_uppercase()))?;
    let verdict = A::verdict(&msg);
    let (mut allowed, mut denial_reason) =
        command_allowed_for_posture_pub(&verdict.command, &posture);
    if allowed {
        if let Err(reason) = A::bound_magnitude(&msg) {
            allowed = false;
            denial_reason = Some(reason.to_string());
        }
    }
    Ok(UnifiedEvaluationResult {
        protocol: A::PROTOCOL.to_string(),
        command: verdict.command,
        allowed,
        denial_reason,
        posture_at_evaluation: format!("{:?}", posture),
        adapter_details: verdict.details,
        triggers_recalculation: verdict.triggers_recalculation,
    })
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

// ---------------------------------------------------------------------------
// Industrial-message replay / freshness protection (IEC 62443)
// ---------------------------------------------------------------------------

/// Max age (and max future skew) of an industrial message's `timestamp_ms`
/// relative to the local clock. Mirrors the federation replay window: a message
/// outside this window is rejected even if its sequence is in order, bounding the
/// delayed-delivery replay surface. The per-source monotonic `sequence` gate
/// (`VerifierStore::industrial_seq_check_and_advance`) is the within-window
/// replay defense; this is the freshness half.
pub const INDUSTRIAL_FRESHNESS_WINDOW_MS: u64 = 5_000;

/// Pure freshness classification for an industrial message. Returns `Some(reason)`
/// when the message is too old or too far in the future (fail-closed), else `None`.
/// Uses `saturating_sub` so a backward/forward clock step can never underflow.
pub fn classify_industrial_freshness(
    timestamp_ms: u64,
    now_ms: u64,
    window_ms: u64,
) -> Option<&'static str> {
    if now_ms.saturating_sub(timestamp_ms) > window_ms {
        return Some("INDUSTRIAL_MESSAGE_STALE");
    }
    if timestamp_ms.saturating_sub(now_ms) > window_ms {
        return Some("INDUSTRIAL_MESSAGE_FUTURE_DATED");
    }
    None
}

#[cfg(test)]
mod industrial_freshness_tests {
    use super::{classify_industrial_freshness, INDUSTRIAL_FRESHNESS_WINDOW_MS as W};

    #[test]
    fn fresh_message_passes() {
        assert_eq!(classify_industrial_freshness(1_000, 1_000, W), None);
        assert_eq!(classify_industrial_freshness(1_000, 1_000 + W, W), None, "exactly at the window edge is fresh");
        assert_eq!(classify_industrial_freshness(1_000 + W, 1_000, W), None, "edge future is fresh");
    }

    #[test]
    fn stale_and_future_are_rejected() {
        assert_eq!(classify_industrial_freshness(1_000, 1_000 + W + 1, W), Some("INDUSTRIAL_MESSAGE_STALE"));
        assert_eq!(classify_industrial_freshness(1_000 + W + 1, 1_000, W), Some("INDUSTRIAL_MESSAGE_FUTURE_DATED"));
    }
}

#[cfg(test)]
mod no_fabricated_velocity_tests {
    use super::*;

    fn modbus_event(operation: &str, value: i64) -> IndustrialEvent {
        IndustrialEvent {
            protocol: IndustrialProtocol::Modbus,
            asset_id: "asset-1".to_string(),
            operation: operation.to_string(),
            address: "40001".to_string(),
            value,
            risk_class: "kinetic_write".to_string(),
        }
    }

    // A register write carries a faithfully-decodable setpoint: the claim must
    // carry the REAL value, not the old fabricated 0.25 crawl / 0.4 turn.
    #[test]
    fn write_register_decodes_real_value_not_fabricated() {
        let claim = map_industrial_event_to_claim(&modbus_event("write_register", 5))
            .expect("a register write is decodable");
        assert_eq!(claim.action_type, "cmd_vel");
        assert_eq!(
            claim.payload["linear_x"].as_f64().unwrap(),
            5.0,
            "the real commanded value passes through (not the fabricated 0.25)"
        );
        assert_eq!(
            claim.payload["angular_z"].as_f64().unwrap(),
            0.0,
            "no fabricated angular component (was 0.4 for value > 1)"
        );
    }

    #[test]
    fn write_register_zero_is_a_real_stop() {
        let claim = map_industrial_event_to_claim(&modbus_event("write_register", 0)).unwrap();
        assert_eq!(claim.payload["linear_x"].as_f64().unwrap(), 0.0);
    }

    // The REAL magnitude is governed: 5 m/s exceeds the 0.5 envelope → denied.
    // Previously this was masked to a fabricated 0.25 and wrongly ALLOWED.
    #[test]
    fn real_magnitude_is_governed_not_masked() {
        let decision = evaluate_industrial_event(modbus_event("write_register", 5), FleetPosture::Nominal);
        assert!(!decision.allowed, "a real 5 m/s setpoint breaches the envelope");
        assert_eq!(decision.reason, "KINEMATIC_ENVELOPE_BREACH");
    }

    // A boolean coil carries no velocity magnitude → refuse to translate;
    // never synthesize 0.25/0.4. Fails closed in every posture.
    #[test]
    fn coil_write_is_unmappable_and_fails_closed() {
        assert_eq!(
            map_industrial_event_to_claim(&modbus_event("coil_write", 1)).unwrap_err(),
            "UNMAPPABLE_TO_KINEMATIC_CLAIM",
        );
        let decision = evaluate_industrial_event(modbus_event("coil_write", 1), FleetPosture::Nominal);
        assert!(!decision.allowed, "an unmappable motion command must NOT be admitted");
        assert!(decision.reason.starts_with("ADAPTER_TRANSLATION_FAILURE"));
        assert!(decision.reason.contains("UNMAPPABLE_TO_KINEMATIC_CLAIM"));
    }

    // B15: a Modbus single-register value that cannot fit a faithful 16-bit
    // register ([-32768, 65535]) is refused, not cast into a setpoint.
    #[test]
    fn modbus_register_value_beyond_16_bits_is_refused() {
        for v in [65_536_i64, 70_000, -32_769, i64::MAX, i64::MIN] {
            assert_eq!(
                map_industrial_event_to_claim(&modbus_event("write_register", v)).unwrap_err(),
                "MODBUS_REGISTER_VALUE_UNFAITHFUL_WIDTH",
                "value {v} is outside a faithful 16-bit register and must be refused",
            );
            let decision =
                evaluate_industrial_event(modbus_event("write_register", v), FleetPosture::Nominal);
            assert!(!decision.allowed, "an unfaithful-width register write must fail closed");
            assert!(decision.reason.contains("MODBUS_REGISTER_VALUE_UNFAITHFUL_WIDTH"));
        }
    }

    // The faithful 16-bit range (signed OR unsigned) still decodes — the width
    // guard rejects only values that no single register could carry. The
    // downstream kinematic envelope, not the width guard, governs magnitude.
    #[test]
    fn modbus_register_value_within_16_bits_still_decodes() {
        for v in [-32_768_i64, -200, 0, 65_535] {
            let claim = map_industrial_event_to_claim(&modbus_event("write_register", v))
                .expect("a faithful 16-bit register value decodes");
            assert_eq!(claim.payload["linear_x"].as_f64().unwrap(), v as f64);
        }
    }

    // The 16-bit width guard is Modbus-scoped: an OPC-UA write_node carries its
    // own wider typed value and must NOT be refused by the Modbus register bound.
    #[test]
    fn opcua_write_node_is_not_constrained_to_16_bits() {
        let event = IndustrialEvent {
            protocol: IndustrialProtocol::OpcUa,
            asset_id: "asset-1".to_string(),
            operation: "write_node".to_string(),
            address: "ns=2;s=Speed".to_string(),
            value: 70_000,
            risk_class: "kinetic_write".to_string(),
        };
        let claim = map_industrial_event_to_claim(&event)
            .expect("an OPC-UA node write is not bound to 16 bits");
        assert_eq!(claim.payload["linear_x"].as_f64().unwrap(), 70_000.0);
    }

    // A generic OPC-UA method call has no velocity semantics → unmappable.
    #[test]
    fn call_method_is_unmappable_and_fails_closed() {
        let event = IndustrialEvent {
            protocol: IndustrialProtocol::OpcUa,
            asset_id: "asset-1".to_string(),
            operation: "call_method".to_string(),
            address: "ns=2;s=Motor".to_string(),
            value: 1,
            risk_class: "kinetic_write".to_string(),
        };
        assert_eq!(
            map_industrial_event_to_claim(&event).unwrap_err(),
            "UNMAPPABLE_TO_KINEMATIC_CLAIM",
        );
        assert!(!evaluate_industrial_event(event, FleetPosture::Nominal).allowed);
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
            source_id: "plc_01".to_string(), sequence: 1, timestamp_ms: 0,
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
            source_id: "can_01".to_string(), sequence: 1, timestamp_ms: 0,
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
            source_id: "sub_01".to_string(), sequence: 1, timestamp_ms: 0,
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
            let r = evaluate_unified_industrial_request(eip_request(), posture).unwrap();
            assert!(!r.posture_at_evaluation.is_empty(), "posture_at_evaluation must always be set");
        }
    }

    #[test]
    fn test_malformed_message_returns_err() {
        let req = UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::EthernetIp,
            message: serde_json::json!({"bad": "data"}),
            source_id: "x".to_string(), sequence: 1, timestamp_ms: 0,
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

    #[test]
    fn test_dnp3_analog_control_magnitude_bound_is_wired_fail_closed() {
        // A DNP3 Direct_Operate (0x05) carrying an Analog Output (g41) value is a
        // posture-admissible SystemMutation under Nominal, but the magnitude bound
        // must additionally gate it. The global envelope is unset in the test
        // process → fail-closed: the analog control is DENIED. Proves the bound is
        // wired into the unified path (not just the dedicated handler).
        let req = UnifiedIndustrialRequest {
            protocol: IndustrialProtocol::Dnp3,
            message: serde_json::json!({
                "source_address": 1, "dest_address": 0x0001, "function_code": 0x05,
                "data_link_control": 0,
                "objects": [{ "group": 41, "variation": 1, "data": 50i32.to_le_bytes().to_vec() }],
                "source_node": "sub_01"
            }),
            source_id: "sub_01".to_string(), sequence: 1, timestamp_ms: 0,
        };
        let r = evaluate_unified_industrial_request(req, FleetPosture::Nominal).unwrap();
        assert!(!r.allowed, "an unbounded analog control must be denied (fail-closed)");
        assert_eq!(
            r.denial_reason.as_deref(),
            Some("DNP3_ANALOG_OUTPUT_ENVELOPE_UNCONFIGURED")
        );
    }
}
