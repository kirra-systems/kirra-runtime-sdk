//! Governor release token — the cryptographic binding from an approved contract
//! snapshot to an actuator release (HVCHAN-001 §3 steps 5-7).
//!
//! Steps 1-4 (the frozen layout, the seqlock coherent read, and validation) live
//! in the `kirra-contract-channel` crate. This module closes the chain:
//!
//! - **Step 5 — digest.** [`contract_digest`] hashes the **exact validated
//!   snapshot bytes** ([`GovernorContractView::canonical_image`]) — the bytes the
//!   judge approved, not the live region which may already have moved on.
//! - **Step 6 — release token.** [`issue_release_token`] signs that digest with
//!   the governor's Ed25519 key, **reusing the existing machinery** (`SigningKey`
//!   / `verify_strict`, the `audit_chain` SHA-256 + domain-separation discipline).
//!   **No new crypto primitive is introduced** (HVCHAN-001 §3 step 6).
//! - **Step 7 — actuator verify-before-release.** [`verify_release`] re-derives
//!   the digest over the command the actuator is **about to actuate** and checks
//!   (a) the token's digest matches it **and** (b) the signature verifies against
//!   the governor key. Either failure ⇒ **no release** (fail-closed).
//!
//! Both the digest and the token signature use **domain-separated,
//! length-prefixed** payloads (the `compute_causal_record_hash` house style), so
//! a governor-release digest or signature can never collide with — or be replayed
//! as — an audit-chain or causal hash/signature.
//!
//! Out of scope here (deployment / integration): where the governor's signing key
//! comes from, key rotation/selection, and the wire/SHM carriage of the token
//! alongside the command. This module is the pure binding logic; it takes keys as
//! parameters and is exercised end-to-end in tests.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use kirra_contract_channel::GovernorContractView;

/// Domain tag for the contract digest (step 5). Distinct from every audit-chain
/// / causal tag so the two hash spaces never collide.
const DIGEST_DOMAIN: &[u8] = b"KIRRA-GOVERNOR-CONTRACT-DIGEST-V1";

/// Domain tag for the release-token signing payload (step 6). Distinct again, so
/// a release signature can never be reused as an audit signature and vice versa.
const RELEASE_DOMAIN: &[u8] = b"KIRRA-GOVERNOR-RELEASE-V1";

/// The release token: a digest of the approved command plus the governor's
/// Ed25519 signature over it. Minimal and fixed-size (96 bytes on the wire); the
/// actuator is supplied the governor verifying key out-of-band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseToken {
    /// SHA-256 digest over the validated snapshot's canonical image (step 5).
    pub digest: [u8; 32],
    /// Ed25519 signature over the domain-separated digest payload (step 6).
    pub signature: [u8; 64],
}

/// Why the actuator refused to release (step 7). Both are fail-closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseDenied {
    /// The token's digest does not match the command about to be actuated — the
    /// approval was for *different* bytes. (Substitution / stale token.)
    DigestMismatch,
    /// The signature does not verify against the governor key — forged or
    /// tampered token, or wrong signer.
    SignatureInvalid,
}

/// The digest payload (step 5): domain tag, then the length-prefixed canonical
/// image. The image is fixed-size, but the length prefix keeps the encoding
/// injective and consistent with the audit-chain discipline.
fn digest_payload(view: &GovernorContractView) -> ([u8; 32 + 1024], usize) {
    // Stack buffer sized to comfortably hold the domain tag + 8-byte length +
    // the canonical image (176 bytes today). Returned with its used length.
    let image = view.canonical_image();
    let mut buf = [0u8; 32 + 1024];
    let mut n = 0;
    buf[n..n + DIGEST_DOMAIN.len()].copy_from_slice(DIGEST_DOMAIN);
    n += DIGEST_DOMAIN.len();
    buf[n..n + 8].copy_from_slice(&(image.len() as u64).to_le_bytes());
    n += 8;
    buf[n..n + image.len()].copy_from_slice(&image);
    n += image.len();
    (buf, n)
}

/// Step 5: the digest over the exact validated snapshot bytes. Deterministic;
/// independent of the live shared region.
pub fn contract_digest(view: &GovernorContractView) -> [u8; 32] {
    let (buf, n) = digest_payload(view);
    let mut hasher = Sha256::new();
    hasher.update(&buf[..n]);
    hasher.finalize().into()
}

/// The signing payload over a digest (step 6): domain tag, then the 32-byte
/// digest length-prefixed. Domain separation prevents cross-protocol reuse.
fn release_signing_payload(digest: &[u8; 32]) -> [u8; RELEASE_DOMAIN.len() + 8 + 32] {
    let mut out = [0u8; RELEASE_DOMAIN.len() + 8 + 32];
    let mut n = 0;
    out[n..n + RELEASE_DOMAIN.len()].copy_from_slice(RELEASE_DOMAIN);
    n += RELEASE_DOMAIN.len();
    out[n..n + 8].copy_from_slice(&(32u64).to_le_bytes());
    n += 8;
    out[n..n + 32].copy_from_slice(digest);
    out
}

/// Step 6: issue a release token binding `view` to the governor's approval, by
/// signing its digest with `signing_key`.
pub fn issue_release_token(view: &GovernorContractView, signing_key: &SigningKey) -> ReleaseToken {
    let digest = contract_digest(view);
    let signature = signing_key.sign(&release_signing_payload(&digest));
    ReleaseToken { digest, signature: signature.to_bytes() }
}

/// Step 7: the actuator's verify-before-release gate. `actuating_view` is the
/// command the actuator is about to drive; `governor_vk` is the trusted governor
/// key. Returns `Ok(())` only if the token's digest matches the actuating command
/// **and** the signature verifies — otherwise a fail-closed [`ReleaseDenied`].
pub fn verify_release(
    token: &ReleaseToken,
    actuating_view: &GovernorContractView,
    governor_vk: &VerifyingKey,
) -> Result<(), ReleaseDenied> {
    // (a) The token must approve exactly the bytes about to be actuated.
    let expected = contract_digest(actuating_view);
    if token.digest != expected {
        return Err(ReleaseDenied::DigestMismatch);
    }
    // (b) The signature must verify against the governor key, over the same
    // digest. `verify_strict` is the attestation path (rejects malleable / small-
    // order points). A tampered digest field fails here even if (a) somehow passed.
    let sig = Signature::from_bytes(&token.signature);
    if governor_vk.verify_strict(&release_signing_payload(&token.digest), &sig).is_err() {
        return Err(ReleaseDenied::SignatureInvalid);
    }
    Ok(())
}

/// Forensic key id of the governor signing key — the `audit_chain` key-id
/// discipline (hex SHA-256 of the public key), for logging which key signed a
/// release without storing it in the wire token.
pub fn governor_key_id(vk: &VerifyingKey) -> String {
    crate::audit_chain::verifying_key_id(vk)
}

impl ReleaseToken {
    /// Canonical 96-byte wire form: `digest(32) || signature(64)`.
    pub fn to_bytes(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..32].copy_from_slice(&self.digest);
        out[32..].copy_from_slice(&self.signature);
        out
    }

    /// Parse the canonical wire form. (No verification — that is [`verify_release`].)
    pub fn from_bytes(bytes: &[u8; 96]) -> Self {
        let mut digest = [0u8; 32];
        let mut signature = [0u8; 64];
        digest.copy_from_slice(&bytes[..32]);
        signature.copy_from_slice(&bytes[32..]);
        Self { digest, signature }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn governor_key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    fn view(seq: u64, cmd: &[u8]) -> GovernorContractView {
        GovernorContractView::new_command(2, seq, 100, 10_000, cmd).unwrap()
    }

    #[test]
    fn honest_token_releases() {
        let sk = governor_key();
        let v = view(1, b"steer:1.5");
        let token = issue_release_token(&v, &sk);
        assert_eq!(verify_release(&token, &v, &sk.verifying_key()), Ok(()));
    }

    #[test]
    fn digest_binds_the_exact_command_bytes() {
        // A token issued for one command must NOT release a different command,
        // even a one-byte change — the digest mismatch is caught.
        let sk = governor_key();
        let approved = view(1, b"steer:1.5");
        let token = issue_release_token(&approved, &sk);

        let tampered = view(1, b"steer:9.9");
        assert_eq!(
            verify_release(&token, &tampered, &sk.verifying_key()),
            Err(ReleaseDenied::DigestMismatch)
        );
    }

    #[test]
    fn any_header_field_change_changes_the_digest() {
        // The digest covers the whole canonical image, not just the payload: a
        // changed sequence (replay number) or deadline yields a different digest.
        let sk = governor_key();
        let token = issue_release_token(&view(1, b"go"), &sk);
        assert_eq!(
            verify_release(&token, &view(2, b"go"), &sk.verifying_key()),
            Err(ReleaseDenied::DigestMismatch),
            "a different sequence is a different approved command"
        );
    }

    #[test]
    fn wrong_governor_key_is_rejected() {
        let real = governor_key();
        let imposter = SigningKey::from_bytes(&[7u8; 32]);
        let v = view(1, b"go");
        // Token signed by the imposter; actuator trusts the real governor key.
        let token = issue_release_token(&v, &imposter);
        assert_eq!(
            verify_release(&token, &v, &real.verifying_key()),
            Err(ReleaseDenied::SignatureInvalid)
        );
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let sk = governor_key();
        let v = view(1, b"go");
        let mut token = issue_release_token(&v, &sk);
        token.signature[0] ^= 0x01;
        assert_eq!(
            verify_release(&token, &v, &sk.verifying_key()),
            Err(ReleaseDenied::SignatureInvalid)
        );
    }

    #[test]
    fn tampered_digest_field_is_rejected() {
        // Flipping the token's digest to match a substituted command does not
        // help: the signature was over the original digest, so verify fails.
        let sk = governor_key();
        let v = view(1, b"go");
        let mut token = issue_release_token(&v, &sk);
        let substitute = view(1, b"no");
        token.digest = contract_digest(&substitute); // now matches `substitute`
        assert_eq!(
            verify_release(&token, &substitute, &sk.verifying_key()),
            Err(ReleaseDenied::SignatureInvalid),
            "digest matches the substitute, but the signature does not cover it"
        );
    }

    #[test]
    fn wire_roundtrip_preserves_the_token() {
        let sk = governor_key();
        let token = issue_release_token(&view(3, b"abc"), &sk);
        assert_eq!(ReleaseToken::from_bytes(&token.to_bytes()), token);
    }

    #[test]
    fn release_payload_is_domain_separated_from_a_bare_digest() {
        // The signed payload is NOT the bare digest; a signature is bound to the
        // release domain and cannot be reused where a bare-digest signature is
        // expected. (Guards against cross-protocol signature reuse.)
        let sk = governor_key();
        let v = view(1, b"go");
        let token = issue_release_token(&v, &sk);
        let bare = Signature::from_bytes(&token.signature);
        assert!(
            sk.verifying_key().verify_strict(&token.digest, &bare).is_err(),
            "the signature must not verify over the bare digest (no domain tag)"
        );
    }

    #[test]
    fn key_id_is_stable_hex_sha256() {
        let sk = governor_key();
        let id = governor_key_id(&sk.verifying_key());
        assert_eq!(id.len(), 64); // hex SHA-256
        assert_eq!(id, governor_key_id(&sk.verifying_key()));
    }
}
