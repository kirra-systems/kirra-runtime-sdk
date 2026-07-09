// src/adapters/canopen.rs

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::gateway::policy::OperationalCommand;
use serde::{Deserialize, Serialize};

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
            0x1 => (OperationalCommand::ReadTelemetry, CanOpenMessageType::SYNC),
            0x2 => (
                OperationalCommand::ReadTelemetry,
                CanOpenMessageType::Timestamp,
            ),
            0x3 => (
                OperationalCommand::ReadTelemetry,
                CanOpenMessageType::Emergency,
            ),
            0x4 | 0x5 => (
                OperationalCommand::ReadTelemetry,
                CanOpenMessageType::PDOTransmit,
            ),
            0x6 | 0x7 => (
                OperationalCommand::WriteState,
                CanOpenMessageType::PDOReceive,
            ),
            0x8 | 0x9 => (
                OperationalCommand::ReadTelemetry,
                CanOpenMessageType::SDOTransmit,
            ),
            0xA | 0xB => (
                OperationalCommand::WriteState,
                CanOpenMessageType::SDOReceive,
            ),
            0xE => (
                OperationalCommand::ReadTelemetry,
                CanOpenMessageType::Heartbeat,
            ),
            _ => (OperationalCommand::Unknown, CanOpenMessageType::Unknown),
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
            && msg
                .data
                .first()
                .map(|&b| matches!(b, 0x02 | 0x80 | 0x81 | 0x82))
                .unwrap_or(false);

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

    /// An SDO expedited download to a CONFIGURED `(node, index, subindex)` target
    /// is faithfully decoded BY THE CONFIGURED TYPE (the object-dictionary entry —
    /// the frame carries width at best, never type, #85) and bounded against its
    /// envelope. Targets with no config stay posture-only unless strict mode is on.
    /// PDOs (raw process data, no per-object target in the frame) remain
    /// posture-only — faithfully bounding them needs PDO-mapping config (follow-up).
    fn bound_magnitude(msg: &CanOpenMessage) -> Result<(), &'static str> {
        CanOpenAdapter::bound_sdo_download(msg, global_sdo_bounds())
    }
}

// ---------------------------------------------------------------------------
// CANopen SDO expedited-download magnitude bounding (#85: faithful, config-typed)
// ---------------------------------------------------------------------------

use crate::adapters::BoundSpec;

/// Env var carrying per-target SDO download bounds. Format: comma-separated
/// `node:index:subindex=type:min:max`, e.g. `5:0x6042:0=i16:-500:500`. `node`
/// and `subindex` are `u8` (decimal or `0x`-hex); `index` is `u16` (decimal or
/// `0x`-hex); `type` ∈ {i8,u8,i16,u16,i32,u32,f32}. Unset → no bounds (SDO
/// writes are posture-only). A malformed entry is SKIPPED, never fabricated, so
/// one bad pair can't silently disable the rest.
pub const CANOPEN_SDO_BOUNDS_ENV: &str = "KIRRA_CANOPEN_SDO_BOUNDS";

/// When `1`/`true`, a CANopen SDO DOWNLOAD to a target with NO configured bound
/// is DENIED (high-assurance mode) instead of falling through to posture-only.
pub const CANOPEN_STRICT_BOUNDS_ENV: &str = "KIRRA_CANOPEN_STRICT_BOUNDS";

/// Per-`(node, index, subindex)` SDO download bounds + the strict-mode flag.
#[derive(Debug, Clone, Default)]
pub struct CanOpenSdoBounds {
    map: HashMap<(u8, u16, u8), BoundSpec>,
    strict: bool,
}

impl CanOpenSdoBounds {
    /// Parse the env spec. Malformed entries are skipped (never fabricated).
    pub fn parse(spec: &str, strict: bool) -> Self {
        let mut map = HashMap::new();
        for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((target, bound)) = entry.split_once('=') else {
                continue;
            };
            let mut t = target.splitn(3, ':');
            let (Some(n), Some(i), Some(si)) = (t.next(), t.next(), t.next()) else {
                continue;
            };
            let (Some(node), Some(index), Some(sub)) =
                (parse_u8_radix(n), parse_u16_radix(i), parse_u8_radix(si))
            else {
                continue;
            };
            let Some(b) = BoundSpec::parse(bound) else {
                continue;
            };
            map.insert((node, index, sub), b);
        }
        Self { map, strict }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Parse a `u8` in decimal or `0x`-hex.
fn parse_u8_radix(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u8>().ok()
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

/// A parsed SDO "initiate download" (write) request header (CiA 301 §7.2.4).
struct SdoDownload<'a> {
    index: u16,
    subindex: u8,
    /// The value region (`data[4..]`) for an EXPEDITED download; `None` for a
    /// segmented (non-expedited) download — its value spans later frames and is
    /// not faithfully present here.
    expedited_value: Option<&'a [u8]>,
    /// The size-indicated value width (`4 - n`) when the command byte's size bit
    /// is set; `None` when size is not indicated (width then comes from config).
    indicated_width: Option<usize>,
}

/// Parse an SDO download header from the data field. `None` when this is not an
/// "initiate download" command (ccs ≠ 1, e.g. an upload request / abort) or the
/// frame is too short to carry the 4-byte command-index-subindex header.
fn parse_sdo_download(data: &[u8]) -> Option<SdoDownload<'_>> {
    if data.len() < 4 {
        return None;
    }
    let cs = data[0];
    // ccs (client command specifier) = bits 7..5; 1 = initiate download (write).
    if (cs >> 5) != 1 {
        return None;
    }
    let index = u16::from_le_bytes([data[1], data[2]]);
    let subindex = data[3];
    let expedited = (cs & 0x02) != 0; // bit 1 (e)
    let size_indicated = (cs & 0x01) != 0; // bit 0 (s)
    let (expedited_value, indicated_width) = if expedited {
        let value = data.get(4..).unwrap_or(&[]);
        let iw = if size_indicated {
            // n = bits 3..2 = number of value bytes NOT containing data.
            Some(4usize.saturating_sub(((cs >> 2) & 0x03) as usize))
        } else {
            None
        };
        (Some(value), iw)
    } else {
        (None, None)
    };
    Some(SdoDownload {
        index,
        subindex,
        expedited_value,
        indicated_width,
    })
}

impl CanOpenAdapter {
    /// MAGNITUDE BOUND — a CANopen SDO expedited download (object-dictionary write)
    /// to a CONFIGURED `(node, index, subindex)` target must carry a value within
    /// the declared envelope. The value's TYPE comes from the integrator config
    /// (the OD entry), NEVER the frame (#85). Fail-closed:
    ///   - not an SDO download (upload request / abort / non-SDO) → `Ok` (no setpoint),
    ///   - download to an UNCONFIGURED target → `Ok` unless strict mode (then denied),
    ///   - configured target but a SEGMENTED download → `Err` (value not faithfully present),
    ///   - configured type width ≠ the frame's size-indicated width → `Err`,
    ///   - value bytes too short for the type → `Err`,
    ///   - value outside `[min, max]` → `Err`.
    pub fn bound_sdo_download(
        msg: &CanOpenMessage,
        bounds: &CanOpenSdoBounds,
    ) -> Result<(), &'static str> {
        // Only SDO (Receive-SDO, client→server) frames carry a download.
        if !matches!(msg.function_code, 0xA | 0xB) {
            return Ok(());
        }
        let Some(dl) = parse_sdo_download(&msg.data) else {
            // Not a download command (upload request / abort / too short) — no
            // commanded setpoint to bound; posture gate still applies.
            return Ok(());
        };
        let key = (msg.node_id, dl.index, dl.subindex);
        let Some(spec) = bounds.map.get(&key) else {
            // Unconfigured target: posture-only, unless strict mode denies it.
            return if bounds.strict {
                Err("CANOPEN_SDO_UNCONFIGURED_TARGET_STRICT")
            } else {
                Ok(())
            };
        };
        // Configured target: must be an EXPEDITED download with the value present.
        let value = dl
            .expedited_value
            .ok_or("CANOPEN_SDO_SEGMENTED_UNBOUNDABLE")?;
        // Cross-check the frame's declared width against the configured type.
        let w = spec.ty.width();
        if let Some(indicated) = dl.indicated_width {
            if indicated != w {
                return Err("CANOPEN_SDO_WIDTH_MISMATCH");
            }
        } else {
            // #694: s=0 (size NOT indicated). The frame declares no value width,
            // so `decode_le` would read only the leading `w` bytes and silently
            // ignore the rest — letting an attacker smuggle hidden, device-
            // interpretable bytes past `[w..]` while the bounded prefix stays
            // benign. Fail closed: any non-zero byte beyond the configured type
            // width is an undeclared payload, never faithfully bound here.
            if value.len() > w && value[w..].iter().any(|&b| b != 0) {
                return Err("CANOPEN_SDO_WIDTH_MISMATCH");
            }
        }
        spec.check(
            value,
            "CANOPEN_SDO_UNDECODABLE",
            "CANOPEN_SDO_NONFINITE",
            "CANOPEN_SDO_ENVELOPE_BREACH",
        )
    }
}

static GLOBAL_SDO_BOUNDS: OnceLock<CanOpenSdoBounds> = OnceLock::new();

/// Read `KIRRA_CANOPEN_SDO_BOUNDS` (+ `KIRRA_CANOPEN_STRICT_BOUNDS`) once into the
/// process-wide bounds, at startup. Mirrors the DNP3 analog-envelope init: kept
/// out of `AppState`/`ServiceState` so this adapter-layer bound needs no change to
/// their construction sites. Idempotent (subsequent calls are no-ops).
pub fn init_sdo_bounds_from_env() {
    let _ = GLOBAL_SDO_BOUNDS.set(load_sdo_bounds_from_env());
}

/// The process-wide CANopen SDO bounds, lazily loaded from the environment on
/// first use if `init_sdo_bounds_from_env` was never called.
pub fn global_sdo_bounds() -> &'static CanOpenSdoBounds {
    GLOBAL_SDO_BOUNDS.get_or_init(load_sdo_bounds_from_env)
}

fn load_sdo_bounds_from_env() -> CanOpenSdoBounds {
    let spec = std::env::var(CANOPEN_SDO_BOUNDS_ENV).unwrap_or_default();
    let strict = std::env::var(CANOPEN_STRICT_BOUNDS_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    CanOpenSdoBounds::parse(&spec, strict)
}

#[cfg(test)]
mod sdo_bounds_tests {
    use super::*;

    // Build an SDO expedited-download frame: command byte + index(LE) + subindex
    // + `value` bytes (padded to the 8-byte SDO frame), size-indicated for `width`.
    fn sdo_expedited(node: u8, index: u16, sub: u8, value: &[u8]) -> CanOpenMessage {
        let width = value.len();
        assert!((1..=4).contains(&width), "expedited value is 1..=4 bytes");
        let n = (4 - width) as u8;
        // ccs=1 (download), n in bits3..2, e=1 (bit1), s=1 (bit0).
        let cs = (1u8 << 5) | (n << 2) | 0x02 | 0x01;
        let idx = index.to_le_bytes();
        let mut data = vec![cs, idx[0], idx[1], sub];
        data.extend_from_slice(value);
        data.resize(8, 0x00); // pad unused value bytes
        CanOpenMessage {
            node_id: node,
            function_code: 0xA, // SDOReceive (download channel)
            data,
            source_node: "rtu-1".into(),
        }
    }

    fn bounds(spec: &str, strict: bool) -> CanOpenSdoBounds {
        CanOpenSdoBounds::parse(spec, strict)
    }

    #[test]
    fn config_parses_decimal_and_hex_targets() {
        let b = bounds("5:0x6042:0=i16:-500:500, 6:24642:1=u8:0:100", false);
        assert!(b.map.contains_key(&(5, 0x6042, 0)));
        assert!(b.map.contains_key(&(6, 24642, 1)));
    }

    #[test]
    fn config_skips_malformed_entries_without_panicking() {
        // bad type, missing field, inverted range, non-numeric node — all skipped.
        let b = bounds(
            "5:0x6042:0=i16:-500:500,bad,7:1:0=xx:0:1,8:1:0=i16:9:1,zz:1:0=i16:0:1",
            false,
        );
        assert_eq!(b.map.len(), 1, "only the one valid entry survives");
    }

    #[test]
    fn in_range_i16_setpoint_is_admitted() {
        let b = bounds("5:0x6042:0=i16:-500:500", false);
        // value = 100 (i16 LE)
        let msg = sdo_expedited(5, 0x6042, 0, &100i16.to_le_bytes());
        assert_eq!(CanOpenAdapter::bound_sdo_download(&msg, &b), Ok(()));
    }

    #[test]
    fn out_of_range_setpoint_is_denied() {
        let b = bounds("5:0x6042:0=i16:-500:500", false);
        let msg = sdo_expedited(5, 0x6042, 0, &1000i16.to_le_bytes());
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b),
            Err("CANOPEN_SDO_ENVELOPE_BREACH")
        );
    }

    #[test]
    fn signedness_matters_negative_in_range() {
        // -200 as i16 is in [-500,500]; as u16 the same bytes are 65336 (breach).
        let b_i16 = bounds("5:0x6042:0=i16:-500:500", false);
        let b_u16 = bounds("5:0x6042:0=u16:0:500", false);
        let msg = sdo_expedited(5, 0x6042, 0, &(-200i16).to_le_bytes());
        assert_eq!(CanOpenAdapter::bound_sdo_download(&msg, &b_i16), Ok(()));
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b_u16),
            Err("CANOPEN_SDO_ENVELOPE_BREACH"),
            "the configured TYPE decides interpretation — not the bytes alone"
        );
    }

    #[test]
    fn unconfigured_target_is_posture_only_unless_strict() {
        let lax = bounds("5:0x6042:0=i16:-500:500", false);
        let strict = bounds("5:0x6042:0=i16:-500:500", true);
        // A different (unconfigured) subindex.
        let msg = sdo_expedited(5, 0x6042, 9, &100i16.to_le_bytes());
        assert_eq!(CanOpenAdapter::bound_sdo_download(&msg, &lax), Ok(()));
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &strict),
            Err("CANOPEN_SDO_UNCONFIGURED_TARGET_STRICT")
        );
    }

    #[test]
    fn configured_type_width_mismatch_is_denied() {
        // Frame is a size-indicated 2-byte download; config says i32 (width 4).
        let b = bounds("5:0x6042:0=i32:-500:500", false);
        let msg = sdo_expedited(5, 0x6042, 0, &100i16.to_le_bytes());
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b),
            Err("CANOPEN_SDO_WIDTH_MISMATCH")
        );
    }

    #[test]
    fn segmented_download_to_configured_target_is_denied() {
        let b = bounds("5:0x6042:0=i32:-1:1", false);
        // ccs=1, e=0 (segmented), s=1 → command byte 0x21; 4-byte size in data[4..8].
        let idx = 0x6042u16.to_le_bytes();
        let msg = CanOpenMessage {
            node_id: 5,
            function_code: 0xA,
            data: vec![0x21, idx[0], idx[1], 0x00, 0x04, 0x00, 0x00, 0x00],
            source_node: "rtu-1".into(),
        };
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b),
            Err("CANOPEN_SDO_SEGMENTED_UNBOUNDABLE")
        );
    }

    #[test]
    fn upload_request_and_non_sdo_carry_no_setpoint() {
        let b = bounds("5:0x6042:0=i16:-500:500", true); // strict, to prove reads pass
                                                         // SDO upload request (ccs=2 → command byte 0x40) on the SDO channel.
        let idx = 0x6042u16.to_le_bytes();
        let upload = CanOpenMessage {
            node_id: 5,
            function_code: 0xA,
            data: vec![0x40, idx[0], idx[1], 0x00, 0, 0, 0, 0],
            source_node: "rtu-1".into(),
        };
        assert_eq!(CanOpenAdapter::bound_sdo_download(&upload, &b), Ok(()));
        // A PDO (function code 0x6) is not an SDO frame → not bounded here.
        let pdo = CanOpenMessage {
            node_id: 5,
            function_code: 0x6,
            data: vec![0xFF, 0xFF, 0xFF, 0xFF],
            source_node: "rtu-1".into(),
        };
        assert_eq!(CanOpenAdapter::bound_sdo_download(&pdo, &b), Ok(()));
    }

    #[test]
    fn truncated_value_for_configured_type_is_undecodable() {
        // Configured i32 (width 4) but the value region is only 2 meaningful bytes
        // AND size-indicated as 2 → caught first as a width mismatch. To exercise
        // UNDECODABLE, use a NON-size-indicated expedited frame whose value region
        // is shorter than the configured width.
        let b = bounds("5:0x6042:0=i32:-1:1", false);
        let idx = 0x6042u16.to_le_bytes();
        // ccs=1, e=1, s=0 → command byte 0x22; only 2 value bytes present.
        let msg = CanOpenMessage {
            node_id: 5,
            function_code: 0xA,
            data: vec![0x22, idx[0], idx[1], 0x00, 0x01, 0x00],
            source_node: "rtu-1".into(),
        };
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b),
            Err("CANOPEN_SDO_UNDECODABLE")
        );
    }

    #[test]
    fn s0_hidden_trailing_bytes_past_configured_width_are_denied() {
        // #694: NON-size-indicated (s=0) expedited download. The configured type
        // is i16 (width 2). The leading 2 value bytes are a benign in-range 100,
        // but bytes [2..4] carry undeclared attacker-controlled data. `decode_le`
        // reads only the leading width and would silently ignore the rest, so the
        // bound must reject the hidden payload as a width mismatch.
        let b = bounds("5:0x6042:0=i16:-500:500", false);
        let idx = 0x6042u16.to_le_bytes();
        // ccs=1, e=1, s=0 → command byte 0x22; value = 100 (LE) + hidden 0xFFFF.
        let msg = CanOpenMessage {
            node_id: 5,
            function_code: 0xA,
            data: vec![0x22, idx[0], idx[1], 0x00, 0x64, 0x00, 0xFF, 0xFF],
            source_node: "rtu-1".into(),
        };
        assert_eq!(
            CanOpenAdapter::bound_sdo_download(&msg, &b),
            Err("CANOPEN_SDO_WIDTH_MISMATCH")
        );
    }

    #[test]
    fn s0_zero_padded_trailing_bytes_are_admitted() {
        // #694 companion: an s=0 expedited frame whose bytes beyond the configured
        // width are all zero carries no hidden payload — the in-range setpoint is
        // still admitted (the check fails closed only on a NON-zero tail).
        let b = bounds("5:0x6042:0=i16:-500:500", false);
        let idx = 0x6042u16.to_le_bytes();
        // ccs=1, e=1, s=0 → 0x22; value = 100 (LE) then zero padding to 8 bytes.
        let msg = CanOpenMessage {
            node_id: 5,
            function_code: 0xA,
            data: vec![0x22, idx[0], idx[1], 0x00, 0x64, 0x00, 0x00, 0x00],
            source_node: "rtu-1".into(),
        };
        assert_eq!(CanOpenAdapter::bound_sdo_download(&msg, &b), Ok(()));
    }
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
                tracing::warn!(
                    entry,
                    "CANopen node-map: skipping malformed entry (expected `canid:fleet_node`)"
                );
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
    Unattributed {
        canopen_node_id: u8,
        reason: UnattributedReason,
    },
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
        let e = CanOpenAdapter::evaluate(&msg(
            0x3,
            vec![0x10, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        ));
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
        assert!(
            e.triggers_recalculation,
            "NMT Stop must trigger recalculation"
        );
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
        let m = CanOpenMessage {
            node_id: 127,
            function_code: 0xE,
            data: vec![],
            source_node: "n".to_string(),
        };
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
        assert_eq!(
            out,
            NmtOfflineOutcome::Attributed {
                fleet_node_id: "robot-01".to_string()
            }
        );
    }

    #[test]
    fn test_classify_unmapped_is_unattributed_no_mapping() {
        // FAIL-CLOSED: an unmapped offline is never dropped — it is classified
        // Unattributed(NoMapping) for the caller to surface.
        let out = classify_nmt_offline(99, None, false);
        assert_eq!(
            out,
            NmtOfflineOutcome::Unattributed {
                canopen_node_id: 99,
                reason: UnattributedReason::NoMapping
            }
        );
    }

    #[test]
    fn test_classify_mapped_but_unregistered_is_unattributed() {
        // A mapping that points at a node the verifier doesn't know is also
        // fail-closed — we cannot attribute the offline to a real asset.
        let out = classify_nmt_offline(5, Some("ghost".to_string()), false);
        assert_eq!(
            out,
            NmtOfflineOutcome::Unattributed {
                canopen_node_id: 5,
                reason: UnattributedReason::NodeNotRegistered
            }
        );
    }
}
