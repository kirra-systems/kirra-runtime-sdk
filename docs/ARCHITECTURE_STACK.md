# Architecture Stack — the Three-Domain Integration Narrative

| Field | Value |
|---|---|
| Issue | **#295** (Occy 0.A sibling); board index **#302**; lane **EPIC #270** |
| Status | Narrative / index — **decides nothing** (see the rule below) |
| Scope | How the stack's technologies integrate, and which decided artifact owns each rule |

> **Governing rule — this document DECIDES NOTHING.** Every claim below cites the
> artifact where the decision actually lives (an ADR, a spec, an AoU, or code). This
> file is *only* anchors; it is an index over decisions, not a second source of
> truth. If a rule here ever lacks an anchor, it does not belong here — it belongs
> in its owning artifact first. (A stack doc that drifts from its sources becomes
> exactly the second-source-of-truth disease the boundary design exists to prevent.)

---

## 1. Thesis

The hypervisor **draws a line**; a **frozen contract crosses it**; the **governor
judges every crossing**; and every other technology is **sorted by which side of
the line it serves and held to that side's rules**. Most robotics stacks have no
such line — perception, planning, and actuation share one trust domain. KIRRA's
differentiator is not any single component but that **the sorting is explicit and
enforced**: a guest cannot smuggle authority across the boundary, because the
boundary is a fixed byte layout the governor re-validates, not a library call it
trusts (ADR-0006 Clause 2; `HYPERVISOR_CONTRACT_CHANNEL.md` §3).

---

## 2. The three domains

The organizing frame descends from the **independent-safety-channel** lineage
(ADR-0004, *Independent Safety Channel — D1–D3 settlement*) and the
partition-transport decision (ADR-0006).

### Safety partition (QNX host)
Every dependency must **justify itself into the trusted computing base**: frozen
`#[repr(C)]` contracts, zero-alloc hot paths, read-only mappings, minimal TCB.
**Members:** the kernel governor / gateway — with `src/gateway/kinematics_contract.rs`
as the **frozen-ABI precedent** (the "talisman": layout stability *is* the safety
claim, `HYPERVISOR_CONTRACT_CHANNEL.md` §2.4); the actuator-release authority
(HVCHAN §3 steps 6–7); and the boundary consumer (HVCHAN §3 snapshot/validate).

### Autonomy guest (Ubuntu)
Throughput-optimized and **assumed fallible** — Autoware / Occy planning,
perception, and the `parko` checker layer. **Precise note:** `parko` is a safety
*layer* **co-resident in the fallible domain** (`parko/QNX_BACKEND_SELECTION.md`
§1: "parko is the guest doer, not on QNX"); it is *not* the final authority. The
boundary's value claim is exactly this: **even a fully compromised guest — planner,
perception, and the parko layer included — cannot actuate**, because authority
lives only on the far side of the contract (HVCHAN §3; ADR-0004).

### The boundary
`planner → frozen contract → HVCHAN → governor → actuator authority`. This is the
seam most stacks lack. It is a **fixed-size, versioned, pointer-free `#[repr(C)]`
region**, not a transport endpoint (HVCHAN §2; ADR-0006 Clause 2).

---

## 3. Per-technology integration

Each: **role / domain / governing rule / anchors.**

### QNX Hypervisor
**Role/domain:** the partition mechanism — the line itself. **Rule:** it must
provide read-only mapping of the contract region into the governor partition, exact
region size/alignment, a shared monotonic clock, and a governor-partition scheduling
guarantee independent of guest CPU behavior (the R-HV-1…R-HV-4 hypervisor-config
requirements). The #279 fault-injection campaign is designed so each injected fault
is attributed to its **named owning barrier** (hypervisor / contract-discipline /
judge) — a fault absorbed by the wrong layer is a finding, not a pass. **Anchors:**
the platform ADRs (`0032-governor-deployment-platform.md`, ADR-0006);
`HYPERVISOR_CONTRACT_CHANNEL.md` §5 (hypervisor requirements); §4 + **#279** (the
barrier-attribution taxonomy).

### KIRRA Governor
**Role/domain:** safety partition — the authority that judges every crossing.
**Rule:** a fixed pipeline — stable **snapshot → bounds → CRC → contract judge →
digest of the validated bytes → Ed25519-signed release token → actuator
verify-before-release** — so "the governor approved exactly the data represented by
this digest, and release is cryptographically bound to that approval." It is
**layered**: a guest-side checker (the parko `SafetyGovernor` layer) plus the
host-side final authority; the host authority is what the safety claim rests on.
**Anchors:** `HYPERVISOR_CONTRACT_CHANNEL.md` §3 (the 7-step normative trust chain);
`src/attestation.rs` (the existing Ed25519 `verify_strict` + single-use
`consume_challenge` verify-then-consume pattern the release token reuses — no new
crypto); the SG detection layer in `parko/crates/parko-kirra` (the guest-side
checker).

### iceoryx2
**Role/domain:** in-partition transport — **both** sides — and **not** the boundary.
**Rule:** *the contract is the layout, not the library.* Inside a partition,
iceoryx2 is the transport (ADR-0006 Clause 1); **across** the partition boundary the
transport is explicitly **not** iceoryx2 — a native endpoint would import discovery,
lifecycle, pools, and version compatibility into the TCB, whereas a frozen layout
imports a struct definition (ADR-0006 Clause 2). The cert-scope rationale is the
durable part: per the ADR's **Asymmetry** note, Clauses 1 and 3 are conditional, but
Clause 2 (frozen-layout boundary) holds even if iceoryx2 were dropped entirely.
**Anchors:** ADR-0006 (all three clauses + the Asymmetry section; the edition-2024
toolchain gate and the QNX-8.0 `--no-default-features` constraint are in its
*Constraints and risks* section); `tools/iceoryx2-spike/README.md` (the #273 spike:
**no `extern "C"` / no `unsafe` / no FFI on the command path** via `&CommandFrame`;
**TornHeader eliminated by transport**, not a skipped test; the **empty** minimal
feature subset; the `sequence <= last_accepted ⇒ reject` judge).

### PTP (precision time)
**Role/domain:** **system timing only** — guest side. **Rule:** there are **two
clock domains** and a **normative non-mixing rule** — boundary/safety timing (the
hypervisor monotonic counter the governor reads) vs. system timing (PTP/gPTP,
wall-adjacent). The **guest converts to the boundary domain before publishing**; the
governor **never reads wall/PTP time on the validation path**; a deadline computed
in one domain and checked in the other is a defined fault. **Anchors:**
`HYPERVISOR_CONTRACT_CHANNEL.md` §5 R-HV-3 (the two-domain model + non-mixing rule);
`ASSUMPTIONS_OF_USE.md` **AOU-TIMESYNC-001** (the integrator timestamp obligation);
HVCHAN §4 (the `cross-domain timestamp` fail-closed row).

### Zenoh
**Role/domain:** the **fleet / QM** lane — robot↔robot, robot↔cloud, federated trust
report distribution. **Rule:** **never the safety channel.** The safety path is the
frozen-layout partition contract (ADR-0006 Clause 2); Zenoh carries fleet/QM traffic
only and must never become a trust-bearing safety transport. **Anchors:** **#296**
(the fleet-transport exploration); ADR-0006 Clause 2 (why it cannot be the boundary).

### LTTng vs tracelogger (tracing)
**Role/domain:** observability, split by domain. **Rule:** guest LTTng / perf is
**diagnostic-only — NON-EVIDENCE** for any governor timing claim (it locates
guest-side latency); only **tracelogger / SAT on the QNX target under FIFO**, with
the **observer-effect rule** (instrumentation overhead measured, bounded, and either
subtracted-with-justification or included-conservatively; stripped in production),
produces governor evidence. **Host numbers are never WCET.** **Anchors:**
`WCET_MEASUREMENT_METHODOLOGY.md` (KIRRA-OCCY-WCET-METH-001) §1 (the six assessor
questions, two-environment table) and §2 (the normative observer-effect rule);
**#297** (the guest LTTng setup, filed).

### Accelerators (GPU / NPU)
**Role/domain:** **guest-side** compute, selected through the `BackendSelector`
factory registry with **fail-closed env selection** (an unknown `KIRRA_BACKEND`
errors, never silently picks a different backend). **Rule:** at the safety edge,
QNX-native backend selection is **deny-by-default** — `Cuda`/`TensorRT` FORBIDDEN
(no CUDA on QNX), every other backend `PENDING-#36` until target evidence confirms
it; QNN is **additionally** gated by the Ride distribution finding (QNX QAIRT
binaries are automotive-BSP-only, not public). **Anchors:**
`parko/QNX_BACKEND_SELECTION.md` (R-1…R-3 + the availability table);
`parko/crates/parko-core/src/backend_selector.rs` (`BackendSelector` /
`backend_permitted` / `current_platform`); `work/decisions.md` **ADL-012** (the QNN
distribution gate); **#300** (the Qualcomm/Ride vendor engagement).

---

## 4. Considered and rejected (so they don't resurface)

- **A serialization framework at the boundary** (protobuf/flatbuffers/CDR/etc.) —
  rejected: the boundary is a frozen `#[repr(C)]` layout precisely to keep
  discovery/versioning/codec machinery out of the TCB (ADR-0006 Clause 2).
- **A general-purpose allocator on the governor hot path** — rejected: the governor
  path is zero-alloc (pre-allocated buffers, reused per cycle); dynamic allocation
  causes latency spikes / OOM risk in bounded-memory safety contexts
  (`QNX_BACKEND_SELECTION.md` R-1; ADL-003).
- **A native transport endpoint in the safety partition** — rejected: it imports
  discovery, lifecycle, loan management, pools, ownership, recovery, and version
  compatibility into the trusted computing base; the frozen layout imports a struct
  definition instead (ADR-0006 Clause 2; HVCHAN §1 non-goals).

---

## 5. Status (honest — nothing on-target is verified until #274 / #36)

| Technology | Status | Anchors |
|---|---|---|
| QNX Hypervisor | **DECIDED-PENDING-HARDWARE** — requirements specced; on-target unverified | HVCHAN §5; #274, #278, #279, #36 |
| KIRRA Governor | **DECIDED-AND-BUILT** (host judge + verifier/attestation); **boundary form PENDING-HARDWARE** | HVCHAN §3; `src/attestation.rs`; #271/#272 (harness), #278 |
| iceoryx2 | **DECIDED** (ADR-0006) + **host spike BUILT** (#273); QNX cross-compile **PENDING-HARDWARE** | ADR-0006; iceoryx2-spike README; #274 |
| PTP / two clock domains | **DECIDED** (model + non-mixing rule); concrete clock primitive **PENDING-HARDWARE** | HVCHAN §5 R-HV-3; AOU-TIMESYNC-001; #274 |
| Zenoh | **FILED-FUTURE** (not scheduled) | #296; ADR-0006 C2 |
| LTTng / tracelogger | **DECIDED** (methodology); guest setup **FILED-FUTURE**; target numbers **PENDING-HARDWARE** | WCET-METH-001; #297, #274 |
| Accelerators / BackendSelector | **DECIDED-AND-BUILT** (rules + enforcement); availability **PENDING-HARDWARE**; QNN engagement **FILED-FUTURE** | QNX_BACKEND_SELECTION.md; backend_selector.rs; ADL-012; #36, #300 |

**The honest line:** every "PENDING-HARDWARE" row is design/spec evidence only —
no end-to-end or on-target timing claim holds until the QNX deployment spike (**#36**)
and the cross-compile/measurement work (**#274**) produce target evidence. This
document indexes the decisions; it does not assert their on-target verification.
