# R2 voice/audio — the as-configured, replayable setup

The **concrete** voice bring-up for the R2 (Jetson **Orin NX**, JetPack **6.2
"Super"**, Ubuntu 22.04). `RABBIT_AUDIO_STACK.md` explains *why*; this is the
*exactly what*, so the config can be **reproduced or reconfigured** from scratch
(e.g. after a reflash, or on a second unit). Validated on hardware 2026-07.

> Values marked **(this unit)** are specific to the peripherals plugged into this
> robot — re-derive them with `aplay -l` / `arecord -l` if you swap devices or
> reflash (ALSA card **numbers are not stable**; the device **names** are).

---

## 0. Audio devices (this unit)

Two USB audio gadgets + the Astra camera's mic. From `aplay -l` / `arecord -l`:

| Role | Device (stable name) | Card (this unit) | ALSA address |
|---|---|---|---|
| 🔊 Speaker (playback) | `UACDemoV1.0` (Jieli) | card **0** | `plughw:0,0` |
| 🎤 Mic (capture) | `USB PnP Sound Device` (TI PCM2902) | card **3** | `plughw:3,0` |
| ✗ ignore | `ORBBEC Depth Sensor` (camera mic) | card 4 | — |

Re-derive after any replug/reflash:
```bash
aplay -l    # find the USB speaker's card number  → plughw:<N>,0
arecord -l  # find the USB mic's card number       → plughw:<M>,0
# quick loopback sanity (record 3 s, play it back):
arecord -D plughw:3,0 -d 3 -f S16_LE -r 16000 -c 1 /tmp/t.wav && aplay -D plughw:0,0 /tmp/t.wav
```
Use `plughw:` (not `hw:`) so ALSA converts the sample rate/format (the speaker
runs 48 kHz; piper is 22050 Hz).

> **Prefer the device-NAME address, not the number.** ALSA card numbers move
> across reboots/replugs — on the 2026-07 hardening pass the mic came back as
> card **1**, not the card **3** in the table above (same physical device). The
> **names** held. Use the name form everywhere (`speak.sh`, `KIRRA_RECORD_CMD`,
> `amixer -c`), and you never chase a renumber:
> ```bash
> # find the stable NAME token (the [id] in brackets), then use plughw:CARD=<id>:
> aplay -l   | sed -n 's/^card [0-9]*: \([^ ]*\).*/speaker id: \1/p'
> arecord -l | sed -n 's/^card [0-9]*: \([^ ]*\).*/mic id:     \1/p'
> #  → speaker: plughw:CARD=UACDemoV10,DEV=0     mic: plughw:CARD=Device,DEV=0
> ```

---

## 1. STT — whisper.cpp (`whisper-cli`)
```bash
cd ~ && git clone https://github.com/ggml-org/whisper.cpp && cd whisper.cpp
cmake -B build && cmake --build build -j --config Release      # CPU; ~1–2 s / 4 s clip
sh ./models/download-ggml-model.sh base.en                     # → models/ggml-base.en.bin
sudo ln -sf ~/whisper.cpp/build/bin/whisper-cli /usr/local/bin/whisper-cli
```
Optional CUDA (moves STT to ~0.5 s): `rm -rf build && cmake -B build -DGGML_CUDA=1 && cmake --build build -j` (JetPack ships CUDA at `/usr/local/cuda`; prefix `CUDACXX=/usr/local/cuda/bin/nvcc` if cmake can't find it).

## 2. TTS — piper (prebuilt aarch64)
```bash
cd ~
wget https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_aarch64.tar.gz
tar -xzf piper_linux_aarch64.tar.gz                            # → ~/piper/
cd ~/piper
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json
```
Both the `.onnx` (~60 M) and its `.onnx.json` are required. If piper errors on a
missing `.so`, run with `LD_LIBRARY_PATH=~/piper` (and bake that into `speak.sh`).

## 3. `speak.sh` (TTS wrapper — text on stdin → speaker)
Keep it **OUTSIDE the git checkout** — `/opt/kirra/robot/speak.sh`. Use the
speaker's **name** (`plughw:CARD=UACDemoV10,DEV=0`), not `plughw:0,0`:
```bash
sudo install -d -m0755 /opt/kirra/robot
sudo tee /opt/kirra/robot/speak.sh >/dev/null <<'SH'
#!/usr/bin/env bash
exec ~/piper/piper --model ~/piper/en_US-lessac-medium.onnx --output-raw \
  | aplay -D plughw:CARD=UACDemoV10,DEV=0 -r 22050 -f S16_LE -t raw -
SH
sudo chmod +x /opt/kirra/robot/speak.sh
```
> **⚠ Why out of the repo (learned the hard way, 2026-07):** an untracked
> `speak.sh` *inside* the checkout gets swept by `git stash push -u` /
> `git clean` during any branch-cleanup — and TTS then dies with a silent
> `FileNotFoundError` while wake/STT/LLM all still look fine. Living under
> `/opt/kirra/robot/` makes it immune to every git operation. Point
> `KIRRA_TTS_CMD` at that path (§4).

## 4. Env — `/etc/kirra/robot.env` (the single source every Rabbit script sources)

> **🎤 Mic capture goes through the SESSION SOUND SERVER, not direct ALSA
> (2026-07 hardening finding).** With the desktop session's PulseAudio active,
> the server owns the C-Media mic: `arecord -D plughw:CARD=Device,DEV=0 …`
> fails **`Device or resource busy`** even while
> `/proc/asound/card*/pcm0c/sub0/status` reads `closed` and no process holds
> the PCM (D-Bus device reservation) — the recorder dies instantly and the
> wake listener loops "capture stream ended". Capture with `parec` against an
> **explicit** source via `robot/pulse_capture.sh` (fail-closed: missing
> parec/source is a visible refusal, never a default-mic fallback). Derive the
> source name once — it is stable across reboots, unlike ALSA card numbers:
> ```bash
> pactl list short sources        # → alsa_input.usb-…USB_PnP_Sound_Device-00.analog-mono
> ```

```bash
KIRRA_STT_CMD="whisper-cli -m /home/jetson/whisper.cpp/models/ggml-base.en.bin -np -nt -f"
KIRRA_TTS_CMD="/opt/kirra/robot/speak.sh"                                   # git-safe path (§3)
# Mic — through the session sound server (see the note above). Explicit source:
KIRRA_PULSE_SOURCE="alsa_input.usb-C-Media_Electronics_Inc._USB_PnP_Sound_Device-00.analog-mono"
KIRRA_RECORD_CMD="python3 /opt/kirra/robot/vad_record.py"      # bounded turn recorder (VAD endpointed)
KIRRA_VAD_CAPTURE_CMD="/opt/kirra/robot/pulse_capture.sh"      # its raw stream, via parec
# (KIRRA_WAKE_RECORD_CMD stays UNSET — the wake listener defaults to
#  pulse_capture.sh over KIRRA_PULSE_SOURCE; bare no-device arecord is refused.)
# ALSA-direct alternative, ONLY for a headless/no-session install (RABBIT_AUDIO_STACK.md §4):
# KIRRA_RECORD_CMD="arecord -D plughw:CARD=Device,DEV=0 -d 4 -f S16_LE -r 16000 -c 1"
# (optional) let rabbit_converse ground perception ("what do you see?") — see §7:
# KIRRA_ROS_SETUP="/opt/ros/humble/setup.bash"
# (optional) verdict-narration voice — an AUDITOR-role token, never the admin token:
# KIRRA_MICK_AUDITOR_TOKEN="<auditor principal token>"
```
Only the `KIRRA_PULSE_SOURCE` / speaker `-D plughw:CARD=…` values and the real
paths differ from `robot/install/rabbit.env.example`.

The turn recorder is **VAD-endpointed** (stops on trailing silence — ~1.3 s for
a short command vs a flat 4 s window) and pulls its raw stream through the same
Pulse backend, so no ALSA device var is needed. Tunables if the room needs them:

```bash
KIRRA_VAD_SILENCE_MS=600
KIRRA_VAD_MIN_SPEECH_MS=250
KIRRA_VAD_MAX_MS=6000
KIRRA_VAD_START_TIMEOUT_MS=3000
```

Still a bounded mic — `KIRRA_VAD_MAX_MS` is a hard ceiling and a wedged device
is bounded by a wall clock. (Only the headless ALSA-direct variant uses
`KIRRA_VAD_DEVICE`, which is then **required**: unset, `vad_record.py` refuses
before opening anything rather than falling back to ALSA's default — not the
mic, and it fails silently.) Details + tuning: `RABBIT_AUDIO_STACK.md` §1a.

**Robot playback is suppressed from wake/follow-up detection** (§1b of
`RABBIT_AUDIO_STACK.md`): while the robot speaks, the TTS side holds an
flock (`KIRRA_VOICE_PLAYBACK_STATE`, per-user runtime dir) that the wake
listener probes — the robot's own Piper output can never fire
`wake_word: follow-up: speech onset` and start a self-conversation loop
(the live 2026-08 failure). A post-playback cooldown
(`KIRRA_VOICE_PLAYBACK_COOLDOWN_MS`, default 500) discards the speaker
tail, and `KIRRA_VOICE_MAX_FOLLOWUP_TURNS` (default 3) caps wake-free
follow-ups per episode. Barge-in is intentionally disabled on this path
(no AEC — RMS cannot tell the operator from the speaker). Rollback:
`systemctl --user start rabbit-voice.service`.

## 5. PTT button (GPIO) — the Orin gotchas
```bash
# JetPack 6.2 "Super": stock Jetson.GPIO 2.1.7 fails ("Could not determine Jetson
# model"). Install >= 2.1.12 from source:
sudo pip3 install --upgrade --ignore-installed --no-cache-dir \
  "Jetson.GPIO @ git+https://github.com/NVIDIA/jetson-gpio.git"
sudo groupadd -f gpio && sudo usermod -aG gpio jetson    # re-login for the group
```
**External pull-up is mandatory on Orin** — Jetson.GPIO ignores the internal
pull-up, so the pin floats and phantom-triggers:
```
 3V3 (pin 1) ──[ 10 kΩ ]──┬── header pin 18   (idles HIGH)
   button (N.O.) ─────────┴── GND (pin 20)    (press → LOW)
```
Defaults live in `robot/ptt_button.py` (`KIRRA_PTT_GPIO_PIN=18`, BOARD, active-low).
Verify a clean trigger before wiring into the loop:
```bash
sudo python3 ~/kirra-runtime-sdk/robot/ptt_button.py | cat -A   # one blank line per press, no phantoms
```
If Jetson.GPIO still can't ID the carrier (third-party Yahboom board warns
"not verified"), the robust path is a **libgpiod** backend (kernel GPIO chardev,
no board database) — a `ptt_button.py` follow-up to add + bench-test.

## 6. Platform fix applied — ROS apt key
The ROS 2 apt repo key expired (`EXPKEYSIG F42ED6FBAB17C654`); refresh it so
`apt` and any future `ros-humble-*` install works:
```bash
sudo curl -sSL https://raw.githubusercontent.com/ros/rosdistro/master/ros.key \
  -o /usr/share/keyrings/ros-archive-keyring.gpg && sudo apt update
```

## 7. Field hardening (2026-07 bring-up — the non-obvious stuff)

Three things that made the difference between "silent/quiet/deaf" and "works",
none of them in the engine setup above:

### 7a. ALSA mixer levels — and PERSIST them (they reset on reboot)
Fresh boots came up with the mic gain low (barely-heard wake word) and the
speaker hot. Set the levels, then **store** them — otherwise ALSA resets to
defaults on the next reboot and you re-debug "it stopped hearing/speaking":
```bash
# control names vary by device — list them first, per card (use the NAME, §0):
amixer -c Device   scontrols          # mic card
amixer -c UACDemoV10 scontrols        # speaker card
# this unit — boost mic capture/AGC to full, set speaker to a comfortable 80%:
amixer -c Device   sset 'Auto Gain Control' 100% 2>/dev/null || true
amixer -c Device   sset 'Mic'  100% cap  2>/dev/null || true
amixer -c UACDemoV10 sset 'PCM' 80%
# PERSIST across reboot (without this, every reboot silently reverts the above):
sudo alsactl store
```
80% on the speaker was the sweet spot on this unit — 100% was uncomfortably loud
in a room. Re-run + `alsactl store` after any reflash.

### 7b. Let the voice service GROUND perception ("what do you see?")
`rabbit_converse` can only answer perception questions if it has a **full ROS
environment** (`ROS_DISTRO`/`AMENT_PREFIX_PATH`), not just `ROS_DOMAIN_ID`. A
bare systemd `--user` service that only exports the domain id answers "I can't
see" to everything. Source ROS in the unit via a drop-in (adjust the unit name
to your voice service):
```bash
mkdir -p ~/.config/systemd/user/rabbit-voice.service.d
cat > ~/.config/systemd/user/rabbit-voice.service.d/10-ros.conf <<'EOF'
[Service]
# Source the ROS base (and the ws overlay if the node needs custom msgs) before exec.
ExecStart=
ExecStart=/usr/bin/env bash -lc 'source "${KIRRA_ROS_SETUP:-/opt/ros/humble/setup.bash}" && exec /opt/kirra/robot/rabbit_voice.sh'
EOF
systemctl --user daemon-reload && systemctl --user restart rabbit-voice.service
# confirm the running process actually has ROS (not just the domain id):
tr '\0' '\n' < /proc/$(pgrep -f rabbit_converse | head -1)/environ | grep -E 'ROS_DISTRO|AMENT_PREFIX'
```
Set `KIRRA_ROS_SETUP` in `/etc/kirra/robot.env` if your ROS lives elsewhere.

### 7c. Resource contention degrades the local LLM router
Running the drive stack + Ollama + whisper concurrently on the Orin starves the
gemma router: it starts returning `directive: null` or mis-classifying (e.g. a
movement request → `cruise`, which carries no goal and is ignored). Mitigations:
use the **router's own example phrasing** ("creep forward one meter" parses far
more reliably than "drive forward"); don't run a big `colcon`/`cargo` build in
the same window as a voice turn; and for a clean deterministic drive trigger,
publish a `/goal_pose` directly instead of going through STT→LLM (see
`ros2_ws/.../occy_doer.py` — it subscribes `/goal_pose`).

---

## Verify (in order)
```bash
# 0. one-shot config doctor (read-only; the fastest "am I misconfigured?" check —
#    catches a drifted ALSA card, a missing engine/model, an unset env key):
./robot/kirra_voice_doctor.sh          # ✔/❌/⚠ + a fix hint per gap; exit 0 iff no ❌
#    (rabbit_boot.py runs it --quiet on boot and SPEAKS a warning on a ❌ — voice line A6.)

# 1. engines standalone:
echo "rabbit online" | ~/kirra-runtime-sdk/speak.sh                 # hear the speaker
# mic through the sound server (5 s of raw s16le/16k/mono ≈ 160,000 bytes):
set -a; . /etc/kirra/robot.env; set +a
timeout 5 /opt/kirra/robot/pulse_capture.sh >/tmp/kirra-mic.raw; ls -l /tmp/kirra-mic.raw
whisper-cli -m ~/whisper.cpp/models/ggml-base.en.bin -np -nt -f /tmp/t.wav   # prints your words

# 2. the governed door over TEXT (no mic/button needed — remote-friendly):
cd ~/kirra-runtime-sdk
set -a; . /etc/kirra/robot.env; set +a
ss -tlnp | grep -E ':(8090|8102|11434)\b'        # verifier / mick / ollama must listen
unset KIRRA_TTS_CMD                              # print-only when you can't hear the room
./robot/rabbit_converse.py
#   "creep forward one meter" → "…the checker will bound it."  (directive accepted)
#   "what do you see?"        → answer, no motion (null directive)
#   "take us to the door"     → "Heading for the door."        (named destination relayed)

# 3. full voice loop (at the bench, mic+speaker+button live):
./robot/rabbit_voice.sh          # Enter-key driver (no GPIO)
./robot/run_voice_ptt.sh         # GPIO-button driver (after the pull-up is wired)
```

## Reconfigure checklist
Changed a device / reflashed? Re-derive the speaker card (§0) for `speak.sh`'s
`plughw:CARD=…` and the mic's PulseAudio source name
(`pactl list short sources`) for `KIRRA_PULSE_SOURCE` (§4), then re-run the
§Verify steps. Nothing else moves.
