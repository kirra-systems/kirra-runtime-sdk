# Roadmap — Kirra OS

Organized by release and by **evidence maturity**, because those are different
axes: a feature can ship long before the evidence supporting a safety claim
about it is complete.

Statuses use the [`SAFETY.md`](SAFETY.md#claim-taxonomy) taxonomy. Nothing here
is evidence for an implemented mechanism — for that, see
[`SAFETY.md`](SAFETY.md) and the source it links.

Detailed sequencing lives in [`docs/roadmap/`](docs/roadmap/) and
[`docs/safety/ROADMAP_TO_ASIL_D.md`](docs/safety/ROADMAP_TO_ASIL_D.md).

---

## Foundation

Project-level identity and navigation.

| Item | Status |
|---|---|
| Platform identity — Kirra OS, Mick, Occy, Governor | This PR |
| Architecture documentation | [`ARCHITECTURE.md`](ARCHITECTURE.md), [`docs/ARCHITECTURE_STACK.md`](docs/ARCHITECTURE_STACK.md) |
| Safety evidence index | [`SAFETY.md`](SAFETY.md), [`docs/safety/SAFETY_CASE_INDEX.md`](docs/safety/SAFETY_CASE_INDEX.md) |
| Companion definition | [`COMPANION.md`](COMPANION.md) |
| Constitution | [`CONSTITUTION.md`](CONSTITUTION.md) |

---

## Kirra OS 1.0

Making a single robot genuinely usable through conversation, with every motion
governed.

| Item | Status | Notes |
|---|---|---|
| Stable local conversation | **Implemented / Tested** | Local Ollama; deterministic identity and memory routes; sentence-safe output |
| Governed explicit motion | **Implemented / Tested** | Text → typed intent → doer → checker → release token → verifying consumer |
| Semantic destination admission | **Implemented / Tested** | Language picks the destination *type*; a trusted resolver picks the coordinates → [`docs/hardware/R2_DESTINATIONS.md`](docs/hardware/R2_DESTINATIONS.md) |
| **Frame-aware governed destination bridge** | **Planned** | The grounded-destination latch exists; the frame-aware consumer that would let it drive does not. Must reject unknown or mismatched `map_id`, stale sequence, duplicate sequence, unsupported frame, route metadata mistaken for a complete route, and a missing map↔planner transform |
| Trusted world-state inputs | **Partial** | Perception and registry paths implemented; broader world state in progress |
| Robot installer | **Implemented** | [`robot/install/`](robot/install/), [`deploy/systemd/install.sh`](deploy/systemd/install.sh) |
| Diagnostics | **Implemented** | [`robot/doctor/`](robot/doctor/), [`docs/diagnostics.md`](docs/diagnostics.md) |
| Hardware verification | **In progress** | Rosmaster R2 + Jetson Orin NX reference → [`docs/hardware/RABBIT_BRINGUP_RUNBOOK.md`](docs/hardware/RABBIT_BRINGUP_RUNBOOK.md) |

> **Honest status.** Voice-to-destination today performs **resolution and
> admission**, not completed navigation. A spoken destination resolves to a
> trusted pose and is admitted by the checker; the bridge that would carry it
> to the wheels is the planned work above. Do not read the implemented rows as
> "the robot drives where you tell it."

---

## Kirra OS 1.5

Persistence, richer world state, and the beginnings of a platform.

| Item | Status |
|---|---|
| Persistent places and objects | Planned |
| Object-goal relay | Partial — planner-side resolver implemented; end-to-end relay planned |
| Route sequencing (multi-waypoint) | Planned — routes currently ground to their first waypoint only |
| Dashboard | Planned |
| Skill SDK | Planned |
| More hardware platforms | Planned |

---

## Kirra OS 2.0

Companion workflows — the robot doing a *job*, not a manoeuvre.

| Item | Status |
|---|---|
| Companion workflows | Conceptual |
| Inspection | Conceptual |
| Delivery | Conceptual |
| Follow-me | Conceptual |
| Charging | Conceptual |
| Home Assistant integration | Conceptual |
| Fleet operations | Conceptual |

Kirra Studio and Kirra Fleet are product names for this horizon. Neither
exists.

---

## Safety evidence and assessment

Tracked explicitly as work, never implied as complete. Current state of every
row: **not started or in progress**.

| Item | Status |
|---|---|
| Safety-case documents through formal confirmation review | **Planned** — all currently Draft |
| Ferrocene toolchain qualification | **Planned** |
| QNX target cross-compile and on-target WCET measurement | **In progress / blocked on hardware** → [`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`](docs/safety/WCET_MEASUREMENT_METHODOLOGY.md) |
| Hardware fault-injection campaigns | **Planned** → [`docs/safety/HV_FAULT_CAMPAIGN.md`](docs/safety/HV_FAULT_CAMPAIGN.md) |
| Independent third-party assessment (ISO 26262) | **Planned — not started** |
| Independent third-party assessment (IEC 61508) | **Planned — not started** |
| Certification body engagement | **Planned — not started** |

→ [`docs/safety/ROADMAP_TO_ASIL_D.md`](docs/safety/ROADMAP_TO_ASIL_D.md),
[`docs/safety/RTM_GAP_REPORT.md`](docs/safety/RTM_GAP_REPORT.md)

---

## Integrations

| Integration | Status |
|---|---|
| Autoware (Option-B two-rate checker) | **Implemented** → [`docs/safety/OCCY_131_OPTIONB_DESIGN.md`](docs/safety/OCCY_131_OPTIONB_DESIGN.md) |
| IEEE 2846 / RSS | **Implemented** → [`docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md`](docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md) |
| QNX governor transport lane | **In progress** — specs landed; on-target work blocked on hardware |
| Apollo AV stack | **Planned** → [`docs/roadmap/APOLLO_KIRRA_INTEGRATION.md`](docs/roadmap/APOLLO_KIRRA_INTEGRATION.md) |
| Postgres shared control-plane state | **Implemented** → [`docs/adr/0038-postgres-shared-state-hybrid.md`](docs/adr/0038-postgres-shared-state-hybrid.md) |

---

## Reading this roadmap

Three distinctions to keep straight:

- **Implemented** means the mechanism exists and is tested. It does not mean
  it is assessed.
- **Planned** means intended and not built. No planned row should be cited as
  a capability.
- **Conceptual** means a direction, not a commitment.

A roadmap entry is never evidence. If you need to know whether something
works, follow the links in [`SAFETY.md`](SAFETY.md) to the source.
