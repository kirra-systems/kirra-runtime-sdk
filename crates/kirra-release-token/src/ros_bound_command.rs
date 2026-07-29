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

/// Why the evidence-bound motor gate refused release.
///
/// Every refusal is fail-closed and leaves sequence, nonce, and perception-frame
/// watermarks unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosBoundCommandRefusal {
    NoToken,
    Denied(ReleaseDenied),
    Undecodable(RosBoundCommandDecodeError),

    /// The payload claims to have been issued after the consumer's current time.
    FutureIssued {
        issued_at_ms: u64,
        now_ms: u64,
    },
    /// #1230 Part B: the token was minted before this consumer process
    /// booted. A restart is a new trust epoch; pre-boot tokens are refused
    /// regardless of how much signed lifetime they have left, so the
    /// post-restart replay window no longer scales with
    /// `KIRRA_FRESHNESS_WINDOW_MS`.
    TokenPredatesBoot {
        issued_at_ms: u64,
        boot_wall_ms: u64,
    },

    /// The signed command has reached or passed its explicit expiration.
    Expired {
        expires_at_ms: u64,
        now_ms: u64,
    },

    /// The author requested a lifetime larger than the configured safety bound.
    LifetimeTooLong {
        lifetime_ms: u64,
        maximum_ms: u64,
    },

    /// The command sequence did not strictly advance.
    SequenceNotAdvanced {
        presented: u64,
        last_released: u64,
    },

    /// The nonce did not strictly advance.
    ///
    /// V2 deliberately requires monotonic nonces rather than retaining an
    /// unbounded set of every nonce ever observed. This provides single-use
    /// behavior with constant memory on the actuation hot path.
    NonceNotAdvanced {
        presented: u64,
        last_released: u64,
    },

    /// The presented platform/profile identity does not match the consumer's
    /// configured deployment identity.
    ProfileDigestMismatch,

    /// A newer perception frame has already released.
    PerceptionFrameSuperseded {
        presented_tracker_generation: u64,
        presented_scan_sequence: u64,
        latest_tracker_generation: u64,
        latest_scan_sequence: u64,
    },
}

/// Motor-side verification gate for the V2 evidence-bound ROS payload.
///
/// The gate owns only constant-size state:
/// - last released command sequence;
/// - last released nonce;
/// - newest released Taj tracker/scan identity.
///
/// A full release verifies the exact signed bytes, decodes all bound fields,
/// checks profile identity and timing, rejects superseded evidence, and only
/// then atomically advances the in-memory watermarks.
pub struct RosBoundCommandGate {
    governor_vk: VerifyingKey,
    expected_profile_digest: [u8; 32],
    maximum_lifetime_ms: u64,
    /// #1230 Part B — the consumer's boot instant (wall ms). Tokens ISSUED
    /// before this are refused regardless of remaining lifetime: a restart is
    /// a new trust epoch, and a pre-boot mint belongs to the previous one.
    /// Interim, deliberately: same-host clock domain on the ADR-0033 topology
    /// makes the comparison sound in the common case, but a backward wall
    /// step DURING the restart re-opens the window — the structural fix (a
    /// boot epoch bound into the signed payload, Part A) is the ADR revision
    /// tracked on the issue. Strict `<`: a token minted at the boot instant
    /// is admitted.
    boot_wall_ms: u64,

    last_released_sequence: Option<u64>,
    last_released_nonce: Option<u64>,
    latest_perception_frame: Option<(u64, u64)>,
}

impl RosBoundCommandGate {
    #[must_use]
    pub fn new(
        governor_vk: VerifyingKey,
        expected_profile_digest: [u8; 32],
        maximum_lifetime_ms: u64,
        boot_wall_ms: u64,
    ) -> Self {
        Self {
            governor_vk,
            expected_profile_digest,
            maximum_lifetime_ms,
            boot_wall_ms,
            last_released_sequence: None,
            last_released_nonce: None,
            latest_perception_frame: None,
        }
    }

    pub fn release(
        &mut self,
        payload_bytes: &[u8; ROS_BOUND_COMMAND_PAYLOAD_LEN],
        token: Option<&ReleaseToken>,
        now_ms: u64,
    ) -> Result<RosBoundCommandPayload, RosBoundCommandRefusal> {
        // 1. No proof means no motion.
        let token = token.ok_or(RosBoundCommandRefusal::NoToken)?;

        // 2. The signature must approve exactly the presented V2 bytes.
        verify_ros_bound_command_release(token, payload_bytes, &self.governor_vk)
            .map_err(RosBoundCommandRefusal::Denied)?;

        // 3. Decode and structurally validate every bound field.
        let payload = RosBoundCommandPayload::decode(payload_bytes)
            .map_err(RosBoundCommandRefusal::Undecodable)?;

        // 4. The consumer must be operating under the same platform/profile.
        if payload.profile_digest != self.expected_profile_digest {
            return Err(RosBoundCommandRefusal::ProfileDigestMismatch);
        }

        // 5. Explicit issue/expiration checks.
        if payload.issued_at_ms > now_ms {
            return Err(RosBoundCommandRefusal::FutureIssued {
                issued_at_ms: payload.issued_at_ms,
                now_ms,
            });
        }

        // #1230 Part B: refuse anything minted before this process booted —
        // a restart is a new trust epoch, and expiry alone made the replay
        // window exactly as wide as the configurable token lifetime.
        if payload.issued_at_ms < self.boot_wall_ms {
            return Err(RosBoundCommandRefusal::TokenPredatesBoot {
                issued_at_ms: payload.issued_at_ms,
                boot_wall_ms: self.boot_wall_ms,
            });
        }

        if now_ms >= payload.expires_at_ms {
            return Err(RosBoundCommandRefusal::Expired {
                expires_at_ms: payload.expires_at_ms,
                now_ms,
            });
        }

        let lifetime_ms = payload.expires_at_ms - payload.issued_at_ms;
        if lifetime_ms > self.maximum_lifetime_ms {
            return Err(RosBoundCommandRefusal::LifetimeTooLong {
                lifetime_ms,
                maximum_ms: self.maximum_lifetime_ms,
            });
        }

        // 6. Command replay/reordering protection.
        if let Some(last) = self.last_released_sequence {
            if payload.sequence <= last {
                return Err(RosBoundCommandRefusal::SequenceNotAdvanced {
                    presented: payload.sequence,
                    last_released: last,
                });
            }
        }

        // 7. Single-use nonce protection with a constant-memory monotonic rule.
        if let Some(last) = self.last_released_nonce {
            if payload.nonce <= last {
                return Err(RosBoundCommandRefusal::NonceNotAdvanced {
                    presented: payload.nonce,
                    last_released: last,
                });
            }
        }

        // 8. Reject evidence older than the newest frame already released.
        //
        // Tracker generation is the primary epoch. Within one tracker
        // generation, scan sequence must never move backward. An equal frame is
        // allowed for several control commands derived from one perception
        // frame, provided command sequence and nonce both advance.
        if let Some((latest_generation, latest_scan)) = self.latest_perception_frame {
            let superseded = payload.tracker_generation < latest_generation
                || (payload.tracker_generation == latest_generation
                    && payload.scan_sequence < latest_scan);

            if superseded {
                return Err(RosBoundCommandRefusal::PerceptionFrameSuperseded {
                    presented_tracker_generation: payload.tracker_generation,
                    presented_scan_sequence: payload.scan_sequence,
                    latest_tracker_generation: latest_generation,
                    latest_scan_sequence: latest_scan,
                });
            }
        }

        // Full pass: advance all watermarks together.
        self.last_released_sequence = Some(payload.sequence);
        self.last_released_nonce = Some(payload.nonce);

        match self.latest_perception_frame {
            Some((generation, scan))
                if payload.tracker_generation == generation && payload.scan_sequence <= scan => {}
            _ => {
                self.latest_perception_frame =
                    Some((payload.tracker_generation, payload.scan_sequence));
            }
        }

        Ok(payload)
    }

    #[must_use]
    pub fn last_released_sequence(&self) -> Option<u64> {
        self.last_released_sequence
    }

    #[must_use]
    pub fn last_released_nonce(&self) -> Option<u64> {
        self.last_released_nonce
    }

    #[must_use]
    pub fn latest_perception_frame(&self) -> Option<(u64, u64)> {
        self.latest_perception_frame
    }
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

    fn gate() -> RosBoundCommandGate {
        RosBoundCommandGate::new(signing_key().verifying_key(), [0x11; 32], 200, 0)
    }

    fn signed(payload: &RosBoundCommandPayload) -> ReleaseToken {
        issue_ros_bound_command_release(payload, &signing_key())
    }

    #[test]
    fn gate_releases_an_honest_evidence_bound_command() {
        let mut gate = gate();
        let payload = payload();
        let token = signed(&payload);

        assert_eq!(
            gate.release(&payload.encode(), Some(&token), 10_050),
            Ok(payload)
        );
        assert_eq!(gate.last_released_sequence(), Some(10));
        assert_eq!(gate.last_released_nonce(), Some(9001));
        assert_eq!(gate.latest_perception_frame(), Some((3, 77)));
    }

    #[test]
    fn gate_rejects_expired_future_and_excessive_lifetime_commands() {
        let mut expired = payload();
        expired.expires_at_ms = 10_050;
        let token = signed(&expired);
        assert_eq!(
            gate().release(&expired.encode(), Some(&token), 10_050),
            Err(RosBoundCommandRefusal::Expired {
                expires_at_ms: 10_050,
                now_ms: 10_050,
            })
        );

        let mut future = payload();
        future.issued_at_ms = 10_100;
        future.expires_at_ms = 10_200;
        let token = signed(&future);
        assert_eq!(
            gate().release(&future.encode(), Some(&token), 10_050),
            Err(RosBoundCommandRefusal::FutureIssued {
                issued_at_ms: 10_100,
                now_ms: 10_050,
            })
        );

        let mut long = payload();
        long.expires_at_ms = 10_201;
        let token = signed(&long);
        assert_eq!(
            gate().release(&long.encode(), Some(&token), 10_050),
            Err(RosBoundCommandRefusal::LifetimeTooLong {
                lifetime_ms: 201,
                maximum_ms: 200,
            })
        );
    }

    #[test]
    fn gate_rejects_sequence_replay_and_duplicate_nonce() {
        let mut gate = gate();
        let first = payload();
        let first_token = signed(&first);
        gate.release(&first.encode(), Some(&first_token), 10_050)
            .unwrap();

        let mut replay = first;
        replay.nonce += 1;
        let replay_token = signed(&replay);
        assert_eq!(
            gate.release(&replay.encode(), Some(&replay_token), 10_050),
            Err(RosBoundCommandRefusal::SequenceNotAdvanced {
                presented: 10,
                last_released: 10,
            })
        );

        let mut duplicate_nonce = first;
        duplicate_nonce.sequence += 1;
        let duplicate_token = signed(&duplicate_nonce);
        assert_eq!(
            gate.release(&duplicate_nonce.encode(), Some(&duplicate_token), 10_050),
            Err(RosBoundCommandRefusal::NonceNotAdvanced {
                presented: 9001,
                last_released: 9001,
            })
        );

        // Refusals do not poison any watermark.
        assert_eq!(gate.last_released_sequence(), Some(10));
        assert_eq!(gate.last_released_nonce(), Some(9001));
    }

    /// #1214: the canonical replay — the EXACT same bytes and the EXACT same
    /// token, presented again.
    ///
    /// This is the only replay an attacker who cannot forge the governor's
    /// signature is able to mount: capture a frame that was legitimately
    /// released and re-present it verbatim. Every other replay test in this
    /// module mutates a field, which requires re-signing and therefore assumes a
    /// capability the attacker does not have.
    ///
    /// It also distinguishes a BURNED nonce from a merely CHECKED one. A check
    /// against a validity rule would pass a second time, because the bytes are
    /// by construction as valid as they were the first time. Only consuming the
    /// nonce refuses them — here via the strictly-advancing watermark, which
    /// gives single-use semantics in constant memory rather than an unbounded
    /// set of every nonce ever seen.
    #[test]
    fn a_verbatim_replay_of_a_released_frame_is_refused() {
        let mut gate = gate();
        let payload = payload();
        let token = signed(&payload);
        let bytes = payload.encode();

        gate.release(&bytes, Some(&token), 10_050)
            .expect("the first presentation is honest and releases");

        // Same bytes, same signature, still inside the validity window: nothing
        // about the frame itself has become invalid.
        let replayed = gate.release(&bytes, Some(&token), 10_060);
        assert_eq!(
            replayed,
            Err(RosBoundCommandRefusal::SequenceNotAdvanced {
                presented: 10,
                last_released: 10,
            }),
            "a captured frame must not be releasable twice"
        );

        // Still inside the window — so the refusal above is replay protection,
        // not expiry doing the work. Without this the test would pass for the
        // wrong reason as soon as the fixture's timings drifted.
        assert!(10_060 < payload.expires_at_ms);

        assert_eq!(gate.last_released_nonce(), Some(9001));
        assert_eq!(gate.last_released_sequence(), Some(10));
    }

    /// A refused frame does not consume the identity it presented.
    ///
    /// The complement of the burn: if a refusal advanced the watermark, anyone
    /// able to inject one malformed frame could burn a range of sequence and
    /// nonce values and lock out the legitimate governor — turning replay
    /// protection into a denial-of-service primitive.
    #[test]
    fn a_refused_frame_does_not_burn_the_identity_it_presented() {
        let mut gate = gate();
        let honest = payload();

        // Refused for a bad signature — signed with the wrong key.
        let impostor = SigningKey::from_bytes(&[7u8; 32]);
        let forged = issue_ros_bound_command_release(&honest, &impostor);
        assert!(gate
            .release(&honest.encode(), Some(&forged), 10_050)
            .is_err());

        // The same sequence and nonce, honestly signed, must still work.
        let token = signed(&honest);
        assert_eq!(
            gate.release(&honest.encode(), Some(&token), 10_050),
            Ok(honest),
            "a rejected impostor must not be able to burn the governor's next nonce"
        );
    }

    #[test]
    fn gate_rejects_profile_mismatch_without_advancing() {
        let mut gate = gate();
        let mut payload = payload();
        payload.profile_digest = [0x44; 32];
        let token = signed(&payload);

        assert_eq!(
            gate.release(&payload.encode(), Some(&token), 10_050),
            Err(RosBoundCommandRefusal::ProfileDigestMismatch)
        );
        assert_eq!(gate.last_released_sequence(), None);
        assert_eq!(gate.last_released_nonce(), None);
        assert_eq!(gate.latest_perception_frame(), None);
    }

    #[test]
    fn gate_rejects_superseded_perception_but_allows_same_frame_commands() {
        let mut gate = gate();

        let first = payload();
        let token = signed(&first);
        gate.release(&first.encode(), Some(&token), 10_050).unwrap();

        // Another command from the exact same evidence frame is valid when its
        // command sequence and nonce advance.
        let mut same_frame = first;
        same_frame.sequence += 1;
        same_frame.nonce += 1;
        let token = signed(&same_frame);
        gate.release(&same_frame.encode(), Some(&token), 10_060)
            .unwrap();

        // Move the released perception watermark forward.
        let mut newer = same_frame;
        newer.sequence += 1;
        newer.nonce += 1;
        newer.scan_sequence += 1;
        let token = signed(&newer);
        gate.release(&newer.encode(), Some(&token), 10_070).unwrap();

        // A later command sequence cannot resurrect the older evidence frame.
        let mut superseded = newer;
        superseded.sequence += 1;
        superseded.nonce += 1;
        superseded.scan_sequence -= 1;
        let token = signed(&superseded);

        assert_eq!(
            gate.release(&superseded.encode(), Some(&token), 10_080),
            Err(RosBoundCommandRefusal::PerceptionFrameSuperseded {
                presented_tracker_generation: 3,
                presented_scan_sequence: 77,
                latest_tracker_generation: 3,
                latest_scan_sequence: 78,
            })
        );

        assert_eq!(gate.last_released_sequence(), Some(12));
        assert_eq!(gate.last_released_nonce(), Some(9003));
        assert_eq!(gate.latest_perception_frame(), Some((3, 78)));
    }

    #[test]
    fn gate_rejects_missing_or_tampered_tokens() {
        let payload = payload();

        assert_eq!(
            gate().release(&payload.encode(), None, 10_050),
            Err(RosBoundCommandRefusal::NoToken)
        );

        let mut changed = payload;
        let token = signed(&payload);
        changed.proposal_digest[0] ^= 1;

        assert_eq!(
            gate().release(&changed.encode(), Some(&token), 10_050),
            Err(RosBoundCommandRefusal::Denied(
                ReleaseDenied::DigestMismatch
            ))
        );
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
