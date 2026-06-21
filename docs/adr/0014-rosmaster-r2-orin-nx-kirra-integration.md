# ADR-0014: Rosmaster R2 + Jetson Orin NX 16GB — dual-system stack governed by KIRRA (planner proposes, governor disposes)

| Field | Value |
|---|---|
| Status | **Proposed (integration design note)** — for owner sign-off; ratified on merge. |
| Date | 2026-06-21 |
| Deciders | Project / safety-case owner |
| Hardware | Rosmaster R2 (Ackermann-steer chassis, onboard ROS expansion board + IMU, Astra Pro depth camera, lidar) · Jetson Orin NX 16GB (~100 TOPS, 16 GB unified, 10–25 W) |
| Stack | Perception (Parko) · Planner (Occy / LLM) · KIRRA governor + verifier + console |
| Cross-refs | ADR-0006 (QM↔safety boundary), ADR-0013 (request-not-command E-stop), #126 / #127 (perception / actuation SEooC AoU), #131 (Option-B trajectory validation), #49 / #171 (cmd_vel robot lane), Parko (`parko-core` vendor-neutral inference), Occy (`kirra-planner`) |

## Context

The target build is a self-contained governed robot: a **Rosmaster R2** (Ackermann chassis —
real-car kinematics, not a balance-bot) on a **Jetson Orin NX 16GB**, running a **perception
layer, a planner, and KIRRA**, organized as the "Fast vs. Slow" dual-system architecture.

That dual-system framing maps almost 1:1 onto KIRRA's existing **doer / checker** split:

- **System 2 (slow, QM-domain, untrusted):** perception + planning + LLM reasoning — the
  *doer*. Proposes intent.
- **System 1 (fast, real-time):** motor/servo driver, IMU, the hardwired E-stop.
- **KIRRA (the boundary between them):** the fail-closed *checker* — the only authority that
  may pass a command to System 1.

## Decision

Adopt the dual-system stack with **KIRRA as the safety boundary**, under one non-negotiable
rule that defines the whole integration:

> **The planner/LLM PROPOSES a typed claim; the KIRRA governor DISPOSES; only a validated,
> clamped command reaches the actuator. The LLM never touches the chassis.**

Concretely, the proposal path is **not** an `exec()` / dynamic-eval of LLM output. It is:

```
LLM / planner output
  → action_policy::UnstructuredTextParser   (LLM JSON → a TYPED AgentAction; no code-eval)
  → action_filter::evaluate_action_claim     (claim vs fleet/node posture)
  → gateway::kinematics_contract::validate_vehicle_command
                                              (Ackermann bicycle-model clamp:
                                               speed / accel / steering / lateral-accel)
  → [perception present?] RSS + SG2 containment
  → verdict: Allow | Clamp | Deny → MRC decel-to-stop·hold
  → /cmd_vel  (only what survives)
```

### Corrected architecture

```
 SYSTEM 2 — QM / untrusted (Orin NX, slow)
   Perception (Parko: Astra depth + camera → detector via TensorRT backend)
   Planner    (Occy, or the LLM-as-planner)      → PROPOSES typed ActionClaim / trajectory
 ───────────────────────────────────────────────────────────────────────────
   ▼  (typed claim — NEVER exec())
 ╔═════════════════════════════════════════════════════════════════════════╗
 ║  KIRRA GOVERNOR — fail-closed, WCET-bounded safety boundary              ║
 ║  action_filter (claim vs posture) → kinematics_contract (Ackermann clamp)║
 ║  → RSS / containment (if perception present) → verdict                   ║
 ╚═════════════════════════════════════════════════════════════════════════╝
   ▼  (only validated, clamped cmd_vel passes)
 SYSTEM 1 — real-time (50 Hz+): motor / servo driver, IMU read,
            *hardware* E-stop (the certifiable stop; ADR-0013)
```

KIRRA is **not a third box bolted on** — it *is* the "bridge" between the two systems, now a
fail-closed trust boundary. It also spans up to the verifier + console (governance plane), so
real state flows out.

## Component mapping (target stack → existing KIRRA/Parko/Occy)

| Stack element | Maps to |
|---|---|
| Perception layer | **Parko** (`parko-core` vendor-neutral inference; TensorRT backend on the Orin). No model yet → absent perception → KIRRA fail-closes to degraded/MRC (safe). |
| Planner | **Occy** (`kirra-planner`, early/Phase-0) **or** the LLM-as-planner. KIRRA governs either identically — the brain can swap without touching the safety case. |
| Governor / safety | **KIRRA** — `action_filter` + `action_policy` + `kinematics_contract` + RSS/containment. |
| Ackermann steering | KIRRA's `VehicleKinematicsContract` **already** uses the bicycle model (`a_lat = v²·tan(δ)/L`). Configure it with the R2's wheelbase + steering limits — KIRRA *is* the Ackermann-aware safety translator. |
| Lidar safety buffer | A **geometric** distance buffer needs **no ML model** — feed it now as an RSS / `perception_monitor` speed-derate input. ML object detection is the Parko-model phase. |
| IMU / sensor health | Feed System 2's world model **and** the verifier `/fleet/diagnostics/report` → drives posture / trust / recovery hysteresis. |
| Console live data | Register the R2 as a verifier node + report → `/console/runtime|sites|versions|fleet` flip `demo → live` (#394). |
| E-stop | **request-not-command** (ADR-0013): operator/LLM *requests* stop → governor commands MRC. Plus a hardwired physical E-stop as the certifiable one. |
| LLM "dream"/personality loop | QM cognition, **off the safety path** — KIRRA does not touch it. |

## Rejected alternative (the anti-pattern)

**LLM output `exec()`'d / dynamically evaluated onto the chassis** (the blueprint's "safe
`exec()` loop"). There is no safe `exec()` of model output onto a moving vehicle — a
hallucination, prompt injection, or malformed block then drives the car. This is precisely the
failure mode KIRRA exists to remove (*"prevent unsafe commands reaching actuators regardless of
what an LLM output instructs"*). Forbidden.

## Hardware reality (Orin NX 16GB)

- **KIRRA's footprint is ~zero** — Rust, no ROS/tokio in the governor core; it adds nothing to
  the compute/memory budget.
- **The tight resource is System 2**, not KIRRA: a **Q4 ~8B LLM (~5–6 GB) + a small TensorRT
  detector (~1–2 GB) + ROS2 + buffers** fits 16 GB unified but must be managed; a *heavy lidar
  DNN* run concurrently is where it breaks. The **cloud-bridge for slow LLM reasoning**
  (e.g. Gemini Flash) keeps local headroom for perception + the real-time path.
- **Fail-closed covers a slow/cloud brain:** if System 2 stalls or the link drops, the governor
  holds (degraded/MRC). A laggy brain is *safe*, not a crash.
- Power: Orin NX 10–25 W alongside drive motors on the R2's 12.6 V pack — use `nvpmodel`. Orthogonal to KIRRA.
- **This stack is NX-16GB-viable *because* it is Parko + Occy/LLM + KIRRA, not Autoware.** Full
  Autoware Universe would push to AGX Orin; you don't need it — KIRRA governs your own planner.

## Phasing

- **Phase 1 — perception-free, works now.** R2 `cmd_vel` + IMU/heartbeat → register as a
  verifier node → KIRRA governs `cmd_vel` (Ackermann envelope + posture + LLM-claim filtering)
  → **live data in the console.** A complete governed-robot loop with no perception model, no
  Autoware, light Jetson load. (A geometric lidar safety buffer can be added here.)
- **Phase 2 — perception.** Parko runs a detector (TensorRT) → world model → RSS / SG2
  object-aware goals go live (SG2 live-wiring tracked at #128).
- **Planner evolution.** LLM-as-planner first (governed), Occy as it matures — identical
  governance either way.

## Status

**Proposed — for owner sign-off** (merge ratifies, as with ADR-0011 / 0012 / 0013). Records the
**propose→govern→actuate** integration before wiring, so the build can't drift into the
LLM→`exec()`→actuator anti-pattern. End-to-end validation is hardware-gated (R2 + Orin NX).
