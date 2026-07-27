//! Evidence-bound ROS motor release payload, V2.
//!
//! This module extends the ADR-0033 ROS release concept without modifying the
//! existing 32-byte `RosTwistPayload` V1 ABI.
//!
//! A V2 release cryptographically binds:
//! - exact enforced linear and angular commands;
//! - perception scan/camera/tracker identity;
//! - platform/profile digest;
//! - Taj evidence digest;
//! - Occy proposal digest;
//! - issue and expiration times;
//! - sequence and single-use nonce.
//!
//! The V2 digest and signature domains are distinct from the V1 ROS twist and
//! frozen SHM governor-contract domains, preventing cross-format replay.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::{ReleaseDenied, ReleaseToken};

pub const ROS_BOUND_COMMAND_DIGEST_DOMAIN: &[u8] = b"KIRRA-ROS-BOUND-COMMAND-DIGEST-V2";
pub const ROS_BOUND_COMMAND_RELEASE_DOMAIN: &[u8] = b"KIRRA-ROS-BOUND-COMMAND-RELEASE-V2";

/// Canonical fixed-width V2 payload.
///
/// Layout:
/// - sequence: 8
/// - nonce: 8
/// - issued_at_ms: 8
/// - expires_at_ms: 8
/// - linear_mps: 8
/// - angular_rad_s: 8
/// - scan_sequence: 8
/// - camera_present: 1
/// - camera padding: 7
/// - camera_sequence: 8
/// - tracker_generation: 8
/// - profile_digest: 32
/// - evidence_digest: 32
/// - proposal_digest: 32
///
/// Total: 176 bytes.
pub const ROS_BOUND_COMMAND_PAYLOAD_LEN: usize = 176;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RosBoundCommandPayload {
    pub sequence: u64,
    pub nonce: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,

    pub linear_mps: f64,
    pub angular_rad_s: f64,

    pub scan_sequence: u64,
    pub camera_sequence: Option<u64>,
    pub tracker_generation: u64,

    pub profile_digest: [u8; 32],
    pub evidence_digest: [u8; 32],
    pub proposal_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosBoundCommandDecodeError {
    NonFiniteCommand,
    InvalidSequence,
    InvalidNonce,
    InvalidScanSequence,
    InvalidTrackerGeneration,
    InvalidCameraEncoding,
    InvalidCameraSequence,
    InvalidTimeRange,
    ZeroProfileDigest,
    ZeroEvidenceDigest,
    ZeroProposalDigest,
}

impl RosBoundCommandPayload {
    #[must_use]
    pub fn encode(&self) -> [u8; ROS_BOUND_COMMAND_PAYLOAD_LEN] {
        let mut out = [0u8; ROS_BOUND_COMMAND_PAYLOAD_LEN];

        out[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..16].copy_from_slice(&self.nonce.to_le_bytes());
        out[16..24].copy_from_slice(&self.issued_at_ms.to_le_bytes());
        out[24..32].copy_from_slice(&self.expires_at_ms.to_le_bytes());

        out[32..40].copy_from_slice(&self.linear_mps.to_le_bytes());
        out[40..48].copy_from_slice(&self.angular_rad_s.to_le_bytes());

        out[48..56].copy_from_slice(&self.scan_sequence.to_le_bytes());

        match self.camera_sequence {
            Some(sequence) => {
                out[56] = 1;
                out[64..72].copy_from_slice(&sequence.to_le_bytes());
            }
            None => {
                out[56] = 0;
            }
        }

        out[72..80].copy_from_slice(&self.tracker_generation.to_le_bytes());
        out[80..112].copy_from_slice(&self.profile_digest);
        out[112..144].copy_from_slice(&self.evidence_digest);
        out[144..176].copy_from_slice(&self.proposal_digest);

        out
    }

    pub fn decode(
        bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN],
    ) -> Result<Self, RosBoundCommandDecodeError> {
        let sequence = read_u64(bytes, 0);
        let nonce = read_u64(bytes, 8);
        let issued_at_ms = read_u64(bytes, 16);
        let expires_at_ms = read_u64(bytes, 24);

        let linear_mps = read_f64(bytes, 32);
        let angular_rad_s = read_f64(bytes, 40);

        let scan_sequence = read_u64(bytes, 48);

        // Reserved camera-presence padding must remain canonical zeros.
        if bytes[57..64].iter().any(|byte| *byte != 0) {
            return Err(RosBoundCommandDecodeError::InvalidCameraEncoding);
        }

        let raw_camera_sequence = read_u64(bytes, 64);
        let camera_sequence = match bytes[56] {
            0 if raw_camera_sequence == 0 => None,
            1 if raw_camera_sequence > 0 => Some(raw_camera_sequence),
            0 | 1 => return Err(RosBoundCommandDecodeError::InvalidCameraSequence),
            _ => return Err(RosBoundCommandDecodeError::InvalidCameraEncoding),
        };

        let tracker_generation = read_u64(bytes, 72);

        let mut profile_digest = [0u8; 32];
        profile_digest.copy_from_slice(&bytes[80..112]);

        let mut evidence_digest = [0u8; 32];
        evidence_digest.copy_from_slice(&bytes[112..144]);

        let mut proposal_digest = [0u8; 32];
        proposal_digest.copy_from_slice(&bytes[144..176]);

        if sequence == 0 {
            return Err(RosBoundCommandDecodeError::InvalidSequence);
        }
        if nonce == 0 {
            return Err(RosBoundCommandDecodeError::InvalidNonce);
        }
        if scan_sequence == 0 {
            return Err(RosBoundCommandDecodeError::InvalidScanSequence);
        }
        if tracker_generation == 0 {
            return Err(RosBoundCommandDecodeError::InvalidTrackerGeneration);
        }
        if !(linear_mps.is_finite() && angular_rad_s.is_finite()) {
            return Err(RosBoundCommandDecodeError::NonFiniteCommand);
        }
        if expires_at_ms <= issued_at_ms {
            return Err(RosBoundCommandDecodeError::InvalidTimeRange);
        }
        if profile_digest == [0u8; 32] {
            return Err(RosBoundCommandDecodeError::ZeroProfileDigest);
        }
        if evidence_digest == [0u8; 32] {
            return Err(RosBoundCommandDecodeError::ZeroEvidenceDigest);
        }
        if proposal_digest == [0u8; 32] {
            return Err(RosBoundCommandDecodeError::ZeroProposalDigest);
        }

        Ok(Self {
            sequence,
            nonce,
            issued_at_ms,
            expires_at_ms,
            linear_mps,
            angular_rad_s,
            scan_sequence,
            camera_sequence,
            tracker_generation,
            profile_digest,
            evidence_digest,
            proposal_digest,
        })
    }
}

fn read_u64(bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("fixed V2 payload slice"),
    )
}

fn read_f64(bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN], offset: usize) -> f64 {
    f64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("fixed V2 payload slice"),
    )
}

#[must_use]
pub fn ros_bound_command_digest(payload_bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ROS_BOUND_COMMAND_DIGEST_DOMAIN);
    hasher.update((payload_bytes.len() as u64).to_le_bytes());
    hasher.update(payload_bytes);
    hasher.finalize().into()
}

fn release_signing_payload(
    digest: &[u8; 32],
) -> [u8; ROS_BOUND_COMMAND_RELEASE_DOMAIN.len() + 8 + 32] {
    let mut out = [0u8; ROS_BOUND_COMMAND_RELEASE_DOMAIN.len() + 8 + 32];
    let mut offset = 0;

    out[offset..offset + ROS_BOUND_COMMAND_RELEASE_DOMAIN.len()]
        .copy_from_slice(ROS_BOUND_COMMAND_RELEASE_DOMAIN);
    offset += ROS_BOUND_COMMAND_RELEASE_DOMAIN.len();

    out[offset..offset + 8].copy_from_slice(&(32u64).to_le_bytes());
    offset += 8;

    out[offset..offset + 32].copy_from_slice(digest);

    out
}

#[must_use]
pub fn issue_ros_bound_command_release(
    payload: &RosBoundCommandPayload,
    signing_key: &SigningKey,
) -> ReleaseToken {
    let digest = ros_bound_command_digest(&payload.encode());
    let signature = signing_key.sign(&release_signing_payload(&digest));

    ReleaseToken {
        digest,
        signature: signature.to_bytes(),
    }
}

pub fn verify_ros_bound_command_release(
    token: &ReleaseToken,
    payload_bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN],
    governor_vk: &VerifyingKey,
) -> Result<(), ReleaseDenied> {
    let expected = ros_bound_command_digest(payload_bytes);

    if token.digest != expected {
        return Err(ReleaseDenied::DigestMismatch);
    }

    let signature = Signature::from_bytes(&token.signature);

    governor_vk
        .verify_strict(&release_signing_payload(&token.digest), &signature)
        .map_err(|_| ReleaseDenied::SignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ros_twist::{
        issue_ros_release, verify_ros_release, RosTwistPayload, ROS_TWIST_PAYLOAD_LEN,
    };

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    fn payload() -> RosBoundCommandPayload {
        RosBoundCommandPayload {
            sequence: 10,
            nonce: 9001,
            issued_at_ms: 10_000,
            expires_at_ms: 10_200,
            linear_mps: 0.75,
            angular_rad_s: -0.25,
            scan_sequence: 77,
            camera_sequence: Some(88),
            tracker_generation: 3,
            profile_digest: [0x11; 32],
            evidence_digest: [0x22; 32],
            proposal_digest: [0x33; 32],
        }
    }

    #[test]
    fn payload_round_trips_bit_exactly() {
        let original = payload();
        let decoded = RosBoundCommandPayload::decode(&original.encode()).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(decoded.linear_mps.to_bits(), original.linear_mps.to_bits());
        assert_eq!(
            decoded.angular_rad_s.to_bits(),
            original.angular_rad_s.to_bits()
        );
    }

    #[test]
    fn honest_bound_release_verifies() {
        let sk = signing_key();
        let payload = payload();
        let token = issue_ros_bound_command_release(&payload, &sk);

        assert_eq!(
            verify_ros_bound_command_release(&token, &payload.encode(), &sk.verifying_key()),
            Ok(())
        );
    }

    #[test]
    fn every_bound_field_is_cryptographically_protected() {
        let sk = signing_key();
        let approved = payload();
        let token = issue_ros_bound_command_release(&approved, &sk);

        let mut variants = Vec::new();

        let mut changed = approved;
        changed.linear_mps = 1.5;
        variants.push(changed);

        let mut changed = approved;
        changed.angular_rad_s = 0.5;
        variants.push(changed);

        let mut changed = approved;
        changed.scan_sequence += 1;
        variants.push(changed);

        let mut changed = approved;
        changed.camera_sequence = None;
        variants.push(changed);

        let mut changed = approved;
        changed.tracker_generation += 1;
        variants.push(changed);

        let mut changed = approved;
        changed.profile_digest[0] ^= 1;
        variants.push(changed);

        let mut changed = approved;
        changed.evidence_digest[0] ^= 1;
        variants.push(changed);

        let mut changed = approved;
        changed.proposal_digest[0] ^= 1;
        variants.push(changed);

        let mut changed = approved;
        changed.expires_at_ms += 1;
        variants.push(changed);

        let mut changed = approved;
        changed.nonce += 1;
        variants.push(changed);

        for changed in variants {
            assert_eq!(
                verify_ros_bound_command_release(&token, &changed.encode(), &sk.verifying_key()),
                Err(ReleaseDenied::DigestMismatch)
            );
        }
    }

    #[test]
    fn malformed_payloads_fail_closed() {
        let mut p = payload();
        p.linear_mps = f64::NAN;
        assert_eq!(
            RosBoundCommandPayload::decode(&p.encode()),
            Err(RosBoundCommandDecodeError::NonFiniteCommand)
        );

        let mut p = payload();
        p.nonce = 0;
        assert_eq!(
            RosBoundCommandPayload::decode(&p.encode()),
            Err(RosBoundCommandDecodeError::InvalidNonce)
        );

        let mut p = payload();
        p.expires_at_ms = p.issued_at_ms;
        assert_eq!(
            RosBoundCommandPayload::decode(&p.encode()),
            Err(RosBoundCommandDecodeError::InvalidTimeRange)
        );

        let mut p = payload();
        p.proposal_digest = [0u8; 32];
        assert_eq!(
            RosBoundCommandPayload::decode(&p.encode()),
            Err(RosBoundCommandDecodeError::ZeroProposalDigest)
        );
    }

    #[test]
    fn noncanonical_camera_encoding_is_rejected() {
        let mut encoded = payload().encode();
        encoded[57] = 1;

        assert_eq!(
            RosBoundCommandPayload::decode(&encoded),
            Err(RosBoundCommandDecodeError::InvalidCameraEncoding)
        );

        let mut encoded = payload().encode();
        encoded[56] = 0;

        assert_eq!(
            RosBoundCommandPayload::decode(&encoded),
            Err(RosBoundCommandDecodeError::InvalidCameraSequence)
        );
    }

    #[test]
    fn v1_and_v2_tokens_cannot_cross_verify() {
        let sk = signing_key();

        let v1 = RosTwistPayload {
            sequence: 10,
            issued_at_ms: 10_000,
            linear_mps: 0.75,
            angular_rad_s: -0.25,
        };

        let v2 = payload();

        let v1_token = issue_ros_release(&v1, &sk);
        let v2_token = issue_ros_bound_command_release(&v2, &sk);

        assert_eq!(
            verify_ros_bound_command_release(&v1_token, &v2.encode(), &sk.verifying_key()),
            Err(ReleaseDenied::DigestMismatch)
        );

        let v1_bytes: [u8; ROS_TWIST_PAYLOAD_LEN] = v1.encode();
        assert_eq!(
            verify_ros_release(&v2_token, &v1_bytes, &sk.verifying_key()),
            Err(ReleaseDenied::DigestMismatch)
        );
    }

    #[test]
    fn digest_is_stable_for_equivalent_payloads() {
        let first = payload();
        let second = payload();

        assert_eq!(first.encode(), second.encode());
        assert_eq!(
            ros_bound_command_digest(&first.encode()),
            ros_bound_command_digest(&second.encode())
        );
    }
}
