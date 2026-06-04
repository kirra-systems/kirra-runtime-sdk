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
