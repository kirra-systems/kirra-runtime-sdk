//! # kirra-release-token — the governor→actuator release bridge (HVCHAN §3 steps 5-7)
//!
//! The last link of the L3 trust chain (ADR-0030 Clause F, ADR-0013): once the
//! governor has validated a [`GovernorContractView`] and decided it is
//! **actuatable**, it **signs the digest of that exact view** so the actuator can
//! verify — before it releases the command — that *"the governor approved exactly
//! these bytes."* A guest cannot forge it; a single flipped byte between governor
//! and actuator invalidates it.
//!
//! Steps 1-4 (the frozen layout, the seqlock coherent read, and validation) live
//! in [`kirra_contract_channel`]. This crate closes the chain:
//!
//! - **Step 5 — digest.** [`contract_digest`] hashes the **signable view's**
//!   [`GovernorContractView::canonical_image`] — the bytes the governor approved,
//!   not the live region which may already have moved on. Note transport `validate`
//!   only CRCs `command[..command_len]`, whereas `canonical_image` covers the
//!   *full* command array; callers must therefore pass a canonicalized / **enforced**
//!   view (e.g. `kirra_core::contract_consumer::GovernorCycle::view_to_sign`, which
//!   zero-fills the tail) so the digest is over deterministic bytes rather than
//!   attacker-controlled padding.
//! - **Step 6 — release token.** [`issue_release_token`] signs that digest with
//!   the governor's Ed25519 key.
//! - **Step 7 — actuator verify-before-release.** [`verify_release`] re-derives
//!   the digest over the command the actuator is **about to actuate** and checks
//!   (a) the token's digest matches it **and** (b) the signature verifies against
//!   the governor key. Either failure ⇒ **no release** (fail-closed).
//!
//! **No new crypto primitives** (HVCHAN §3): the SAME `ed25519-dalek` + `sha2`
//! the verifier's attestation / audit-chain paths already use, applied at this
//! seam. `#![forbid(unsafe_code)]`.
//!
//! **One canonical token format.** Both the digest and the token signature use
//! **domain-separated, length-prefixed** payloads (the `audit_chain` /
//! `compute_causal_record_hash` house style), so a governor-release digest or
//! signature can never collide with — or be replayed as — an audit-chain or causal
//! hash/signature. This crate is the SINGLE source of the release-token format:
//! the heavy `kirra-verifier` root crate re-exports it from
//! `src/governor_release.rs` (which adds only the root-local `governor_key_id`
//! forensic helper), so there is exactly one digest/sign/verify implementation.

#![forbid(unsafe_code)]

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use kirra_contract_channel::GovernorContractView;

/// Fail-closed provisioning of the governor signing key (ADR-0031 Clause E) — the
/// ONE place that decides *where* the key `issue_release_token` signs with comes
/// from (file / dev-fixed; TPM-unseal deferred). See [`provisioning`].
pub mod provisioning;

/// WP-13 (MGA G-7) — the Uptane four-role OTA metadata model (root / targets /
/// snapshot / timestamp): role-separated keys, rollback + freeze protection,
/// and key rotation. Pure metadata + verification core; see [`uptane`].
pub mod uptane;

/// WP-24 slice 2 (MGA G-15) — project a VERIFIED Uptane `targets` manifest onto
/// the parko model-integrity vocabulary (the signed model allow-list); see
/// [`model_targets`].
pub mod model_targets;

/// Crate-private standard-alphabet base64 (padding), shared by
/// [`artifact_release`] and [`uptane`]. Inlined so the lean actuation-path
/// crate pulls no `base64` dependency (see the manifest header).
pub(crate) mod b64 {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(crate) fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(B64[(n >> 18) as usize & 63] as char);
            out.push(B64[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
        }
        out
    }

    pub(crate) fn decode(s: &str) -> Option<Vec<u8>> {
        let s = s.trim_end_matches('=');
        let mut out = Vec::with_capacity(s.len() * 3 / 4);
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for c in s.bytes() {
            let v = B64.iter().position(|&b| b == c)? as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn round_trips_all_lengths() {
            for len in 0..70 {
                let data: Vec<u8> = (0..len as u8).collect();
                assert_eq!(decode(&encode(&data)).unwrap(), data, "len {len}");
            }
        }
    }
}

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

/// Step 5: the digest over the exact validated snapshot bytes. Deterministic;
/// independent of the live shared region.
///
/// The digest input is the domain tag, then the length-prefixed canonical image
/// (the audit-chain house style — the length prefix keeps the encoding injective).
/// It is **streamed directly into the hasher** rather than assembled in a buffer,
/// so there is no fixed-size assumption to overflow if `canonical_image()` ever
/// grows: `SHA-256(domain || len || image)` is identical fed in one slice or three.
#[must_use]
pub fn contract_digest(view: &GovernorContractView) -> [u8; 32] {
    let image = view.canonical_image();
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update((image.len() as u64).to_le_bytes());
    hasher.update(image);
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
/// signing its digest with `signing_key`. The governor calls this on an
/// actuatable verdict, over the exact snapshot it validated (e.g.
/// `kirra_core::contract_consumer::GovernorCycle::view_to_sign`).
#[must_use]
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
    // Re-derive the digest over the command the actuator is about to actuate,
    // then defer to the crypto core.
    verify_release_over_digest(token, &contract_digest(actuating_view), governor_vk)
}

/// The crypto core of step 7, over an ALREADY-COMPUTED expected digest — the
/// caller supplies the digest of the command it is about to actuate (rather than
/// a [`GovernorContractView`] to re-derive it from). Same two fail-closed checks
/// as [`verify_release`]: (a) the token approves exactly `expected_digest`, and
/// (b) the signature verifies against the governor key. This is the seam the C ABI
/// (`kirra_verify_release_token`) and any caller that already holds the digest use.
pub fn verify_release_over_digest(
    token: &ReleaseToken,
    expected_digest: &[u8; 32],
    governor_vk: &VerifyingKey,
) -> Result<(), ReleaseDenied> {
    // (a) The token must approve exactly the bytes about to be actuated.
    if &token.digest != expected_digest {
        return Err(ReleaseDenied::DigestMismatch);
    }
    // (b) The signature must verify against the governor key, over the same
    // digest. `verify_strict` is the attestation path (rejects malleable / small-
    // order points). A tampered digest field fails here even if (a) somehow passed.
    let sig = Signature::from_bytes(&token.signature);
    if governor_vk
        .verify_strict(&release_signing_payload(&token.digest), &sig)
        .is_err()
    {
        return Err(ReleaseDenied::SignatureInvalid);
    }
    Ok(())
}

impl ReleaseToken {
    /// Canonical 96-byte wire form: `digest(32) || signature(64)`.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..32].copy_from_slice(&self.digest);
        out[32..].copy_from_slice(&self.signature);
        out
    }

    /// Parse the canonical wire form. (No verification — that is [`verify_release`].)
    #[must_use]
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

    /// The extracted crypto core [`verify_release_over_digest`] agrees with
    /// [`verify_release`] and applies both fail-closed checks over a supplied
    /// digest (the seam the C ABI uses).
    #[test]
    fn verify_over_digest_matches_the_view_path() {
        let sk = governor_key();
        let vk = sk.verifying_key();
        let v = view(1, b"steer:1.5");
        let token = issue_release_token(&v, &sk);
        let digest = contract_digest(&v);

        // Matches the view-based path on the honest token.
        assert_eq!(verify_release_over_digest(&token, &digest, &vk), Ok(()));
        // Digest of a different command → mismatch.
        let other = contract_digest(&view(1, b"steer:9.9"));
        assert_eq!(
            verify_release_over_digest(&token, &other, &vk),
            Err(ReleaseDenied::DigestMismatch)
        );
        // Wrong key → signature invalid.
        let wrong = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        assert_eq!(
            verify_release_over_digest(&token, &digest, &wrong),
            Err(ReleaseDenied::SignatureInvalid)
        );
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
}

// ---------------------------------------------------------------------------
// WP-12 (MGA G-7) — governor ARTIFACT release signatures.
// ---------------------------------------------------------------------------

/// WP-12 — sign/verify a governor ARTIFACT digest (the OTA campaign's
/// content identity), domain-separated from the per-command release token
/// above: a signature over an artifact can never be replayed as a command
/// release nor vice versa. The payload signs the VALIDATED 64-lowercase-hex
/// ASCII digest (the canonical representation campaigns, stores, and the
/// assignment API already exchange), length-prefixed per the house style.
pub mod artifact_release {
    use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

    const ARTIFACT_RELEASE_DOMAIN: &[u8] = b"KIRRA-GOVERNOR-ARTIFACT-RELEASE-V1";

    /// Why an artifact-release signature was refused (or could not be made).
    /// Fail-closed: every variant means "do not stage/serve this artifact".
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ArtifactReleaseError {
        /// The digest is not 64 lowercase hex chars — refuse before any crypto.
        MalformedDigest,
        /// The signature is not valid base64 / not 64 bytes.
        MalformedSignature,
        /// The signature does not verify against the release key.
        SignatureInvalid,
    }

    fn digest_hex_valid(digest_hex: &str) -> bool {
        digest_hex.len() == 64
            && digest_hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }

    fn payload(digest_hex: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(ARTIFACT_RELEASE_DOMAIN.len() + 8 + digest_hex.len());
        out.extend_from_slice(ARTIFACT_RELEASE_DOMAIN);
        out.extend_from_slice(&(digest_hex.len() as u64).to_le_bytes());
        out.extend_from_slice(digest_hex.as_bytes());
        out
    }

    /// Sign an artifact digest with the governor release key → base64
    /// signature (the campaign-record / assignment-API wire form).
    pub fn sign_artifact_release(
        digest_hex: &str,
        signing_key: &SigningKey,
    ) -> Result<String, ArtifactReleaseError> {
        if !digest_hex_valid(digest_hex) {
            return Err(ArtifactReleaseError::MalformedDigest);
        }
        let sig = signing_key.sign(&payload(digest_hex));
        Ok(base64_encode(&sig.to_bytes()))
    }

    /// Verify a base64 artifact-release signature over `digest_hex` against
    /// the pinned release verifying key. `verify_strict` (the attestation
    /// path — rejects malleable/small-order points); every failure mode is a
    /// distinct fail-closed error.
    pub fn verify_artifact_release(
        digest_hex: &str,
        signature_b64: &str,
        release_vk: &VerifyingKey,
    ) -> Result<(), ArtifactReleaseError> {
        if !digest_hex_valid(digest_hex) {
            return Err(ArtifactReleaseError::MalformedDigest);
        }
        let bytes = base64_decode(signature_b64).ok_or(ArtifactReleaseError::MalformedSignature)?;
        let arr: [u8; 64] =
            bytes.try_into().map_err(|_| ArtifactReleaseError::MalformedSignature)?;
        let sig = Signature::from_bytes(&arr);
        release_vk
            .verify_strict(&payload(digest_hex), &sig)
            .map_err(|_| ArtifactReleaseError::SignatureInvalid)
    }

    use crate::b64::{decode as base64_decode, encode as base64_encode};

    #[cfg(test)]
    mod tests {
        use super::*;
        use ed25519_dalek::SigningKey;

        fn key() -> SigningKey {
            SigningKey::from_bytes(&[7u8; 32])
        }
        const DIGEST: &str =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        #[test]
        fn round_trip_signs_and_verifies() {
            let sk = key();
            let sig = sign_artifact_release(DIGEST, &sk).unwrap();
            assert!(verify_artifact_release(DIGEST, &sig, &sk.verifying_key()).is_ok());
        }

        #[test]
        fn wrong_key_is_refused() {
            let sig = sign_artifact_release(DIGEST, &key()).unwrap();
            let other = SigningKey::from_bytes(&[9u8; 32]);
            assert_eq!(
                verify_artifact_release(DIGEST, &sig, &other.verifying_key()),
                Err(ArtifactReleaseError::SignatureInvalid)
            );
        }

        #[test]
        fn signature_over_a_different_digest_is_refused() {
            let sk = key();
            let sig = sign_artifact_release(DIGEST, &sk).unwrap();
            let other_digest =
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            assert_eq!(
                verify_artifact_release(other_digest, &sig, &sk.verifying_key()),
                Err(ArtifactReleaseError::SignatureInvalid)
            );
        }

        #[test]
        fn malformed_digest_and_signature_fail_closed_before_crypto() {
            let sk = key();
            assert_eq!(
                sign_artifact_release("ABCDEF", &sk),
                Err(ArtifactReleaseError::MalformedDigest)
            );
            assert_eq!(
                verify_artifact_release("not-hex", "AAAA", &sk.verifying_key()),
                Err(ArtifactReleaseError::MalformedDigest)
            );
            assert_eq!(
                verify_artifact_release(DIGEST, "@@not-base64@@", &sk.verifying_key()),
                Err(ArtifactReleaseError::MalformedSignature)
            );
            assert_eq!(
                verify_artifact_release(DIGEST, "AAAA", &sk.verifying_key()),
                Err(ArtifactReleaseError::MalformedSignature),
                "wrong length after decode"
            );
        }

        /// Domain separation: an artifact signature is NOT a valid command
        /// release token signature (and the payloads differ by construction).
        #[test]
        fn artifact_domain_is_separated_from_command_release() {
            assert_ne!(ARTIFACT_RELEASE_DOMAIN, super::super::RELEASE_DOMAIN);
        }
    }
}
