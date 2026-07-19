# KITT audio stack — STT, TTS, PTT, and the systemd-audio trap

> The **audio-engineering** companion to `KITT_BRINGUP_RUNBOOK.md` (which is the
> *how to run it* sheet). This doc is the *how to choose and tune the engines*,
> and — front and centre — **how to make audio work from a systemd service**,
> because the whole KIRRA stack runs as services and that is the single most
> likely thing to eat an evening.
>
> Everything here is **Channel A / the fenced door** (`KITT_CONVERSATION_DESIGN.md`):
> the mic and speaker are a doer-side UX layer. STT feeds the LLM *text*; TTS reads
> its *words*; the one path to the wheels is the same fail-closed `MickIntent`
> parse + checker a human typing hits. None of this can actuate.

## The path, and where the time goes

```
  🎤 ─► [PTT trigger] ─► arecord (bounded clip) ─► whisper-cli (STT) ─► TEXT
                                                                          │
                    kitt_converse.py: router + persona LLM (Ollama) ◄─────┘
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
> (`kitt_boot.py`), proactive warnings (`kitt_watch.py`), and OTA replies
> (`kitt_ota.py`) do **no** record and **no** LLM — they render a template
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
The `-d 4` bound is the single biggest latency lever. To make KITT feel snappier,
either shorten it (`-d 2` for terse commands) or, as a follow-up, put a VAD in
front so it stops on silence instead of waiting the full window.

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
KIRRA_TTS_CMD="./speak.sh"      # unset → KITT prints instead of speaks (still works, just silent)
```
Match `aplay`'s `-r` to the voice's sample rate (piper medium voices are 22050 Hz;
a mismatch = chipmunk/slow-mo). A distinctive "KITT voice" is Stage-4 polish — any
piper voice drops in here; the architecture doesn't care which.

## 3. PTT — push-to-talk (`ptt_button.py`), and why not always-listening

Push-to-talk is the right default, for three reasons beyond ergonomics:
- **It bounds *when* the LLM even runs** — no turn fires without a press, so there
  is no idle model churn and no ambient-audio surprise.
- **No wake-word false-fires** — a wake word mishears; a button doesn't.
- **No hot-mic privacy issue** — the mic is closed until you hold the button.

Trigger = **one newline on stdout per press**, piped into the voice front-end
(`ptt_button.py | kitt_voice.sh`). It's just an external OS process standing in
for the Enter key — the Rust/Python loop is unchanged. Keyboard Enter is the
zero-hardware fallback (`kitt_voice.sh` alone).

GPIO wiring (default): a normally-open momentary button between the pin and GND;
the internal pull-up idles HIGH, a press pulls LOW.

```bash
KIRRA_PTT_GPIO_PIN=18       # BOARD (physical) pin
KIRRA_PTT_PIN_MODE=BOARD    # BOARD | BCM
KIRRA_PTT_ACTIVE=low        # low = button→GND (pull-up) | high = button→3V3
KIRRA_PTT_LED_PIN=          # optional: OUTPUT pin lit while recording
```
Needs `Jetson.GPIO` + the user in the `gpio` group (udev rules), or root.

> 🔴 The PTT button is a **microphone trigger, not the e-stop.** The e-stop is a
> separate hardware kill in the motor-power line (`R2_UNTETHERED_BRINGUP.md` §3).
> Losing the button = you can't talk to it; losing the e-stop = you can't stop it.
> Different circuits, different criticality — never wire one as the other.

---

## 4. 🔴 Audio from a systemd service (read this before enabling the units)

**The trap:** audio works perfectly when you run a KITT script by hand, then goes
**silent the moment the same script runs as a systemd service** — with no error.
Because the whole KIRRA stack runs as services (`kirra-kitt-watch`,
`kirra-kitt-greet`, and any voice front-end you unit-ise), every KITT audio path
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
sudo systemctl start kirra-kitt-greet && journalctl -u kirra-kitt-greet -f
```
If it prints but doesn't speak, it's this section — re-check the `-D` device and
the `audio` group. **KITT degrades to printing when TTS can't reach the device
(harmless), so silence is the symptom, not a crash.** The units
(`kirra-kitt-watch`, `kirra-kitt-greet`) already carry this caveat inline; this
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
- `docs/hardware/KITT_BRINGUP_RUNBOOK.md` — the run-it-start-to-finish sheet
- `docs/hardware/KITT_CONVERSATION_DESIGN.md` — the architecture + the safety fence
- `docs/kitt/KITT_VOICE_LINES.md` — every spoken line (the deterministic paths that skip STT/LLM)
- `robot/install/kitt.env.example` — the single env template these vars live in
- `robot/install/systemd/kirra-kitt-{watch,greet}.service` — the units that carry the §4 caveat
- `docs/testing/SPEECH_KITT_DEMO.md` — the built STT→intent→TTS loop
