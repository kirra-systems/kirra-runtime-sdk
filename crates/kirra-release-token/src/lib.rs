//! # kirra-release-token — the governor→actuator release bridge (HVCHAN §3.5-6)
//!
//! The last link of the L3 trust chain (ADR-0030 Clause F, ADR-0013): once the
//! governor has validated a [`GovernorContractView`] and decided it is
//! **actuatable**, it **signs the digest of that exact view** so the actuator can
//! verify — before it releases the command — that *"the governor approved exactly
//! these bytes."* A guest cannot forge it; a single flipped byte between governor
//! and actuator invalidates it.
//!
//! Two steps, matching HVCHAN §3:
//! - **step 5 — digest.** SHA-256 over the canonical little-endian image
//!   ([`GovernorContractView::canonical_image`]).
//! - **step 6 — sign.** Ed25519 over that digest, with the governor's key.
//!
//! **No new crypto primitives** (HVCHAN §3): the SAME `ed25519-dalek` +
//! `sha2` the verifier's attestation path already uses (`src/attestation.rs`),
//! applied at this seam. `#![forbid(unsafe_code)]`.
//!
//! Fail-closed: [`verify_release`] returns `false` on ANY mismatch — a tampered
//! image, a wrong key, or a malformed/altered signature. The actuator releases
//! ONLY on `true`.

#![forbid(unsafe_code)]

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use kirra_contract_channel::GovernorContractView;

/// A governor release token: the Ed25519 signature (64 bytes) over the SHA-256
/// digest of the validated contract image. The bytes an actuator checks before
/// releasing a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseToken {
    /// The raw Ed25519 signature over the image digest.
    pub signature: [u8; 64],
}

/// SHA-256 over the canonical contract image (HVCHAN §3 step 5). Deterministic;
/// the actuator recomputes it from the image it holds, so a tampered image
/// yields a different digest and fails verification.
#[must_use]
pub fn digest_image(canonical_image: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(canonical_image);
    hasher.finalize().into()
}

/// Sign the digest of `canonical_image` with the governor's key (HVCHAN §3
/// steps 5-6). The image is [`GovernorContractView::canonical_image`] of the
/// exact validated snapshot; see [`sign_view`] for the typed convenience.
#[must_use]
pub fn sign_release(canonical_image: &[u8], signing_key: &SigningKey) -> ReleaseToken {
    let digest = digest_image(canonical_image);
    ReleaseToken { signature: signing_key.sign(&digest).to_bytes() }
}

/// Verify a release token against `canonical_image` and the governor's public
/// key — the actuator's pre-release gate. **Fail-closed:** `false` on any
/// mismatch (tampered image → different digest; wrong key; bad signature).
#[must_use]
pub fn verify_release(
    canonical_image: &[u8],
    token: &ReleaseToken,
    verifying_key: &VerifyingKey,
) -> bool {
    let digest = digest_image(canonical_image);
    verifying_key
        .verify_strict(&digest, &Signature::from_bytes(&token.signature))
        .is_ok()
}

/// Sign the validated [`GovernorContractView`] (its canonical image). The
/// governor calls this on an actuatable verdict, over the exact snapshot it
/// validated (e.g. `kirra_core::contract_consumer::GovernorCycle::view`).
#[must_use]
pub fn sign_view(view: &GovernorContractView, signing_key: &SigningKey) -> ReleaseToken {
    sign_release(&view.canonical_image(), signing_key)
}

/// Verify a release token against a [`GovernorContractView`] — the actuator's
/// gate over the typed view. Fail-closed, as [`verify_release`].
#[must_use]
pub fn verify_view(
    view: &GovernorContractView,
    token: &ReleaseToken,
    verifying_key: &VerifyingKey,
) -> bool {
    verify_release(&view.canonical_image(), token, verifying_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(seed: u8) -> (SigningKey, VerifyingKey) {
        // Deterministic (no RNG) — same pattern as the attestation tests.
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn image() -> [u8; kirra_contract_channel::CANONICAL_IMAGE_LEN] {
        GovernorContractView::new_command(2, 1, 0, u64::MAX / 2, b"steer:1.0")
            .unwrap()
            .canonical_image()
    }

    #[test]
    fn sign_then_verify_accepts() {
        let (sk, vk) = keypair(1);
        let img = image();
        let token = sign_release(&img, &sk);
        assert!(verify_release(&img, &token, &vk));
    }

    #[test]
    fn a_tampered_image_is_rejected() {
        let (sk, vk) = keypair(1);
        let mut img = image();
        let token = sign_release(&img, &sk);
        img[48] ^= 0xFF; // flip a command byte AFTER signing
        assert!(!verify_release(&img, &token, &vk));
    }

    #[test]
    fn a_wrong_public_key_is_rejected() {
        let (sk, _) = keypair(1);
        let (_, other_vk) = keypair(2);
        let img = image();
        let token = sign_release(&img, &sk);
        assert!(!verify_release(&img, &token, &other_vk));
    }

    #[test]
    fn a_tampered_token_is_rejected() {
        let (sk, vk) = keypair(1);
        let img = image();
        let mut token = sign_release(&img, &sk);
        token.signature[0] ^= 0xFF;
        assert!(!verify_release(&img, &token, &vk));
    }

    #[test]
    fn sign_view_agrees_with_sign_release_over_the_canonical_image() {
        let (sk, vk) = keypair(3);
        let view = GovernorContractView::new_command(4, 2, 0, u64::MAX / 2, b"steer:2.0").unwrap();
        let token = sign_view(&view, &sk);
        assert!(verify_view(&view, &token, &vk));
        // Same as signing the raw canonical image.
        assert_eq!(token, sign_release(&view.canonical_image(), &sk));
    }
}
