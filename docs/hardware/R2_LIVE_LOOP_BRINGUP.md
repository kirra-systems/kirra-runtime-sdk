# R2 live-loop bring-up — Mick (LLM) + Taj (perception) → checker → drive

> **Status: integration playbook.** Every stage below is REAL, wired code (mapped
> file-by-file). What has NOT happened yet is running the *full* loop end-to-end
> on this robot — only the motor consumer has run, against the dev publisher
> stand-in. This is the ordered procedure to stand the whole loop up, with the
> two integration gotchas that will otherwise cost you hours.

## The pipeline

```
  text goal ─► mick_service ─► /intent/last ┐
                (LLM, Ollama)                │  occy_doer polls the intent as its goal
  lidar /scan ─► taj_service (corridor+cap) ─┼─► planner_service /plan
                                             │     (Occy grounds intent, KIRRA
                                             ▼      slow-loop checker BOUNDS it)
                                        /cmd_vel_raw
                                             │
                        cmd_vel_interceptor  │  Taj cap → POST verifier
                                             ▼  /actuator/motion/command
                                     verifier MINTS a signed release
                                             │  (interceptor RELAYS the bytes)
                                             ▼
                                       /kirra/release  (128-byte signed frame)
                                             │
                     kirra_motor_consumer ───┤  Rust FFI Ed25519 verify-before-release
                     (R2 closed-loop drive)  ▼
                                          wheels
```

**The safety invariant that makes this legal:** Mick (the LLM) only *proposes a
typed intent* — it is structurally fenced (`ci/check_mick_actuation_fence.py`)
from ever reaching a publisher, serial seam, or the token mint. Occy proposes a
trajectory; the **checker bounds it**; the **verifier** is the sole minter of the
release; the **consumer** verifies the exact bytes. An LLM can ask to drive
anywhere — it cannot make the wheels do anything the checker didn't authorize.

---

## 🔴 Two integration gotchas — read before you start

### 1. The verifier must sign with the SAME key the consumer pins
The R2 consumer (`run_consumer_r2.sh`) pins `KIRRA_GOVERNOR_VK_HEX = pubkey(0x2a×32)`.
The verifier's built-in **`dev-fixed` source signs with `0x07×32`** — a DIFFERENT
key. Run `dev-fixed` and **every release is `SignatureInvalid` at the consumer →
permanent safe stop + a latched key alarm.** Fix: run the verifier with a `file:`
source holding the 2a seed (Stage 0 below writes it).

### 2. Interceptor "wheelbase" is the CLASS contract wheelbase, NOT the R2's 0.229 m
The interceptor cross-checks its `wheelbase_m` param against the wheelbase the
verifier reports it used, and the check is **exact** (`abs(param − reported) ≤
1e-6`, `enforcement_decision.py:WHEELBASE_TOLERANCE_M`). A mismatch **latches a
permanent stop**. That reported value is the *active-class contract* wheelbase
(courier **0.5 m**, delivery-av **1.9 m** — `src/gateway/contract_profiles.rs`),
a different quantity from the motor consumer's physical
`KIRRA_R2_WHEELBASE_M=0.229` (its Ackermann last-hop). Set the interceptor's
`wheelbase_m` to the exact CLASS value.

⚠️ **The shipped config is wrong for courier:** `kirra_params.yaml` currently has
`wheelbase_m: 0.2`. Under `KIRRA_VEHICLE_CLASS=courier` the verifier reports 0.5,
so 0.2 ≠ 0.5 → instant latched stop. Change it to `0.5` (courier) — or pick the
class whose contract wheelbase you set.

> **Caveat, be honest about it:** the R2 has no vehicle class of its own — it is
> smaller than `courier`. Running under `courier` means the *checker envelope*
> (speed/accel/steering bounds) is courier-sized, looser than a 0.229 m robot
> strictly needs. For a tethered low-speed demo bounded by `KIRRA_DEMO_VX_MAX`
> that is acceptable; a production R2 wants its own contract profile
> (`PLATFORM_R2_PENDING`). Pick `courier` (closest of the three).

---

## Must-match across services

| Thing | Where it's set | Must equal |
|---|---|---|
| Governor key | verifier `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=file:<2a seed>` | consumer `KIRRA_GOVERNOR_VK_HEX` (= pubkey 0x2a) |
| `KIRRA_ADMIN_TOKEN` | verifier (required to boot) | interceptor `kirra_token` Bearer |
| Vehicle class | verifier `KIRRA_VEHICLE_CLASS=courier` | fixes the contract wheelbase below |
| Interceptor `wheelbase_m` | `ros2_ws/.../config/kirra_params.yaml` | courier contract **0.5** (NOT 0.229) |
| `ROS_DOMAIN_ID` | everywhere (consumer forces **28**) | 28 on the launch/robot side too |
| Ports | verifier 8090 · planner 8100 · taj 8101 · mick 8102 · ollama 11434 | interceptor `kirra_url`, `planner_url`, `taj_url` match |
| `release_topic` | interceptor `/kirra/release` | consumer `KIRRA_RELEASE_TOPIC` (default `/kirra/release`) |

---

## Stage 0 — governor key (once)

Make the verifier's mint key match the consumer's pin:
```bash
cd ~/kirra-runtime-sdk
./robot/install/make_gov_seed.sh                       # → /etc/kirra/gov_2a.seed (0600)
# (sudo if /etc/kirra isn't writable, or pass a path you own:
#  ./robot/install/make_gov_seed.sh ~/gov_2a.seed )
```

---

## Stage 1 — doer-only dry run (NO actuation — safe over SSH)

Prove Mick+Taj+Occy produce sane `/cmd_vel_raw` **without** the verifier, interceptor,
or consumer — nothing can move because the release path isn't up. Good first
integration check while remote.

**Terminals (each: `cd ~/kirra-runtime-sdk`, `source /opt/ros/humble/setup.bash`, `export ROS_DOMAIN_ID=28`):**

1. **Ollama + model** (the LLM):
   ```bash
   ollama serve &                 # if not already running
   ollama pull gemma3:4b          # once; matches KIRRA_MICK_MODEL default class
   ```
2. **mick_service** (text → intent):
   ```bash
   cargo run -p kirra-sidecars --bin mick_service --release
   # listens 127.0.0.1:8102 ; needs Ollama at http://localhost:11434
   curl -s localhost:8102/health
   ```
3. **Launch the doer stack** (starts taj + planner sidecars, occy_doer, perception_governor):
   ```bash
   ros2 launch kirra_safety kirra_with_robot.launch.py use_occy_doer:=true use_perception_cap:=true
   ```
4. **Lidar** publishing `/scan` (your 4ROS driver).
5. **Give it a goal, two ways:**
   - LLM: `curl -s localhost:8102/intent -XPOST -H 'content-type: application/json' -d '{"text":"creep forward to the doorway"}'` then `curl -s localhost:8102/intent/last`
   - or a pose: `ros2 topic pub -1 /goal_pose geometry_msgs/msg/PoseStamped '{header:{frame_id: odom}, pose:{position:{x: 1.0}, orientation:{w: 1.0}}}'`
6. **Observe the proposal** (no motion — the consumer/verifier aren't up):
   ```bash
   ros2 topic echo /cmd_vel_raw            # Occy's bounded proposal
   ros2 topic echo /kirra/perception_health
   ```
   Put an obstacle in front of the lidar → `/cmd_vel_raw` should collapse to ~0 (Taj cap). That's the perception-driven refusal, proven with zero actuation risk.

---

## Stage 2 — full loop, ELEVATED (🔴 physically present, wheels up, e-stop in hand)

Add the verifier + interceptor + consumer so releases actually flow. **Do NOT do
this over SSH-only** — you need eyes on the robot and a hand on the e-stop.

7. **Verifier** (mints; 2a key so the consumer accepts it):
   ```bash
   export KIRRA_ADMIN_TOKEN=<pick-a-token>
   export KIRRA_VEHICLE_CLASS=courier
   export KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=file:/etc/kirra/gov_2a.seed
   cargo run --bin kirra_verifier_service --release      # listens 0.0.0.0:8090
   curl -s localhost:8090/health
   ```
8. **Interceptor wheelbase** — set the courier contract value in
   `ros2_ws/src/kirra_safety/config/kirra_params.yaml`: `wheelbase_m: 0.5`
   (NOT 0.229 — see gotcha #2). Re-launch the stack from Stage 1 **with the token**:
   ```bash
   export KIRRA_ADMIN_TOKEN=<same token as the verifier>
   ros2 launch kirra_safety kirra_with_robot.launch.py \
        use_occy_doer:=true use_perception_cap:=true kirra_url:=http://localhost:8090
   ```
9. **Consumer** — the R2 closed-loop drive, pinned to the 2a key (matches the verifier):
   ```bash
   export KIRRA_R2_CLOSED_LOOP=1 KIRRA_R2_CLOSED_LOOP_DEBUG=1 \
          KIRRA_R2_SPEED_KP=40 KIRRA_R2_SPEED_EMA_ALPHA=0.3
   ./robot/run_consumer_r2.sh
   # log must show: car-type 5 read back + "CLOSED-LOOP speed matching"
   ```
10. **Run the guided acceptance** — `robot/live_loop_elevated.sh` walks the four
    wheels-up phases: (a) nominal governed motion, (b) obstacle → Taj cap → STOP,
    (c) dead lidar → hold, (d) dead verifier → consumer decel-to-zero. Answer its
    prompts; the `cl …` debug lines from the consumer show the closed-loop numbers.

Only after Stage 2 passes elevated does the tethered floor run make sense (same
`live_loop_elevated.sh` chain, wheels down, short tether).

---

## Health checks (each service exposes one)

```bash
curl -s localhost:8090/health   # verifier
curl -s localhost:8100/health   # planner (Occy)
curl -s localhost:8101/health   # taj
curl -s localhost:8102/health   # mick
curl -s localhost:11434/api/tags # ollama (model list)
```

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Consumer logs `REFUSED (SIGNATURE_INVALID)` / key alarm latches | verifier not signing with the 2a seed (probably `dev-fixed`=0x07) | Stage 0 + `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=file:/etc/kirra/gov_2a.seed` |
| Interceptor latches every command to stop | `wheelbase_m` param ≠ verifier class contract wheelbase | set `wheelbase_m: 0.5` (courier) in `kirra_params.yaml` |
| Consumer starves → decel-to-stop, no releases | verifier not minting (no signing-key source) OR interceptor got 401 | set the signing source; ensure `KIRRA_ADMIN_TOKEN` matches on verifier + interceptor |
| `/cmd_vel_raw` always ~0 | no goal, or stale `/scan`, or Taj unhealthy (cap 0) | publish a goal/intent; check the lidar; `curl localhost:8101/health` |
| mick `/intent` returns 422 | Ollama not up / model not pulled | `ollama serve` + `ollama pull gemma3:4b` |
| `/kirra/release` and `/cmd_vel` don't cross | ROS_DOMAIN_ID mismatch | force **28** everywhere (consumer already does) |

## References
- `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md` — the R2 drive last-hop (the wheels end)
- `robot/live_loop_elevated.sh` — the guided elevated acceptance for the full chain
- `docs/safety/GOVERNOR_KEY_PROVISIONING.md` — signing-key sources (why dev-fixed ≠ 2a)
- `ci/check_mick_actuation_fence.py` — the Mick actuation fence (why the LLM can't drive directly)
- `ros2_ws/src/kirra_safety/kirra_with_robot.launch.py` — the launch stack
