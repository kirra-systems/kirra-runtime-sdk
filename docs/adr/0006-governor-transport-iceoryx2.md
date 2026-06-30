# ADR-0006: Governor transport — iceoryx2 inside partitions; frozen-layout contract at the partition boundary; FFI demoted to integration boundary

| Field | Value |
|---|---|
| Status | **Accepted (direction)** — see *Conditions that reopen this decision* (evidence strengthened 2026-06-30; reopening conditions remain open) |
| Date | 2026-06-11 (updated 2026-06-30 — see §*Update 2026-06-30*) |
| Deciders | Project owner |
| Issues | #275 (this ADR); EPIC #270; evidence #273 / PR #277; **production carrier + latency PR #736**; validating condition #274; support posture #276; boundary design #278 |
| Doc | `tools/iceoryx2-spike/README.md` (the #273 host spike; **PR #736 adds** the frozen-contract carrier & latency bench); `docs/adr/KIRRA_QNX_CROSSCOMPILE.md` (toolchain notes) |
| Builds on | ADR-0004 (independent safety channel / doer–checker) |

## Context

KIRRA is a fail-closed safety governor whose argument rests on an **independent
safety channel** (ADR-0003/0004): it validates the *output* of an
integrator-supplied AI/perception stack it does not build. The intended target
is a **QNX-resident governor** alongside a guest (Autoware / ROS 2) running under
a hypervisor on the same SoC.

Historically the transport to the governor was reached through a **C++** shim
(classic Eclipse iceoryx is C++), which put an `extern "C"` boundary — a raw
pointer dereference inside `unsafe` — on the command path. Two things changed:

- **iceoryx classic is in maintenance**; **iceoryx2** (Rust, daemon-less) is the
  recommended implementation. v0.7.0 added QNX 7.1 as a tier-3 open-source
  platform (tier-1 via the commercial **ekxide** engagement, #276).
- A Rust subscriber edge makes the governor hot path **Rust end-to-end** — judge
  and driver as ordinary Rust modules — coherent with the QNX + **Ferrocene**
  certification stack.

The #273 host spike (`tools/iceoryx2-spike/`, PR #277) was built to supply
concrete evidence for this decision; its findings are cited below.

## Decision

Three clauses, each load-bearing and **distinct**:

### Clause 1 — INSIDE a partition, the transport is iceoryx2

Within each partition, iceoryx2 is the transport: the guest side via
`rmw_iceoryx2` as it matures (Autoware / ROS 2); the governor-side host
processes via **native Rust iceoryx2**. Rationale: classic is
maintenance/superseded; the hot path is Rust end-to-end; the #273 spike
demonstrated a real zero-copy pub/sub edge with the no-FFI judge as an ordinary
function call.

### Clause 2 — ACROSS the guest↔host partition boundary, the transport is NOT iceoryx2

The cross-partition boundary is a **frozen, versioned, fixed-size layout over
hypervisor shared memory** — *not* a native iceoryx2 endpoint in the safety
partition (#278's hypothesis, **"the contract is the layout, not the library"**).

Certification-scope rationale: a native endpoint in the safety partition imports
**discovery, lifecycle, loan management, memory pools, ownership transitions,
recovery, and version compatibility** into the trusted computing base; a **frozen
layout imports a struct definition.** This mirrors the in-repo precedent of the
`src/gateway/kinematics_contract.rs` **talisman** — layout stability *is* the
safety claim, and the artifact is held byte-stable rather than re-derived.

### Clause 3 — the C ABI / FFI layer is demoted to the integration boundary

The C ABI / FFI is retained **only** as the documented integration boundary for
C/C++ components (DDS bridges, vendor stacks). It is **no longer the governor hot
path**.

## Evidence (from the #273 spike — `tools/iceoryx2-spike/README.md`)

- **Minimal feature subset = EMPTY.** On host, iceoryx2 0.9.1 compiles **and**
  runs the zero-copy pub/sub + subscriber-lifecycle path with
  `default-features = false` and nothing re-added (`std` and `console` both
  droppable). This is the input to the #274 QNX 8.0 `--no-default-features`
  check.
- **TornHeader eliminated by construction.** The publisher writes into an
  **exclusively-loaned** slot and `send()` publishes an **immutable** sample; the
  subscriber's `receive()` returns an **owned** sample over a stable, not-yet-
  recycled slot. The application never double-reads a live, mutating buffer — a
  fault class the transport *removes* (not merely catches). `transport-
  eliminates-X` is durable safety-argument evidence.
- **No-FFI / no-unsafe hot path, COMPILER-ENFORCED.** The spike carries
  `#![forbid(unsafe_code)]`; the judge is an ordinary function call on a typed
  `&CommandFrame`. The fault matrix is green in **both** feature configurations.
- **Replay / regress discipline.** The judge rejects on
  **`sequence <= last_accepted`** (equal = replay, lower = regress; strictly-
  newer passes) — the corrected rule, proven red/green. This is also the
  **generation rule** for the #278 cross-partition channel, and aligns with the
  durable **epoch fence (#79)** used elsewhere in the system.

## Update 2026-06-30 — production frozen-contract carrier, latency evidence, QNX-KVM progress

Three developments since the original decision **strengthen the evidence base**.
They do **not** discharge the reopening conditions below (which remain target- and
toolchain-gated), so the Status stays *Accepted (direction)*.

### 1. The carrier now rides the PRODUCTION frozen contract (PR #736)

The #273 spike carried an ad-hoc `CommandFrame`. **In PR #736**, the new
`tools/iceoryx2-spike/src/frozen.rs` publishes the **production**
`kirra_contract_channel::GovernorContractView` (the
frozen 176-byte `#[repr(C)]`, by-value image — Clause 2's layout) over a real
iceoryx2 zero-copy channel and validates every received owned sample with the
**production** `kirra_contract_channel::validate()`. All nine transport-contract
fault classes (layout-version, magic, bounds, CRC, sequence-regress, replay,
generation-regress, deadline, valid) are green over the live channel, on the full
**and** minimal (`--no-default-features`) iceoryx2 configs. This demonstrates Clause 1
with the real contract, not a spike-local copy.

**Isolation held (the #275 gate, reaffirmed):** the dependency direction is
spike → `kirra-contract-channel` (a path dep on the lean, `no_std`, zero-dep,
`#![forbid(unsafe_code)]` core), never the reverse — iceoryx2 still **never** enters
the SDK/parko dependency tree. "Accepted (direction)" is the *architecture*; it is
**not** an SDK adoption of iceoryx2 (the core stays transport-agnostic, and Clause 2's
layout is library-independent).

### 2. Latency evidence — the quantitative rationale

The driving requirement is the **lowest-latency way to cross the mandatory
doer↔checker isolation boundary** (freedom-from-interference forbids co-locating
them — so the boundary cannot be deleted to save latency).
**In PR #736**, `tools/iceoryx2-spike/src/bin/latency_bench.rs` times the 176-byte
contract handoff three ways (host-**INDICATIVE** — shared dev box, no core isolation /
FIFO; 100k iters):

```
transport                  p50_ns     p99_ns    p999_ns     max_ns
in-process (floor)             28         37         60      69069
socket+serde (UDP loopback)   976       1901      13934      83820
iceoryx2 (zero-copy)          309        424        995      46083
```

iceoryx2 keeps the crossing **sub-microsecond** (~3× the median of a bare
UDP+serialize hop) and — the part that matters for a jitter-bounded safety path —
the **tail is ~14× tighter (p99.9 of 995 ns vs 13.9 µs)**. `socket+serde` is a
*conservative* proxy; a real DDS hop adds RTPS/discovery/typed-CDR overhead (tens of
µs–ms → 100–1000×; external refs in the spike README). The 28 ns in-process floor is
what the mandatory isolation costs; iceoryx2 holds the crossing near it. The deployment
**lowest-latency mode** pairs iceoryx2 with **busy-wait polling on an isolated core
under `SCHED_FIFO`** → toward iceoryx2's published ~100 ns; the certified figure is a
QNX-target-under-FIFO measurement (#274).

### 3. Where iceoryx2 applies (utilization scope)

A whole-system boundary map (every IPC / serialization / FFI hop) shows the
opportunity is **targeted, not universal** — most internal hops are already
in-process (Tokio / atomics) where iceoryx2 would *add* overhead, and cross-host hops
stay network transport. This bounds Clause 1's reach:

| Class | Boundaries | Verdict |
|---|---|---|
| iceoryx2 (intra-partition, cross-process) | governor-side contract handoff reusing the Clause 2 frozen layout (`GovernorContractView`) as payload | the carrier (this ADR) |
| iceoryx2 candidate (intra-host) | local actuator / sensor DDS hops | bypass DDS → the latency win |
| already in-process (no gain) | parko inference loop, posture engine, capture/audit mpsc, impact latch, Taj corridor, RSS gates | leave as-is |
| cross-host (NOT iceoryx2) | fleet transport (Zenoh, ADR-0007), two-box UDP governor | network — stays Zenoh/DDS |
| integration boundary (not hot path) | C FFI (`src/ffi.rs`), supervisor reset | Clause 3 (FFI demoted) |

### 4. Dependency / condition progress

- **PR #736 moves the pin `=0.9.1` → `=0.9.2`.** The `0.9.1` umbrella no longer composes
  with its `0.9.2` sub-crates on a fresh resolve (`ZeroCopySendError::NoConnectedReceiver`
  missing); the spike code compiles unchanged at `0.9.2`. (`Cargo.lock` is gitignored
  in the isolated spike, so the pin is the control.) PR #736 also adds a **dedicated CI
  lane** to gate the spike + carrier (full + minimal configs + clippy) — previously
  nothing ran it, which is exactly how the `0.9.1` breakage went unnoticed.
- **#274 advanced (judge, not iceoryx2 yet).** The QNX 8.0 cross-build + FDIT
  **verdict-correctness gate now PASSES on a `mkqnximage`/QEMU QNX 8.0 KVM VM**
  (`max ≈ 20 µs < 100 µs`, KVM-indicative; `tools/qnx-rtm-harness/results/`). This
  validates the *judge* on a real QNX target. It does **not** yet measure **iceoryx2**
  on QNX, nor discharge the **edition-2024 / Ferrocene** toolchain gate — both remain
  open conditions. Running `latency_bench` on the QNX KVM VM under `SCHED_FIFO` is the
  next step.

## Constraints and risks (honest section — none softened)

- **Edition-2024 toolchain gate.** iceoryx2 0.9.1/0.9.2 and its entire
  `iceoryx2-*` / `iceoryx2-bb-*` / `iceoryx2-pal-*` dependency family declare
  `edition = "2024"` (verified across the lock tree; unchanged by the 0.9.2 bump). Edition 2024
  stabilized in **Rust 1.85**, so an older toolchain (e.g. cargo 1.75) refuses
  the tree outright. The **QNX cross-toolchain AND the qualified Ferrocene
  `rustc` must support edition 2024**, or the iceoryx2 pin must move to an older
  release whose tree predates the bump. **UNRESOLVED until #274.** (The spike
  crate itself is edition 2021; only the iceoryx2 dependency tree forces 2024.)
- **QNX support is tier-3** in the open-source repo — not in upstream CI without
  licenses. Tier-1 is a **commercial ekxide engagement (#276)**.
- **QNX 8.0 requires `--no-default-features`** with std-dependent gaps —
  **UNVERIFIED on target until #274.** The empty-subset finding makes the
  zero-feature build *plausible*, not *proven*, on the 8.0 target.
- **Guest-side maturity.** `rmw_iceoryx2` is **alpha**; unsized types
  (`PointCloud2` — Autoware's bottleneck) currently take a **serialization
  fallback**, so guest-side zero-copy is **per-message-type, not blanket**.

## Status and conditions that reopen this decision

**Status: Accepted (direction).** The decision sets the architecture; the
target-validation conditions below remain open.

**Conditions that reopen this decision:**
- #274 failing the **feature-subset** or **toolchain** gate on QNX 8.0.
- **Ferrocene's** qualified version not reaching **edition-2024** support on the
  certification timeline.
- iceoryx2 / ekxide **abandoning the QNX tier**.

**Asymmetry (the durable part).** Clauses 1 and 3 (in-partition transport choice;
FFI demotion) depend on the conditions above. **Clause 2 — the frozen-layout
partition boundary — stands INDEPENDENT of them:** it is a property of the
*boundary contract*, not of any transport library or toolchain. If iceoryx2 were
dropped entirely (toolchain, tier, or maturity), the cross-partition frozen-
layout contract would remain the correct design — it is the most durable part of
this decision.

## Cross-references

- **EPIC #270** — iceoryx2 transport adoption for the QNX governor lane.
- **#273 / PR #277** — the host-side spike supplying the original evidence
  (`tools/iceoryx2-spike/README.md`).
- **PR #736** — the production frozen-contract carrier (`frozen.rs`) + the latency
  benchmark (`src/bin/latency_bench.rs`) + the dedicated CI lane; the
  §*Update 2026-06-30* evidence.
- **#274** — QNX 8.0 cross-compile + feature-subset + toolchain validation (the
  reopening condition; the judge's FDIT gate now passes on a QNX KVM VM —
  `tools/qnx-rtm-harness/results/`).
- **`crates/kirra-contract-channel/`** — the frozen `GovernorContractView` +
  `validate()` the carrier now rides (Clause 2's layout, library-independent).
- **#276** — ekxide tier-1 / commercial support posture.
- **#278** — the hypervisor-shared-memory frozen-layout contract channel (Clause
  2's design).
- **`docs/adr/0004-independent-safety-channel.md`** — the independent-safety-
  channel frame this ADR extends.
- **`docs/adr/KIRRA_QNX_CROSSCOMPILE.md`** — QNX cross-toolchain notes (the
  edition-2024 gate lands here at integration time).
- **`src/gateway/kinematics_contract.rs`** — the talisman; the in-repo precedent
  for layout-stability-as-safety-claim (Clause 2).
- **#79** — the durable epoch fence; the generation/replay discipline precedent.
