# Developer guide

## Build and test

Host:

```bash
cmake -S firmware/rosmaster-r2 -B /tmp/r2-build \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo -DR2_ENABLE_SANITIZERS=ON
cmake --build /tmp/r2-build --parallel
ctest --test-dir /tmp/r2-build --output-on-failure
/tmp/r2-build/r2_deterministic_sim
```

Generate API documentation after installing Doxygen:

```bash
cd firmware/rosmaster-r2
doxygen Doxyfile
```

Target core cross-build (STM32F103RCT6, Cortex-M3) — proves the portable
`r2_platform_core` (kinematics, control loop, protocol, safety manager,
configuration) carries no host dependency:

```bash
cmake -S firmware/rosmaster-r2 -B /tmp/r2-target \
  -DCMAKE_TOOLCHAIN_FILE="$PWD/firmware/rosmaster-r2/cmake/arm-none-eabi-cortex-m3.cmake"
cmake --build /tmp/r2-target --parallel   # builds libr2_platform_core.a (armv7-m)
```

Cross-compiling disables the host tests, simulation and fuzzer automatically
(they are host-only). This is a build-only lane — a static library needs no
linker script. The linker script / flash map, startup + vector table, the
concrete HAL drivers (#967) and the flashable application image (#968) are the
remaining follow-ups after the BSP closure gates in `drivers/README.md`; the
host target is not flashable.

## Coding rules

- C++17 freestanding subset; fixed-width types and explicit units in names.
- No heap after startup, exceptions, RTTI, recursion, hidden blocking or
  unbounded loops.
- No raw hardware access outside BSP/drivers.
- No parser, flash, text formatting or transport operation in the 1 kHz loop.
- Validate finite/range/schema/length before use.
- Make safety states and error results explicit; no “best effort” actuation.
- Use monotonic injected time; never wall clock for a deadline.
- Hardware outputs initialize disabled and fail disabled.
- Review against MISRA C++:2023; deviations need rationale, scope and test.

`clang-tidy` and `cppcheck` are advisory until tool versions and rule profiles
are pinned. Compiler warnings and tests are blocking now. Production adds
qualification/version evidence rather than claiming that a tool invocation
alone establishes MISRA compliance.

## Test strategy

- Unit: kinematics, limiters, PID, framing, CRC, sequence, safety, configuration.
- Property/fuzz: arbitrary protocol bytes, finite control domains, flash
  interruption points.
- Software integration: HAL mocks, 1 kHz deterministic plant, restart/replay.
- HIL: timer/DMA rates, UART BER/latency, sensors, watchdog, brownout, E-stop.
- Vehicle: elevated first; then tethered low-speed straight/arc/stop tests.
- Production: 24-hour zero-deadline-miss soak and temperature/voltage corners.

Every bug fix adds a reproducer at the lowest useful layer and an integration
test when the failure crossed layers.

## API map

| Namespace | Stable responsibility |
|---|---|
| `r2::hal` | replaceable hardware contracts and evidence manifest |
| `r2::protocol` | canonical frame, CRC32C, COBS and sequence acceptance |
| `r2::kinematics` | Ackermann forward/inverse transformations |
| `r2::control` | jerk limiter, wheel PID and motion composition |
| `r2::safety` | safety state and fault authority |
| `r2::diagnostics` | fixed-memory runtime metrics |
| `r2::application` | versioned transactional configuration |
| `r2::boot` | authenticated-image verification seam |

Public headers are the API reference source; Doxygen extracts them. Dependencies
must continue pointing down the architecture in `ARCHITECTURE.md`.

## Change control

Changes to pin maps, wire format, state transitions, hard limits, flash layout,
watchdog timing or boot verification require a design review, updated safety
impact, tests and target evidence. Protocol major versions are never silently
compatible. Calibration changes create a new generation and preserve rollback.
