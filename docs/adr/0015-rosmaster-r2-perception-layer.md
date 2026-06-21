# ADR-0015: Rosmaster R2 perception layer — feeding the KIRRA perception input contract (geometric-first, then Parko ML)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — for owner sign-off; ratified on merge. |
| Date | 2026-06-21 |
| Deciders | Project / safety-case owner |
| Parent | ADR-0014 (R2 + Orin NX integration) — this is its Phase-2 perception detail |
| Sensors | R2: 2D lidar (`/scan`), Orbbec Astra Pro (RGB + depth / point cloud), IMU + wheel odom |
| Cross-refs | #126 (Base-tier Perception Input Contract, SEooC AoU), #127 (actuation safe-stop AoU), #131 (Option-B validation), KIRRA-OCCY-PMON-003 (perception derate), Parko (`parko-core` / `parko-tensorrt`); code: `perception_ingest.rs`, `corridor/` (`CorridorSource`), `gateway/perception_monitor.rs`, `gateway/containment.rs`, `validation.rs` |

## Context — perception is a bounded, monitored INPUT, not free-form

KIRRA does not *compute* perception; it *consumes* it as a SEooC input (#126) that it bounds and
monitors. Three distinct consumers exist for the R2's raw sensors, with **different trust
semantics** — they must not be conflated (the blueprint / earlier mapping blurred them):

1. **Sensor → text for the LLM** (System-2 cognition): QM, *untrusted*, for reasoning. **Not** a
   safety input.
2. **Sensor *health* → verifier `/fleet/diagnostics/report`**: drives node trust / posture
   (is the sensor alive / confident).
3. **Sensor → perception → the KIRRA safety input contract** (*this ADR*): RSS (SG1),
   containment (SG2), the perception derate (PMON).

The same lidar feeds all three; this ADR is **#3 — the safety-perception layer**.

## The contract — what perception must produce (read from the code)

**Every output carries health (confidence + freshness).** Absent / stale / low-confidence /
implausible → KIRRA fail-closes to degraded / MRC. That monitoring is the SEooC bound (#126).

1. **`PerceivedObject[]`** (`state.rs`) — per object: `id`, `pos`, `velocity_mps` (magnitude),
   `heading_rad`, `vel` (world-frame velocity *vector*). → RSS distance (SG1) + Track-C
   kinematic plausibility.
2. **`Corridor`** (`containment.rs`) — `left[]` / `right[]` boundary points + `confidence` +
   `age_ms` (+ `min_confidence` / `max_age_ms`). → SG2 drivable-space containment.
3. **`PerceptionOutput`** (`perception_monitor.rs`) — `confidence`, detection `range`,
   **implausible-fraction**. → graded speed **derate** (implausible-fraction → cap-factor table;
   untrusted range → `R_obs = 0` → controlled stop).

KIRRA's **consumption side is already built** (`perception_ingest.rs`, the `CorridorSource`
trait + `MockCorridorSource`, `validate_trajectory_slow`, the PMON derate) and is fed today by
`MockCorridorSource`. **This layer replaces the mock with the R2's real sensors.**

## Decision — R2 sensors → the contract, geometric-first

### Sensor mapping

- **2D lidar (`/scan`)** → cluster + track → `PerceivedObject[]` (pos from ranges; velocity from
  frame-to-frame tracking; confidence from cluster persistence; heading from track). And →
  geometric free-space → `Corridor`. **No ML model.**
- **Astra Pro depth** → point cloud → geometric ground / obstacle segmentation → corridor
  refinement + obstacle confirmation + detection-range / confidence. **No ML model.**
- **Astra Pro RGB** → *(Phase B)* a detector via **Parko's TensorRT backend** → semantic objects
  (class + confidence) → richer `PerceivedObject`.

### Phasing

- **Phase A — geometric, model-free (build first).** Lidar / depth → objects + corridor +
  health, wired into `perception_ingest` / `CorridorSource`. Lights up **SG1 RSS + SG2
  containment + the PMON derate on real R2 data with zero ML**; CPU-only; exercises all KIRRA
  plumbing so Phase B is a drop-in. Maximum safety value per effort.
- **Phase B — Parko ML detector (the "perception model").** RGB / depth detector → TensorRT via
  Parko → semantic classes. The safety plumbing is already proven by Phase A; this adds
  classification + robustness.

## Two must-gets (the code is explicit about these)

1. **World-frame absolute velocities.** `PerceivedObject.vel` is documented as *map / world-frame
   ground-velocity* (PMON-003 §4 / the **D4 frame-confirm** gate). The R2's lidar gives
   *sensor-frame* objects while the robot is moving — so **ego-motion-compensate** (fuse odom /
   IMU) before reporting velocity, or RSS + plausibility math is wrong. The single most
   important correctness gate.
2. **Enablement is gated, default-OFF.** The derate is env-gated
   (`KIRRA_PERCEPTION_DERATE_ENABLED`, default off); enabling on a real vehicle is additionally
   gated on the D4 frame-confirm + a sim / bench validation gate. Sequence: **build → validate on
   bench / sim → then enable.** Never flip it on live first.

## Non-goals / rejected

- **Perception is not trusted blindly.** KIRRA *bounds and derates* it (checker over doer): a
  high-confidence-but-wrong perception still cannot exceed the kinematic envelope or re-initiate
  from a stop. Perception tightens the envelope; it never loosens it.
- **No raw-telemetry streaming to the console** (#394 out-of-mandate) — the console shows
  governance, not lidar / pose.
- **Full Autoware perception is not adopted.** This is the R2-scoped layer feeding the *same*
  contract Autoware's Option-B path feeds (#131) — interchangeable at the contract boundary.

## Status

**Proposed — for owner sign-off.** The ROS2 perception node itself is laptop + hardware, but
this pins the build target (three outputs + health + world-frame + enablement gate) against
*existing* interfaces, so it is a **wiring job against a real contract, not research**. Sequenced
as ADR-0014 Phase 2; Phase A (geometric) can start as soon as the R2 publishes `/scan` + odom.
