# Occy / KIRRA — Safety Goals (systematic derivation)

**Issue:** S1 (#113) — HARA + STPA → safety goals + ASIL allocation.
**Status:** Working derivation for review. This is a methodologically sound
*draft* HARA/STPA, not a certified analysis; ASIL ratings and S/E/C judgments
must be confirmed by a qualified safety assessor before any formal safety-case
use. It exists to replace the previously *implicit* Occy-specific safety goals
with an *explicit, traceable* derivation.

**Relationship to AEGIS-SG-001.** This document does NOT supersede
`docs/safety/SAFETY_GOALS.md` (AEGIS-SG-001), which defines the broader
16-goal scheme covering the full Kirra kernel scope (AV + UGV + robot + drone +
industrial protocols + fabric). This document is the planner-focused Occy
derivation; the two coexist. See §6.2 for the cross-mapping.

---

## 1. Item definition

The **item** is the automated driving function for the Phase-1 ODD: lane
keeping, stopping, and minimal-risk maneuvering. Its elements:

- **Occy** — the trajectory planner (the *doer*); proposes a trajectory.
- **KirraGovernor** — the independent runtime checker (the *safety monitor*);
  returns Accept | Reject→MRC | Clamp on the proposed trajectory.
- **World model** — perception/state feed (Phase 1: shared, conservative — the
  (iii) decision).
- **Control adapter** — executes the committed trajectory through the
  per-command kinematics contract.
- **MRC family** — the set of minimal-risk conditions + selector.
- **Teleoperator path** — intermittent remote command source.
- **Fleet posture** — supervisory mode (LockedOut / Degraded / Nominal).

**ODD (Phase 1):** constrained — defined formally in S4. **Controllability
assumption:** driverless operation, so there is no human in the loop to
intervene; controllability is therefore **C3 (uncontrollable)** for essentially
all hazards. This is the single biggest ASIL driver and is what pushes the core
driving hazards to ASIL D.

---

## 2. Method

Two complementary analyses, as S1 specifies:

- **HARA (ISO 26262):** vehicle-level hazards from *malfunctioning behavior* of
  the function, classified by Severity (S0–3), Exposure (E0–4), Controllability
  (C0–3) → ASIL, → safety goal + safe state.
- **STPA:** control-structure analysis that finds *unsafe control actions* and
  *loss scenarios* regardless of whether the cause is a fault (26262) or a
  functional insufficiency (ISO 21448 SOTIF). The hazard clusters
  (flood/rail/post-collision/teleop) are SOTIF triggering conditions and are
  carried here as hazards H4–H7; their systematic catalog is S4.

ASIL determination uses the standard S×E×C table. With **C3** and **S3**:
E4→D, E3→C, E2→B, E1→A.

---

## 3. HARA

| # | Hazard (malfunctioning behavior) | Operational situation | S | E | C | ASIL | SG | Safe state |
|---|---|---|---|---|---|---|---|---|
| H1 | Trajectory causes forward/longitudinal collision (gap too small / fails to brake / accelerates at obstacle) | Following or approaching a lead object | 3 | 4 | 3 | **D** | SG1 | stop short / pull over |
| H2 | Trajectory departs drivable area / crosses into oncoming / mounts curb | Any lateral control | 3 | 4 | 3 | **D** | SG2 | stop short / pull over |
| H3 | Commanded kinematics exceed safe dynamic envelope (speed/accel/jerk/curvature/dt) | Any motion | 3 | 4 | 3 | **D** | SG3 | clamp / stop |
| H4 | Enters standing water of unverified depth | Flooded/ponding road segment | 3 | 2 | 3 | **B** (C if flood-prone ODD) | SG4 | stop short of water |
| H5 | Enters a high-consequence commit zone without confirmed clearance / cannot exit / stops inside | Rail crossing, box junction, narrow bridge | 3 | 2 | 3 | **B** (C if frequent crossings) | SG5 | stop short of zone |
| H6 | Maneuvers after a collision with unconfirmed clearance | Post-impact, person possibly under/near vehicle | 3 | 1 | 3 | **A** (priority-elevated — see §7) | SG6 | immobilize in place |
| H7 | Executes an unsafe teleoperator/remote command | Remote-assist engaged | 3 | 2 | 3 | **B**, but check inherits **D** | SG7 | same as H1–H3 |
| H8 | No reachable / wrong minimal-risk condition when degraded (e.g. stops in a live lane, or fails to stop) | Any check failure / timeout / stale state | 3 | 3 | 3 | **D** | SG8 | context-appropriate MRC |
| H9 | Acts on stale / invalid world state | Sensor drop, latency, occlusion | 3 | 3 | 3 | **D** | SG9 | reject → MRC |
| H10 | Safety checker fails silently (unsafe trajectory passes unchecked) | Governor fault/timeout/NaN | 3 | 3 | 3 | **D** | SG9 | fail closed → MRC |

---

## 4. Derived safety goals

Each goal carries the ASIL of its worst contributing hazard. **Allocation**
names the element that *enforces* the goal; the ASIL-D goals are realized by
decomposition (S2), with the **Governor as the substantive ASIL-D element** and
Occy as the lower-ASIL doer.

**FTTI policy (read once).** Absolute FTTI values depend on closing dynamics,
ODD (S4/#116), and vehicle limits (S8/#120) and are therefore given here as
*form*, not absolute milliseconds. The numbers are fixed when the ODD is set
and then flow directly into S3 (#115) — **FTTI minus actuation latency IS the
Governor WCET budget S3 must prove**. Do not invent absolute milliseconds yet.

- **SG1 (ASIL D)** — The system shall not command a trajectory that results in a
  longitudinal collision (shall maintain an RSS-safe longitudinal distance to
  the nearest lead object). *Enforce: Governor (RSS over horizon). Safe state:
  stop short.* [H1]
  - **FTTI:** per-cycle — verdict before actuation within one control cycle; 0.5 s chain reaction budget is the dead time; Governor WCET is a slice (target ≪ 0.5 s; exact bound proven in S3/#115). RSS look-ahead horizon at the 50 mph cap follows the SG4/SG5 ≈ 94 m basis.
  - **Verification method:** sim scenario suite (cut-in, lead-brake, occluded-pedestrian) + fault-injection (inject unsafe trajectory → assert Reject→MRC) + conservatism check that Governor RSS ≥ parko-core rss bound. Artifacts: #92 (1.C), Phase 2.
- **SG2 (ASIL D)** — The system shall not command a trajectory that departs the
  drivable area or crosses into oncoming traffic / curb / static obstacle.
  *Enforce: Governor (per-step kinematics + drivable-space). Safe state: stop
  short / pull over.* [H2]
  - **FTTI:** per-cycle — verdict before actuation within one control cycle; same per-cycle bound as SG1.
  - **Verification method:** drivable-space boundary unit tests + curve/lane-edge scenarios + curb-mount / oncoming injection → Reject/Clamp. Artifact: #92.
  - **Lateral margin:** 0.40 m (`CONTAINMENT_LATERAL_MARGIN_M`), derived in KIRRA-OCCY-SG2-MARGIN-001 (`docs/safety/OCCY_SG2_MARGIN.md`). Assumes G2 AoU (#123): integrator localization achieves ≤ 0.10 m 95th-percentile lateral error within the deployment ODD. Conservative-fallback 0.75 m documented as a deployment configuration for ODDs that don't meet the G2 AoU.
- **SG3 (ASIL D)** — The system shall not command kinematics exceeding the safe
  dynamic envelope. *Enforce: Governor (per-step kinematics contract; clamp).*
  [H3]
  - **FTTI:** per-cycle — verdict before actuation within one control cycle; same per-cycle bound as SG1.
  - **Verification method:** per-step kinematics unit tests (over-speed/accel/curvature, zero/neg dt, NaN) + clamp-correctness test (egress actually rewritten). Artifact: #92 (existing tests).
- **SG4 (ASIL B — active, water in deployment ODD)** — The system shall not enter
  standing water of unverified depth. *Enforce: Governor (WATER_UNTRAVERSABLE).
  Safe state: stop short of water.* [H4]
  - **FTTI:** look-ahead — Reject ≥ **94 m ahead** at the 50 mph cap (SSD = v·t_react + v²/2a with t_react=0.5 s, a=3 m/s² comfortable; v_max = 22.35 m/s). Derates with the cap under degraded conditions (rain / fog / low-light) per the dynamic-derate rule in ADR-0001.
  - **Verification method:** unbounded-depth → WATER_UNTRAVERSABLE unit test + flood CARLA demo (#100) + earn-back negative test (no false-traverse without evidence, ties F1/#98).
- **SG5 (ASIL B)** — The system shall not enter
  a high-consequence commit zone without confirmed clearance and a verified
  exit, and shall not stop within one. *Enforce: Governor (map-anchored
  COMMIT_ZONE_BLOCKED). Safe state: stop short of zone.* [H5]
  - **FTTI:** look-ahead — Reject ≥ **94 m ahead** at the 50 mph cap (same SSD derivation as SG4). Derates with the cap under degraded conditions.
  - **Verification method:** gate-down / can't-exit / stop-inside unit tests + map-prior perception-miss test (Reject fires from map alone) + commit-zone CARLA demo (#109).
- **SG6 (ASIL A by table; developed to elevated rigor per owner decision)** — After a detected collision with
  unconfirmed clearance, the system shall immobilize and execute no further
  motion until clearance is confirmed. *Enforce: Governor (post-collision latch
  + motion veto). Safe state: immobilize in place.* [H6]
  - **FTTI:** ≤ 1 control cycle from confirmed impact; FDTI dominated by impact-detection latency (PC1/#102).
  - **Verification method:** impact → immobilize + motion-veto fault-injection + "no resume without clearance" test + post-collision CARLA demo (#105).
- **SG7 (inherits ASIL D)** — The system shall apply the same safety checks to
  teleoperator/remote commands as to planner commands, with no source-based
  relaxation. *Enforce: Governor (doer-agnostic check). command_source is
  audit-only.* [H7]
  - **FTTI:** per-cycle, identical to the planner path; same per-cycle bound as SG1.
  - **Verification method:** teleop unsafe-command injection → verdict identical to planner + command_source-does-not-alter-verdict test + teleop curb-mount integration (1.D / #112).
- **SG8 (ASIL D)** — The system shall at all times have a reachable,
  context-appropriate minimal-risk condition, and shall commit it on any check
  failure, timeout, or stale world state. *Enforce: planner (standing
  validated MRC) + control adapter (commit-on-failure).* [H8]
  - **FTTI:** standing — a validated MRC is continuously available; commit within ≤ 1 control cycle on failure. MRC controlled stop ≈ **50 m firm to 83 m comfortable** at the 50 mph cap (kinematic, v²/2a).
  - **Verification method:** standing-MRC always-present invariant test + commit-on-failure integration (Reject/timeout/stale → MRC committed) + wrong-context guard (no stop-in-live-lane default; ties S7/#119).
- **SG9 (ASIL D)** — The safety check shall fail closed: any fault, timeout, or
  non-finite input shall result in rejection (→ MRC), never silent acceptance.
  *Enforce: Governor (bounded WCET, NaN trap, fail-closed-timeout).* [H9, H10]
  - **FTTI:** per-cycle — the fail-closed timeout IS the FDTI+FRTI bound; ≤ 1 control cycle; absolute = the WCET bound proven in S3 (#115).
  - **WCET bound (S3 #115):** target `GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100 µs` (deployment); CI regression gate at `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS = 1000 µs` (generous, hardware-noise-tolerant). CI-measured steady-state p99.9 across all verdict-path entry points: < 400 ns. See `src/wcet_gate.rs` for the structural boundedness argument + measurement methodology. The certified target-hardware number is re-measured on the D3 independent compute under S8 (#120).
  - **Verification method:** NaN/Inf → Reject (no panic) + checker-timeout → fail-closed (bounded WCET, S3) + checker-fault → MRC (S7) tests + `wcet_gate::ci_gate_tests::wcet_*` CI regression tests.

### 4.1 Verification & FTTI summary

| SG | ASIL | FTTI (50 mph cap) | Verification artifact | Sets S3 budget? |
|----|------|-------------------|------------------------|-----------------|
| SG1 | D | per-cycle; WCET ≪ 0.5 s (bound in S3) | scenario suite + injection; #92 | yes |
| SG2 | D | per-cycle | drivable-space tests + injection; #92 | yes |
| SG3 | D | per-cycle | kinematics + clamp tests; #92 | yes |
| SG4 | **B (active)** | look-ahead ≥ 94 m at cap (derates) | flood demo #100 + unit tests | indirectly (horizon) |
| SG5 | B | look-ahead ≥ 94 m at cap (derates) | commit-zone demo #109 + map-prior test | indirectly (horizon) |
| SG6 | A* (elevated rigor) | ≤ 1 cycle from impact | post-collision demo #105 + injection | yes (detection) |
| SG7 | D | per-cycle (= planner path) | teleop injection; #112 | yes |
| SG8 | D | standing + ≤ 1 cycle commit; MRC stop ≈ 50–83 m at cap | invariant + commit-on-failure tests | partially |
| SG9 | D | ≤ 1 cycle (= WCET bound in S3) | NaN/timeout/fault tests | DEFINES it |

---

## 5. STPA

**Losses.** L1 death/injury (occupants, road users, pedestrians). L2 vehicle
loss/damage. L3 loss of public/regulatory trust (the post-collision cover-up
dynamic is an L3 amplifier).

**System-level hazards (states that can lead to a loss).** HA violate safe
separation (long./lat.); HB leave drivable/traversable region; HC unsafely
enter/occupy a high-consequence zone; HD continue motion after a collision; HE
exceed the safe dynamic envelope; HF lack/abandon a required MRC.

**Control structure (summary).** Occy → (proposed trajectory) → Governor →
(Accept / Reject→MRC / Clamp verdict) → control adapter → vehicle. Teleoperator
→ (command) → **through** Governor. Fleet posture → mode → Occy & Governor.
World model → feedback → Occy & Governor. All verdicts/commands → audit chain.

**Unsafe control actions (focus: the Governor's *verdict*, the safety control
action).**

| UCA | Description | Violates |
|---|---|---|
| UCA-1 | Governor provides **Accept** for an unsafe trajectory | SG1–SG5 |
| UCA-2 | Governor does **not Reject** when required (missing safety action) | SG1–SG6 |
| UCA-3 | Governor verdict arrives **too late** to block the unsafe command | SG1–SG3, SG9 |
| UCA-4 | Governor forces **MRC where stopping is itself hazardous** (e.g. immobilize in a live lane) | SG8 |
| UCA-5 | Governor produces a verdict from **stale world state** | SG9 |
| UCA-6 | Teleop command **bypasses** the Governor | SG7 |
| UCA-7 | **MRC not committed** when Accept cannot be given | SG8 |

**Loss scenarios → safety constraints (and where they're handled).**

- Governor consumes the planner's world model → **common-mode perception
  error** → both miss a hazard → UCA-1/2. *Constraint: re-derive at worst-case
  bounds (ConservativeWorldView); resolve independence in S2 (the (iii)→(ii)
  decision).*
- Unbounded Governor timeout → UCA-3. *Constraint: bounded verdict time —
  structural boundedness argument + host-indicative p99.9, not certified WCET
  (target re-measure #274) → S3.*
- Non-finite (NaN) propagation → silent Accept → UCA-1. *Constraint: is_finite
  trap, fail-closed → already in 1.C / SG9.*
- Missing map prior at a commit zone → UCA-2. *Constraint: map-anchored
  enforcement → commit-zone cluster.*
- Missed impact detection → UCA-2 for SG6. *Constraint: richer impact detection
  → PC1.*
- Teleop path wired around the Governor → UCA-6. *Constraint: route through the
  Governor → 1.D / teleop cluster.*
- No standing validated MRC → UCA-7. *Constraint: continuously validated
  standing MRC → 1.B / SG8.*
- Stop-in-live-lane as default MRC → UCA-4. *Constraint: context-dependent MRC
  selection + Governor fault model → S7.*

---

## 6. Traceability

### 6.1 Project issue traceability

| Safety goal | ASIL | Enforcing element | Project issue(s) |
|---|---|---|---|
| SG1 longitudinal collision | D | Governor RSS | #92 (1.C); RSS Phase 2/3 |
| SG2 road/lane departure | D | Governor kinematics + drivable-space | #92 (1.C) |
| SG3 dynamic envelope | D | Governor per-step contract / clamp | #92 (1.C) |
| SG4 water untraversable | B–C | Governor WATER_UNTRAVERSABLE | flood (#97/98/99/100) |
| SG5 commit-zone | B–C | Governor map-anchored | commit-zone (#106–109) |
| SG6 post-collision immobilize | A* | Governor latch + veto | post-collision (#101–105) |
| SG7 teleop parity | D | Governor doer-agnostic | teleop (#110–112) |
| SG8 MRC reachability | D | planner standing MRC + adapter | #91 (1.B), #93 (1.D), S7 (#119) |
| SG9 fail-closed | D | Governor WCET/NaN/timeout | #92 (1.C), S3 (#115) |
| (independence of the above) | D | doer/checker decomposition + DFA | S2 (#114) |

### 6.2 Mapping to AEGIS-SG-001 (preexisting kernel safety goals)

The preexisting `docs/safety/SAFETY_GOALS.md` (AEGIS-SG-001 v1.0.0) defines a
broader 16-goal scheme covering the full Kirra kernel scope. The Occy
derivation here is a planner-focused subset with different numbering. Both
schemes coexist; this table shows the correspondence so an assessor (or a code
reviewer reading a `Safety:` traceability tag) can navigate either way.

| Occy SG | AEGIS-SG-001 SG(s) | Notes |
|---|---|---|
| SG1 longitudinal collision (D) | (no direct equivalent — new) | RSS over horizon; planner-level concept not in preexisting kernel scheme |
| SG2 road/lane departure (D) | (no direct equivalent — new) | Drivable-space check; planner-level, new |
| SG3 dynamic envelope (D) | **SG-001** (velocity), **SG-002** (lat-accel) | Existing kernel pieces; Occy SG3 is the umbrella over them |
| SG4 water untraversable (B–C) | (no equivalent — new) | Hazard cluster H4 (flood) |
| SG5 commit zone (B–C) | (no equivalent — new) | Hazard cluster H5 (rail/box-junction/bridges) |
| SG6 post-collision immobilize (A*) | (no equivalent — new) | Hazard cluster H6 |
| SG7 teleop parity (D inherited) | (no equivalent — new) | Hazard cluster H7 |
| SG8 MRC reachability (D) | **SG-005** (cache-stale → MRC; partial) | Occy SG8 is broader: any check failure / timeout / stale → MRC |
| SG9 fail-closed (D) | **SG-004** (NaN/Inf), **SG-006** (Unknown denial), **SG-008** (startup), **SG-015** (admin-token) | All flavors of fail-closed across the kernel |
| (independence) | (cross-cuts) | DFA → S2 (#114) |

AEGIS SGs without an Occy equivalent (kernel scope outside the planner item):
**SG-003** sensor-watchdog, **SG-007** cross-asset propagation, **SG-009**
HA promotion, **SG-010** audit-chain tamper, **SG-011** CANopen NMT,
**SG-012** DNP3 broadcast audit, **SG-013** recovery hysteresis, **SG-014**
federation replay, **SG-016** DDS Volatile durability. Those remain owned by
AEGIS-SG-001 and are out of scope for the Occy item definition (§1).

---

## 7. Notes carried forward

- **ASIL decomposition (→ S2).** SG1–SG3 and SG7–SG9 are ASIL D. They are met by
  decomposition: the Governor is the substantive ASIL-D(D) element (not a
  watchdog), Occy is the lower-ASIL doer; independence proven by DFA. The
  (iii) shared world view is the likely common-cause finding.
- **SG6 priority vs ASIL.** By the table SG6 rates ASIL A (low exposure), but
  its severity is catastrophic and it is a known reputation-ending failure
  mode. Treat its priority as higher than its ASIL implies — ASIL governs
  development rigor, not deployment priority.
- **SG4/SG5 exposure is ODD-dependent.** Re-rate when the ODD (S4) is fixed: a
  flood-prone or rail-dense ODD raises E and therefore the ASIL.
- **Driverless C3.** If an interim ODD retains a safety operator, controllability
  drops below C3 and several ASILs fall — but the target is driverless, so C3 is
  the design basis.
- **Review.** This derivation should be reviewed against the actual ODD (S4) and
  confirmed before it backs any UL 4600 safety case (S5).
