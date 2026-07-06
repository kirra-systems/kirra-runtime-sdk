# Measured-boot attestation (Gate C #3)

Rooting node trust at BOOT, not runtime: a node proves — with a TPM the node cannot
lie to — that it booted the expected, unmodified software chain. This is the third
Gate-C criterion ("secure-boot-rooted attestation on at least one reference
platform"). This doc describes the design, what is **scaffolded in-tree today**, and
the **on-device steps** that remain when the Jetson Orin is flashed.

---

## 1. Why the self-report path isn't enough

`attestation::verify_attestation_proof_with_pcr16` binds a node's *self-reported*
PCR16 digest under its AK signature (issue #73 follow-up). That authenticates the
digest — but a node **in control of its AK** could still sign a false digest. The
value is only as trustworthy as the key, and a software AK offers no boot-time root.

The fix is a genuine TPM2 **quote**: the TPM signs a `TPMS_ATTEST` structure over the
**live PCR bank** using an Attestation Key that is *resident in and non-extractable
from* the TPM. The node cannot forge the measured-boot state, because it never holds
the signing key and never sees the PCRs except through the TPM.

## 2. The trust chain (what the verifier checks)

`attestation_quote::verify_measured_boot_quote(ak_pem, quote, sig, nonce, expected)`
enforces, fail-closed at every step:

1. **AK present** — the node has a registered `ak_public_pem` (else `NoRegisteredKey`).
2. **Quote signature** — `sig` verifies over the whole `TPMS_ATTEST` blob against the
   AK (else `SignatureInvalid`). *The TPM signed the actual PCR state.*
3. **Genuine quote** — magic `TPM_GENERATED_VALUE` + type `TPM_ST_ATTEST_QUOTE`, and
   the structure parses (else `MalformedQuote`).
4. **Freshness** — the quote's `extraData` carries THIS challenge's nonce (8-byte
   big-endian), so a captured old quote can't be replayed (else `QuoteNonceMismatch`).
5. **Measured-boot state** — the quoted PCR-composite digest equals the node's
   registered golden value `expected_pcr16_digest_hex` (else `Pcr16Mismatch`).
   Comparing the composite transitively enforces both the PCR *values* and the
   *selection* (a different selection ⇒ a different composite).

## 3. What is scaffolded in-tree today

- `src/attestation_quote.rs` — the `TPMS_ATTEST` wire-format parser (TPM 2.0
  §10.12.8, big-endian), the magic/type checks, the `extraData`→nonce binding, and
  the composite-digest comparison. All fail-closed, panic-free (bounds-checked
  reader), and exercised end-to-end by unit tests (valid quote, wrong signature,
  wrong measured-boot state, replayed nonce, non-TPM blob, truncated blob).
- `AttestationError::{MalformedQuote, QuoteNonceMismatch}` reason codes.

This is a **ready-to-wire verifier + its evidence** — it is NOT yet called from the
live `/attestation/verify` route (that needs nodes that actually produce quotes).

## 4. The one on-device seam: the quote signature algorithm

The scaffold verifies the quote signature with the existing **Ed25519** AK model, so
it is testable now and consistent with the rest of `attestation`. A production TPM AK
is typically **RSA-2048** or **ECC-P256**. Wiring that in is a single call-site swap
inside `verify_quote_signature` — the parser, the nonce binding, and the digest check
are algorithm-independent and unchanged. (Add the RSA/ECDSA verifier dependency, map
the AK PEM to that key type, and verify over the same `quote_bytes`.)

## 5. On-device integration steps (when the Orin is flashed)

1. **Secure Boot + dm-verity** (per `docs/ota/ROOTFS_AB_DESIGN.md §5`): enroll keys so
   the Orin verifies the signed boot chain and the rootfs Merkle root. This is what
   makes PCR16 *meaningful* — it measures an image that can't be silently altered.
2. **Enroll the AK + golden PCR16**: register the node's TPM AK public key
   (`ak_public_pem`) and the golden measured-boot composite digest
   (`expected_pcr16_digest_hex`) captured from a known-good boot, via
   `POST /attestation/identity/register` + the node record.
3. **Node produces the quote**: on challenge, the node runs the TPM quote (e.g.
   `tpm2_quote -c <ak> -l sha256:16 -q <nonce>`), returning the `TPMS_ATTEST` blob +
   signature; the nonce is the challenge nonce as qualifying data.
4. **Wire the verifier**: call `verify_measured_boot_quote` from
   `attestation::verify_attestation` (the `/attestation/verify` handler) when a node
   submits a quote, alongside/instead of the self-report PCR16 path. Fail-closed:
   a node enrolled with a golden value MUST present a valid quote to be `Trusted`.
5. **Extend to PCR16 quote in the trust decision** (`CRITICAL SECURITY INVARIANT #3`):
   once quotes are live, the default enrolled path requires a measured-boot quote —
   completing "secure-boot-rooted attestation" for Gate C.

## 6. Status

- **Live (tested):** the quote verifier + parser + fail-closed trust chain (§2–§3).
- **On-device follow-up:** the signature-algorithm swap (§4) and steps §5.1–§5.5 —
  all require the flashed Orin with Secure Boot + a provisioned TPM AK.
