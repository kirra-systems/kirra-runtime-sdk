//! ADR-0033 — the ROS-path release token: verify-before-release at the motor
//! boundary of the `ros2_ws` deployment topology.
//!
//! The SHM/inline path binds a release token to the frozen 176-byte
//! [`GovernorContractView`](kirra_contract_channel::GovernorContractView) under
//! the `KIRRA-GOVERNOR-*-V1` domains. This module is the ROS deployment's
//! sibling: its OWN payload type and its OWN domain pair
//! (`KIRRA-ROS-TWIST-DIGEST-V1` / `KIRRA-ROS-RELEASE-V1`), so a signature under
//! one path can never verify under the other — **no cross-path replay** (ADR-0033
//! settled decision 2). Nothing here touches `GovernorContractView` or its
//! freeze assertions; the token wire shape stays the canonical 96 bytes
//! (`digest(32) || signature(64)`, [`ReleaseToken`]) because freshness rides
//! INSIDE the signed payload, not on the token.
//!
//! ## The payload is the wire artifact
//!
//! [`RosTwistPayload`] encodes to a **fixed-width 32-byte little-endian image**
//! ([`RosTwistPayload::encode`]) of `{sequence, issued_at_ms, linear_mps,
//! angular_rad_s}` — the ADR's canonical encoding. The governor signs those
//! exact bytes; every consumer decodes its floats FROM those bytes
//! ([`RosTwistPayload::decode`]) rather than re-deriving them from JSON or ROS
//! messages, so there is no cross-language float-canonicalization step anywhere
//! on the trust path: the bytes that were signed are the bytes that actuate.
//!
//! ## [`RosReleaseGate`] — the ONE verify-before-release implementation
//!
//! ADR-0033 settled decision 1 requires the token verification, the sequence
//! watermark, and the refusal taxonomy to live in **one Rust implementation**
//! shared by every consumer shape (the future full-Rust consumer, the C-ABI
//! surface for the Python node, and the Tier-1 regression guard). That
//! implementation is [`RosReleaseGate::release`], mirroring
//! `ActuatorStation::release` (`kirra-inline-governor`) step for step and
//! adding the ADR's freshness rule:
//!
//! 1. a token must exist (no proof → no release);
//! 2. the token must verify over EXACTLY the presented payload bytes
//!    (digest match + strict Ed25519, under the ROS domains);
//! 3. the payload must decode (non-finite floats refuse — never actuate NaN);
//! 4. the token must be FRESH (`issued_at_ms` within the window of `now_ms`,
//!    both directions — a stale token is refused even mid-run);
//! 5. the sequence must strictly advance past the last RELEASED sequence
//!    (`<=` is a replay), with resync-from-zero: an empty watermark adopts the
//!    first FRESH token's sequence as the baseline (ADR-0033 settled
//!    decision 3 — no durable watermark).
//!
//! A refusal at any step leaves the watermark untouched (refusals are not
//! releases and must never block a later legitimate release), and — consumed
//! one level up — refusals are not LIVENESS either (a flood of invalid tokens
//! must starve into the safe stop exactly as silence does).
//!
//! **The freshness window is load-bearing across a consumer restart.** The
//! watermark is deliberately in-memory (decision 3: a durable watermark puts a
//! write on the actuation hot path and a corruptible file on an SD card), so
//! after a restart the ONLY thing standing between a captured pre-restart token
//! and the motors is step 4. Do not widen the window casually: the replay
//! exposure after a restart is exactly one stale-but-fresh, envelope-bounded,
//! governor-signed command.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::{ReleaseDenied, ReleaseToken};

/// Domain tag for the ROS twist digest. MUST differ from every other digest
/// domain in the system (asserted in tests against the SHM pair).
pub const ROS_TWIST_DIGEST_DOMAIN: &[u8] = b"KIRRA-ROS-TWIST-DIGEST-V1";

/// Domain tag for the ROS release signing payload. MUST differ from every
/// other release domain (asserted in tests against the SHM pair).
pub const ROS_TWIST_RELEASE_DOMAIN: &[u8] = b"KIRRA-ROS-RELEASE-V1";

/// Fixed width of the encoded payload: 4 × 8 little-endian bytes.
pub const ROS_TWIST_PAYLOAD_LEN: usize = 32;

/// The signed command image on the ROS path (ADR-0033 settled decision 2):
/// the ENFORCED twist the checker approved, a strictly-advancing sequence,
/// and the issue timestamp — freshness INSIDE the signature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RosTwistPayload {
    /// Strictly advancing per governor boot (the consumer's watermark rule).
    pub sequence: u64,
    /// Milliseconds since UNIX epoch at mint time. Inside the signed bytes so
    /// the freshness rule cannot be stripped or forged.
    pub issued_at_ms: u64,
    /// ENFORCED linear velocity (m/s) — post-envelope, checker-approved.
    pub linear_mps: f64,
    /// ENFORCED angular rate (rad/s) — post-envelope, checker-approved.
    pub angular_rad_s: f64,
}

/// Why a payload image failed to decode. Fail-closed: never actuate bytes
/// that don't parse to a finite command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosTwistDecodeError {
    /// A float field decoded to NaN or ±Inf — not a command.
    NonFiniteValue,
}

impl RosTwistPayload {
    /// The canonical fixed-width little-endian image: `sequence ||
    /// issued_at_ms || linear_mps || angular_rad_s`. These are the bytes that
    /// are digested, signed, carried, verified, and decoded — the single wire
    /// truth for the command.
    #[must_use]
    pub fn encode(&self) -> [u8; ROS_TWIST_PAYLOAD_LEN] {
        let mut out = [0u8; ROS_TWIST_PAYLOAD_LEN];
        out[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..16].copy_from_slice(&self.issued_at_ms.to_le_bytes());
        out[16..24].copy_from_slice(&self.linear_mps.to_le_bytes());
        out[24..32].copy_from_slice(&self.angular_rad_s.to_le_bytes());
        out
    }

    /// Decode the canonical image. Fail-closed on non-finite floats: a NaN or
    /// ±Inf twist is refused here, before any watermark or serial write.
    pub fn decode(bytes: &[u8; ROS_TWIST_PAYLOAD_LEN]) -> Result<Self, RosTwistDecodeError> {
        let sequence = u64::from_le_bytes(bytes[0..8].try_into().expect("fixed slice"));
        let issued_at_ms = u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice"));
        let linear_mps = f64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice"));
        let angular_rad_s = f64::from_le_bytes(bytes[24..32].try_into().expect("fixed slice"));
        if !(linear_mps.is_finite() && angular_rad_s.is_finite()) {
            return Err(RosTwistDecodeError::NonFiniteValue);
        }
        Ok(Self {
            sequence,
            issued_at_ms,
            linear_mps,
            angular_rad_s,
        })
    }
}

/// SHA-256 over the domain-separated, length-prefixed payload image (the
/// audit-chain house style, same discipline as `contract_digest` — but under
/// the ROS digest domain, so the two hash spaces never collide).
#[must_use]
pub fn ros_twist_digest(payload_bytes: &[u8; ROS_TWIST_PAYLOAD_LEN]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ROS_TWIST_DIGEST_DOMAIN);
    hasher.update((payload_bytes.len() as u64).to_le_bytes());
    hasher.update(payload_bytes);
    hasher.finalize().into()
}

/// The ROS release signing payload: domain tag, then the 32-byte digest
/// length-prefixed. Same construction as the SHM path, different domain — a
/// ROS release signature can never verify as an SHM release or vice versa.
fn ros_release_signing_payload(digest: &[u8; 32]) -> [u8; ROS_TWIST_RELEASE_DOMAIN.len() + 8 + 32] {
    let mut out = [0u8; ROS_TWIST_RELEASE_DOMAIN.len() + 8 + 32];
    let mut n = 0;
    out[n..n + ROS_TWIST_RELEASE_DOMAIN.len()].copy_from_slice(ROS_TWIST_RELEASE_DOMAIN);
    n += ROS_TWIST_RELEASE_DOMAIN.len();
    out[n..n + 8].copy_from_slice(&(32u64).to_le_bytes());
    n += 8;
    out[n..n + 32].copy_from_slice(digest);
    out
}

/// Issue a ROS release token over the payload's canonical image. The token is
/// the SAME 96-byte [`ReleaseToken`] wire shape as the SHM path (`digest(32) ||
/// signature(64)`) — `issued_at_ms` rides inside the digested image, so the
/// wire shape needed no change (ADR-0033 settled decision 2).
#[must_use]
pub fn issue_ros_release(payload: &RosTwistPayload, signing_key: &SigningKey) -> ReleaseToken {
    let digest = ros_twist_digest(&payload.encode());
    let signature = signing_key.sign(&ros_release_signing_payload(&digest));
    ReleaseToken {
        digest,
        signature: signature.to_bytes(),
    }
}

/// The crypto half of the gate, stateless: does `token` approve exactly
/// `payload_bytes` under the ROS domains? Two fail-closed checks, mirroring
/// `verify_release_over_digest`: (a) digest match over the exact presented
/// bytes, (b) strict Ed25519 over the ROS-domain payload.
pub fn verify_ros_release(
    token: &ReleaseToken,
    payload_bytes: &[u8; ROS_TWIST_PAYLOAD_LEN],
    governor_vk: &VerifyingKey,
) -> Result<(), ReleaseDenied> {
    let expected = ros_twist_digest(payload_bytes);
    if token.digest != expected {
        return Err(ReleaseDenied::DigestMismatch);
    }
    let sig = Signature::from_bytes(&token.signature);
    if governor_vk
        .verify_strict(&ros_release_signing_payload(&token.digest), &sig)
        .is_err()
    {
        return Err(ReleaseDenied::SignatureInvalid);
    }
    Ok(())
}

/// Why the motor-side gate refused to release. Every variant is fail-closed:
/// the consumer holds its safe state and the release watermark is untouched.
/// Mirrors `ReleaseRefusal` (`kirra-inline-governor`) with the ADR-0033
/// freshness rule added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosReleaseRefusal {
    /// No token was presented (a deny verdict upstream, or a rogue publisher
    /// that has no key). Nothing to prove → nothing to release.
    NoToken,
    /// The token failed verification over the presented bytes (substitution /
    /// forgery / tamper / cross-path replay).
    Denied(ReleaseDenied),
    /// The (verified) payload failed to decode — refuse bytes that don't
    /// parse to a finite command.
    Undecodable(RosTwistDecodeError),
    /// The token is outside the freshness window (either too old, or
    /// implausibly future-dated — both refuse; a stale token is refused even
    /// mid-run, and after a consumer restart this is the ONLY replay barrier).
    Stale {
        issued_at_ms: u64,
        now_ms: u64,
        window_ms: u64,
    },
    /// The presented sequence does not STRICTLY advance past the last
    /// released one — a replayed or reordered release (the kernel `<=` rule,
    /// re-enforced at the final authority boundary).
    SequenceNotAdvanced { presented: u64, last_released: u64 },
}

/// The motor-side verify-before-release gate: trusted governor verifying key,
/// freshness window, and the in-memory released-sequence watermark.
///
/// NOT thread-safe by design (the consumer owns exactly one gate on its
/// single actuation path); wrap externally if you must share it.
pub struct RosReleaseGate {
    governor_vk: VerifyingKey,
    freshness_window_ms: u64,
    last_released: Option<u64>,
}

impl RosReleaseGate {
    /// `freshness_window_ms` is the ADR-0033 decision-3 window (proposed ≈ 2
    /// control periods, ≤ 200 ms at 10 Hz — tuned per deployment and recorded
    /// in the config registry). It bounds BOTH staleness and future-dating.
    #[must_use]
    pub fn new(governor_vk: VerifyingKey, freshness_window_ms: u64) -> Self {
        Self {
            governor_vk,
            freshness_window_ms,
            last_released: None,
        }
    }

    /// The verify-before-release gate — see the module docs for the five
    /// ordered, fail-closed steps. Only a full pass advances the watermark.
    pub fn release(
        &mut self,
        payload_bytes: &[u8; ROS_TWIST_PAYLOAD_LEN],
        token: Option<&ReleaseToken>,
        now_ms: u64,
    ) -> Result<RosTwistPayload, RosReleaseRefusal> {
        // 1. No proof → no release.
        let token = token.ok_or(RosReleaseRefusal::NoToken)?;
        // 2. The token must approve EXACTLY these bytes, under the ROS domains.
        verify_ros_release(token, payload_bytes, &self.governor_vk)
            .map_err(RosReleaseRefusal::Denied)?;
        // 3. The bytes must parse to a finite command.
        let payload =
            RosTwistPayload::decode(payload_bytes).map_err(RosReleaseRefusal::Undecodable)?;
        // 4. Freshness — two-sided: |now - issued_at| must be within the
        //    window. Load-bearing across a consumer restart (module docs).
        let age = now_ms.abs_diff(payload.issued_at_ms);
        if age > self.freshness_window_ms {
            return Err(RosReleaseRefusal::Stale {
                issued_at_ms: payload.issued_at_ms,
                now_ms,
                window_ms: self.freshness_window_ms,
            });
        }
        // 5. Strictly advancing sequence; empty watermark = resync-from-zero
        //    (the first FRESH token's sequence becomes the baseline).
        if let Some(last) = self.last_released {
            if payload.sequence <= last {
                return Err(RosReleaseRefusal::SequenceNotAdvanced {
                    presented: payload.sequence,
                    last_released: last,
                });
            }
        }
        self.last_released = Some(payload.sequence);
        Ok(payload)
    }

    /// The last released sequence (observability/tests).
    #[must_use]
    pub fn last_released(&self) -> Option<u64> {
        self.last_released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{issue_release_token, verify_release_over_digest};
    use kirra_contract_channel::GovernorContractView;

    fn governor_key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    fn payload(sequence: u64, issued_at_ms: u64) -> RosTwistPayload {
        RosTwistPayload {
            sequence,
            issued_at_ms,
            linear_mps: 0.75,
            angular_rad_s: -0.25,
        }
    }

    const WINDOW: u64 = 200;

    #[test]
    fn honest_token_releases_and_advances_watermark() {
        let sk = governor_key();
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let p = payload(1, 10_000);
        let token = issue_ros_release(&p, &sk);
        let released = gate.release(&p.encode(), Some(&token), 10_050).unwrap();
        assert_eq!(released, p);
        assert_eq!(gate.last_released(), Some(1));
    }

    #[test]
    fn payload_round_trips_bit_exactly() {
        let p = payload(7, 123);
        assert_eq!(RosTwistPayload::decode(&p.encode()).unwrap(), p);
        // Negative zero and denormals survive (bit-pattern carriage).
        let odd = RosTwistPayload {
            sequence: 1,
            issued_at_ms: 2,
            linear_mps: -0.0,
            angular_rad_s: f64::MIN_POSITIVE / 2.0,
        };
        let back = RosTwistPayload::decode(&odd.encode()).unwrap();
        assert_eq!(back.linear_mps.to_bits(), (-0.0f64).to_bits());
        assert_eq!(back.angular_rad_s.to_bits(), odd.angular_rad_s.to_bits());
    }

    #[test]
    fn non_finite_payload_is_undecodable() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let p = RosTwistPayload {
                sequence: 1,
                issued_at_ms: 10_000,
                linear_mps: bad,
                angular_rad_s: 0.0,
            };
            assert_eq!(
                RosTwistPayload::decode(&p.encode()),
                Err(RosTwistDecodeError::NonFiniteValue)
            );
            // And through the gate: a governor bug signing a NaN still refuses.
            let sk = governor_key();
            let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
            let token = issue_ros_release(&p, &sk);
            assert_eq!(
                gate.release(&p.encode(), Some(&token), 10_000),
                Err(RosReleaseRefusal::Undecodable(
                    RosTwistDecodeError::NonFiniteValue
                ))
            );
            assert_eq!(gate.last_released(), None, "refusal must not advance");
        }
    }

    #[test]
    fn no_token_is_refused() {
        let sk = governor_key();
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let p = payload(1, 10_000);
        assert_eq!(
            gate.release(&p.encode(), None, 10_000),
            Err(RosReleaseRefusal::NoToken)
        );
        assert_eq!(gate.last_released(), None);
    }

    #[test]
    fn token_over_different_bytes_is_refused() {
        let sk = governor_key();
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let approved = payload(1, 10_000);
        let token = issue_ros_release(&approved, &sk);
        let mut substituted = approved;
        substituted.linear_mps = 9.9;
        assert_eq!(
            gate.release(&substituted.encode(), Some(&token), 10_000),
            Err(RosReleaseRefusal::Denied(ReleaseDenied::DigestMismatch))
        );
        assert_eq!(gate.last_released(), None);
    }

    #[test]
    fn unsigned_or_wrong_key_is_refused() {
        let sk = governor_key();
        let imposter = SigningKey::from_bytes(&[7u8; 32]);
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let p = payload(1, 10_000);
        let forged = issue_ros_release(&p, &imposter);
        assert_eq!(
            gate.release(&p.encode(), Some(&forged), 10_000),
            Err(RosReleaseRefusal::Denied(ReleaseDenied::SignatureInvalid))
        );
        let mut tampered = issue_ros_release(&p, &sk);
        tampered.signature[0] ^= 1;
        assert_eq!(
            gate.release(&p.encode(), Some(&tampered), 10_000),
            Err(RosReleaseRefusal::Denied(ReleaseDenied::SignatureInvalid))
        );
    }

    #[test]
    fn replayed_token_is_refused_and_does_not_poison_the_watermark() {
        let sk = governor_key();
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let p1 = payload(5, 10_000);
        let t1 = issue_ros_release(&p1, &sk);
        gate.release(&p1.encode(), Some(&t1), 10_000).unwrap();

        // Exact replay → sequence rule refuses (equal = replay).
        assert_eq!(
            gate.release(&p1.encode(), Some(&t1), 10_010),
            Err(RosReleaseRefusal::SequenceNotAdvanced {
                presented: 5,
                last_released: 5
            })
        );
        // Reordered (older sequence, fresh signature) → refused.
        let p_old = payload(4, 10_020);
        let t_old = issue_ros_release(&p_old, &sk);
        assert_eq!(
            gate.release(&p_old.encode(), Some(&t_old), 10_020),
            Err(RosReleaseRefusal::SequenceNotAdvanced {
                presented: 4,
                last_released: 5
            })
        );
        // A legitimate NEXT release still passes: refusals never poisoned it.
        let p2 = payload(6, 10_030);
        let t2 = issue_ros_release(&p2, &sk);
        assert!(gate.release(&p2.encode(), Some(&t2), 10_030).is_ok());
    }

    #[test]
    fn stale_and_future_dated_tokens_are_refused() {
        let sk = governor_key();
        let mut gate = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let p = payload(1, 10_000);
        let token = issue_ros_release(&p, &sk);
        // Too old.
        assert!(matches!(
            gate.release(&p.encode(), Some(&token), 10_000 + WINDOW + 1),
            Err(RosReleaseRefusal::Stale { .. })
        ));
        // Implausibly future-dated.
        assert!(matches!(
            gate.release(&p.encode(), Some(&token), 10_000 - WINDOW - 1),
            Err(RosReleaseRefusal::Stale { .. })
        ));
        // Boundary: exactly at the window edge is still fresh.
        assert!(gate
            .release(&p.encode(), Some(&token), 10_000 + WINDOW)
            .is_ok());
    }

    /// ADR-0033 settled decision 3 — restart semantics. The watermark is
    /// in-memory: a "restarted" gate (fresh instance) must (a) accept a fresh
    /// token with an arbitrary sequence and adopt it as the baseline, and
    /// (b) REFUSE a captured pre-restart token that is outside the freshness
    /// window — the window is the only replay barrier here, which is exactly
    /// why it is load-bearing.
    #[test]
    fn restart_resyncs_from_zero_and_freshness_blocks_pre_restart_replay() {
        let sk = governor_key();
        let captured = payload(500, 10_000);
        let captured_token = issue_ros_release(&captured, &sk);

        // ... consumer restarts much later ...
        let mut restarted = RosReleaseGate::new(sk.verifying_key(), WINDOW);
        let now = 60_000;
        assert!(matches!(
            restarted.release(&captured.encode(), Some(&captured_token), now),
            Err(RosReleaseRefusal::Stale { .. })
        ));
        assert_eq!(restarted.last_released(), None);

        // A fresh token resyncs the baseline even though its sequence (3) is
        // lower than anything pre-restart — the empty watermark adopts it.
        let fresh = payload(3, now);
        let fresh_token = issue_ros_release(&fresh, &sk);
        assert!(restarted
            .release(&fresh.encode(), Some(&fresh_token), now)
            .is_ok());
        assert_eq!(restarted.last_released(), Some(3));
    }

    /// ADR-0033 settled decision 2 — cross-path replay is structurally
    /// impossible: the ROS domains differ from the SHM domains, a ROS release
    /// signature does not verify as an SHM release, and an SHM release
    /// signature does not verify as a ROS release.
    #[test]
    fn ros_and_shm_paths_are_domain_separated() {
        assert_ne!(ROS_TWIST_DIGEST_DOMAIN, crate::DIGEST_DOMAIN);
        assert_ne!(ROS_TWIST_RELEASE_DOMAIN, crate::RELEASE_DOMAIN);

        let sk = governor_key();
        let vk = sk.verifying_key();

        // A captured SHM release token, replayed against the ROS gate: even
        // with the digest field REWRITTEN to match a ROS payload (digest
        // check passes), the signature was made under the SHM domain and
        // cannot verify under the ROS domain.
        let view = GovernorContractView::new_command(2, 1, 100, 10_000, b"go").unwrap();
        let mut shm_token = issue_release_token(&view, &sk);
        let ros_payload = payload(1, 10_000);
        shm_token.digest = ros_twist_digest(&ros_payload.encode());
        assert_eq!(
            verify_ros_release(&shm_token, &ros_payload.encode(), &vk),
            Err(ReleaseDenied::SignatureInvalid)
        );

        // And the mirror image: a ROS release token cannot verify as an SHM
        // release over its own digest.
        let ros_token = issue_ros_release(&ros_payload, &sk);
        assert_eq!(
            verify_release_over_digest(&ros_token, &ros_token.digest, &vk),
            Err(ReleaseDenied::SignatureInvalid)
        );
    }

    /// ADR-0033: the 96-byte token wire shape survives `issued_at_ms` because
    /// the stamp rides inside the digested payload, not on the token.
    #[test]
    fn token_wire_shape_is_unchanged_at_96_bytes() {
        let sk = governor_key();
        let token = issue_ros_release(&payload(1, 2), &sk);
        assert_eq!(token.to_bytes().len(), 96);
        assert_eq!(ReleaseToken::from_bytes(&token.to_bytes()), token);
    }
}
