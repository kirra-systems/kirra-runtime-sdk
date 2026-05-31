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

## 7. SG2 goes live

This wiring activates validate_trajectory_containment in the verdict path →
**SG2 moves PENDING-WIRING → ENFORCED**. The containment lateral margin (0.30 m
placeholder) needs the S8 number → **#131 and S8 interlock**: SG2 isn't credibly
live until S8 sets the margin (the matrix flips only when both land).

## 8. ROS 2 ↔ Rust adapter

The Governor runs as a ROS 2 node (rclrs / r2r) on separate compute. Subscribes:
Trajectory (planning), MapBin/Lanelet2 (map), PredictedObjects (perception), ego
pose (localization), outgoing control command (to gate). Maps them to the
Governor's internal types (kinematics contract, corridor, footprint). Independence
+ separate compute fall out of the ROS 2 node/process model (the FFI/D3 story for
free).

## 9. Demo (CARLA + scenario_runner)

Autoware-in-CARLA as the doer; Governor as the final gate. Inject:
- perception dropout / stale objects → SG9 fail-closed → MRC;
- trajectory clipping the Lanelet2 corridor → SG2 reject;
- a cut-in creating an RSS violation over the horizon → SG1 reject/MRC;
- an over-aggressive trajectory exceeding the envelope → SG3 clamp/reject.
Contrast: bare Autoware (its QM checks) proceeds; the Governor catches + MRCs —
plus the fail-closed / MRC / integrity-evidence properties RSS-in-CARLA lacks.

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
