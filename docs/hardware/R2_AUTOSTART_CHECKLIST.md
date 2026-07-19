# R2 autostart — the boot-service enablement checklist

> How the R2 comes up on power-on with no laptop (the untethered Layer 1,
> `R2_UNTETHERED_BRINGUP.md` §1). The unit files exist; this is the ordered,
> **validate-then-enable** procedure. The rule throughout: **enable a service
> only after it passes its acceptance wheels-up as a service** — a boot stack
> that was only ever validated by hand is an unproven configuration.
>
> 🔴 **Cannot fail open.** Every unit is fail-closed: a service that doesn't come
> up leaves the robot **stationary**, never running away. A missing verifier →
> no releases minted → the consumer decel-to-stops (`503 → 0.0`, SS-002). Boot
> order below is for *smoothness*, not safety.

## The full unit inventory

| Unit | What | User | Source |
|---|---|---|---|
| `kirra.target` (verifier + planner + taj + mick) | governor stack (mint + sidecars) | `kirra` (service acct, sandboxed) | `deploy/systemd/` (via `install.sh`) |
| `kirra-consumer.service` | verifying motor consumer (ADR-0033 chokepoint) | robot user (`dialout`) | `robot/install/` (staged by `install_kirra.sh`) |
| `kirra-ros-stack.service` | occy_doer + cmd_vel_interceptor + perception_governor | robot user (ROS + ws) | `robot/install/systemd/` |
| `kirra-kitt-watch.service` | proactive event speech (Channel A) | robot user (`audio`) | `robot/install/systemd/` |
| *(external)* `ollama.service` | the local LLM (mick's brain) | its own | the Ollama installer |
| *(external)* lidar (`ydlidar_ros2_driver`) | `/scan` | robot user | vendor / your launch |

`kitt_voice.sh` / `speech_shell` / `ptt_button.py` are **interactive** (a person
talks) — they are NOT boot services; run them in a session when you want to talk.

## Boot / dependency order

```
  network-online.target
        │
        ├─► ollama.service ─────────────┐  (mick needs it; else /intent 422s, fail-closed)
        │                               ▼
        └─► kirra.target ──────────────► verifier(8090) + planner(8100) + taj(8101) + mick(8102)
                    │
                    ├─► kirra-ros-stack ─► occy_doer + interceptor  (holds if a sidecar is down)
                    ├─► kirra-consumer ──► wheels (decel-to-stops if no releases)
                    └─► kirra-kitt-watch ► narration (silent if a source is down)
        (external)  lidar ─► /scan       (occy holds 'stale-scan' without it)
```

None of these is a HARD requirement of another — every consumer of a missing
producer **fail-soft holds**. Order is `After=` (best-effort), never `Requires=`.

## Install the units

```bash
# governor stack (service account, sandboxed) — one command:
sudo deploy/systemd/install.sh                      # installs + enables kirra.target

# robot-side units (rendered to YOUR user; needs the placeholders filled):
#   __KIRRA_ROBOT_USER__  → your login user (in dialout + audio groups)
#   copy binaries/scripts to /opt/kirra (install_kirra.sh does the consumer bits)
sudo install_kirra.sh                               # stages kirra-consumer (not enabled)
# render + install the ros-stack + kitt-watch units the same way (fill User=,
# set KIRRA_ROS_WS_SETUP in /etc/kirra/robot.env), then daemon-reload.
sudo systemctl daemon-reload
```

## Enable — ONE service at a time, validate first

For **each** unit: start it, confirm it does its job wheels-up, THEN enable for boot.

```bash
# 1. governor stack (no wheels involved) — kirra.target install.sh already enabled it.
systemctl status kirra.target
curl -s localhost:8090/health   # verifier

# 2. consumer — wheels UP, e-stop in hand. Prove it holds, drives a governed
#    command, and safe-stops on a killed verifier BEFORE enabling.
sudo systemctl start kirra-consumer && journalctl -u kirra-consumer -f
#    …acceptance passes…            → sudo systemctl enable kirra-consumer

# 3. ros-stack — prove occy_doer holds/plans, interceptor relays.
sudo systemctl start kirra-ros-stack && journalctl -u kirra-ros-stack -f
#    …acceptance passes…            → sudo systemctl enable kirra-ros-stack

# 4. kitt-watch — confirm it SPEAKS on a posture/deny event (audio-in-service
#    caveat: needs an ALSA-direct KIRRA_TTS_CMD + the audio group).
sudo systemctl start kirra-kitt-watch && journalctl -u kirra-kitt-watch -f
#    …speaks on an event…           → sudo systemctl enable kirra-kitt-watch
```

## The cold-boot test (the acceptance for Layer 1)

Only after all four are enabled AND individually validated:

1. **Hardware e-stop in hand** (`R2_ESTOP_SPEC.md`), wheels-up first.
2. Power the robot with **no laptop attached**.
3. Confirm the stack comes up: `systemctl is-active kirra.target kirra-consumer
   kirra-ros-stack kirra-kitt-watch` → all `active`; `curl localhost:8090/health`.
4. Give a goal (voice via a session, or a pre-loaded mission) → governed motion.
5. **Fail-closed drill:** `systemctl stop kirra-verifier` mid-run → the consumer
   decel-to-stops (no runaway). Restart → recovers.

Record results in the bring-up log. Then, and only then, is unattended cold-boot
operation in scope.

## Status (honest)

- ✅ Governor stack units + one-command install (`deploy/systemd/`).
- ✅ Consumer unit staged (`install_kirra.sh`).
- ✅ **ROS-stack + KITT-watch units authored** (`robot/install/systemd/`, this
  change) — `start_sidecars:=false` so they don't double-bind `kirra.target`'s.
- ⬜ **Per-service wheels-up validation + `enable`** — hardware, not yet done.
- ⬜ **Audio-in-a-system-service** proven for `kirra-kitt-watch` (ALSA-direct) —
  until then, run the narrator in a login session.
- ⬜ An installer step that renders the two new units' `User=` +
  `KIRRA_ROS_WS_SETUP` (today: fill the placeholders by hand).

## Bench tooling (automates the steps above)

Three scripts turn the manual checklist into runnable commands:

- **`robot/install/lint_robot_env.sh`** — validate `/etc/kirra/robot.env` for the
  two systemd-`EnvironmentFile` traps: inline `# comments` on value lines (systemd
  keeps them IN the value → the consumer fail-closes) and duplicate keys (last
  wins — a trap on reorder/delete). `--fix` normalizes to one bare `KEY=VALUE` per
  key (backs up first). Run after any hand-edit of `robot.env`.
- **`robot/install/disable_vendor_autostart.sh`** — find whatever autostarts the
  Yahboom vendor base node (systemd unit / cron `@reboot` / `rc.local` /
  autostart) — it opens `/dev/myserial` and fights the consumer. Report-only by
  default; `--disable` acts on confident hits (never touches `kirra-*`). **Must be
  clean before the cold-boot drill.**
- **`robot/cold_boot_drill.sh`** — after a physical power cycle: Phase A verifies
  every KIRRA service is up, posture is readable, a node is Trusted, the board is
  KIRRA-owned, and no vendor node runs (read-only). `--fail-closed` runs Phase B:
  stop the verifier → prove the consumer decel-stops (SS-002) + the interceptor
  denies → restart → prove recovery. 🔴 wheels elevated for Phase B.

Order at the bench: `lint_robot_env.sh` → `disable_vendor_autostart.sh --disable`
→ power-cycle → `cold_boot_drill.sh` → (wheels up) `cold_boot_drill.sh --fail-closed`.

## References
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` §1 — the autostart layer
- `docs/hardware/R2_ESTOP_SPEC.md` — the hardware e-stop (do first)
- `deploy/systemd/README.md` + `install.sh` — the governor stack lifecycle
- `robot/install/install_kirra.sh` — the consumer staging
