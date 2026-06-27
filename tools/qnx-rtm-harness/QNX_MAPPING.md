# QNX_MAPPING — RTM tracing & QNX cross-compile notes (#271 → #272 / #274)

This document **grounds** each harness fault row against the kernel's real safety
artifacts — `docs/safety/SAFETY_GOALS.md` (AEGIS-SG-001, the `SG-001…SG-016`
definitions) and `docs/safety/REQUIREMENTS_TRACEABILITY.md` (AEGIS-RTM-001, the
`TR-NNN` rows). Those docs are read as **ground truth and are NOT modified** by
this change (#272 touches only `tools/qnx-rtm-harness/**`).

> **The local `SG-0N` is the HARNESS ROW INDEX, not a kernel RTM id.** The
> `rtm_id`/`tr_id` columns are the bridge. The mapping is honest: only **one** row
> has a genuine kernel TR home; **seven** are real coverage gaps; one is a control.
> A shoehorned mapping would be worse than a named gap — the gap IS the evidence
> (it is exactly what `RTM_GAP_REPORT.md` exists to capture).

---

## 1. The grounded mapping table

| Harness row | Injects | Verdict | `rtm_id` | `tr_id` | Disposition |
|---|---|---|---|---|---|
| SG-00 | valid command (no fault) | `Ok` | `CONTROL` | `NONE` | clean-accept control |
| SG-01 | bad header magic | `StaleHeader` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-02 | sequence strictly-lower | `SequenceRegress` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-03 | deadline elapsed | `DeadlineMissed` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-04 | payload CRC mismatch | `PayloadCorrupt` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-05 | payload oversize (bounds) | `PayloadOversize` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-06 | over-envelope velocity | `KinematicLimit` | `SG-001` | `TR-001` | **genuine hit (proxy)** |
| SG-07 | replay (`seq == last`) | `SequenceRegress` | `NO-RTM-ID` | `CANDIDATE` | **gap** |
| SG-08 | torn write (odd generation) | `StaleHeader` | `NO-RTM-ID` | `CANDIDATE` | **gap** |

---

## 2. Per-row justification (one line each, citing the SG/TR text)

- **SG-00 valid → `CONTROL`.** No fault is injected; this is the negative control
  that proves the accept path. It exercises the *accept side* of SG-001's velocity
  envelope (an in-envelope command must pass) but evidences no fault-detection TR,
  so it carries `CONTROL`/`NONE` rather than a forced mapping.

- **SG-06 over-envelope → `SG-001 / TR-001` (the only genuine hit).** SG-001
  ("Velocity Envelope Enforcement") requires that no command with
  `linear_velocity.abs() > max_speed` reach an actuator; TR-001 is the
  `validate_vehicle_command` magnitude check. The judge's
  `|commanded_velocity| > PROXY_MAX_COMMANDED_VELOCITY` rejection is the **same
  safety property**. **Qualified as proxy:** the bound is the harness PROXY
  (`22_350` mm/s), *not* the certified `VehicleKinematicsContract`, and the judge
  **rejects** (`KinematicLimit`) where TR-001 specifies **clamp** (`ClampLinear`).
  So this is *proxy/partial* evidence for SG-001 — it does **not** discharge or
  substitute for TR-001's certified `test_speed_above_ceiling_triggers_clamp_linear`.

The seven **`NO-RTM-ID`** rows below are honest gaps: each names the **principle
precedent** SG (the safety principle that already exists in the kernel for a
*different* artifact) under which a candidate new TR would live, but **no current
TR covers the transport-contract instantiation** the harness exercises.

- **SG-01 bad-magic → `NO-RTM-ID`.** Rejecting a frame whose header magic/version
  is invalid is a transport-framing validity property. **No kernel SG covers frame
  header validity** (verified: `grep` for magic/frame-header in the SG/RTM docs
  returns nothing). Candidate new TR under a *new* EPIC-#270 transport-contract
  safety goal.

- **SG-02 sequence-regress → `NO-RTM-ID`.** Anti-rollback on a per-message
  monotonic counter. The *principle* exists as **SG-014** ("Federation Report
  Replay Prevention": reject `generation <= last accepted`), but TR-014 is
  `reconcile_reports` on `FederatedTrustReportV2` — federation reports, **not** the
  message bus. Candidate new TR extending the SG-014 anti-rollback principle to the
  transport contract.

- **SG-03 deadline-missed → `NO-RTM-ID`.** Per-message freshness/deadline. The
  *principle* is the staleness-fail-closed family — **SG-003** (sensor timeout) and
  **SG-005** (posture-cache staleness: "never evaluate a command against a stale
  cache"). But TR-003 is the telemetry watchdog and TR-005 is the posture-cache
  TTL; **neither is a per-message frame deadline**. Candidate new TR under the
  SG-003/SG-005 staleness family.

- **SG-04 payload-corrupt → `NO-RTM-ID`.** Payload integrity via CRC. The
  *principle* is integrity/tamper detection — **SG-010** ("Audit Chain Tamper
  Detection", SHA-256 `prev_hash`). But TR-010 is the *audit-log chain*, a
  different artifact from an in-transit message payload. Candidate new TR under the
  SG-010 integrity principle for message-payload integrity.

- **SG-05 payload-oversize → `NO-RTM-ID`.** Bounds rejection at the FFI boundary —
  a memory-safety/robustness property of the shim (it short-circuits and never
  crosses into the judge). The evidence home for FFI-boundary isolation is
  `docs/safety/OCCY_FFI_EVIDENCE.md` (Freedom-From-Interference, communication
  isolation) and `OCCY_DFA.md`, **not a kernel SG/TR**. Candidate new TR under an
  FFI-robustness / freedom-from-interference goal.

- **SG-07 replay → `NO-RTM-ID`.** Replay (`sequence == last_accepted`) is the equal
  case of the same SG-014 anti-replay principle as SG-02; same finding — federation
  TR-014 is not the message bus. The nearest *existing command-path* replay
  requirement is **TR-016b** (REQUIREMENTS_TRACEABILITY.md:179 — the DDS bridge
  shall hold no sequence/history cache that would let stale commands replay to
  reconnecting subscribers). SG-07 still does not trace to it: TR-016b verifies the
  **absence** of a replay-enabling cache in the bridge (code inspection), whereas
  SG-07 verifies **active rejection** of a replayed frame at the judge — they are
  **complementary barriers** (don't-cache vs do-reject), and neither substitutes for
  the other. Candidate new TR (alongside SG-02) remains needed.

- **SG-08 torn write (odd generation) → `NO-RTM-ID`.** A snapshot caught with an
  **odd** generation (a write in progress) — or a generation that changes across
  the copy — is rejected by the shim's **odd/even generation seqlock** (HVCHAN-001
  §3 steps 2-3) before the FFI, so the judge is never called (a shim-side
  short-circuit, like SG-05). This replaces the prior compiler-fence-only
  double-read-compare (review finding **S2**): the seqlock uses a real CPU acquire
  barrier and a monotonic counter, so it is sound on weakly-ordered targets and
  not ABA-prone. The mechanism mirrors `crates/kirra-contract-channel`. No kernel
  TR exists for cross-partition snapshot coherence — candidate new TR under the
  same FFI-robustness / freedom-from-interference goal as SG-01.

---

## 3. Coverage gaps surfaced (the held line)

The harness traces **9 transport-contract fault classes**; against the **current**
kernel RTM, exactly **one** (over-envelope → SG-001/TR-001, proxy) has a real TR
home. The other **seven** are genuine **`NO-RTM-ID`** gaps:

| Gap | Fault class | Principle precedent | Candidate new TR home |
|---|---|---|---|
| 1 | frame header validity (bad magic) | — (none) | new transport-contract SG (EPIC #270) |
| 2 | message sequence monotonicity | SG-014 (federation anti-rollback) | transport anti-rollback TR |
| 3 | message deadline freshness | SG-003 / SG-005 (staleness family) | per-message deadline TR |
| 4 | message payload integrity (CRC) | SG-010 (audit-chain integrity) | payload-integrity TR |
| 5 | FFI payload bounds (oversize) | OCCY_FFI_EVIDENCE / OCCY_DFA (FFI) | FFI-robustness TR |
| 6 | message replay (`seq == last`) | SG-014 (federation replay) | transport replay TR |
| 7 | snapshot coherence (torn write / odd generation) | OCCY_FFI_EVIDENCE / OCCY_DFA (FFI) | snapshot-coherence / seqlock TR |

This is the finding, not a defect of the harness: the EPIC #270 iceoryx2/QNX
**transport-contract** surface (framing, ordering, freshness, integrity, bounds,
snapshot coherence) is **not yet represented in the kernel RTM**, which was
authored for the HTTP verifier + governor + protocol adapters. Surfacing these
seven gaps is the point.

> **Adding these candidate TRs to the RTM itself is a follow-up `docs/safety/**`
> change requiring its own safety review — it is explicitly NOT part of this PR.**
> #272 only traces the harness to the RTM as it stands today; it does not modify
> the RTM or the gap report.

---

## 4. CSV format — PROPOSED (no per-test table in the gap report to match)

`docs/safety/RTM_GAP_REPORT.md` has **no per-test CSV evidence table** to conform
to — its tables are markdown (`goal | ASIL | description | tests`) with **no timing
columns**. Per #272's instruction, the harness therefore emits a **proposed**
format (and labels it "proposed" in the output):

```
harness_row,rtm_id,tr_id,fault_class,verdict_expected,verdict_observed,pass,p50_ns,p99_ns,max_ns,wcet_status
```

- `harness_row` — the **local** index (`SG-0N`), never presented as an RTM id.
- `rtm_id` / `tr_id` — the kernel-RTM bridge (`SG-001`/`TR-001`, or
  `NO-RTM-ID`/`CANDIDATE`, or `CONTROL`/`NONE`).
- `wcet_status` — constant **`TBD-QNX-TARGET`**: host timing is never WCET (§6).

If a per-test CSV schema is later defined in the gap report, the column order here
should be re-aligned to it (a one-line `printf` change).

---

## 5. The concern split (recap)

- **C++ shim = driver** — memory/transport safety: generation-seqlock tear detection,
  bounds (oversize short-circuits, never crosses the FFI), CRC over the payload.
- **Rust judge = checker** — the contract verdict (magic → sequence → deadline →
  integrity → kinematic) on the shim's stabilized snapshot.

This is **ADR-0006 Clause 3**'s documented C/C++ integration boundary — no longer
the governor hot path. (The mapping in §1 reflects it: the two shim-side rows
SG-04/SG-05 never reach the judge — visible as the fast p50 on SG-05.)

---

## 6. QNX cross-compile + on-target run (#274)

**Status: cross-build wired AND run on a QNX 8.0 x86_64 target.** `run_qnx_fdit.sh`
+ `qnx.toolchain.cmake` + the gated CMake path cross-build the `no_std` judge
(`x86_64-pc-nto-qnx800`, core-only via `cargo -Zbuild-std=core`, **no QNX std**)
and the C++ shim/harness/`wcet_measure` (`q++`, QCC 12.2.0). The recipe below is
realized by the driver; see §6.1 for the executed result.

The host build uses native `g++` + `rustc`. For a QNX 8.0 target:

- **C++**: a CMake toolchain file points at `q++` / `qcc` (the QNX SDP compilers);
  the `-fno-exceptions -fno-rtti` discipline already matches a
  freestanding-friendly build.
- **Rust judge**: build the `no_std` staticlib for the QNX target tuple
  `x86_64-pc-nto-qnx800` (or `aarch64-unknown-nto-qnx800`), keeping
  `-C panic=abort`. The judge is already `no_std` + zero-alloc + integer-only, so
  it links `core` + a custom-built `compiler_builtins` only — the #189 QNX-std
  blocker does not apply.
- **FDIT/WCET**: re-run the harness on the target under **FIFO scheduling**. This
  is **#274**.

See `docs/adr/KIRRA_QNX_CROSSCOMPILE.md` and `docs/safety/WCET_QNX_BRINGUP.md` for
the full recipe + acceptance criteria.

### 6.1 On-target result — Phase-I feasibility (QNX SDP 8.0 x86_64 VM)

Executed on a QNX 8.0 `mkqnximage`/QEMU x86_64 VM (the binaries baked into the IFS,
auto-run at boot). **The cross-compiled matrix passes byte-identically on QNX:**

```
GATE (verdict correctness): PASS          # all 9 rows verdict_observed == verdict_expected
```

This satisfies the **verdict-correctness** acceptance for Phase-I (cross-compilation
changed not one verdict — SG-00..SG-08 all PASS on the nto-qnx800 binary, including
the corrected replay rule SG-07 → `SequenceRegress` and the seqlock torn-write
SG-08 → `StaleHeader`).

**Timing on this run is INDICATIVE ONLY — the VM ran under QEMU TCG (software
emulation), NOT KVM** (the dev laptop has VT-x disabled in firmware). A representative
`SCHED_FIFO` WCET requires KVM (near-native) or hardware; under TCG the absolute
numbers carry full interpreter + VM-deschedule overhead (observed: median ≈ 3.6 µs,
p99.9 ≈ 10.6 µs, **max ≈ 2.2 ms** — the millisecond max is emulation jitter, not the
judge). So the `max < 100 µs` criterion is **deferred** to a KVM/hardware run; the
typical-case single-digit-µs figure even under heavy emulation is feasibility-positive.

## 7. WCET — Phase-I indicative, cert-grade TBD

The on-target run (§6.1) produced a real QNX-`SCHED_FIFO` row, but **under TCG
emulation it is INDICATIVE, never a WCET**. The harness CSV still carries the
`TBD-QNX-TARGET` discipline for the certified figure. Cert-grade WCET is **Phase-II**:
NVIDIA DRIVE (Orin/Thor) + QNX OS for Safety + Ferrocene-qualified Rust under FIFO —
that number, not a VM figure, backs the FTTI claim. The PASS gate remains **verdict
correctness**, which is now demonstrated on a real QNX target.
