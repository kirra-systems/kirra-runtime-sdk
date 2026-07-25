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
```bash
KIRRA_STT_CMD="whisper-cli -m /home/jetson/whisper.cpp/models/ggml-base.en.bin -np -nt -f"
KIRRA_TTS_CMD="/opt/kirra/robot/speak.sh"                                   # git-safe path (§3)
KIRRA_RECORD_CMD="arecord -D plughw:CARD=Device,DEV=0 -d 4 -f S16_LE -r 16000 -c 1"  # -D = the MIC (name, not number)
# (optional) let rabbit_converse ground perception ("what do you see?") — see §7:
# KIRRA_ROS_SETUP="/opt/ros/humble/setup.bash"
# (optional) verdict-narration voice — an AUDITOR-role token, never the admin token:
# KIRRA_MICK_AUDITOR_TOKEN="<auditor principal token>"
```
Only the two `-D plughw:CARD=…` values and the real paths differ from
`robot/install/rabbit.env.example`. `-d 4` is the record window (drop to `-d 3`
for snappier turns).

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
Changed a device / reflashed? Re-derive card numbers (§0), update the two
`plughw:X,0` values in `speak.sh` (speaker) and `KIRRA_RECORD_CMD` (mic), and
re-run the §Verify steps. Nothing else moves.
