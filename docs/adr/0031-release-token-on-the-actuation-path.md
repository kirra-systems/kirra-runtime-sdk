# ADR-0031: The release-token crypto rides the actuation path — never inside the verdict WCET

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-07-02 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG3** (per-command kinematic envelope — the release token is the authenticated *proof* of the bounded command, ADR-0013); FTTI budget integrity (the verdict gate stays crypto-free and bounded) |
| Cross-refs | ADR-0030 (Clause F; its *Update 2026-07-02* Phase-I evidence section **lands in PR #766**); ADR-0013 (authenticated actuation gate); `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` §3 steps 5–7; `crates/kirra-release-token` (the canonical token); `kirra_core::contract_consumer::decide_cycle`; `kirra_core::contract_consumer::GovernorCycle::view_to_sign()`; `src/wcet_gate.rs` (`GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100`, p99.9 gate); the measurements: **PR #766** (`crates/kirra-l3-e2e`, results `qnx800-x86_64-vm-kvm.txt`, INDICATIVE-KVM) |

## Context — the measured problem

ADR-0030's Phase-I run (PR #766) put target-side numbers on the L3 release path, on QNX 8.0
(`x86_64-pc-nto-qnx800`, KVM VM, INDICATIVE — the *ratios* are the takeaway):

| path | p50 | p99 | p99.9 |
|---|---|---|---|
| `decide_cycle` (read+validate+decode+bound), FIFO | 0.69 µs | 0.77 µs | **1.1 µs** |
| `issue_release_token` (Ed25519 sign, governor) | 31.6 µs | 42 µs | 63 µs |
| `verify_release` (Ed25519 verify, actuator) | 59.6 µs | 74 µs | 88 µs |

Two facts follow:

1. **The verdict pipeline is ~1% of its budget.** The whole transport+validate+decode+bound
   step sits at ~1.1 µs p99.9 against the 100 µs `GOVERNOR_VERDICT_WCET_TARGET_MICROS`.
   The carrier and the checker are not the cost.
2. **The token pair (~91 µs p50, sign+verify) is ~99% of the release path and cannot ride
   inside the verdict target** — at p99 (42+74 ≈ 116 µs) it alone exceeds 100 µs. Any
   accounting that folds the crypto into the verdict WCET breaks the gate on this CPU class.

The question this ADR decides: **where is the release-token crypto budgeted, and what
changes if it ever stops fitting?**

## Decision

Five clauses.

### Clause A — budget split (NORMATIVE)

The FTTI decomposition is `verdict_WCET + actuation_latency < control_cycle (≪ FTTI)`.

- **The verdict budget (`GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100`, p99.9) covers the
  verdict pipeline ONLY** — `consume_and_bound` / `decide` / `decide_cycle`
  (snapshot → validate → decode → kinematic bound). It is and stays **crypto-free**.
  The WCET gate (`src/wcet_gate.rs`) and any future on-target verdict-WCET claim must
  never include token issuance.
- **The release token (sign governor-side, verify actuator-side) is accounted to the
  actuation-latency leg.** It executes strictly AFTER the verdict returns and before
  physical release — it can delay actuation of an *approved* command, but it can never
  delay the *decision* (and therefore never delays an MRC: a SafeStop needs no token).

### Clause B — per-command tokens are retained (and are affordable)

At the 100 Hz control cycle (10 ms), the measured pair costs ~91 µs p50 / ~116 µs at
p99-sum (42+74) / ~151 µs at p99.9-sum (63+88) — **~1–2 % of the cycle**; even the VM's
worst-case outliers (340–465 µs, KVM jitter) are <5 %. Per-command, per-cycle tokens
therefore fit with an order-of-magnitude
margin on this CPU class. **No weakening of the per-command binding is needed or taken**:
every actuated command carries a token over its exact enforced bytes
(`view_to_sign` → `issue_release_token` → `verify_release`).

### Clause C — rejected alternatives (and why)

- **Folding the token into the verdict WCET** — arithmetic above; also conceptually wrong:
  the token is an *authorization artifact* of an already-made decision, not part of making it.
- **Reduced-rate / epoch tokens** (sign every Kth cycle or per time window) — rejected.
  A token that covers a window re-opens a substitution surface inside that window, and the
  claim degrades from "the governor approved exactly these bytes" to "…some bytes this
  epoch." That claim is the whole point of HVCHAN §3.5–7.
- **Dropping authentication on the release hop** (CRC-only) — rejected outright; the CRC
  is integrity against corruption, not authenticity against a compromised guest.

### Clause D — the designed escalation: Ed25519-bootstrapped session MAC (specified now, built on trigger)

If the token pair ever stops fitting, the escalation is **not** rate reduction — it is a
cheaper per-command primitive with the same binding:

- **At boot / re-key**: the governor generates a session key and signs it (plus epoch +
  identities) with its **existing Ed25519 identity**; the actuator verifies once. Asymmetric
  crypto stays where it is cheap — identity and bootstrap.
- **Per command**: the token's signature field carries an **HMAC-SHA256** (already in the
  tree; no new primitive) over the SAME domain-separated contract digest, keyed by the
  session key. Measured HMAC cost on this class of hardware is ~1–2 µs — two orders under
  Ed25519 — restoring the ~99 % headroom.
- **The trade, stated honestly**: a symmetric MAC keeps *integrity + authenticity* inside
  the vehicle TCB but loses per-command *non-repudiation* (either key-holder could mint).
  Forensic non-repudiation is retained at the session level (the Ed25519-signed key
  attestation) and via the audit chain. Acceptable for the governor→actuator hop, whose
  threat is the compromised guest, not a dishonest governor.

**Triggers** (any one): the control rate rises to ≥ 1 kHz; measured token p99 exceeds 10 %
of the control cycle on deployment hardware; or the deployment CPU class measures
materially slower than this reference. Until a trigger fires, Clause B stands.

### Clause E — hardware crypto is NOT the default answer

TPM / secure-element offload (Phase-II) is noted and deliberately not selected: discrete
secure elements typically cost *milliseconds* per operation on 96-byte payloads — worse
than software Ed25519, far worse than the Clause-D MAC. It becomes interesting only for
key custody (the governor signing key living in hardware), which composes with Clause D
(the session-key signature is the rare, offloadable operation). Adopt only with measured
numbers on the deployment SoC.

**Key-custody seam (landed).** The *provisioning* half of Clause E — deciding **where**
the governor signing key comes from — is now a fail-closed seam,
`kirra_release_token::provisioning` (see `docs/safety/GOVERNOR_KEY_PROVISIONING.md`).
`KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` selects `file:<path>` (a permission-checked, zeroized
32-byte seed) or `dev-fixed` (the well-known harness key, admitted ONLY under
`KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV`); an unset/misconfigured source **refuses** rather
than minting an unpinnable key. `tpm:<handle>` is wired but **deferred** — it returns
`TpmUnsealUnsupported` until tss2 libs + hardware land, so a deployment can *name* the TPM
source without it ever silently degrading to a weaker one. The `kirra-l3-e2e` harness now
draws its fixed key through this seam (`dev-fixed`, `allow_dev`), proving a live caller.

## Measurement standing

`crates/kirra-l3-e2e` (PR #766) is the standing measurement harness — its
`issue_release_token` / `verify_release` timing rows re-validate this ADR's arithmetic on
every future target (Orin, QNX Hypervisor hardware). The budgets above are re-checked, not
assumed, per target class; the recorded run is INDICATIVE-KVM and the certified figures
are Phase-II hardware under FIFO (#274 discipline).

## Consequences

- The verdict gate keeps its clean, crypto-free 100 µs claim; no change to `wcet_gate.rs`.
- The governor call-site (ADR-0030 Clause D) orders its cycle: `decide_cycle` → (if
  actuatable) `issue_release_token` → hand off; the actuator orders `verify_release` →
  physical release. A SafeStop bypasses the token entirely — fail-closed costs nothing extra.
- Integrators get a stated, measurable feasibility envelope (Clause B) and a pre-designed
  escalation (Clause D) instead of an ad-hoc redesign under schedule pressure.

## Conditions that reopen this decision

- A Clause-D trigger fires (rate/cycle-share/CPU-class) → implement the session MAC.
- Deployment hardware measurements contradict Clause B's arithmetic (e.g. the p99.9 token
  cost exceeding ~10 % of the cycle even at 100 Hz).
- A certification requirement demands per-command non-repudiation → Clause D's MAC is
  insufficient; revisit with hardware key custody (Clause E) in the loop.
