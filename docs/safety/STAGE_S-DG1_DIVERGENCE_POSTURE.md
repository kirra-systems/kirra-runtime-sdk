# Stage S-DG1 — Governor-Divergence → Fleet Posture

> **STATUS: SUBSTANTIALLY IMPLEMENTED (WP-09, 2026-07-07) — pending safety-engineer
> sign-off.** S-DG1a (verifier: `LockoutReason::GovernorDivergence`,
> `PostureRecalcTrigger::GovernorDivergence`, `apply_governor_divergence_state`,
> composition into the Degraded/LockedOut clauses AND the #688 sticky-downgrade guard),
> S-DG1b (parko-kirra: `PostureSignalSink` DI seam on `GovernorComparator`, emission
> reusing the comparator's own accumulator/escalation state — `None` = byte-identical
> audit-only), and the S-DG1c ADAPTER (`PostureEngineSenderSink`, `verifier-sink`
> feature: non-blocking `try_send`, fail-safe drop semantics) are LIVE with the full
> §5 test matrix (see below) plus an end-to-end worker test
> (`governor_divergence_trigger_drives_fleet_posture_through_worker`).
>
> **Remaining before ENFORCED:** the live parko-ros2 NODE wiring. The node reaches the
> verifier via `StoreHandle` (remote topology), not an embedded posture engine, so
> wiring `with_posture_sink(PostureEngineSenderSink)` there needs either an embedded
> engine or a remote trigger transport — follow-up. An integrator embedding the engine
> can wire it today with the shipped pieces. The RTM must NOT list S-DG1 as ENFORCED
> until that lands. (Same governance as S-FI1.)

## 1. Motivation

Parko runs **two independent governors** over every command — a primary
(`DiverseKirraGovernor`) and a shadow — and a `GovernorComparator` reconciles them to a
safe output. A *disagreement* between two independently-derived safety governors is one of
the strongest fault signatures the system can observe: it means at least one governor is
wrong, and we cannot tell which. Today that disagreement is **reconciled locally and
audited** (`DivergenceEventSink`) — but it does **not** drive the fleet posture. A
persistent governor disagreement should fail the fleet *closed*, exactly as a stale
localization frame does (S-FI1). This stage promotes the existing local divergence signal
into a first-class, fail-closed **posture** input.

This is the diverse-redundancy analogue of the frame-integrity gate: turn a *detected*
fault from "logged" into "drives the safe state."

## 2. Current state (grounded in real symbols)

- `parko_kirra::comparator::GovernorComparator` compares primary vs shadow and produces a
  reconciled command.
- `comparator::DivergenceEvent` already carries: `primary_lin/shadow_lin/delta_lin`,
  `primary_ang/shadow_ang/delta_ang`, an **`accumulator: u32`** (sustained-divergence
  counter), the **`reconciled_lin/reconciled_ang`** safe output, and an
  **`escalated_to_lockout: bool`** flag.
- `comparator::DivergenceEventSink` (+ `AuditChainLinkerDivergenceSink`) durably,
  signed-and-hash-linked, **audits** each divergence.
- **Gap:** the `accumulator` / `escalated_to_lockout` state affects the *local command
  reconciliation* and the *audit*, but is **not wired to the kirra-core posture engine**
  (`FleetPosture` / `posture_engine_v2`). A diverging fleet keeps running at Nominal posture.

So the detection, accumulation, and escalation logic **already exist**; S-DG1 is the
*wiring* of that existing signal into posture — it does not invent a new divergence metric.

## 3. Design

**Reuse the comparator's existing accumulator + escalation; do not invent a new criterion.**

- **Posture-relevant divergence = the comparator's `accumulator`-significant divergence**
  (its existing threshold), NOT every nonzero `delta_lin/ang`. Small clamp-magnitude deltas
  are *expected* from diverse computation (two correct-but-different governors will not
  produce bit-identical clamps); the existing accumulator already filters those. Keying
  posture off the accumulator threshold means posture reacts to *sustained, significant*
  disagreement, not numerical noise.
- **Mapping (mirrors S-FI1 / SS-002):**
  - accumulator crosses its significance threshold → **immediate `Degraded`** (decel-to-stop
    -and-HOLD MRC). The first significant tick drops posture; no grace period.
  - existing `escalated_to_lockout` → **`LockedOut`** (human reset) — a sustained / repeated
    disagreement is a genuine fault (a real governor bug or a corrupted input), not a
    transient, so it earns a human inspection.
  - divergence clears → **auto-recover** via the existing `recovery_hysteresis` machinery
    (N consecutive agreeing ticks), Degraded → Nominal.
- **Defense-in-depth unchanged:** the comparator STILL reconciles every command to the safe
  output regardless of posture. Posture is an *additional* fail-closed consequence, never a
  replacement for reconciliation.

### New kirra-core surface (additive)
- `posture_engine_v2::LockoutReason::GovernorDivergence` (new structured reason code).
- `posture_engine_v2::PostureRecalcTrigger::GovernorDivergence { significant, escalated }`
  (typed trigger), mirroring the frame-integrity trigger shape.

### Cross-workspace emission (the key mechanism)
parko is a **separate workspace** depending on kirra-core; it must drive the verifier's
posture engine without a dependency inversion. So the comparator gains an **optional
posture-trigger sink** — `PostureSignalSink` — dependency-injected exactly like the
existing `DivergenceEventSink`. parko-ros2 wires it to the `PostureEngineSender`; an
integrator who runs the comparator standalone leaves it `None` (audit-only, today's
behavior). No new hard edge parko→kirra-core posture internals.

## 4. Sub-stages

- **S-DG1a** — kirra-core: add `LockoutReason::GovernorDivergence` +
  `PostureRecalcTrigger::GovernorDivergence`. Module + posture-engine handling only;
  touches nothing in parko. (The exhaustive `LockoutReason`/trigger matches force a
  compile error to handle it — no silent gap.)
- **S-DG1b** — parko-kirra: add the optional `PostureSignalSink` to `GovernorComparator`;
  emit on the accumulator-significant / escalated transitions (reusing the existing state).
- **S-DG1c** — parko-ros2: wire the sink to the `PostureEngineSender`; integration test.
- Each sub-stage held for review; S-DG1c is the first that changes runtime behavior.

## 5. Test matrix

| Case | Expectation |
|------|-------------|
| tiny clamp-magnitude delta (below accumulator threshold) | audit yes, **posture unchanged** |
| accumulator crosses significance threshold | **Degraded** (immediate), audit yes |
| sustained / `escalated_to_lockout` | **LockedOut**, audit yes |
| divergence clears, N agreeing ticks | auto-recover Degraded → Nominal |
| asymmetry test | every divergence is audited; only *significant* divergence moves posture |
| `PostureSignalSink = None` | byte-for-byte today's behavior (audit-only) |

## 6. Assumptions of use / honesty

- This **strengthens** diverse-redundancy from "divergence is recorded" to "sustained
  divergence fails the fleet closed." It does not change the reconciled command path.
- It does **not** claim to identify *which* governor is wrong — it cannot. The safe
  disposition under "one of two governors is wrong, unknown which" is to stop (Degraded)
  and, if it persists, require a human (LockedOut). Diagnosing the offending governor is a
  separate offline activity over the signed divergence audit.

## 7. Decisions needed before S-DG1a

1. **Significance threshold** — reuse the comparator's existing `accumulator` threshold for
   posture-significance (recommended — one source of truth, already validated), or introduce
   a separate posture threshold? Recommend **reuse**.
2. **Immediate-Degraded** — confirm posture drops on the *first* significant tick (no grace);
   graduation governs only Degraded→LockedOut. Recommend **confirm** (consistent with S-FI1).
3. **Emission mechanism** — the dependency-injected `PostureSignalSink` (recommended, mirrors
   `DivergenceEventSink`) vs a direct `PostureEngineSender` handle threaded into the
   comparator? Recommend the **DI sink**.
