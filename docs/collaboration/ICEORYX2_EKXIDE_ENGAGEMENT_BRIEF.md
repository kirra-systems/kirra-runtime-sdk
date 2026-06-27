# iceoryx2 / ekxide Engagement Brief — KIRRA Governor Transport

| Field | Value |
|---|---|
| Doc ID | **KIRRA-OCCY-ICX-BRIEF-001** |
| Status | **DRAFT — engagement scoping** |
| Owns | Project owner (engagement); KIRRA safety/runtime engineering (technical) |
| Counterpart | ekxide / iceoryx2 maintainers |
| Builds on | ADR-0006 (`docs/adr/0006-governor-transport-iceoryx2.md`); the #273 host spike (`tools/iceoryx2-spike/`); HVCHAN-001 (`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`) |
| Issues | EPIC #270; #274 (target validation); #276 (tier-1 / commercial posture) |

> This is an **engagement-scoping brief**, not a safety artifact. It states what
> the host spike already proved, what is still **target-gated**, and the precise
> **open questions** for the ekxide engagement. Every on-target claim below is a
> *requirement to be verified*, not a result.

---

## 1. Who we are and what we're building

KIRRA is a **fail-closed runtime safety governor** built on a **doer/checker**
architecture: an untrusted planner (the *doer*, e.g. an Autoware / ROS 2 stack)
proposes trajectories/commands; an independent kinematic + RSS checker (the
*checker*) bounds them. The intended target is a **QNX-resident governor**
running alongside an Autoware/ROS 2 **guest** under a hypervisor on the same SoC.

ADR-0006 sets the transport direction in three **distinct** clauses:

- **Clause 1 — inside a partition, the transport is iceoryx2** (Rust, daemon-less;
  guest side via `rmw_iceoryx2` as it matures, governor-side host processes via
  native Rust iceoryx2). **This is what the engagement is about.**
- **Clause 2 — across the guest↔host partition boundary, the transport is NOT
  iceoryx2.** It is a **frozen, versioned, fixed-size `#[repr(C)]` layout over
  hypervisor shared memory** (`GovernorContractView`, HVCHAN-001), mapped
  read-only into the governor partition. This deliberately keeps the iceoryx2
  TCB **out of the safety partition** — see §4. *(Not an ask of ekxide; stated so
  the boundary is unambiguous.)*
- **Clause 3 — the C ABI / FFI is demoted** to the documented C/C++ integration
  boundary (DDS bridges, vendor stacks) and is **no longer the governor hot path.**

The engagement is the **#276 commercial / tier-1 posture** that ADR-0006 names as
the path to clearing the **#274** target-validation conditions below.

---

## 2. What the host spike already proved (`tools/iceoryx2-spike/`, PR #277)

The #273 spike is a **standalone crate** (its own `[workspace]` / `Cargo.lock`);
**iceoryx2 never enters the KIRRA or parko dependency tree.** It pins
`iceoryx2 = "=0.9.1"`. On **host (x86-64 Linux)** it established:

1. **Minimal feature subset = EMPTY.** iceoryx2 0.9.1 compiles **and** runs the
   zero-copy pub/sub + subscriber-lifecycle path with `default-features = false`
   and nothing re-added — both `std` and `console` are droppable. *(Host only;
   the input to the #274 QNX `--no-default-features` check.)*
2. **TornHeader eliminated by construction.** The publisher writes into an
   **exclusively-loaned** slot and `send()` publishes an **immutable** sample; the
   subscriber's `receive()` returns an **owned** sample over a stable,
   not-yet-recycled slot. The application never double-reads a live, mutating
   buffer — a fault class the transport *removes*, not merely catches.
3. **No-FFI / no-unsafe hot path, compiler-enforced.** The spike carries
   `#![forbid(unsafe_code)]`; the judge is an ordinary function call on a typed
   `&CommandFrame`. The fault matrix is green in **both** feature configurations.
4. **Replay / regress discipline.** The judge rejects on
   **`sequence <= last_accepted`** (equal = replay, lower = regress; strictly
   newer passes), proven red/green. This is also the **generation rule** for the
   Clause 2 boundary channel and aligns with the durable epoch-fence (#79).

**What the spike does NOT prove:** anything on the QNX target. Items 1–4 are
host findings. The on-target build, feature subset, timing, and scheduling
behavior are all open (#274).

---

## 3. Open questions for ekxide (the heart of the engagement)

Grouped by the ADR-0006 reopening conditions. Each is a **decision input**, not a
position we hold.

### Q1 — Edition-2024 toolchain gate *(highest-leverage)*
The entire `iceoryx2-*` / `iceoryx2-bb-*` / `iceoryx2-pal-*` 0.9.1 tree declares
`edition = "2024"`, which stabilized in **Rust 1.85**. Our certification stack is
**QNX + Ferrocene**.
- Does the **QNX cross-toolchain** and a **qualified Ferrocene `rustc`** reach
  **edition-2024** support on a timeline compatible with our program?
- Is there a **maintained iceoryx2 release line whose tree predates the
  edition-2024 bump** that we could pin as a fallback, and what is its QNX status?
- What is iceoryx2's policy on the **minimum-supported Rust edition/version** going
  forward (so we can plan the floor rather than chase it)?

### Q2 — QNX 8.0 target build & feature subset
iceoryx2 added **QNX 7.1 as tier-3** in v0.7.0; our target is **QNX 8.0**.
- What is the **QNX 8.0** support status today (PAL coverage, known gaps)?
- Does **`--no-default-features`** build *and run* on QNX 8.0, and which
  **std-dependent gaps** remain there (the spike's empty-subset is host-only)?
- Are there QNX-specific configuration constraints (shared-memory provider,
  service-discovery, fixed-pool sizing) we should bake in from day one?

### Q3 — Tier-1 / commercial posture (#276)
- What does the **ekxide tier-1 engagement** concretely cover — platform CI,
  patch/issue SLAs, QNX 8.0 enablement, version-pin maintenance?
- Does ekxide provide **qualification-supporting artifacts** (test evidence,
  requirements/traceability, a safety manual / “safety element out of context”
  style package) that could feed our safety case, or is that strictly our scope?

### Q4 — Determinism & WCET on the in-partition path
The governor's FTTI claim needs **bounded, measured** timing under **FIFO
scheduling** on target.
- Is the steady-state `loan → send → receive` path **zero-allocation** once pools
  are sized, and what config guarantees that (static allocation / fixed pools, no
  late discovery on the hot path)?
- What **WCET-relevant guidance** exists for the publish/receive primitives on
  QNX (worst-case under contention, pool-exhaustion behavior)?
- Recommended pattern for a **bounded, drop-on-full** sensor-data lane vs. a
  reliable command lane?

### Q5 — `rmw_iceoryx2` maturity (guest side)
`rmw_iceoryx2` is **alpha**; unsized types (e.g. `PointCloud2`, Autoware's
bottleneck) currently take a **serialization fallback**, so guest-side zero-copy
is **per-message-type, not blanket**.
- Roadmap for **unsized-type / blanket zero-copy** support?
- Which Autoware-relevant message types are zero-copy **today** vs. fallback?
- *(Guest-side / perception-ingest concern — distinct from the governor host hot
  path, but it gates end-to-end latency.)*

---

## 4. Boundary we are NOT asking ekxide to cross (Clause 2)

For **certification scope**, the **cross-partition guest↔host boundary is a frozen
SHM layout, not a native iceoryx2 endpoint in the safety partition.** A native
endpoint there would import discovery, lifecycle, loan management, memory pools,
ownership transitions, recovery, and version compatibility into the **trusted
computing base**; a frozen layout imports a **struct definition**
(`GovernorContractView`, HVCHAN-001 §2). This is the most durable part of
ADR-0006 and stands **independent** of the iceoryx2 toolchain/tier conditions.

We would welcome ekxide's perspective on this boundary, but the working
assumption is: **iceoryx2 is the in-partition transport; it does not cross the
partition boundary.** The fleet lane (vehicle ↔ ops/cloud) is **Zenoh** (ADR-0007),
strictly QM and out of scope for this engagement.

---

## 5. What we can share / logistics

- **Shareable now:** ADR-0006, the #273 spike (`tools/iceoryx2-spike/`, PR #277),
  and HVCHAN-001 (the boundary spec) — subject to a mutual NDA.
- **Logistics to settle:** NDA, licensing terms for a tier-1 engagement, and a
  cadence for the #274 on-target validation campaign.
- **Definition of done for #274 (our side):** on QNX 8.0, with the qualified
  toolchain, the spike's fault matrix is green under `--no-default-features`, with
  measured timing under FIFO scheduling feeding the WCET methodology
  (`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`).

---

## 6. Cross-references

- **ADR-0006** — `docs/adr/0006-governor-transport-iceoryx2.md` (the three clauses
  and the reopening conditions).
- **HVCHAN-001** — `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` (the Clause 2
  frozen-layout boundary contract).
- **The #273 spike** — `tools/iceoryx2-spike/README.md` (the host evidence cited
  in §2).
- **EPIC #270 / #274 / #276** — transport adoption, target validation, tier-1
  posture.
- **WCET methodology** — `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md` (host
  timing is INDICATIVE; only QNX-target-under-FIFO numbers feed an FTTI claim).
