# Hypervisor Contract-Channel — Layout + Trust-Chain Specification

| Field | Value |
|---|---|
| Doc ID | **KIRRA-OCCY-HVCHAN-001** |
| Status | **DRAFT — pending formal safety-engineer review** |
| Date | 2026-06-11 |
| Deciders | Project owner (design); a human safety engineer is the review gate |
| Issues | #278 (this spec — the design half); #279 (the fault-injection campaign that tests it); EPIC #270 |
| Builds on | ADR-0006 Clause 2 (frozen-layout partition boundary); ADR-0004 (independent safety channel) |
| Evidence | `tools/iceoryx2-spike/README.md` (the #273 host spike) |

> # ⚠️ DRAFT — design intent, not validated behavior
>
> This document **specifies** the guest↔host governor contract channel that
> ADR-0006 Clause 2 promises. It is the artifact that **hardware day implements**
> (when #274 clears) — not designs. **Every timing and behavioral claim here is
> DESIGN-INTENT until the #274/#278 hardware work measures it on the QNX 8.0
> target.** A human safety engineer must review and validate this spec before it
> is treated as authoritative (the same gate as the UL4600 / TARA / angular-SOTIF
> drafts).

---

## 1. Scope and non-goals

**In scope.** The **guest↔host governor channel only**: the path by which an
Autoware/ROS 2 **guest partition** delivers planner outputs into the
**QNX-resident governor partition** for read-only validation, and by which the
governor's approval is bound to an actuator release.

**Non-goals (each owned elsewhere):**
- **In-partition transport** — iceoryx2 inside each partition (ADR-0006
  **Clause 1**). Not this channel.
- **The C/C++ integration boundary** — DDS bridges / vendor stacks (ADR-0006
  **Clause 3**). Not this channel.
- **On-target validation** — every timing/behavior claim in this spec is
  **design-intent** until #274/#278 measures it. The numbers and bounds named
  here are *requirements to be verified*, not results.

The architectural premise (ADR-0006 Clause 2): across the partition boundary the
transport is **not** a library — it is a **frozen, versioned, fixed-size layout
over hypervisor shared memory**. A native endpoint in the safety partition would
import discovery, lifecycle, loan management, pools, ownership transitions,
recovery, and version compatibility into the trusted computing base; a frozen
layout imports a **struct definition**.

---

## 2. The layout

The channel is a single fixed-size region whose bytes are interpreted as
`GovernorContractView`. It is **`#[repr(C)]`, fixed-size, and pointer-free**.

### 2.1 Deliberate divergence from the harness `KirraContractView`

The #271 `qnx-rtm-harness` view (`KirraContractView`, forthcoming) carries the
command **by pointer** — correct for a *same-address-space C FFI* call, where the
caller and judge share one address space. **`GovernorContractView` carries the
command BY VALUE** (a fixed-size array), because **pointers do not cross a
partition boundary**: a guest-partition pointer is meaningless — and a forgeable
attack surface — in the governor partition's address space. This by-value vs
by-pointer difference is the single deliberate, load-bearing divergence between
the in-partition harness view and the cross-partition contract view.

### 2.2 Field layout (design intent)

```rust
/// Cross-partition governor contract view. FROZEN once certified (see §2.4).
/// `#[repr(C)]`, fixed size, pointer-free; mapped READ-ONLY into the governor
/// partition (see §5). All multi-byte integers are little-endian on the wire.
#[repr(C)]
pub struct GovernorContractView {
    // --- version-discovery prefix (STABLE across ALL layout versions) ---
    /// Layout version. Writer: guest publisher. Checker: governor, FIRST, before
    /// any other field is interpreted — a mismatch ⇒ reject (§4). Held at
    /// **offset 0** deliberately so a reader can always locate it regardless of
    /// how later fields evolve (a versioned-prefix discipline, like a file
    /// magic). This offset-0 stability is the one justified exception to the
    /// otherwise widest-first ordering below.
    pub layout_version: u32,
    /// Fixed channel sentinel (a constant). Writer: guest. Checker: governor —
    /// a wrong sentinel is a gross-corruption / wrong-region signal ⇒ reject.
    /// Pairs with `layout_version` to fill an 8-byte boundary (no padding hole).
    pub magic: u32,

    // --- widest-first body (natural alignment; NOTHING is packed) ---
    /// Monotonic write generation; the seqlock counter (§3). Writer: guest
    /// publisher (odd while writing, even on commit). Checker: governor, read
    /// before and after the snapshot copy.
    pub generation: u64,
    /// Monotonic command sequence. Writer: guest. Checker: governor judge —
    /// `sequence <= last_accepted ⇒ reject` (replay/regress; §3).
    pub sequence: u64,
    /// Publication timestamp (nanoseconds, shared monotonic domain — §5).
    /// Writer: guest at commit. Checker: governor (freshness / liveness).
    pub publication_nanos: u64,
    /// Absolute deadline (nanoseconds, same domain) by which the command must be
    /// consumed. Writer: guest. Checker: governor judge — `now > deadline ⇒
    /// reject`.
    pub deadline_nanos: u64,

    /// CRC-32 (IEEE) over `command[..command_len]`. Writer: guest. Checker:
    /// governor edge, AFTER the snapshot is stable, BEFORE the judge (§3).
    pub crc32: u32,
    /// Valid command length in bytes; `> MAX_COMMAND_BYTES ⇒ reject` (bounds).
    /// Writer: guest. Checker: governor edge. Pairs with `crc32` to 8 bytes.
    pub command_len: u32,

    /// The command payload, BY VALUE (see §2.1). Fixed capacity; the command
    /// occupies the first `command_len` bytes. Writer: guest. Reader: governor
    /// judge (only after bounds + CRC pass).
    pub command: [u8; MAX_COMMAND_BYTES],
}
```

- **`MAX_COMMAND_BYTES`** is sized to the command contract (DESIGN-INTENT; a
  small fixed bound, e.g. 64–128 B). It is fixed at the layout version; growing
  it is a new `layout_version`, never an in-place edit.
- **Field order is widest-first** (the `u64` body) for natural alignment with
  **no packing** and no interior padding holes. The `u32`+`u32` pairs
  (`layout_version`+`magic`, `crc32`+`command_len`) each fill an 8-byte boundary.
  **It is naturally aligned, `repr(C)`-matched — nothing is packed** (the #273
  spike corrected exactly this "packed" mislabel once; it is not repeated here).

### 2.3 Who writes / who checks (summary)

Every field's doc-comment above states its **meaning**, its **writer** (always
the guest publisher), and its **checker** (always the governor) — the
talisman-style treatment. The guest is **untrusted for safety**: it writes; the
governor validates every byte and fails closed (§4). No field is trusted because
the guest set it.

### 2.4 Freeze discipline

Once implemented and certified, `GovernorContractView` receives the
**`src/gateway/kinematics_contract.rs` talisman treatment**: **layout stability
IS the safety claim.** The struct is held byte-stable; any change — a field, a
size, an order, `MAX_COMMAND_BYTES` — requires a **new `layout_version`** and a
re-validation, **never an in-place edit**. A reader encountering an unknown
`layout_version` rejects (§4); it never best-effort-parses a layout it was not
built and certified against.

---

## 3. The write/read protocol (normative trust chain)

The seven steps below are **normative**. The closed claim they produce:
**"the governor approved exactly the data represented by this digest, and
actuator release is cryptographically bound to that approval."**

1. **Publisher writes** the payload, `command_len`, `crc32`, `sequence`,
   `publication_nanos`, `deadline_nanos` into the region.
2. **Publisher commits the generation — seqlock discipline (NORMATIVE choice).**
   The channel uses a **generation seqlock**: `generation` is **odd while a write
   is in progress** and **even on commit**. The publisher (a) increments
   `generation` to the next **odd** value, (b) writes all body fields, (c)
   increments `generation` to the next **even** value to publish. *(This is the
   one chosen mechanism; the double-read-compare alternative is **not** used —
   the odd/even seqlock makes an in-progress write directly observable to the
   reader as an odd counter, which the double-read-compare cannot do without an
   extra round, and it is the classic torn-write defense.)*
3. **Governor obtains a stable snapshot.** Read `generation`; if **odd**, a write
   is in progress → retry. If **even**, **copy the whole struct** into governor-
   partition-local memory, then **re-read `generation`**: if it is **unchanged
   and still even**, the snapshot is **coherent**. If it changed (or is odd), the
   copy was torn → retry. Retries are **bounded** (`MAX_SNAPSHOT_RETRIES`,
   DESIGN-INTENT); **retry-exhaustion ⇒ fail-closed reject** (§4). The governor
   validates only its **local copy**, never the live shared region (this, with
   the read-only mapping of §5, is the torn-read defense — and mirrors the #273
   spike's owned-sample discipline).
4. **Validate** the local snapshot in the spike's order: **bounds**
   (`command_len <= MAX_COMMAND_BYTES`) → **CRC** (`crc32(command[..len])`) →
   **contract judge** (`layout_version`/`magic` already checked; then `sequence`
   monotonic, `deadline`, the kinematic/contract checks). Any failure ⇒
   fail-closed reject (§4).
5. **Compute a digest** over the **validated snapshot bytes** (the exact bytes
   the judge approved — not the live region, which may already have moved on).
6. **Release token carries the digest, signed with the EXISTING Ed25519
   machinery.** Reuse the verifier's signing/verification path — `src/attestation.rs`
   (Ed25519 signature verification + the **verify-then-consume** single-use
   discipline: `AppState::consume_challenge` atomically removes a pending entry so
   a replayed token finds nothing) and the hash-chained, signed ledger in
   `src/audit_chain.rs` (`fabric_generation` is already appended as LE bytes under
   `previous_hash`). **No new crypto primitives are introduced.**
7. **Actuator path verifies the digest before release.** The actuator interface
   checks the release token's signature **and** that its digest matches the
   command it is about to actuate. A token whose digest does not match, or whose
   signature does not verify, ⇒ **no release** (fail-closed).

### 3.1 Generation / sequence discipline

`generation` (the seqlock counter) and `sequence` (the command counter) are both
**monotonic; equal-or-older ⇒ reject.** This is:
- the **#273 judge's `sequence <= last_accepted ⇒ reject`** rule (equal = replay,
  lower = regress; `tools/iceoryx2-spike/README.md`), and
- the **#79 epoch fence** discipline used at
  `src/bin/kirra_verifier_service.rs` (`held_epoch` atomic + in-transaction
  epoch re-check; a fenced writer self-demotes and rejects).

`fabric_generation` is **already hash-chained** in `src/audit_chain.rs` — the
release-token digest extends the same forensic-ledger discipline rather than
inventing one.

---

## 4. Failure semantics (every row fail-closed)

Each row names the **detection point**, the **verdict** (always fail-closed), and
the **barrier layer that owns it** — using the **#279 attribution taxonomy**
(hypervisor / contract-discipline / judge), so the fault-injection campaign maps
**1:1** onto this table.

| Fault | Detection point | Verdict | Owning barrier (#279) |
|---|---|---|---|
| `layout_version` mismatch | governor, first check (§2.2) | reject; no parse | contract-discipline |
| `magic` wrong | governor, prefix check | reject (wrong/corrupt region) | contract-discipline |
| Generation **retry-exhaustion** (persistent odd / churn) | snapshot loop (§3 step 3) | reject after `MAX_SNAPSHOT_RETRIES` | contract-discipline |
| CRC fail | governor edge (§3 step 4) | reject | contract-discipline |
| Bounds (`command_len` oversize) | governor edge | reject | contract-discipline |
| Judge reject (deadline / kinematic / contract) | governor judge | reject | judge |
| Stale / equal generation or sequence | governor judge (`<=` rule, §3.1) | reject (replay/regress) | judge |
| Clock skew beyond bound (§5) | governor freshness check | reject | hypervisor / contract-discipline |
| **Publisher silent** (no fresh generation within the watchdog window) | governor liveness watchdog | the existing **sensor-liveness / SG-003** posture (Degraded → MRC) | hypervisor (scheduling) + governor |

- **Publisher-silent liveness** reuses the existing watchdog discipline
  (`src/telemetry_watchdog.rs`, SG-003 sensor-timeout fault detection): no fresh
  **even** generation within the window ⇒ the documented degraded/MRC posture,
  not a stale-data accept.
- **Attribution rule (from #279):** a fault absorbed by the *wrong* layer is a
  **finding, not a pass**. E.g. a publisher flood that survives only because the
  judge is fast means the **hypervisor partition scheduling config** (§5) failed,
  not that the channel is safe. The "owning barrier" column is the claim each
  fault-injection case must confirm.

---

## 5. Hypervisor requirements (configuration requirements, not assumptions)

These are **requirements on the hypervisor configuration** that the channel's
safety argument leans on. They become the **hypervisor-config checklist** that
#279's hypervisor-layer faults test.

- **R-HV-1 — Read-only mapping.** The contract region is mapped **read-only**
  into the governor partition. The governor **cannot** write it; a guest
  **cannot** induce a governor write. Verified at the **hypervisor config level**
  (a #278 "Done when").
- **R-HV-2 — Region size & alignment.** The region is exactly
  `size_of::<GovernorContractView>()`, aligned to the struct's natural alignment;
  the governor maps no more and no less.
- **R-HV-3 — Shared monotonic clock source.** `publication_nanos` /
  `deadline_nanos` are in a **single shared monotonic domain** both partitions
  read — **likely the hypervisor's shared monotonic counter**. *(OPEN: the exact
  source and the skew owner are unresolved until the target is in hand. Resolution
  path: a hypervisor-provided monotonic source read identically by both
  partitions, with a bounded max skew; until resolved, "clock skew beyond bound"
  is the §4 fail-closed row, and an integrator/hypervisor **clock-provision AoU**
  is implied — see §6.)*
- **R-HV-4 — Partition scheduling guarantees.** The governor partition receives a
  scheduling guarantee sufficient to run its bounded snapshot/validate/digest
  path within the FTTI allocation **regardless of guest CPU behavior** — i.e. a
  guest CPU flood or starve-then-burst cannot starve the governor. This is the
  barrier #279's "publisher consumes all guest CPU" / "starve-then-burst" cases
  must hit at the **hypervisor** layer, not the judge.

---

## 6. Traceability hooks

- **Safety-case registration:** registered as **KIRRA-OCCY-HVCHAN-001** in
  `docs/safety/SAFETY_CASE_INDEX.md` (this change).
- **Cross-references:**
  - **ADR-0006** (`docs/adr/0006-governor-transport-iceoryx2.md`) Clause 2 — the
    decision this spec realizes.
  - **ADR-0004** (`docs/adr/0004-independent-safety-channel.md`) — the
    independent-safety-channel frame.
  - **EPIC #270**; **#278** (this design half + the hardware implementation);
    **#279** (the fault-injection campaign — its taxonomy is §4's "owning
    barrier" column).
  - **`tools/iceoryx2-spike/README.md`** — the #273 evidence (seqlock-style owned
    snapshot, `<=` replay rule, transport-eliminates-torn-read).
  - In-repo precedents cited normatively: `src/attestation.rs` (Ed25519
    verify-then-consume), `src/audit_chain.rs` (`fabric_generation` hash-chain),
    `src/bin/kirra_verifier_service.rs` (`held_epoch` #79 fence),
    `src/telemetry_watchdog.rs` (SG-003 liveness),
    `src/gateway/kinematics_contract.rs` (the talisman / freeze precedent).
- **AoU candidates this spec implies (named here, NOT filed):**
  - **Integrator / hypervisor clock provision** — a shared monotonic source with
    a bounded max skew across partitions (R-HV-3). To be filed as an AoU register
    entry when the clock source is resolved on target.
  - **Hypervisor partition-scheduling guarantee** — the governor-partition CPU
    guarantee independent of guest behavior (R-HV-4).
  - **Read-only-mapping enforcement** — the hypervisor config that makes R-HV-1
    a checkable precondition.
