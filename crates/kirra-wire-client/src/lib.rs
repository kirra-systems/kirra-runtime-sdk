// crates/kirra-wire-client/src/lib.rs
//
// kirra-wire-client — the SINGLE shared client-side mirror of the
// `kirra-governor-service` UDP wire schema. Pure serde + bincode + std; it does
// NOT depend on `kirra-runtime-sdk` / the verdict core. Both the dev bench
// (`kirra-proposal-bench`) and the future car bridge node reuse THIS crate so
// the wire types are defined exactly once on the client side.
//
// DEV/TEST ONLY — not a shipped or safety artifact. The governor's verdict core
// (`src/gateway/kinematics_contract.rs`) is the sole source of truth; this is a
// convenience mirror for talking to it over the wire.
//
// ┌───────────────────────────────────────────────────────────────────────────┐
// │ WIRE-FORMAT INVARIANT — READ BEFORE EDITING                                 │
// │                                                                             │
// │ bincode puts NO field/variant NAMES on the wire: it encodes enums by their  │
// │ POSITIONAL VARIANT INDEX (a u32 tag) and structs by FIELD ORDER. Therefore  │
// │ the variant order of `ClientEnforceAction` / `ClientDenyCode` and the field │
// │ order of `ProposedCommand` / `Proposal` / `Verdict` MUST EXACTLY MIRROR:    │
// │                                                                             │
// │   • src/gateway/kinematics_contract.rs  →  EnforceAction, DenyCode,         │
// │                                            ProposedVehicleCommand           │
// │   • crates/kirra-governor-service/src/main.rs  →  Proposal, Verdict         │
// │                                                                             │
// │ Reordering or inserting a variant/field on EITHER side silently corrupts    │
// │ every decode (a deny would read as a different deny; a clamp as garbage).   │
// │ The `wire_layout` tests below pin the byte layout and fail on drift.        │
// └───────────────────────────────────────────────────────────────────────────┘

use serde::{Deserialize, Serialize};

/// Mirror of core `ProposedVehicleCommand` — fields in the SAME ORDER.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedCommand {
    /// Desired forward velocity at end of step (m/s); negative = reverse.
    pub linear_velocity_mps: f64,
    /// Actual forward velocity at start of step (m/s).
    pub current_velocity_mps: f64,
    /// Planning step duration (s); must be > 0.
    pub delta_time_s: f64,
    /// Desired steering angle at end of step (deg); +left (ISO 8855).
    pub steering_angle_deg: f64,
    /// Actual steering angle at start of step (deg).
    pub current_steering_angle_deg: f64,
}

/// Mirror of the governor's `Proposal` (car → governor). Field order MUST match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub seq: u64,
    pub ts_nanos: u128,
    pub command: ProposedCommand,
}

/// Mirror of core `DenyCode` — variants in the SAME ORDER (the index IS the
/// wire tag). The trailing `// N` is the bincode variant index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientDenyCode {
    NanInfLinearVelocity,        // 0
    NanInfCurrentVelocity,       // 1
    NanInfSteeringAngle,         // 2
    NanInfCurrentSteering,       // 3
    NanInfDeltaTime,             // 4
    InvalidTimeDelta,            // 5
    AssetLockedOut,              // 6
    DrivableSpaceDeparture,      // 7
    DegradedReinitiationDenied,  // 8
    DegradedSpeedIncreaseDenied, // 9
    FrameIntegrityUntrusted,     // 10  (Stage S-FI1 — appended last; index = wire tag)
    TrajectoryHorizonExceeded,   // 11  (review B10 — over-horizon doer-contract violation)
}

/// Mirror of core `EnforceAction` — variants in the SAME ORDER.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientEnforceAction {
    Allow,                      // 0
    ClampLinear(f64),           // 1
    ClampSteering(f64),         // 2
    DenyBreach(ClientDenyCode), // 3
}

/// Mirror of the governor's `Verdict` (governor → car). The governor's
/// `Verdict.action` is `Serialize`-only; here `ClientEnforceAction` is fully
/// (de)serializable so the client can DECODE the reply. `Serialize` is derived
/// too only so the drift tests can re-encode and byte-compare.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub seq: u64,
    pub action: ClientEnforceAction,
    pub reason_code: u32,
}

// bincode 2.x defaults to VARINT encoding, which would change the wire bytes.
// `legacy()` is the bincode-1.x-compatible config (fixint, little-endian, u32
// enum tags), so the wire format — pinned byte-for-byte by the `wire_layout`
// tests — is unchanged across the 1.x -> 2.x upgrade.
#[inline]
fn wire_cfg() -> impl bincode::config::Config {
    bincode::config::legacy()
}

/// Encode a `Proposal` for the wire (matches the governor's encoder).
pub fn encode_proposal(p: &Proposal) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(p, wire_cfg())
}

/// Decode a `Verdict` from the wire (matches the governor's encoder).
///
/// Strict framing: the decoded length MUST consume the WHOLE datagram. Trailing
/// bytes (a truncated/over-long/corrupt frame whose prefix happens to decode)
/// are rejected fail-closed rather than silently ignored — a malformed verdict
/// must never be accepted as a clean one.
pub fn decode_verdict(bytes: &[u8]) -> Result<Verdict, bincode::error::DecodeError> {
    let (v, len) = bincode::serde::decode_from_slice(bytes, wire_cfg())?;
    if len != bytes.len() {
        return Err(bincode::error::DecodeError::Other(
            "verdict frame has trailing bytes (length mismatch)",
        ));
    }
    Ok(v)
}

#[cfg(test)]
mod wire_layout {
    use super::*;

    // bincode free-function config is fixint + little-endian. These literal
    // byte sequences are computed by hand from that layout, so they pin the wire
    // format independently of this crate's own code — if anyone reorders a
    // variant/field, decode lands on the wrong variant and these fail.

    /// A `DenyBreach(NanInfDeltaTime)` verdict, seq=7, reason_code=5.
    /// Layout: seq u64 LE | action: enum-index 3 (DenyBreach) u32 LE,
    ///                       inner DenyCode index 4 (NanInfDeltaTime) u32 LE
    ///       | reason_code u32 LE
    const DENY_VERDICT_BYTES: [u8; 20] = [
        7, 0, 0, 0, 0, 0, 0, 0, // seq = 7
        3, 0, 0, 0, // EnforceAction::DenyBreach (index 3)
        4, 0, 0, 0, // DenyCode::NanInfDeltaTime (index 4)
        5, 0, 0, 0, // reason_code = 5
    ];

    #[test]
    fn known_bytes_deny_verdict_decodes_and_reencodes() {
        let v = decode_verdict(&DENY_VERDICT_BYTES).expect("decode known bytes");
        assert_eq!(
            v,
            Verdict {
                seq: 7,
                action: ClientEnforceAction::DenyBreach(ClientDenyCode::NanInfDeltaTime),
                reason_code: 5,
            },
            "known deny verdict must decode to the mirrored variant — if this fails, \
             the enum order drifted from the core"
        );
        // Re-encode must reproduce the exact bytes (round-trip closes the loop).
        assert_eq!(bincode::serde::encode_to_vec(&v, wire_cfg()).unwrap(), DENY_VERDICT_BYTES);
    }

    #[test]
    fn known_bytes_allow_verdict() {
        // seq=0, Allow (index 0, no payload), reason_code=0 → 16 zero bytes.
        let bytes = [0u8; 16];
        let v = decode_verdict(&bytes).expect("decode");
        assert_eq!(v.action, ClientEnforceAction::Allow, "index 0 must be Allow");
        assert_eq!(v.seq, 0);
        assert_eq!(v.reason_code, 0);
        assert_eq!(bincode::serde::encode_to_vec(&v, wire_cfg()).unwrap(), bytes);
    }

    #[test]
    fn all_twelve_deny_codes_pin_their_index() {
        // Each DenyCode must encode at its declared position. The DenyBreach
        // inner index sits at byte offset 12 (seq[0..8] + DenyBreach-tag[8..12]).
        let codes = [
            ClientDenyCode::NanInfLinearVelocity,
            ClientDenyCode::NanInfCurrentVelocity,
            ClientDenyCode::NanInfSteeringAngle,
            ClientDenyCode::NanInfCurrentSteering,
            ClientDenyCode::NanInfDeltaTime,
            ClientDenyCode::InvalidTimeDelta,
            ClientDenyCode::AssetLockedOut,
            ClientDenyCode::DrivableSpaceDeparture,
            ClientDenyCode::DegradedReinitiationDenied,
            ClientDenyCode::DegradedSpeedIncreaseDenied,
            ClientDenyCode::FrameIntegrityUntrusted, // 10 (Stage S-FI1)
            ClientDenyCode::TrajectoryHorizonExceeded, // 11 (review B10)
        ];
        for (i, code) in codes.iter().enumerate() {
            let v = Verdict {
                seq: 0,
                action: ClientEnforceAction::DenyBreach(*code),
                reason_code: 0,
            };
            let bytes = bincode::serde::encode_to_vec(&v, wire_cfg()).unwrap();
            assert_eq!(bytes[8], 3, "DenyBreach must be EnforceAction index 3");
            assert_eq!(
                bytes[12], i as u8,
                "{code:?} must encode at DenyCode index {i} (wire-order drift!)"
            );
        }
    }

    #[test]
    fn clamp_payload_round_trips() {
        // Exercises the f64 payload on ClampLinear/ClampSteering (indices 1/2).
        for action in [
            ClientEnforceAction::ClampLinear(22.35),
            ClientEnforceAction::ClampSteering(-12.5),
        ] {
            let v = Verdict { seq: 99, action: action.clone(), reason_code: 0 };
            let bytes = bincode::serde::encode_to_vec(&v, wire_cfg()).unwrap();
            assert_eq!(decode_verdict(&bytes).unwrap(), v);
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        // A well-formed allow verdict (16 bytes) with one extra byte appended must
        // be refused — strict framing, fail-closed (a corrupt/over-long datagram
        // is never accepted as a clean verdict).
        let mut bytes = vec![0u8; 16];
        bytes.push(0xAB);
        assert!(
            decode_verdict(&bytes).is_err(),
            "a frame with trailing bytes must be rejected, not silently truncated"
        );
        // The exact-length frame still decodes.
        assert!(decode_verdict(&bytes[..16]).is_ok());
    }

    #[test]
    fn proposal_encodes_with_expected_prefix() {
        // seq=1 (u64), ts_nanos=0 (u128 = 16 bytes), then the 5 f64 command
        // fields → 8 + 16 + 40 = 64 bytes total; seq prefix is 01 00..00.
        let p = Proposal {
            seq: 1,
            ts_nanos: 0,
            command: ProposedCommand {
                linear_velocity_mps: 1.0,
                current_velocity_mps: 1.0,
                delta_time_s: 0.1,
                steering_angle_deg: 0.0,
                current_steering_angle_deg: 0.0,
            },
        };
        let bytes = encode_proposal(&p).unwrap();
        assert_eq!(bytes.len(), 8 + 16 + 5 * 8, "fixed-schema proposal size");
        assert_eq!(&bytes[0..8], &[1, 0, 0, 0, 0, 0, 0, 0], "seq u64 LE prefix");
    }
}
