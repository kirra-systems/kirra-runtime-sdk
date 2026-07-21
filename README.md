# Kirra Runtime SDK

![CI](https://github.com/kirra-systems/kirra-runtime-sdk/actions/workflows/ci.yml/badge.svg)
![Version](https://img.shields.io/github/v/tag/kirra-systems/kirra-runtime-sdk)

A distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems. Kirra enforces **fail-closed trust semantics** across a heterogeneous fleet — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM output, or upstream orchestration layer instructs.

> **Note on versioning:** v1.5.0 was a documentation-only release
> (ASIL-D safety case foundation) tagged out-of-band by CI automation, so it
> sorts *above* the v1.1.x runtime line that supersedes it. The next release is
> **v2.0.0** — the disambiguating MAJOR (`docs/VERSIONING_POLICY.md` §2.1) that
> permanently clears that inversion (every active version is then > 1.5) and
> completes the ADR-0035 re-export-shim removal (#1029), on top of the Occy-line
> features (SG2 enforced, Option-B per-trajectory wiring on Autoware, S8
> quantitative evidence).

---

## The Kirra Stack (named components)

The runtime is built as a **doer / checker** architecture — untrusted components *propose*, the governor *disposes*:

| Component | Role |
|---|---|
| **Mick** | the LLM brain — System-2 cognition / LLM-as-planner. *Proposes* intent; never touches the actuator. |
| **Taj** | perception — sensors → the Kirra perception input contract (objects + drivable corridor + per-output health). |
| **Occy** (`kirra-planner`) | the formal autonomy planner — proposes trajectories the governor checks (Option-B, #131). |
| **Kirra** | the fail-closed **governor / checker** — the sole authority that may pass a command to an actuator. |
| **Parko** (`parko-core`) | vendor-neutral ML inference substrate (TensorRT / ONNX / OpenVINO / QNN / …) that Taj's ML phase runs on. |

**The rule:** Mick reasons / Occy plans → they *propose* typed claims → **Kirra** validates against posture + the kinematic envelope + RSS/containment → only a clamped, safe command reaches the chassis. See `docs/adr/0014`–`0015` for the Rosmaster R2 + Jetson Orin NX reference integration.

---

## AI Safety Integration

Kirra is the enforcement layer that prevents LLM hallucinations from reaching physical actuators — drop it between your AI agent and your robot fleet in minutes.

```
LLM output  →  Kirra Action Filter  →  Actuator
```

Every AI-generated command is evaluated against the live fleet posture before any hardware interaction occurs. A model that hallucinates a velocity of 999 m/s, invents a non-existent action type, or issues a kinetic command while the fleet is degraded is stopped at the software layer — and the attempt is permanently recorded in a SHA-256 hash-chained audit ledger.

### Posture-Action Matrix

| Action Type | Nominal | Degraded | LockedOut |
|-------------|---------|----------|-----------|
| `cmd_vel` (kinetic write) | ✓ with kinematics validation | ✗ | ✗ |
| `read_telemetry` | ✓ | ✓ | ✗ |
| Unknown / unrecognized | ✗ | ✗ | ✗ |

Compatible with **OpenAI function calling**, **LangChain tools**, or any agent framework that can make an HTTP POST.

**Docs:**
- [Action Filter Architecture](docs/action_filter.md) — pipeline, hallucination containment, API reference
- [LLM Integration Guide](docs/llm_integration_guide.md) — 5-minute quickstart, auth, agent loop patterns, SSE posture stream
- [OpenAI example](examples/openai_action_filter.py) — GPT function calling with Kirra safety filter
- [LangChain example](examples/langchain_action_filter.py) — `@tool` decorator pattern with Kirra safety filter

---

## Autonomous Vehicle Safety (Occy)

Kirra ships an Occy line — a Safety Element out of Context (SEooC)
Governor specialized for autonomous driving stacks. It runs as a two-rate
checker on the planning + control pipeline, enforces drivable-space
containment against an HD map, and publishes a Minimal Risk Condition
(MRC) verdict the vehicle stack must honor before any trajectory reaches
actuators.

### Occy Safety Goals

| Goal | Title | ASIL | Status |
|------|-------|------|--------|
| SG1 | Speed envelope (50 mph / 80 km/h hard cap) | D | ENFORCED |
| SG2 | Drivable-space lateral containment (≥ 0.40 m margin) | D | ENFORCED |
| SG3 | RSS longitudinal/lateral safety distance | D | ENFORCED (parko-core, wired in #131) |
| SG4 | MRC publication on contract violation | D | ENFORCED |
| SG5 | Trajectory liveness / staleness fail-closed | D | ENFORCED |
| SG6 | Independent Detection Channel (D1 IDC) | B | Optional add-on tier |
| SG7 | Fault model + degraded-mode availability | D | Specified |
| SG8 | Quantitative HW metrics (SPFM/LFM/PMHF) | D | Specified (dual-supply gate) |
| SG9 | NaN / Inf / non-finite input rejection | D | ENFORCED |

See [`docs/safety/OCCY_SAFETY_GOALS.md`](docs/safety/OCCY_SAFETY_GOALS.md) for HARA + STPA derivation.

### Option-B per-trajectory wiring (#131)

The Occy Governor sits on the Autoware ROS 2 stack as a two-rate checker:

- **Slow loop** (~10 Hz, ~10 ms budget) — runs at planning rate; validates
  the candidate trajectory against the HD-map corridor (Lanelet2), the
  RSS-over-horizon contract, the speed envelope, and the kinematic
  contract. Produces a `TrajectoryVerdict` of `Accept`, `Clamp`,
  `MRCFallback`, or `Pending`.
- **Fast loop** (~100 Hz, 200 μs budget) — runs at control rate; replays
  the accepted trajectory point-by-point, enforces the verdict, and
  publishes the MRC topic the vehicle MUST honor before steering /
  throttle / brake.

The wiring lives in the [`kirra-ros2-adapter`](crates/kirra-ros2-adapter)
crate. See [`docs/safety/OCCY_131_OPTIONB_DESIGN.md`](docs/safety/OCCY_131_OPTIONB_DESIGN.md)
for the design and [`docs/testing/CARLA_SCENARIO_SUITE.md`](docs/testing/CARLA_SCENARIO_SUITE.md)
for the integrator runbook.

### S8 Quantitative Evidence Package (#120)

| Item | Subject | Outcome |
|------|---------|---------|
| A | SG2 lateral margin derivation | `CONTAINMENT_LATERAL_MARGIN_M = 0.40` m (PRIMARY); 0.75 m fallback. G2 AoU on #123: ε_loc ≤ 0.10 m 95th-pct lateral error |
| B | D1 IDC detection-range spec | Per-sensor table + SSD-derate cap-impact; closes Item C AoU rows 1 + 4 in the D1 tier |
| C | Speed-cap validation matrix | 50 mph cap **unchanged**; PROVEN / OK-ANALYTICAL / AoU-GAP disposition for each ADR-0001 assumption |
| D | SPFM / LFM / PMHF | Single-supply PMHF **17.7 FIT (FAIL)**; dual-supply **8.7 FIT (PASS)** → deployment requirement: ASIL-D-class redundant supply |

---

## Safety Certification

Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet been performed.

### Foundation (Kirra / Aegis line)

| Document | Doc ID | Status |
|----------|--------|--------|
| Hazard Analysis and Risk Assessment (HARA) | KIRRA-HARA-001 | Draft |
| Safety Goals | AEGIS-SG-001 | Draft |
| Safety Architecture | KIRRA-SA-001 | Draft |
| Requirements Traceability Matrix | KIRRA-RTM-001 | Draft |
| Coding Guidelines | KIRRA-CG-001 | Draft |
| Safety Standards Matrix (23 standards, 5 verticals) | KIRRA-STD-001 | Draft |
| ASTM F3269 Run Time Assurance Mapping | KIRRA-F3269-001 | Draft |
| ASTM F3269-21 Bounded Operation Mapping (current) | KIRRA-RTA-001 | Draft |
| IEC 61508 SIL 3 Preliminary Claim Mapping | KIRRA-61508-001 | Draft |
| IEC 61508 SIL 3 Requirements Mapping (current) | KIRRA-SIL3-001 | Draft |
| External Security/Safety Review Wrap-Up | KIRRA-REV-001 | Final |

### Occy line — AV-specific safety case

| Document | Doc ID | Status |
|----------|--------|--------|
| Occy Safety Goals (HARA + STPA derivation) | KIRRA-OCCY-SG-001 | Draft |
| Occy ODD + SOTIF triggering conditions (ISO 21448) | KIRRA-OCCY-ODD-001 | Draft |
| Occy Speed-Envelope Analysis (SSD / breaking-point / derate) | KIRRA-OCCY-SPEED-001 | Draft |
| Occy ASIL Decomposition + Dependent Failure Analysis | KIRRA-OCCY-DFA-001 | Draft |
| Occy Independent Detection Channel (IDC) design | KIRRA-OCCY-IDC-001 | Draft |
| Occy two-tier architecture (base Governor + optional D1 add-on) | KIRRA-OCCY-ARCH-001 | Draft |
| Occy Governor integrity evidence plan (S3) | KIRRA-OCCY-INTEG-001 | Draft |
| Occy Governor fault model + degraded-mode availability (S7) | KIRRA-OCCY-FAULT-001 | Draft |
| Occy Safety Traceability Convention (`// SAFETY:` tags + CI gate) | KIRRA-OCCY-TRACE-001 | Draft |
| Occy Safety Traceability Matrix (auto-generated) | KIRRA-OCCY-TRACE-MATRIX-001 | Auto-generated |
| KIRRA Governor Safety Manual (SEooC consolidated deliverable) | KIRRA-OCCY-MANUAL-001 | Draft |
| Occy Freedom From Interference (FFI) evidence | KIRRA-OCCY-FFI-001 | Draft |
| Occy MC/DC coverage evidence | KIRRA-OCCY-MCDC-001 | Draft |
| Occy #131 Option-B per-trajectory wiring on Autoware | KIRRA-OCCY-OPTIONB-001 | Draft |
| Occy SG2 lateral margin derivation (S8 Item A) | KIRRA-OCCY-SG2-MARGIN-001 | Draft |
| Occy speed-cap validation matrix (S8 Item C; cap unchanged at 50 mph) | KIRRA-OCCY-SPEED-VAL-001 | Draft |
| Occy D1 IDC detection-range specification (S8 Item B) | KIRRA-OCCY-IDC-RANGES-001 | Draft |
| Occy quantitative HW safety metrics — SPFM/LFM/PMHF (S8 Item D) | KIRRA-OCCY-QUANT-001 | Draft |

### Architecture Decision Records

| ADR | Title | Status |
|-----|-------|--------|
| ADR-0001 | Occy ODD speed cap = 50 mph / 80 km/h | Accepted |
| ADR-0002 | Condition-dependent speed cap + sub-ODD partition | Accepted |
| ADR-0003 | Two-tier KIRRA architecture — base + optional D1 | Accepted |
| ADR-0004 | Independent Safety Channel — D1–D3 settlement | Superseded by ADR-0003 |

The **cert-target platform** decision and its prototype bring-up plan live under
[`docs/adr/`](docs/adr/): `0032-governor-deployment-platform.md` (KIRRA
governor on QNX Hypervisor 8.0 for Safety; the Autoware/ROS 2 doer as an isolated
guest VM; a Ferrocene `no_std` verdict core as the certified artifact), with
companions `KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md`, `KIRRA_BRINGUP_RUNBOOK.md`,
and `KIRRA_QNX_CROSSCOMPILE.md`. (That file's `ADR-0001` prefix is independent of
the numbered ODD-cap ADRs in the table above — a known naming overlap.)

[`0036-autoware-distro-migration-occy-gap.md`](docs/adr/0036-autoware-distro-migration-occy-gap.md)
records the Occy-vs-Autoware gap analysis and the decision for the Ubuntu
22.04/Humble → 24.04/Jazzy transition: **keep Autoware** as the AV-stack doer
(Occy does not cover its L4 breadth — localization, control, mature fused
perception), **isolate it as the only component on Humble** in its own container,
and move the rest of the stack (ros2 adapter, checker, Occy/Taj) to Jazzy now —
meeting only on 5 curated, hash-verified boundary topics. The safety spine is
`no_std`/ROS-agnostic, so this is a doer-side migration with no re-certification.
Isolation scaffold: [`deploy/autoware-isolation/`](deploy/autoware-isolation/).

### Governor transport / QNX partition lane (EPIC #270)

The governor command path is moving to **Rust end-to-end** on a QNX-resident
safety partition, with the Autoware/ROS 2 planner as an isolated guest. The C
ABI / FFI is demoted to the documented C/C++ integration boundary (ADR-0006
Clause 3) — it is no longer the command hot path.

| Document | Doc ID | Status |
|----------|--------|--------|
| Hypervisor contract-channel layout + trust-chain spec (#278) | KIRRA-OCCY-HVCHAN-001 | Draft |
| WCET measurement methodology (#274 / #279 timing-evidence strategy) | KIRRA-OCCY-WCET-METH-001 | Draft |
| Assumptions-of-Use register (incl. `AOU-TIMESYNC-001` boundary-clock time-sync) | KIRRA-OCCY-AOU-001 | Draft |

Test-evidence tooling: [`tools/qnx-rtm-harness/`](tools/qnx-rtm-harness/) — a C++
shim (driver) → Rust judge (checker) FDIT/RTM fault-injection harness, every row
traced to the kernel RTM (#271 / #272); [`tools/iceoryx2-spike/`](tools/iceoryx2-spike/)
— the host-side iceoryx2 feature-subset spike (#273). **Host timing is indicative
only; certified WCET is measured on the QNX target under FIFO scheduling (#274).**

See [docs/safety/](docs/safety/) for the complete safety case foundation,
[docs/safety/SAFETY_CASE_INDEX.md](docs/safety/SAFETY_CASE_INDEX.md) for the
full document registry, and [docs/safety/ROADMAP_TO_ASIL_D.md](docs/safety/ROADMAP_TO_ASIL_D.md)
for the certification roadmap.

---

## Roadmap

Pre-execution architecture sketches for planned integrations. Each document
includes honest caveats, effort estimates, and explicit sequencing dependencies.

| Integration | Description | Status |
|-------------|-------------|--------|
| [Autoware (Option-B)](docs/safety/OCCY_131_OPTIONB_DESIGN.md) | Two-rate Governor check on the Autoware ROS 2 stack; per-trajectory verdict with MRC publication | **Implemented (#131 closed)** |
| [IEEE 2846 / RSS](docs/roadmap/RSS_KIRRA_INTEGRATION.md) | Behavioral safety invariants based on IEEE 2846 — safe distance enforcement given perception state | **Implemented** (parko-core RSS, wired via SG3 in #131) |
| [QNX governor transport lane](docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md) | Rust-end-to-end command path on a QNX safety partition; hypervisor contract channel + iceoryx2 transport; FFI demoted to integration boundary (EPIC #270) | In progress — RTM harness + HVCHAN/WCET specs landed (#271/#272/#278/#274 docs); QNX cross-compile + hardware fault-injection campaigns blocked (#274/#279) |
| [Apollo AV Stack](docs/roadmap/APOLLO_KIRRA_INTEGRATION.md) | Cyber RT bridge between Apollo Control and Canbus — kinematic enforcement and lockout in the Apollo pipeline | Planned — after QNX + robot demo |
| Ferrocene compiler qualification | Switch from upstream `rustc` to Ferrocene + `criticalup.toml` for the ASIL-D toolchain claim | Planned — tracked in #132 |

See [docs/roadmap/](docs/roadmap/) for sequencing dependencies and execution plans.

---

## Overview

Modern robotic and autonomous deployments increasingly rely on AI models to generate operational commands. Kirra sits between those models and the physical actuators, acting as a cryptographically-grounded safety layer that:

- **Attests** each fleet node via HMAC-SHA256 challenge/response
- **Tracks trust posture** per-node and fleet-wide using a gray/black DAG traversal algorithm
- **Gates commands** based on live posture — locking out unsafe operations before they reach hardware
- **Monitors AV sensor health** with a configurable telemetry watchdog and hysteresis-based recovery
- **Enforces kinematics envelopes** — hard physical limits on velocity, acceleration, and yaw rate
- **Federates** trust across multiple controllers using Ed25519-signed reports
- **Audits** all state transitions via a SHA-256 hash-chained tamper-evident ledger
- **Supports HA deployments** with automatic passive-standby promotion

---

## Features

### Runtime Governor (core)
- **Fail-closed by design** — missing or invalid credentials yield `503`, never silent pass-through
- **Constant-time token comparison** — timing-safe token verification throughout
- **Gray/black DAG traversal** — cycle detection and diamond-DAG memoization for fleet dependency graphs
- **Kinematics enforcement** — vehicle command envelope validation with forward simulation
- **NaN / Inf / non-finite rejection** — Priority 0 guard before all envelope checks (SG9)
- **Posture engine worker** — mpsc channel coalesces burst faults into a single DAG recalculation
- **Generation persistence** — monotonic posture generation counter survives restarts via SQLite
- **SSE posture broadcast** — real-time fleet posture stream for subscribers

### Autonomous Vehicle / Occy line
- **SG2 drivable-space containment (ENFORCED)** — lateral margin ≥ 0.40 m against the HD-map corridor
- **Option-B per-trajectory wiring** — two-rate (slow @ planning / fast @ control) Governor check on Autoware
- **RSS-over-horizon** — IEEE 2846 safe-distance enforcement (SG3), as the **§4 conjunction** (danger needs BOTH longitudinal AND lateral unsafe — admits a safe stationary queue / stopped lead), plus **occlusion (RSS Rule 4)**, **multi-modal predictive RSS** (CV/CTRV modes, worst-cased), and a **True-Redundancy perception-divergence monitor** (two channels must agree; divergence → MRC-floor)
- **Doer-checker planner (Occy)** — a swappable, never-trusted DOER (geometric / learned / LLM-driven Mick intents incl. **multi-junction `RouteTo`** and **occlusion-aware junction creep**) PROPOSES; KIRRA BOUNDS. `LearnedManeuverPlanner` proves a 2-D learned vocabulary stays governed.
- **MRC publication** — `TrajectoryVerdict::{Accept,Clamp,MRCFallback,Pending}` published to the vehicle stack
- **Lanelet2 corridor source** — cxx-rs wrapper around `lanelet2_core` + `boost::serialization` for `LaneletMapBin.data`
- **Subscription-staleness watchdog** — fast-loop refuses to advance when trajectory / objects / odometry feeds go silent
- **Two-tier architecture** — base Governor SEooC (ASIL-D) + optional D1 Independent Detection Channel (ASIL-B)
- **ASIL decomposition** — SG-level decomposition with documented Dependent Failure Analysis (DFA)
- **Freedom From Interference (FFI)** — spatial / temporal / communication isolation evidence
- **Ferrocene-ready** — `// SAFETY:` traceability convention + CI gate; productization tracked in #132
- **CARLA scenario suite** — integrator runbook with curb-cut, occluded pedestrian, stale-feed, and MRC fallback scenarios

### Trust / Posture / Fleet
- **AV sensor watchdog** — per-node telemetry timeout detection (warn at 1 s, fault at 2 s)
- **Recovery hysteresis** — 5 consecutive healthy reports required over a 10 s window to restore trust
- **Industrial protocol support** — Modbus and OPC-UA event evaluation
- **DDS bridge** — CDR-encapsulated actuator topics with `Volatile` durability
- **Ed25519 federation** — cross-controller trust reports with replay prevention and nonce burning
- **Federation reconciliation** — generation-ordered conflict resolution for multi-controller deployments
- **HA standby/promotion** — heartbeat-based automatic promotion from passive standby to active
- **WAL-mode SQLite** — fail-closed write **ordering** (disk before memory, INV-12); power-loss **durability** is precise: `synchronous=FULL` for the HA epoch fence and federation nonce burn (survive a hard cut), audit ledger durable to the last checkpoint (see `docs/safety/CODING_GUIDELINES.md` INV-12)
- **SHA-256 hash-chained audit ledger** — tamper-evident record of all state transitions

### Test / Tooling
- **Deterministic test harness** — `ScenarioRunner` with virtual clock injection for temporal integration tests
- **MC/DC pair-completing tests** — coverage evidence per KIRRA-OCCY-MCDC-001
- **CARLA integration** — `kirra_carla_client` binary for AV simulator connectivity
- **Two-box prototype tools** — `kirra-governor-service` (UDP governor wrapping the verdict core), `kirra-proposal-bench` (proposal-sweep harness), and the shared `kirra-wire-client` mirror; pure-Rust, runs the governed-car demo before the QNX cert factoring (ADR-0001, `docs/adr/KIRRA_BRINGUP_RUNBOOK.md`)
- **Auto-generated traceability matrix** — `// SAFETY:` tag scanner produces `docs/safety/TRACEABILITY_MATRIX.md`

---

## Architecture

> **The seven-technology integration, in one place:** [docs/ARCHITECTURE_STACK.md](docs/ARCHITECTURE_STACK.md) — the three-domain model (safety partition / boundary / autonomy guest), every claim anchored to its owning ADR/spec.

```
src/
├── verifier.rs                — AppState, FleetPosture, DAG traversal, TransportIdentityConfig
├── verifier_store.rs          — SQLite persistence (all tables; WAL mode)
├── posture_cache.rs           — SharedPostureCache, CachedFleetPosture, ServiceState,
│                                OperationalCommand, should_route_command
├── posture_engine.rs          — recalculate_and_broadcast, derive_fleet_posture,
│                                generation counter, init_generation_from_store
├── posture_engine_v2.rs       — LockoutReason, PostureRecalcTrigger, PostureEngineSender,
│                                start_posture_engine_worker, resolve_posture_with_reason
├── recovery_hysteresis.rs     — evaluate_recovery_report, HysteresisDecision
├── telemetry_watchdog.rs      — spawn_telemetry_watchdog (AV sensor health monitoring)
├── clock.rs                   — Clock trait, SystemClock, VirtualClock (test injection)
├── scenario_runner.rs         — ScenarioRunner, ScenarioEvent, PostureAssertion
├── standby_monitor.rs         — spawn_heartbeat_writer, spawn_promotion_monitor
├── federation.rs              — FederatedTrustReport, Ed25519 verify pipeline
├── federation_reconciliation.rs — FederatedTrustReportV2, reconcile_reports
├── audit_chain.rs             — SHA-256 hash-chained audit log
├── kinematics_contract.rs     — KinematicContract, scalar envelope clamping
├── kinematics_sim.rs          — VehicleState, forward simulator, apply_enforcement
├── action_filter.rs           — ActionFilter<C>, ActionClaim evaluation
├── action_policy.rs           — LLM JSON → typed AgentAction parser
├── security.rs                — constant_time_compare
├── protocol_adapter.rs        — Modbus/OPC-UA industrial event mapping
├── kirra_core.rs              — KirraKernelGovernor (clamping + rate limiting)
├── ros2_adapter.rs            — NaN/Inf rejection before ROS2 publish
├── dds_bridge.rs              — CDR encapsulation, Volatile durability
├── gateway/
│   ├── policy.rs              — classify_command (path + method → OperationalCommand)
│   ├── policy_layer.rs        — Tower KirraPolicyLayer middleware
│   ├── cmd_vel.rs             — CmdVel validation, DEFAULT_CMD_VEL_LIMITS
│   ├── interceptor.rs         — gateway interceptor
│   ├── kinematics_contract.rs — VehicleKinematicsContract, validate_vehicle_command
│   └── kinematics_proptest.rs — property-based tests for kinematics validation
└── bin/
    ├── kirra_verifier_service.rs — axum HTTP service, all route handlers
    └── kirra_carla_client.rs     — CARLA AV simulator integration

crates/
├── kirra-ros2-adapter/           — Occy #131 Option-B per-trajectory wiring (ros2 feature)
    ├── src/
    │   ├── lib.rs                — public surface + re-exports
    │   ├── config.rs             — VehicleConfig (envelope params, FTTI budgets)
    │   ├── state.rs              — AdaptorState, AcceptedTrajectory,
    │   │                           TrajectoryVerdict, Pose, TrajectoryPoint,
    │   │                           PerceivedObject
    │   ├── corridor/             — CorridorSource trait + impls
    │   │   ├── mod.rs            — Point, CorridorSource trait, MockCorridorSource
    │   │   ├── lanelet2.rs       — Lanelet2CorridorSource (ros2 feature)
    │   │   ├── lanelet2_bridge.rs — cxx-rs FFI declarations
    │   │   ├── lanelet2_bridge.h — C++ header (lanelet2_core / boost::serialization)
    │   │   └── lanelet2_bridge.cpp — C++ implementation
    │   ├── validation.rs         — validate_trajectory_slow (slow loop)
    │   ├── node.rs               — r2r-backed ROS 2 node, subscriptions,
    │   │                           slow / fast loop tasks, MRC publisher,
    │   │                           subscription-staleness watchdog (ros2 feature)
    │   └── bin/                  — verifier service binary (ros2 feature)
    └── tests/                    — slow / fast loop unit + integration tests
├── kirra-governor-service/       — minimal over-the-wire (UDP) governor wrapping the
│                                   EXISTING verdict core verbatim (serde + bincode + std
│                                   only — no ROS/async); two-box prototype demonstrator,
│                                   QNX cert-target path per ADR-0001
├── kirra-wire-client/            — single shared client-side mirror of the governor's UDP
│                                   wire schema (dev/test; reused by the bench + future car
│                                   bridge so the wire types are defined once)
├── kirra-proposal-bench/         — dev/test UDP bench: sweeps proposals at a running
│                                   kirra-governor-service and prints a CASE/REASON/VERDICT table
├── kirra-capture-schema/         — governor-free capture-record wire schema, shared verbatim
│                                   with the offline collector
└── kirra-collector/              — offline capture / replay collector (db3 + mcap readers)
```

The `kirra-ros2-adapter` crate is feature-gated on `ros2` (default build
produces no ROS deps; opt in via `--features ros2` against a sourced ROS 2
+ Lanelet2 environment).

### Parko ROS 2 Node (parallel path — M2)

For edge robotics / differential-drive deployments where the control
policy is an ML model rather than a planner+follower, the
**`parko/crates/parko-ros2`** crate provides a parallel ROS 2 node
that runs Parko's end-to-end ML control path live:

```
sensor topic → SensorFrame → InferenceLoop → GovernorComparator → OutgoingTwist → /cmd_vel
                  (parko-core)     (parko-kirra dual-governor;        (geometry_msgs/Twist)
                                    divergence audit + escalation)
```

Layout mirrors `kirra-ros2-adapter`:
- Default build = no ROS / no ORT deps (pure logic; `MockBackend`-based
  unit tests).
- `ros2` feature → r2r 0.9.5 + the node binary.
- `onnx-backend` feature → parko-onnx OrtBackend (production inference;
  requires `ORT_DYLIB_PATH`). CPU by default; parko-onnx's `cuda` feature
  selects the NVIDIA CUDA execution provider (fail-closed — a missing
  GPU/driver/provider errors out, never a silent CPU fallback).

Fail-closed paths: sensor staleness → stopped twist (MRC);
`InferenceLoop::tick` error → stopped twist; comparator divergence
escalation (LockedOut) → stopped twist; backend `load_model` failure
at startup → process exit with a clear error (not a silent no-op).

```bash
# Stable lane (MockBackend unit tests; no ROS / no ORT)
cd parko && cargo test -p parko-ros2

# Production lane (requires sourced ROS 2 + ORT_DYLIB_PATH)
source /opt/ros/humble/setup.bash
export ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so
cd parko && cargo build -p parko-ros2 --features ros2,onnx-backend
```

The Parko and Occy paths run **side by side**, not chained — they
produce incompatible artifacts (instantaneous commands vs. full
trajectories) and share only the safety primitives (parko-core RSS,
`VehicleKinematicsContract`, posture-driven MRC, ODD speed cap). See
[`docs/safety/PARKO_OCCY_TOPOLOGY.md`](docs/safety/PARKO_OCCY_TOPOLOGY.md)
(KIRRA-OCCY-TOPOLOGY-001) for the L1 parallel-paths decision.

### Fleet Posture States

| Posture | Command Routing |
|---------|----------------|
| `Nominal` | All commands allowed except `Unknown` |
| `Degraded` | `ReadTelemetry` only |
| `LockedOut` | All commands blocked |

### Trust Evaluation Pipeline

1. Node registers its attestation-key (AK) public key (`ak_public_pem`) and PCR16 value
2. Verifier issues a time-limited nonce challenge (TTL: 30 s)
3. Node signs the `(node_id, nonce)` challenge with its AK **private** key; the verifier verifies that Ed25519 signature against the registered `ak_public_pem` (issue #73 — node-proven identity, not admin-asserted). Fail-closed: no registered AK / bad signature → rejected. PCR16 measured-boot quote verification is a tracked follow-up.
4. Trust state (`Trusted` / `Untrusted` / `Unknown`) stored to SQLite
5. Fleet posture recalculated via DAG traversal; broadcast over SSE

### AV Sensor Recovery Pipeline

1. Sensor reports arrive with confidence score and hardware fault flag
2. Below-floor confidence or `hw_fault=true` marks the node `Untrusted` immediately
3. Recovery requires **5 consecutive healthy reports** within a **10 s window**
4. A new fault during recovery resets the streak to 0
5. Telemetry watchdog independently monitors for silence — faults at 2 s, warns at 1 s

### Occy Per-Trajectory Verdict Pipeline (#131 Option-B)

1. **Trajectory in** — planner publishes a candidate trajectory on ROS 2
2. **Slow loop** (planning rate, ~10 Hz) runs `validate_trajectory_slow`:
   - NaN/Inf rejection (SG9)
   - Speed envelope (SG1) — 50 mph hard cap
   - Drivable-space containment (SG2) — ≥ 0.40 m lateral margin vs. Lanelet2 corridor
   - RSS over horizon (SG3) — parko-core invariants vs. `PerceivedObject` set
   - Kinematic contract — velocity / lateral-accel / yaw-rate envelope
3. **Verdict** stored in `AcceptedTrajectory` as `Accept` / `Clamp` / `MRCFallback` / `Pending`
4. **Fast loop** (control rate, ~100 Hz, 200 μs budget):
   - Verifies subscription staleness (trajectory / objects / odom) within FTTI
   - On any stale feed → publishes `MRCFallback`
   - On verdict `Accept` → forwards the current trajectory point
   - On verdict `Clamp` → publishes the clamped command
   - On verdict `MRCFallback` → publishes the MRC topic the vehicle stack MUST honor
5. **CARLA harness** exercises the full pipeline against the scenario suite

See [`docs/safety/OCCY_131_OPTIONB_DESIGN.md`](docs/safety/OCCY_131_OPTIONB_DESIGN.md)
and [`docs/testing/CARLA_SCENARIO_SUITE.md`](docs/testing/CARLA_SCENARIO_SUITE.md).

---

## Getting Started

### Prerequisites

- Rust 2021 edition toolchain (`rustup`)
- A writable path for the SQLite database

### Build

```bash
# Core verifier service (no ROS deps)
cargo build --release

# Occy ROS 2 adapter (requires sourced ROS 2 + Lanelet2)
source /opt/ros/humble/setup.bash
cargo build --release -p kirra-ros2-adapter --features ros2
```

### Run

```bash
export KIRRA_ADMIN_TOKEN="your-secret-token"
export KIRRA_SUPERVISOR_RESET_KEY="your-reset-key"
cargo run --bin kirra_verifier_service
```

The service listens on `0.0.0.0:8090` by default.

### Install (Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/kirra-systems/kirra-runtime-sdk/main/install.sh | sudo bash
```

See [INSTALL.md](INSTALL.md) for full installation documentation including non-interactive mode, HA setup, and upgrade/uninstall instructions.

### Test

```bash
cargo test
```

---

## Configuration

All configuration is via environment variables.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `KIRRA_ADMIN_TOKEN` | Yes (mutation routes) | — | Bearer token for admin endpoints. Absent or empty → `503`. |
| `KIRRA_SUPERVISOR_RESET_KEY` | Yes (reset ops) | — | Reset authorization key. Must be non-empty, ≤ 64 bytes. |
| `KIRRA_VERIFIER_MODE` | No | `active` | `passive_standby` → read-only. Runtime-promotable via HA monitor. |
| `KIRRA_DB_PATH` | No | `kirra_verifier.sqlite` | Path to the SQLite database file. |
| `KIRRA_VERIFIER_ADDR` | No | `0.0.0.0:8090` | Listen address. |
| `KIRRA_TRUSTED_INGRESS_MODE` | No | `false` | Enforce `x-kirra-client-id` header on identity-gated routes. |
| `KIRRA_CLIENT_ID_HEADER` | No | `x-kirra-client-id` | Header name for client identity. |
| `KIRRA_INSTANCE_ID` | No | hostname | Unique identifier for this instance in HA deployments. |
| `KIRRA_HEARTBEAT_INTERVAL` | No | `2000` | HA heartbeat write interval (ms). |
| `KIRRA_PROMOTION_TIMEOUT` | No | `10000` | Standby promotes if primary silent for this many ms. |

---

## API Reference

### Public / Unauthenticated

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Liveness check |
| `GET` | `/ready` | Readiness check |
| `GET` | `/fleet/posture` | Current fleet-wide posture |
| `GET` | `/fleet/posture/:node_id` | Per-node posture |
| `GET` | `/fleet/history/:node_id` | Posture event history |
| `GET` | `/fleet/flapping/:node_id` | Flap detection for a node |
| `GET` | `/attestation/status/:node_id` | Node trust state |
| `GET` | `/federation/reports/:asset_id` | Federation reports for an asset |
| `POST` | `/attestation/challenge/:node_id` | Issue attestation challenge |
| `POST` | `/attestation/verify` | Submit challenge response |

### Identity-Gated (admin token + `x-kirra-client-id`)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/system/posture/stream` | SSE stream of real-time posture events |
| `POST` | `/federation/reports/submit` | Submit signed federated trust report |
| `POST` | `/action_filter/evaluate` | Evaluate an action claim against posture |
| `POST` | `/industrial/evaluate` | Evaluate a Modbus/OPC-UA industrial event |

### Admin-Only (`Authorization: Bearer <KIRRA_ADMIN_TOKEN>`)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/attestation/register` | Register a node |
| `POST` | `/fleet/dependencies` | Register dependency graph edges |
| `POST` | `/system/backup/export` | Full state dump |
| `GET` | `/system/audit/verify` | Verify audit chain integrity |
| `POST` | `/federation/controllers/register` | Register a trusted peer controller |
| `POST` | `/attestation/identity/register` | Register a hardware fingerprint |

---

## Security Model

- **Fail-closed everywhere** — any missing token, expired nonce, or verification failure results in denial, never silent pass-through.
- **Constant-time comparisons** — all token verification uses `constant_time_compare`; standard `==` is never used on security-critical byte sequences.
- **No hardcoded secrets** — `KIRRA_ADMIN_TOKEN` and `KIRRA_SUPERVISOR_RESET_KEY` must come from environment variables. No fallback values exist in code.
- **Volatile DDS durability** — actuator topics are never persisted via `TransientLocal`.
- **Ordered SQLite writes** — disk persistence always precedes in-memory state updates.
- **Nonce burning** — federation report nonces are stored and checked before acceptance; replays are rejected.
- **Posture-gated routing** — `OperationalCommand::Unknown` is rejected in all posture states, including `Nominal`.

---

## High Availability

Kirra supports active/passive HA with automatic failover.

**Primary** (`KIRRA_VERIFIER_MODE=active`): writes a heartbeat to the shared database every 2 s.

**Standby** (`KIRRA_VERIFIER_MODE=passive_standby`): polls the heartbeat. If the primary is silent for 10 s (`KIRRA_PROMOTION_TIMEOUT`), the standby automatically promotes itself to active and begins enforcing posture.

Both instances must share the same SQLite database (NFS mount, shared block storage, or equivalent).

```bash
# Primary
KIRRA_VERIFIER_MODE=active KIRRA_INSTANCE_ID=kirra-primary ./kirra_verifier_service

# Standby
KIRRA_VERIFIER_MODE=passive_standby KIRRA_INSTANCE_ID=kirra-standby ./kirra_verifier_service
```

---

## Dependencies

| Crate | Version | Purpose |
|-------|---------|----------|
| `axum` | 0.8 | HTTP framework |
| `tokio` | 1 | Async runtime |
| `tower` | 0.5 | Middleware (`KirraPolicyLayer`) |
| `dashmap` | 6 | Concurrent hashmaps |
| `rusqlite` | 0.31 (bundled) | WAL-mode SQLite persistence |
| `ed25519-dalek` | 2 | Federation signature verification |
| `hmac` + `sha2` | 0.12 / 0.10 | Attestation proof computation |
| `base64` | 0.22 | Encoding |
| `tokio-stream` | 0.1 | SSE broadcast |
| `reqwest` | 0.12 | CARLA client HTTP |
| `tracing` | 0.1 | Structured logging |
| `proptest` | 1 | Kinematics property-based tests |

---

## Releases

### v2.0.0 (in progress) — disambiguating MAJOR

**Version-line + architecture cleanup**
- **Disambiguating MAJOR cut** — root version 1.1.2 → 2.0.0, permanently clearing the
  v1.5.0-"Aegis"/v1.1.2 sort inversion (`docs/VERSIONING_POLICY.md` §2.1; A4 / #1049).
  The `ci/check_version_ordering.py` guard now confirms every active version is > 1.5.
- **ADR-0035 re-export shims removed (#1029 / A1)** — all 14 live `pub use` back-compat
  shims (plus 3 dead ones pre-2.0) are gone across four waves; every internal caller
  imports from the canonical leaf crate (`kirra_persistence`, `kirra_core`,
  `kirra_safety_authority`, `kirra_industrial`, `kirra_ota_campaign`, `kirra_fabric_types`,
  `kirra_fleet_types`). The `ci/check_reexport_shims.py` ratchet is now a permanent
  **zero-tolerance** guard (`max_shims: 0`). See the CHANGELOG `### Removed (BREAKING)`
  migration table.

**Junction safety + RSS completeness (doer-checker depth)**
- **Multi-junction routing** — `MickIntent::RouteTo { x_m, y_m }`: resolve ego + destination
  lanes, plan the lane-id route across every junction (`LaneGraph::route_to_point`, Dijkstra),
  follow the stitched `route_corridor` through each turn; re-resolved from the ego pose each tick
  (receding horizon). KIRRA bounds the corridor exactly as any other.
- **Dynamic obstacle mid-turn** — the doer yields (predictive yield) / follows (lead-match) a
  moving obstacle on the *curved* route corridor, and KIRRA's per-pose RSS bounds it; a dedicated
  mid-turn predictive-yield gap (aligned to the checker's longitudinal-conflict distance) makes the
  yield checker-*admissible* instead of fail-closing to MRC.
- **Occlusion-aware speed bound at junctions** (RSS Rule 4, lateral) — a blind junction caps the
  approach speed to the assured-clear-distance speed (`behavior::OccludedApproach`), so the ego
  CREEPS in able to stop for emergent cross-traffic; sight distance carried per approach-lane on
  `LaneGraph`.
- **Multi-modal-prediction-aware RSS** (LIVE) — the checker's `predictive_rss_breach` now runs on
  real perception: `prediction::predicted_modes_from_objects` rolls live objects into CV (+ CTRV
  when the tracker yaw feed is fresh) `PredictedMode`s, worst-cased — catching a cut-in / turn-in
  the snapshot RSS filtered as laterally clear.
- **Perception-divergence assurance monitor** (True-Redundancy analog, LIVE) — `cross_check` of two
  independent perception channels; a divergence (phantom / miss / speed mismatch) or a silent
  redundant channel maps to an MRC-floor cap composed into the Track-C derate. Channel B feeds from
  a `~/input/objects_secondary` subscription, gated on `KIRRA_PERCEPTION_REDUNDANCY_ENABLED`.
- **Learned *maneuvering* doer** — `LearnedManeuverPlanner`: the Hydra-MDP-shaped learned planner
  generalized to a 2-D (lateral offset × speed) trajectory vocabulary, so the learned scorer can
  route AROUND a hazard — bounded by KIRRA (admits a band-clearing pass that fits, rejects one that
  doesn't or a misaligned straight-through).
- **Safety-weighted perception eval** — `kirra-taj::SemanticEvalSummary`: scores the semantic
  detector at the fusion level (`UnsafeMiss` / `OverConservative` / `Correct`, `hazard_recall`,
  per-class breakdown) — a missed hazard is the headline bar, not mAP.

**S131 Option-B per-trajectory wiring on Autoware (#131 — closed)**
- New `crates/kirra-ros2-adapter` crate (feature-gated on `ros2`)
- Two-rate Governor check: slow loop @ planning rate (~10 ms budget) +
  fast loop @ control rate (200 μs budget)
- `AdaptorState`, `AcceptedTrajectory`, `TrajectoryVerdict` state machine
- `CorridorSource` trait + `MockCorridorSource` + `Lanelet2CorridorSource`
  (cxx-rs wrapper around `lanelet2_core` + `boost::serialization` for
  `LaneletMapBin.data`)
- r2r-backed ROS 2 node with trajectory / objects / odom subscriptions,
  MRC publisher, subscription-staleness watchdog (Phase 4b stamping fix)
- Typed forwarding pipeline (Phase 4c): `parse_trajectory`,
  `parse_predicted_objects`, `parse_odom`, `quat_to_yaw`
- `docs/safety/OCCY_131_OPTIONB_DESIGN.md` (KIRRA-OCCY-OPTIONB-001)
- `docs/testing/CARLA_SCENARIO_SUITE.md` — Phase 4 integrator runbook

**S8 Quantitative Evidence Package (#120 — closed)**
- **Item A** — SG2 lateral margin derivation: `CONTAINMENT_LATERAL_MARGIN_M`
  raised 0.30 → 0.40 m (PRIMARY) with 0.75 m fallback; G2 AoU on #123
  (ε_localization ≤ 0.10 m 95th-pct lateral error)
- **Item B** — D1 IDC detection-range spec: per-sensor table + SSD-derate
  cap-impact + vendor-RFP requirements
- **Item C** — Speed-cap validation matrix: 50 mph cap **unchanged**;
  PROVEN / OK-ANALYTICAL / AoU-GAP disposition for each ADR-0001 assumption
- **Item D** — SPFM / LFM / PMHF target-vs-claimed across 5 sub-elements;
  **single-supply PMHF 17.7 FIT (FAIL)** vs. **dual-supply 8.7 FIT (PASS)**
  → deployment requirement: ASIL-D-class redundant supply

**Occy safety case foundation**
- 16 new KIRRA-OCCY-* documents covering HARA, SG, ODD/SOTIF, DFA, IDC,
  two-tier architecture, integrity evidence (S3), fault model (S7),
  traceability convention, MC/DC evidence, FFI evidence, and a consolidated
  Governor Safety Manual (SEooC deliverable for integrators/assessors)
- 4 ADRs: ODD speed cap (50 mph), condition-dependent cap, two-tier
  architecture, independent safety channel (superseded)
- Perception Input Contract (#126) — 3 AoU clauses + 1 actuation clause
  on #127 + DR-1/DR-2

### v1.1.1
- Complete Aegis → Kirra rename across all source files, binaries, systemd units, ROS2 packages, Docker images, Helm charts, and documentation
- 13 bug fixes including post-rename import cleanup, binary path corrections, and CI pipeline fixes

### v1.1.0
- Multi-Asset Safety Fabric
- ASIL-D and SOTIF safety case foundation documents
- Ed25519 log signing with export and key rotation
- Action Filter with LLM integration guide
- EtherNet/IP, CANOpen, DNP3 protocol adapters
- ROS2 safety interlock package
- Docker multi-platform images and Helm chart
- CARLA integration client
- 333 tests passing, 0 failures

---

## License

See [COPYRIGHT](COPYRIGHT) for details.

© 2026 Kirra Systems, LLC. All rights reserved.
