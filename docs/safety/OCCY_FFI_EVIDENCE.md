# Occy / KIRRA — Freedom From Interference (FFI) Evidence (S3)

**Issue:** S3 (#115).
**Doc ID (proposed):** KIRRA-OCCY-FFI-001.
**Status:** Evidence consolidation for review. Documents the spatial, temporal,
and communication isolation that lets the ASIL-D Governor stand independent of
the QM planner — the freedom-from-interference half of the decomposition's PO-2
(OCCY_DFA.md). Most of this is already built; this doc assembles it as evidence.

---

## 1. Why FFI is required

The decomposition (ADR-0003 / OCCY_DFA.md) is valid only if the QM planner cannot
defeat or corrupt the ASIL-D Governor. FFI is the demonstration that it can't —
across the three ISO 26262 interference classes.

## 2. Spatial (memory) FFI

- **Independent compute (D3 / ADR-0003).** The Governor + IDC run on compute
  separate from the planner (separate SoC preferred; hardware-isolated partition
  the minimum) — no shared address space to corrupt.
- **No shared mutable state.** The Governor consumes the world model as input; it
  does not share writable state with the planner.
- **Input copy + validation.** The planner cannot corrupt the Governor through
  malformed input: the body-bound cap (a9c4b54, 16 KiB / 413) bounds input size,
  and the P0 NaN/Inf traps reject non-finite values before they reach the math
  kernel. Inputs are validated, not trusted.

## 3. Temporal (timing) FFI

- **No CPU starvation.** Separate compute (D3) means the planner cannot starve
  the Governor of execution time.
- **Bounded verdict + fail-closed timeout.** The verdict WCET is bounded and
  measured (S3 / GOVERNOR_INTEGRITY_EVIDENCE.md); the SG9 timeout converts any
  budget overrun into Reject → MRC. A timing fault cannot produce an unsafe
  Accept.
- **Decoupled audit write.** The audit write is off the verdict path (Pass B2,
  commit 2f72f7b) — SQLite/lock I/O cannot block or delay the verdict. The whole
  verdict path is lock-free (Pass A+B).

## 4. Communication (egress) FFI

- **In-line on actuation egress.** The Governor sits on the path between planner
  and actuator; the verdict gates the command. There is no path from the planner
  (or teleop — the doer-agnostic SG7 property) to the actuators that bypasses the
  Governor.
- **Fail-closed on loss.** No valid verdict → no Accept → the actuator safe-stops
  (the #127 assumption of use). Death/absence cannot leak an unchecked command.

## 5. Evidence sources

ADR-0003 (D3 independent compute); body-bound a9c4b54; P0 NaN/Inf traps
(kinematics_contract.rs:139); WCET bound + SG9 (S3, commits 89d0694/1e9ce36);
lock-free verdict path (Pass A 5b15641 + B1 6ed2703 + B2 2f72f7b); SG7
doer-agnostic parity test; the actuation safe-stop AoU (#127).

## 6. Assumptions / residuals

- **D3 deployment is an assumption of use.** The spatial/temporal FFI relies on
  the integrator actually running the Governor on separate compute. The Governor
  Safety Manual states this; if violated (Governor co-resident with the planner),
  the FFI argument weakens to partition-isolation only.
- The test-fallback `store.lock()` (policy_layer.rs:197) is production-unreachable
  (the writer is installed before any listener binds) — not an FFI hole.

Cross-refs: OCCY_DFA.md (PO-2), ADR-0003, GOVERNOR_INTEGRITY_EVIDENCE.md, the
Governor Safety Manual (KIRRA-OCCY-MANUAL-001), #127. Register as
KIRRA-OCCY-FFI-001.
