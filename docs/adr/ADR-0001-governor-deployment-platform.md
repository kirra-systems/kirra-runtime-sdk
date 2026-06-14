# ADR-0001 — Governor Deployment Platform

**Status:** Accepted
**Date:** 2026-06-08
**Owner:** Kirra Systems, LLC
**Scope:** Certification-target platform and deployment topology for the KIRRA runtime safety governor. This is a decision of record for the *cert target*, not a statement of achieved certification.

---

## Decision

The KIRRA governor runs as the **safety-domain payload directly on QNX Hypervisor 8.0 for Safety**. The Autoware / ROS 2 Jazzy / Occy planning stack (the *doer*) runs **unmodified as an isolated Ubuntu guest VM** on the same SoC. The certified virtual machine manager provides the ASIL-D freedom-from-interference boundary between them.

The governor is implemented in **Rust, built with the Ferrocene qualified toolchain** targeting QNX Neutrino. The **certified artifact is the minimal `no_std` verdict core** (the kinematic contract and its gates), not the full governor process.

PikeOS and INTEGRITY are **not** build targets. They remain roadmap-only, activated solely by a named customer whose platform is already certified on them (aerospace / defense / space / rail).

---

## Context

KIRRA's thesis is runtime assurance: a small, certified **checker** sits above a large, uncertified **doer** and gates every actuator command. That decomposition is only creditable for a top-level ASIL-D claim if the isolation between checker and doer is itself a *certified* freedom-from-interference boundary — engineering isolation is not sufficient. The platform decision therefore turns on three requirements:

1. A certified separation kernel / hypervisor that can host an uncertified Linux workload next to an ASIL-D component with certified FFI.
2. A qualified path for the governor's implementation language (Rust) at ASIL-D.
3. Minimal cert scope — only the verdict logic and its I/O boundary should fall inside the certified envelope.

All three resolve cleanly on QNX. The dev environment (Ubuntu / Jazzy laptop) is the *doer-side* environment and is unchanged by this decision.

---

## Platform stack (grounded, with certifying body)

| Layer | Product | Certification | Notes |
|---|---|---|---|
| Hypervisor / VMM | **QNX Hypervisor 8.0 for Safety** (GA 2026-03-10) | TÜV Rheinland — ISO 26262 **ASIL D**, IEC 61508 SIL 4, IEC 62304 Class C | Certified VMM; hosts **unmodified** Linux/Android guests; SMMU manager contains guest DMA; guarantees safe transitions to a Design Safe State; ARMv8 + x86-64. |
| Safety OS | **QNX OS for Safety 8.0** | TÜV Rheinland — ISO 26262 **ASIL D**, IEC 61508 SIL 3, IEC 62304 Class C, ISO/SAE 21434 | "Certify only the parts you build, not the OS or toolchains." Qualified C/C++ toolchain (TCL3). POSIX. FFI built in. |
| Rust toolchain | **Ferrocene** (QNX Neutrino target) | TÜV SÜD — ISO 26262 **ASIL D** (TCL3) compiler | Qualified targets include QNX Neutrino 7.1.0 (x86-64 + Armv8-A). **Compiler** is ASIL-D; **certified library** is currently a `core` subset at ASIL-B / SIL2 and growing. → governor core must be `no_std` / core-subset; anything outside the certified subset is qualified by the team. |

---

## Domain layout (single SoC)

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
        GPU (if present) ── passthrough to Ubuntu VM, SMMU-contained
```

- **Safety domain (runs directly on the hypervisor):** the governor — minimal `no_std` Rust verdict core, its watchdog, and the actuator output path.
- **Non-safety domain (isolated VM):** the entire Ubuntu/Autoware/Jazzy/Occy stack, unmodified, plus all DDS/r2r/ROS plumbing.
- **Boundary:** certified FFI — spatial (MMU + SMMU DMA containment) and temporal (CPU partitioning, vCPUs pinned to physical cores).

---

## Binding invariants (the design rules that make it safe)

1. **Propose vs. dispose.** The doer only *proposes* — it writes a trajectory/command into the inter-VM channel. It never touches actuators.
2. **Governor owns the actuator path.** The physical actuator bus terminates in the safety domain. The Ubuntu VM is *physically incapable* of driving actuators directly. The governor is the last gate.
3. **Narrow, analyzable channel.** The doer↔governor interface is a fixed-schema shared-memory mailbox. DDS / r2r / ROS stay entirely inside the Ubuntu VM. No ROS reaches into the safety domain.
4. **Governor has its own clock and watchdog,** driven by the certified scheduler. "No fresh valid proposal by deadline" = fault → Design Safe State (the hypervisor guarantees the safe-state transition).
5. **All doer output is untrusted input.** The governor's safety function (detect → safe-state) must not depend on the doer behaving. Validity is established by the verdict core, never assumed.

---

## Cert-scope boundary

**Inside the ASIL-D envelope (small, qualifiable):**
- the `no_std` verdict core (kinematic contract + gates)
- inter-VM channel read
- actuator write
- the governor's clock + watchdog

**Outside the ASIL-D envelope (in the guest, or in a lower-ASIL QNX process):**
- Autoware, the Occy planner, perception, all ML
- the GPU path
- DDS / r2r / ROS / `tokio` / `std`
- federation, attestation, telemetry, and audit machinery (anything that pulls in `std` / async)

This mirrors the existing "protect the verdict path as the fixed core" discipline; this ADR makes that the formal certification rationale — the verdict core is the qualifiable kernel, everything else is plumbing held outside the boundary.

---

## Open items / integration work (the part we actually build)

- **Inter-VM channel contract.** Define the fixed-schema shared-memory mailbox (proposal in, verdict/clamp out). This is the primary new engineering surface.
- **Actuator-ownership wiring.** Route the physical command path through the safety domain.
- **Ferrocene library coverage check.** Confirm the governor core stays within (or qualifies beyond) the certified `core` subset; track subset growth across Ferrocene releases.
- **Safe-state mapping.** Wire the governor's safe-state output to the hypervisor's Design Safe State transition.
- **GPU.** If perception needs it, pass the GPU through to the Ubuntu VM with SMMU containment; the governor requires no accelerator by design.

---

## Consequences

- **Dev workflow unchanged.** The Ubuntu / Jazzy laptop remains the doer-side development environment. This ADR concerns the cert *target*, not day-to-day development.
- **Rust is retained at ASIL-D** via Ferrocene's qualified QNX target — the reason QNX was chosen over PikeOS/INTEGRITY, which are not Ferrocene-qualified targets today.
- **The talisman discipline gains a formal basis.** Keeping the verdict path minimal and fixed is now the certification kernel strategy, not just hygiene.
- **Vendor-neutrality is preserved as an architecture property,** not a free-deployment claim. Each additional kernel (PikeOS/INTEGRITY) is a separate per-platform qualification campaign, triggered only by a named customer in that domain.
- **"Ubuntu isolated as a VM on a certified OS, governor on QNX" is now a supported product configuration,** not a bespoke design — QNX Hypervisor 8.0 for Safety (GA 2026-03-10) is the certified VMM that provides it.
