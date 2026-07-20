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
`~/kirra-runtime-sdk/speak.sh` (the `-D plughw:0,0` is the **speaker** device):
```bash
#!/usr/bin/env bash
exec ~/piper/piper --model ~/piper/en_US-lessac-medium.onnx --output-raw \
  | aplay -D plughw:0,0 -r 22050 -f S16_LE -t raw -
```
`chmod +x ~/kirra-runtime-sdk/speak.sh`

## 4. Env — `/etc/kirra/robot.env` (the single source every Rabbit script sources)
```bash
KIRRA_STT_CMD="whisper-cli -m /home/jetson/whisper.cpp/models/ggml-base.en.bin -np -nt -f"
KIRRA_TTS_CMD="/home/jetson/kirra-runtime-sdk/speak.sh"
KIRRA_RECORD_CMD="arecord -D plughw:3,0 -d 4 -f S16_LE -r 16000 -c 1"   # -D = the MIC device
# (optional) verdict-narration voice — an AUDITOR-role token, never the admin token:
# KIRRA_MICK_AUDITOR_TOKEN="<auditor principal token>"
```
Only the two `-D plughw:X,0` values and the real paths differ from
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

---

## Verify (in order)
```bash
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
