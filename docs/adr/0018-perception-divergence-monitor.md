# ADR-0018: Perception-divergence monitor + sustained-divergence posture escalation

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG9** (acts on stale/invalid world state — a phantom, a miss, or a silent redundant channel means the perceived world cannot be trusted), **SG8** (degraded/MRC posture — a sustained fault must drive the fleet posture, not just a per-tick cap) |
| Cross-refs | gap #2b (competitive analysis); True-Redundancy doctrine; code: `crates/kirra-ros2-adapter/src/perception_redundancy.rs` (`cross_check`, `resolve_redundancy_cap`, `DivergenceEscalator`), composes into `kirra_core::perception_monitor::apply_perception_cap`, escalates via `kirra_core::FleetPosture::escalate`, wired in `crates/kirra-ros2-adapter/src/node.rs` (slow loop, ros2-gated); tests: `crates/kirra-ros2-adapter/src/perception_redundancy.rs` unit tests |

## Context

A single perception channel has no way to know it is wrong: a phantom object, a missed object, or a
frozen feed all look like valid perception from the inside. SG9 (act only on a trustworthy world
state) therefore needs **independent redundancy** — a second perception channel that must AGREE.
Separately, a *momentary* glitch and a *sustained* sensor failure are not the same hazard: the
first is handled by a one-tick speed cap, but the second means the redundant channel is genuinely
gone and the system should no longer run at Nominal posture (SG8).

## Decision

**Two-channel cross-check (SG9, per-tick cap).** `cross_check` requires the primary and a secondary
perception channel to agree (object correspondence + speed). Any divergence — a phantom in one
channel, a miss, a speed mismatch — OR a **stale/silent secondary** (redundancy lost) →
`resolve_redundancy_cap` returns the MRC-floor `Some(0.0)`, composed into the Track-C
`apply_perception_cap` derate via the more-restrictive-cap rule. Env-gated
(`KIRRA_PERCEPTION_REDUNDANCY_ENABLED`); **disabled ⇒ inert**, byte-identical prior behaviour.

**Sustained-divergence escalation (SG8, posture).** `DivergenceEscalator` tracks how long the
divergence has persisted: a **momentary** divergence holds posture at Nominal (the per-tick MRC cap
already covers it); divergence sustained past `DIVERGENCE_DEGRADE_MS` recommends `Degraded`, and past
`DIVERGENCE_LOCKOUT_MS` recommends `LockedOut`. The slow loop folds this in **escalation-only** via
`posture.escalate(escalator.recommended_posture(now))` — it can make the effective posture stricter,
never relax it. A divergence that clears resets the streak.

## Consequences

- **Positive:** a wrong-but-confident single channel can no longer drive the vehicle unchecked; a
  lost redundant channel fails closed; and a *persistent* perception fault escalates the fleet
  posture (not just one tick's speed), so recovery requires the posture to clear, not merely the
  current frame to look fine.
- **Fail-closed / derate-only:** the cap is `Some(0.0)` (MRC floor) or `None` (no-op) — never a
  relaxation; the escalation is monotone-up only. Disabled / consistent / fresh-and-agreeing ⇒ no
  cap and no escalation (`disabled_monitor_is_inert…`, `a_consistent_stream_never_escalates`).
- **Honest scope:** "two independent channels" is an integrator wiring contract — the monitor
  asserts agreement, it does not by itself guarantee the channels are diverse in sensor/algorithm.
- **Composition proven:** the divergence cap folds into the existing perception derate, and the
  escalation rides the same `FleetPosture::escalate` lattice the frame-integrity and RSS couplers use.
