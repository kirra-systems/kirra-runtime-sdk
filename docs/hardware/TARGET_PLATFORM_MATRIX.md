# Target platform matrix — where the Kirra stack runs

> **Status: informative selection guide.** The load-bearing *decision of record*
> for the governor's certification target is **ADR-0032** (QNX Hypervisor on an
> application-class SoC) and **`AOU-HW-QNX-TARGET-001`** (NVIDIA DRIVE AGX
> Orin/Thor for the certified-WCET numbers). This document does not change those;
> it maps them onto concrete AV vs robotics hardware configs and part numbers,
> and records the one architectural fork that decides "best hardware."

## 0. The stack is not one box

Kirra is a **doer / checker split**, and the two halves have different hardware
homes:

| Element | What it is | Where it runs |
|---|---|---|
| **Doer** | Autoware / ROS 2 / Occy perception + planning — *proposes* trajectories | A rich-OS (Linux) application processor. Swappable, never trusted for safety. |
| **Governor** | The fail-closed checker — the `no_std` verdict core + kinematic contract that *bounds* the doer (`src/wcet_gate.rs`, `crates/kirra-inline-governor`, `crates/kirra-contract-channel`) | A safety domain with certified freedom-from-interference. The invariant. |
| **Actuator element** | Terminates the actuator bus (CAN/serial/DDS) + the physical E-stop | A lockstep safety MCU. Power-stage authority is removed **independently of any MCU**. |

Because the checker core is `no_std`/`core`-subset and the doer↔governor
interface is a **frozen `#[repr(C)]` layout, not a library** (ADR-0006 Clause 2),
the doer and the governor **need not share a silicon vendor**. That portability
is deliberate — it is what lets the matrix below mix vendors per role.

## 1. The fork that decides "best"

There are exactly two ways to realize "rich-OS doer + fail-closed governor +
certified isolation," and the best hardware differs per branch:

### Branch A — one SoC (the ADR-0032 cert path)

A single application SoC hosts **QNX on-chip** *and* a Linux guest for the doer,
with a hardware **safety island**; the certified Type‑1 hypervisor provides the
ASIL‑D freedom-from-interference boundary (spatial: MMU + SMMU/IOMMU; temporal:
CPU partitioning).

- **Requires** a SoC with **both** a QNX BSP **and** a safety island.
- **NVIDIA Jetson does NOT qualify** — it is Linux-only (L4T / Jetson Linux); NVIDIA
  provides no QNX BSP for it (`AOU-HW-QNX-TARGET-001`). Jetson is a *doer*, never
  the governor's cert target.

### Branch B — two boxes (the ADR-0033 working pattern)

A Linux SoC runs the doer; a **discrete lockstep safety MCU** runs the governor +
actuator gate over the frozen contract. This is the R2 chokepoint (Orin ↔ STM32,
ADR-0033) scaled to a real ASIL/SIL part. The safety TCB is a small, cheap,
easily-qualified MCU — the rich OS carries no safety claim.

**AV leans Branch A; robotics leans Branch B.** Both are valid; the choice is a
cost / certification-tier trade, not a correctness one.

## 2. AV config (automotive, ASIL‑D)

| Role | Recommended | Notes |
|---|---|---|
| **Compute SoC** | **NVIDIA DRIVE AGX Thor** (DRIVE AGX Orin if you need silicon shipping now) | The only platform where Kirra's design lands as-authored: **QNX OS for Safety on-chip** + a Linux guest doer + a **Cortex‑R52 lockstep safety island** + ASIL‑D systematic capability. It is the repo's named cert target (`AOU-HW-QNX-TARGET-001`). |
| **Second source** | **Qualcomm Snapdragon Ride Flex (SA8775P)** | Mixed-criticality SoC, ASIL‑D safety island, QNX Hypervisor + multi-guest, lower SWaP than DRIVE. Maps cleanly to the doer-guest / safety-partition split; good for a non-NVIDIA second source or L2+/L3. |
| **Actuator MCU** | On-SoC safety island, or discrete **Infineon AURIX TC4x** / **NXP S32Z/E** | Terminates the actuator bus in the safety domain; independent E-stop removes power-stage authority. |
| **RTOS + compiler** | **QNX OS for Safety 8.0** + **Ferrocene** Rust | Required for the ASIL‑D cert artifact and the WCET-under-FIFO evidence (`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`). |

**Single best pick (AV): DRIVE AGX Thor + QNX OS for Safety + Ferrocene.**

## 3. Robotics config (AMR / industrial, SIL 2 / PL d)

Not automotive ASIL‑D, and you don't get QNX-on-Jetson — so the honest best is
**Branch B (two-box)**:

| Role | Recommended | Notes |
|---|---|---|
| **Doer (perception/planning)** | **NVIDIA Jetson Orin NX/AGX** (Thor when available), Linux | Strong perception compute + the ROS 2 ecosystem. This is what the Yahboom testbed already is. Runs the doer, never the verdict. |
| **Governor + actuator gate** | Discrete lockstep safety MCU: **TI AM263x / AM243x**, **NXP S32K39**, or **Infineon AURIX TC3xx** | Runs the `no_std` verdict core over the frozen SHM/serial contract — the ADR-0033 chokepoint at production grade (STM32 → a qualified ASIL‑D/SIL‑3 part). |
| **One-SoC alternative** | **TI TDA4x / Jacinto** or **Qualcomm QRB5165 / RB6** | These carry a **Cortex‑R5F safety island** on-die, so the governor can be the on-chip safety partition and the A-cores run Linux — a single-SoC robot without DRIVE-class cost (Branch A at a robotics price point). |
| **x86 industrial AMR** | **AMD Ryzen Embedded** (or Intel Atom x7000RE) + a safety MCU | QNX runs natively on **x86_64** — literally the current dev target. Best where you want commodity compute and SIL 2, not automotive ASIL‑D. **AMD Versal AI Edge** adds FPGA + a hardened R5F safety island. |

**Single best pick (robotics): Jetson Orin doer + a TI AM263x / NXP S32K3 safety
MCU governor** — the cheapest path to a *certifiable* safety story, because the
TCB is a sub-$15 lockstep MCU, not a multi-thousand-dollar AV computer.

## 4. Cross-cutting truths (every config)

- **A Jetson-class module is a doer, never the governor's cert target.** It can never
  carry the QNX-target-under-FIFO WCET numbers that back an FTTI claim (`AOU-HW-QNX-TARGET-001`,
  and the invariant *"host timing is INDICATIVE, never WCET"*). Keep it as the
  doer / dev rig.
- **A safety MCU + independent E-stop are mandatory in all configs.** The actuator bus
  terminates in the safety domain; the E-stop removes power-stage authority
  independently of any MCU (`firmware/rosmaster-r2/README.md`).
- **The dev/CI target today is a QNX 8.0 x86_64 VM** (`mkqnximage` / QEMU → KVM); the
  `no_std` judge cross-compiles `x86_64-pc-nto-qnx800`, `kirra-l3-e2e` runs the
  enforced path there. Its numbers are `INDICATIVE-NOT-WCET`; cert-grade WCET is
  Phase II on DRIVE + QNX OS for Safety + Ferrocene under `SCHED_FIFO`.
- **Vendor portability by role, not one vendor for all:** put the doer on
  Intel / AMD / Qualcomm / Jetson as convenient; keep the governor on QNX-on-DRIVE
  (AV) or a TI / NXP / Infineon lockstep MCU (robotics). The frozen contract +
  `no_std` core is what makes that legal.

## 5. Silicon you have / have targeted, mapped

| Silicon | Best role in Kirra |
|---|---|
| **NVIDIA DRIVE AGX Orin / Thor** | AV governor SoC (QNX on-chip) — the cert path |
| **NVIDIA Jetson Orin** (your Yahboom testbed) | Doer / dev rig — Linux, never the governor |
| **Qualcomm Snapdragon Ride Flex** | AV compute (Branch A second source) |
| **Qualcomm QRB / RB robotics** | One-SoC robotics (on-die safety island) |
| **TI AM26x / AM24x** (TMS570 lineage) | The safety-MCU governor role (Branch B) |
| **TI TDA4 / Jacinto** | Mid-tier one-SoC robotics / ADAS |
| **NXP S32K3 / S32Z/E / S32G** | Safety-MCU governor role; S32G for A-core + safety |
| **Infineon AURIX TC3xx / TC4x** | Automotive-grade actuator/governor safety MCU |
| **Intel / AMD x86** | QNX dev VM; x86 industrial-AMR doer + discrete safety MCU |
| **AMD Versal AI Edge** | FPGA robotics with a hardened R5F safety island |
| **STM32F103 (Cortex‑M3)** | The **R2 robot's motor MCU** — fixed hardware, `firmware/rosmaster-r2`; distinct from the governor |

## 6. Bottom line

- **AV:** NVIDIA **DRIVE AGX Thor** + QNX OS for Safety + Ferrocene (Qualcomm
  **Ride Flex** as second source). One SoC, ASIL‑D, matches the repo exactly.
- **Robotics:** **Jetson Orin doer + a discrete lockstep safety MCU** (TI AM263x /
  NXP S32K3 / Infineon AURIX) running the governor — the ADR-0033 pattern at
  production grade. One-SoC option: **TI TDA4** or **Qualcomm QRB** with an on-die
  safety island.

## References

- `docs/adr/0032-governor-deployment-platform.md` — governor on QNX Hypervisor 8.0 for Safety (decision of record)
- `docs/adr/0006-governor-transport-iceoryx2.md` — iceoryx2 (intra-partition) + frozen SHM layout (cross-partition)
- `docs/adr/0033-*` — the ROS actuation chokepoint (the two-box governor-on-MCU pattern)
- `docs/safety/ASSUMPTIONS_OF_USE.md` — `AOU-HW-QNX-TARGET-001` (DRIVE not Jetson), the HV clock/scheduling/read-only-map AoUs
- `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md` — why only QNX-target-under-FIFO numbers back an FTTI claim
- `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` — the `#[repr(C)]` contract view + two-clock-domain model
- `firmware/rosmaster-r2/README.md` — the R2 motor MCU (STM32F103), a separate concern
