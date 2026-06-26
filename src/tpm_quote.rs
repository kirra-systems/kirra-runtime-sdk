//! # TPM 2.0 quote verification (measured-boot, hardware-rooted) — the deeper
//! attestation follow-up named in `attestation.rs`.
//!
//! `verify_attestation_proof_with_pcr16` (the #572 step) binds + enforces the
//! node's *self-reported* PCR16 digest under its AK signature. Its honest limit:
//! a node in control of its AK could sign a FALSE digest, because nothing ties
//! the digest to the actual measured-boot state.
//!
//! A TPM **quote** closes that: the TPM itself (not the node software) produces a
//! `TPMS_ATTEST` structure over the live PCR bank and a caller-supplied nonce,
//! signed by the AK. The verifier here parses that structure and checks:
//!   1. the AK signature over the exact quote bytes (authenticity),
//!   2. `magic == TPM_GENERATED_VALUE` — the structure was produced by a TPM,
//!      not forged in software (this is the load-bearing distinction from #572),
//!   3. `type == TPM_ST_ATTEST_QUOTE`,
//!   4. `extraData == nonce` — anti-replay / freshness (bound to the challenge),
//!   5. PCR16 is in the quoted selection,
//!   6. the quoted `pcrDigest` equals the registered expectation.
//!
//! This module is the PURE, hardware-free verification engine (parser + checks),
//! testable with synthetic quotes (see [`tests::marshal_quote`]). Generating a
//! real quote on a node is a TPM/`tss-esapi` concern (the `tpm` feature).
//!
//! LIVE WIRING: [`verify_tpm_quote`] is enforced on `/attestation/verify` for a
//! node whose `node_attestation_policy.require_tpm_quote` is set; the handler
//! bridges the registered raw PCR16 value to the quote's `pcrDigest` via
//! [`expected_single_pcr_digest_hex`], and [`marshal_pcr16_quote`] is the
//! reference body encoder the node side must match.
//!
//! Marshaling note: TPM 2.0 structures are BIG-ENDIAN; `TPM2B_*` fields are a
//! `u16` size prefix followed by that many bytes. Every read is bounds-checked
//! and fails closed (a short/garbage quote is `MalformedQuote`, never a panic).
//!
//! AK algorithm: this system's AK is Ed25519; TPM 2.0 defines `TPM_ALG_EDDSA`,
//! so the quote signature is verified with the same `verify_strict` Ed25519 path
//! as the base attestation proof.

use ed25519_dalek::{Signature, VerifyingKey};

use crate::attestation::parse_ed25519_public_pem;

/// `TPM_GENERATED_VALUE` (TPM 2.0 Part 2 §6.2) — every TPM-generated attestation
/// structure begins with this. A software-forged structure cannot set it without
/// also forging the AK signature over it; together they root trust in the TPM.
const TPM_GENERATED_VALUE: u32 = 0xFF54_4347; // "\xFFTCG"

/// `TPM_ST_ATTEST_QUOTE` (TPM 2.0 Part 2 §10.12.10) — the `type` of a quote.
const TPM_ST_ATTEST_QUOTE: u16 = 0x8018;

/// `TPM_ALG_SHA256` (TPM 2.0 Part 2 §6.3).
const TPM_ALG_SHA256: u16 = 0x000B;

/// PCR index for the measured-boot register this system binds.
const PCR16_INDEX: usize = 16;

/// Bound on the number of `TPMS_PCR_SELECTION` entries parsed — a real quote has
/// one or a few; a huge count is a malformed/hostile quote, rejected (not looped).
const MAX_PCR_SELECTIONS: u32 = 16;

/// Length of `TPMS_CLOCK_INFO`: clock(u64) + resetCount(u32) + restartCount(u32)
/// + safe(u8) = 17 bytes.
const CLOCK_INFO_LEN: usize = 17;

/// Structured, non-leaking TPM-quote verification failures. Every variant is a
/// fail-closed reject; carries no key/quote material, only the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpmQuoteError {
    /// No registered AK → cannot verify a quote.
    NoRegisteredKey,
    /// The registered `ak_public_pem` is not a parseable Ed25519 SPKI key.
    MalformedRegisteredKey,
    /// The quote or signature was not valid hex, or the signature was not 64 bytes.
    MalformedEncoding,
    /// The quote bytes could not be parsed as a `TPMS_ATTEST` (short/garbage).
    MalformedQuote,
    /// The AK signature did not verify over the quote bytes.
    SignatureInvalid,
    /// `magic != TPM_GENERATED_VALUE` — not a TPM-produced structure.
    NotTpmGenerated,
    /// `type != TPM_ST_ATTEST_QUOTE` — a non-quote attestation (e.g. a certify).
    NotAQuote,
    /// `extraData != nonce` — the quote is not bound to this challenge (replay/stale).
    NonceMismatch,
    /// PCR16 was not in the quote's PCR selection.
    Pcr16NotSelected,
    /// The quoted `pcrDigest` did not equal the registered expectation.
    PcrDigestMismatch,
}

impl TpmQuoteError {
    /// Stable, material-free reason token for HTTP/audit surfaces.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoRegisteredKey => "tpm quote: node has no registered attestation key",
            Self::MalformedRegisteredKey => "tpm quote: registered attestation key is malformed",
            Self::MalformedEncoding => "tpm quote: quote/signature encoding malformed",
            Self::MalformedQuote => "tpm quote: TPMS_ATTEST structure malformed",
            Self::SignatureInvalid => "tpm quote: signature invalid",
            Self::NotTpmGenerated => "tpm quote: not TPM-generated (bad magic)",
            Self::NotAQuote => "tpm quote: attestation is not a quote",
            Self::NonceMismatch => "tpm quote: nonce/extraData mismatch",
            Self::Pcr16NotSelected => "tpm quote: PCR16 not in selection",
            Self::PcrDigestMismatch => "tpm quote: PCR digest mismatch",
        }
    }
}

/// A bounds-checked big-endian reader over the quote bytes. Every accessor
/// returns `None` on a short read, so the parser fails closed rather than panics.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// A `TPM2B_*`: a `u16` size prefix followed by that many bytes.
    fn tpm2b(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// The fields extracted from a parsed quote that the checks consume.
struct ParsedQuote<'a> {
    magic: u32,
    typ: u16,
    extra_data: &'a [u8],
    pcr16_selected: bool,
    pcr_digest: &'a [u8],
}

/// Parse a marshaled `TPMS_ATTEST` of `type == QUOTE`. Returns `None` (→
/// `MalformedQuote`) on any short read or an over-large PCR selection count.
fn parse_quote(buf: &[u8]) -> Option<ParsedQuote<'_>> {
    let mut r = Reader::new(buf);
    let magic = r.u32()?;
    let typ = r.u16()?;
    let _qualified_signer = r.tpm2b()?; // TPM2B_NAME (AK name) — not checked here
    let extra_data = r.tpm2b()?; // TPM2B_DATA — the nonce
    let _clock_info = r.take(CLOCK_INFO_LEN)?;
    let _firmware_version = r.take(8)?; // UINT64
    // TPMS_QUOTE_INFO: TPML_PCR_SELECTION { count, [TPMS_PCR_SELECTION] } + TPM2B_DIGEST
    let count = r.u32()?;
    if count > MAX_PCR_SELECTIONS {
        return None; // hostile/garbage count — fail closed
    }
    let mut pcr16_selected = false;
    for _ in 0..count {
        let hash_alg = r.u16()?;
        let size_of_select = r.u8()? as usize;
        let select = r.take(size_of_select)?;
        // PCR16 → byte 16/8 = 2, bit 16%8 = 0.
        let byte_idx = PCR16_INDEX / 8;
        let bit = 1u8 << (PCR16_INDEX % 8);
        if hash_alg == TPM_ALG_SHA256
            && select.len() > byte_idx
            && (select[byte_idx] & bit) != 0
        {
            pcr16_selected = true;
        }
    }
    let pcr_digest = r.tpm2b()?; // TPM2B_DIGEST — hash over the selected PCRs
    Some(ParsedQuote { magic, typ, extra_data, pcr16_selected, pcr_digest })
}

/// Verify a TPM 2.0 quote (`TPMS_ATTEST` of type QUOTE) for measured-boot
/// attestation. Fail-closed on every path.
///
/// - `ak_public_pem` — the node's registered AK (Ed25519 SPKI). `None` →
///   [`TpmQuoteError::NoRegisteredKey`].
/// - `nonce` — the challenge nonce the verifier issued; must equal the quote's
///   `extraData` (anti-replay).
/// - `expected_pcr_digest_hex` — the registered expected value of the quote's
///   `pcrDigest` (the hash over the selected PCRs; for a PCR16-only quote, the
///   hash of PCR16). Compared case-insensitively.
/// - `quote_msg_hex` — hex of the marshaled `TPMS_ATTEST` the AK signed.
/// - `signature_hex` — hex of the 64-byte Ed25519 signature over `quote_msg`.
// SAFETY: SG9 | REQ: attestation-tpm-quote-verification | TEST: tpm_quote_valid_verifies,tpm_quote_bad_signature_rejected,tpm_quote_bad_magic_rejected,tpm_quote_wrong_type_rejected,tpm_quote_nonce_mismatch_rejected,tpm_quote_pcr16_not_selected_rejected,tpm_quote_pcr_digest_mismatch_rejected,tpm_quote_truncated_is_malformed,tpm_quote_absent_key_fails_closed
pub fn verify_tpm_quote(
    ak_public_pem: Option<&str>,
    nonce: &[u8],
    expected_pcr_digest_hex: &str,
    quote_msg_hex: &str,
    signature_hex: &str,
) -> Result<(), TpmQuoteError> {
    // Registered AK (fail-closed when absent / malformed).
    let pem = ak_public_pem.ok_or(TpmQuoteError::NoRegisteredKey)?;
    let vk: VerifyingKey =
        parse_ed25519_public_pem(pem).ok_or(TpmQuoteError::MalformedRegisteredKey)?;

    // Decode the quote bytes + the 64-byte Ed25519 signature.
    let quote = hex::decode(quote_msg_hex).map_err(|_| TpmQuoteError::MalformedEncoding)?;
    let sig_bytes = hex::decode(signature_hex).map_err(|_| TpmQuoteError::MalformedEncoding)?;
    let sig_array: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| TpmQuoteError::MalformedEncoding)?;
    let signature = Signature::from_bytes(&sig_array);

    // 1. Authenticity: the AK signed THESE quote bytes. (Done before parsing so an
    //    unauthenticated structure is never inspected for trust decisions.)
    vk.verify_strict(&quote, &signature)
        .map_err(|_| TpmQuoteError::SignatureInvalid)?;

    // 2–6. Structure + content checks.
    let parsed = parse_quote(&quote).ok_or(TpmQuoteError::MalformedQuote)?;
    if parsed.magic != TPM_GENERATED_VALUE {
        return Err(TpmQuoteError::NotTpmGenerated);
    }
    if parsed.typ != TPM_ST_ATTEST_QUOTE {
        return Err(TpmQuoteError::NotAQuote);
    }
    if parsed.extra_data != nonce {
        return Err(TpmQuoteError::NonceMismatch);
    }
    if !parsed.pcr16_selected {
        return Err(TpmQuoteError::Pcr16NotSelected);
    }
    let quoted_digest_hex = hex::encode(parsed.pcr_digest);
    if !quoted_digest_hex.eq_ignore_ascii_case(expected_pcr_digest_hex.trim()) {
        return Err(TpmQuoteError::PcrDigestMismatch);
    }
    Ok(())
}

/// Compute the `pcrDigest` a TPM produces for a quote that selects EXACTLY the
/// SHA-256 PCR16 — i.e. `SHA256(pcr16_value)`. TPM 2.0 defines a quote's
/// `pcrDigest` as the bank hash over the concatenation of the selected PCR
/// values in increasing index order; for a single PCR that is `H(value)`.
///
/// The live attestation flow registers the raw PCR16 VALUE (the 32-byte
/// measured-boot register content, the same datum the #572 self-report binds);
/// the quote attests a HASH OVER it. This bridges the two: pass the registered
/// `expected_pcr16_digest_hex` (the value) and get the `pcrDigest` to compare a
/// PCR16-only quote against. Returns `None` if the input is not valid hex.
///
/// A quote that selects PCR16 PLUS other PCRs yields a different `pcrDigest`
/// and is therefore rejected by [`verify_tpm_quote`] against this expectation —
/// fail-closed: only a quote over exactly PCR16 with the expected value passes.
#[must_use]
pub fn expected_single_pcr_digest_hex(pcr16_value_hex: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let value = hex::decode(pcr16_value_hex.trim()).ok()?;
    Some(hex::encode(Sha256::digest(value)))
}

/// Reference encoder for the canonical single-PCR16 SHA-256 quote body a node's
/// TPM produces for this system: a `TPMS_ATTEST` of type `QUOTE`, `extraData =
/// nonce`, selecting exactly the SHA-256 PCR16, with `pcrDigest = SHA256(
/// pcr16_value)`. This is the WIRE-FORMAT SPECIFICATION the node side must
/// match (the bytes the AK signs) and the encoder the tests/tooling use.
///
/// It does NOT sign — authenticity comes solely from the node's AK over these
/// bytes; a verifier-side encoder cannot mint a verifiable quote without the
/// node's private key. `nonce` is the verifier's challenge in its canonical
/// 8-byte big-endian form (`u64::to_be_bytes`).
#[must_use]
pub fn marshal_pcr16_quote(nonce: &[u8], pcr16_value: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let pcr_digest = Sha256::digest(pcr16_value);
    let mut q = Vec::new();
    q.extend_from_slice(&TPM_GENERATED_VALUE.to_be_bytes());
    q.extend_from_slice(&TPM_ST_ATTEST_QUOTE.to_be_bytes());
    // qualifiedSigner (TPM2B_NAME) — not checked by the verifier.
    let signer = b"\x00\x0bAK-NAME-DIGEST";
    q.extend_from_slice(&(signer.len() as u16).to_be_bytes());
    q.extend_from_slice(signer);
    // extraData (TPM2B_DATA) — the challenge nonce.
    q.extend_from_slice(&(nonce.len() as u16).to_be_bytes());
    q.extend_from_slice(nonce);
    q.extend_from_slice(&[0u8; CLOCK_INFO_LEN]); // clockInfo
    q.extend_from_slice(&0u64.to_be_bytes()); // firmwareVersion
    // TPML_PCR_SELECTION: one selection, SHA-256, PCR16 set.
    q.extend_from_slice(&1u32.to_be_bytes());
    q.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes());
    q.push(3u8); // sizeofSelect (3 octets → PCR0..23)
    let mut sel = [0u8; 3];
    sel[PCR16_INDEX / 8] |= 1u8 << (PCR16_INDEX % 8);
    q.extend_from_slice(&sel);
    // pcrDigest (TPM2B_DIGEST).
    q.extend_from_slice(&(pcr_digest.len() as u16).to_be_bytes());
    q.extend_from_slice(&pcr_digest);
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};

    fn ephemeral() -> SigningKey {
        SigningKey::from_bytes(&[0x5A; 32])
    }

    fn pem(vk: &VerifyingKey) -> String {
        const PFX: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        let mut der = PFX.to_vec();
        der.extend_from_slice(vk.as_bytes());
        format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", B64.encode(&der))
    }

    fn tpm2b(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        out.extend_from_slice(bytes);
    }

    /// Marshal a synthetic `TPMS_ATTEST` quote (big-endian, TPM wire format) so a
    /// test can sign it and exercise the full verification path. `select_pcr16`
    /// toggles the PCR16 bit; the other fields are configurable to drive each
    /// rejection case.
    fn marshal_quote(magic: u32, typ: u16, nonce: &[u8], select_pcr16: bool, pcr_digest: &[u8]) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&magic.to_be_bytes());
        q.extend_from_slice(&typ.to_be_bytes());
        tpm2b(&mut q, b"\x00\x0bAK-NAME-DIGEST"); // qualifiedSigner (arbitrary)
        tpm2b(&mut q, nonce); // extraData
        q.extend_from_slice(&[0u8; CLOCK_INFO_LEN]); // clockInfo
        q.extend_from_slice(&0u64.to_be_bytes()); // firmwareVersion
        // TPML_PCR_SELECTION: count = 1
        q.extend_from_slice(&1u32.to_be_bytes());
        q.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes()); // hash
        q.push(3u8); // sizeofSelect (3 octets → 24 PCRs)
        let mut sel = [0u8; 3];
        if select_pcr16 {
            sel[PCR16_INDEX / 8] |= 1u8 << (PCR16_INDEX % 8);
        }
        q.extend_from_slice(&sel);
        tpm2b(&mut q, pcr_digest); // pcrDigest
        q
    }

    const NONCE: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
    const DIGEST: [u8; 32] = [0xAB; 32];

    fn signed(q: &[u8], sk: &SigningKey) -> (String, String) {
        (hex::encode(q), hex::encode(sk.sign(q).to_bytes()))
    }

    #[test]
    fn tpm_quote_valid_verifies() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &sk);
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Ok(())
        );
    }

    #[test]
    fn tpm_quote_bad_signature_rejected() {
        let sk = ephemeral();
        let attacker = SigningKey::from_bytes(&[0x11; 32]);
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &attacker); // signed by the WRONG key
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::SignatureInvalid)
        );
    }

    #[test]
    fn tpm_quote_bad_magic_rejected() {
        let sk = ephemeral();
        let q = marshal_quote(0x1234_5678, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &sk); // correctly signed, but not TPM-generated
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::NotTpmGenerated)
        );
    }

    #[test]
    fn tpm_quote_wrong_type_rejected() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, 0x8017 /* CERTIFY */, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &sk);
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::NotAQuote)
        );
    }

    #[test]
    fn tpm_quote_nonce_mismatch_rejected() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &sk);
        // The verifier expects a DIFFERENT nonce than the one in the quote.
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), b"different-nonce", &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::NonceMismatch)
        );
    }

    #[test]
    fn tpm_quote_pcr16_not_selected_rejected() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, false, &DIGEST);
        let (qh, sh) = signed(&q, &sk);
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::Pcr16NotSelected)
        );
    }

    #[test]
    fn tpm_quote_pcr_digest_mismatch_rejected() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &[0xCD; 32]);
        let (qh, sh) = signed(&q, &sk);
        // The quote attests digest 0xCD…, but the registration expected 0xAB….
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::PcrDigestMismatch)
        );
    }

    #[test]
    fn tpm_quote_truncated_is_malformed() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let truncated = &q[..q.len() - 10]; // chop the tail → short read while parsing
        let (qh, sh) = signed(truncated, &sk); // sign the truncated bytes so the sig passes
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::MalformedQuote)
        );
    }

    #[test]
    fn tpm_quote_absent_key_fails_closed() {
        let sk = ephemeral();
        let q = marshal_quote(TPM_GENERATED_VALUE, TPM_ST_ATTEST_QUOTE, NONCE, true, &DIGEST);
        let (qh, sh) = signed(&q, &sk);
        assert_eq!(
            verify_tpm_quote(None, NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::NoRegisteredKey)
        );
    }

    #[test]
    fn tpm_quote_over_large_pcr_selection_count_is_malformed() {
        // A hostile count field (> MAX_PCR_SELECTIONS) must be rejected, not looped.
        let sk = ephemeral();
        let mut q = Vec::new();
        q.extend_from_slice(&TPM_GENERATED_VALUE.to_be_bytes());
        q.extend_from_slice(&TPM_ST_ATTEST_QUOTE.to_be_bytes());
        tpm2b(&mut q, b"name");
        tpm2b(&mut q, NONCE);
        q.extend_from_slice(&[0u8; CLOCK_INFO_LEN]);
        q.extend_from_slice(&0u64.to_be_bytes());
        q.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // absurd selection count
        let (qh, sh) = signed(&q, &sk);
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &hex::encode(DIGEST), &qh, &sh),
            Err(TpmQuoteError::MalformedQuote)
        );
    }

    /// The reference encoder + the single-PCR digest bridge round-trip through
    /// the verifier: a quote `marshal_pcr16_quote(nonce, value)` verifies against
    /// the expectation `expected_single_pcr_digest_hex(hex(value))`, and the
    /// quoted `pcrDigest` is `SHA256(value)` (a HASH OVER the value, not the
    /// value itself — the live-flow bridge from the registered PCR16 datum).
    #[test]
    fn tpm_quote_pcr16_reference_encoder_round_trips_through_verify() {
        use sha2::{Digest, Sha256};
        let sk = ephemeral();
        let pcr16_value = [0x9Au8; 32];
        let q = marshal_pcr16_quote(NONCE, &pcr16_value);
        let (qh, sh) = signed(&q, &sk);
        let expected = expected_single_pcr_digest_hex(&hex::encode(pcr16_value)).unwrap();
        // The expectation is the HASH of the value, never the value itself.
        assert_eq!(expected, hex::encode(Sha256::digest(pcr16_value)));
        assert_ne!(expected, hex::encode(pcr16_value));
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), NONCE, &expected, &qh, &sh),
            Ok(())
        );
    }

    /// A reference quote bound to one nonce does NOT verify against a different
    /// nonce — the canonical big-endian nonce encoding is anti-replay-bound.
    #[test]
    fn tpm_quote_reference_encoder_is_nonce_bound() {
        let sk = ephemeral();
        let pcr16_value = [0x9Au8; 32];
        let nonce_a = 0x1122_3344_5566_7788u64.to_be_bytes();
        let nonce_b = 0x8877_6655_4433_2211u64.to_be_bytes();
        let q = marshal_pcr16_quote(&nonce_a, &pcr16_value);
        let (qh, sh) = signed(&q, &sk);
        let expected = expected_single_pcr_digest_hex(&hex::encode(pcr16_value)).unwrap();
        assert_eq!(
            verify_tpm_quote(Some(&pem(&sk.verifying_key())), &nonce_b, &expected, &qh, &sh),
            Err(TpmQuoteError::NonceMismatch)
        );
    }

    #[test]
    fn expected_single_pcr_digest_rejects_non_hex() {
        assert!(expected_single_pcr_digest_hex("not-hex-zz").is_none());
    }
}
