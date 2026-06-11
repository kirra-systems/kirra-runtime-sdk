# Kirra Safety Kernel — Assumptions of Use (Register)

**Document ID:** KIRRA-OCCY-AOU-001.
**Status:** Draft. Living register.
**Classification:** ISO 26262 Part 10 (SEooC assumptions of use) / Part 9 (safety-related
application conditions).
**Cross-refs:** `SAFETY_CASE_INDEX.md` (AEGIS-SC-000), ADR-0004 (independent safety
channel / doer–checker).

---

## What this register is

Kirra is developed as a **Safety Element out of Context (SEooC)**: it is the independent
governor (ADR-0004) that checks the *output* of an integrator-supplied AI/perception
stack and equipment it does not itself build. An SEooC carries **assumptions of use
(AoU)** — the conditions on the surrounding system that must hold for Kirra's safety
argument to be valid. Where an assumption is the integrator's responsibility, it becomes
a **safety-related application condition (SRAC)** the integrator must discharge for their
specific deployment.

This file is the **central register** of those assumptions. Many AoUs were first recorded
inline where they arose (e.g. the AoU-GAP dispositions in
`OCCY_SPEED_CAP_VALIDATION.md`); the SG2 lateral-margin G2 AoU #123, derived inline in
`OCCY_SG2_MARGIN.md`, is now **filed here as `AOU-LOCALIZATION-001`** (its source analysis
remains authoritative in that document).
This register collects the **cross-cutting, deployment-precondition** assumptions in one
place so each has a stable ID, an explicit verification method, and a recorded
consequence-if-violated. Inline AoUs in their owning documents remain authoritative for
their local context; this register links to them and adds the ones (like the
perception-frame assumption) that are not owned by any single existing doc.

**ID scheme:** `AOU-<TOPIC>-NNN`. Each entry records: the assumption, why it is
load-bearing, the supporting evidence, scope/preconditions, verification status +
method, and the consequence if violated. **An AoU with `VERIFICATION STATUS: OPEN` is a
pre-enable gate** — the dependent mechanism must remain fail-closed / default-off until
the verification passes for the target deployment.

### Index

| AoU ID | Title | Owner | Status | Gates |
|--------|-------|-------|--------|-------|
| AOU-PERCEPTION-FRAME-001 | Upstream object velocity is absolute, map/world-frame | Integrator (perception) | **OPEN** | `KIRRA_PERCEPTION_DERATE_ENABLED` (PMON-003 D4 pre-enable gate) |
| AOU-MSG-TOOLCHAIN-001 | ROS message toolchain codegens the full Autoware message set (no trimmed packages) | Integrator (build / toolchain) | **SUPERSEDED** (owner decision 2026-06-05, option C) | superseded by the curated-interface resolution — see below / KIRRA-OCCY-MSGSYNC-001; residual → AOU-MSG-TOOLCHAIN-002 |
| AOU-MSG-TOOLCHAIN-002 | r2r codegen of the FULL Autoware message set on a host co-resident with full Autoware | Integrator (build / toolchain) | **OPEN** | any co-resident-with-full-Autoware deployment topology |
| AOU-PERCEPTION-RANGE-001 | Reliable detection at ≥ 130 m worst-case over the deployment ODD (degraded-condition R per the derate table) | Integrator (perception) | **AoU-GAP** (base) → Item-B-measured (D1) | the 22.35 m/s ODD speed cap (ADR-0001 / SPEED-VAL-001 row 1) |
| AOU-PERCEPTION-CLASS-001 | Reliable detection of the worst-case object classes (pedestrian / cyclist / child / low-contrast debris) at ≥ R_reliable | Integrator (perception) | **AoU-GAP** (base) → D1 IDC omission coverage | the speed cap (SPEED-VAL-001 row 4); the SG1/SG6 worst-case-object basis |
| AOU-VEHICLE-FRICTION-001 | Effective deceleration ≥ 3.0 m/s² over the deployment ODD (else sub-ODD weather-derate) | Integrator (vehicle / road) | **OK-ANALYTICAL** (vehicle) + **AoU-GAP** (road friction) | the speed cap's SSD braking term (SPEED-VAL-001 row 3) |
| AOU-ACTUATION-LATENCY-001 | Actuation completes safe-stop initiation ≤ 499 ms of the MRC verdict, and safe-stops on loss of a valid verdict | Integrator (actuation) | **OK-PROVEN** (Governor) + **AoU-GAP** (actuator residual) | the speed cap's t_react budget (SPEED-VAL-001 row 2); SS-003 MRC fallback |
| AOU-HW-POWER-001 (DR-1) | Governor D3 compute on an ASIL-D-class redundant / supervised power supply | Integrator (hardware / platform) | **AoU-GAP** — pre-production HW gate | the ASIL-D PMHF target for the Governor element (KIRRA-OCCY-QUANT-001) |
| AOU-HW-COMMBUS-001 (DR-2) | Governor comm path (Auto-Ethernet PHY+MAC) achieves LFM ≥ 90 % | Integrator (hardware / platform) | **AoU-GAP** — pre-production HW gate | the ASIL-D LFM target for the Governor element (KIRRA-OCCY-QUANT-001) |
| AOU-LOCALIZATION-001 | Integrator localization ≤ 0.10 m 95th-pct lateral (cross-track) error over the ODD; else the documented 0.75 m conservative-fallback margin | Integrator (localization) | **AoU-GAP** (base) — integrator-characterized; runtime gate live (#123 / PR #264) | `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m` (SG2 ASIL D); all map-anchored SG5 commit-zone enforcement (#260–#262) + the SG4 `MapKnownSafe` earn-back |
| AOU-CLEARANCE-AUTH-001 | The integrator/verifier shall issue an `OperatorClearanceGrant` ONLY after authenticating the operator (the parko clearance loop enforces structure, not identity) | Integrator (verifier / operations) | **AoU** (by design) — structural loop live (#103 / PR #267); authentication delegated | the SG6 post-collision no-resume (`ClearanceLoop::try_clear` is the only un-latch path); SS-003 human-reset intent |
| AOU-TIMESYNC-001 | Sensor/message timestamps consumed by governor staleness/deadline validation are synchronized, monotonic, drift-bounded vs the boundary clock domain, and converted to it before publication (HVCHAN §5 non-mixing rule) | Integrator (time sync / platform) | **AoU-GAP** — integrator obligation; drift bound **VALIDATION-PENDING** (set with the FTTI budget, #274/#278) | the governor staleness/deadline barrier (HVCHAN §3 judge; the #271 harness + #273 spike deadline checks); PTP/gPTP expected discharge |

---

## AOU-PERCEPTION-FRAME-001 — Upstream object velocity is absolute (map/world frame)

### Assumption
The integrated upstream perception source publishes each tracked/predicted object's
velocity — `PredictedObject.kinematics.initial_twist_with_covariance.twist.linear`
(`{x, y}`) — as the **object's absolute velocity expressed in the map/world frame**. It
is **NOT** ego-relative / closing velocity, and **NOT** body/sensor-frame velocity.

### Why it is load-bearing
The PMON-001 kinematic-plausibility ceiling (`V_OBJECT_MAX_MPS = 60.0 m/s`) is a
**magnitude** check, `sqrt(vx² + vy²) ≤ ceiling`, over the velocity vector preserved by
the slice-1 ingest (KIRRA-OCCY-PMON-003).

- **Frame rotation does not matter.** Magnitude is rotation-invariant, so whether the
  vector is expressed in the map frame or a (rotated) body frame yields the **same
  speed** — the check is insensitive to that distinction.
- **Absolute-vs-ego-relative is decisive.** Under an *ego-relative* twist:
  - a **stationary** object reads as ≈ **ego speed** → spurious derate while the ego is
    moving (false positive — availability loss, but also erodes trust in the guard); and
  - a fast **absolute** object moving with the ego could read as ≈ **0** → the ceiling
    never fires when it should (false negative — a **missed derate**, a safety-relevant
    failure of the check's purpose).

Preserving the twist *vector* and checking its *magnitude* (PMON-003 §5 "PRESERVE") is
therefore correct **only if** the reported velocity is absolute. This assumption is the
condition that makes the magnitude check meaningful.

### Evidence (Autoware — paraphrased; see sources)
Autoware's tracking/prediction pipeline estimates object motion in the **world frame**:
the EKF-based multi-object tracker outputs each object's **absolute** velocity, and
`map_based_prediction` generates object paths and works in **global coordinates**. A
`map_based_prediction` issue thread further indicates the object twist components are
expressed in the **map/world frame**, with **both** `vx` and `vy` populated — i.e. `vx`
is **not** assumed to be a dominant longitudinal (body-frame) component, which is what a
body-frame convention would imply. For radar-sourced tracks, the
`radar_tracks_msgs_converter` exposes an **ego-motion-compensation** option precisely
because radar returns are natively ego-relative and must be compensated to become
absolute.

Sources:
- Perception interface (`PredictedObject` kinematics structure):
  https://autowarefoundation.github.io/autoware-documentation/pr-493/design/autoware-interfaces/components/perception-interface/
- `map_based_prediction` (global-coordinate output):
  https://autowarefoundation.github.io/autoware_universe/main/perception/autoware_map_based_prediction/
- Issue #6192 (map/world-frame twist components; `vx`/`vy` both populated):
  https://github.com/autowarefoundation/autoware_universe/issues/6192
- `radar_tracks_msgs_converter` (ego-motion compensation flag):
  https://autowarefoundation.github.io/autoware_universe/main/perception/autoware_radar_tracks_msgs_converter/

### Namespace / scope
- **In scope:** `autoware_perception_msgs` — the namespace the adapter already binds
  (`r2r::autoware_perception_msgs::msg::PredictedObjects`).
- **Out of scope (current binding):** `autoware_auto_perception_msgs` (the autoware.auto-era
  legacy). It carries the **same semantics** (a migrated variant), but a deployment on it
  would **not interoperate** without a separate adapter message binding — so it is not
  covered by this assumption as currently wired.

### Deployment precondition (radar)
If the perception fusion includes radar tracks via `radar_tracks_msgs_converter`, its
**ego-vehicle twist compensation MUST be enabled** (linear, and yaw if the ego rotates).
Without it, radar-sourced object twist is **ego-relative** and **violates** this
assumption. Lidar/camera tracking via the EKF is absolute by construction; the radar path
is the one that can silently break the assumption through misconfiguration.

### Pairs with
- **D1 precondition** (PMON-003): consume Autoware **tracking/prediction output**, not raw
  detections. Raw detections would also fail the absolute-velocity assumption (and would
  force association inside Kirra — an **ADR-0004 boundary violation** to reject).

### Verification status — **OPEN**
The Autoware documentation **narrows** the assumption (the convention is absolute,
map-frame) but does **not confirm it for a specific deployment**: a misconfigured radar
converter, a custom tracker, or a non-standard pipeline can override the convention.

**Verification method (pre-enable gate).** On the **target Autoware version and config**,
observe a tracked object with **known ground-truth velocity** and confirm the reported
twist **magnitude matches the object's absolute ground speed independent of ego motion** —
i.e. a moving ego does **not** shift a stationary object's reported speed, and an object
moving with the ego is not reported as ≈ 0. This is the concrete check behind
**PMON-003 §D4**.

`KIRRA_PERCEPTION_DERATE_ENABLED` **stays OFF until this passes** (together with the
end-to-end freshness verification and the sim/bench validation gate named in PMON-003).

### Consequence if violated
The kinematic-ceiling derate is **invalid** — false derates on a moving ego and/or missed
derates of genuinely fast objects. This is exactly why the mechanism ships
**fail-closed / default-OFF** and gated on this verification: an unverified frame
assumption must never be allowed to drive (or fail to drive) a real actuator.

### Cross-references
- KIRRA-OCCY-PMON-001 — the kinematic guard + `V_OBJECT_MAX_MPS` derivation
  (`KIRRA-OCCY-PMON-KIN-MARGIN-001`); its frame comment was corrected in PMON-003 slice-1
  to point at this assumption rather than overstate "confirmed against the adapter."
- KIRRA-OCCY-PMON-003 §D4 — the pre-enable gate this AoU formalizes.
- KIRRA-OCCY-PMON-002 — the cap-composition mechanism the derate feeds.
- ADR-0004 — independent safety channel (Kirra checks perception output; builds none).

---

## AOU-MSG-TOOLCHAIN-001 — ROS message toolchain codegens the full Autoware message set

### Assumption
The deployment's ROS 2 message toolchain (the binding generator the adapter is built
against) **generates bindings for the integrator's genuine, full Autoware message set** —
in particular the complete `autoware_planning_msgs` (and the `autoware_common_msgs` it
depends on) — **without** requiring any message package to be trimmed or stubbed.

### Why it is load-bearing
The adapter binds real Autoware messages (`autoware_planning_msgs::Trajectory`,
`autoware_perception_msgs::PredictedObjects`, `autoware_control_msgs::Control`). If the
toolchain cannot codegen a deployment's real message packages, the adapter cannot be built
against the real interface — and any *workaround* that trims the message set changes the
interface the safety function is integrated against. A safety function must run against the
**genuine** deployment interface, not a reduced stand-in.

### Evidence / origin (KIRRA-OCCY-PMON-004 §8 constraint 1, 2026-06-04)
On the sub-gate-1 bench (Ubuntu 24.04 + ROS 2 Jazzy), `r2r = "=0.9.5"`'s binding generator
(`r2r_msg_gen`) **panics** on Jazzy's full `autoware_planning_msgs` — specifically the route
messages (`LaneletPrimitive`, the `ClearRoute` service) and `autoware_common_msgs/ResponseStatus`
— and a **single** un-generatable type aborts the **entire** binding run (so even the
`Trajectory` the adapter needs never generates). r2r's `IDL_PACKAGE_FILTER` is include-only
with no nested-dependency resolution, so it cannot exclude a bad message inside a needed
package.

**Workaround used to run sub-gate 1 (DEV/SIM ONLY):** the apt `autoware_planning_msgs` was
replaced with a trimmed overlay containing only `Trajectory` + `TrajectoryPoint` (verbatim
official `.msg`). This unblocks the *mechanism + decode* validation on the bench but **must
not** be carried into a real integration.

### Scope
- **In scope:** any bench/vehicle deployment that builds the adapter against the
  integrator's real Autoware messages via r2r 0.9.5 (or whatever generator is in use).
- This is a **build/toolchain** precondition, not a runtime assumption — it is discharged at
  integration/build time, not per-cycle.

### Verification status — **SUPERSEDED** (owner decision 2026-06-05)
Resolved by **option C — SUPERSEDE** (see the Resolution below). The original OPEN condition
("the toolchain codegens the **full** Autoware message set") is replaced by the reframed,
satisfiable condition realized by the **sanctioned** curated-interface package — which is
exactly the third resolution option this AoU originally listed ("a sanctioned minimal-
interface package … distinct from the ad-hoc bench trim"). Discharged for the isolated-
governor topology; the co-resident-codegen residual is tracked as AOU-MSG-TOOLCHAIN-002.
The trimmed bench overlay is retired (it was never an acceptable deployment artifact).

### Consequence if violated
The adapter is integrated against a **reduced** interface — messages/fields the real
deployment carries are absent, so behavior on the real interface is unproven (and, worse,
could mask a field the safety logic should see). The mechanism/decode evidence from the
trimmed-package bench does not transfer to a full-interface deployment.

### Cross-references
- KIRRA-OCCY-PMON-004 §8 — the execution record where this constraint was observed.
- KIRRA-OCCY-DEPLOY-001 — the Pacifica deployment architecture (bench/vehicle tiers this
  precondition gates).
- The adapter `README.md` — the `ros2` vs `lanelet2` build matrix and the dev-only trim note.
- KIRRA-OCCY-MSGSYNC-001 (`MSG_INTERFACE_VERSION_SYNC.md`) — the curated-interface SRAC (see
  the relationship below).

### Resolution — option C (SUPERSEDE), owner decision 2026-06-05
**Owner decision (2026-06-05): OPTION C — SUPERSEDE.** The reframed condition is adopted:

> *the governor runs **ISOLATED** (its build/runtime host carries **no** full Autoware
> message set) against a **hash-verified curated subset** that uses the **real** Autoware
> package names + **verbatim** message closures, **version-synced** to the deployed Autoware.*

This is the right condition for an *independent* governor (ADR-0004): a small, audited
interface surface, kept wire-compatible by byte-diff + RIHS hash, replaces a dependency on a
third-party toolchain codegen-ing a large message set — and it retires the un-versioned trim
entirely.

**DISCHARGED for the isolated-governor topology** (KIRRA-OCCY-MSGSYNC-001 TOPO-1). Phase 2 is
complete:
- `scripts/curated_interface/verify_hashes.sh` = **PASS** (2026-06-05, ROS 2 Jazzy) — all 8
  curated `.msg` byte-identical to the apt reference
  `ros-jazzy-autoware-{perception,planning,common}-msgs` **1.11.0-1noble.20260412**. Wire
  compatibility (RIHS type hash) holds by construction.
- `cargo build/test -p kirra-ros2-adapter --features ros2` = **GREEN** against the curated
  overlay with **NO full Autoware present** (no apt Autoware, no trim). Verdict path unchanged.

**Going-forward governance — a standing obligation, not a one-time discharge:**
KIRRA-OCCY-MSGSYNC-001 (version pin + byte-diff re-verify on any Autoware version change;
per-target re-verification under TOPO-1 interface isolation / TOPO-2). The deployment-topology
commitment that satisfies TOPO-1 is KIRRA-OCCY-DEPLOY-001 (container-isolation on the
single-Orin bench; dedicated/container on the Pacifica).

**Residual NOT covered by this discharge — tracked as AOU-MSG-TOOLCHAIN-002 (OPEN):** r2r
0.9.5 still cannot codegen the full message set, so a topology in which the governor must
co-reside with full Autoware on the same r2r codegen path is **out of scope** here. The
curated package **avoids** the codegen panic by topology; it does **not fix** r2r.

---

## AOU-MSG-TOOLCHAIN-002 — r2r cannot codegen the full Autoware message set (co-resident topology)

### Assumption (OPEN)
The curated-interface discharge of AOU-MSG-TOOLCHAIN-001 holds **only** where the governor's
r2r build/codegen host carries the **curated subset alone**. A deployment topology in which
the governor must share an r2r codegen path with a **full** Autoware install is **NOT**
covered: r2r 0.9.5 (`r2r_msg_gen`) still panics on Jazzy's full `autoware_planning_msgs`
(route messages `LaneletPrimitive` / `ClearRoute`, `autoware_common_msgs/ResponseStatus`), and
one un-generatable type aborts the entire binding run.

### Why load-bearing / resolution options
Such a co-resident topology needs an **r2r bump off `=0.9.5`**, or an upstream
**nested-dependency-aware filter fix** in `r2r_msg_gen`. Until then, **only the
isolated-governor topology is supported** (KIRRA-OCCY-DEPLOY-001: container-isolation on the
single-Orin bench; dedicated / container on the Pacifica). This item makes that residual
explicit so the AOU-MSG-TOOLCHAIN-001 discharge is not an overclaim.

### Scope
- **In scope:** any deployment whose r2r codegen host also carries the full Autoware message
  set (co-resident).
- **Out of scope:** the isolated-governor deployments — covered by the AOU-MSG-TOOLCHAIN-001
  / KIRRA-OCCY-MSGSYNC-001 discharge.

### Verification status — **OPEN** (tracked, deferred)
Resolved only by an r2r codegen fix/bump (the original AOU-MSG-TOOLCHAIN-001 options 1–2). Not
required for the intended isolated-governor deployments; tracked here so the residual is
visible to a certifier and revisited if a co-resident topology is ever adopted.

### Consequence if violated
A co-resident codegen build fails (the r2r panic), or — if forced through with a trim — falls
back to the un-versioned trim that the safety case prohibits. Either way the governor is not
built against a verified genuine interface.

### Cross-references
- AOU-MSG-TOOLCHAIN-001 — the superseded parent; this is its named residual.
- KIRRA-OCCY-MSGSYNC-001 — the SRAC whose TOPO-1 isolation precondition keeps this case out
  of the intended deployments.
- KIRRA-OCCY-DEPLOY-001 — the deployment-topology commitment (isolation).
- KIRRA-OCCY-PMON-004 §8 — where the r2r-on-Jazzy panic was first recorded; the r2r-version
  track.

---

# Perception Input Contract (#126) — SEooC assumptions of use

The three clauses below formalize the base-tier **Perception Input Contract**
(#126) into stable register entries. They were derived and dispositioned in the
speed-cap validation matrix (`OCCY_SPEED_CAP_VALIDATION.md` §2–4); this register
**files** them — it does not re-derive them. Each keeps its real disposition
from that analysis (`AoU-GAP` / `OK-ANALYTICAL`); none is upgraded to closed.

The contract is **bidirectional**: the kernel guarantees fail-closed behavior on
*absent / stale / out-of-envelope* perception **if** the integrator guarantees
the in-envelope detection performance these clauses name. The kernel-side half is
already in code — `AgentScene::Absent → UNSAFE` ("no agents ≠ clear",
`parko/crates/parko-kirra/src/lib.rs:160`), `OcclusionScene::Absent → stop`
(#122, PR #251 in review), and the sensor-liveness watchdog (SG-003 / SG9,
`telemetry_watchdog::spawn_telemetry_watchdog`, wired in
`src/bin/kirra_verifier_service.rs`). What the kernel **cannot** self-verify is
that perception, *when it does report*, actually detects the worst-case object at
the range the speed cap assumes — that is the integrator obligation filed here.

## AOU-PERCEPTION-RANGE-001 — Reliable detection range ≥ 130 m worst-case

### Assumption
> *Integrator perception shall deliver reliable detection at ≥ 130 m worst-case
> over the deployment ODD; degraded-condition R characterized per the
> SPEED_ENVELOPE.md §5–6 derate table.*

### Why it is load-bearing
The 22.35 m/s (50 mph) ODD speed cap (ADR-0001) is set by the safe-stopping-
distance relation `SSD = R`: the cap is only valid if the worst-case detection
range `R_reliable` actually holds. `R_reliable ≈ 130 m` is an integrator
perception-pipeline property; KIRRA takes no base-tier measurement of it. If the
real range is shorter, the cap is unconservative — the ego could be committed to
a speed from which it cannot stop within the distance it can actually see.

### Evidence
- `SPEED_ENVELOPE.md` §2 (the `R_reliable ≈ 130 m` design basis and its
  rain/fog/spray degradation note) — anchor `SPEED_ENVELOPE.md:35`.
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 1 (disposition + clause derivation).

### Scope
- **In scope:** the forward look-ahead perception coverage feeding the speed-cap
  envelope, over the full deployment ODD including degraded conditions.
- **Owner:** Integrator (perception). Base-tier; the D1 add-on (#124, Item B)
  supplies a KIRRA-measured per-sensor range and supersedes the gap at D1.

### Verification status — **AoU-GAP** (base) → Item-B-measured (D1)
No KIRRA base-tier measurement. Discharged for a deployment by the integrator's
perception range characterization over the ODD (S8-style), or closed unilaterally
at D1 by the IDC channel (Item B) whose `min(R_radar, R_thermal, R_optical)` is
empirically characterized.

### Consequence if violated
The speed cap is unconservative: the ego may travel faster than it can stop
within its true sightline → a forward-collision pathway (defeats the cap's SSD
basis). The kernel cannot detect this from inside; it is exactly why the clause is
an explicit pre-deployment integrator obligation.

### Cross-references
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 1, §4 clause 1 — the source analysis.
- `ADR-0001` — the cap and its R assumption.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — an assumed external requirement the
  "absence of unreasonable risk" claim rests on; a violation is a path to
  unreasonable risk via the speed-cap basis.
- Occy **SG1** (RSS / no forward collision) — the safety goal a range shortfall
  defeats. Kernel complement: `AgentScene::Absent → UNSAFE` (`lib.rs:160`);
  occlusion stop (#122); telemetry watchdog (SG-003 / SG9).
- `#124` / Item B — the D1 closure.

## AOU-PERCEPTION-CLASS-001 — Worst-case object-class detection at ≥ R_reliable

### Assumption
> *Integrator perception shall reliably detect ISO-26262-relevant worst-case
> object classes (pedestrian, cyclist, child-pedestrian, low-contrast debris) at
> ≥ R_reliable distance within the deployment ODD; FN rate per class
> characterized per integrator's safety case.*

### Why it is load-bearing
Range alone is insufficient — the cap assumes the worst-case **object class** is
detected at that range. A pipeline that achieves 130 m on a car but misses a
child-pedestrian or low-contrast debris until much closer breaks the cap basis
for the dominant safety case (the 130 m + pedestrian-class combination). Per-class
false-negative rate is an integrator perception property KIRRA does not measure at
base tier.

### Evidence
- `SPEED_ENVELOPE.md` §2 (worst-case object implicit in `R_reliable`) — anchor
  `SPEED_ENVELOPE.md:35`.
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 4, §4 clause 2.
- `OCCY_DFA.md` §3 C5 — the common-cause **omission** the D1 IDC closes.

### Scope
- **In scope:** per-class detection (incl. FN-rate characterization) for the
  ISO-26262 worst-case vulnerable classes, at ≥ R_reliable, over the ODD.
- **Owner:** Integrator (perception). Base-tier; D1 IDC omission coverage
  (thermal pedestrian-class, radar low-RCS) closes it at D1.

### Verification status — **AoU-GAP** (base) → D1 IDC omission coverage
No KIRRA base-tier measurement. Discharged by the integrator's per-class FN-rate
characterization, or closed at D1 by the IDC channel's diverse omission coverage
(`OCCY_DFA.md` §3 C5).

### Consequence if violated
A worst-case object (e.g. child-pedestrian) is detected too late for the
cap-derived stopping distance → forward collision with a vulnerable road user.
Both this and AOU-PERCEPTION-RANGE-001 must hold for the cap; neither is
kernel-verifiable.

### Cross-references
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 4, §4 clause 2 — the source analysis.
- `OCCY_DFA.md` §3 C5 — the omission common-cause and the D1 disposition.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement.
- Occy **SG1** (RSS) and **SG6** (post-collision / worst-case object) — the goals a
  class-detection gap defeats. Kernel complement as above (Absent → UNSAFE;
  occlusion stop; watchdog).
- `#124` / Item B — the D1 closure.

## AOU-VEHICLE-FRICTION-001 — Effective deceleration ≥ 3.0 m/s² over the ODD

### Assumption
> *Integrator vehicle / road combination shall maintain effective deceleration ≥
> 3.0 m/s² over the deployment ODD; conditions below this threshold are excluded
> from the ODD or trigger a sub-ODD weather-derate per ADR-0002.*

### Why it is load-bearing
The cap's stopping-distance term `v² / 2a` uses the comfortable-decel basis
`a_comfortable = 3.0 m/s²`. The **vehicle** side is analytically covered — the
kernel reference profile `VehicleKinematicsContract::max_brake_mps2 = 4.5 m/s²`
(`src/gateway/kinematics_contract.rs:112`, via `VehicleConfig::default_urban`)
exceeds 3.0 with ~50 % headroom, and the MRC fallback profile holds 3.0
(`mrc_fallback_profile`, `:134`). The **road-friction** side is not: wet / icy /
loose surfaces can reduce achievable deceleration below the basis. The kernel
clamps *commanded* decel to the contract's capability; it cannot guarantee the
tyre-road interface delivers it.

### Evidence
- `src/gateway/kinematics_contract.rs:112` (`max_brake_mps2: 4.5`,
  `VehicleConfig::default_urban`) and `:134` (`mrc_fallback_profile` = 3.0) — the
  vehicle-capability anchor.
- `SPEED_ENVELOPE.md` §2 (`a_comfortable = 3.0 m/s²`, "achievable on wet roads").
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 3, §4 clause 3.
- `ADR-0002` — the condition-dependent sub-ODD weather-derate the road-friction
  residual routes through.

### Scope
- **In scope:** the effective vehicle+road deceleration available over the ODD.
  The **vehicle hardware** half is OK-ANALYTICAL (kernel profile); the **road
  friction** half is the AoU-GAP.
- **Owner:** Integrator (vehicle / road) — ODD definition + weather-derate
  configuration.

### Verification status — **OK-ANALYTICAL** (vehicle) + **AoU-GAP** (road friction)
Vehicle capability is proven by the kernel reference profile. Road-friction
degradation is discharged by the integrator either excluding sub-threshold
conditions from the ODD or wiring an ADR-0002 weather-derate (which the runtime
posture coupling, #99, can drive).

### Consequence if violated
The cap over-states stopping capability on a low-friction surface → the ego
cannot stop within the cap-derived distance → forward collision. Mitigated only
if the integrator excludes the condition from the ODD or derates for it.

### Cross-references
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 3, §4 clause 3 — the source analysis.
- `ADR-0002` — the weather-derate composition; `#99` — runtime posture coupling.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement.
- Occy **SG1** (RSS braking term). Kernel complement: the kinematic-contract brake
  clamp in `validate_vehicle_command` (bounds commanded decel to
  `max_brake_mps2`).

---

# Actuation Output Contract (#127) — SEooC assumptions of use

The actuation clause formalizes the #127 **Actuation Output Contract** into the
register. The Governor authors a verdict; the integrator's actuation pipeline
must *act* on it — both promptly (latency clause) and on loss of it (the
fail-closed safe-stop the fault model already assumes,
`OCCY_FAULT_MODEL.md` — "no Accept emitted → actuator safe-stops"). The kernel
side is `SS-003` LockedOut MRC fallback (`SAFE_STATE_SPECIFICATION.md`): the
Governor emits the MRC; the integrator must realize it.

## AOU-ACTUATION-LATENCY-001 — Safe-stop initiation ≤ 499 ms; safe-stop on loss of verdict

### Assumption
> *Integrator actuation pipeline (control compute + bus latency + actuator
> response) shall complete the safe-stop initiation within 499 ms of the
> Governor's MRC verdict.*

**Companion fail-closed sub-clause (loss of valid verdict).** The integrator's
vehicle interface shall honour the Governor's MRC (`linear_velocity_mps = 0.0`,
`accel_mps2 = −max_decel_mps2`) when published, **and safe-stop on loss of a
valid verdict** within a bounded `T_safe-stop`. The fault model already
classifies loss-of-verdict as "MRC immediately" (`OCCY_FAULT_MODEL.md`; #119) —
this is the integrator's side of that contract.

### Why it is load-bearing
ADR-0001's reaction-time budget is `t_react = 0.5 s` total
(`SPEED_ENVELOPE.md:29`). It splits into perception confirmation (integrator,
#126), the **Governor verdict** (S3-PROVEN: p99.9 ≈ 170–352 ns, ≤ 219 µs jitter
ceiling — 4–6 orders of magnitude of headroom, effectively 0 contribution), and
the **actuation residual**. With the Governor contribution negligible, the
residual budget is `≈ (0.5 s − Governor WCET) ≈ 499.78 ms → 499 ms`. If the
actuation pipeline is slower, the t_react chain the cap rests on is violated. The
fail-closed sub-clause is the deeper invariant: a Governor that emits MRC into an
actuator that does not act is no protection at all (defeats SG9 at the actuator
boundary).

### Evidence
- `SPEED_ENVELOPE.md` §2 (`t_react = 0.5 s` budget) — anchor `SPEED_ENVELOPE.md:29`.
- `GOVERNOR_INTEGRITY_EVIDENCE.md` §5 — the S3 Governor-WCET proof (the
  negligible-contribution basis).
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 2, §3 (the t_react sub-component
  breakdown), §4 clause 4.
- `OCCY_FAULT_MODEL.md` — loss-of-verdict → MRC-immediately; the `T_safe-stop`
  output-contract framing.
- `SAFE_STATE_SPECIFICATION.md` **SS-003** — the LockedOut MRC fallback the
  verdict keys off.

### Scope
- **In scope:** the actuation pipeline latency (control compute + bus + actuator
  response) from the Governor's MRC verdict to safe-stop initiation, and the
  fail-closed-on-loss-of-verdict behavior.
- **Owner:** Integrator (actuation / vehicle interface).

### Verification status — **OK-PROVEN** (Governor) + **AoU-GAP** (actuator residual)
The Governor's contribution to t_react is S3-PROVEN. The 499 ms actuator residual
and the loss-of-verdict safe-stop are integrator obligations, discharged by the
integrator's actuation-pipeline latency characterization and a demonstrated
safe-stop-on-loss-of-verdict (brake-by-wire timing test). Typical brake-by-wire
initiates in tens to ~200 ms, so the budget is generous — the clause exists to
make the contract explicit and surface the rare non-conforming pipeline.

### Consequence if violated
A slow pipeline blows the t_react budget → the cap is unconservative (forward
collision pathway). An actuator that does **not** safe-stop on loss of verdict
defeats the entire fail-closed architecture at its last boundary (the Governor's
MRC never reaches the wheels). Either is safety-critical.

### Cross-references
- `OCCY_SPEED_CAP_VALIDATION.md` §2 row 2, §3, §4 clause 4 — the source analysis.
- `GOVERNOR_INTEGRITY_EVIDENCE.md` §5 — the Governor-WCET proof; `#115` (S3).
- `OCCY_FAULT_MODEL.md` (#119) — loss-of-verdict MRC; `SAFE_STATE_SPECIFICATION.md`
  SS-003.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement.
- Occy **SG9** (fail-closed safe-stop) — the goal a non-acting actuator defeats;
  Occy **SG1** (the t_react term of the cap).
- Companion hardware-platform deployment requirements: **AOU-HW-POWER-001** (DR-1),
  **AOU-HW-COMMBUS-001** (DR-2) — filed below (both posted to #127).

---

# Hardware-platform deployment requirements (#127 — DR-1, DR-2)

DR-1 and DR-2 were posted to #127 as **pre-production hardware-deployment gates**
derived from the quantitative HW-metrics analysis (`OCCY_QUANTITATIVE_METRICS.md`,
KIRRA-OCCY-QUANT-001). They are recorded here verbatim. **Note (honest filing):**
unlike the actuation-latency clause above, DR-1/DR-2 are **hardware platform**
(PMHF / LFM) requirements on the Governor's compute element — they are *not*
actuation-latency sub-clauses; they are filed as their own register entries and
cross-linked from AOU-ACTUATION-LATENCY-001 because they share the #127 tracker.

## AOU-HW-POWER-001 (DR-1) — ASIL-D-class power supply for the Governor compute

### Assumption
> *DR-1 — Power supply: the Governor's D3 compute must be powered by an
> ASIL-D-class redundant or supervised supply (dual-PMIC with voted outputs, or
> single PMIC with ≥ 99 % built-in self-test diagnostic coverage). Single-supply
> configuration yields 10 FIT residual from the power sub-element alone, exceeding
> the ASIL-D PMHF target of 10 FIT for the entire Governor element.*

### Why it is load-bearing
The Governor element carries an ASIL-D PMHF budget. The quantitative analysis
shows a **single-supply PMHF = 17.7 FIT (FAIL)** vs **dual-supply = 8.7 FIT
(PASS)** — the power sub-element alone can blow the whole-element target, so the
fail-closed safety argument's random-hardware-failure budget is not met without a
redundant/supervised supply.

### Evidence
- `OCCY_QUANTITATIVE_METRICS.md` (KIRRA-OCCY-QUANT-001) — SPFM/LFM/PMHF analysis;
  single-supply 17.7 FIT FAIL / dual-supply 8.7 FIT PASS.

### Scope
- **In scope:** the power supply to the Governor's D3 compute element.
- **Owner:** Integrator (hardware / platform). Pre-production deployment gate.

### Verification status — **AoU-GAP** — pre-production hardware-deployment gate
Discharged by the integrator selecting an ASIL-D-class redundant/supervised
supply and recording the resulting PMHF in their hardware safety case.

### Consequence if violated
The Governor element's PMHF exceeds the ASIL-D target — a random power-element
fault can disable the safety function without diagnosis, undermining the
fail-closed claim's quantitative basis.

### Cross-references
- `OCCY_QUANTITATIVE_METRICS.md` (KIRRA-OCCY-QUANT-001) — the source.
- `AOU-ACTUATION-LATENCY-001` — the #127 actuation contract it is tracked beside.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement.
- Occy **SG9** (fail-closed integrity) — the goal an under-budget power element
  undermines.

## AOU-HW-COMMBUS-001 (DR-2) — Comm-bus latent-fault metric ≥ 90 %

### Assumption
> *DR-2 — Comm bus LFM: the Governor's communication path (Automotive Ethernet
> PHY+MAC) must achieve LFM ≥ 90 %, via either (a) a redundant ASIL-D TSN stack or
> (b) a documented periodic self-test protocol executed at startup and every 24 h
> of operation. Without this, the rolled LFM for the Governor element is ~89.5 %,
> missing the ASIL-D target by 0.5 pp.*

### Why it is load-bearing
The ASIL-D Latent-Fault-Metric target is **≥ 90 %** per sub-element. The comm
path's rolled LFM lands at **~89.5 %** without additional latent-fault detection —
0.5 pp short — so a latent fault in the communication element could go undetected
and accumulate toward a dual-point failure.

### Evidence
- `OCCY_QUANTITATIVE_METRICS.md` (KIRRA-OCCY-QUANT-001) — LFM ≥ 90 % per-sub-element
  gate; comm-path ~89.5 % without mitigation.

### Scope
- **In scope:** the Governor's communication path (Automotive Ethernet PHY+MAC)
  latent-fault detection.
- **Owner:** Integrator (hardware / platform). Pre-production deployment gate.

### Verification status — **AoU-GAP** — pre-production hardware-deployment gate
Discharged by either a redundant ASIL-D TSN stack or a documented startup +
24 h periodic self-test, with the resulting LFM recorded in the integrator's
hardware safety case.

### Consequence if violated
The Governor element misses the ASIL-D LFM target — latent communication faults
may accumulate undetected, eroding the dual-point-failure protection the ASIL-D
argument assumes.

### Cross-references
- `OCCY_QUANTITATIVE_METRICS.md` (KIRRA-OCCY-QUANT-001) — the source.
- `AOU-ACTUATION-LATENCY-001` — the #127 actuation contract it is tracked beside.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement.
- Occy **SG9** (fail-closed integrity) — the goal an under-target LFM undermines.

---

# Localization integrity (#123) — SEooC assumption of use

The clause below files the **G2 localization assumption-of-use** as a stable
register entry. It was derived and dispositioned in the SG2 lateral-margin
analysis (`OCCY_SG2_MARGIN.md`, KIRRA-OCCY-SG2-MARGIN-001 §2–3, §5); this register
**files** it — it does not re-derive. The **runtime complement** (the parko-core
localization-integrity gate, #123 runtime half / PR #264) is merged; this is its
**contractual** half. The two are a pair: the gate fail-closes every map-anchored
veto when the integrator's integrity reporting says the bound is not currently
held, and this AoU records the obligation that the bound *be* held (and the
documented fallback for ODDs that cannot).

## AOU-LOCALIZATION-001 — Integrator localization ≤ 0.10 m 95th-pct lateral error

### Assumption
> *Integrator localization shall achieve ≤ 0.10 m 95th-percentile lateral
> (cross-track) error within the deployment ODD; ODDs that cannot meet this bound
> shall deploy the documented conservative-fallback margin configuration
> (0.75 m) instead.*

### Why it is load-bearing
The SG2 lateral-containment margin `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m`
(`src/gateway/containment.rs:56`, SG2 ASIL D) is **derived** assuming the
localization cross-track term `ε_localization = 0.10 m` — the *typical*-column
value in the `OCCY_SG2_MARGIN.md` §2 error-budget table. That same row records the
**worst-case** urban-canyon NDT / visual-LiDAR figure of `≈ 0.30 m` — exactly the
case the 0.10 m bound **excludes**. The 0.40 m PRIMARY setting is the typical-term
sum; substituting the 0.30 m worst-case localization term is what drives the
0.75 m CONSERVATIVE FALLBACK (`OCCY_SG2_MARGIN.md` §3). So a violated bound
silently consumes the containment headroom the ASIL-D SG2 argument depends on.

Additionally — and this is the broader reach the runtime gate makes explicit —
**every map-anchored trust in the kernel interprets map geometry through the ego
pose:** all SG5 commit-zone enforcement (the #260–#262 bricks — a mapped rail
crossing / box junction is located *relative to the ego*) and the SG4
`MapKnownSafe` water earn-back (a mapped ford is anchored in the map frame). A
localization bound violation **silently mislocates every one of these
map-anchored vetoes** — the veto may fire for the wrong stretch of road, or fail
to fire for the right one — without any single check observing the fault. This is
why the assumption is filed once, centrally, rather than inline under SG2 alone.

### Evidence
- `OCCY_SG2_MARGIN.md` §2 error-budget table — the `ε_localization` row
  (typical **0.10 m** / worst-case 0.30 m) — anchor `OCCY_SG2_MARGIN.md:45`; and
  the rounded-sum row (PRIMARY **0.40 m** / FALLBACK **0.75 m**) at
  `OCCY_SG2_MARGIN.md:49`.
- `OCCY_SG2_MARGIN.md` §3 margin-setting table — PRIMARY 0.40 m (conditional on
  the ≤ 0.10 m G2 AoU) vs CONSERVATIVE FALLBACK 0.75 m (uncharacterized accuracy,
  `ε_localization > 0.10 m`, or urban-canyon / multipath ODD) — anchor
  `OCCY_SG2_MARGIN.md:73`.
- `OCCY_SG2_MARGIN.md` §5 — the G2 localization assumption-of-use statement
  (≤ 0.10 m, 95th-percentile, evaluated over the ODD) — anchor
  `OCCY_SG2_MARGIN.md:99`.
- `src/gateway/containment.rs:56` — `CONTAINMENT_LATERAL_MARGIN_M = 0.40` (the
  derived constant the bound underwrites).

### Scope
- **In scope:** the integrator localization stack's 95th-percentile cross-track
  (lateral) accuracy over the **full deployment ODD**, including the urban-canyon
  / multipath conditions the worst-case column names.
- **Owner:** Integrator (localization). Base-tier; KIRRA takes no base-tier
  measurement of localization accuracy (`OCCY_SG2_MARGIN.md` §5 — "pilot does not
  measure").

### Verification status — **AoU-GAP** (base) — integrator-characterized
No KIRRA base-tier measurement. Discharged for a deployment by the integrator's
per-deployment characterization of the localization stack vs. ground truth on a
representative track (the deployment-specific **G2 evidence package** named in
`OCCY_SG2_MARGIN.md` §5). An ODD that **cannot** meet the bound — or whose
localization accuracy is uncharacterized, or which contains urban-canyon /
multipath zones outside the integrator's localization profile — must instead
deploy the **0.75 m conservative-fallback** margin configuration
(`OCCY_SG2_MARGIN.md` §3; a deployment config flag, **not** the code default).

**Kernel complement (runtime half, #123 / PR #264).** The parko-core
localization-integrity gate (`parko/crates/parko-core/src/localization.rs`:
`localization_trusted`, `gate_commit_zone_scene`, `gate_water_scene`) is the
runtime reaction to a **reported** bound violation: when the integrator's
integrity channel reports lateral error over the bound (or is stale / absent),
every map-anchored commit-zone scene degrades to `Unknown` (fail-closed veto) and
the `MapKnownSafe` water earn-back is stripped (operator authority survives —
it is not map-frame-dependent). The gate **does not measure** the bound; it
fail-closes on the integrator's report that the bound is not met. It is a
mitigation, not a discharge — this AoU remains the integrator obligation.

### Consequence if violated
The SG2 containment margin is **unconservative**: the 0.40 m PRIMARY value no
longer covers the true cross-track error budget, so the ego can leave the
intended corridor inside the margin the ASIL-D argument assumes — a lateral
departure / lane-edge-incursion pathway. Concurrently, **every map-anchored SG5
commit-zone veto and the SG4 `MapKnownSafe` earn-back is referenced to a wrong
pose** — a high-consequence zone (rail crossing, box junction, narrow bridge) or
a mapped ford is mislocated, so the veto can mis-fire or silently fail to fire.
The kernel cannot self-detect the pose error; it is mitigated only by the
integrator holding the bound (or deploying the 0.75 m fallback), with the runtime
gate fail-closing whatever the integrity channel does report.

### Cross-references
- `OCCY_SG2_MARGIN.md` (KIRRA-OCCY-SG2-MARGIN-001) §2–3, §5 — the source analysis
  (the error budget, the PRIMARY / FALLBACK setting, and the G2 AoU statement);
  this register files it without re-deriving.
- `src/gateway/containment.rs:56` — `CONTAINMENT_LATERAL_MARGIN_M = 0.40` (the
  derived SG2 ASIL-D constant the bound underwrites).
- `parko/crates/parko-core/src/localization.rs` (#123 runtime half / PR #264) —
  the runtime localization-integrity gate (the kernel complement to this clause).
- The SG5 commit-zone bricks **#260–#262** and the SG4 water `MapKnownSafe`
  earn-back (#98) — the map-anchored mechanisms a violated bound mislocates.
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement; a violation
  is a path to unreasonable risk via the SG2 margin and the map-anchored vetoes.
- Occy **SG2** (lateral containment) — the goal whose margin this bound
  underwrites; **SG5** (commit-zone) and **SG4** (water-untraversable) — the
  map-anchored goals a violated pose mislocates.
- `#123` — the issue (runtime gate = PR #264; this clause = the docs half).

---

# Post-collision clearance (#103) — SEooC assumption of use

The clause below files the **operator-authentication boundary** of the SG6
post-collision clearance loop. The runtime structure (the `ClearanceLoop` state
machine, #103 runtime half / PR #267) is merged; this is its **contractual**
half. The two are a pair: the loop guarantees that — once immobilized after a
detected impact — the vehicle CANNOT resume except via a well-formed operator
grant (structural no-resume), and this AoU records the obligation the loop
cannot itself discharge: that a grant is only ever issued to an **authenticated**
operator. parko enforces *structure*, not *identity*.

## AOU-CLEARANCE-AUTH-001 — Clearance grants issued only after operator authentication

### Assumption
> *The integrator / verifier shall issue an `OperatorClearanceGrant` (the only
> input that releases the SG6 post-collision immobilization) ONLY after it has
> authenticated the clearing operator. The parko clearance loop enforces that a
> grant is structurally well-formed; it does NOT — and cannot — authenticate the
> operator's identity or authority.*

### Why it is load-bearing
SS-003's safe-state intent for a post-collision latch is *"human intervention
required"* — a person may be under or near the vehicle, so the governor
immobilizes and resumes only on a deliberate human act (contrast the automatic
SS-001/SS-002 recovery). The `ClearanceLoop` (#103) makes that **structural**:
once `Latched` / `EscalationRaised`, the ONLY transition back to `Normal` is
`ClearanceLoop::try_clear` with a grant that passes
`OperatorClearanceGrant::is_well_formed` (non-empty operator id, not
future-dated, not stale). Clean evidence never clears it.

But *structure is not identity.* A well-formed grant proves only that the input
is shaped correctly and fresh — NOT that it came from a real, authorized
operator. parko, by design (ADR-0004 independent governor), holds **no**
credential store and performs **no** authentication: it checks the output of the
surrounding system, it does not own the operator-trust boundary. If the
integrator issues grants without authenticating the operator, the structural
no-resume is defeated at its only door — anything that can synthesize a
well-formed grant can resume a post-collision-immobilized vehicle. The
authentication itself lives in the verifier / `kirra_core` reset mechanism
(`KIRRA_SUPERVISOR_RESET_KEY`, #255) — a constant-time-compared, env-sourced,
non-empty supervisor key — which is the authenticated act that should precede
grant issuance.

### Evidence
- `parko/crates/parko-core/src/impact.rs` — `ClearanceLoop` (the structural
  no-resume state machine) and `OperatorClearanceGrant::is_well_formed` (the
  shape/freshness check, explicitly NOT authentication; the boundary is stated in
  the type's doc-comment).
- `docs/safety/SAFE_STATE_SPECIFICATION.md` **SS-003** — the
  LockedOut / hard-stop safe state whose recovery is by **human reset** (the
  intent the clearance loop realizes for the SG6 post-collision case).
- `KIRRA_SUPERVISOR_RESET_KEY` (#255) — the authenticated, env-sourced,
  constant-time-compared supervisor reset key the integrator/verifier should gate
  grant issuance behind (read in `src/ffi.rs` / `src/main.rs`; invariant #7 in
  `CLAUDE.md` — present, non-empty, ≤ 64 bytes).

### Scope
- **In scope:** the authentication of the operator BEFORE a clearance grant is
  issued to the SG6 clearance loop.
- **Owner:** Integrator (verifier / operations). The kernel structurally requires
  a well-formed grant; the integrator owns who is allowed to produce one.
- **Out of scope (named deferrals):** operator-notification **transport** — how
  `ClearanceLoop::escalation_pending` reaches a human (UI / paging) is integrator
  territory; and the cryptographic binding of a grant to an authenticated session
  (a possible future hardening — today the grant is a structurally-validated
  value, not a signed token at the parko layer).

### Verification status — **AoU** (by design)
Not a gap to be closed in parko — it is an architectural boundary (ADR-0004): the
independent governor enforces structure and delegates identity. Discharged for a
deployment by the integrator wiring grant issuance behind an authenticated
operator action (e.g. the #255 supervisor-reset path) and recording that binding
in their integration safety case. The structural half is **live** (#103 / PR
#267): the loop admits no other un-latch path, and every clearance attempt
(grant accepted OR rejected, with reason + operator-id subject) lands in the
tamper-evident audit chain via the #263 sink family — so an unauthenticated or
malformed clearance attempt is itself an audited event.

### Consequence if violated
If grants are issued without authentication, the SG6 post-collision no-resume is
defeated at its only door: a vehicle immobilized after a detected collision —
potentially with a person underneath — could be resumed by any party able to
synthesize a well-formed grant. This is precisely the high-consequence,
human-in-the-loop case SS-003 exists for; the kernel cannot self-detect a forged
authorization, which is why authentication is an explicit integrator obligation.

### Cross-references
- `parko/crates/parko-core/src/impact.rs` (#103 / PR #267) — `ClearanceLoop` /
  `OperatorClearanceGrant`, the structural no-resume this clause complements.
- `docs/safety/SAFE_STATE_SPECIFICATION.md` SS-003 — the human-reset safe state.
- `#255` (`KIRRA_SUPERVISOR_RESET_KEY`) — the authenticated reset mechanism that
  should gate grant issuance; `CLAUDE.md` invariant #7.
- `#263` — the audit bridge that records clearance / escalation / rejection
  events (the transparency half of the post-collision sequence, #104).
- `UL4600_SAFETY_CASE.md` (G-UL-TOP) — assumed external requirement; an
  unauthenticated resume is a path to unreasonable risk at the SG6 boundary.
- Occy **SG6** (post-collision immobilize) — the goal this clause's authentication
  precondition protects.
- `#103` — the issue (runtime structure = PR #267; this clause = the docs half).

---

# Hypervisor contract-channel time synchronization (#278 / EPIC #270) — SEooC assumption of use

The clause below files the **time-synchronization obligation** the hypervisor
contract-channel spec leans on. `HYPERVISOR_CONTRACT_CHANNEL.md`
(KIRRA-OCCY-HVCHAN-001) §5 R-HV-3 resolves the channel's clock question into a
**two-clock-domain model with a normative non-mixing rule** (safety/boundary
timing vs system timing); this register **files** the integrator obligation that
model depends on. It is the **contractual** half of that §5 resolution — the spec
defines the domains; this AoU records what the integrator must guarantee about the
timestamps that enter the **boundary** one.

## AOU-TIMESYNC-001 — Synchronized, monotonic, boundary-domain-converted timestamps

### Assumption
> *Integrator-provided sensor and message timestamps consumed by governor
> staleness/deadline validation shall originate from a synchronized, monotonic
> time source meeting a defined drift bound relative to the boundary clock domain
> (bound: **VALIDATION-PENDING**, to be set with the FTTI budget); timestamps
> shall be converted to the boundary clock domain before publication into the
> contract channel.*

### Why it is load-bearing
Every staleness check the governor performs is structurally
`now − message_timestamp < deadline` — concretely the HVCHAN §3 judge step
`now > deadline ⇒ reject`, the same `now_monotonic_ns > deadline_monotonic_ns ⇒
DeadlineMissed` check in the #271 harness judge (`kirra_judge.rs`
`kirra_judge_assess`), and the same deadline discipline in the #273 spike judge.
That comparison is only meaningful if `message_timestamp` and `now` are in the
**same monotonic domain** and the source neither jumps nor runs backwards:
- an **unsynchronized** source makes `now − message_timestamp` an arbitrary
  quantity — a stale command can compute as fresh, or a fresh one as stale;
- a **non-monotonic** source (wall-clock step, NTP/PTP slew backwards, counter
  reset) can make a real delay read as negative → a **stale command admitted as
  fresh**.

This is the §5 **non-mixing rule** as an integrator obligation: the guest must
convert to the boundary domain **before** publishing, because the governor — by
design — reads **only** the boundary clock on its validation path and never
wall/PTP time. An out-of-domain or unsynchronized timestamp **silently disables
the deadline barrier**: no fault is raised, the check simply stops meaning what it
asserts — the guarantee degrades without any detectable failure.

### Evidence / consuming mechanisms
- `HYPERVISOR_CONTRACT_CHANNEL.md` (KIRRA-OCCY-HVCHAN-001) **§5 R-HV-3** — the
  two-clock-domain model + non-mixing rule this AoU discharges; **§4** — the
  `cross-domain timestamp` and `clock skew beyond bound` fail-closed rows that
  catch *gross* violations.
- `tools/qnx-rtm-harness/kirra_judge.rs` (`kirra_judge_assess`) — the deadline
  check `now_monotonic_ns > deadline_monotonic_ns ⇒ DeadlineMissed` (#271).
- `tools/iceoryx2-spike/` judge — the same deadline/`<=`-freshness discipline (#273).
- `ADR-0006` Clause 2 — the partition-boundary contract channel the boundary
  domain lives in.

### Scope
- **In scope:** the synchronization, monotonicity, and **boundary-domain
  conversion** of every sensor/message timestamp the governor's staleness/deadline
  validation consumes.
- **Owner:** Integrator (time synchronization / platform). KIRRA takes **no**
  base-tier measurement of the integrator's sensor-clock synchronization; it
  validates only the timestamps presented in the contract channel, already in the
  boundary domain.
- **Out of scope:** the boundary clock **provision** itself (the hypervisor shared
  monotonic source + bounded skew, HVCHAN R-HV-3) — a separate, not-yet-filed
  hypervisor AoU, target work (#274/#278); and **system-timing** uses (sensor
  fusion, audit ordering, fleet analysis) that never touch the governor validation
  path.

### Verification status — **AoU-GAP** (integrator obligation; no kernel measurement of sensor-clock sync)
No KIRRA base-tier measurement of upstream clock synchronization. Discharged for a
deployment by the integrator establishing a synchronized, monotonic time
distribution and characterizing its drift against the boundary domain within the
**VALIDATION-PENDING** bound (to be fixed with the FTTI budget on target,
#274/#278). **PTP / gPTP (IEEE 1588 / IEEE 802.1AS) is the *expected* discharge
mechanism — named, not mandated**; any source meeting the synchronization,
monotonicity, and drift-bound properties satisfies the clause.

**Partial runtime mitigations (honest — catch gross, not subtle).** The §4
`cross-domain timestamp` (domain-tag / range-plausibility) and `clock skew beyond
bound` rows, plus the judge's monotonic-`sequence` `<=` reject, catch **gross**
violations — an out-of-range timestamp, an obviously-backwards counter, a stale
generation. They do **NOT** catch **drift within the plausible range**: a slowly
desynchronized but still-plausible timestamp passes every runtime check while
quietly eroding the deadline margin. Only the integrator's sync discipline closes
that — the runtime checks are a backstop, **not** a discharge.

### Consequence if violated
Staleness validation becomes **meaningless**: stale commands are admitted as fresh
(or fresh ones spuriously rejected). The deadline barrier — one of the channel's
fail-closed guarantees — is **silently disabled** with no detectable fault,
defeating the freshness/liveness property the staleness check exists to provide.
**SG-relevant:** this is the same deadline/staleness barrier the SG-003
sensor-liveness posture and the §3 judge deadline check rest on; with it silently
off, a stale command reaches actuation as if fresh. This is exactly why it is
filed as an explicit integrator obligation rather than assumed.

### Cross-references
- `HYPERVISOR_CONTRACT_CHANNEL.md` (KIRRA-OCCY-HVCHAN-001) §5 R-HV-3 (the
  two-domain resolution + non-mixing rule) and §4 (the `cross-domain timestamp` /
  `clock skew beyond bound` fail-closed rows) — the source resolution this clause
  files.
- `ADR-0006` Clause 2 — the partition-boundary contract channel.
- The **#126 perception-AoU family** (AOU-PERCEPTION-FRAME-001 / -RANGE-001 /
  -CLASS-001) — the sibling integrator-input obligations; this is the **time**
  analogue of those **frame / range / class** perception-input assumptions (an
  integrator-owned property of the data the governor consumes, kernel-unverifiable
  at base tier, fail-closed only against gross violation).
- **EPIC #270**; **#278** (the HVCHAN design + hardware implementation that fixes
  the drift bound); **#279** (the fault-injection campaign whose clock-domain /
  skew cases exercise the §4 rows).
- **PTP / gPTP** (IEEE 1588 / IEEE 802.1AS) — the **expected** discharge mechanism
  (named, **not** mandated).
