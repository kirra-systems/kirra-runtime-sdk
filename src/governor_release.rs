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
//!   — the exact validated snapshot bytes, not the live region.
//! - **Step 6 — release token** ([`issue_release_token`]): Ed25519 over the
//!   domain-separated digest payload. No new crypto primitive.
//! - **Step 7 — verify-before-release** ([`verify_release`]): re-derive the digest
//!   over the command about to be actuated; a digest mismatch OR bad signature ⇒
//!   fail-closed [`ReleaseDenied`], no release.

// The single canonical release-token implementation. Re-exported so existing
// callers (`kirra_verifier::governor_release::*`) are unchanged after the
// consolidation.
pub use kirra_release_token::{
    contract_digest, issue_release_token, verify_release, ReleaseDenied, ReleaseToken,
};

use ed25519_dalek::VerifyingKey;

/// Forensic key id of the governor signing key — the `audit_chain` key-id
/// discipline (hex SHA-256 of the public key), for logging which key signed a
/// release without storing it in the wire token. Root-local (the lean release
/// crate does not depend on `audit_chain`).
#[must_use]
pub fn governor_key_id(vk: &VerifyingKey) -> String {
    crate::audit_chain::verifying_key_id(vk)
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
}
