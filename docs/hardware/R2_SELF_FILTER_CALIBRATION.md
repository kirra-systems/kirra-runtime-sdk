# R2 LiDAR self-filter calibration

**Issue:** #1210 · **Status:** machinery landed, physical capture NOT performed
· **Applies to:** ROSMASTER R2 (Jetson Orin NX), forward LiDAR

---

## 1. What this document is for

`kirra_taj::self_filter` can discard the robot's own LiDAR returns. It ships
**disarmed** — `TajPhaseA::self_filter` defaults to `None` and nothing is
filtered — because nobody has yet measured which returns are the robot.

This document is the procedure that produces that measurement, and the rule for
turning it into a mask. Until it has been run on a physical R2, the correct
configuration is no mask.

## 2. Why a wrong mask is worse than no mask

Every other operation in Taj tightens the envelope. A detected object narrows
the corridor; a bad scan drops confidence; a semantic hazard clips the clear
distance. When those are wrong, the robot slows down.

Self-filtering is the only operation that goes the other way. Discarding a
return **adds** free space: the corridor widens, an object disappears, the
assured-clear distance grows. A mask that is slightly too generous does not
make the robot cautious — it makes it blind to whatever is standing in the
region you told it to ignore.

That asymmetry is why this procedure is a measurement rather than an estimate,
and why the code refuses a mask reaching further than roughly the robot's own
body (`SelfFilterBounds::for_footprint`, backstopped at
`MAX_SELF_MASK_VERTEX_RADIUS_M`).

**Running unfiltered is a real cost, not a free default.** The R2 reports its
own mast as an obstacle, so the corridor is narrower and the speed cap lower
than the world justifies. That is availability lost to caution, and it is the
right trade to hold until the capture exists.

## 3. Capture procedure

Everything below is done with the **wheels raised or motion disabled**. No part
of this requires the robot to move.

### 3.1 Prepare

1. Place the R2 on blocks in an open area with no object within 2 m in any
   direction. Photograph the setup, including a tape measure showing the
   nearest wall.
2. Record: date, robot serial, LiDAR model and firmware, mount hardware
   revision, and whether any physical modification has happened since the last
   capture.
3. Confirm the LiDAR is publishing at its rated rate before recording. A frozen
   sensor produces a beautifully consistent capture of nothing (that failure is
   #1211's subject; do not let it contaminate this one).

### 3.2 Record

4. Record at least 60 seconds of raw `/scan` with the robot stationary.
5. Turn the steering to full left lock, record 30 s. Repeat at full right lock.
   Steering changes what the wheels occlude, and a mask authored at centre lock
   can be wrong at full lock.
6. Move anything articulated (mast, camera arm) through its range, recording
   throughout. **Any return that moves with the robot's own articulation is
   body**, and a mask authored in one pose can be wrong in another. If a
   protrusion sweeps a region, the mask must cover the swept region rather than
   the single pose — subject to the radius bound, which is the check that stops
   "swept region" from quietly becoming "everything".

### 3.3 Identify

7. Overlay all frames. Returns that are **stationary in the sensor frame across
   every pose** are candidate body returns.
8. For each candidate cluster, physically identify what it is. Walk around the
   robot and confirm by hand: this cluster is the mast, this one is the rear
   cable loop. Record the identification with a photograph.
   **A cluster you cannot physically name does not go in the mask.** An
   unexplained stationary return might be a bracket; it might also be a table
   leg that was there for every recording.
9. Record the angular sector and range extent of each identified cluster.

### 3.4 Verify the transform

10. Measure, with a tape, the LiDAR's optical centre relative to `base_link`:
    x, y, and yaw. Do not take these from the URDF — record what the built
    robot actually has, and note any discrepancy against the model, because a
    discrepancy is itself a finding.
11. Record the measurement in the mask's `SensorMount` with
    `MountProvenance::Measured`. `assert_mount_verified()` refuses anything
    else for production.

> **Open finding.** Taj currently treats the sensor origin as the base origin —
> an implicit identity transform that has never been checked. If the LiDAR is
> mounted forward of `base_link`, every object Taj reports is off by that
> offset, and no test in this repository would catch it. Step 10 is what closes
> that, and the number it produces may require a follow-up to the corridor and
> clear-distance geometry. Do not assume the answer is zero because that is
> what the code assumes today.

## 4. Authoring the mask

Author polygons in the **sensor frame** — where the returns natively live, so
the mask does not inherit any error in the transform.

```rust
use kirra_taj::{
    MountProvenance, SelfFilterBounds, SelfFilterMask, SelfFilterPolygon, SensorMount, TajConfig,
    TajTracker,
};

let mount = SensorMount {
    x_m: 0.0,   // from step 10 — MEASURED, not from the URDF
    y_m: 0.0,
    yaw_rad: 0.0,
    provenance: MountProvenance::Measured,
};

let mask = SelfFilterMask::new(
    1, // revision — bump on every geometry change
    vec![SelfFilterPolygon::new(vec![
        // the mast, from step 9
        (0.13, -0.02),
        (0.25, -0.02),
        (0.25, 0.12),
        (0.13, 0.12),
    ])],
    mount,
    SelfFilterBounds::for_footprint(0.203, 0.330, &mount), // measured R2 body
)?;
mask.assert_mount_verified()?; // production gate

let tracker = TajTracker::with_self_filter(TajConfig::default(), mask);
```

Rules the code enforces, so you do not have to remember them:

| Rule | Mechanism |
|---|---|
| No vertex further than the body radius plus mount offset plus 0.10 m | `SelfFilterBounds::for_footprint` |
| Never further than 1.0 m regardless of configuration | `MAX_SELF_MASK_VERTEX_RADIUS_M` |
| A return exactly on the boundary is kept, not filtered | `SelfFilterPolygon::contains` |
| A mask consuming over half the scan makes the frame unhealthy | `MAX_SELF_MASKED_RAY_FRACTION` |
| An over-large mask is refused, never silently shrunk | `SelfFilterError::ExceedsRadius` |
| An unmeasured mount is refused for production | `assert_mount_verified` |

Rules you must hold yourself:

- **Author tight, not comfortable.** Every millimetre of margin you add is a
  millimetre of the world the robot cannot see. If a return sits near the mask
  edge and you are unsure, leave it outside — an occasional phantom obstacle
  costs availability, and the other error costs more.
- **Bump the revision on every change.** The digest changes automatically; the
  revision is what a human reads. A digest that moved while the revision stood
  still is itself a finding.

## 5. Verification after authoring

Before a masked configuration goes anywhere near a floor:

1. Re-run the stationary capture. Confirm `rejected.self_masked` is non-zero
   and stable, and that no identified body cluster survives.
2. Place a known obstacle — a 100 mm box — at the mask boundary and step it
   outward in 10 mm increments. **Every position outside the mask must produce
   an object.** This is the test that matters; the rest is bookkeeping.
3. Repeat step 2 at full left and full right lock.
4. Confirm the corridor width with the mask armed is not wider than the
   physically measured free space. A corridor claiming more room than the room
   has is the failure this whole document exists to prevent.
5. Record `rejected.self_masked` as a fraction of the ray count. Single-digit
   percent is expected for the R2. Anything approaching
   `MAX_SELF_MASKED_RAY_FRACTION` means the mask is wrong, not that the ceiling
   is too low.

## 6. What is still open

- **The capture itself.** Steps 3.1–3.4 need a physical R2 and have not been
  run. This is the blocked half of #1210.
- **The transform.** Step 10's measurement does not exist, so the implicit
  identity transform stands (see the open finding in §3.4).
- **Digest enforcement across processes.** `SelfFilterMask::digest_hex()` gives
  a mask a stable identity, but nothing yet refuses startup when a node's mask
  digest disagrees with the platform manifest — because there is no platform
  manifest yet. That is #1219, and the digest here is the hook it will use.
- **Rejected-point metrics as a time series.** The tally reaches the perception
  wire (`PerceptionResponse::rejected`); exporting it belongs with the watchdog
  work in #1211, which needs the same plumbing.
