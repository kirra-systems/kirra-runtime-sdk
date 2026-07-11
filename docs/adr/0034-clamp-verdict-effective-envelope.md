# ADR-0034: The Clamp-verdict effective velocity envelope on the ROS fast loop

| | |
|---|---|
| Status | Proposed — pending owner sign-off |
| Date | 2026-07-10 |
| Supersedes | — |
| Related | ADR-0033 (actuation authority / PO-2 topology gap), #893 (verdict narration side-channel), the H1/M1 `ClampBoth` re-pin (`1a258a5`, `7e7b7bb`) |

## Context — finding B1

A Cursor architecture review flagged **B1 (High)**, verified real on `8ea3e90`:

On the ROS deployment path, the slow-loop checker computes a per-pose
`EnforceAction::ClampLinear(v)` / `ClampBoth{linear: v, ..}` — the derated
velocity is *inside* the variant — but the slow loop **discarded** it
(`crates/kirra-trajectory/src/validation.rs`, the `clamp_seen = true` arm) and
aggregated to a payload-less `TrajectoryVerdict::Clamp`. `AcceptedTrajectory`
stored the **original planner points**, and the fast-loop conformance gate
`check_command_conforms` compared the incoming command against
`nearest.velocity_mps` — the planner's *unclamped* velocity — **never reading
`trajectory.verdict`**.

Consequence: a command at the planner's original (over-cap) speed on a `Clamp`
verdict **passed conformance**, so the checker's derate was silently dropped in
the adapter's own gate. This is the sibling of ADR-0033/PO-2 (checker verdict
authoritative in architecture, unenforced on the ROS path) but distinct and
arguably worse: no rogue actor — the trusted gate ran, said "clamp", and the
clamp was dropped. `AcceptedTrajectory::fail_closed`'s own docstring already
promised "Phase 3 conformance enforces the derate" — the contract existed; the
wiring did not.

## Decision

Carry the checker's **effective per-pose velocity envelope** from the slow loop
to the fast loop, and gate `Clamp`-verdict conformance against it.

**Representation — Option A (chosen): a field on `AcceptedTrajectory`.**
`effective_velocity_ceiling: Option<Vec<f64>>`, aligned index-for-index with
`points`. `Some(ceilings)` when a **velocity** clamp fired (the checker's own
enforced value per pose, the planner velocity where no clamp fired); `None` on
`Accept` **and** on a pure steering clamp (no velocity derate → the planner
velocity is the correct ceiling). `check_command_conforms` gates against
`ceiling[nearest]` when present; a `Some`-but-missing entry **fails closed**
(MRC), never falling back to the planner speed. The envelope is materialized
LAZILY (only when a velocity clamp is first observed), so the `Accept` /
no-velocity-derate slow-loop path allocates nothing extra.

**Rejected — Option B: a payload on `TrajectoryVerdict::Clamp`.**
`TrajectoryVerdict` is pinned at **one byte** (`trajectory_verdict_stays_one_byte`,
`crates/kirra-core/src/trajectory.rs`) and lives in the lean core beside the
frozen kinematics talisman (`ed00f4da…`). Growing it with a payload is exactly
what #893 refused for narration — the WCET-critical verdict type must not carry
side data. B was declined to keep the one-byte pin.

### Why A is safe on the size/layout axis

- `AcceptedTrajectory` has **no size/layout pin**: no `size_of` assertion, not
  `no_std`, absent from the talisman and `src/wcet_gate.rs`. It is already a
  heap `std` struct (`Vec<TrajectoryPoint>` + `String`). The verdict core stays
  **byte-identical** (`ed00f4da…` unchanged; `kirra-core` zero-diff).
- It **is** in the fast-loop hot path (cloned per tick), so the added
  `Vec<f64>` grows that per-tick clone — modestly (8 B/pose vs the ~40 B/pose
  the points already clone) and O(1) on the conformance read (one indexed field
  + a branch). This is the exact clone the planned `Arc<[T]>` optimization
  removes, so B1 correctly gates that work.

### Single source of truth

The ceiling is emitted by the checker's **own** per-pose loop
(`validate_trajectory_slow_with_envelope`), reusing the `ClampLinear`/`ClampBoth`
value `validate_vehicle_command` already computed — never recomputed in a
second pass, so it cannot drift from the checker's decision.
`validate_trajectory_slow_explained` / `_capped` / `_slow` are thin wrappers
that discard the envelope, so their many callers are unchanged.

## Consequences

- A `Clamp`-verdict command at the unclamped speed now fails conformance → MRC
  (B1 closed); a command at/below the derated ceiling passes ("drive slower,
  not stop", honoring the `Clamp` contract).
- The `Accept` fast path is byte-identical (envelope `None`).
- Regression coverage is the primary artifact: `crates/kirra-trajectory/tests/
  conformance.rs::b1_clamp_verdict_derates_the_conformance_ceiling` drives the
  real checker to a known ceiling and asserts the unclamped command MRCs and
  the at-ceiling command passes — replacing the missing coverage the prior
  `clamp_verdict_is_preserved_while_fresh_mrc_is_floored` test never provided
  (that test asserts a true, distinct property — Clamp survives the staleness
  collapse — and is left unchanged).
