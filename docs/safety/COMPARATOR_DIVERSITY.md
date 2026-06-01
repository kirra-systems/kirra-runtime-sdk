# Governor Comparator — Diversity Argument

**Doc ID:** KIRRA-CERT006-DIVERSITY-001
**Tracker:** CERT-006 (comparator diversity)
**Implements:** `parko/crates/parko-kirra/src/diverse.rs`,
`parko/crates/parko-kirra/src/comparator.rs`
**Relates to:** CERT-006 (software lockstep comparator),
`docs/safety/ANGULAR_VELOCITY_SOTIF.md` (#136, the shared angular spec),
`docs/safety/IEC_61508_SIL3_MAPPING.md` §FM-002
**Date:** 2026-06-01

---

> # ⚠️ DRAFT — pending formal safety-engineer review
>
> This is a **first** diverse implementation plus a **draft** diversity
> argument. A diversity claim is a safety argument that a real assessor
> will scrutinise; it has **not** yet been reviewed or signed off by a
> safety engineer. The same human-review bar that applies to the SOTIF
> angular-bound work (#136) applies here.
>
> What is engineering fact (testable, and tested) vs what is a reasoned
> argument (not testable) is called out explicitly below. Do not cite the
> argument sections as validated coverage.

---

## 1. Background and problem statement

The `GovernorComparator` is Kirra's software lockstep: it runs two safety
governors on the same input each tick and compares their outputs. On a
persistent divergence (and once the vehicle is at a safe speed) it escalates
to a fail-closed `Deny` (LockedOut). The escalation, leaky-bucket
accumulator, speed gate and audit sink are unchanged by this work — see the
`comparator.rs` module documentation.

Until CERT-006-diversity, the comparator ran **two identical
`KirraGovernor` instances**. Identical redundancy detects:

- **random / transient faults** — bit flips, single-event upsets, memory
  corruption, a transient error in one instance;
- **state divergence** — the two copies fed inconsistent RSS / posture
  updates.

It is, by construction, **blind to systematic faults**. A logic or numerical
bug in the shared code path (a wrong comparison operator, an off-by-`dt`
error, a mis-signed clamp) computes the *same* wrong answer in *both* copies.
They agree, the comparator sees no divergence, and the wrong command reaches
the actuator. This is the gap CERT-006-diversity closes.

## 2. Decision — diversity kind (Approach A)

Two options were considered:

| | Approach A — structural / algorithmic diversity | Approach B — full N-version |
|---|---|---|
| What | A second governor enforcing the **same** properties via deliberately **different computation** (different control flow, different algebra, different decomposition). | A clean-room reimplementation from the **spec only**, not the primary's code. |
| Catches | Implementation-level systematic faults (the most common class). | Maximal implementation diversity. |
| Shares | The specification **and** the config/contract. | The specification only. |
| Effort | Tractable now. | Substantially more; independent team, independent spec interpretation. |
| Decision | **IMPLEMENTED (this deliverable).** | Noted as the stronger-but-later step (§6). Not built now. |

**Approach A is implemented** as `DiverseKirraGovernor`. It is the right
first step: it removes the largest and most likely fault class
(implementation bugs) at tractable cost, and it is honest about what it does
not cover (§5).

## 3. The two properties, and which are testable

| Property | Kind | Where demonstrated |
|---|---|---|
| **CORRECTNESS** — the diverse governor produces the **same physical verdict** as the primary on valid inputs (no false divergence). | Testable; critical. | `diverse::tests::agreement_*` (hand-built set spanning Allow / each Clamp / Deny across every posture + both RSS states + multiple configs) and `diverse::tests::proptest_diverse_agrees_with_primary` (10 000 bounded cases). |
| **DIVERSITY** — the two are structurally different enough that a systematic implementation bug would not appear identically in both. | An **argument**, not a test. You cannot test "would a hypothetical bug be caught." | §4 (documented structural differences) + the fault-class reasoning in §4.2. |
| **DETECTION** — injecting a fault into one path makes the comparator diverge → escalate → fail-closed. | Demonstrable. | `comparator::tests::test_injected_fault_is_detected_and_escalates` (+ the in-envelope control case). |

A false divergence on a valid input would itself be a safety-relevant
regression (the comparator would clamp/stop a *good* command), which is why
CORRECTNESS is the property guarded most heavily.

## 4. The structural differences (the reviewable core)

`DiverseKirraGovernor` enforces every property `KirraGovernor` does — the ODD
speed ceiling, the acceleration / braking rate envelope, the SOTIF
angular-velocity bound `ω_max(v)`, the MRC contraction, and the LockedOut
hard stop — but reaches the verdict through different computation. It does
**not** call `validate_vehicle_command` and does **not** call any
`KirraGovernor` method; it only reads the shared *config* (the kinematics
contract's limit fields and the `AngularVelocityBound`).

### 4.1 Concrete differences

1. **Regime selection.**
   *Primary:* checks `LockedOut`, then a **separate** RSS early-return, then
   `match posture` (`KirraGovernor::evaluate`).
   *Diverse:* a single `classify` step folds "Degraded posture **OR**
   RSS-unsafe" into one `MinimumRisk` predicate and treats `LockedOut` as the
   dominating hard stop, then dispatches once. The primary reaches the MRC
   code from two different branches; the diverse governor reaches it from
   one. (Algebraically equivalent — verified by the agreement tests.)

2. **Linear rate envelope — the most error-prone arithmetic.**
   *Primary:* computes a scalar implied acceleration `(v − c)/dt` and
   **sign-splits** into two magnitude comparisons against the accel and brake
   limits (`apply ...` then return the first triggered clamp).
   *Diverse:* builds the **admissible-velocity interval**
   `[c − brake·dt, c + accel·dt]`, clipped to `±ceiling`, and does interval
   containment. The primary's `+1e-9` tolerance lives in *acceleration*
   space; it is mapped into *velocity* space (`1e-9·dt`) here. Different
   decomposition, identical result. A bug like "forgot to multiply the brake
   limit by `dt`" in one formulation would not reproduce in the other.

3. **Effective ceiling.**
   *Primary:* calls `contract.effective_max_speed_mps()`.
   *Diverse:* re-derives `min(max_speed, odd_cap)` inline, so it does not
   share that accessor.

4. **Clamp reconstruction.**
   *Primary:* `value * x.signum()` and `f64::clamp`.
   *Diverse:* `f64::copysign` and an explicit `.max(..).min(..)` fold.

5. **No shared enforcement code.** The diverse path shares *config*, not
   *computation*. This is the "same limits, computed differently" the safety
   case requires; reusing the primary's functions would defeat the diversity
   and is deliberately avoided (and flagged in the module docs if ever
   introduced).

### 4.2 Fault class covered

Approach A covers **implementation-level systematic faults**: bugs in *how*
one governor computes a verdict from the (correct) limits — wrong operator,
wrong sign, off-by-`dt`, wrong rounding/tolerance handling, a mis-ordered
clamp, a transposed branch. Because the two governors decompose the same
computation differently, such a bug in one path is unlikely to produce a
byte-for-byte identical wrong output in the other, so the comparator's
divergence detection fires. This is the most common and most likely class of
systematic fault in hand-written safety code, which is why it is the right
first target.

## 5. What this diversity does **NOT** cover (the honest limit)

- **Shared-specification faults.** Both governors are derived from the same
  spec. If the *spec* is wrong — a wrong limit value, a wrong safety goal, a
  flaw in the `ω_max(v)` derivation itself (`ANGULAR_VELOCITY_SOTIF.md`,
  still DRAFT) — **both** implement the same wrong behaviour, they agree, and
  the comparator stays silent. Diversity of implementation cannot catch a
  fault of specification.
- **Shared-config faults.** The two governors consume the same kinematics
  contract and the same `AngularVelocityBound`. A mis-configured limit is
  applied identically by both. (Diversity is in the *enforcement*, not in
  the *limit values*.)
- **The shared angular derivation.** `ω_max(v)` is computed by the shared
  `AngularVelocityBound::omega_max`. The diverse governor re-implements the
  *enforcement* around it (the clamp decision), but **not** the SOTIF
  derivation. A fault inside `omega_max` is shared.
- **Common-cause faults below the software layer** — compiler bugs, the Rust
  core/std, the host CPU/FPU. Out of scope for a same-binary software
  comparator; these belong to the hardware/diverse-toolchain story.

These are not defects in this deliverable — they are the inherent boundary of
implementation diversity, stated so the safety case does not overclaim.

## 6. What a full N-version step (Approach B) would add later

A clean-room reimplementation from the spec — ideally by an independent
person interpreting the requirements without reading the primary's code —
would add resilience to **spec-interpretation** faults: two engineers are
less likely to misread the same requirement in the same way than one
engineer is to write two structurally different encodings of their own
(possibly mistaken) understanding. It still shares the written specification,
so a genuine spec error survives even Approach B; that residual is addressed
by **diverse review of the spec itself**, not by code diversity. Approach B
is materially more effort and is **not** built in this deliverable.

## 7. Verification summary

- `cargo test -p parko-kirra` — green, including the agreement set, the
  10 000-case agreement proptest, and the injected-fault detection test.
- `cargo test --workspace --exclude parko-onnx --exclude parko-openvino`
  (in `parko/`) — green.
- Root `cargo test --workspace` — unchanged.
- The comparator's escalation / accumulator / speed-gate / audit logic is
  **unchanged**; only the shadow governor's implementation differs and the
  comparator is now generic over the shadow type (default
  `DiverseKirraGovernor`; a second `KirraGovernor` still constructs the
  legacy identical-redundancy comparator for tests).

## 8. Status

**Derived / implemented — pending safety-engineer review.** Not closed. The
diversity argument in §4–§6 requires the same human review as the #136 SOTIF
work before it can be cited as validated coverage in any assessment.
