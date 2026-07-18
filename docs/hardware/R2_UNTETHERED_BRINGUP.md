# R2 untethered bring-up — driving off the network, voice-directed

> **Status: architecture + procedure.** The load-bearing claim of this doc is
> that **your home wifi is not in the robot's control loop** — everything that
> drives the R2 runs on the Jetson Orin as localhost services. "Untethered" is
> therefore not a re-architecture; it is replacing the three jobs your SSH
> session currently stands in for. Each layer below cites the real code; where a
> piece is not yet hardware-validated it says so.

## 0. Why this is not a re-architecture

Cut the wifi mid-drive and the robot does not notice. The whole governed loop is
on-box:

```
  voice / goal ─► mick_service (LLM: Ollama gemma3:4b, LOCAL) ─► /intent/last ┐
  lidar /scan ─► taj_service (corridor) ─────────────────────┼─► planner_service
                                                              │    (Occy plan, KIRRA
                                                              ▼     slow-loop checker)
                                                         /cmd_vel_raw
                                                              │  cmd_vel_interceptor
                                                              ▼  → verifier /actuator…
                                                    verifier MINTS signed release
                                                              │  (relay, not mint)
                                                              ▼
                                          kirra_motor_consumer (FFI Ed25519 verify,
                                          ADR-0033 chokepoint) ─► wheels
```

Every box is a **localhost** process on the Orin — Ollama included. Wifi/SSH give
you exactly two things, **neither in the control path**: a *terminal* (start /
stop / observe) and *dev convenience* (`git pull`, editing). Going off-network
means replacing those two jobs with three onboard capabilities:

| Layer | Replaces (today) | Section |
|---|---|---|
| **1. Autostart** | "you type the launch commands over SSH" | §1 |
| **2. Voice goal source** | "you `curl POST /intent` / publish `/goal_pose`" | §2 |
| **3. Hardware e-stop** | "you press Ctrl-C over SSH" | §3 |

Comms (own hotspot / RF / Zenoh) is **§4 — optional, for you to monitor, not for
the robot to drive.**

---

## 1. Autostart — the stack comes up on power-on

The mechanism exists: `robot/install/install_kirra.sh` step 6 **stages** a
`/etc/systemd/system/kirra-consumer.service` unit (rendered with the robot user),
reading `/etc/kirra/robot.env` via `EnvironmentFile`. It is deliberately **NOT
enabled** — the install script warns that consumer-as-a-service has not been
hardware-validated (the validated path is a terminal run).

**To go headless you enable, per service, AFTER an elevated re-test of each:**

```bash
# after validating each as a service, wheels-up, one at a time:
sudo systemctl enable --now kirra-consumer       # the verifying motor consumer
# The full unit set now EXISTS: kirra.target (verifier + planner + taj + mick,
# deploy/systemd/), kirra-consumer (robot/install/), and the ROS doer stack +
# KITT watcher (robot/install/systemd/kirra-ros-stack.service /
# kirra-kitt-watch.service). The ordered validate-then-enable procedure + the
# cold-boot acceptance test are in docs/hardware/R2_AUTOSTART_CHECKLIST.md.
# What remains is per-service wheels-up validation, not authoring.
```

🔴 **Gate:** each service must pass its wheels-up acceptance *as a service*
before `enable`. A boot-time stack that was only ever validated by hand is an
unproven configuration. Bring them up in dependency order (verifier + Ollama →
sidecars/launch → consumer → voice shell); a consumer that starts before the
verifier simply starves into decel-to-stop (fail-safe), so ordering is a
smoothness concern, not a safety one.

**Boot-order safety note:** none of this can fail *open*. If the verifier isn't
up when the consumer starts, no releases are minted → the consumer holds its
minimal-risk stop (the `503 → 0.0` floor, SS-002). Power-on with a missing
service is a stationary robot, never a runaway.

---

## 2. Voice goal source — you speak, KIRRA still disposes

This is **already built and CI-covered** — `speech_shell`
(`crates/kirra-sidecars/src/bin/speech_shell.rs` + `speech.rs`), documented in
`docs/testing/SPEECH_KITT_DEMO.md`. You are wiring hardware to an existing seam,
not adding a code path.

```
audio in → STT (whisper.cpp, external OS process) → text
         → POST /intent   (the SAME fail-closed door typed text uses:
                            MickIntent::parse_llm_json)
         → …the governed loop, unchanged…
verdict + #893 narration → TTS (Piper, external OS process) → audio out ("the car says why")
```

### The safety guarantee that makes voice legal
`speech.rs` imports **nothing** from `kirra_planner` or the checker. Its only
output toward the loop is a `String` handed to the intent door. So a misheard
command, a garbled transcript, or ambient noise parsed as words **dead-ends
exactly where typed garbage does** — `MickIntent::parse_llm_json` returns a parse
failure → no intent latched → no motion. There is no audio→goal path that skips
that parser, and the Mick actuation fence (`ci/check_mick_actuation_fence.py`)
covers the speech module too. Voice is as safe as typing, by construction.

### Push-to-talk, never open-mic
Each turn records ONE bounded clip (`KIRRA_RECORD_CMD`, e.g. `arecord -d 4 …`)
after you trigger it — no wake word, no always-on microphone. On an untethered
robot the trigger is a **physical button** (a GPIO push-to-talk), not the Enter
key.

### Your hardware (mic + speaker)
- **USB mic + USB/3.5mm speaker** into the Orin. Confirm ALSA sees them:
  `arecord -l` (capture) / `aplay -l` (playback); set the default device or pass
  the card in the record/play commands.
- **STT engine** — build whisper.cpp `whisper-cli` + fetch a model
  (`ggml-base.en.bin` is a good Orin starting point). Fully offline.
- **TTS engine** — Piper + a voice (`en_US-lessac-medium.onnx`), wrapped in a
  one-line `speak.sh` (`piper … --output-raw | aplay -r 22050 -f S16_LE -t raw -`).
  Fully offline. Unset `KIRRA_TTS_CMD` → narration prints instead of speaks.

### Env (fail-closed on malformed — see SPEECH_KITT_DEMO.md §setup)
```bash
export KIRRA_STT_CMD="whisper-cli -m models/ggml-base.en.bin -np -nt -f"  # required
export KIRRA_TTS_CMD="./speak.sh"          # optional; unset → print-only
export KIRRA_RECORD_CMD="arecord -d 4 -f S16_LE -r 16000 -c 1"  # push-to-talk
# KIRRA_MICK_URL default http://127.0.0.1:8102
```

Untethered, `speech_shell` becomes a boot service (§1) driven by the PTT button —
the operator interface with zero network dependency. Everything it needs (whisper,
Piper, Ollama) is on the box.

### The GPIO push-to-talk button (`robot/ptt_button.py`)

`speech_shell`'s interactive loop fires ONE bounded recording turn per stdin line
("press Enter"). `robot/ptt_button.py` is a GPIO watcher that emits exactly one
newline per button press, so a hardware button **becomes** the Enter key —
**no change to the Rust binary**; the button is just another external OS process,
like the STT/TTS/record commands.

```
  [ momentary button ]
     pin ●───────────┐         one command:
                     │           ./robot/run_voice_ptt.sh
     GND ●───────────┘         (= python3 robot/ptt_button.py | speech_shell)
   internal pull-up idles HIGH; press pulls LOW (falling edge = press)
```

**Wiring:** a normally-open momentary button between a free GPIO pin and GND. The
script enables the internal pull-up — **no external resistor**. Confirm the pin is
a real, unmuxed GPIO on your 40-pin header first.

**Env** (all optional; the button pin is INPUT-only, so a wrong value cannot drive
anything):

| var | default | meaning |
|---|---|---|
| `KIRRA_PTT_GPIO_PIN` | `18` | button pin |
| `KIRRA_PTT_PIN_MODE` | `BOARD` | `BOARD` (physical pin #) or `BCM` |
| `KIRRA_PTT_ACTIVE` | `low` | `low` = button→GND (pull-up); `high` = button→3V3 (pull-down) |
| `KIRRA_PTT_DEBOUNCE_MS` | `200` | press debounce |
| `KIRRA_PTT_LED_PIN` | — | optional OUTPUT pin lit while pressed (recording feedback) |

**Deps:** `sudo pip3 install Jetson.GPIO` + the running user in the `gpio` group
with the Jetson udev rules (or run as root). The script fails closed with an
install hint if `Jetson.GPIO` is unavailable.

🔴 **The PTT button is a MIC trigger, NOT the e-stop.** A press starts one voice
clip; a misheard/silent press dead-ends in `MickIntent::parse_llm_json` → no
intent → no motion. It cannot inject a goal or bypass the checker — as safe as
pressing Enter. The **e-stop is a separate hardware kill** in the motor-power line
(§3); do not conflate the two circuits (losing the button = can't talk; losing
the e-stop = can't stop — different criticality).

> **Remaining for a robot-grade voice UX (beyond this button):** audio-feedback
> tones (a "listening" chirp on press, distinct from the spoken narration), and
> mic placement/gain for a moving platform. A **hold-to-talk** variant (record
> only while held, vs. today's press → one bounded `-d 4` clip) is a follow-up —
> it needs the recorder driven by button *state* rather than a fixed duration.

---

## 3. Hardware e-stop — the real prerequisite (do this FIRST)

Today your e-stop is **Ctrl-C over SSH**. That vanishes the moment you leave the
network. **Do not drive the R2 untethered until it has a physical/RF hardware
e-stop** — a switch that removes motor power (or forces the consumer's
decel-to-stop) *independently of software*.

- The **software half exists**: the operator-console e-stop request (ADR-0013),
  the LockoutReason path, and the always-available MRC decel-to-stop (SS-002).
- The **physical half is a hardware addition you must make**: a normally-closed
  kill switch in the motor-power line and/or an RF kill fob. Software e-stops are
  necessary but not sufficient for an untethered vehicle — a wedged process or a
  dead SBC must still be stoppable by a human reaching the robot or a fob.

This is the one item where "it worked on wifi" does **not** carry over — plan the
e-stop before the first untethered meter. **Concrete design + BOM + test
procedure: `docs/hardware/R2_ESTOP_SPEC.md`** (an energize-to-run safety relay in
the motor feed, fail-safe on any fault, latching, with a two-part re-arm that
reuses the ADR-0013 clearance grant so there's no post-reset lurch).

---

## 4. Comms (optional — monitoring, not control)

Because driving needs no link, comms is purely so *you* can observe/command
remotely. Ordered by how quickly you can stand each up:

| Option | What it buys | Notes |
|---|---|---|
| **Robot's own Wi-Fi AP / your phone hotspot** | SSH + terminal anywhere near the robot | Simplest interim; "carry the network with you." No infra. |
| **Low-bandwidth RF** (LoRa / telemetry radio) | e-stop + status when you don't need video | Pairs well with the §3 RF kill fob. |
| **Cellular (LTE/5G modem)** | true remote monitoring, anywhere with coverage | For telemetry/observability, still not the control path. |
| **Zenoh fleet transport** (`kirra-fleet-transport`, ADR-0007) — **the end goal** | node reports trust/posture to a governor; multi-robot fleet | Untrusted carrier: Ed25519 verify-before-use on every ingest + rate-limit; **fails closed locally when the link drops**. This is the fleet story, not a single-robot requirement. |

**Interim recommendation:** stand up the robot's own AP (or a phone hotspot) so
you keep your SSH/observability while the voice + autostart + e-stop layers mature;
move to Zenoh when you go multi-robot. Neither is on the driving path, so you can
swap them freely without touching the safety loop.

---

## 5. The safety punchline

- **Loss of network is safe by design, not a hazard.** If releases stop flowing
  the consumer decel-to-stops (`503 → 0.0`, SS-002). Nothing about going
  off-wifi can make the governor fail *open* — the verify chokepoint (ADR-0033)
  and the checker are local and fail-closed.
- **The single thing that genuinely changes off-tether is your soft e-stop** —
  which is why §3 (a hardware e-stop) is the real prerequisite, ahead of any
  comms or autostart work.
- Voice, autostart, and comms are all **operator-interface** changes; the
  doer→checker→verifier→consumer safety spine is byte-identical whether you're on
  wifi, on a hotspot, on cellular, or on nothing.

---

## Bring-up order (recommended)

1. **Hardware e-stop** (§3) — before the first untethered meter. Test it: drive a
   little (tethered/on wifi), hit the physical stop, confirm wheels cut.
2. **Voice on wifi** (§2) — wire the mic/speaker, run `speech_shell` by hand per
   `SPEECH_KITT_DEMO.md`, prove "creep forward" → intent → bounded proposal (the
   Stage-1 loop you already validated), with narration spoken back.
3. **Autostart, one service at a time** (§1) — validate each as a service
   wheels-up, then `enable`.
4. **Cut the cord** — power-on cold, no laptop: stack autostarts, PTT button
   drives voice, hardware e-stop in hand. Own-AP/hotspot for observability.
5. **Fleet (later)** — Zenoh transport when you go multi-robot.

## References
- `docs/hardware/R2_LIVE_LOOP_BRINGUP.md` — the tethered governed loop (Stages 1–2)
- `docs/testing/SPEECH_KITT_DEMO.md` — the voice UX (whisper.cpp / Piper, env, CI)
- `robot/install/install_kirra.sh` — installer + the staged `kirra-consumer.service`
- `ci/check_mick_actuation_fence.py` — why the LLM (and voice) can't drive directly
- ADR-0033 — the on-device verify chokepoint (local, fail-closed)
- ADR-0007 / `crates/kirra-fleet-transport` — the Zenoh fleet carrier (verify-before-use)
- ADR-0013 / SS-002 — the operator e-stop request + the decel-to-stop safe state
