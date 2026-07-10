//! Governor release token — the cryptographic binding from an approved contract
//! snapshot to an actuator release (HVCHAN-001 §3 steps 5-7).
//!
//! **The binding logic now lives in the lean [`kirra_release_token`] crate** —
//! ONE canonical release-token format (domain-separated, length-prefixed digest +
//! signature), shared by both the heavy `kirra-verifier` root crate (here) and
//! the lean L3 integration seam (`kirra-core` + `kirra-release-token`). This
//! module re-exports that single implementation and adds only the root-local
//! [`governor_key_id`] forensic helper, which needs the `audit_chain` key-id
//! discipline (a root-crate dependency the lean crate deliberately does not pull).
//!
//! - **Step 5 — digest** ([`contract_digest`]): SHA-256 over the domain-separated,
//!   length-prefixed [`kirra_contract_channel::GovernorContractView::canonical_image`]
//!   of the **signable** view, not the live region. (Transport `validate` CRCs only
//!   `command[..command_len]`; `canonical_image` covers the full command array, so
//!   sign a canonicalized/enforced view — see the `kirra-release-token` crate docs.)
//! - **Step 6 — release token** ([`issue_release_token`]): Ed25519 over the
//!   domain-separated digest payload. No new crypto primitive.
//! - **Step 7 — verify-before-release** ([`verify_release`]): re-derive the digest
//!   over the command about to be actuated; a digest mismatch OR bad signature ⇒
//!   fail-closed [`ReleaseDenied`], no release.

// The single canonical release-token implementation. Re-exported so existing
// callers (`kirra_verifier::governor_release::*`) are unchanged after the
// consolidation.
pub use kirra_release_token::{
    contract_digest, issue_release_token, verify_release, verify_release_over_digest,
    ReleaseDenied, ReleaseToken,
};

pub use kirra_release_token::ros_twist::{
    issue_ros_release, verify_ros_release, RosReleaseGate, RosReleaseRefusal, RosTwistPayload,
    ROS_TWIST_PAYLOAD_LEN,
};

use ed25519_dalek::{SigningKey, VerifyingKey};
use std::sync::atomic::{AtomicU64, Ordering};

/// Forensic key id of the governor signing key — the `audit_chain` key-id
/// discipline (hex SHA-256 of the public key), for logging which key signed a
/// release without storing it in the wire token. Root-local (the lean release
/// crate does not depend on `audit_chain`).
#[must_use]
pub fn governor_key_id(vk: &VerifyingKey) -> String {
    crate::audit_chain::verifying_key_id(vk)
}

/// ADR-0033 — the verifier-side minting state for the ROS-path release token:
/// the provisioned governor signing key plus the strictly-advancing sequence
/// counter. One instance per Active verifier process, created at startup from
/// `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` (fail-closed provisioning, ADR-0031
/// Clause E) and threaded to the actuator handler as a router `Extension`.
///
/// ## Sequence continuity across verifier restarts
///
/// The counter is seeded from the boot wall clock (ms since epoch) and
/// incremented by one per mint. ADR-0033's settled decision 3 covers CONSUMER
/// restarts (resync-from-zero + freshness); it does not settle VERIFIER
/// restarts, where a from-zero counter would mint sequences BELOW a live
/// consumer's watermark and deadlock the path until the consumer restarts.
/// Seeding from the clock keeps sequences monotonic across restarts whenever
/// the wall clock is sane and the long-run mint rate stays under one command
/// per millisecond (the R2 control rate is 10–20 Hz — 50× headroom). A
/// backwards clock step at reboot degrades FAIL-CLOSED: the consumer refuses
/// (`SequenceNotAdvanced`) until its own restart resyncs the baseline; motion
/// stops, nothing unsafe releases.
pub struct RosReleaseSigner {
    signing_key: SigningKey,
    sequence: AtomicU64,
}

impl RosReleaseSigner {
    #[must_use]
    pub fn new(signing_key: SigningKey, sequence_seed: u64) -> Self {
        Self {
            signing_key,
            sequence: AtomicU64::new(sequence_seed),
        }
    }

    /// Mint a release token over the ENFORCED twist. Called ONLY from the
    /// actuator handler's 200 arm, after the epoch fence — the deny paths
    /// (400/403/503) structurally cannot reach this. (The invariant "the deny
    /// path never mints a token" is pinned by the bin-internal
    /// `src/bin/kirra_verifier_service/ros_release_mint_tests.rs`.)
    pub fn mint(
        &self,
        linear_mps: f64,
        angular_rad_s: f64,
        issued_at_ms: u64,
    ) -> (RosTwistPayload, ReleaseToken) {
        // Relaxed: the counter needs uniqueness/monotonicity only (atomic RMW
        // guarantees both regardless of ordering); no other memory synchronizes
        // on it, and this sits on the actuation response path.
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = RosTwistPayload {
            sequence,
            issued_at_ms,
            linear_mps,
            angular_rad_s,
        };
        let token = issue_ros_release(&payload, &self.signing_key);
        (payload, token)
    }

    /// The verifying key a consumer pins (distributed out-of-band per
    /// `docs/safety/GOVERNOR_KEY_PROVISIONING.md`).
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Forensic key id (hex SHA-256 of the verifying key).
    #[must_use]
    pub fn key_id(&self) -> String {
        governor_key_id(&self.verifying_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn key_id_is_stable_hex_sha256() {
        let sk = SigningKey::from_bytes(&[42u8; 32]);
        let id = governor_key_id(&sk.verifying_key());
        assert_eq!(id.len(), 64); // hex SHA-256
        assert_eq!(id, governor_key_id(&sk.verifying_key()));
    }

    /// The lean crate's `verifying_key_id_hex` (used by consumer enrollment
    /// diagnostics) must stay byte-identical to the root `audit_chain`
    /// key-id discipline `governor_key_id` wraps — one key names ONE id
    /// everywhere it appears in logs.
    #[test]
    fn lean_key_id_matches_the_audit_chain_discipline() {
        let vk = SigningKey::from_bytes(&[42u8; 32]).verifying_key();
        assert_eq!(
            governor_key_id(&vk),
            kirra_release_token::ros_twist::verifying_key_id_hex(&vk)
        );
    }

    #[test]
    fn signer_mints_strictly_advancing_gate_accepted_tokens() {
        let sk = SigningKey::from_bytes(&[42u8; 32]);
        let signer = RosReleaseSigner::new(SigningKey::from_bytes(&sk.to_bytes()), 1_000);
        let mut gate = RosReleaseGate::new(signer.verifying_key(), 200);

        let (p1, t1) = signer.mint(0.5, 0.1, 10_000);
        let (p2, t2) = signer.mint(0.6, 0.0, 10_050);
        assert_eq!(p1.sequence, 1_001);
        assert_eq!(p2.sequence, 1_002, "sequence strictly advances per mint");

        assert!(gate.release(&p1.encode(), Some(&t1), 10_000).is_ok());
        assert!(gate.release(&p2.encode(), Some(&t2), 10_050).is_ok());
        // Replay of p1 after p2 → refused by the watermark.
        assert!(matches!(
            gate.release(&p1.encode(), Some(&t1), 10_060),
            Err(RosReleaseRefusal::SequenceNotAdvanced { .. })
        ));
    }
}
