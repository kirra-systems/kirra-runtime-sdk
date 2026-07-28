#!/usr/bin/env python3
"""consumer_config_contract.py — ONE definition of what the motor consumer
requires, so the doctor, the preflight, the installer and the unit cannot drift.

Two live bring-up failures motivated this:

1. **A mode-blind warning.** The config doctor warned
   `KIRRA_EXPECTED_CAR_TYPE set: unset` while the robot ran
   `KIRRA_DRIVE_MODE=r2_ackermann` — a mode in which the consumer never reads
   that variable. The operator was told to set something that does nothing.

2. **A lint-clean but useless env.** The voice setup copied a Rabbit-focused
   file into `/etc/kirra/robot.env`. It passed the syntax linter and was
   missing every actuation key, so the consumer discovered them ONE AT A TIME
   through a restart loop: freshness window, then control period, then missed
   periods, then stop decel, then the caps, then the port, then the drive mode,
   then the FFI. Over 200 restarts to learn eight facts.

Lint answers "is this a valid shell file". This answers "can the consumer
actually start", which is a different question and the one that matters before
the unit is enabled. `missing_keys` returns the WHOLE set in one pass.

Pure: every function takes an env mapping and returns data. No file I/O, no
serial, no systemd — so the whole contract is host-testable.
"""
from __future__ import annotations

MODE_R2 = "r2_ackermann"
MODE_X3 = "x3"
MODE_R2CP = "r2cp"

# Required in EVERY mode — the governed-consumer safety envelope.
ALWAYS_REQUIRED = (
    "KIRRA_GOVERNOR_VK_HEX",      # the release-token verifying key
    "KIRRA_PROFILE_DIGEST",       # binds the consumer to a checker profile
    "KIRRA_FRESHNESS_WINDOW_MS",  # stale release → refuse
    "KIRRA_CONTROL_PERIOD_MS",
    "KIRRA_MISSED_PERIODS",       # → safe stop
    "KIRRA_STOP_DECEL_MPS2",
    "KIRRA_DEMO_VX_MAX",
    "KIRRA_DEMO_VZ_MAX",
    "KIRRA_MOTOR_PORT",
)

# X3 only. The consumer reads this to verify the base reports the car type it
# was calibrated for; the R2 Ackermann path has no such handshake.
X3_ONLY = ("KIRRA_EXPECTED_CAR_TYPE",)

# R2 Ackermann: the measured calibration `calibration_from_env` consumes. These
# are per-robot MEASUREMENTS — a default would be a guess at the steering
# geometry of a machine that can move, so they are required, not defaulted.
R2_REQUIRED = (
    "KIRRA_R2_WHEELBASE_M",
    "KIRRA_R2_V_PER_PWM",
    "KIRRA_R2_PWM_MAX",
    "KIRRA_R2_STEER_UNITS_PER_RAD",
    "KIRRA_R2_DELTA_MAX_RAD",
    "KIRRA_R2_STEER_SIGN",
    "KIRRA_R2_CENTER_TRIM",
)

# R2CP: the host speaks GOVERNED PHYSICAL UNITS to an MCU that owns its own
# calibration, so none of the R2 Ackermann measurements above apply here. PWM
# scaling, steering geometry, drive limits and watchdog policy belong in the
# MCU's manifest/firmware, not duplicated on the Jetson — duplicating them is
# how the two ends drift, and the MCU is the one attached to the motors.
#
# What the host genuinely needs is the link itself.
R2CP_REQUIRED = (
    "KIRRA_MOTOR_PORT",  # also in ALWAYS_REQUIRED; named here for the reader
    "KIRRA_R2CP_HANDSHAKE_TIMEOUT_MS",
    "KIRRA_R2CP_COMMAND_TIMEOUT_MS",
)

# Deliberately NOT required yet: an expected MCU identity. The protocol does
# not define stable identity semantics, and inventing one here would create a
# host-only convention the firmware never agreed to. Revisit when it does.

# Only when the corresponding feature is switched on.
ODOM_REQUIRED = ("KIRRA_R2_M_PER_TICK",)
CLOSED_LOOP_REQUIRED = ("KIRRA_R2_V_PER_PWM_RIGHT",)

# Needed to load the verify core at all; its absence was one of the restart-loop
# discoveries and is a deterministic, not transient, failure.
FFI_KEY = "KIRRA_CONSUMER_LIB"


def _truthy(v) -> bool:
    return (v or "").strip().lower() in ("1", "true", "yes", "on")


def drive_mode(env) -> str:
    """The configured mode. Unset means the consumer's own default path; report
    it verbatim rather than guessing, so a typo is visible as a typo."""
    return (env.get("KIRRA_DRIVE_MODE") or "").strip() or MODE_X3


def odom_enabled(env) -> bool:
    return _truthy(env.get("KIRRA_R2_ODOM_ENABLED"))


def closed_loop_enabled(env) -> bool:
    return _truthy(env.get("KIRRA_R2_CLOSED_LOOP"))


def required_keys(env) -> list[str]:
    """Every key this env's MODE actually needs, in report order."""
    mode = drive_mode(env)
    keys = list(ALWAYS_REQUIRED) + [FFI_KEY]
    if mode == MODE_R2CP:
        # Only the link settings on top of the always-required envelope. No
        # vendor car-type handshake and no measured calibration: the MCU holds
        # its own.
        keys += [k for k in R2CP_REQUIRED if k not in ALWAYS_REQUIRED]
    elif mode == MODE_R2:
        keys += list(R2_REQUIRED)
        if odom_enabled(env):
            keys += list(ODOM_REQUIRED)
        if closed_loop_enabled(env):
            keys += list(CLOSED_LOOP_REQUIRED)
    else:
        keys += list(X3_ONLY)
    return keys


def inapplicable_keys(env) -> list[str]:
    """Keys that belong to a DIFFERENT mode than the configured one. Reported
    as a warning, never a failure — a leftover value is untidy, not unsafe."""
    mode = drive_mode(env)
    if mode == MODE_R2CP:
        # Everything physical belongs to the MCU in this mode.
        return (
            list(X3_ONLY)
            + list(R2_REQUIRED)
            + list(ODOM_REQUIRED)
            + list(CLOSED_LOOP_REQUIRED)
            + ["KIRRA_R2_DRIVE_DEADBAND_PWM"]
        )
    if mode == MODE_R2:
        out = list(X3_ONLY)
        if not odom_enabled(env):
            out += list(ODOM_REQUIRED)
        if not closed_loop_enabled(env):
            out += list(CLOSED_LOOP_REQUIRED)
        return out
    return list(R2_REQUIRED) + list(ODOM_REQUIRED) + list(CLOSED_LOOP_REQUIRED)


def missing_keys(env) -> list[str]:
    """THE whole missing set, in one pass — the point of this module. The
    consumer would surface these one restart at a time."""
    return [k for k in required_keys(env) if not (env.get(k) or "").strip()]


def sufficient_for_actuation(env) -> bool:
    return not missing_keys(env)


def report(env) -> dict:
    """A migration/preflight report: what is missing, what is inapplicable but
    present, and what mode the answer is relative to."""
    mode = drive_mode(env)
    return {
        "mode": mode,
        "missing": missing_keys(env),
        "present_but_inapplicable": [k for k in inapplicable_keys(env)
                                     if (env.get(k) or "").strip()],
        "required_count": len(required_keys(env)),
        "sufficient": sufficient_for_actuation(env),
    }
