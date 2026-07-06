//! Measured-boot TPM2 **quote** verification (Gate C #3 scaffold).
//!
//! `attestation::verify_attestation_proof_with_pcr16` binds a node's *self-reported*
//! PCR16 digest under its AK signature — honest, but a node in control of its AK
//! could still sign a false digest. The measured-boot root of trust is a genuine TPM2
//! **quote**: the TPM itself signs a `TPMS_ATTEST` structure over the LIVE PCR bank
//! plus a fresh nonce, using an Attestation Key resident in (and non-extractable
//! from) the TPM. The verifier then checks:
//!
//!   1. the quote signature verifies against the node's registered AK;
//!   2. the quote's `extraData` carries THIS challenge's nonce (freshness / anti-replay);
//!   3. the quoted PCR-composite digest equals the node's registered golden value
//!      (`expected_pcr16_digest_hex`).
//!
//! Because the TPM signs the actual PCR state, the node can no longer assert a false
//! measured-boot state — the gap the self-report path left open.
//!
//! ## Scaffold status (what is real vs. the hardware-integration seam)
//!
//! - **REAL + tested here:** the `TPMS_ATTEST` wire-format parser (TPM 2.0 §10.12.8,
//!   big-endian), the magic/type checks, `extraData`→nonce binding, and the
//!   PCR-composite-digest comparison — all fail-closed. A synthetic quote round-trips
//!   through the full verify in the tests.
//! - **THE ONE SEAM to swap on-device:** the quote SIGNATURE algorithm. This scaffold
//!   verifies with the existing Ed25519 AK model (so it's testable now and consistent
//!   with the rest of `attestation`). A production TPM AK is typically RSA-2048 or
//!   ECC-P256; wiring that in is a call-site swap of [`verify_quote_signature`] — the
//!   parser + binding + digest checks are unchanged. See
//!   `docs/safety/MEASURED_BOOT_ATTESTATION.md`.
//!
//! Nothing here is wired into the live `/attestation/verify` route yet — that needs
//! the node to actually produce quotes (Orin measured boot). This module is the
//! ready-to-wire verifier + its evidence.

use ed25519_dalek::{Signature, Verifier as _};

use crate::attestation::{parse_ed25519_public_pem, AttestationError};

/// `TPM_GENERATED_VALUE` — the magic (`"\xFFTCG"`) prefixing every structure the TPM
/// itself produced; guards against a caller passing a forged/externally-built blob.
pub const TPM_GENERATED_VALUE: u32 = 0xFF54_4347;

/// `TPM_ST_ATTEST_QUOTE` — the `TPMI_ST_ATTEST` tag identifying a PCR quote.
pub const TPM_ST_ATTEST_QUOTE: u16 = 0x8018;

/// The PCR the kirra measured-boot golden value is anchored on (measured boot / #73).
pub const KIRRA_MEASURED_BOOT_PCR: u8 = 16;

/// The parsed subset of a `TPMS_ATTEST` quote the verifier needs.
struct ParsedQuote<'a> {
    /// `extraData` — the requester's qualifying data; kirra puts the 8-byte
    /// big-endian challenge nonce here.
    extra_data: &'a [u8],
    /// The `TPMS_QUOTE_INFO.pcrDigest` — the composite hash over the selected PCRs.
    pcr_digest: &'a [u8],
}

/// A minimal big-endian cursor reader (TPM structures are network byte order). Every
/// read is bounds-checked and returns `None` past the end — a truncated quote fails
/// closed rather than panicking.
struct BeReader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> BeReader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    /// A `TPM2B_*` — a `UINT16` size followed by that many bytes.
    fn tpm2b(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// Parse a `TPMS_ATTEST` PCR quote far enough to extract `extraData` and the
/// `pcrDigest`. Returns `None` (→ [`AttestationError::MalformedQuote`]) on any
/// structural malformation, wrong magic, or non-quote type.
fn parse_quote(bytes: &[u8]) -> Option<ParsedQuote<'_>> {
    let mut r = BeReader::new(bytes);

    // magic (must be TPM-generated) + type (must be a quote).
    if r.u32()? != TPM_GENERATED_VALUE {
        return None;
    }
    if r.u16()? != TPM_ST_ATTEST_QUOTE {
        return None;
    }
    // qualifiedSigner: TPM2B_NAME — skipped (identity of the signing key; the AK
    // signature check below is the authority on who signed).
    r.tpm2b()?;
    // extraData: TPM2B_DATA — the challenge nonce.
    let extra_data = r.tpm2b()?;
    // clockInfo: TPMS_CLOCK_INFO = clock(u64) + resetCount(u32) + restartCount(u32) +
    // safe(u8) = 17 bytes; firmwareVersion: UINT64 = 8 bytes. Both skipped.
    r.take(17)?;
    r.take(8)?;

    // attested: TPMS_QUOTE_INFO { pcrSelect: TPML_PCR_SELECTION, pcrDigest: TPM2B_DIGEST }.
    // TPML_PCR_SELECTION: count(u32) then `count` × TPMS_PCR_SELECTION
    // { hash(u16), sizeofSelect(u8), pcrSelect[sizeofSelect] }.
    let count = r.u32()?;
    for _ in 0..count {
        let _hash = r.u16()?;
        let sizeof_select = r.u8()? as usize;
        r.take(sizeof_select)?;
    }
    // pcrDigest: TPM2B_DIGEST — the composite hash over the selected PCRs. Comparing
    // THIS to the registered golden value transitively enforces both the values AND
    // the selection (a different selection yields a different composite).
    let pcr_digest = r.tpm2b()?;

    Some(ParsedQuote {
        extra_data,
        pcr_digest,
    })
}

/// Verify the quote SIGNATURE over the raw `TPMS_ATTEST` bytes. **The one on-device
/// seam:** this scaffold uses the Ed25519 AK model; swap the body for RSA-2048 /
/// ECC-P256 `verify` when the AK is a real TPM key (the caller and the parser are
/// unchanged). Fail-closed on a malformed key or signature.
fn verify_quote_signature(
    ak_public_pem: &str,
    quote_bytes: &[u8],
    signature: &[u8],
) -> Result<(), AttestationError> {
    let vk =
        parse_ed25519_public_pem(ak_public_pem).ok_or(AttestationError::MalformedRegisteredKey)?;
    let sig_array: [u8; 64] = signature
        .try_into()
        .map_err(|_| AttestationError::MalformedQuote)?;
    vk.verify(quote_bytes, &Signature::from_bytes(&sig_array))
        .map_err(|_| AttestationError::SignatureInvalid)
}

/// Verify a measured-boot TPM2 quote against a node's registered AK + golden PCR
/// value. Fail-closed at every step (see the module docs for the trust chain):
///
/// - no registered AK → [`AttestationError::NoRegisteredKey`];
/// - signature doesn't verify → [`AttestationError::SignatureInvalid`];
/// - blob isn't a genuine TPM PCR quote → [`AttestationError::MalformedQuote`];
/// - `extraData` ≠ the challenge nonce → [`AttestationError::QuoteNonceMismatch`];
/// - quoted PCR-composite digest ≠ the registered golden value →
///   [`AttestationError::Pcr16Mismatch`].
///
/// `expected_nonce` is THIS challenge's nonce (the node must have run the TPM quote
/// with it as qualifying data); `expected_pcr16_digest_hex` is the node's registered
/// golden measured-boot composite digest (`RegisteredNode::expected_pcr16_digest_hex`).
pub fn verify_measured_boot_quote(
    ak_public_pem: Option<&str>,
    quote_bytes: &[u8],
    signature: &[u8],
    expected_nonce: u64,
    expected_pcr16_digest_hex: &str,
) -> Result<(), AttestationError> {
    // 1. Registered AK required.
    let pem = ak_public_pem.ok_or(AttestationError::NoRegisteredKey)?;

    // 2. The TPM signs the ENTIRE TPMS_ATTEST — verify before trusting any field.
    verify_quote_signature(pem, quote_bytes, signature)?;

    // 3. Structural parse (fail-closed on any malformation / wrong magic / non-quote).
    let parsed = parse_quote(quote_bytes).ok_or(AttestationError::MalformedQuote)?;

    // 4. Freshness: extraData must carry THIS challenge's nonce (8-byte big-endian),
    //    so a captured old quote can't be replayed.
    if parsed.extra_data != expected_nonce.to_be_bytes() {
        return Err(AttestationError::QuoteNonceMismatch);
    }

    // 5. Measured-boot state: the quoted composite digest must equal the registered
    //    golden value. Non-secret comparison (the expectation is an admin-registered
    //    value; the security is the TPM signature that binds the live PCR state).
    let presented_hex = hex::encode(parsed.pcr_digest);
    if !presented_hex.eq_ignore_ascii_case(expected_pcr16_digest_hex.trim()) {
        return Err(AttestationError::Pcr16Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    // A test-only SPKI PEM for an Ed25519 verifying key (RFC 8410 prefix; public
    // only — no committed secret).
    fn public_key_to_pem(vk: &ed25519_dalek::VerifyingKey) -> String {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        const PREFIX: [u8; 12] = [
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        let mut der = PREFIX.to_vec();
        der.extend_from_slice(vk.as_bytes());
        format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            B64.encode(&der)
        )
    }

    /// Build a well-formed TPMS_ATTEST PCR quote with the given nonce (into extraData)
    /// and pcr-composite digest. Mirrors the parser's expected layout exactly.
    fn build_quote(nonce: u64, pcr_digest: &[u8]) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&TPM_GENERATED_VALUE.to_be_bytes());
        q.extend_from_slice(&TPM_ST_ATTEST_QUOTE.to_be_bytes());
        // qualifiedSigner TPM2B_NAME (4 dummy bytes).
        q.extend_from_slice(&4u16.to_be_bytes());
        q.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x01]);
        // extraData TPM2B_DATA = 8-byte BE nonce.
        let nb = nonce.to_be_bytes();
        q.extend_from_slice(&(nb.len() as u16).to_be_bytes());
        q.extend_from_slice(&nb);
        // clockInfo (17) + firmwareVersion (8).
        q.extend_from_slice(&[0u8; 17]);
        q.extend_from_slice(&0u64.to_be_bytes());
        // attested: TPML_PCR_SELECTION { count=1, [SHA256, sizeofSelect=3, select PCR16] }
        q.extend_from_slice(&1u32.to_be_bytes());
        q.extend_from_slice(&0x000Bu16.to_be_bytes()); // TPM_ALG_SHA256
        q.push(3u8);
        q.extend_from_slice(&[0x00, 0x00, 0x01]); // PCR16 = byte 2, bit 0
        // pcrDigest TPM2B_DIGEST.
        q.extend_from_slice(&(pcr_digest.len() as u16).to_be_bytes());
        q.extend_from_slice(pcr_digest);
        q
    }

    fn golden() -> ([u8; 32], String) {
        let d = [0x5Au8; 32];
        (d, hex::encode(d))
    }

    fn signed(sk: &SigningKey, quote: &[u8]) -> Vec<u8> {
        sk.sign(quote).to_bytes().to_vec()
    }

    #[test]
    fn valid_quote_verifies() {
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        let (dg, dg_hex) = golden();
        let q = build_quote(42, &dg);
        let sig = signed(&sk, &q);
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &q, &sig, 42, &dg_hex),
            Ok(())
        );
    }

    #[test]
    fn no_registered_key_fails_closed() {
        let (dg, dg_hex) = golden();
        let q = build_quote(1, &dg);
        assert_eq!(
            verify_measured_boot_quote(None, &q, &[0u8; 64], 1, &dg_hex),
            Err(AttestationError::NoRegisteredKey)
        );
    }

    #[test]
    fn wrong_signature_is_rejected() {
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let other = SigningKey::from_bytes(&[7u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        let (dg, dg_hex) = golden();
        let q = build_quote(42, &dg);
        let sig = signed(&other, &q); // signed by the WRONG key
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &q, &sig, 42, &dg_hex),
            Err(AttestationError::SignatureInvalid)
        );
    }

    #[test]
    fn tampered_pcr_digest_is_detected() {
        // Flip a byte of the quoted digest AFTER signing → sig fails first (the TPM
        // signs the whole blob). Instead, sign a quote whose digest differs from the
        // registered golden value: valid signature, but Pcr16Mismatch.
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        let (_dg, golden_hex) = golden();
        let different = [0x11u8; 32];
        let q = build_quote(42, &different);
        let sig = signed(&sk, &q);
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &q, &sig, 42, &golden_hex),
            Err(AttestationError::Pcr16Mismatch),
            "a genuine quote of the WRONG measured-boot state is rejected"
        );
    }

    #[test]
    fn stale_nonce_is_rejected() {
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        let (dg, dg_hex) = golden();
        let q = build_quote(100, &dg); // quote bound to nonce 100
        let sig = signed(&sk, &q);
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &q, &sig, 999, &dg_hex), // expect 999
            Err(AttestationError::QuoteNonceMismatch),
            "a quote not bound to this challenge's nonce is a replay → rejected"
        );
    }

    #[test]
    fn non_tpm_blob_is_malformed() {
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        // A validly-SIGNED blob that isn't a TPM quote (wrong magic) → MalformedQuote,
        // never silently accepted.
        let junk = b"not a tpm quote at all, but signed".to_vec();
        let sig = signed(&sk, &junk);
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &junk, &sig, 1, &golden().1),
            Err(AttestationError::MalformedQuote)
        );
    }

    #[test]
    fn truncated_quote_fails_closed_without_panic() {
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let pem = public_key_to_pem(&sk.verifying_key());
        let (dg, dg_hex) = golden();
        let mut q = build_quote(42, &dg);
        q.truncate(20); // chop mid-structure
        let sig = signed(&sk, &q);
        assert_eq!(
            verify_measured_boot_quote(Some(&pem), &q, &sig, 42, &dg_hex),
            Err(AttestationError::MalformedQuote)
        );
    }
}
