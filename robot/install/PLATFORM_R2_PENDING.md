# Layer B — steering / R2 platform config: 🔴 PENDING R2 IMAGE + VALIDATION

> **This layer is NOT complete. It requires the validated R2 base image and
> on-hardware drive+steering verification before it can be scripted. Do not
> run it; do not assume steering works.** Nothing in Layer A
> (`install_kirra.sh`) configures steering or car-type 5.

This stub exists so the gap is explicit and the completion path is written
down — the difference between "the image mysteriously doesn't steer" and
"here is exactly what's needed." Provenance for every finding:
`docs/hardware/HARDWARE_FINDINGS_R2X3.md`.

## Why Layer B is blocked (the bench findings)

1. **The hardware is R2 (Ackermann)** — vendor-confirmed by the
   `Rosmaster_Lib` docstrings ("akerman type (R2) car"), but it shipped
   running the **cross-labeled X3 software image** reporting car-type 1.
2. **`set_car_type(5)` is a trade, not a fix**: under the current X3 image,
   type 5 makes `set_akm_steering_angle` physically steer — but the SAME
   `set_car_motion` that drove straight under type 1 then moves **one wheel**
   (the R2 drivetrain model isn't wired in this image). The flip is also
   RAM-volatile (reverts on reboot).
3. **Therefore the blocking dependency is external**: Yahboom's
   **Ultimate-Orin NX R2 image** (drive AND steering wired together).
   **Not yet obtained — emailing Yahboom is the critical path.** Only that
   image unblocks this layer via `set_car_motion` (Path A).

> **Path B (no vendor firmware) — proposal, in review.** The read-only
> investigation confirmed `set_car_motion` is a pure pass-through and the
> car-type kinematics are firmware-side; `set_motor` (no car-type byte) drives
> the motor channels directly, bypassing the broken mixer. A bench probe mapped
> the channels (`robot/motor_channel_probe_results.txt`: M1=rear-left, M4=rear-right,
> both `+`=forward; steering on the AKM path). The open-source Ackermann drive
> that uses this — rear wheels via `set_motor`, steering via
> `set_akm_steering_angle`, KIRRA-governed — is proposed in
> `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md` (calibration measurements pending
> before it can drive). Path B and Path A are alternatives; either unblocks R2
> drive+steer.

## What Layer B WILL contain (once unblocked)

Each item lands only after on-hardware validation on the R2 image:

- **Base**: the vendor R2 image flashed (Layer 0 of
  `docs/hardware/R2_GOLDEN_BUILD.md`); board defaults to car-type 5; drive
  AND steering verified together, wheels elevated.
- **Platform switch**: `KIRRA_EXPECTED_CAR_TYPE=5` in `/etc/kirra/robot.env`
  (the consumer's startup register assertion,
  `robot/kirra_motor_consumer.py:128-155`) and
  `installer/kirra_install.py verify --platform r2` passing (mode-5 check,
  `installer/platform_map.toml:18-39`).
- **Steering calibration measurements** (`robot/steering_bench_elevated.sh`,
  Track-A A1/A5 — wheels elevated, protractor + tape):
  - physical road-wheel angle at full lock (the future profile's
    `max_steering_deg` — the vendor command envelope is `[-45,+45]` command
    units about a 90 default, and command units ≠ road-wheel degrees until
    measured);
  - wheelbase confirmation (≈0.229 m measured on the bench — confirm on the
    R2 image before it becomes a profile value);
  - footprint (half-length / half-width), servo slew, `v_z` sign convention.
- **The `r2` contract profile** (Track-A A2): a NEW compiled class named `r2`
  (not `x3`, not the interim `courier`) in the contract-profiles registry,
  with wheelbase / steering / envelope values **from the measurements above —
  never invented**. `KIRRA_VEHICLE_CLASS=r2` then replaces the interim
  `courier` (`installer/platform_map.toml:29`).
- **The steering→`v_z` last-hop seam** (Track-A A4): the third
  `set_car_motion` argument means *mecanum yaw rate* under type 1 but drives
  the *steering servo model* under type 5 — the consumer's `(linear, angular)`
  semantics must be re-validated under the R2 firmware before any steering
  command flows.
- **The interceptor wheelbase closure**: `wheelbase_m` is already required
  config with a per-release cross-check against the verifier's contract value
  (`ros2_ws/src/kirra_safety/kirra_safety/cmd_vel_interceptor.py:52-89`; the
  old loose 0.2 default is gone) — but the deployment YAML still carries a
  legacy `0.2` (`ros2_ws/src/kirra_safety/config/kirra_params.yaml:6`). Layer
  B sets it to the `r2` profile's wheelbase so the parameter, the profile,
  and the physical robot agree (the A3 cross-check latches a stop on any
  mismatch, so a wrong value fails safe — but Layer B is where it becomes
  *correct*, derived from the profile, not a loose default).

## Definition of done for Layer B

1. Vendor R2 image flashed; drive + steering verified together (elevated).
2. `steering_bench_elevated.sh` measurements recorded
   (`steering_bench_results.txt` committed).
3. The `r2` contract profile merged (measurement-derived, human-reviewed).
4. `install_platform_r2.sh` scripted here (env switch to car-type 5 +
   `KIRRA_VEHICLE_CLASS=r2` + interceptor wheelbase), validated on hardware.
5. `first_run_elevated.sh` + `live_loop_elevated.sh` re-passed on the R2 base.

Until ALL five: this file is the honest state of steering. 🔴
