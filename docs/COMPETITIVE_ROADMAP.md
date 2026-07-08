# Kirra Competitive Roadmap
## Path to Production-Grade Safety Governance

This roadmap tracks the work required to close the gap between Kirra's
current state and what is required for production automotive deployment,
productization, and certification. It is organized by milestone,
not by calendar date.

> **Market context:** `docs/MARKET_AUTONOMOUS_SERVICES.md` maps the converging
> autonomous-services market (sidewalk couriers → delivery AVs → robotaxis) that
> promotes several items below — remote transport (#296), multi-tenant
> console/federation, per-class kinematic-contract profiles, and a VRU-dense ODD
> profile.

---

## Current State (v1.1.1 — May 2026)

| Capability | Status |
|------------|--------|
| Runtime safety kernel (posture engine, DAG traversal) | ✅ Complete |
| Kinematic envelope enforcement | ✅ Complete |
| RSS / IEEE 2846 safe-distance (longitudinal + lateral) | ✅ Complete |
| RSS pre-actuator gate in KirraGovernor | ✅ Complete |
| Hash-chained audit log (Ed25519 signed) | ✅ Complete |
| Multi-asset safety fabric | ✅ Complete |
| Industrial protocol adapters (EtherNet/IP, CANOpen, DNP3) | ✅ Complete |
| Action Filter (LLM → governor pipeline) | ✅ Complete |
| InferenceBackend trait + CPU ONNX backend | ✅ Complete |
| Feature-gated hardware backend stubs | ✅ Complete |
| ROS2 safety interlock package | ✅ Complete |
| Docker multi-platform images + Helm chart | ✅ Complete |
| ISO 26262 ASIL-D foundation docs (HARA, Goals, RTM, Arch) | ✅ Complete |
| 340 tests passing, 0 failures | ✅ Complete |
| QNX deployment | 🔄 In progress |
| TensorRT backend | ⏳ Jetson arriving |
| Hardware-in-the-loop demonstration | ⏳ Robot arriving |
| Third-party safety certification (TÜV SÜD) | 📋 Not started |
| ISO 21434 cybersecurity certification | 📋 Not started |

---

## Milestone 1 — Hardware Validation
### "Kirra runs on real hardware"
**Target: v1.2.0**

The gap between simulation and production credibility. Every serious evaluator
will ask "where has this run?" This milestone answers that question.

### M1.1 — QNX Deployment (TIME-SENSITIVE — license active)
- [ ] PARK-024: QNX resource manager binary — `kirra_safety_interlock` running on QNX SDP 8.0
- [ ] PARK-025: QNN + QNX compatibility analysis document
- [ ] PARK-026: QNX-safe backend selection rules
- [ ] Kirra `/health` endpoint returning 200 on QNX target
- [ ] `aarch64-unknown-nto-qnx800` added to release pipeline

**Why it matters:** BlackBerry QNX is already inside NVIDIA DriveOS AGX Thor.
A Kirra binary running on QNX puts Kirra inside the same deployment
environment as NVIDIA's certified stack — and demonstrates it works
without NVIDIA's silicon.

### M1.2 — Jetson TensorRT Backend
- [ ] PARK-020: TensorRT C shim spike — validates toolchain on Jetson
- [ ] PARK-021: TensorRTBackend struct implementing InferenceBackend
- [ ] PARK-022: BackendSelector — runtime backend selection (CPU vs TensorRT)
- [ ] PARK-023: CPU vs TensorRT output comparison harness
- [ ] MNIST-equivalent model running through KirraGovernor on Jetson

**Why it matters:** TensorRT on Jetson is the reference platform for
edge AI inference in autonomous systems. NVIDIA's entire automotive
ecosystem runs on this stack. Kirra running on Jetson with TensorRT
demonstrates hardware-level integration with the dominant platform
while remaining vendor-neutral at the governance layer.

### M1.3 — Robot Demonstration
- [ ] PARK-037: KirraGovernor wired into ROS2 cmd_vel pipeline
- [ ] Closed-loop demo: sensor input → AI planner → Kirra enforcement → actuator
- [ ] Video recording of robot operating under Kirra governance
- [ ] Demo showing posture degradation and recovery in real time

**Why it matters:** A working robot demo is the proof that the architecture
is not theoretical. It is the single most persuasive artifact for
customer conversations, conference presentations, and press.

---

## Milestone 2 — Multi-Silicon Validation
### "Kirra runs on more than one hardware platform"
**Target: v1.3.0**

The vendor-neutral claim requires demonstrating it on more than one
vendor's silicon. This milestone makes that claim concrete.

### M2.1 — OpenVINO Backend (Intel x86/VPU)
- [ ] PARK-029: OpenVINO runtime spike
- [ ] PARK-029: OpenVINOBackend struct implementing InferenceBackend
- [ ] CPU ONNX vs OpenVINO output comparison — same model, same inputs
- [ ] Latency benchmark: OpenVINO vs CPU baseline

**Why it matters:** OpenVINO runs on existing x86 hardware today.
No special hardware required. Second backend = vendor-neutral claim
becomes demonstrable, not theoretical.

### M2.2 — Qualcomm QNN Backend
- [ ] PARK-027: QNN API exploration spike
- [ ] PARK-027: QNNBackend struct implementing InferenceBackend
- [ ] QNN + QNX integration — safety governance on Snapdragon Ride target
- [ ] Latency profile on Snapdragon hardware

**Why it matters:** Qualcomm's Snapdragon Ride is a leading non-NVIDIA
automotive compute platform. Kirra running on Snapdragon Ride demonstrates
the vendor-neutral governance layer on a second silicon family — a safety
governance layer that makes the platform more competitive against NVIDIA
DRIVE without building it in-house.

### M2.3 — AMD Vitis AI Backend
- [ ] Hardware procurement: AMD Kria K26 (~$200)
- [ ] PARK-030: Vitis AI toolchain spike
- [ ] PARK-030: VitisAIBackend struct implementing InferenceBackend
- [ ] Deterministic latency benchmark — FPGA inference timing profile

**Why it matters:** FPGAs are used in automotive safety systems specifically
because of deterministic latency. FPGA inference with nanosecond-predictable
execution is a differentiator against TensorRT (JIT variance) and QNN
(thermal throttling variance). Per ADL-008.

### M2.4 — Cross-Backend Validation Harness
- [ ] Golden-model comparison: same inputs through all available backends
- [ ] Divergence detection and logging
- [ ] Latency envelope per backend wired into Kirra timing watchdog
- [ ] CI jobs running CPU backend on every PR

---

## Milestone 3 — Certification Track
### "Kirra has engaged the certification process"
**Target: v1.4.0**

This is the hardest milestone and the longest lead time. Full ASIL-D
certification takes 2-3 years minimum. The goal here is to begin the
process and produce artifacts that demonstrate certification intent
to customers and assessors.

### M3.1 — ISO 21434 Cybersecurity (no code required)
- [ ] TARA (Threat Analysis and Risk Assessment) document
- [ ] Cybersecurity goals derived from TARA
- [ ] Cybersecurity concept document
- [ ] Vulnerability management process defined
- [ ] Incident response process defined

**Why it matters:** ISO 21434 is now table stakes for any automotive
safety system. NVIDIA, Qualcomm, and the Tier-1 ADAS benchmark vendor all have it.
It is a prerequisite for any serious automotive software partnership.
This is process documentation, not code — it can be started today.

### M3.2 — IEC 61508 SIL 3 Requirements Mapping
- [ ] PARK (new): Map Kirra architecture to IEC 61508 SIL 3 requirements
- [ ] Gap analysis against current implementation
- [ ] SIL 3 claims documented per subsystem

**Why it matters:** IEC 61508 is the industrial safety standard —
relevant for Kirra's industrial robotics and infrastructure deployments
beyond automotive.

### M3.3 — ASTM F3269 RTA (Runtime Assurance)
- [ ] PARK (new): Map Kirra to ASTM F3269 Run Time Assurance framework
- [ ] RTA monitor specification document
- [ ] Recovery action specification

**Why it matters:** ASTM F3269 is the FAA-adjacent standard for AI
runtime safety. Relevant for drone deployments and any aerospace-adjacent
customer.

### M3.4 — TÜV SÜD Preliminary Engagement
- [ ] Prepare preliminary assessment package
- [ ] Engage TÜV SÜD for scoping conversation
- [ ] Define assessment scope (which subsystems, which standards)
- [ ] Begin formal assessment process

**Why it matters:** TÜV SÜD certified NVIDIA's core hardware and
software process to ASIL-D. Without a TÜV assessment, Kirra cannot
be used in a production vehicle regardless of code quality. Beginning
this process now puts Kirra 2-3 years ahead of a competitor who waits.

---

## Milestone 4 — Simulation at Scale
### "Kirra's safety claims are backed by synthetic validation data"
**Target: v1.5.0**

### M4.1 — CARLA CI Integration
- [ ] CARLA scenarios running in CI pipeline
- [ ] RSS safe-distance validated against CARLA ground truth
- [ ] Posture engine tested against synthetic sensor failure scenarios
- [ ] KirraGovernor envelope enforcement validated in simulation

### M4.2 — Scenario Library
- [ ] Standard NCAP scenarios implemented in CARLA
- [ ] Edge case library: sensor failure, network partition, adversarial inputs
- [ ] Scenario replay from audit log (closed-loop validation)

### M4.3 — V2X Integration Spike
- [ ] Research: C-V2X (Cellular Vehicle-to-Everything) signal ingestion
- [ ] Spike: V2X safety signals as PostureRecalcTrigger variants
- [ ] Design document: V2X events in Kirra posture model

---

## Milestone 5 — Productization & Public Launch
### "Kirra is ready for external evaluation"

### M5.1 — Public Repository
- [ ] Repo made public on GitHub
- [ ] README reflects current capability accurately
- [ ] CLAUDE.md updated with Kirra naming throughout
- [ ] Contribution guidelines and security policy added

### M5.2 — Technical Publication
- [ ] Substack post #1 published: DDS Volatile durability ✅
- [ ] Substack post #2 published: When the Tests Are Wrong
- [ ] Substack post #3: The RSS enforcement architecture
- [ ] Substack post #4: Multi-silicon backend design
- [ ] Substack post #5: The certification path

### M5.3 — Reference Architecture Documents (public)
- [ ] "Kirra + NVIDIA Jetson" reference architecture
- [ ] "Kirra + QNX" reference architecture
- [ ] "Kirra + ROS2" reference architecture
- [ ] "Kirra + RSS" integration guide (already drafted)

### M5.4 — Packaging
- [ ] PARK-031: Normalize Docker/Helm naming to Kirra throughout
- [ ] PARK-032: Parko runtime in Kirra Docker image
- [ ] QNX deployment recipe (after PARK-024)
- [ ] One-command install verified on clean Ubuntu 24.04

---

## Competitive Gap Summary

| Feature | NVIDIA Halos | Tier-1 ADAS benchmark | Kirra Today | Kirra M1 | Kirra M3 |
|---------|-------------|----------|-------------|----------|----------|
| Runtime safety enforcement | ✅ | ✅ | ✅ | ✅ | ✅ |
| RSS / IEEE 2846 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Vendor neutral | ❌ | ❌ | ✅ | ✅ | ✅ |
| QNX deployment | ✅ | ❌ | 🔄 | ✅ | ✅ |
| TensorRT backend | ✅ | ❌ | stub | ✅ | ✅ |
| Multi-silicon | ❌ | ❌ | arch only | partial | ✅ |
| ISO 26262 ASIL-D certified | ✅ | ✅ | docs only | docs only | in progress |
| ISO 21434 certified | ✅ | ✅ | ❌ | ❌ | in progress |
| Hardware demo | ✅ | ✅ | ❌ | ✅ | ✅ |
| Open source | ❌ | ❌ | private | public | public |
| Audit log (tamper-evident) | partial | ❌ | ✅ | ✅ | ✅ |
| Multi-asset fleet governance | ❌ | ❌ | ✅ | ✅ | ✅ |

---

## The Core Thesis

NVIDIA and the Tier-1 ADAS benchmark vendor sell safety governance bundled with their own silicon.
They are structurally unable to offer vendor-neutral safety governance
without cannibalizing their hardware business.

Kirra is the safety governance layer that works on any silicon.
Every OEM that does not want to be locked into a single chip vendor
needs something like Kirra. Every Tier 1 that sells hardware to multiple
OEMs needs something like Kirra. Every platform vendor (QNX, ROS2, AUTOSAR)
needs something like Kirra to complete their stack.

The certification path is the durable moat. Once Kirra achieves
TÜV SÜD ASIL-D assessment, it becomes extremely difficult for a
well-resourced competitor to replicate quickly — certification artifacts
are harder to reproduce than code.

---

*Last updated: May 2026*
*Current version: v1.1.1*
*Active development branch: main*
