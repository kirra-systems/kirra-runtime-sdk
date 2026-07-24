# Rabbit audio stack — STT, TTS, PTT, and the systemd-audio trap

> The **audio-engineering** companion to `RABBIT_BRINGUP_RUNBOOK.md` (which is the
> *how to run it* sheet). This doc is the *how to choose and tune the engines*,
> and — front and centre — **how to make audio work from a systemd service**,
> because the whole KIRRA stack runs as services and that is the single most
> likely thing to eat an evening.
>
> Everything here is **Channel A / the fenced door** (`RABBIT_CONVERSATION_DESIGN.md`):
> the mic and speaker are a doer-side UX layer. STT feeds the LLM *text*; TTS reads
> its *words*; the one path to the wheels is the same fail-closed `MickIntent`
> parse + checker a human typing hits. None of this can actuate.

## The path, and where the time goes

```
  🎤 ─► [PTT trigger] ─► arecord (bounded clip) ─► whisper-cli (STT) ─► TEXT
                                                                          │
                    rabbit_converse.py: router + persona LLM (Ollama) ◄─────┘
                          │ (A) SPEAK                  │ (B) directive TEXT → mick /intent
                          ▼                            ▼   → Occy → KIRRA checker → wheels
                    speak.sh: piper (TTS) ─► aplay ─► 🔊
```

**Latency budget** (indicative on an Orin NX — *not* a WCET claim; measure yours,
recipe below). The two dominant terms are the **fixed record window** and the
**LLM turn**:

| Stage | Typical | Notes |
|---|---|---|
| record (`arecord -d 4`) | **4.0 s fixed** | it waits the whole window — the biggest lever (see tuning) |
| STT (whisper `base.en`) | ~0.5–2 s | for a 4 s clip; CUDA build << CPU build |
| LLM (`gemma3:4b`) | ~2–5 s | the conversational cost; local, no cloud |
| TTS (piper `medium`) | ~0.3–1 s | faster than real-time for a sentence |

> **The deterministic lines skip both dominant terms.** The boot greeting
> (`rabbit_boot.py`), proactive warnings (`rabbit_watch.py`), and OTA replies
> (`rabbit_ota.py`) do **no** record and **no** LLM — they render a template
> straight to piper. That's why the safety-/reliability-relevant speech is
> instant and the conversational speech is the only thing that waits.

---

## 1. STT — whisper.cpp (`whisper-cli`)

Build whisper.cpp and fetch **one** `ggml-*.en` model (English-only variants are
smaller/faster than the multilingual ones). The house default is `base.en`:

| Model | Size | Speed | Accuracy | Use when |
|---|---|---|---|---|
| `tiny.en` | ~75 MB | fastest | lowest | latency-bound; short commands in a quiet room |
| **`base.en`** | ~142 MB | **sweet spot** | good | **the default** — commands + short questions |
| `small.en` | ~466 MB | slower | better | accents / background noise / longer utterances |

```bash
KIRRA_STT_CMD="whisper-cli -m models/ggml-base.en.bin -np -nt -f"
```
House convention (all engines): the **WAV path is appended as the last argument**;
STT prints the transcript to stdout. `-np -nt` = no-prints / no-timestamps so only
the text comes back. Build with CUDA on the Orin if you can — it moves STT from the
"seconds" column to the "sub-second" column.

**Recording** is a *bounded* clip — never an open mic:
```bash
KIRRA_RECORD_CMD="arecord -d 4 -f S16_LE -r 16000 -c 1"   # 4 s, 16 kHz mono, the whisper-native rate
```
The `-d 4` bound is the single biggest latency lever. To make Rabbit feel snappier,
either shorten it (`-d 2` for terse commands) or use **VAD endpointing** (below).

### 1a. VAD endpointing (`vad_record.py`) — opt-in, stop on silence

`robot/vad_record.py` is a **drop-in replacement** for the `arecord` line: it
records ONE utterance that ends on trailing silence instead of always waiting the
full window, so a terse "check yourself" returns in ~1 s while a longer sentence
still gets its time — up to a hard ceiling. It writes the appended WAV path (the
same `$KIRRA_RECORD_CMD "$wav"` contract), so nothing else in the pipeline
changes:

```bash
KIRRA_RECORD_CMD="python3 /opt/kirra/robot/vad_record.py"    # opt in
#   (left as the arecord line above → byte-identical prior behaviour)
```

It stays a **bounded** mic, not an open one: `KIRRA_VAD_MAX_MS` (default 8 s) is a
hard ceiling the endpointer always stops at, exactly like arecord's `-d` bound,
and a silence-only capture ends at `KIRRA_VAD_START_TIMEOUT_MS` with an empty clip
→ the fenced parser latches nothing → no motion. It emits only a WAV the existing
STT transcribes — **no new authority**. The default backend is `energy` (RMS vs
`KIRRA_VAD_RMS_FLOOR`, zero new deps — the same idea as `wake_word.py`'s pre-gate);
`KIRRA_VAD_BACKEND` is a fail-closed seam for a future Silero/webrtc detector (an
unimplemented value is refused, never silently ignored). The endpoint state
machine (min actual speech + trailing-silence + hard cap) is host-tested in
`robot/vad_record_test.py`; the capture loop is the hardware seam. Full env set:
`robot/install/rabbit.env.example`.

## 2. TTS — piper

Fetch **one** piper voice and wrap playback in a `speak.sh` (text on **stdin** →
raw PCM → `aplay`). `medium` quality is the latency/quality sweet spot; `high`
is richer at more CPU; `low` if you're CPU-bound.

```bash
#!/usr/bin/env bash
# speak.sh — text on stdin → spoken. NOTE the -D hw: device (see §4).
exec piper --model en_US-lessac-medium.onnx --output-raw \
  | aplay -D hw:0,0 -r 22050 -f S16_LE -t raw -
```
```bash
KIRRA_TTS_CMD="./speak.sh"      # unset → Rabbit prints instead of speaks (still works, just silent)
```
Match `aplay`'s `-r` to the voice's sample rate (piper medium voices are 22050 Hz;
a mismatch = chipmunk/slow-mo). A distinctive "Rabbit voice" is Stage-4 polish — any
piper voice drops in here; the architecture doesn't care which.

## 3. PTT — push-to-talk (`ptt_button.py`), and why not always-listening

Push-to-talk is the right default, for three reasons beyond ergonomics:
- **It bounds *when* the LLM even runs** — no turn fires without a press, so there
  is no idle model churn and no ambient-audio surprise.
- **No wake-word false-fires** — a wake word mishears; a button doesn't.
- **No hot-mic privacy issue** — the mic is closed until you hold the button.

Trigger = **one newline on stdout per press**, piped into the voice front-end
(`ptt_button.py | rabbit_voice.sh`). It's just an external OS process standing in
for the Enter key — the Rust/Python loop is unchanged. Keyboard Enter is the
zero-hardware fallback (`rabbit_voice.sh` alone).

GPIO wiring (default): a normally-open momentary button between the pin and GND;
a press pulls LOW.

> ⚠ **Orin needs an EXTERNAL pull resistor.** Jetson.GPIO on Orin **ignores**
> `setup()`'s `pull_up_down` (it warns as much at runtime), so the internal
> pull-up is *not* applied and the input **floats → phantom triggers** (a spurious
> press that records `[BLANK_AUDIO]`). Add **10 kΩ from the pin to 3V3** for
> active-low (idle HIGH, button→GND), or 10 kΩ pin→GND + button→3V3 for
> `KIRRA_PTT_ACTIVE=high`. This is not the older-Jetson behaviour where the
> internal pull-up sufficed.

```bash
KIRRA_PTT_GPIO_PIN=18       # BOARD (physical) pin
KIRRA_PTT_PIN_MODE=BOARD    # BOARD | BCM
KIRRA_PTT_ACTIVE=low        # low = button→GND (+ external pull-UP) | high = button→3V3 (+ external pull-DOWN)
KIRRA_PTT_LED_PIN=          # optional: OUTPUT pin lit while recording
```
Needs `Jetson.GPIO` + the user in the `gpio` group (udev rules), or root. On
**JetPack 6.2 "Super"** boards the apt/pip 2.1.7 fails with *"Could not determine
Jetson model"* — install ≥ 2.1.12 from NVIDIA's GitHub (`pip install --upgrade
--ignore-installed "Jetson.GPIO @ git+https://github.com/NVIDIA/jetson-gpio.git"`).
Concrete, replayable setup for this R2: `R2_VOICE_AUDIO_SETUP.md`.

> 🔴 The PTT button is a **microphone trigger, not the e-stop.** The e-stop is a
> separate hardware kill in the motor-power line (`R2_UNTETHERED_BRINGUP.md` §3).
> Losing the button = you can't talk to it; losing the e-stop = you can't stop it.
> Different circuits, different criticality — never wire one as the other.

## 3b. Wake word (W1, opt-in) — "hello rabbit" as a third trigger source

PTT **stays the recommended default**; the wake word is an opt-in *trigger
producer* honoring the same one-newline contract, so it drops in with zero
change to the pipeline:

```bash
python3 robot/wake_word.py | ./robot/rabbit_voice.sh                              # wake word only
{ python3 robot/ptt_button.py & python3 robot/wake_word.py; } | ./robot/rabbit_voice.sh   # both
```

As a service: `kirra-rabbit-voice.service` (staged by `install_robot_units.sh`,
enable deliberately). Master gate `KIRRA_WAKE_ENABLED` in `robot.env`; the
listener exits cleanly when unset. Full env set: `robot/install/rabbit.env.example`.

**Detection** (no LLM anywhere): `arecord` raw stream → in-memory ring buffer →
**RMS energy pre-gate** (a silent room runs zero inference) → whisper.cpp
**tiny** (`KIRRA_WAKE_STT_CMD` — a second, smaller model than the turn STT) on a
~2 s tmpfs window → a **pure token matcher** (ordered-adjacent, one-edit
tolerance on long tokens only: "hallo rabbits" wakes; a stray "yo" or "hey the
rabbit robot" never does; host-tested in `robot/wake_word_test.py`). On a hit:
ack cue ("Yes?" through TTS by default — a **false fire is heard, not silent**),
the mic is **released** so the turn recorder can claim the device, then a
cooldown.

**Re-arm is event-driven, not a blind timer (Slice R).** The mic is held closed
only until the turn it triggered actually *finishes*, then reopens at once — so
the operator's immediate follow-up "hey rabbit" is heard. The old fixed
`KIRRA_WAKE_HOLDOFF_S` sleep couldn't fit a variable-length turn: a fast turn left
the mic shut through a dead window that dropped the follow-up (the "it only
answered one question" symptom), while a long turn reopened mid-reply. Now
`rabbit_converse` publishes a tmpfs turn-state file (`turn_state.py`, the same
atomic-epoch pattern as `barge_in.py`) that the listener waits on, bounded by
`KIRRA_WAKE_TURN_GRACE_S` (wait-for-start / garbage-clip reopen) and
`KIRRA_WAKE_TURN_MAX_S` (hung-writer ceiling) and fail-safe (an old/crashed
writer degrades to the timers). The pure `rearm_decision` gate — including a
five-consecutive-cycle re-arm proof — is host-tested in
`robot/turn_state_test.py`. `KIRRA_WAKE_REARM=timer` restores the legacy blind
`KIRRA_WAKE_HOLDOFF_S` hold-off.

**How this answers §3's three objections** (which still stand for *naive*
always-listening):
- *Bounds when the LLM runs* — the LLM still runs at most once per wake, exactly
  as once per press; detection is energy-gated tiny-whisper + a regex-class
  matcher, and idle ≈ zero inference.
- *Mishears* — a false wake is exactly the phantom-PTT-press class (one bounded
  clip → garbage transcript → no intent → no motion), made audible by the ack
  cue. The wake word adds **zero actuation authority**.
- *Hot mic* — transcribe-and-discard: fixed in-memory window, tmpfs wav deleted
  every cycle, no audio persisted, no transcripts logged (only wake hits).
  Operator controls: **"rabbit, go to sleep"** (nap, `KIRRA_WAKE_NAP_MIN`) /
  **"stop listening"** (mute) — deterministic matchers in `rabbit_wake.py`,
  never LLM-decided; while suspended the capture process is closed entirely.
  Optional `KIRRA_WAKE_LED_PIN` lights while the mic is open. Note the
  asymmetry: a muted listener can't hear "start listening" — resume via the
  button/Enter or the nap timer.

Before enabling on battery, measure the duty-cycled tiny.en cost (§5 pattern,
`tegrastats` A/B with the listener on/off). If it's too hot, the recorded
fallback is swapping the producer for a dedicated wake engine (openWakeWord) —
the trigger contract means that changes one script and nothing else.

### 3c. Barge-in (`barge_in.py`) — opt-in, cut a reply and listen

When Rabbit is mid-reply and you want to talk NOW, barge-in stops the speech
instead of making you wait for the sentence to finish. Opt in with
`KIRRA_BARGE_IN_ENABLED=1`; default off → conversational replies use the plain
blocking `speak()` (byte-identical).

How it works: a **PTT press** (`ptt_button.py`) raises a monotonic **signal
file** (`KIRRA_BARGE_IN_FILE`), and the in-progress conversational reply — played
in a killable subprocess by `barge_in.speak_interruptible` — polls that signal
and terminates playback the moment it advances past the baseline captured when
the reply started (so a leftover signal never false-cuts the *next* reply). The
press's own trigger then records your follow-up as usual. Any event source can
raise one too: `python3 robot/barge_in.py --signal` (wire it to an e-stop / a
"hush" button / a critical posture transition).

🔴 It is **Channel A / cosmetic**: a barge-in only STOPS speech — it never starts
motion, emits an intent, or touches the fenced `/intent` door, so cutting a reply
early is always safe. The priority model is P0 e-stop > P1 {wake, human-interrupt}
> P2 mission > P3 info-speech; a reply is P3, so any raised barge-in cuts it
(`should_interrupt`, host-tested).

The **PTT** path works today because the button is independent of the mic. The
*acoustic* "say the wake word **over** Rabbit while it is still speaking" path
does not yet: the wake mic stays closed until the reply finishes, so a barge-in
mid-sentence needs full-duplex audio (echo cancellation) — a tracked follow-up.
Note this is now **only** the interrupt-while-speaking case: the wake word
**immediately after** a reply is heard right away (event-driven re-arm, Slice R
above), so back-to-back questions no longer need the button.

---

## 4. 🔴 Audio from a systemd service (read this before enabling the units)

**The trap:** audio works perfectly when you run a Rabbit script by hand, then goes
**silent the moment the same script runs as a systemd service** — with no error.
Because the whole KIRRA stack runs as services (`kirra-rabbit-watch`,
`kirra-rabbit-greet`, and any voice front-end you unit-ise), every Rabbit audio path
will hit this. Budget for it here, not at 2 a.m.

**Why it happens:** a `User=` systemd service has **no login/graphical session**.
So:
- `XDG_RUNTIME_DIR` (`/run/user/<uid>`) is **not set**, so a PulseAudio/PipeWire
  client can't find the per-user socket (`/run/user/<uid>/pulse/native`) → **no
  sink/source → silence**, and often no error (the client just picks "null").
- The service user may not be in the `audio` group → no permission on
  `/dev/snd/*` even for ALSA.
- `aplay -l` / `arecord -l` numbering (`hw:0`) is **not stable** and the USB
  audio device is frequently **not** card 0.

**The fix (recommended): bypass PulseAudio — talk to ALSA directly.**

1. **Name the exact device.** `aplay -l` and `arecord -l` → find your USB codec's
   card/device, e.g. `card 1: Device …`. Use `hw:1,0` (or `plughw:1,0` if you need
   ALSA to convert sample rate/format). `hw:` = no conversion, one process at a
   time — fine here (one speaker, one mic).
   ```bash
   # speak.sh   → aplay -D plughw:1,0 -r 22050 -f S16_LE -t raw -
   # KIRRA_RECORD_CMD → arecord -D plughw:1,0 -d 4 -f S16_LE -r 16000 -c 1
   ```
2. **Put the service user in `audio`.** `sudo usermod -aG audio <user>` (the
   installer renders `User=` to the invoking user — that user needs `audio`).
3. **No Pulse in the path.** With `-D hw:/plughw:` there is no dependency on the
   user session at all → it works headless from a service.

**The alternative (only if you specifically want mixing/Pulse):** set both in the
unit so the client finds the session bus —
```ini
Environment=XDG_RUNTIME_DIR=/run/user/1000
Environment=PULSE_SERVER=unix:/run/user/1000/pulse/native
```
— but this couples the service to a logged-in user's Pulse and is more fragile
than ALSA-direct. Prefer ALSA-direct unless you have a reason not to.

**Validate in-service, not just by hand:**
```bash
# playback from a service context (no session):
sudo systemd-run --uid=<user> --pty bash -c 'echo test | ./speak.sh'
# or just enable the greet unit and watch it actually speak on boot:
sudo systemctl start kirra-rabbit-greet && journalctl -u kirra-rabbit-greet -f
```
If it prints but doesn't speak, it's this section — re-check the `-D` device and
the `audio` group. **Rabbit degrades to printing when TTS can't reach the device
(harmless), so silence is the symptom, not a crash.** The units
(`kirra-rabbit-watch`, `kirra-rabbit-greet`) already carry this caveat inline; this
section is the full fix.

---

## 5. Measure your end-to-end latency (don't guess)

```bash
# time each stage on a real clip:
time ( arecord -D plughw:1,0 -d 4 -f S16_LE -r 16000 -c 1 /tmp/c.wav )   # ~record window
time ( whisper-cli -m models/ggml-base.en.bin -np -nt -f /tmp/c.wav )    # STT
time ( curl -s localhost:11434/api/chat -d '{"model":"gemma3:4b","stream":false,
        "messages":[{"role":"user","content":"say hello"}]}' >/dev/null ) # LLM turn
time ( echo "all systems nominal" | ./speak.sh )                          # TTS
```
Sum = PTT-release → spoken reply. If it's too slow: shorten `-d`, drop to
`tiny.en`, or use a smaller/faster router model for the chat/directive split.

## 6. Troubleshooting (audio-service delta)

| Symptom | Cause | Fix |
|---|---|---|
| Works by hand, silent as a service | no session / Pulse socket unreachable | §4: ALSA-direct `-D plughw:X,0`; user in `audio` |
| `aplay: ... No such file or directory` | wrong `hw:` card number | `aplay -l`; the USB codec is often not card 0 |
| Chipmunk / slow-mo voice | `aplay -r` ≠ voice sample rate | match `-r` to the piper voice (medium = 22050) |
| Records silence/garbage from a service | wrong `arecord -D` / not in `audio` | name the capture device; `usermod -aG audio` |
| Greeting never speaks on boot | TTS device unreachable in-service | validate with `systemd-run --uid`; it degraded to print |

## References
- `docs/hardware/RABBIT_BRINGUP_RUNBOOK.md` — the run-it-start-to-finish sheet
- `docs/hardware/RABBIT_CONVERSATION_DESIGN.md` — the architecture + the safety fence
- `docs/rabbit/RABBIT_VOICE_LINES.md` — every spoken line (the deterministic paths that skip STT/LLM)
- `robot/install/rabbit.env.example` — the single env template these vars live in
- `robot/install/systemd/kirra-rabbit-{watch,greet}.service` — the units that carry the §4 caveat
- `docs/testing/SPEECH_RABBIT_DEMO.md` — the built STT→intent→TTS loop
