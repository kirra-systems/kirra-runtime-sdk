# ADR-0030: L3 last mile — binding the cross-partition contract carrier and driving it from the guest and governor

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. The software path (L3.1–L3.3) is on `main`; this ADR specifies the remaining target-integration mile. |
| Date | 2026-07-01 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG3** (per-command kinematic envelope — the governor bounds every crossed command); **SG2** (containment — unchanged, drive-agnostic); **SG8** (degraded / MRC — any non-actuatable verdict converges to the safe state); freedom-from-interference / the doer↔checker partition boundary (ADR-0004, ADR-0020) |
| Cross-refs | ADR-0006 (governor transport — **Clause 2** frozen layout, **Clause 3** FFI/unsafe demoted to the integration boundary); `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` (**HVCHAN-001** — the contract, the 7-step trust chain, **R-HV-1..4**); ADR-0004 (independent safety channel); ADR-0020 (doer-invariant safety case); ADR-0012 (authoritative MRC envelope per asset — `KIRRA_VEHICLE_CLASS` → the per-class `VehicleKinematicsContract`); ADR-0013 (SW e-stop / release token — the authenticated actuation gate the digest feeds); ADR-0011 (Degraded 503→0.0 / decel gate — the MRC discipline a non-actuatable verdict routes to); ADR-0014 (Rosmaster R2 + Orin NX — the guest platform); `docs/safety/ASSUMPTIONS_OF_USE.md` (**AOU-TIMESYNC-001**); EPIC #270; #274 (QNX target validation); #278 (this design half + hardware); #279 (fault-injection campaign). Delivered software: `kirra_contract_channel::{command,seqlock,validate}` (L3.1), `kirra_ros2_adapter::contract_producer` (L3.2), `kirra_core::contract_consumer` (L3.3). |

## Context

L3 (the ROS2/QNX guest split) has delivered the **complete software doer→checker path** over the frozen Clause-2 contract, and it is host-tested end-to-end over the in-process reference carrier:

- **L3.1** — `kirra-contract-channel`: the frozen `GovernorContractView`, the odd/even seqlock (`publish` / `read_coherent_snapshot`), `validate` + `AcceptedWatermark`, and the `VehicleCommandPayload` command codec (the on-wire form of `ProposedVehicleCommand`, fail-closed `decode`).
- **L3.2** — `kirra_ros2_adapter::contract_producer`: the guest producer — `proposal_payload` (map the fast-loop `IngressControlCommand` → the frozen payload, rad→deg) + `ProposalSequencer::publish_to` (monotonic sequence + committed generation over any `ContractWriter`).
- **L3.3** — `kirra_core::contract_consumer`: the governor consumer — `consume_and_bound` (`read_coherent_snapshot → validate → decode → validate_vehicle_command`) → the typed, fail-closed `GovernorVerdict`.

All three are generic over the **carrier-agnostic seam** `ContractReader` / `ContractWriter`, and today that seam has exactly **one** binding: `InProcessRegion` (atomics, single process — the reference carrier). That is deliberate: the seam is the boundary between the **contract** (frozen layout + trust chain, which never changes) and the **carrier** (how the bytes physically cross the partition boundary, which does).

**What remains — the last mile — is the real carrier and the two call-sites that drive it:**

1. a **cross-partition** binding of `ContractReader`/`ContractWriter` over real shared memory (not one-process atomics);
2. the **guest** call-site (the ROS adapter's fast loop publishes each proposal);
3. the **governor** call-site (the governor partition polls `consume_and_bound` and gates actuation).

This mile is where the **only `unsafe`** in the entire L3 path lives (memory-mapping the region) — the ADR-0006 **Clause 3** integration boundary. Everything above it (`kirra-contract-channel`, `kirra-core`) stays `#![forbid(unsafe_code)]`. It is also where the two-clock-domain model (HVCHAN §5, R-HV-3) and the hypervisor-config requirements (R-HV-1..4) become concrete integrator obligations. Because it is target-integration work — QNX / ros2-gated, not host-unit-testable as pure logic — it is specified here rather than merged as another host PR.

## Decision

Six clauses.

### Clause A — one audited carrier crate holds ALL L3 mapping `unsafe`

Introduce a single small integration crate — `kirra-hv-carrier` — that is **the only crate in the L3 path NOT under `#![forbid(unsafe_code)]`**. It binds `ContractReader` / `ContractWriter` to a memory-mapped region and contains a **bounded, enumerated `unsafe` budget** (Clause 3):

1. the **map** syscall (`mmap`/`shm_open` on host; the hypervisor-region map on QNX) producing a pointer to exactly `size_of::<GovernorContractView>()` bytes (R-HV-2);
2. the **atomic field accesses** over the mapped bytes, reproducing `InProcessRegion`'s memory model **exactly** — the `generation` counter carries the ordering (Acquire on read, Release on write) and body fields are Relaxed; the seqlock generation fences them. This is the load-bearing correctness detail: the trust chain's torn-read freedom (HVCHAN §3) holds across the partition boundary **only** if the shim preserves those orderings on real shared memory.

No other `unsafe` is admitted. The `#[repr(C)]` layout match that the shim relies on is **already proven** by the freeze assertions in `kirra-contract-channel::view` (offsets, size, alignment) — the shim maps bytes and does atomic access; it does not re-derive the layout.

### Clause B — two real bindings: host POSIX-SHM (testable now) and QNX (target)

The carrier crate provides **two** bindings of the same traits, feature-selected:

- **`PosixShmRegion` (host, Linux) — the testable intermediate.** `shm_open` + `mmap` of one `GovernorContractView`-sized region shared between two OS **processes**. This exercises the *real* cross-process seqlock and atomic memory model — the step the reference `InProcessRegion` (one process, one `Arc`) cannot — **without QNX**. It is the direct analogue of the #273 iceoryx2 spike's `--two-process` mode, and it is the recommended **first** last-mile increment: a genuine two-process producer→consumer integration test that the QNX binding must then match byte-for-byte.
- **`HvRegion` (QNX) — the target binding.** Maps the hypervisor-provided shared region: the **guest** maps read-write, the **governor** maps **read-only** (R-HV-1). Same two traits, same trust chain; only the map primitive differs. This is #278 hardware-day work.

The behavioral spec both bindings must satisfy is the existing L3.1–L3.3 test suite over `InProcessRegion` — the seam is validated, so adopting a real carrier is "implement two traits, change nothing else."

### Clause C — the guest call-site (ROS adapter fast loop)

The ros2-gated `node.rs` fast loop drives the L3.2 producer on the guest-mapped writer. After the existing conformance path, for each `IngressControlCommand` it:
1. builds the payload via `contract_producer::proposal_payload(cmd, current_velocity_mps, current_steering_angle_deg, delta_time_s)` — the actual start state from `EgoOdom`, the step duration from the fast-loop period;
2. publishes via `ProposalSequencer::publish_to(&writer, &payload, publication_nanos, deadline_nanos)`, with **both timestamps in the boundary clock domain** (converted per AOU-TIMESYNC-001 before the call).

This is the point at which `contract_producer.rs`'s temporary unconditional `#![allow(dead_code)]` (L3.2) tightens back to `#![cfg_attr(not(feature = "ros2"), allow(dead_code))]` — the module now has a ros2 caller.

### Clause D — the governor call-site (governor partition loop)

The governor partition polls the consumer each control cycle on the **read-only-mapped** reader:

```
let verdict = consume_and_bound(&reader, &mut watermark, now_boundary_nanos, &contract, MAX_SNAPSHOT_RETRIES);
```

- `now_boundary_nanos` is read from the **boundary clock** only (R-HV-3); the governor never reads wall/PTP time on this path.
- `contract` is the per-class `VehicleKinematicsContract` selected by `KIRRA_VEHICLE_CLASS` (ADR-0012 — fail-closed: no default class).
- `watermark` is a persistent `AcceptedWatermark` across cycles.
- **Actuation gate:** `verdict.is_actuatable()` (only `Bounded(Allow | Clamp*)`) permits actuation; **every other outcome — any snapshot/contract/codec fault, or a `Bounded(DenyBreach)` — routes to the MRC safe-stop** (the ADR-0011 decel-to-stop / 503→0.0 discipline). Fail-closed by construction.

### Clause E — clock domain (R-HV-3)

`publication_nanos` / `deadline_nanos` are **defined in and only compared within the boundary clock domain**. The guest converts from its system clock **before** publishing (AOU-TIMESYNC-001); the governor validates against the boundary clock. A cross-domain timestamp is the defined `cross-domain timestamp` fault (HVCHAN §4). The concrete QNX boundary-clock **primitive**, its guest-visibility mechanism, and its bounded max skew remain target work (#274/#278) — until measured, the deadline/skew rows of the §4 table are the fail-closed backstops.

### Clause F — the release-token bridge (HVCHAN §3 steps 5–6)

On an **actuatable** verdict, before the command reaches the actuator, the governor computes the digest over `GovernorContractView::canonical_image` and signs it with the **existing** Ed25519 machinery (`src/attestation.rs`) — **no new crypto primitive** (HVCHAN §3). This is the bridge from the L3 receive path to the authenticated actuation gate of **ADR-0013** (SW actions are authenticated *requests* to the governor; the actuator verifies the token). Wiring this bridge is part of the governor call-site work (Clause D), specified here so the digest→sign→release step is not lost between the consumer verdict and the actuator.

## Consequences

- **The `unsafe` budget is one small, auditable crate.** The safety-relevant logic (`validate`, `decode`, `validate_vehicle_command`, the seqlock protocol) stays `#![forbid(unsafe_code)]` and host-tested; the carrier crate is the single Clause-3 integration unit, reviewed against the enumerated budget in Clause A.
- **The host POSIX-SHM binding (Clause B) keeps the last mile partly host-testable** — a real two-process seqlock crossing — so QNX is reduced to swapping the map primitive, not first-time-validating the memory model on target.
- **Fail-closed is preserved end-to-end.** Every fault the carrier can introduce (a torn/incoherent snapshot, a stale region, a wrong-size map) surfaces as a `GovernorVerdict` non-actuatable outcome → MRC (Clause D). The carrier cannot fabricate an `Allow`.
- **The Nominal WCET-critical path is unchanged.** `validate_vehicle_command` and its Priority-0 guard are untouched; the carrier adds the map + atomic reads, whose cost (notably the boundary-clock read) is on the governor validation path and is measured on target (#274).

## Requirements / Assumptions of Use (integrator-owned)

These are **not** discharged by this ADR; they are the hypervisor-config checklist (HVCHAN §5) this design leans on. Enumerated here as the "Done when" for the QNX binding:

- **R-HV-1 — read-only governor mapping** (verified at hypervisor config). *Register entry NOT filed yet.*
- **R-HV-2 — region size/alignment** = `size_of::<GovernorContractView>()`, natural alignment.
- **R-HV-3 — boundary clock domain** (shared monotonic source, bounded skew) + the non-mixing rule. Integrator-timestamp half **filed as AOU-TIMESYNC-001**; the hypervisor **clock-provision** half **NOT filed yet** (target primitive + skew figure, #274/#278).
- **R-HV-4 — partition-scheduling guarantee** for the governor independent of guest CPU behavior. *Register entry NOT filed yet.*

## Status and conditions that reopen this decision

**Status: Proposed (design note).** The software seam is settled; this ADR sets the carrier + call-site design. Reopening conditions:

- The QNX hypervisor cannot provide a **read-only** governor mapping (R-HV-1) or a **bounded-skew shared clock** (R-HV-3) — either would force a different boundary design.
- The measured **boundary-clock read cost** (target) blows the governor validation-path WCET budget (#274) — would force a clock-primitive change, not a contract change (Clause 2 is transport-agnostic).
- The atomic memory model cannot be reproduced faithfully over the hypervisor's shared-memory mapping — would reopen Clause A's seqlock-over-SHM assumption.

## Implementation order (recommended, each a bounded increment)

1. **`kirra-hv-carrier` + `PosixShmRegion` + a two-process host integration test** (host-testable; proves the cross-process seqlock).
2. **Guest call-site** — wire `contract_producer` into `node.rs` (ros2-gated; tighten the `dead_code` allow).
3. **Governor call-site** — the poll loop + `is_actuatable` actuation gate + the Clause-F digest/release bridge.
4. **`HvRegion` (QNX binding)** + the R-HV config checklist + #279 hypervisor-layer fault injection (target).

## Cross-references

- **ADR-0006** — Clause 2 (the frozen layout this carries) and Clause 3 (the FFI/unsafe integration boundary this crate *is*).
- **HVCHAN-001** (`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`) — the contract, the 7-step trust chain, R-HV-1..4, the two-clock-domain model.
- **ADR-0004 / ADR-0020** — the independent safety channel and the doer-invariant frame the partition boundary enforces.
- **ADR-0012** — the per-class `VehicleKinematicsContract` the governor call-site selects (`KIRRA_VEHICLE_CLASS`).
- **ADR-0013 / ADR-0011** — the authenticated release-token actuation gate (Clause F) and the MRC discipline a non-actuatable verdict routes to (Clause D).
- **ADR-0014** — the Rosmaster R2 + Orin NX guest platform.
- **`AOU-TIMESYNC-001`** (`docs/safety/ASSUMPTIONS_OF_USE.md`) — the boundary-domain timestamp conversion (Clause E).
- **EPIC #270**; **#274** (QNX target validation + WCET); **#278** (hardware implementation); **#279** (fault-injection campaign — the hypervisor-layer rows).
- Delivered software: `kirra_contract_channel::{command,seqlock,validate,reference}` (L3.1), `kirra_ros2_adapter::contract_producer` (L3.2), `kirra_core::contract_consumer` (L3.3).
