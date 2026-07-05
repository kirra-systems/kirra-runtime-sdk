# Kirra — Engineering Roadmap

This directory contains pre-execution architecture sketches and execution plans
for planned integrations and extensions. Each document represents a reviewed
and approved roadmap item with honest caveats, effort estimates, and explicit
sequencing dependencies.

For the full task-level roadmap (PARK-001 through PARK-040), see
[/work/roadmap.md](/work/roadmap.md).

## Documents

| Document | Description | Priority | Gating Dependencies | PARK Tasks |
|----------|-------------|----------|---------------------|------------|
| [RSS_KIRRA_INTEGRATION.md](RSS_KIRRA_INTEGRATION.md) | IEEE 2846 / RSS-style behavioral safety extension — canonical perception types, safe-distance evaluator, posture engine wiring, and audit chain entries | After Increment 2 (HAL complete) | parko-core v0.1.0 tagged (PARK-006); InferenceBackend finalized (PARK-008) | PARK-013 – PARK-019 |
| [APOLLO_KIRRA_INTEGRATION.md](APOLLO_KIRRA_INTEGRATION.md) | Apollo AV stack integration — Cyber RT bridge between Control and Canbus, posture sideband, demo scenarios | After Increment 3 + ROS2 demo | QNX spike (PARK-024); ROS2 bring-up (PARK-036, PARK-037) | Extends Increment 4 |

## Current Execution Order

> **Live status is not tracked in this file.** This directory holds pre-execution
> architecture sketches (the Documents table above) — those don't go stale. The
> authoritative, current task-level roadmap + work-in-progress status lives in
> [`/work/roadmap.md`](/work/roadmap.md) and [`/work/active.md`](/work/active.md),
> with GitHub issue state as the other source of truth. An increment snapshot used
> to be duplicated here and had drifted badly (it listed long-completed
> Increment-1 tasks — attach-governor, test seam, divergence proptest — as
> "in progress," while the tree is well past that: parko backends, the RSS/IEEE-2846
> behavioral-safety layer, and the QNX governor-judge harness all exist). It was
> removed in favor of the single source above. The stable design constraints that
> govern any sequencing are kept below.

## Sequencing Rules

1. Do not start Increment 3 (RSS / IEEE 2846) before Increment 1 is tagged.
2. Do not start Apollo integration before the ROS2 interlock demo (PARK-037) is on record.
3. Do not let PARK-024 (QNX, 30-day license) slip — treat as a hard deadline regardless of other increment progress.
4. The RSS evaluator and Apollo bridge are capability extensions; they add value to an already-working product. Core runtime + QNX + hardware validation come first.
