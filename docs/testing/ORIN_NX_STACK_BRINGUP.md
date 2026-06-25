# Mick · Taj · Occy · KIRRA on a Jetson Orin NX 16GB

The full Kirra **System-2** stack runs natively on a Jetson Orin NX 16GB — the deployment
target of **[ADR-0014](../adr/0014-rosmaster-r2-orin-nx-kirra-integration.md)**. This is the
bring-up recipe: build the four components for aarch64, prove the doer-checker loop headless,
then (optionally) bring up the governance plane so a ROS 2 / Python client can drive real egos.

> **The Orin is the robot's brain, not a sim host.** CARLA's photoreal server is x86 + desktop
> GPU only — it does *not* run on the Orin. You don't need it: the stack below is pure Rust
> compute (no GPU, no CARLA, no ROS required to run the demo), so it executes on the Orin
> exactly as it would on the vehicle. For visuals on the Orin use Gazebo/Ignition (ARM-native,
> wired in `ros2_ws/src/kirra_safety`); for CARLA cities put CARLA in the cloud with the Orin
> as the client.

## The four components

| Component | Crate | Role (ADR-0014 / ADR-0015) |
|---|---|---|
| **Taj** | `kirra-taj` | Perception — R2 lidar/depth → the Kirra perception contract (corridor + objects + health). Phase A is geometric/model-free; Phase B is the Parko TensorRT detector. |
| **Mick** | `kirra_planner::mick` | The intent seam — an LLM authors a TYPED `MickIntent` as JSON, parsed **fail-closed** (`from_llm_json`). The local model (Gemma via Ollama) lives in `kirra-mick`. |
| **Occy** | `kirra-planner` | The DOER — grounds Mick's intent against Taj's perception into a proposed trajectory (`plan_for_intent`). Only PROPOSES. |
| **KIRRA** | `validate_trajectory_slow` (`kirra-ros2-adapter`) + `kirra_verifier_service` | The CHECKER — the sole safety authority. Bounds Occy's proposal (RSS + containment + kinematics + posture); the verifier service is the governance/console plane. |

The one rule, end to end: **the brain PROPOSES a typed claim; KIRRA DISPOSES; only a validated,
clamped command reaches the actuator.** Occy is never trusted to stop — KIRRA is.

## Hardware reality (Orin NX 16GB)

- **KIRRA's footprint is ~zero** — Rust, no ROS/tokio in the governor core.
- **System 2 is the tight budget**, not KIRRA: a small instruct LLM (Gemma 3 4B / Llama 3.2 3B,
  Q4 via TensorRT-LLM) + a small TensorRT detector + ROS 2 + buffers fit 16 GB unified but must
  be managed. A heavy lidar DNN run concurrently is where it breaks.
- **Fail-closed covers a slow/cloud brain**: if System 2 stalls or a cloud link drops, the
  governor HOLDs (Degraded/MRC). A laggy brain costs *availability*, never *safety*.
- Power: 10–25 W; set a mode with `nvpmodel`.

## 0. Prerequisites on the Orin

```bash
# Rust toolchain (native aarch64 — no cross-compile needed when building ON the Orin)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt-get install -y build-essential pkg-config   # cc/linker for the bundled SQLite etc.
git clone https://github.com/kirra-systems/kirra-runtime-sdk && cd kirra-runtime-sdk
```

## 1. One-command bring-up

```bash
scripts/orin_bringup.sh            # build the 4 components + run the headless stack demo
scripts/orin_bringup.sh --serve    # ALSO start verifier (:8090) + Occy planner (:8100) + Taj (:8101)
```

The script sanity-checks the toolchain, detects the Jetson, builds release binaries, and runs
the headless four-component demo. Expected output:

```
Taj(perception) → Mick(intent) → Occy(doer) → KIRRA(checker) — on-Orin stack (ADR-0014), headless

  scenario                            objs  occy      kirra         min_y  result
  --------------------------------------------------------------------------------------
  clear corridor                         2  Motion    Clamp          0.00  drives (KIRRA admits)
  stopped car ahead                      3  SafeStop  Accept         0.00  controlled stop behind it (admitted)
  off-centre obstacle, wide road         3  Motion    Clamp         -1.50  routes around (admitted)
  LockedOut posture                      2  SafeStop  MRCFallback    0.00  KIRRA refuses all motion (MRC)
```

Each row is one full pass of Taj→Mick→Occy→KIRRA. The load-bearing property: across every
regime Occy only PROPOSES; KIRRA admits or refuses. In **LockedOut**, KIRRA refuses all motion
*even though Occy proposed only a safe-stop* — the fault flows through the whole stack.

> Run it by hand any time: `cargo run --release -p kirra-mick --example taj_occy_kirra_stack`.
> The same loop fully in Rust against a route graph (no Taj) is the `drive_session` example.

## 2. Drive real egos through the live governor

With `--serve` the **governance plane** (the verifier on `:8090`), the **Occy planner
endpoint** (`POST /plan` on `:8100`), and the **Taj perception sidecar** (`POST /perception`
on `:8101` — the cmd_vel speed cap) are up, each health-checked before the banner prints.
Point a client at them:

- **On the robot** — the `ros2_ws/src/kirra_safety` launch wires the R2's `cmd_vel` through the
  governor (Ackermann envelope + posture + LLM-claim filtering). This is ADR-0014 **Phase 1**:
  a complete governed-robot loop, light Jetson load. **Taj's geometric corridor is now wired into
  this path**: the `perception_governor` node turns `/scan` into an assured-clear-distance speed
  cap (via the `taj_service` sidecar) that derates `cmd_vel` before the governor — opt-in,
  fail-closed (`use_perception_cap:=true`; see `ros2_ws/src/kirra_safety/README.md`). The Parko
  TensorRT detector (semantic objects) is **Phase 2**.
- **Headless / desktop** — `scripts/governor_drive_session.py` drives a kinematic ego through the
  real governor and captures the divergence (and a `kirra-collector` dataset; see
  [DRIVE_SESSION_SETUP.md](DRIVE_SESSION_SETUP.md)).
- **Occy as the doer** — `scripts/carla_drive_session.py --occy http://127.0.0.1:8100` drives egos
  with the actual planner via the endpoint.

```bash
KIRRA_ADMIN_TOKEN=… KIRRA_SUPERVISOR_RESET_KEY=… scripts/orin_bringup.sh --serve
```

(`KIRRA_ADMIN_TOKEN` and `KIRRA_SUPERVISOR_RESET_KEY` are required and fail-closed — absent/empty
→ admin routes 503. See the root `CLAUDE.md` env table.)

## 3. Topology — single-box vs two-box

- **Single-box (this script, Phase 1).** Everything on the Orin; freedom-from-interference rests
  on partition/hypervisor isolation (the #270 / #278 QNX path).
- **Two-box (stronger separation, recommended for the safety demo).** System 2 (Taj + Mick + Occy)
  on the Orin; the **KIRRA governor on a separate small board** (a Raspberry Pi is the better
  governor box — zero GPU needed, lower power, GPIO for the E-stop audit), linked by the
  `kirra-governor-service` UDP wire. A memory/GPU/thermal fault in System 2 then *physically*
  cannot starve the governor.

See [ADR-0014](../adr/0014-rosmaster-r2-orin-nx-kirra-integration.md) §"Compute topology" for the
governor-box trade-off and the cert-correctness caveat (the certifiable governor target remains
QNX on a safety MCU; the hardwired E-stop is independent of compute, ADR-0013).

## What's gated (not built by the demo)

- **ROS 2 node** (`kirra-ros2-adapter` `node.rs`) — needs a sourced ROS 2 toolchain (`r2r`),
  built only with `--features ros2`. The bring-up demo deliberately avoids it so it runs with no
  ROS install.
- **Parko TensorRT detector** (Phase 2 perception) — hardware/CI-gated; Phase A Taj is model-free
  and runs today.
