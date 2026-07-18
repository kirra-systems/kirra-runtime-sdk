#!/usr/bin/env python3
"""ptt_button.py — a GPIO push-to-talk button for the voice shell.

The R2's untethered voice UX needs a physical trigger to replace "press Enter"
in `speech_shell`'s interactive push-to-talk loop (crates/kirra-sidecars/src/bin/
speech_shell.rs: `loop { read_line(stdin) }` — ONE bounded recording turn per
line). This watcher emits exactly ONE newline on stdout per button press, so:

    python3 robot/ptt_button.py | speech_shell

makes a hardware button = the Enter key. Zero change to the Rust binary; the
button is just another external OS process, like the STT/TTS/record commands.

🔴 SAFETY — WHAT THIS BUTTON IS (and is NOT):
  * It is a MICROPHONE trigger. A press starts ONE bounded voice clip; the
    transcript enters the loop ONLY through the existing fail-closed intent door
    (MickIntent::parse_llm_json). A press with silence / noise / a misheard word
    → empty-or-garbage transcript → parse failure → NO intent latched → NO
    motion. The button cannot inject a goal or a command; it cannot bypass the
    checker. It is exactly as safe as pressing Enter.
  * It is NOT the e-stop. The e-stop is a SEPARATE hardware kill in the motor
    power line (docs/hardware/R2_UNTETHERED_BRINGUP.md §3). Do not wire this
    button as, or in place of, the e-stop. Losing this button = you can't talk
    to the robot; losing the e-stop = you can't stop it. They are different
    circuits with different criticality.

STDOUT DISCIPLINE: ONLY the trigger newline goes to stdout (it is speech_shell's
stdin). Every log/banner/error goes to STDERR — otherwise a log line would be
read as a turn and fire a spurious recording (harmless, but noisy).

Wiring (default): a normally-open momentary button between the configured pin and
GND. The internal pull-up idles the pin HIGH; a press pulls it LOW (falling
edge = press). No external resistor needed.

Env (all optional; the button pin is INPUT-only so a wrong value cannot drive
anything — but confirm the pin is a free GPIO on your header, not muxed):
  KIRRA_PTT_GPIO_PIN    button pin (default 18)         — see KIRRA_PTT_PIN_MODE
  KIRRA_PTT_PIN_MODE    BOARD | BCM  (default BOARD = physical pin numbers)
  KIRRA_PTT_ACTIVE      low | high   (default low: button→GND, internal pull-up)
  KIRRA_PTT_DEBOUNCE_MS debounce (default 200)
  KIRRA_PTT_LED_PIN     optional OUTPUT pin lit while pressed (recording feedback)

Requires Jetson.GPIO (`sudo pip3 install Jetson.GPIO`) + the running user in the
`gpio` group with the Jetson udev rules installed, or run as root.
"""
import os
import signal
import sys
import time


def log(msg):
    """Everything human-facing goes to STDERR — stdout is the trigger channel."""
    print(f"ptt_button: {msg}", file=sys.stderr, flush=True)


def env_int(key, default):
    raw = os.environ.get(key, "").strip()
    if not raw:
        return default
    try:
        return int(raw, 0)
    except ValueError:
        log(f"FATAL: {key}={raw!r} is not an integer")
        sys.exit(2)


def main():
    try:
        import Jetson.GPIO as GPIO
    except Exception as e:  # noqa: BLE001
        log(f"Jetson.GPIO unavailable ({e}). Install: sudo pip3 install Jetson.GPIO "
            "(+ gpio group / udev), or run as root.")
        sys.exit(2)

    pin = env_int("KIRRA_PTT_GPIO_PIN", 18)
    led_pin = env_int("KIRRA_PTT_LED_PIN", 0)  # 0 = no LED
    debounce_s = env_int("KIRRA_PTT_DEBOUNCE_MS", 200) / 1000.0
    mode = os.environ.get("KIRRA_PTT_PIN_MODE", "BOARD").strip().upper()
    active = os.environ.get("KIRRA_PTT_ACTIVE", "low").strip().lower()
    if mode not in ("BOARD", "BCM"):
        log(f"FATAL: KIRRA_PTT_PIN_MODE={mode!r} — expected BOARD or BCM")
        sys.exit(2)
    if active not in ("low", "high"):
        log(f"FATAL: KIRRA_PTT_ACTIVE={active!r} — expected low or high")
        sys.exit(2)

    # active-low (button→GND) → internal PULL-UP, pressed reads LOW.
    # active-high (button→3V3) → internal PULL-DOWN, pressed reads HIGH.
    GPIO.setmode(getattr(GPIO, mode))
    if active == "low":
        GPIO.setup(pin, GPIO.IN, pull_up_down=GPIO.PUD_UP)
        pressed_level = GPIO.LOW
    else:
        GPIO.setup(pin, GPIO.IN, pull_up_down=GPIO.PUD_DOWN)
        pressed_level = GPIO.HIGH
    if led_pin:
        GPIO.setup(led_pin, GPIO.OUT, initial=GPIO.LOW)

    def cleanup(*_):
        try:
            GPIO.cleanup()
        finally:
            log("stopped")
            sys.exit(0)

    signal.signal(signal.SIGINT, cleanup)
    signal.signal(signal.SIGTERM, cleanup)

    log(f"push-to-talk ready on {mode} pin {pin} (active {active}); "
        f"press = one voice clip. Pipe into speech_shell. Ctrl-C quits.")

    POLL_S = 0.02
    is_pressed = False
    try:
        while True:
            down = GPIO.input(pin) == pressed_level
            if down and not is_pressed:
                # debounce: confirm still held after the settle window
                time.sleep(debounce_s)
                if GPIO.input(pin) == pressed_level:
                    is_pressed = True
                    if led_pin:
                        GPIO.output(led_pin, GPIO.HIGH)
                    # THE TRIGGER — the only thing that ever touches stdout.
                    sys.stdout.write("\n")
                    sys.stdout.flush()
                    log("press -> record one clip")
            elif not down and is_pressed:
                is_pressed = False
                if led_pin:
                    GPIO.output(led_pin, GPIO.LOW)
            time.sleep(POLL_S)
    finally:
        cleanup()


if __name__ == "__main__":
    main()
