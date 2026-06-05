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

This file is the **central register** of those assumptions. Until now AoUs were recorded
inline where they arose (e.g. the SG2 lateral-margin G2 AoU #123 in
`OCCY_SG2_MARGIN.md`, and the AoU-GAP dispositions in `OCCY_SPEED_CAP_VALIDATION.md`).
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
