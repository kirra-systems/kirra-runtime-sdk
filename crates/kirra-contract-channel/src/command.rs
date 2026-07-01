//! The frozen **command payload** carried by [`GovernorContractView::command`]
//! (ADR-0006 Clause 2 / HVCHAN-001 §2.2 — the doer↔checker actuator command that
//! crosses the guest↔host partition boundary, L3).
//!
//! The view's `command` field is an opaque `[u8; MAX_COMMAND_BYTES]`; THIS module
//! defines what those bytes MEAN for [`LAYOUT_VERSION`](crate::LAYOUT_VERSION) 1. The payload is the
//! vehicle motion command the governor bounds — the on-wire form of the checker's
//! input type `ProposedVehicleCommand` (`kirra_core::kinematics_contract`): the
//! DOER (QM planner, guest) proposes it, the CHECKER (ASIL governor) validates it
//! with `validate_vehicle_command`. Keeping the codec HERE — in the lean, no_std,
//! zero-dep, `#![forbid(unsafe_code)]` contract crate rather than pulling the
//! heavy `kirra-core` into the governor partition — keeps Clause 2's promise: the
//! cross-partition TCB imports a struct definition, not a library.
//!
//! ## The frozen encoding (LAYOUT_VERSION 1)
//!
//! Five little-endian `f64` fields, offsets pinned by the assertions below. The
//! encoding is the safety contract, exactly like [`GovernorContractView`]'s: any
//! change to a field, its order, or its width is a NEW [`LAYOUT_VERSION`](crate::LAYOUT_VERSION), never an
//! in-place edit.
//!
//! ```text
//! off  field                          type
//!   0  linear_velocity_mps            f64   desired forward velocity, end of step
//!   8  current_velocity_mps           f64   actual forward velocity, start of step
//!  16  delta_time_s                   f64   planning step duration
//!  24  steering_angle_deg             f64   desired steering, end of step (ISO 8855, +left)
//!  32  current_steering_angle_deg     f64   actual steering, start of step
//! ```
//!
//! ## Fail-closed decode
//!
//! [`VehicleCommandPayload::decode`] rejects two fault classes at the boundary:
//! a wrong length ([`CommandCodecError::LengthMismatch`]) and any **non-finite**
//! field ([`CommandCodecError::NonFinite`] — NaN/±Inf, which would poison every
//! downstream comparison and clamp). This is **defense in depth** that composes
//! with — never replaces — `validate_vehicle_command`'s own Priority-0 NaN/Inf
//! guard: a malformed publisher whose bytes pass CRC (genuinely-written NaN) is
//! refused before the command is ever handed to the governor. Semantic bounds
//! (e.g. `delta_time_s > 0`, envelope limits) remain the governor's job, not the
//! codec's — the codec is a faithful, fail-closed (de)serializer, not the checker.

use crate::view::{GovernorContractView, MAX_COMMAND_BYTES};

/// Wire length of a [`VehicleCommandPayload`]: five little-endian `f64`s.
pub const COMMAND_PAYLOAD_LEN: usize = 40;

/// Which field failed the finiteness check, so a reject names the offending
/// scalar (mirrors `validate_vehicle_command`'s per-field NaN codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandField {
    LinearVelocityMps,
    CurrentVelocityMps,
    DeltaTimeS,
    SteeringAngleDeg,
    CurrentSteeringAngleDeg,
}

/// A fail-closed command-payload decode failure. Every variant is a reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCodecError {
    /// Payload byte length is not exactly [`COMMAND_PAYLOAD_LEN`] (structural —
    /// a truncated/oversize/short-version payload is never best-effort parsed).
    LengthMismatch { found: usize, expected: usize },
    /// A field decoded to a non-finite `f64` (NaN or ±Inf) — refused at the
    /// boundary before it can poison the governor's arithmetic.
    NonFinite { field: CommandField },
}

/// The frozen vehicle-command payload (LAYOUT_VERSION 1). The on-wire form of
/// `kirra_core::kinematics_contract::ProposedVehicleCommand`; field semantics are
/// identical (see the module docs). Kept `#[repr(C)]` so its own byte layout is
/// pinned too, though [`encode`](Self::encode) is the authoritative wire form.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VehicleCommandPayload {
    /// Desired forward velocity at end of this step (m/s; negative = reverse).
    pub linear_velocity_mps: f64,
    /// Actual forward velocity at start of this step (m/s).
    pub current_velocity_mps: f64,
    /// Duration of this planning step (seconds). The governor requires `> 0`; the
    /// codec only guarantees it is finite.
    pub delta_time_s: f64,
    /// Desired steering angle at end of this step (degrees; +left, ISO 8855).
    pub steering_angle_deg: f64,
    /// Actual steering angle at start of this step (degrees).
    pub current_steering_angle_deg: f64,
}

impl VehicleCommandPayload {
    /// Serialize to the frozen little-endian [`COMMAND_PAYLOAD_LEN`]-byte image.
    /// Total — no failure mode (any `f64`, finite or not, serializes); the
    /// finiteness gate is on [`decode`](Self::decode), the trust boundary.
    pub fn encode(&self) -> [u8; COMMAND_PAYLOAD_LEN] {
        let mut out = [0u8; COMMAND_PAYLOAD_LEN];
        out[0..8].copy_from_slice(&self.linear_velocity_mps.to_le_bytes());
        out[8..16].copy_from_slice(&self.current_velocity_mps.to_le_bytes());
        out[16..24].copy_from_slice(&self.delta_time_s.to_le_bytes());
        out[24..32].copy_from_slice(&self.steering_angle_deg.to_le_bytes());
        out[32..40].copy_from_slice(&self.current_steering_angle_deg.to_le_bytes());
        out
    }

    /// Parse the frozen little-endian image, failing closed on a wrong length or
    /// any non-finite field. `bytes` is typically
    /// [`GovernorContractView::validated_command`] AFTER [`validate`](crate::validate)
    /// has passed (CRC/bounds already checked); the length check here also guards a
    /// caller that skipped that path.
    pub fn decode(bytes: &[u8]) -> Result<Self, CommandCodecError> {
        if bytes.len() != COMMAND_PAYLOAD_LEN {
            return Err(CommandCodecError::LengthMismatch {
                found: bytes.len(),
                expected: COMMAND_PAYLOAD_LEN,
            });
        }
        let f64_at = |o: usize| {
            f64::from_le_bytes([
                bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3], bytes[o + 4], bytes[o + 5],
                bytes[o + 6], bytes[o + 7],
            ])
        };
        let cmd = Self {
            linear_velocity_mps: f64_at(0),
            current_velocity_mps: f64_at(8),
            delta_time_s: f64_at(16),
            steering_angle_deg: f64_at(24),
            current_steering_angle_deg: f64_at(32),
        };
        // Fail closed on any non-finite field (defense-in-depth with the
        // governor's Priority-0 guard).
        for (value, field) in [
            (cmd.linear_velocity_mps, CommandField::LinearVelocityMps),
            (cmd.current_velocity_mps, CommandField::CurrentVelocityMps),
            (cmd.delta_time_s, CommandField::DeltaTimeS),
            (cmd.steering_angle_deg, CommandField::SteeringAngleDeg),
            (cmd.current_steering_angle_deg, CommandField::CurrentSteeringAngleDeg),
        ] {
            if !value.is_finite() {
                return Err(CommandCodecError::NonFinite { field });
            }
        }
        Ok(cmd)
    }

    /// Build a [`GovernorContractView`] body carrying this command, with a
    /// freshly computed CRC (via [`GovernorContractView::new_command`]). The
    /// payload always fits [`MAX_COMMAND_BYTES`] (freeze-asserted below), so this
    /// never fails.
    ///
    /// Note: when publishing via [`publish`](crate::publish), the shared-region
    /// `generation` comes from the seqlock counter — `store_body` deliberately
    /// does not write the `generation` field — so the `generation` argument here
    /// only sets the returned body's field (useful when building a standalone
    /// view, e.g. for `canonical_image`), not the published region's counter.
    pub fn to_view(
        &self,
        generation: u64,
        sequence: u64,
        publication_nanos: u64,
        deadline_nanos: u64,
    ) -> GovernorContractView {
        let encoded = self.encode();
        GovernorContractView::new_command(
            generation,
            sequence,
            publication_nanos,
            deadline_nanos,
            &encoded,
        )
        // COMMAND_PAYLOAD_LEN <= MAX_COMMAND_BYTES is freeze-asserted, so
        // new_command's oversize branch is unreachable here.
        .expect("COMMAND_PAYLOAD_LEN <= MAX_COMMAND_BYTES (freeze-asserted)")
    }

    /// Decode the command out of a view that has ALREADY passed
    /// [`validate`](crate::validate). Uses [`GovernorContractView::validated_command`]
    /// (returns a [`CommandCodecError::LengthMismatch`] mapped from an out-of-range
    /// `command_len`, though a validated view cannot hit that), then [`decode`](Self::decode).
    pub fn from_validated_view(view: &GovernorContractView) -> Result<Self, CommandCodecError> {
        let bytes = view.validated_command().ok_or(CommandCodecError::LengthMismatch {
            found: view.command_len as usize,
            expected: COMMAND_PAYLOAD_LEN,
        })?;
        Self::decode(bytes)
    }
}

// --------------------------------------------------------------------------
// FREEZE ASSERTIONS — the command encoding IS part of the Clause 2 contract.
// A change to the length or a field width breaks the build here; fix it by
// minting a new LAYOUT_VERSION, not by editing these numbers.
// --------------------------------------------------------------------------
const _: () = assert!(COMMAND_PAYLOAD_LEN == 40);
const _: () = assert!(COMMAND_PAYLOAD_LEN <= MAX_COMMAND_BYTES);
const _: () = assert!(COMMAND_PAYLOAD_LEN == 5 * core::mem::size_of::<f64>());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{publish, read_coherent_snapshot, validate, AcceptedWatermark, MAX_SNAPSHOT_RETRIES};

    fn sample() -> VehicleCommandPayload {
        VehicleCommandPayload {
            linear_velocity_mps: 3.5,
            current_velocity_mps: 3.0,
            delta_time_s: 0.1,
            steering_angle_deg: -4.25,
            current_steering_angle_deg: -4.0,
        }
    }

    #[test]
    fn encode_decode_roundtrips() {
        let cmd = sample();
        assert_eq!(cmd.encode().len(), COMMAND_PAYLOAD_LEN);
        assert_eq!(VehicleCommandPayload::decode(&cmd.encode()), Ok(cmd));
    }

    #[test]
    fn encode_is_little_endian_at_fixed_offsets() {
        let cmd = sample();
        let img = cmd.encode();
        assert_eq!(&img[0..8], &3.5f64.to_le_bytes());
        assert_eq!(&img[8..16], &3.0f64.to_le_bytes());
        assert_eq!(&img[16..24], &0.1f64.to_le_bytes());
        assert_eq!(&img[24..32], &(-4.25f64).to_le_bytes());
        assert_eq!(&img[32..40], &(-4.0f64).to_le_bytes());
    }

    #[test]
    fn decode_rejects_wrong_length() {
        assert_eq!(
            VehicleCommandPayload::decode(&[0u8; COMMAND_PAYLOAD_LEN - 1]),
            Err(CommandCodecError::LengthMismatch {
                found: COMMAND_PAYLOAD_LEN - 1,
                expected: COMMAND_PAYLOAD_LEN,
            })
        );
        assert_eq!(
            VehicleCommandPayload::decode(&[0u8; COMMAND_PAYLOAD_LEN + 1]),
            Err(CommandCodecError::LengthMismatch {
                found: COMMAND_PAYLOAD_LEN + 1,
                expected: COMMAND_PAYLOAD_LEN,
            })
        );
    }

    #[test]
    fn decode_rejects_nan_and_inf_in_every_field() {
        // Each field, poisoned in turn, must be named in the reject.
        let cases = [
            (0usize, CommandField::LinearVelocityMps),
            (8, CommandField::CurrentVelocityMps),
            (16, CommandField::DeltaTimeS),
            (24, CommandField::SteeringAngleDeg),
            (32, CommandField::CurrentSteeringAngleDeg),
        ];
        for (offset, field) in cases {
            for poison in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut img = sample().encode();
                img[offset..offset + 8].copy_from_slice(&poison.to_le_bytes());
                assert_eq!(
                    VehicleCommandPayload::decode(&img),
                    Err(CommandCodecError::NonFinite { field }),
                    "offset {offset} poison {poison}"
                );
            }
        }
    }

    #[test]
    fn to_view_then_from_validated_view_roundtrips_after_validate() {
        // The full L3 producer->consumer path over the in-process region:
        // encode -> view -> publish (seqlock) -> coherent read -> validate -> decode.
        let region = crate::reference::InProcessRegion::new();
        let cmd = sample();
        let body = cmd.to_view(0, 1, 1_000, 10_000);
        let committed = publish(&region, 0, &body);

        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        assert_eq!(snap.generation, committed);

        let mut wm = AcceptedWatermark::new();
        validate(&snap, 5_000, &wm).expect("fresh command validates");
        wm.record(&snap);

        assert_eq!(VehicleCommandPayload::from_validated_view(&snap), Ok(cmd));
    }

    #[test]
    fn replayed_command_is_rejected_before_decode() {
        // A second publish at the SAME sequence is a replay: validate rejects it,
        // so from_validated_view is never reached (the boundary refuses it).
        let region = crate::reference::InProcessRegion::new();
        let cmd = sample();
        let mut wm = AcceptedWatermark::new();

        let first = cmd.to_view(0, 7, 0, 10_000);
        let g1 = publish(&region, 0, &first);
        let s1 = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        validate(&s1, 5_000, &wm).expect("first accepts");
        wm.record(&s1);

        // Same sequence 7 again (generation advances, sequence does not).
        let replay = cmd.to_view(g1, 7, 0, 10_000);
        let _ = publish(&region, g1, &replay);
        let s2 = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        assert_eq!(
            validate(&s2, 5_000, &wm),
            Err(crate::ContractFault::SequenceRegressOrReplay { found: 7, last_accepted: 7 })
        );
    }

    #[test]
    fn a_flipped_command_byte_is_caught_by_crc_before_decode() {
        // Corruption in the payload is a CRC fault at validate, upstream of the
        // codec — decode never sees corrupt bytes on the validated path.
        let cmd = sample();
        let mut view = cmd.to_view(2, 3, 0, 10_000);
        view.command[0] ^= 0xFF; // flip a payload byte; crc32 now stale
        let wm = AcceptedWatermark::new();
        assert!(matches!(
            validate(&view, 5_000, &wm),
            Err(crate::ContractFault::CrcMismatch { .. })
        ));
    }
}
