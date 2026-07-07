# Kirra v1.0.0 — TPM Attestation Verification Specification

This document provides the definitive mathematical and structural specification for the Kirra v1.0.0 Trusted Platform Module (TPM) remote attestation architecture. It details the precise tracking mechanics that ensure the control plane relies exclusively on unmockable, cryptographically validated platform state assertions rather than transient network configurations.

---

## 1. Attestation Trust Model

The Kirra security model operates on a zero-trust network posture. Network layer identifiers (such as IP addresses, MAC coordinates, or transport-layer sessions) are treated as hostile, modifiable vectors.

> **Root Security Invariant**: Kirra does not trust network identity. Kirra trusts measured platform state.

Trust is established by building a rigid cryptographic chain of custody from physical silicon registers up to cluster-wide policy enforcement:

```
TPM Hardware Silicon Registers
  → Attestation Identity Key (AIK)
  → Signed Cryptographic Quote
  → PCR16 Runtime State Measurement
  → Verifier Validation Engine
  → FleetPosture Topology Promotion
```

* **Node Identity Binding**: A node's persistent operational identity is securely bound to the unique public component of its asymmetric Attestation Identity Key (AIK), configured during device enrollment.
* **Runtime Legitimacy Binding**: A node's operational integrity is determined exclusively by verifying its runtime software stack against binary measurements inside Platform Configuration Register 16 (`PCR16`). The resulting SHA-256 digest must match the verifier's expected baseline.
* **Temporal Freshness Binding**: Replay immunity is enforced by encapsulating every remote evaluation cycle within an explicit, short-lived, single-use cryptographic challenge-response loop.

---

## 2. End-to-End Verification Sequence

The runtime verification sequence is executed via an atomic transaction chain spanning the edge node, the storage engine, and the runtime state machine.

| Step | Executing Component | Targeted Action Description |
| :---: | :--- | :--- |
| **1** | Edge Client Node | Requests a new temporal challenge token from the verifier endpoint: `POST /attestation/challenge/:node_id`. |
| **2** | Verifier Control Engine | Generates a high-entropy, cryptographically secure random nonce using an underlying CSPRNG pool. |
| **3** | Volatile Memory Cache | Caches the generated nonce inside `pending_challenges`, indexed by `node_id`, and stamps it with an expiration timestamp bound by `CHALLENGE_TTL_MS`. |
| **4** | Client Hardware TPM | Executes an internal `TPM2_Quote` operation, reading the live runtime configuration registers (`PCR16`), embedding the verifier's nonce inside the quote body, and signing the complete block using the private AIK. |
| **5** | Edge Client Node | Submits the structured quote payload, binary signature, and node parameters back to the verifier: `POST /attestation/verify`. |
| **6** | Verifier Control Engine | **DESTRUCTIVELY CONSUMES THE NONCE BEFORE ANY VALIDATION STARTS.** The verifier pops the `node_id` entry from `pending_challenges`. If the nonce does not exist or has expired, processing aborts instantly. |
| **7** | Persistent Data Layer | Pulls the absolute reference AIK public key PEM structure out of the secure, persistent SQLite data table. |
| **8** | Cryptographic Engine | Parses the public key and validates the incoming signature against the immutable byte payload of the raw TPM quote block. |
| **9** | Cryptographic Engine | Parses the validated quote body, extracts the raw binary values of the `PCR16` registers, and passes them through a local SHA-256 calculation. |
| **10** | Validation Engine | Executes a binary comparison matching the freshly calculated SHA-256 digest against the node's registered `expected_pcr16_digest_hex` read out of the database store. |
| **11** | Concurrent Cache | Toggles the active memory cache layer from `NodeTrustState::Unknown` to `NodeTrustState::Trusted`. |
| **12** | DAG Topo Engine | Triggers a recursive graph recalculation traversing the White/Gray/Black dependency pathways affected by the trust state transition. |
| **13** | Edge Proxy Gateway | The updated `FleetPosture` state is fetched during asynchronous polling sweeps, altering global route authorization constraints instantly. |

> **CRITICAL INVARIANT**: Nonce consumption occurs before cryptographic validation. If a quote submission fails verification, its associated nonce is already destroyed. The client cannot attempt alternative signature iterations against the same challenge window.

---

## 3. Replay Protection Guarantees

The verification pipeline implements a multi-tiered defense matrix designed to ensure that compromised historical payloads are rendered cryptographically inert if intercepted by a malicious observer.

* **Single-Use Lifetime Constraint**: A nonce exists for exactly one extraction pass. Once popped from the active `pending_challenges` map during a validation entry request, it cannot be reclaimed, reused, or re-inserted.
* **Volatile Memory Isolation**: Nonce tracking structures are maintained strictly in volatile RAM. They are never written to the persistent SQLite layer, removing the risk of post-crash state residue leakage.
* **Temporal Windows (`CHALLENGE_TTL_MS`)**: Active challenges enforce an uncompromising expiration limit. If a client fails to return a signed quote before the lookback window closes, the memory slot is cleared.
* **Destructive Restoration Cleansing**: Invoking disaster recovery data ingest routines (`POST /system/backup/import`) triggers an instantaneous, global clear of the `pending_challenges` map.
* **Process Lifetime Coherence**: Restarting the verifier process completely invalidates all active challenge windows globally.

```
Historic Attestation Packets + Destroyed/Missing Nonce → Instant 401 Unauthorized / Reject
```

---

## 4. PCR16 Measurement Binding

A valid cryptographic signature only proves that a packet originated from an authorized TPM. Real legitimacy requires proving that the platform is running an uncorrupted software environment. `PCR16` acts as the definitive anchor for runtime state validation.

### Verification Failure Matrix

The verifier asserts five distinct evaluation checks to determine legitimacy. If any condition fails, the system defaults to a fail-closed response:

| Encountered Processing Condition | Structural Result | Imposed State Consequences |
| :--- | :---: | :--- |
| Asymmetric Signature Invalid / Broken | **Untrusted** | Node status set to `Untrusted`. Topological dependencies break instantly. |
| `PCR16` Digest Hash Mismatch | **Untrusted** | Node status set to `Untrusted`. Triggering instant downstream containment. |
| Challenge Nonce Missing from Cache | **Reject** | Aborts processing immediately with HTTP 400. Existing cache states are preserved. |
| Challenge Nonce Expired (`> CHALLENGE_TTL_MS`) | **Reject** | Aborts processing immediately with HTTP 400. Clears the expired challenge entry. |
| Unregistered / Unknown `node_id` | **Reject** | Aborts processing immediately with HTTP 404. No state processing occurs. |

---

## 5. TPM Simulation Evidence

To enable comprehensive integration testing within automated CI/CD environment runners and local engineering workspaces without requiring a physical hardware cryptographic chip attachment, the platform relies on synthetic TPM simulation frameworks.

* **Mock TPM Quote Architecture**: Test harnesses simulate a hardware state environment by generating binary blocks matching the precise schema serialization specifications of true TPM2.0 quotes.
* **Fixed Key Fixtures**: Tests leverage synthetic AIK asymmetric PEM key pairs to sign test assertions, validating that the underlying OpenSSL or Ring cryptographic modules execute accurate verification passes.
* **Synthetic PCR Vectors**: Integration matrices specify realistic `PCR16` register tracking sequences, generating known-good SHA-256 target digests alongside intentionally flawed test entries.

> **Core Testing Invariant**: Mock vectors simulate hardware behavior. They do NOT bypass cryptographic verification logic. The validation code runs identical parsing, signature evaluation, and hash comparison steps regardless of whether it interacts with a physical or a simulated TPM chip.

---

## 6. Posture Promotion Rules

The system maps local node attestation states directly to systemic global authorization metrics through recursive Directed Acyclic Graph (DAG) inheritance passes:

| Verified `NodeTrustState` | Structural Topology State | Evaluated `FleetPosture` Result |
| :--- | :--- | :---: |
| `Trusted` | All dependent parent chains are `Trusted` & healthy | **Nominal** |
| `Trusted` | At least one parent dependency is `Untrusted` / `Unknown` | **Degraded** |
| `Untrusted` | Irrespective of downstream dependency layouts | **LockedOut** |
| `Unknown` | Base state upon fresh registration or system boot | **Degraded** |

```text
  [node-02: Untrusted] ── (Breaks Chain) ──> [node-01: Trusted]
                         │
                         ▼
             Global Posture = Degraded
```

---

## 7. Forbidden Cryptographic Regressions

> **CRITICAL CRYPTOGRAPHIC ENFORCEMENT PROTECTION RULES**

 * **Do not hardcode `NodeTrustState::Trusted`.** Trust status must be earned through successful cryptographic verification on every cycle; static validation short-circuits are forbidden.
 * **Do not skip nonce consumption, and consume it VERIFY-THEN-CONSUME.** A challenge nonce is single-use and is atomically consumed from volatile memory upon a SUCCESSFUL verification (`consume_challenge` runs only after the AK signature, the PCR16 binding, and — when required — the TPM quote all pass). Consuming *before* validation is forbidden: it would burn the nonce on a transient/failed attempt and deny a genuine node its retry. Replay is prevented because a successful verify consumes the nonce atomically (a re-submission finds nothing), and the nonce carries a short TTL and node binding.
 * **Do not persist nonces to SQLite.** Challenges must remain strictly in volatile RAM caches to prevent post-incident state exploitation.
 * **Do not trust unsigned PCR payloads.** Platform measurement configurations must be extracted strictly from inside cryptographically verified TPM quote bodies.
 * **Do not bypass AIK signature validation.** The system must calculate and verify the signature against the actual public components of the registered node record for every incoming quote.
 * **Do not accept expired challenges.** Nonce timestamp deltas must be evaluated stringently against `CHALLENGE_TTL_MS` rules on every pass.
 * **Do not evaluate posture before verification completes.** Dependency hierarchy updates must execute downstream of successful cryptographic and database checkpoint saves.
 * **Do not downgrade Untrusted into Degraded.** If a node actively fails a cryptographic challenge or measurement validation, it must trigger an absolute **LockedOut** isolation directive instantly.

---

## 8. Measured-boot enrollment (WP-16 / MGA G-8)

Enrolling a node as *measured-boot* means registering three things together so
`/attestation/verify` will thereafter demand a genuine hardware TPM quote (a
self-reported PCR16 alone no longer suffices): the node's **AK public key**, its
**expected PCR16 value**, and **`require_tpm_quote = true`**. The register handler
persists the quote policy to `node_attestation_policy` *before* the node record,
so a required-quote node is never live without its requirement (fail-closed).

### 8.1 One-call enrollment — `kirra-ota-ctl enroll`

The node-side CLI does it in one audited POST to `/attestation/register`:

```text
kirra-ota-ctl enroll \
  --verifier https://verifier:8090 --node-id edge-7 \
  --ak-key /var/lib/kirra/ak.pkcs8.pem \   # PKCS#8 Ed25519 private; public half is DERIVED
  --pcr16 $(tpm2_pcrread sha256:16 | ...) \ # the measured-boot PCR16 VALUE (hex)
  --token "$KIRRA_ADMIN_TOKEN"              # /attestation/register is admin-scoped
```

- The **private AK never leaves the node** — `enroll` derives and sends only the
  SPKI public PEM (or accepts one directly via `--ak-pub`).
- `require_tpm_quote` defaults to **true** for `enroll` (it *is* the measured-boot
  path) and is sent EXPLICITLY, so the enrollment is deterministic regardless of
  the verifier's fleet-default gate (§8.2). `--no-require-quote` opts a TPM-less
  node out.
- **swtpm-friendly / sandbox-testable:** the PCR16 value is supplied as an argument
  (read offline via `tpm2_pcrread` against a real TPM or an swtpm), so enrollment
  and the whole challenge→quote→verify path run in CI with no physical chip — the
  quote itself is generated by the canonical `tpm_quote::marshal_pcr16_quote`
  encoder (§5). Provisioning is NOT best-effort: a non-201 is a hard failure.

### 8.2 Fleet default — `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT`

For a fleet that is fully measured-boot, set `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT=1`
on the verifier: a registration that **omits** `require_tpm_quote` then defaults to
quote-required. An EXPLICIT request field always wins (a genuinely TPM-less node can
still register `require_tpm_quote: false`), and the gate defaults **off** so an
un-opted deployment is byte-identical to prior behaviour. The decision is the pure
`resolve_require_tpm_quote(explicit, fleet_default)` (`explicit.unwrap_or(default)`),
unit-tested without `set_var`.

### 8.3 What remains external (hardware track)

The verifier-side enforcement, the enrollment CLI, and the fleet default are the
software half. The hardware track (per MGA §4 G-8) is the on-device rooting: Orin
Secure Boot + dm-verity + a provisioned TPM AK (the quote signature is Ed25519
today; a real TPM AK moves it to RSA/ECC), and enrolling the AK+PCR16 against each
node's physical TPM at manufacture.
