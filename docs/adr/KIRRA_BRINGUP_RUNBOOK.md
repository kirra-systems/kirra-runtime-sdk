# KIRRA Bring-Up Runbook — Two-Box Governed-Car Prototype

**Date:** 2026-06-08
**Status:** Active build plan
**Goal:** Get to a physical, demonstrable governed robot — a car whose actuator commands are gated by the KIRRA governor, with a deliberately bad command visibly clamped or safe-stated. This is the pilot demonstrator the strategy doc flags as the #1 value lever.

This is the **prototype** stage (QM, not the cert build). It proves the logic, the channel, and the gating end-to-end on available hardware. The cert-target factoring (Ferrocene, `no_std`, ASIL-D, DRIVE Orin, single-SoC hypervisor) comes after the logic is proven — see ADR-0001 and the platform strategy doc.

---

## Hardware roles

| Hardware | Role | OS / stack |
|---|---|---|
| **x86 PC + NVIDIA GPU** | QNX dev host **and** the checker box | Ubuntu (QNX SDP dev host) → QNX target for the governor |
| **Jetson Orin NX ROS2 car** | The doer + embodied demonstrator | Native Ubuntu / ROS 2 Jazzy + Occy + perception + Parko |
| **SSD** | Bench storage | Clean Ubuntu dev install on the PC (or NVMe on the car if it's storage-starved) |
| **Link** | Doer ↔ checker channel | WiFi/Ethernet, UDP (prototype) |

The car proposes; the governor disposes. The car's ROS 2 stack runs internally as it does today, but at the boundary a bridge node serializes the proposed command, sends it to the governor over the link, and applies the returned verdict to the actuator topic. The governor (QNX, **no ROS**) deserializes, runs the existing verdict core, and returns a verdict.

---

## The wire contract (single source of truth)

Two fixed-schema messages over UDP, one-for-one request/response:

**Proposal** (car → governor):
- a monotonic `seq` (u64) and a `timestamp` (for staleness/watchdog)
- the exact kinematic inputs the verdict core requires (current state + proposed command) — **bind these to the real signature of `kinematics_contract`, do not invent fields**

**Verdict** (governor → car):
- echoed `seq`
- the enforcement action: `Accept`, `Clamp { ...corrected fields... }`, or `SafeState`
- a deny/reason code

Transport for the prototype: length-prefixed `bincode` of a shared struct, or a hand-packed fixed layout. (For the cert path this later becomes a fixed C-ABI shared-memory mailbox on a single SoC — same semantics, different transport.)

---

## Milestones (checklist)

- [ ] **M0 — Inventory & roles.** PC, car, SSD assigned per the table above. SSD destination decided.
- [ ] **M1 — PC dev host.** Ubuntu on the PC; QNX SDP 8.0 installed (non-commercial/eval license); cross-compile a hello-world for the QNX x86-64 target and run it on a QNX target (PC booted to QNX, or QNX in a VM/QEMU for first light).
- [ ] **M2 — Governor service on QNX (bench).** The governor service binary builds for QNX, wraps the **unmodified** verdict core, and answers a test proposal over UDP with a correct verdict — driven by a fake proposal sender, no car yet.
- [ ] **M3 — Car doer.** Native Ubuntu/Jazzy + Occy + perception + Parko driving the car under the planner.
- [ ] **M4 — Car bridge node.** A ROS 2 node that intercepts the planner's command, sends a proposal to the governor, and applies the verdict to the actuator topic; plus a fault-injection hook.
- [ ] **M5 — Integration (the demo).** Car ↔ PC link live: normal driving passes through; a deliberately out-of-envelope command is clamped/safe-stated before reaching the wheels. **Film this.**
- [ ] **M6 — Hardening.** Governor watchdog + car-side deadline: a stale/missing verdict triggers safe-state. Then begin collapsing toward single-box (Tier-3 on the PC) and the cert factoring (Ferrocene / `no_std`).

---

## Claude Code — Prompt A (PC / QNX governor service)

Run in the **`kirra-runtime-sdk`** repo on the dev host.

```
Task: Add a minimal network governor service that wraps the EXISTING verdict core for an over-the-wire two-box prototype. Do NOT modify the verdict path. src/gateway/kinematics_contract.rs must stay byte-identical (talisman blob 997fb7ae15ce3e11adec9218044c7c84b049ad3b).

Step 0 — sync and branch:
  git fetch origin '+refs/heads/*:refs/remotes/origin/*'
  git checkout -B feat/governor-udp-service origin/main

Step 1 — read the real types first:
  Open src/gateway/kinematics_contract.rs and identify the verdict function's exact input type(s) and its output/enforcement type. The wire schema MUST be built from these real types — do not invent kinematic fields.

Step 2 — create a new minimal binary target `kirra-governor-service` (its own bin, or a small member crate) that depends ONLY on:
  - the verdict core (kinematics_contract)
  - std networking + serde + bincode
  It MUST NOT depend on r2r, ROS 2, DDS, or tokio — the QNX target has none of these. Use blocking std::net::UdpSocket.

Step 3 — implement:
  - Define `Proposal { seq: u64, ts_nanos: u128, <kinematic inputs matching the verdict core's input type> }` and `Verdict { seq: u64, action: <the verdict core's enforcement output>, reason_code: u32 }`, both serde-serializable.
  - Bind a UDP socket (configurable addr/port). Loop: recv → bincode-decode Proposal → call the verdict core VERBATIM (no logic changes) → bincode-encode Verdict → send back to sender.
  - Track last-seen seq/timestamp for an M6 watchdog (stub the staleness check now; wire the safe-state later).

Step 4 — verify, then PR:
  - cargo build for the HOST first (prove it compiles with the minimal deps).
  - Document the QNX x86-64 cross-compile invocation (QNX SDP toolchain) in the bin's README — do not assume it builds in CI; the QNX build happens on the dev host.
  - Confirm: git diff shows kinematics_contract.rs UNCHANGED; no r2r/ROS/tokio pulled into the new target's dependency tree.
  - Commit, push, open a PR, report the PR and the new target's dependency tree (cargo tree -p kirra-governor-service).
```

## Claude Code — Prompt B (car / doer bridge node)

Run in the **car's ROS 2 (Jazzy) workspace** on the Jetson. This is a separate repo from `kirra-runtime-sdk`.

```
Task: Add a ROS 2 (Jazzy) bridge node that puts the KIRRA governor in the actuator command path over UDP, plus a fault-injection hook for the demo. Mirror the wire schema defined by the governor service (Proposal/Verdict, bincode).

Step 1 — discover the real interfaces (do not assume topic names):
  Inspect the running ROS graph. Identify (a) the topic carrying the planner's proposed command, and (b) the topic the car's actuators actually subscribe to. Report both before writing code.

Step 2 — implement the bridge node:
  - Subscribe to the planner's command topic.
  - On each command: serialize a Proposal (seq, timestamp, the kinematic fields matching the governor's schema) and send it to the governor over UDP; await the Verdict (with a short timeout).
  - Apply the Verdict to the actuator topic: Accept → republish unchanged; Clamp → republish the corrected command; SafeState → publish the car's safe-stop command.
  - On timeout / no verdict within the deadline → publish safe-stop locally (belt-and-suspenders; this is the M6 car-side deadline).

Step 3 — fault injection for the demo:
  - Add a parameter or service that, when triggered, emits a deliberately out-of-envelope command (e.g., a step beyond the kinematic limits) so the governor's clamp/safe-state is visibly demonstrated.

Step 4 — verify:
  - Build the workspace (colcon).
  - Dry-run with the governor service on the PC: confirm normal commands pass through and the injected bad command is clamped/safe-stated at the actuator topic.
  - Report the two discovered topic names and the observed clamp behavior.
```

---

## Caveats & honest notes

- **Prototype, not cert build.** First light uses regular Rust for the QNX target to get moving. The ASIL-D factoring (Ferrocene, `no_std`, certified `core` subset) is a later stage once the logic is proven — it does not block the demo.
- **Verify QNX std/tokio surface.** Start with blocking `std::net` to keep the QNX-side dependency surface minimal; only reach for async if a real need appears.
- **Network link ≠ control-loop transport.** UDP over WiFi/Ethernet is fine for the demo, not for tight real-time control. The governor's watchdog (M6) and the car-side deadline cover stale/missing verdicts by safe-stating — which is the correct fail direction and itself worth demonstrating.
- **This is QM hardware.** PC + consumer GPU + Jetson + Pi-class boards are prototype/dev silicon, never the cert target. The cert target remains DRIVE AGX Orin per ADR-0001.
- **Talisman discipline holds.** The governor service wraps the verdict core; it never edits it. Re-verify the blob after Prompt A.

---

*Companion documents: `0032-governor-deployment-platform.md` (the decision of record) and `KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md` (the full rationale).*
