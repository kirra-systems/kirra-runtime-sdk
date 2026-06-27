# KIRRA Platform & Deployment Strategy

**Date:** 2026-06-08
**Status:** Decision captured (see ADR-0001)
**Purpose:** Full technical and strategic rationale behind the governor's platform and deployment decision. The concise decision of record lives in `ADR-0001-governor-deployment-platform.md`; this document is the supporting analysis, the comparative reasoning, and the cited evidence.

---

## Executive summary

The KIRRA governor will run as the safety-domain payload **directly on QNX Hypervisor 8.0 for Safety**, with the Autoware / ROS 2 Jazzy / Occy stack (the *doer*) running **unmodified as an isolated Ubuntu guest VM** on the same SoC. The certified VMM supplies the ASIL-D freedom-from-interference boundary that makes the runtime-assurance decomposition creditable. The governor is written in **Rust** — today built with upstream `rustc`, and **Ferrocene-ready** for the ASIL-D build (Ferrocene is qualified for QNX Neutrino; productization is tracked in #132) — and the **certified artifact is the minimal `no_std` verdict core**, not the whole process.

QNX is chosen over PikeOS and INTEGRITY for one decisive reason on top of parity: it is the only one of the three with a qualified ASIL-D Rust toolchain target, so the governor stays in Rust without a bespoke toolchain qualification. PikeOS and INTEGRITY remain roadmap-only, activated by a named customer already certified on them.

---

## 1. The architectural distinction everything rests on

A separation-kernel hypervisor runs two fundamentally different kinds of partition:

- **Native certified partition** — code running in (or directly on) the certified RTOS/hypervisor, inside the safety envelope.
- **Linux guest VM** — a full general-purpose OS hosted as an isolated virtual machine.

These have completely different certification implications, and conflating them is the most common mistake in this space. The governor belongs in the certified domain; the planner belongs in a Linux guest. The hypervisor enforces the boundary between them.

---

## 2. Why the doer stack is Linux-guest-grade

Autoware + ROS 2 Jazzy + the Rust/r2r planning code is a heavyweight Linux workload (Ubuntu base, large dependency tree, frequently GPU/CUDA). It does not run natively on a microkernel. The realistic — and correct — home for it is an **unmodified Linux guest VM**, where it behaves exactly as it does on bare Ubuntu because, inside the guest, it *is* Ubuntu.

This is a feature, not a compromise: the doer ports to the target platform "for free" (it's just a VM), and the entire uncertified stack is quarantined behind the hypervisor boundary. The doer is the *uncertified* partition by design.

---

## 3. Substrate comparison — QNX vs PikeOS vs INTEGRITY

All three are credible certified separation kernels and all three are supported targets for Apex.Grace (the certified ROS 2 fork), per Apex.AI's own statements. The decision criteria that matter to KIRRA:

| Criterion | QNX | PikeOS | INTEGRITY |
|---|---|---|---|
| ISO 26262 ASIL-D certified kernel | Yes (TÜV Rheinland) | Yes | Yes |
| Certified hypervisor for mixed-criticality | **Yes — Hypervisor for Safety 8.0** | Yes (separation kernel) | Yes |
| Built-in freedom-from-interference | Yes | Yes | Yes |
| Qualified ASIL-D **Rust** toolchain target | **Yes (Ferrocene / QNX Neutrino)** | Not today | Not today |
| Qualified C/C++ toolchain | Yes (TCL3) | Yes | Yes |
| POSIX compliance (eases porting) | **Strong** | Partial (personalities) | Partial |
| Primary domain | **Automotive / industrial / robotics** | European avionics / space / rail (ITAR-free) | Aerospace / defense (DO-178C DAL A) |
| Automotive incumbency | **Highest** (hundreds of millions of vehicles) | Moderate | Moderate |

QNX wins on parity plus three KIRRA-specific advantages: the qualified Rust path (Section 4), strong POSIX compliance (cheaper porting), and automotive incumbency (credibility with the OEM/Tier-1 buyers a safety governor must persuade). A near-exact customer analog already exists publicly: **FERNRIDE selected QNX OS for Safety for autonomous terminal tractors**, citing POSIX compliance and the goal of shortening the timeline to certify the *entire* stack — the same off-highway/bounded-ODD profile KIRRA should target for a first pilot.

---

## 4. The Rust-at-ASIL-D path (Ferrocene) — and why QNX is uniquely good for it

KIRRA's governor is Rust. Using Rust in an ASIL-D component requires a *qualified* Rust toolchain, which is **Ferrocene** — the first qualified Rust compiler, TÜV SÜD-qualified to ISO 26262 ASIL-D (TCL3), IEC 61508, and IEC 62304.

The decisive fact: **Ferrocene's qualified targets include QNX Neutrino 7.1.0 on x86-64 and Armv8-A** (alongside x86-64 Linux and bare-metal Arm). They do **not** currently include PikeOS or INTEGRITY native — those would be "additional RTOSes on request," i.e., a fresh target qualification effort. So QNX is the one substrate where "Rust governor + RTOS + ASIL-D" is an off-the-shelf, already-qualified path.

**The nuance to engineer around — compiler vs. library coverage:**
- The Ferrocene **compiler** is ASIL-D.
- The Ferrocene **certified library** is currently a subset of `core` at ASIL-B / SIL2 (expanded again at embedded world 2026, and growing).
- `std`, `tokio`, and `r2r` are far outside that certified subset.

**Implication:** the ASIL-D-qualified artifact must be the **minimal `no_std` verdict core** — the kinematic contract and its gates — compiled with Ferrocene against the certified `core` subset, with any code outside the subset qualified by the team. The `std`/async/ROS machinery stays *outside* the ASIL-D boundary. This is not a constraint imposed by the platform so much as the same cert-scope discipline the architecture already depends on; the platform just makes it explicit.

*(Fallback if the Rust-at-ASIL-D path ever proves impractical for a given assessor: QNX's qualified path is C/C++ TCL3, with a C++ library pre-certified — ASIL-B headers/templates, ASIL-D runtime binaries. That would mean reimplementing the certified verdict core in C/C++. It is a known, supported escape hatch, not the plan.)*

---

## 5. Deployment architecture — QNX Hypervisor 8.0 for Safety

As of **GA 2026-03-10**, this is a purchasable, certified product configuration — not a bespoke design. QNX Hypervisor 8.0 for Safety is a TÜV-Rheinland-certified VMM (ISO 26262 ASIL D, IEC 61508 SIL 4, IEC 62304 Class C) that hosts **unmodified Linux and Android guests** and is explicitly positioned for "Physical AI" — autonomous and intelligent software-defined systems across automotive, robotics, medical, and industrial — which is precisely KIRRA's space.

### Topology (single SoC)

```
+-----------------------------------------------------------------+
|              QNX Hypervisor 8.0 for Safety  (ASIL D)            |
|                  certified freedom-from-interference            |
|                                                                 |
|  SAFETY DOMAIN (on hypervisor)        NON-SAFETY DOMAIN (VM)     |
|  +-----------------------------+      +----------------------+   |
|  |  KIRRA GOVERNOR (checker)   |      |  Ubuntu guest (doer) |   |
|  |  - no_std verdict core      |      |  - ROS 2 Jazzy       |   |
|  |    (kinematic contract)     |      |  - Autoware          |   |
|  |  - own clock + watchdog     |      |  - Occy planner      |   |
|  |  - owns actuator output     |      |  - perception / ML   |   |
|  |  - Ferrocene / Rust         |      |  - DDS / r2r (here)  |   |
|  +--------------+--------------+      +----------+-----------+   |
|                 ^   verdict / clamp             | proposal       |
|                 |  (narrow shared-mem mailbox)  v                |
|                 +-------------------------------+                |
|                                                                 |
|   actuator bus (CAN/etc.) terminates in the SAFETY domain only  |
+-----------------------------------------------------------------+
        GPU (if present) -- passthrough to Ubuntu VM, SMMU-contained
```

The recommended configuration is one QNX documents directly: the guest runs in a VM while the safety-critical application runs directly on the hypervisor.

### Data flow and binding invariants

1. **Propose vs. dispose.** The doer only *proposes* — it writes a trajectory/command into the inter-VM channel. It never touches actuators.
2. **Governor owns the actuator path.** The physical actuator bus terminates in the safety domain; the Ubuntu VM is physically incapable of driving actuators directly. The governor is the last gate.
3. **Narrow, analyzable channel.** A fixed-schema shared-memory mailbox. DDS / r2r / ROS stay entirely inside the Ubuntu VM; no ROS reaches into the safety domain.
4. **Governor has its own clock and watchdog,** off the certified scheduler. "No fresh valid proposal by deadline" = fault → Design Safe State (the hypervisor guarantees safe-state transitions).
5. **All doer output is untrusted input.** The governor's detect→safe-state function must never depend on the doer behaving.

### Why it is creditable

The ASIL decomposition — uncertified QM-level Ubuntu beside an ASIL-D governor, still supporting a top-level safety claim — only holds if the isolation boundary is itself certified. It is: the hypervisor carries the ASIL-D freedom-from-interference, with spatial isolation (MMU plus an SMMU manager that contains guest DMA) and temporal isolation (CPU partitioning, vCPUs pinnable to physical cores). A fault, hang, flood, or compromise in the Ubuntu guest cannot prevent the governor from detecting and safe-stating. The runtime-assurance bet rests on a certified foundation rather than on Linux behaving.

### Cert-scope boundary

**Inside the ASIL-D envelope:** the `no_std` verdict core, the inter-VM channel read, the actuator write, and the governor's clock/watchdog.
**Outside:** Autoware, the Occy planner, perception, ML, the GPU path, DDS/r2r/ROS/`tokio`/`std`, and the federation/attestation/telemetry/audit machinery.

---

## 6. Integration work — the part we actually build

The isolation is *bought*; the integration is built. The real new engineering surface is small and well-defined:

- **Inter-VM channel contract** — the fixed-schema shared-memory mailbox (proposal in, verdict/clamp out). Primary new surface.
- **Actuator-ownership wiring** — route the physical command path through the safety domain.
- **Ferrocene library coverage check** — keep the governor core within (or qualify beyond) the certified `core` subset; track subset growth across releases.
- **Safe-state mapping** — wire the governor's safe-state output to the hypervisor's Design Safe State transition.
- **GPU** — if perception needs it, pass it through to the Ubuntu VM with SMMU containment; the governor needs no accelerator by design.

---

## 7. PikeOS and INTEGRITY — roadmap positioning

For the first certified target (automotive AV, Rust governor), PikeOS and INTEGRITY do **not** come into play; selecting them would forfeit the off-the-shelf Ferrocene Rust path. They enter the picture only later, and only as **customer environments**, driven by KIRRA's vendor-neutral positioning:

- **INTEGRITY** — aerospace/defense incumbent (DO-178C DAL A, EAL 6+, 80-plus airborne systems), with automotive presence. The target if a pilot originates in defense robotics.
- **PikeOS** — European avionics/space/rail (DO-178C, ECSS, EN 50128), fully European and ITAR-free — a sovereignty selling point. The target for European or ITAR-sensitive opportunities.

A customer already certified on one of these will not switch to QNX to adopt the governor; KIRRA ports *to them*.

**Honest cost framing:** vendor-neutrality is an *architecture property*, not a free-deployment claim. Each additional kernel is a separate per-platform qualification campaign — a qualified Rust toolchain on that kernel (Ferrocene "on request"), plus re-doing the inter-partition channel and integration evidence. The verdict *logic* ports cheaply because it is minimal `no_std`; the *certification evidence* is per-platform and is where the cost lives. The only thing required now to keep the option open is to hold the verdict core free of QNX-specific syscalls and `std` — which the cert-scope discipline already does.

---

## 8. Market and relationship context

- **Apex.AI is a category neighbor, not a competitor.** Apex.Grace is an ASIL-D-certified ROS 2 runtime (the *doer* layer); KIRRA is a runtime-assurance governor that sits *above* a planner. KIRRA could even run over Apex.Grace. Apex.Grace is supported on QNX, INTEGRITY, PikeOS, and Linux-PREEMPT_RT — so leaning on it would inherit multi-kernel portability, though KIRRA's own cert evidence remains per-kernel. The natural relationship is complementary.
- **The #1 value multiplier remains a named pilot.** The platform decision is now settled; the gating item for credibility is a real deployment. The kernel choice should follow the pilot customer's existing substrate — which, for the most likely (automotive/industrial/off-highway) customers, is QNX anyway.

---

## 9. What does NOT change

- **Dev workflow** — the Ubuntu / Jazzy laptop stays the doer-side development environment. This is a cert-*target* decision, not a day-to-day one.
- **Rust** — retained at ASIL-D via Ferrocene's qualified QNX target.
- **The talisman discipline** — keeping `kinematics_contract.rs` minimal and fixed is now the formal certification-kernel strategy, not just hygiene. That file is the thing that becomes the qualified `no_std` core.

---

## Sources

- Apex.OS support across QNX / INTEGRITY / Cisco / PikeOS / Linux-PREEMPT_RT — Electronic Design: https://www.electronicdesign.com/markets/automotive/video/21240334/making-ros-2-automotive-ready
- INTEGRITY ASIL-D, Apex companion, Tier IV/Autoware partnership — Embedded.com: https://www.embedded.com/av-developers-build-on-ros-framework/
- INTEGRITY DO-178C DAL A / EAL 6+ / architecture — ArchiveOS: https://archiveos.org/integrity/
- PikeOS certifications, architectures, ITAR-free — MathWorks: https://www.mathworks.com/products/connections/product_detail/pikeos.html
- micro-ROS supported RTOSes (FreeRTOS/Zephyr/NuttX) — micro.ros.org: https://micro.ros.org/docs/overview/rtos/
- QNX OS for Safety 8.0 — ASIL D / SIL3 / Class C / ISO-SAE 21434 — QNX: https://qnx.software/en/software/products-and-solutions/qnx-os-and-os-for-safety
- QNX OS for Safety — "certify only what you build," TCL3 toolchain — Automate.org: https://www.automate.org/products/blackberry-qnx/qnx-operating-system-for-safety
- QNX OS for Safety — C++ library pre-cert (ASIL-B headers / ASIL-D runtime) — AWS Marketplace: https://aws.amazon.com/marketplace/pp/prodview-26pvihq76slfa
- FERNRIDE selects QNX OS for Safety (autonomous terminal tractors) — Stock Titan: https://www.stocktitan.net/news/BB/fernride-selects-qnx-for-safety-certified-autonomous-terminal-aswl1mle15q3.html
- Ferrocene ASIL-D + qualified QNX Neutrino target — BusinessWire: https://www.businesswire.com/news/home/20250114192138/en/
- Ferrocene 24.11 (QNX toolchains qualified) — Ferrous Systems: https://ferrous-systems.com/blog/ferrocene-24-11-0/
- Ferrocene 26.02 (certified core subset ASIL-B) — Ferrous Systems: https://ferrous-systems.com/blog/ferrocene-26-02-0/
- Ferrocene targets (Linux, QNX, bare-metal Arm; additional RTOSes on request) — Ferrocene: https://ferrocene.dev/
- QNX Hypervisor 8.0 for Safety GA (2026-03-10), certified VMM, unmodified Linux/Android guests — Newswire: https://www.newswire.com/news/qnx-hypervisor-8-0-for-safety-powers-the-industry-shift-toward-physical-ai
- QNX Hypervisor for Safety — certified VM isolation, focus cert on your components — SoftwareOne: https://platform.softwareone.com/product/qnx-hypervisor-for-safety/PCP-9229-3037
- QNX Hypervisor — SMMU DMA containment, Design Safe State, mixed criticality — QNX: https://qnx.software/en/software/products-and-solutions/qnx-hypervisor-and-hypervisor-for-safety
- QNX partnerships (Vector/TTTech, AMD, Microsoft Azure, GEDP), 275M+ vehicles — SEC 8-K: https://www.sec.gov/Archives/edgar/data/0001070235/000107023525000065/q4fy25ex-991.htm

---

*Companion document: `ADR-0001-governor-deployment-platform.md` (the decision of record).*
