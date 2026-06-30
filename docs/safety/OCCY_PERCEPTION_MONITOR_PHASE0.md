# Occy / KIRRA — Track-C Perception Monitor, Phase-0 Design (Kinematic-Plausibility + Range-Based Derate)

**Doc ID (proposed):** KIRRA-OCCY-PMON-001.
**Status:** Design doc for review. No code in this commit — this specifies the
two highest-leverage Phase-0 guards from the perception-upgrade track re-cut, so
implementation can proceed against a fixed contract. Derived from
`kirra_perception_upgrade_tracks.md` (Track C, "highest leverage first").
**Scope:** Track C only — the *independent governor* side of the boundary. Track A
(perception models) and Track B (Parko inference runtime) are out of scope here by
construction; this doc never proposes a learned-inference component.
**Supersedes/extends:** composes onto ADR-0001 (ODD speed cap), ADR-0002
(condition-dependent cap + sub-ODD partition), ADR-0004 (independent safety
channel). Reuses the SG2 containment pattern (`gateway/containment.rs`,
KIRRA-OCCY-SG2-MARGIN-001) as the structural template.

---

## 0. The one principle (why this is Track C, not Track A)

```
[raw sensors] → Parko (Track B: governed inference) → Occy world model + plan (Track A)
             → KIRRA (Track C: independent checks + range-based derate + fail-closed) → actuators
```

KIRRA is the **independent governor** (ADR-0004). It is **agnostic to which models
Occy/Parko run** — it checks their *outputs*, never their internals. Both Phase-0
guards are simple, deterministic, bounded analytic computations (kinematics, set
membership, thresholds, monotone arithmetic) over Track-A **outputs**. Neither is
learned inference; neither hosts a perception DNN on the verdict path. This is the
line between a **monitor** and a **perceiver**: the monitor *ingests and checks*
detections; it does **not** produce them.

**Non-negotiable Track-C constraints (from the track re-cut §"Track-C constraints"),
restated as acceptance gates for this design:**

| # | Constraint | How Phase-0 satisfies it |
|---|------------|--------------------------|
| C1 | No perception DNN on the verdict path | Both guards are bounded analytic functions; if a check needed a detector to *work*, it would be Track A. |
| C2 | Verdict path stays byte-stable / bounded-WCET / fail-closed | The Nominal hot path `validate_vehicle_command` is **unchanged**. Both guards are **sibling** entry points (exactly as `validate_trajectory_containment` is today), WCET-profiled via `wcet_gate`. |
| C3 | Agnostic to Track-A model choice | Inputs are a typed *perception-output contract* (object lists, observed detection range), not model handles. Any Track-A model swap is adoptable with zero KIRRA change. |

---

## 1. Phase-0 scope (this doc) vs. deferred Track-C slices

**In scope (Phase-0 — the chosen first slice):**

1. **Kinematic-plausibility guard** — reject implausible object kinematics:
   impossible velocities (beyond an urban kinematic ceiling) and teleporting
   objects (frame-to-frame position jumps that imply impossible speed). §4.
2. **Range-based derate pipeline** — derate the permitted speed envelope on
   *observed* detection-range degradation, realizing ADR-0001 rule 1
   (cap-as-function-of-R) and the `range_supported(R)` term of the ADR-0002
   composition. Covers rain/fog/night/dirt **uniformly** with no weather
   classifier; downward-only; no hysteresis on the drop. §5.

**Explicitly deferred to later Track-C slices (named here so the contract in §3 is
forward-compatible, but NOT designed in this doc):**

- Cross-sensor disagreement → disagreement-driven derate.
- Map-vs-perception disagreement → derate.
- Localization-degradation flag → localization-driven derate.
- Unknown-region (occupancy-uncertainty) speed cap.
- The full Kirra scene-graph **monitor** structure (ego + static map + dynamic
  objects + occupancy + uncertainty) that will host all of the above. Phase-0
  intentionally implements its two guards as standalone bounded functions; the
  scene-graph aggregator is a later slice once ≥ 2 guards exist to aggregate.

**The one real cross-track coupling (from the re-cut §Sequencing):** Track-B
baseline **detection-range profiling feeds** the §5 range-based derate. The derate
consumes a *measured observed range* `R_obs`; producing `R_obs` per sensor under
degraded conditions is Track-B work (OCCY_SPEED_CAP_VALIDATION.md Item B / #120).
Phase-0 defines the *interface* (§3) and the *transfer function* (§5); it does not
depend on the profiling being complete to land the guard with synthetic/contract
inputs (same staging the SG2 containment check used — unit-tested against
constructed inputs, live-wiring filed as a follow-up).

---

## 2. Disposition model — why both guards emit a *derate*, not a *deny*

The existing Governor verdict type is `EnforceAction`
(`gateway/kinematics_contract.rs`):

```
EnforceAction = Allow | ClampLinear(f64) | ClampSteering(f64) | DenyBreach(DenyCode)
```

A SG2 containment breach is a hard binary (`DenyBreach`) because the *ego command
itself* departs the drivable space. Phase-0's two guards are different in kind: a
failed plausibility check or a degraded detection range means **the perception
input is less trustworthy**, not that this specific ego command is illegal. The
correct, proportionate disposition is therefore a **derate** — reduce the permitted
speed envelope — which composes into the ADR-0002 cap and, when the derate forces
the commanded speed below the proposed value, surfaces as the existing
`EnforceAction::ClampLinear(cap)` on the command path. A *severe* plausibility
failure (e.g. perception output structurally invalid) derates to the **MRC floor**,
which is observationally a controlled stop — fail-closed, but via the derate
channel, not a new deny code on the hot path.

This keeps the two Phase-0 guards **homogeneous in output** (both produce a
`DerateDecision`), keeps `DenyCode` and `validate_vehicle_command` untouched (C2),
and reuses `ClampLinear` rather than inventing a parallel clamp path.

```
PerceptionOutput ──▶ kinematic_plausibility_derate ─┐
                                                     ├─▶ min() ─▶ permitted speed cap ─▶ (ADR-0002 composition)
observed range   ──▶ range_supported_derate ───────┘                                     │
                                                                                          ▼
                                                              cap < proposed?  →  EnforceAction::ClampLinear(cap)
                                                              cap ≈ 0 (MRC)    →  controlled stop
```

`min()` is the same most-conservative-wins composition ADR-0002 already mandates;
Phase-0 adds two new inputs to that min, it does not change the rule.

---

## 3. The Perception-Output Contract (what KIRRA ingests)

Phase-0 reads a **bounded, typed snapshot** of Track-A output. This is the KIRRA-side
view of the SEooC Perception Input Contract (#126); it is deliberately minimal —
only what the two Phase-0 guards consume — and additive-only so later slices extend
it without breaking Phase-0.

**Bounding (WCET discipline, mirrors `containment::MAX_*`):**

- `MAX_TRACKED_OBJECTS` — hard cap on objects inspected per cycle (proposed: 256).
  Over-cap → conservative derate (treat as degraded perception), never silent
  truncation.
- Every scalar is `f64`/`f32`; every field NaN/Inf-checked at the guard boundary
  (fail-closed on non-finite, exactly as `containment::*_is_finite`).
- Snapshot carries `confidence` and `age_ms` with `min_confidence` / `max_age_ms`
  health gates, identical in spirit to `Corridor::is_healthy` — absent / stale /
  low-confidence snapshot → conservative derate.

**Phase-0 fields (proposed shape — names illustrative, to be finalized in impl):**

| Field | Type | Consumed by | Notes |
|-------|------|-------------|-------|
| `objects: &[TrackedObject]` | bounded slice | §4 | each: `id`, `pos_m {x,y}`, `vel_mps {x,y}`, `prev_pos_m`, `dt_s` |
| `observed_range_m: f64` | scalar | §5 | S8/Track-B worst-case measured detection range `R_obs` |
| `range_confidence: f32` | scalar | §5 | below floor → conservative (treat `R_obs` as 0) |
| `snapshot_age_ms: u64` | scalar | §4, §5 | stale → conservative derate |

A `TrackedObject`'s `prev_pos_m` + `dt_s` let the teleport check run **statelessly**
(no KIRRA-side track memory needed in Phase-0 — the contract supplies the prior
frame). This keeps the guard a pure function (testable, no `SystemTime::now()`,
takes `now_ms`/`dt` as input per the repo's testability rule).

---

## 4. Guard 1 — Kinematic-plausibility derate

**Requirement ID:** KIRRA-OCCY-PMON-KIN-001.
**Safety tag:** perception-integrity monitor; supports SG8 (envelope) via
conservative derate; feeds the §2 disposition model.

### 4.1 Checks (all bounded analytic, per object)

For each `TrackedObject` (≤ `MAX_TRACKED_OBJECTS`):

1. **Finite guard.** Any non-finite field → object is implausible.
2. **Velocity-ceiling check.** `|vel_mps| > V_OBJECT_MAX_MPS` → implausible. The
   ceiling is an *urban kinematics* bound (an object reported faster than any
   plausible urban actor is a sensor/tracker artifact, OR a genuine fast approach —
   both warrant conservatism). Proposed `V_OBJECT_MAX_MPS` derived from the ADR-0001
   ODD (urban surface streets) + a margin; exact value set with a `Safety:`-tagged
   derivation note like CONTAINMENT_LATERAL_MARGIN_M.
3. **Teleport / frame-jump check.** Implied speed
   `|pos_m − prev_pos_m| / dt_s > V_OBJECT_MAX_MPS` (with `dt_s > 0`, else
   conservative) → object teleported between frames → implausible. Catches
   track-ID swaps and detection flicker that velocity alone misses.

### 4.2 Aggregation → derate (downward-only, conservative)

- Count implausible objects. The derate magnitude is a **monotone non-increasing**
  function of the implausible fraction:
  - 0 implausible → no derate (cap unchanged).
  - ≥ 1 implausible → derate the permitted cap (perception partially untrusted).
  - Structural failure (snapshot unhealthy, over-cap object count, all-objects
    implausible) → derate to **MRC floor** (≈ controlled stop). Fail-closed.
- The transfer function is a small fixed table / clamped linear map — **no learned
  component, no hysteresis on the drop** (consistent with ADR-0002 rule 4
  "drop down instantly"). Recovery (derate relaxing as objects become plausible
  again) MAY use the existing recovery-hysteresis discipline
  (`recovery_hysteresis.rs`, AV_RECOVERY_STREAK_THRESHOLD) so a single good frame
  doesn't bounce the cap back up — *up*-transitions are streak-gated, *down* are
  instant. (Asymmetric, matching ADR-0002 and the existing AV recovery pattern.)

### 4.3 Signature (proposed — pure, bounded, no allocation)

```
#[must_use]
pub fn kinematic_plausibility_derate(
    perception: &PerceptionOutput,   // bounded; health-gated
    contract:   &KinematicPlausibilityContract, // V_OBJECT_MAX_MPS, floors, caps
) -> DerateDecision                  // { cap_mps: f64, reason: DerateCode }
```

Bounded properties (for `wcet_gate`): `O(MAX_TRACKED_OBJECTS)` finite-and-threshold
work; no heap allocation; no recursion; scalar `f64` only — same WCET shape as
`validate_trajectory_containment`.

---

## 5. Guard 2 — Range-based derate

**Requirement ID:** KIRRA-OCCY-PMON-RNG-001.
**Safety tag:** realizes ADR-0001 rule 1 (cap = f(R)) and the `range_supported`
term of the ADR-0002 composition.

### 5.1 Transfer function (the core)

From ADR-0001 (SPEED_ENVELOPE.md §5–6): the cap is bounded by the speed whose
stopping-sight-distance fits inside the observed detection range, `SSD(v) ≤ R_obs`.
Inverting the SSD model gives the **range-supported cap**:

```
range_supported(R_obs) = v_max such that SSD(v_max) = R_obs
SSD(v) = v·t_react + v² / (2·a_brake)     // reaction + braking distance
```

with `t_react` (FTTI/actuation latency chain, S3/#115) and `a_brake` (the comfortable
decel basis, `VehicleKinematicsContract::max_brake_mps2`, see
OCCY_SPEED_CAP_VALIDATION.md item 3). Solve the quadratic for `v_max ≥ 0`.

**Properties (all structural, no classifier):**

- **Condition-agnostic.** The derate keys on the *observed range* `R_obs` only.
  Rain, fog, night, dirt, sensor degradation all manifest as a shorter `R_obs` and
  are handled **uniformly** — there is deliberately **no weather classifier** (a
  classifier would be Track A; this is Track C).
- **Downward-only, no hysteresis on the drop.** `R_obs` falls → cap drops same
  cycle (ADR-0002 rule 4). Cap *raise* on `R_obs` recovery is streak/confirmation
  gated (asymmetric), never instantaneous.
- **Conservative on bad input.** `range_confidence < floor`, stale snapshot, or
  non-finite `R_obs` → treat `R_obs = 0` → cap → MRC floor. Fail-closed.
- **Monotone.** `range_supported` is monotone non-decreasing in `R_obs` — a longer
  observed range never lowers the cap. (A property worth a proptest, like
  `kinematics_proptest.rs`.)

### 5.2 Composition (unchanged ADR-0002 rule)

```
cap = min( subODD_nominal(confirmed sub-ODD),   // ADR-0002 rule 1/2 (existing)
           weather_derate(conditions),          // ADR-0002 (existing; #99)
           range_supported(R_obs),              // THIS guard — Phase-0 makes it executable
           kinematic_plausibility_cap )          // §4 — new Phase-0 input
```

Phase-0 turns `range_supported` from a documented rule into a runtime function and
adds the §4 input. The min-composition itself is not changed (C2).

### 5.3 Signature (proposed)

```
#[must_use]
pub fn range_supported_derate(
    observed_range_m: f64,
    range_confidence: f32,
    snapshot_age_ms:  u64,
    contract: &RangeDerateContract,  // t_react, a_brake, floors, sub-ODD nominal
) -> DerateDecision
```

Pure, constant-time (one quadratic solve + health gates), no allocation.

---

## 6. Reason-coded derate logging (audit chain)

Both guards' `DerateDecision` carries a `DerateCode` reason, logged into the
existing **SHA-256 / Ed25519 audit chain** (`audit_chain.rs`,
`audit_log_chain` table — the work is already done; Phase-0 only *feeds* it). Mirror
the `DenyCode` design exactly (`gateway/kinematics_contract.rs`):

- `#[derive(Copy, Clone, Eq, Serialize)]`, `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`,
  a `const fn reason() -> &'static str`, and a `Display` impl — so the audit token is
  byte-stable and allocation-free on the check path (S3/#115 discipline).
- Proposed Phase-0 variants:
  `OBJECT_VELOCITY_IMPLAUSIBLE`, `OBJECT_FRAME_TELEPORT`,
  `PERCEPTION_SNAPSHOT_UNHEALTHY`, `DETECTION_RANGE_DEGRADED`,
  `DETECTION_RANGE_UNTRUSTED`.
- A stable-token unit test per variant (like
  `deny_code_drivable_space_departure_renders_stable_token`).

---

## 7. Integration points & what stays untouched

| Surface | Phase-0 change | Constraint |
|---------|----------------|------------|
| `validate_vehicle_command` (Nominal hot path) | **none** — byte-stable | C2 / INV preserved as in #70, #165 |
| `gateway/containment.rs` | none — reused only as the structural template | — |
| New module (proposed) `gateway/perception_monitor.rs` | two pure functions + contracts + `DerateCode` | sibling entry, like containment |
| Cap composition (ADR-0002) | add two inputs to existing `min()` | rule unchanged |
| `audit_chain.rs` | feed `DerateCode` reasons | substrate already exists |
| `wcet_gate.rs` | add both guards to the WCET budget set | bounded-WCET evidence |

**Live-wiring** of the derate onto real command traffic is a **follow-up issue**
(the same staging SG2 containment went through before it graduated to ENFORCED,
#128/#131): Phase-0 lands the guards as
unit-tested pure functions against contract/synthetic inputs; activation on live
`ProposedVehicleCommand` traffic + the Track-B `R_obs` feed is filed separately.

---

## 8. Test plan (to author with the impl)

Mirror the containment test density (analytic guards demand exhaustive boundary +
MC/DC coverage):

- **Guard 1:** velocity at/over `V_OBJECT_MAX_MPS`; teleport at/over the implied-speed
  bound; `dt_s ≤ 0` → conservative; NaN/Inf in each object field; over-`MAX_TRACKED_OBJECTS`
  → MRC-floor derate; all-plausible → no derate; mixed fraction → graded derate;
  unhealthy snapshot → conservative.
- **Guard 2:** `R_obs` boundary cases (just above/below each `SSD(v)` inflection);
  `R_obs = 0` → MRC floor; monotonicity **proptest** (`proptest`, dev-dep already
  present); low `range_confidence` / stale → conservative; non-finite `R_obs`.
- **Composition:** `min()` picks the most conservative of the (now four) inputs;
  adding the two new inputs never *raises* the cap above the legacy two.
- **Asymmetry:** down-transition instant; up-transition streak-gated (reuse
  `recovery_hysteresis` semantics).
- **Audit:** stable-token test per `DerateCode` variant.

---

## 9. Traceability

| Item | Reference |
|------|-----------|
| Boundary / doer-checker | ADR-0004 (independent safety channel); track re-cut §0 |
| Cap = f(R) rule | ADR-0001 rule 1; SPEED_ENVELOPE.md §5–6 |
| min()-composition + asymmetric transitions | ADR-0002 rules 2 & 4 |
| Conservative-on-bad-input pattern | `gateway/containment.rs` (`is_healthy`); OCCY_FAULT_MODEL.md §3 |
| `R_obs` source (the one cross-track coupling) | Track-B profiling; OCCY_SPEED_CAP_VALIDATION.md Item B / #120 |
| Perception Input Contract | #126 (SEooC); OCCY_DFA.md §; this doc §3 is the KIRRA-side view |
| Audit substrate | `audit_chain.rs`, `audit_log_chain` table (done) |
| Recovery asymmetry | `recovery_hysteresis.rs` (AV_RECOVERY_STREAK_THRESHOLD) |
| Verdict-path preservation | C2; #70 / #165 (`validate_vehicle_command` byte-stable) |

---

## 10. Open decisions for review (before impl)

1. **`V_OBJECT_MAX_MPS` value + derivation** — needs a `Safety:`-tagged derivation
   note (like KIRRA-OCCY-SG2-MARGIN-001). Urban ODD object-class kinematics + margin.
2. **Derate transfer-function shape** — fixed step table vs. clamped-linear map for
   the §4.2 implausible-fraction → cap mapping. Recommend a small fixed table
   (auditable, trivially WCET-bounded).
3. **`PerceptionOutput` ownership** — does KIRRA hold the prior frame (stateful
   teleport check) or does the contract supply `prev_pos_m`/`dt_s` (stateless)?
   This doc proposes **stateless** (pure functions, easiest to certify); revisit if
   the contract can't supply the prior frame.
4. **Module placement** — `gateway/perception_monitor.rs` (proposed) vs. a top-level
   `perception_monitor.rs`. Gateway placement keeps it beside `containment.rs` and
   the cap composition.
