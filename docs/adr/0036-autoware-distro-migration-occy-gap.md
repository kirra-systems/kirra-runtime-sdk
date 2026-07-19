# ADR-0036 — Autoware Distro Migration & Occy/KIRRA Gap Analysis

**Status:** Accepted
**Date:** 2026-07-19
**Owner:** Kirra Systems, LLC
**Scope:** How the KIRRA stack crosses the Ubuntu 22.04 / ROS 2 Humble → Ubuntu 24.04 / ROS 2 Jazzy boundary, given that the **Autoware** AV-stack doer is the one component pinned to Humble. Records the doer-vs-doer gap analysis (Occy/KIRRA vs Autoware) that decides "keep vs replace," and the migration/isolation decision that follows. This is a *doer-side* decision — it does not touch the safety spine or any certification claim (ADR-0032).

---

## Decision

1. **Autoware stays.** It remains the AV-stack **doer**; KIRRA does **not** reimplement it. Occy/KIRRA is a *lean doer + heavy checker* and does not cover Autoware's L4 breadth (localization, closed-loop control, mature fused perception, full HD maps). Reimplementing those would dilute the thesis — KIRRA's value is the **checker that bounds any doer**, not a second Autoware.

2. **Autoware becomes the *only* thing on 22.04 / Humble, isolated in its own container.** Everything else — `kirra-ros2-adapter`, the checker/governor, the robot `rclpy` nodes, the Occy/planner/Taj crates — targets **24.04 / Jazzy**. The rest of the stack's Jazzy move is therefore **unblocked now** and does not wait on Autoware.

3. **Interop crosses at the frozen contract, not at raw cross-distro ROS topics.** The Humble-Autoware container and the Jazzy world are bridged at the distro-agnostic `#[repr(C)]` governed-command / doer↔checker boundary (ADR-0006 Clause 2, ADR-0032), or through a **narrow, version-matched** message set via `domain_bridge`. Naive Humble↔Jazzy DDS is **not** relied upon (RIHS type-hash / message-version mismatch risk).

4. **The Humble container is retired when Autoware ships stable Jazzy support.** Autoware tracks the ROS LTS; a 24.04/Jazzy Autoware is the expected successor. That external release — not anything in this repo — is the trigger. Confirm current Autoware roadmap status before committing a date.

The **safety spine is unaffected** by any of this: the checker is `no_std` and ROS-agnostic, and ADR-0032 already names the doer guest as "ROS 2 Jazzy."

---

## Context

- **Humble + Ubuntu 22.04 Jammy** reach end-of-support **~May 2027** (tied together). **Jazzy + 24.04 Noble** is the LTS successor (Jazzy EOL 2029).
- In the KIRRA doer/checker split (`docs/hardware/TARGET_PLATFORM_MATRIX.md`), the ROS/Ubuntu box is the **swappable, never-trusted doer**. Its distro coupling is a **contained, non-safety** concern.
- Audit of the workspace: **the only component structurally tied to Humble is the Autoware AV-stack doer.** The Rust binding (`r2r = "=0.9.5"`) already lists Humble / Jazzy / Kilted support; the `kirra-ros2-adapter` and robot `rclpy` nodes are source-mostly-portable Humble→Jazzy (the sharp edge is Python 3.10→3.12 native deps). Message interfaces are tracked in `docs/safety/MSG_INTERFACE_VERSION_SYNC.md`.
- The open question was whether Occy/KIRRA could **replace** Autoware and delete the dependency. Answer: only for narrow uses — see the gap analysis.

---

## Gap analysis — Occy/KIRRA doer vs Autoware

**One line:** Occy/KIRRA is a lean *doer* attached to a heavy *checker*; Autoware is a full production AV autonomy stack. They overlap on "propose a trajectory along a lane corridor" and diverge at the edges.

### What Occy/KIRRA's doer actually is
- **`kirra-map`** — Lanelet2-**lite**: route Dijkstra, multi-junction `route_corridor`, right-of-way / junction context, occlusion sight-distance. A subset of Lanelet2, not the full HD-map spec.
- **`kirra-planner` (Occy)** — geometric planner + Mick intent grounding (`GoTo`/`LaneChange`/`Cruise`/`Overtake`/`PullOver`/`TurnAt`/`RouteTo`) + learned planners (Hydra-MDP speed; 2-D lateral×speed route-around) + `behavior.rs` (traffic-control + occluded-approach creep).
- **`kirra-taj`** — lidar perception: geometric corridor + semantic hazard fusion (Phase-A/B).
- **`parko`** — ML inference backends (ONNX / OpenVINO / TensorRT) + detector.
- **The checker** (`kirra-ros2-adapter` validation: RSS, occlusion bound, multi-modal predictive RSS, perception redundancy) — where KIRRA is *ahead*; Autoware has **no** equivalent formal bounding layer.

### Coverage table

| Autoware subsystem | Occy/KIRRA coverage | Verdict |
|---|---|---|
| **Localization** (NDT lidar + EKF pose fusion, GNSS) | *none* | 🔴 **Major gap** — no map-relative localization |
| **Control** (trajectory follower: MPC / pure-pursuit) | *none* — R2 uses the Ackermann last-hop + governor clamp | 🔴 **Major gap** — no closed-loop path tracker |
| **Perception** (3-D detection + MOT tracking + map-based prediction + traffic lights) | parko ML detectors + Taj corridor/hazard + predictive-RSS (in the *checker*) | 🟠 **Partial** — detection + bounding, not a mature fused doer pipeline |
| **HD maps** (full Lanelet2 + pointcloud maps + projection) | Lanelet2-**lite** | 🟠 **Partial** — subset |
| **Behavior planning** (intersection, crosswalk, blind-spot, stop-line, traffic-light) | traffic-control + occluded-approach | 🟠 **Partial** — a slice of the suite |
| **Motion planning** (obstacle avoidance, path optimization, freespace/parking) | geometric + learned 2-D route-around, `PullOver` | 🟠 **Partial** — vocabulary-based, not optimization-based |
| **Sensor drivers** (lidar/cam/radar/GNSS/IMU) | ydlidar + off-the-shelf | 🟠 mostly distro-independent |
| **Route / mission planning** | Occy `RouteTo` (Dijkstra) | 🟢 **Covered** (for the lite map) |
| **Vehicle interface** | R2 governed consumer (Ackermann) | 🟢 **Covered** for R2; not general AV |
| **Formal command bounding** (RSS / occlusion / decel envelope) | the KIRRA checker | 🟢 **KIRRA-only** — Autoware has no equivalent |

### What the gaps imply
The red/orange cells cluster in the "**full L4 AV autonomy on real roads**" columns — localization, control, mature fused perception/HD-maps. Those are multi-year to close **and are not what KIRRA is trying to be.** So the decision maps by *usage*:

- **Autoware as the real AV autonomy stack** (localize on an HD map → perceive/track → follow trajectories) → Occy does **not** replace it → **keep + isolate** (this ADR).
- **Autoware as a *reference doer*** feeding trajectories to the checker for the doer/checker demonstration → Occy already fills that role on scoped scenarios → droppable for that purpose.
- **Specific Autoware modules** (e.g. just localization or just perception) → those are exactly the red cells → keep those, drop the rest.

---

## The Autoware boundary (as inventoried)

The handoff between Autoware and the KIRRA stack is **already curated to a small,
hash-verified interface** — the checker does not depend on Autoware, only on a
pinned subset of its message packages. This makes the Humble isolation far
smaller than a generic Autoware container.

**The seam = 5 topics into the checker node (`kirra-ros2-adapter/src/node.rs`):**

| Topic | Type | Direction | Role |
|---|---|---|---|
| `~/input/trajectory` | `autoware_planning_msgs/Trajectory` | Autoware → checker | the proposed path being **bounded** |
| `~/input/objects` (+ `~/input/objects_secondary`) | `autoware_perception_msgs/PredictedObjects` | Autoware → checker | perception (+ redundant channel-B) |
| `~/input/map` | `autoware_map_msgs/LaneletMapBin` | Autoware → checker | lanelet map |
| `~/input/odometry` | `nav_msgs/Odometry` | (standard ROS) | ego odom |
| `~/input/control_cmd` | `autoware_control_msgs/Control` | Autoware → checker | control command being **gated** |
| *(output)* | `autoware_control_msgs/Control` | checker → actuation | the **governed** (bounded) command |

**Already decoupled.** The four `autoware_*_msgs` packages are **vendored +
curated** in `ros2_ws/src/` — only the verbatim message closures the governor
consumes (`Trajectory`/`TrajectoryPoint`; the `PredictedObjects` family;
`LaneletMapBin`; `Control`/`Lateral`/`Longitudinal`), **not a full Autoware
install.** The checker builds these from their `.msg` on any distro. A
`kirra_bridge_cpp` node already exists at the governor FFI boundary.

**Already governed.** `docs/safety/MSG_INTERFACE_VERSION_SYNC.md`
(KIRRA-OCCY-MSGSYNC-001) is a hash-verified **version-sync SRAC** that keeps the
curated subset wire-compatible with the deployed Autoware.

**So the cross-distro crux reduces to one check:** do these 5 message definitions
have identical **RIHS type-hashes** on Humble and Jazzy? The curated `.msg` files
are pinned in-repo, so:
- **If byte-identical → Autoware (Humble) ↔ adapter (Jazzy) talk directly over
  DDS** for these interfaces, no bridge needed.
- **If an upstream `.msg` drifts between distros** → the MSGSYNC SRAC catches it,
  and `kirra_bridge_cpp` / `domain_bridge` translates that one interface.

**Net shape of "only Autoware on Humble":**
1. **Container A (Humble):** Autoware only — publishes the 4 inputs, subscribes the governed `Control`.
2. **Container/host B (Jazzy):** the adapter + checker + Occy/Taj + robot nodes, building the 4 curated msg packages unchanged.
3. **Extend the MSGSYNC SRAC** to hash-verify the 5 interfaces across the **Humble↔Jazzy** pair — the one *new* safety-relevant check this migration introduces.
4. **Env-lock both sides.**

---

## Options considered

1. **Isolate + track (CHOSEN).** Autoware pinned to Humble in its own container; the rest of the stack moves to Jazzy; bridge at the frozen contract; adopt Autoware's Jazzy release when it ships. Unblocks everything else now; plays to the thesis; the only cost is running EOL Humble for one isolated, non-safety box.
2. **Replace Autoware with Occy.** Rejected for full-AV use — the red-cell gaps (localization, control, mature perception) are prohibitive and off-thesis. Viable only if the actual Autoware use was narrow.
3. **Freeze the whole stack on Humble.** Rejected — drags the entire stack past EOL to accommodate one component, and forfeits the Jazzy security/support horizon for no benefit.
4. **Naive cross-distro DDS bridge at ROS topics.** Rejected as the *primary* boundary — Humble↔Jazzy interface hashes can mismatch silently; used only for a narrow version-matched set via `domain_bridge` if needed.

---

## Migration & isolation plan ("only Autoware on 22.04/Humble")

1. **Inventory the Autoware boundary.** ✅ Done — see "The Autoware boundary (as inventoried)" above: 5 topics into the checker node, backed by 4 curated (already-vendored, hash-verified) `autoware_*_msgs` packages. The design input is settled.
2. **Containerize Autoware**, pinned: a `ros:humble` + Autoware image, versions locked. Nothing else runs in that container.
3. **Move the rest to Jazzy**: `kirra-ros2-adapter` (rebuild against `/opt/ros/jazzy`; `r2r =0.9.5` already supports it), robot `rclpy` nodes (mind Python 3.10→3.12 native deps — Jetson.GPIO, numpy), Occy/Taj crates.
4. **Bridge at the frozen contract.** Route safety-relevant Autoware output through the doer↔checker boundary (distro-agnostic), not raw cross-distro topics. Where a ROS-topic bridge is unavoidable, use `domain_bridge` over a **narrow, version-matched** interface set and record the matched versions.
5. **Record an environment lock** (Ubuntu + ROS distro + rmw + `r2r` + Python + key apt/pip versions) per side — the reproducibility pin, mirroring the LLM digest-pin discipline. Add a Jazzy row to `TARGET_PLATFORM_MATRIX.md`.
6. **Add a Jazzy CI build lane** (`--features ros2` against a `ros:jazzy` container, in parallel with Humble) — the analog of the MSRV lane: validate the next target continuously so an EOL cutover is a non-event.
7. **Retire on Autoware-Jazzy.** When Autoware ships stable Jazzy, migrate the container, run the governed-loop bring-up smoke, drop Humble.

**Orin note:** on the Jetson the host OS is JetPack/L4T (NVIDIA-controlled; JetPack 6 = 22.04), not something you `do-release-upgrade`. Containerizing ROS is what lets the ROS distro move ahead of the JetPack-bound host. (Per ADR-0032, the Orin is a *doer*, never the governor cert target.)

---

## Consequences

**Positive**
- The rest of the stack's Jazzy migration is unblocked immediately; no dependency on Autoware's timeline.
- Autoware stays a swappable, checker-bounded doer — the thesis holds, and the "we complement, not compete with Autoware" positioning is documented (proposal-relevant).
- The safety spine is untouched (`no_std`, ROS-agnostic checker) — no re-certification.

**Negative / risks**
- One isolated box runs EOL Humble until Autoware-Jazzy lands. Mitigated: it carries **no safety claim**, is network-isolated, and is behind the checker.
- Cross-distro interop needs care; the frozen-contract boundary is the mitigation, `domain_bridge` the fallback.
- Autoware's Jazzy timeline is **external** — track it; do not commit a repo date to it.

## Follow-ups
- Autoware boundary inventory (step 1) — the immediate next work item.
- Jazzy CI build lane (step 6).
- Governed-loop bring-up smoke checklist (Jazzy), analogous to `robot/rabbit_model_smoketest.py`.

## References
- ADR-0032 — Governor Deployment Platform (doer guest = "ROS 2 Jazzy"; Jetson is a doer, not the cert target)
- ADR-0006 — Governor transport / the frozen `#[repr(C)]` contract (Clause 2)
- ADR-0033 — Actuation-authority ROS/R2 topology
- `docs/hardware/TARGET_PLATFORM_MATRIX.md` — doer/checker hardware homes
- `docs/safety/MSG_INTERFACE_VERSION_SYNC.md` (KIRRA-OCCY-MSGSYNC-001) — the hash-verified curated-interface version-sync SRAC (extend to the Humble↔Jazzy pair)
- `crates/kirra-ros2-adapter/src/node.rs` — the checker node + the 5-topic Autoware seam
- `ros2_ws/src/autoware_{planning,perception,map,control}_msgs/` — the curated, vendored message packages (the boundary)
- `ros2_ws/src/kirra_bridge_cpp/` — the existing C++ bridge node at the governor FFI boundary
- `crates/kirra-ros2-adapter/Cargo.toml` — `r2r =0.9.5` distro support (Humble/Jazzy/Kilted)
