# KITT bring-up runbook — talk to the R2, start to finish

> One start-to-finish sheet that ties the four KITT scripts + the audio engines +
> the governed loop together. Every KITT layer is **Channel A (SPEAK) or the ONE
> fenced door** — none can drive unsafely (see
> `docs/hardware/KITT_CONVERSATION_DESIGN.md`). Do the safety item (§0) first.

## The pieces (what talks to what)

```
  🎤 mic ─► whisper (STT) ─► TEXT ─┬─► kitt_converse.py ─(SPEAK)─► Piper (TTS) ─► 🔊
   (PTT button / Enter)            │      persona + memory + router
                                   └─(directive TEXT)─► mick POST /intent ─► Occy
                                        the ONE fail-closed door ─► KIRRA checker ─► wheels

  kitt_watch.py ─(reads /metrics + /narration/last)─(SPEAK on events)─► Piper ─► 🔊
```

| Script | Role | Channel |
|---|---|---|
| `robot/kitt_ask.py` | one-shot grounded Q&A | SPEAK only |
| `robot/kitt_converse.py` | multi-turn dialogue + persona + router | SPEAK + the fenced door |
| `robot/kitt_watch.py` | proactive event speech | SPEAK only |
| `robot/kitt_voice.sh` | voice glue: trigger→record→STT→`kitt_converse` | (drives the above) |
| `robot/ptt_button.py` | GPIO push-to-talk trigger | trigger only |

---

## 0. Safety first (do NOT skip)

- 🔴 **Hardware e-stop present and tested** if the wheels can turn — a physical
  kill in the motor-power line (`R2_UNTETHERED_BRINGUP.md` §3). The PTT button is
  a MIC trigger, never the e-stop.
- For first voice tests, keep it **on the bench, wheels up** (KITT is doer-side;
  the checker bounds motion, but you're validating a new front-end).

## 1. Prerequisites (once)

> Engine selection (whisper model size, piper voice), the latency budget, and the
> **audio-from-a-systemd-service** fix are their own sheet:
> `docs/hardware/KITT_AUDIO_STACK.md`. Read its §4 before enabling the KITT units.

- **Audio hardware:** USB mic + speaker on the Orin. Verify ALSA sees them:
  `arecord -l` (capture) and `aplay -l` (playback).
- **Offline speech engines:**
  - whisper.cpp — build `whisper-cli`, fetch a model (`ggml-base.en.bin`).
  - Piper — fetch a voice (`en_US-lessac-medium.onnx`), wrap playback in
    `speak.sh` (text on stdin → `piper … --output-raw | aplay -r 22050 -f S16_LE -t raw -`).
- **LLM:** Ollama running + the model pulled: `ollama pull gemma3:4b`.
- **Built binaries:** `cargo build -p kirra-sidecars --release`
  (mick_service, planner_service, taj_service, speech_shell) and the verifier.
- **Python:** `pip3 install requests`; for the PTT button `pip3 install Jetson.GPIO`
  (+ gpio group / udev). For the perception answer, a ROS-sourced shell.

## 2. One env file

Copy `robot/install/kitt.env.example` → fill it in → make the KITT scripts read
it (they source `/etc/kirra/robot.env`):

```bash
cp robot/install/kitt.env.example /tmp/kitt.env      # edit STT/TTS/RECORD paths
set -a; . /tmp/kitt.env; set +a                       # or append into /etc/kirra/robot.env
```

Key ones: `KIRRA_STT_CMD` (required), `KIRRA_TTS_CMD` (speak.sh), `KIRRA_RECORD_CMD`,
`KIRRA_KITT_MODEL`. For **"why did we stop"** + proactive **deny** events, also set
`KIRRA_MICK_AUDITOR_TOKEN` (an auditor-role principal, NOT the admin token) so
mick's `/narration/last` relay works.

## 3. Bring up the governed loop

Per `docs/hardware/R2_LIVE_LOOP_BRINGUP.md`:
- **Stage-1 (doer dry run, no actuation):** `./robot/stage1_doer_dryrun.sh` +
  your lidar (`ydlidar_ros2_driver`). Enough for KITT to **see, answer, and
  route** — nothing moves.
- **Stage-2 (elevated, wheels up):** add the verifier (2a key) + interceptor +
  consumer for real motion. **Physically present.**

Health check: `curl -s localhost:{8090,8100,8101,8102}/health` and
`curl -s localhost:11434/api/tags`.

## 4. Talk to KITT

Pick your entry point.

### A. Type to it (no audio needed — prove the brain first)
```bash
./robot/kitt_converse.py
> what do you see?
> are we OK?
> creep forward one meter          # ← a directive: goes to the fenced door
```

### B. One-shot questions
```bash
./robot/kitt_ask.py "why did we stop?"
```

### C. Voice — keyboard trigger (press Enter to talk)
```bash
./robot/kitt_voice.sh                 # Enter → record → whisper → kitt_converse → speak
```

### D. Voice — GPIO push-to-talk button
```bash
python3 robot/ptt_button.py | ./robot/kitt_voice.sh
```

### E. Command-only voice (Stage 0, straight to the door — no dialogue)
```bash
./robot/run_voice_ptt.sh              # ptt_button | speech_shell → POST /intent
```

## 5. Proactive voice (KITT talks first)

In its own terminal, alongside any of the above:
```bash
./robot/kitt_watch.py                 # speaks on posture / checker-deny transitions
```
Put an obstacle in front → the checker refuses → KITT: *"I've had to refuse a
command. …"*. Force a lockout → *"I'm locking out and holding a safe stop."*

## 6. The KITT experience (all together)

Three terminals + the loop:
1. the loop (§3) + lidar
2. `./robot/kitt_watch.py`            (proactive)
3. `python3 robot/ptt_button.py | ./robot/kitt_voice.sh`   (converse by voice)

Press the button, ask *"what's ahead?"* → it looks and answers. Say *"take us
that way"* → it routes the directive through the door; the checker bounds it;
KITT confirms. Approach an obstacle → it warns, unprompted. That's KITT.

---

## Swapping the LLM (a newer/better model)

The model is behind an HTTP seam, so a swap is **config-only** — no rebuild, and
**no safety re-review** (the checker, the fence, and the intent parse are
model-agnostic; a swap changes only the doer's proposals, never its authority):

```bash
ollama pull <new-model>
# gate the candidate against the router contract BEFORE flipping the env:
python3 robot/kitt_model_smoketest.py <new-model>     # JSON contract + directive vs chat + no fabrication
# then in /etc/kirra/robot.env:
KIRRA_KITT_MODEL="<new-model>"
sudo systemctl restart kirra-kitt-watch               # + the converse/voice front-end
```

`kitt_model_smoketest.py` is a **doer-quality** gate (not a safety gate, not a CI
test — it needs a live Ollama). A model that fails it is still *safe* to run — an
unparseable turn fails closed to SPEAK-only — but the drive-by-voice path may not
fire reliably, so prefer one that passes. Also measure end-to-end latency
(`KITT_AUDIO_STACK.md` §5): a bigger model is slower and needs more VRAM.

**"No version bump" stealth updates.** A model can change *in place* — same
Ollama tag, different weights (e.g. the July 2026 Gemma-4 overhaul: tool-calling
patches + reduced truncation, no version label change). So the rule is **re-run
the smoketest after any `ollama pull`, not just on a version string change** — the
tag can lie. On a full pass the smoketest records the vetted **digest** *and the
vetted-at timestamp* (`~/.kirra_kitt_model.pin`) — the "which weights, verified
when" trail researchers recommend logging for reproducibility; add provenance
with `--note`:
```bash
python3 robot/kitt_model_smoketest.py --note "hf re-pull 2026-07-16"   # test + pin with a dated note
python3 robot/kitt_model_smoketest.py --pin-check                       # running digest vs pin (+ vetted-at), no LLM
```
Boot compares the running digest to the pin and, on a mismatch, KITT says so
(voice line A5) — a stealth swap is loud, not silent. The robot itself is not
changed by an upstream update until someone re-pulls (Ollama runs the local
blob); the pin catches the re-pull that silently brought new weights. (For KITT
specifically the tool-calling/truncation fixes are *welcome* — they harden the
`{say, directive}` JSON contract; the smoketest confirms it. The FA4/vision gains
don't reach the Orin — Ampere, text-only router.)

### Update commands (copy-paste)

**A — re-pull the same tag** (stealth in-place update, e.g. the Gemma-4 case):
```bash
cd ~/kirra-runtime-sdk
ollama pull gemma3:4b                                              # new weights, same tag
python3 robot/kitt_model_smoketest.py gemma3:4b --note "re-pull $(date -I)"   # RE-VET + re-pin
sudo systemctl restart kirra-kitt-watch kirra-kitt-greet          # load them
```
No `robot.env` edit — the tag is unchanged. Boot speaks warning A5 if a re-pull
ever slipped in unvetted.

**B — switch to a different / bigger model:**
```bash
cd ~/kirra-runtime-sdk
ollama pull gemma4:8b
python3 robot/kitt_model_smoketest.py gemma4:8b --note "eval $(date -I)"      # gate the candidate
sudo sed -i 's/^KIRRA_KITT_MODEL=.*/KIRRA_KITT_MODEL="gemma4:8b"/' /etc/kirra/robot.env
sudo systemctl restart kirra-kitt-watch kirra-kitt-greet
```
Then measure end-to-end latency (`KITT_AUDIO_STACK.md` §5) — bigger = slower/more VRAM.

**C — check anytime (no LLM call, no change):**
```bash
python3 robot/kitt_model_smoketest.py --pin-check
#   OK → running digest == vetted pin | CHANGED → stealth update, re-run A | UNPINNED → vet once
```
Needs Ollama up (`ollama serve`) + `python3-requests`. Doer-quality gate only —
the checker is untouched by any model change. The unattended OTA timer checks for
a governed *software* update (`kirra-ota-ctl pull`); it never touches LLM weights
— updating Gemma is always this deliberate pull + re-vet.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| KITT prints but never speaks | `KIRRA_TTS_CMD` unset / speak.sh broken | set it; test `echo hi \| ./speak.sh` |
| "my voice module is offline" | Ollama down / model not pulled | `ollama serve`; `ollama pull gemma3:4b` |
| "why did we stop" → "unavailable" | mick has no auditor token | set `KIRRA_MICK_AUDITOR_TOKEN`, restart mick |
| "what do you see" → "perception unavailable" | no ROS / no `/scan` | source ROS; bring up the lidar |
| voice records silence / garbage | wrong ALSA device / gain | check `arecord -l`; set the card in `KIRRA_RECORD_CMD` |
| button does nothing | Jetson.GPIO / wrong pin | `pip3 install Jetson.GPIO` + gpio group; set `KIRRA_PTT_GPIO_PIN` |
| a spoken command didn't drive | checker refused (correct!) or door 422 | ask "why did we stop"; check the loop + verifier |

## References
- `docs/hardware/KITT_AUDIO_STACK.md` — engine choices + tuning + the systemd-audio fix
- `docs/hardware/KITT_CONVERSATION_DESIGN.md` — the architecture + the safety fence
- `docs/hardware/R2_LIVE_LOOP_BRINGUP.md` — the governed loop (Stages 1–2)
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` — off-network + the hardware e-stop
- `docs/testing/SPEECH_KITT_DEMO.md` — the built STT→intent→TTS loop + engines
- `robot/install/kitt.env.example` — the single env template
