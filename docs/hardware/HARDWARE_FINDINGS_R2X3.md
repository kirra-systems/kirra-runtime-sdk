# Hardware Findings — Rosmaster R2 vs X3 (the steering investigation)

Bench-verified findings from the 2026-07 steering investigation. This is the
provenance record for `installer/platform_map.toml` and the consumer's
`KIRRA_EXPECTED_CAR_TYPE` assertion — the first real entries in the installer's
hardware→config knowledge base. Vendor-authoritative sources throughout.

## The platform

- The robot is **vendor-confirmed R2 (Ackermann) hardware**: the `Rosmaster_Lib`
  docstrings for `set_akm_default_angle` and `set_akm_steering_angle` both say
  *"akerman type (R2) car"*.
- It shipped running the **cross-labeled X3 software image** (`ROBOT_TYPE=x3`),
  a known Yahboom R2/X3 labeling wrinkle.

## The car-type register (the load-bearing finding)

`get_car_type_from_machine()` reads the board's **configured drive model** —
NOT immutable chassis identity. Constants read from a live library instance:

| Value | Constant | Drive model |
|---|---|---|
| 1 | `CARTYPE_X3` | mecanum |
| 2 | `CARTYPE_X3_PLUS` | mecanum |
| 4 | `CARTYPE_X1` | — |
| 5 | `CARTYPE_R2` | Ackermann |

Bench-verified behavior:

- **Type 1 (as shipped)**: `set_car_motion` drive works (mecanum mixing; the
  first-run governed test passed on it). `set_akm_steering_angle` is **ignored**
  (no steering servo in the mecanum model) — steering silently inert.
- **Type 5 (after `set_car_type(5)`)**: steering works — `set_akm_steering_angle(-45)`
  / `(+45)` physically turned the front wheels. **But drive breaks**: the same
  `set_car_motion(0.15, 0, 0)` that drove straight under type 1 moved **one
  wheel** — R2 mode uses a different drivetrain model the X3 image doesn't wire.
- **The flip is RAM-volatile**: reverts to type 1 on reboot.
- **Conclusion:** `set_car_type(5)` is a mode flip that trades a working drive
  path for working steering — not a fix. Drive + steering both correct requires
  **Yahboom's proper R2 software image**. The board was reverted to type 1
  (known-good drive) for the straight-line demo path.

## Vendor steering command envelope (for the future r2 contract profile)

- `set_akm_steering_angle(angle)`: **`angle ∈ [-45, +45]`** command units
  (negative = left), relative to a default of 90.
- `set_akm_default_angle`: `[60, 120]` (center 90).
- ⚠ Command units ≠ road-wheel degrees until measured: the physical wheel angle
  at full lock (the profile's `max_steering_deg`) still requires the bench
  protractor measurement (`robot/steering_bench_elevated.sh` §A1.2) on the R2
  image.

## Safety consequences wired into the code

1. **`KIRRA_EXPECTED_CAR_TYPE`** (required, no default) — the consumer reads the
   register at startup and **refuses to start on mismatch/unreadable**
   (`robot/kirra_motor_consumer.py`): a consumer validated against one drive
   model must never command a board configured for another. The expected value
   comes from the platform mapping, never guessed.
2. **`installer/platform_map.toml`** keys each platform on `required_car_type`
   and the installer's `verify` refuses on mismatch, naming the exact
   remediation (flash the vendor R2 image; a flip alone breaks drive).
3. **Steering + the r2 contract profile are deferred** behind the vendor R2
   image + the bench measurements (Track-A A2/A4). The demo path is
   straight-line under type 1, where the third `set_car_motion` argument is the
   mecanum yaw rate and the consumer's `(linear, angular)` semantics are
   correct as-is.
