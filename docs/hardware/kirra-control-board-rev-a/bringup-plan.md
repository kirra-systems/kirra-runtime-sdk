# Rev A bring-up plan (wheels-up, staged)

> **This documentation does not authorize floor motion.** Completing every
> stage below still does not put the robot on the floor: floor testing has
> its own separate approvals (§15). Stages are ordered; a stage starts only
> when the previous one passed with recorded results. Wheels up, motor
> power isolated, current-limited bench supply — the same discipline as
> `firmware/rosmaster-r2/docs/MCU_BRINGUP_DRILL.md`.

## Stages

1. **Visual inspection and continuity.** Bare/assembled board inspection,
   photographed; continuity on power nets, ground plane, and all
   safety-signal paths; short checks. No power applied.
2. **Logic-power verification with the Nucleo removed.** Apply protected
   logic power from a current-limited supply; verify protection behavior
   (fuse path, reverse-polarity, TVS presence), rails, and that
   `DRIVER_ENABLE_HW` reads disabled with no MCU present (`safety.md` §4
   "Nucleo removed" row).
3. **Install the Nucleo and verify rails.** Confirm the MB1367
   power-configuration (as frozen at DR-2) against measured rails; confirm
   consumption against the fuse rating.
4. **SWD programming and reset.** Probe connect, device ID check, flash a
   minimal image, exercise `NRST`; confirm reset never glitches the enable
   path (scope on `DRIVER_ENABLE_HW`).
5. **UART/R2CP handshake with no actuator connection.** Bring up the
   Jetson link; verify voltage domains at the test points match the DR-2
   measurements; run R2CP traffic (the host codec and, where useful, the
   PTY-proven bridge) against the board. No actuator wiring attached.
6. **E-stop hardware truth-table test.** Exercise every combination of
   loop intact/open against `E_STOP_OK_HW`, `E_STOP_SENSE`, and
   `DRIVER_ENABLE_HW`; verify the full `safety.md` conjunction row by row,
   including wire-cut behavior.
7. **Watchdog timeout test.** Healthy kick → permission present; stop the
   kick (halt firmware via debugger) → `WATCHDOG_OK_HW` drops within the
   DR-2 window; measure the window. Verify a crashed/held MCU cannot keep
   the enable active.
8. **Driver-enable output test into a dummy load or logic analyzer.**
   Verify `DRIVER_ENABLE_HW` electrical behavior (levels, edges,
   power-up/power-down glitch-freedom) with no real driver attached.
9. **Encoder-input simulation.** Signal generator / encoder simulator into
   `ENCODER_*` at and beyond the expected edge rates (once measured);
   verify counts, filter behavior, and A/B matching.
10. **Steering-output test without mechanical load.** `STEERING_PWM` into
    a scope/analyzer; verify pulse behavior across the commanded range;
    exercise the optional `STEERING_FB` input if fitted.
11. **External driver connection with motor power isolated.** Connect the
    retained drivers' logic/control/fault wiring only — motor supply
    disconnected. Characterize enable polarity, fault outputs, and input
    thresholds against the DR-2 assumptions; correct the record where they
    differ.
12. **Wheels-up motor test.** Motor power connected, robot elevated,
    E-stop chain live. First powered rotation under the full gate; verify
    disable paths (E-stop, watchdog, request-drop) stop rotation.
13. **Fault-injection testing.** The `safety.md` §4 matrix executed for
    real: resets mid-motion (wheels up), communication cut, Jetson
    restart, Nucleo brown-out, stuck-kick and no-kick cases, driver-fault
    assertion. Every row must converge to disabled actuation; record
    measured latencies.
14. **HIL acceptance.** The board joins the firmware tree's
    hardware-in-loop stage (roadmap staged-migration stage 2): sustained
    R2CP under load, BER/latency measurement on the real link, watchdog
    expiry under load, and the on-target timing evidence the firmware
    phases require. Host timing remains indicative only.
15. **Floor testing — separate approvals required.** Floor motion happens
    only after distinct, recorded approvals for (a) odometry and (b)
    closed-loop wheel control — neither is granted by this plan — plus the
    untethered prerequisites already established
    (`../R2_UNTETHERED_BRINGUP.md`, `../R2_ESTOP_SPEC.md`). Rev A bring-up
    ends at HIL acceptance.

## Recording

Each stage produces a dated entry (who, board serial, firmware/image hash,
instruments, results, deviations) in the bring-up log, mirroring the
firmware tree's immutable bring-up-report practice. A failed stage is
recorded, not erased.
