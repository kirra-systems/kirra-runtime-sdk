# Occy / KIRRA — #131 Option-B Per-Trajectory Wiring, grounded on Autoware

**Doc ID:** KIRRA-OCCY-OPTIONB-001.
**Issue:** #131 (Option-B per-trajectory check).
**Status:** Design for review. Specializes the Option-B architecture to Autoware's
actual ROS 2 interfaces so the per-trajectory check is concrete. Activates SG2
live and brings in RSS-over-horizon. Flags decisions; does not decide unilaterally.
Autoware interface names are from current docs — re-verify exact fields at
integration time (§10.5).

## 1. Motivation — an independent gate above the doer's own checks

Autoware's design docs state plainly that the planner is not a safety guarantee:
the planning component "is not capable of 'never crashes'," and policy is
deliberately separated from mechanism (the mechanism follows even a bad policy).
Autoware has internal checks — a planning_validator and the vehicle_cmd_gate
limitation filter — but these are the *doer's* QM-level self-checks. #131 wires
KIRRA's Governor as the *independent, fail-closed, ASIL-D* gate above them. The
doer/checker split, made concrete on a real stack.

## 2. Insertion point — the final independent gate

Autoware egress: planning → trajectory_follower (control) → vehicle_cmd_gate →
vehicle interface. The vehicle_cmd_gate already filters abnormal commands and
selects among sources (trajectory-follower / MRM / external-remote).

The Governor sits as the **final gate, downstream of vehicle_cmd_gate, on separate
compute (D3)**. Autoware's gate remains the doer's internal QM filter; the
Governor's gate is the ASIL-D one — nothing reaches the vehicle without the
Governor's Accept. (Insertion variant to decide: downstream-of vs. replace —
§10.1.)

## 3. Two-rate check (the WCET resolution)

The heavy per-trajectory check and the fast per-cycle check run at DIFFERENT
RATES — which is what resolves the WCET concern.

- **Per-trajectory validation (slow, planning rate ~10 Hz).** On each new
  Trajectory (autoware_planning_msgs::Trajectory, ~10 s @ 0.1 s ≈ 100 points),
  validate the WHOLE trajectory: per-pose footprint containment in the corridor
  (validate_trajectory_containment), per-pose kinematic envelope, and RSS over
  the horizon vs. perception objects. Verdict: Accept (promote) / Reject→MRC /
  Clamp (promote a speed-derated variant). The ~10 ms containment WCET fits the
  ~100 ms planning budget with ~10× margin.
- **Per-cycle conformance (fast, control rate).** Each control cycle, a
  lightweight check that the outgoing command conforms to the ACCEPTED trajectory
  and the instantaneous dynamic envelope — the existing sub-µs verdict, SG9
  timeout = existing 100 µs.

The Governor holds the **currently-accepted trajectory** as state: the fast loop
conforms commands to it; the slow loop validates each new candidate before
promoting it to replace it.

## 4. SG9 / FTTI re-derivation — two budgets, not one

- **Fast loop**: per-cycle conformance; SG9 timeout = 100 µs (unchanged). The
  per-cycle FTTI.
- **Trajectory validation**: must complete before a new trajectory is *promoted*;
  FTTI ≈ one planning cycle (~100 ms); ~10 ms WCET fits.
- **Fail-closed**: a new trajectory not validated in time → keep following the
  last-accepted trajectory; if exhausted or staleness exceeds budget → MRC.
  Governor death/absence → no Accept → MRC (#127 actuation safe-stop AoU).

Option-B therefore does NOT pay the 10 ms cost every control cycle — only per new
trajectory. The WCET=FTTI=timeout loop holds, split across the two rates.

## 5. Independence wiring (the safety-case crux)

The Governor's world model must not come from the planner:
- **Corridor**: the Governor derives it from the Lanelet2 map (MapBin) +
  localization (ego pose) — NOT the planner's drivable_area. The checker computes
  the safe corridor itself.
- **Objects**: from perception (autoware_perception_msgs PredictedObjects). In
  **Tier 1**, this is the SAME perception the planner uses → the omission
  common-cause from OCCY_DFA.md / #124 (the Governor can't catch what perception
  missed). The **D1 Tier-2 channel** closes it with independent detection.
  Document this: base-tier Autoware shares perception → a disclosed, accepted
  omission limitation, closed by D1.
- **Trajectory**: from planning — the artifact under check.

This is the Tier-1 SEooC consuming the integrator's world model, as designed.

## 6. MRC injection + teleop

On Reject the Governor supplies the MRC as an **independent command source** — it
does NOT defer to Autoware's MRM module (part of the doer). The Governor's final
gate selects its own MRC. Maps to #127 (vehicle honors the safe-stop) and the
MRC family / standing-MRC.

SG7 (teleop): the operation-mode manager routes Local/Remote sources through the
gate; the Governor's final gate is doer-agnostic — it checks the command
regardless of source (planner / MRM / remote), preserving SG7.

## 7. SG2 goes live — status

**As of Phase 4 (S131 implementation complete):** the
`validate_trajectory_containment` check IS called on every candidate
trajectory in `validate_trajectory_slow` (the slow-loop entry point);
a containment failure short-circuits to `TrajectoryVerdict::MRCFallback`,
which removes the per-asset `AcceptedTrajectory` slot so the fast loop
publishes MRC on the next cycle. The wiring is live in the adapter
binary (`kirra_ros2_adapter_node`).

**SG2 is now `ENFORCED` in `TRACEABILITY_MATRIX.md`** (#128). The two gates
that previously held the formal flip have both landed:
- The lateral margin is characterized: `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m`
  (was a `0.30` placeholder), derived as `localization_error + perception_error
  + control_error` per the S8 (#120) Item-A characterization in
  `docs/safety/OCCY_SG2_MARGIN.md` (KIRRA-OCCY-SG2-MARGIN-001).
- The CARLA scenario suite verifies containment end-to-end against an
  Autoware-driven trajectory injection, with the gated MRC observed on
  `~/output/control_cmd` (`docs/testing/CARLA_SCENARIO_SUITE.md` §C.2 —
  an integrator-environment artefact, not a blocking gate).

With both landed, the matrix flipped and the safety case carries SG2 as
`ENFORCED`. The disposition is honest about which fields are
implementation-complete vs measurement-complete.

## 8. ROS 2 ↔ Rust adapter

The Governor runs as a ROS 2 node (rclrs / r2r) on separate compute. Subscribes:
Trajectory (planning), MapBin/Lanelet2 (map), PredictedObjects (perception), ego
pose (localization), outgoing control command (to gate). Maps them to the
Governor's internal types (kinematics contract, corridor, footprint). Independence
+ separate compute fall out of the ROS 2 node/process model (the FFI/D3 story for
free).

### 8a. Posture awareness (M1)

`validate_trajectory_slow` now consumes a `FleetPosture` parameter and
selects the effective per-pose kinematics contract per the same mapping
parko-kirra uses, so the AV slow-loop and the differential-drive bridge
stay consistent:

| Posture | Per-pose contract | Containment / RSS | Verdict effect |
|---|---|---|---|
| `Nominal` | `VehicleConfig::to_kinematics_contract` (full envelope) | always run | unchanged from Phase 2A |
| `Degraded` | `VehicleConfig::to_mrc_kinematics_contract` (MRC-derated dynamic limits, integrator geometry preserved) | always run | per-pose Clamp fires at the tighter cap |
| `LockedOut` | n/a — short-circuit | not run | always `MRCFallback` |

Posture **augments** the geometry checks; it does not replace them.
Containment (SG2) and RSS (SG1) always run when posture is `Nominal` or
`Degraded`, so a corridor breach under `Degraded` still produces
`MRCFallback` (most-restrictive-wins).

The current-posture cache lives on `AdaptorState::current_posture`
(default `Nominal`) with `update_posture` / `current_posture` accessors.
Until **M1b** lands — wiring this cache to a live source (an SSE
subscriber on the verifier's `/system/posture/stream`, or a bridged ROS 2
posture topic from a fleet-monitor node) — the adapter behaves exactly
as the pre-M1 path did.

Enforcement site: `crates/kirra-ros2-adapter/src/validation.rs::validate_trajectory_slow`
(SAFETY: SG8 | REQ: posture-driven-profile-selection). Helper:
`VehicleConfig::to_mrc_kinematics_contract`
(SAFETY: SG8 | REQ: mrc-derated-contract-shape).

### 8b. Live posture source (M1b — fail-closed PostureTracker)

M1 made the adapter posture-aware but `AdaptorState::current_posture`
defaulted to `Nominal` and nothing drove it live. M1b connects the
verifier's computed fleet posture to the adapter, with three
**fail-closed properties** that mirror the SG9 telemetry-staleness
watchdog:

1. **Pre-first-event seed = `Degraded`.** A source-configured adapter
   that has not yet received a posture event MUST NOT command at the
   full envelope. Mirrors the verifier's own "new asset → Degraded"
   seed behaviour.
2. **Staleness derate.** If wall-clock now exceeds
   `last_event_ms + POSTURE_STALENESS_TIMEOUT_MS` (default 6 s ≈ one
   posture cycle + slack), the tracker derates `Nominal` → `Degraded`.
   We do NOT hold the last-known `Nominal`.
3. **`LockedOut` sticky-toward-safe.** A `LockedOut` observation holds
   until an explicit non-LockedOut observation from the source (the
   "recovery event"). A staleness timeout cannot relax the ceiling.

The state machine lives in `crates/kirra-ros2-adapter/src/posture_tracker.rs`
(`PostureTracker`, `POSTURE_STALENESS_TIMEOUT_MS`). It is a pure,
deterministic function of `(now_ms, observations)` — unit-tested on
stable in `posture_tracker::tracker_tests` (10 tests covering all
three fail-closed properties + the boundary condition).

Two modes (selected at `AdaptorState` construction):

| Constructor | Tracker mode | `current_posture()` baseline |
|---|---|---|
| `AdaptorState::new` / `with_config` | `nominal_default_no_source` | always `Nominal` — **preserves the M1 default** for verifier-less deployments and unit tests |
| `AdaptorState::with_posture_source` | `with_source` | pre-first-event `Degraded`; updates via `update_posture` from the SSE task |

**SSE transport** (`crates/kirra-ros2-adapter/src/posture_source.rs`,
gated on the `ros2` feature):

1. `reqwest` streaming GET to `${KIRRA_POSTURE_STREAM_URL}/system/posture/stream`
   with `Authorization: Bearer ${KIRRA_ADMIN_TOKEN}` and
   `x-kirra-client-id: ${KIRRA_POSTURE_CLIENT_ID}` (defaults to
   `kirra-ros2-adapter`).
2. The verifier's SSE event payload (`PostureStreamEvent`) carries
   either a per-node `FleetNodePosture` or a transition marker with
   `posture: None`. Rather than decode both variants, the adapter
   uses the SSE event as a **wake-up signal** and re-fetches the
   aggregate via `GET /fleet/posture` (folded worst-of across nodes).
3. Disconnect → exponential backoff (1 s → 2 s → 4 s → … capped at 30 s).
   The `PostureTracker`'s staleness derate (property 2 above) covers
   the disconnect window automatically — no special reconnect logic
   is required to keep the safety invariant.

Selection is via env var. The binary classifies the environment into
THREE states (see `kirra_ros2_adapter_node::classify_posture_source`)
and treats the misconfiguration case as fail-closed:

| Env state | Decision | Posture floor |
|---|---|---|
| `KIRRA_POSTURE_STREAM_URL` unset | `NoSource` → `AdaptorState::new` + no SSE task | `Nominal` (M1 default; verifier-less / CARLA-only deployments) |
| `KIRRA_POSTURE_STREAM_URL` set + `KIRRA_ADMIN_TOKEN` set | `Live` → `AdaptorState::with_posture_source` + SSE task | live posture; tracker starts at `Degraded` and updates on each event |
| `KIRRA_POSTURE_STREAM_URL` set + `KIRRA_ADMIN_TOKEN` missing/empty | `ConfiguredNoTransport` → `AdaptorState::with_posture_source` + **no SSE task** + WARN | **`Degraded` (fail-closed)** — tracker seeds Degraded pre-first-event, no observe will ever fire, the adapter holds Degraded until the operator fixes the auth and restarts |

The `ConfiguredNoTransport` branch is the critical fail-closed
amendment. When the operator's INTENT to govern is explicit (the URL
is set), any failure to actually use the source must hold the adapter
in `Degraded` — never silently fall back to the no-source `Nominal`
baseline. This is the posture analog of "missing admin token → 503,
never 200" in the verifier.

(Considered and rejected: hard refusal-to-start on misconfiguration.
Graceful Degraded is recommended — the vehicle can still operate in
MRC rather than not at all, and the disposition matches the rest of
the fail-closed machine.)

**Alternative considered (deferred).** Option B — a separate ROS 2
bridge republishing verifier posture on a topic — keeps the adapter
pure-ROS 2 but adds a component. Defer unless a deployment already has
such a bridge.

Enforcement sites:
- State machine: `crates/kirra-ros2-adapter/src/posture_tracker.rs::PostureTracker` (SAFETY: SG8 SG9 | REQ: posture-source-fail-closed).
- SSE transport: `crates/kirra-ros2-adapter/src/posture_source.rs::spawn_posture_source` (SAFETY: SG8 SG9 | REQ: posture-source-fail-closed-transport — integration-tested).

## 9. Demo (CARLA + scenario_runner)

Autoware-in-CARLA as the doer; Governor as the final gate. Inject:
- perception dropout / stale objects → SG9 fail-closed → MRC;
- trajectory clipping the Lanelet2 corridor → SG2 reject;
- a cut-in creating an RSS violation over the horizon → SG1 reject/MRC;
- an over-aggressive trajectory exceeding the envelope → SG3 clamp/reject.
Contrast: bare Autoware (its QM checks) proceeds; the Governor catches + MRCs —
plus the fail-closed / MRC / integrity-evidence properties RSS-in-CARLA lacks.

## 9a. Audit disposition (pilot)

**Pilot evidence (Phase 4):** every slow-loop trajectory verdict
(`Accept` / `Clamp` / `MRCFallback`) and every fast-loop conformance
verdict (`Accept` / `MRCFallback`) is emitted as a structured
`tracing` log line with the asset id, verdict, elapsed-μs, and the
proximate cause. The `subscription_staleness_mrc` path additionally
emits the configured timeout. These structured logs are the pilot
audit evidence: an integrator can replay a CARLA scenario, capture the
JSON-line stream from the adapter binary's `tracing-subscriber::fmt`,
and demonstrate that every MRC published on `~/output/control_cmd` is
accompanied by a matching log line stating the cause.

**Full integration with the hash-chained `audit_log_chain` in
`kirra_verifier_service` is a productization step**, not a Phase 4
deliverable. The adapter binary and the verifier service are separate
processes today; closing this loop requires one of:
- IPC between the adapter and the service (the natural fit: the
  adapter posts deny/MRC events to a `POST /actuator/trajectory/audit`
  endpoint, joining the existing `audit_writer` Pass-B2 pipeline), OR
- Co-locating `AppState` so the adapter holds an `Arc<AppState>` and
  writes via the same `audit_writer_tx` the actuator middleware uses.

Either route preserves the byte-identical-payload contract (Pass B3 —
alphabetical struct fields, deterministic serialization). The pilot
ships tracing logs; productization moves to the chained ledger.
Tracked separately from the safety case.

## 10. Decisions to flag (NOT decided here)

1. Insertion: downstream of vehicle_cmd_gate vs. replacing it (downstream leaves
   Autoware unmodified; replacing gives one clean gate).
2. Conformance semantics: how the fast loop maps an outgoing command back to its
   point on the accepted trajectory (metric + tolerance).
3. Corridor from Lanelet2: which lanelets form the corridor (current + reachable;
   how intersections / lane-changes branch it).
4. Tier-1 perception-sharing: is the disclosed omission common-cause acceptable
   for the base-tier AV claim, or does the AV ODD require D1?
5. Confirm current autoware_planning_msgs::Trajectory + perception/map message
   fields at integration time.

Cross-refs: #131, OCCY_DFA.md, OCCY_ARCHITECTURE_TIERS.md, #124 (D1), #126
(perception input contract), #127 (safe-stop), #128 / SG2, S8 (#120 margin),
containment WCET. Register as KIRRA-OCCY-OPTIONB-001.
