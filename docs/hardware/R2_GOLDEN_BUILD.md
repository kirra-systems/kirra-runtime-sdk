# R2 Golden Build — the correct-platform image, detect-verified install

The target: a Rosmaster **R2 (Ackermann)** unit running the **correct vendor
base image** (drive AND steering both correct), with KIRRA and all dependencies
layered on top, installed by `kirra-install` — which **detects** the hardware,
**verifies** it against the declared platform, and **selects** the validated
configuration. Detection emits names; every safety number comes from a reviewed
profile. (Provenance: `docs/hardware/HARDWARE_FINDINGS_R2X3.md`,
`installer/platform_map.toml`, the installer gap analysis.)

## Layer stack

| # | Layer | Source | Status |
|---|---|---|---|
| 0 | **Yahboom R2 base image** (R2 drivetrain model wired: drive + steering both work; board defaults to car-type 5) | **VENDOR ARTIFACT — operator-acquired/flashed.** Cannot be synthesized from this repo; the current X3 image is cross-labeled and breaks drive under a type-5 flip | ⚠ the blocking item |
| 1 | JetPack/L4T (Jetson Orin) | vendor image / NVIDIA | present on current unit |
| 2 | ROS 2 Humble, `ROS_DOMAIN_ID` | base image | present |
| 3 | Lidar driver: `ydlidar_ros2_driver` @ 512000 on `/dev/ydlidar` (TG30, BEST_EFFORT `/scan`) | mapping entry `platform.r2.lidar` | driver present on current unit |
| 4 | Cameras | detect-report-only (no camera driver config exists in-repo yet — honest gap) | deferred |
| 5 | KIRRA: verifier + sidecars (`deploy/systemd/install.sh`), consumer FFI (`kirra-consumer-ffi`), `robot/` scripts, `ros2_ws/kirra_safety` | this repo, `scripts/build_release.sh` | built |
| 6 | Platform config: `KIRRA_VEHICLE_CLASS` (interim `courier` until the measurement-gated `r2` profile lands), `KIRRA_EXPECTED_CAR_TYPE=5`, motor/lidar ports | `kirra-install install --platform r2` | built |
| 7 | Keys: governor signing key (`file:` source), consumer pin (`kirra-consumer-ctl enroll`), node attestation (`kirra-ota-ctl enroll`) | enrollment ceremony — **never the dev seed** on a golden unit | tooling exists; ceremony is operator-run |
| 8 | Acceptance: `ffi_smoke_test.py` + `teardown_smoke_test.py` (host), then the ELEVATED suites (`first_run_elevated.sh`, `steering_bench_elevated.sh`) | this repo | built |

## The install flow

```bash
# On the flashed R2 base image, repo present, binaries built:
python3 installer/kirra_install.py detect              # what the hardware says
python3 installer/kirra_install.py verify  --platform r2   # fail-closed gate
python3 installer/kirra_install.py install --platform r2 --dry-run   # review the plan
sudo python3 installer/kirra_install.py install --platform r2        # execute
```

`verify` refuses (with remediation) when: the board reports the wrong car-type
(e.g. 1 = the cross-labeled X3 image — remediation: flash the vendor R2 image;
a `set_car_type(5)` flip alone breaks the drive path and is RAM-volatile), the
motor/lidar devices are absent, or the platform name is unknown (no
nearest-match). The consumer independently re-asserts the car type at every
startup (`KIRRA_EXPECTED_CAR_TYPE`) — the installer check and the runtime check
are belt and braces over the same hazard.

## Select-not-infer (the product's safety boundary)

- Detection emits **names** (`r2`, `ydlidar-tg30`, device paths).
- `platform_map.toml` carries the reviewed config those names select.
- Safety numbers live in the **compiled contract profiles** chosen by
  `profile_class` — the installer writes the class *name* only. It deliberately
  does NOT write envelope/wheelbase/decel values; those are deployment-review
  inputs (and the r2 profile itself is measurement-gated, Track-A A2).

## Deferred (tracked, not forgotten)

- **The r2 contract profile** (A2) + last-hop steering units (A4): gated on the
  vendor R2 image + `steering_bench_elevated.sh` measurements.
- **Signed profile/mapping artifacts** (plan B1/B3): the Uptane targets
  mechanism exists (third reuse); today the mapping is repo-reviewed content.
- **Cameras**: enumeration only until a camera driver config exists.
- **Per-unit key ceremony automation** (plan B4/item 12).
