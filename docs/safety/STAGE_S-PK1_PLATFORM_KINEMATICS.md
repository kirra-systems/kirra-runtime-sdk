# Stage S-PK1 — Platform Kinematics (one platform-parameterized governor)

> ## ⚠ PROPOSED — NOT A SAFETY CLAIM
> This is a **design proposal**. It is **not implementation** and confers **no
> safety coverage** until **ACCEPTED via ADR-0017** and its sub-stages land. The
> RTM / `TRACEABILITY_MATRIX` must **NOT** list S-PK1 (or any new-platform safety
> goal) as ENFORCED until the corresponding sub-stage is complete. A PROPOSED
> safety doc is fine in `docs/safety/`; a PROPOSED doc *counted as coverage* is the
> failure mode this banner prevents.

**Status:** PROPOSED — design-for-review. No code written. Awaiting owner sign-off.
**Date:** 2026-06-24
**Owner:** Kirra Systems, LLC
**Scope:** Lift the governor's three scattered, platform-specific expressions into a
single **platform-parameterized kinematic contract** so the existing safety
surface (envelope, containment, RSS, decel-to-stop, frame-integrity) extends to
non-Ackermann platforms. This is the Track-3 keystone: the prerequisite for the
doer–checker thesis to hold *at all* on a new platform.
**Future decision of record:** ADR-0017 (filed on acceptance).
**Companion:** the frame-integrity gate (`STAGE_S-FI1_FRAME_INTEGRITY_GATE.md`) is
the model for format and discipline.

---

## 1. Problem (grounded in code)

The governor's safety surface is **Ackermann-shaped**, and the platform expressions
are **scattered across three places**, not parameterized behind one abstraction:

1. **Ackermann bicycle** — `VehicleKinematicsContract` + `validate_vehicle_command`
   (`crates/kirra-core/src/kinematics_contract.rs`): `max_steering_deg`,
   `wheelbase_m`, lateral accel `a_lat = v²·tan(δ)/L`. The **angular channel is
   "intentionally NOT gated here for the Ackermann"** (`kinematics_contract.rs:354`)
   — it is implied by speed × steering.
2. **Differential drive** — a **separate** angular-velocity channel bolted on in
   `parko-kirra` (`angular_bound.rs`, `diverse.rs`; #407), applying the
   converge-to-zero / no-re-initiation rule to an independent `ω` channel via
   `STOP_EPSILON_RAD_S`.
3. **Generic scalar** — `KirraKernelGovernor<C: SafetyContract>`
   (`src/kirra_core.rs`): a third primitive that clamps a single scalar against a
   `SafetyContract`.

And **containment is 2D only** — `validate_trajectory_containment`
(`crates/kirra-core/src/containment.rs`) has **no z / altitude / joint** terms
(verified: zero such fields).

**Consequence.** The governor can only bound a robot whose physics it can
*express*. Today it cannot express a diff-drive's envelope in one place, let alone
an omni or a quadrotor. So on any non-Ackermann platform, *every doer* (nav,
SLAM, planning, behavior) would be **unbounded** until a platform-parameterized
contract exists.

## 2. The two risk classes (same lens as S-FI1)

Frame-integrity reordered Track 2 by splitting items into *frame-defining* (the
checker's own inputs, which it cannot de-risk) vs *doer-side* (bounded by the
checker). Track 3 splits the same way — and **only one item is checker-side**:

**Checker-side (extends the safety guarantee to new platforms):**
- **#2 Platform kinematics** — THE keystone. The governor's ability to *express* a
  platform's envelope is part of the safety surface, not a doer. Until #2 lands,
  the doers below are unbounded on any non-Ackermann platform.
- *(Sub-item, smaller)* **free-space containment** hiding inside #1 — see §6.

**Doer-side (bounded by the existing checker once #2 lands → build per-platform):**
- #1 free-space navigation, #3 SLAM, #4 fleet orchestration, #5 behavior framework.

## 3. The payoff — Track 3 is mostly Track 2 re-parameterized

These are **not** parallel greenfields; they reuse the existing safety core:

| Track-3 doer | Re-uses | Behind which checker |
|---|---|---|
| #3 SLAM | Track-2 localization (a different doer) | the **same** S-FI1 frame-integrity gate |
| #1 free-space nav | Track-2 planning | the **same** SG2 containment (corridor+footprint, polygon inside-test — lane-agnostic; the lane assumption lives in `kirra-map`, the doer's map, not the governor) |
| #5 behavior framework | the existing `action_filter`/`action_policy` (LLM-action→governor) and Mick (intent→governor) | the governor envelope — skills propose, the governor bounds |

So building **#2 extends the entire existing safety architecture** — envelope,
containment, RSS, decel-to-stop, frame-integrity — to new platforms **at once**.
The rest is doer capability we already know how to bound.

## 4. The design crux — unify the VERDICT, not just the envelope

"Same checker, different envelope" is true for **containment** (a polygon
inside-test is drive-agnostic). It is **NOT** sufficient for the **kinematic
contract verdict**, which is where the three expressions genuinely differ:

- Ackermann returns `EnforceAction{ Allow, ClampLinear(f64), ClampSteering(f64),
  DenyBreach(DenyCode) }`. **`ClampSteering` is meaningless for differential
  drive**, which clamps *angular velocity* `ω`, not a steering angle. The scalar
  kernel has yet another shape.

So a `PlatformKinematics` abstraction must reconcile **clamp semantics** across
drive types, not just swap numbers behind one `EnforceAction`. This is the open
design question this stage must answer. Two candidate shapes:

- **(A) Generalized verdict** — a drive-agnostic actuation verdict with a
  *longitudinal* channel and an *angular* channel (`ClampAngular(f64)`), where
  Ackermann maps steering→angular via the bicycle relation and diff-drive uses
  `ω` directly. One verdict type; per-platform channel meaning.
- **(B) Associated verdict type** — `trait PlatformKinematics { type Verdict; … }`,
  Ackermann's `Verdict = EnforceAction` (unchanged, frozen), diff-drive defines
  its own. Maximal back-compat (the talisman verdict is literally untouched), at
  the cost of N verdict types downstream consumers must handle.

**Recommendation:** start from (B) for the Ackermann adapter (zero change to the
frozen verdict — see §5), and evaluate (A) as the diff-drive sibling lands, when
the actual channel-unification cost is concrete rather than speculative. The spec
does not pre-commit; the trait shape is the first reviewable artifact.

## 5. The non-negotiable constraint — additive around the frozen talisman

`validate_vehicle_command` / `VehicleKinematicsContract` is **INV-3-adjacent
frozen**. S-PK1 must **wrap or relocate it, never modify it** — the same
discipline as the de-monolith (the talisman only ever gets wrapped or relocated).

- The **Ackermann implementation IS the existing talisman, untouched** — exposed
  through the new trait by a **verbatim adapter** (zero behaviour change).
- Diff-drive / omni become **sibling implementations** under the trait.
- This preserves the **entire existing AV safety case** while generalizing, and it
  makes the regression argument trivial: **the existing 42 talisman tests + parko's
  angular-channel tests are the proof that unification changed nothing.** If they
  stay green against the verbatim adapter, the abstraction is behaviour-preserving.

## 6. Free-space containment — doer-side, *conditionally*

#1 (free-space nav) is doer-side **iff** free space stays a *single closed
boundary polygon*: containment's PNPoly handles any polygon, so a
two-polyline-corridor → closed-region swap is a doer-side map change with **at most
a modest containment input generalization** (closed polygon vs two boundary
polylines), not a checker rewrite.

It becomes **checker-side** only if free space needs *interior keep-out holes*
enforced **by containment** (obstacles-as-geometry) rather than by the
planner/RSS. Multi-polygon / hole support in the inside-test is a real checker
extension. **Decision deferred:** keep obstacles in the planner/RSS domain (#1
stays doer-side) unless a platform requires containment-enforced keep-outs; if so,
that is a scoped checker sub-stage (S-PK-adjacent), surfaced here so it is never
smuggled in as "just a different polygon."

## 7. Tiers (do A first)

| Tier | Platforms | Checker work | Recommendation |
|---|---|---|---|
| **A — ground holonomy** | diff-drive, omni (AMR/AGV) | Unify Ackermann contract + parko's separate angular channel + the scalar kernel under one `PlatformKinematics`. **Same 2D footprint+corridor checker, different envelope + verdict.** Pieces exist, just scattered → extension, not greenfield. | **Do first.** Highest leverage, lowest risk; covers the dominant robot segment. |
| **B — aerial** | quadrotor, etc. | **3D containment** (2D today) **AND** a new envelope (thrust/attitude/altitude-rate, not speed/steering). Both a checker extension and a Tier-A-style envelope impl. | After A. |
| **C — manipulator** | arms | Joint-space reachable-set / self-collision — a **different safety surface**, not a parameter of the ground/aerial one. Double cost: very-high doer **plus** a whole new checker. | **Cut** unless a customer is paying for arms. |

## 8. Sequencing (smallest reviewable move first, S-FI discipline)

- **S-PK1a** — ✅ the `PlatformKinematics` trait + `PlatformVerdict` bound + the
  **Ackermann impl as a verbatim adapter** over the frozen `validate_vehicle_command`
  / `VehicleKinematicsContract` (`crates/kirra-core/src/platform_kinematics.rs`).
  Zero behaviour change — the adapter's `evaluate` *literally calls*
  `validate_vehicle_command`, and an equivalence test asserts
  `AckermannPlatform.evaluate(cmd) == validate_vehicle_command(cmd, contract)` over
  a command battery; the existing talisman tests are unchanged. No other code touched.
- **S-PK1b** — ✅ the **differential-drive sibling** (`parko-kirra/src/platform.rs`):
  `DiffDrivePlatform<G: SafetyGovernor>` wraps parko's existing diff-drive
  `SafetyGovernor::evaluate` (verbatim), with `Command = ControlCommand`,
  `State = DiffDriveState{previous, dt, posture}`, `Verdict = DiffDriveVerdict`.
  D1 confirmed against a genuinely different verdict (`EnforcementAction`'s
  explicit linear/angular clamps). **Three things the second platform surfaced:**
  (i) **`deny_reason` generalized `&'static str` → `&str`** in the kirra-core trait
  — parko's `Deny{reason: String}` is a runtime string, not a static token (the
  `&'static` was an Ackermann-ism); (ii) **orphan rule** → `DiffDriveVerdict`
  newtype over `EnforcementAction` (also keeps parko-core kirra-core-free per
  S-FI1c); (iii) **`&State` earns its keep** — diff-drive needs previous/dt/posture
  (Ackermann's is `()`). parko-core 198 + parko-kirra 153 tests pass; kirra-core +
  parko clippy + workspace clean.

  **Two findings flagged for review (open):**
  - **Footprint shape.** `VehicleFootprint` is rear-axle/bicycle-shaped; diff-drive
    uses the geometric-center convention (`wheelbase_m = 0`, symmetric overhangs)
    via `DiffDrivePlatform::centered_footprint`. Works, but whether the shared
    footprint type should be genericized (drive-agnostic) is a candidate S-PK1c
    decision.
  - **`evaluate` scope asymmetry.** Ackermann's `evaluate` is pure kinematics
    (`validate_vehicle_command`); diff-drive's wraps `SafetyGovernor::evaluate`,
    which in parko folds in the pushed RSS verdict — wider scope. Reflects parko's
    existing architecture (no separate public pure-kinematic entry); separating it
    is out of S-PK1b scope.
- **S-PK1c** — wire **containment + RSS** to consume `PlatformKinematics` (footprint
  + envelope from the trait); confirm the 2D checker is drive-agnostic. Free-space
  boundary-polygon generalization only if/when #1 needs it (§6).
- **S-PK1d** — (optional, gated by a customer) Tier-B 3D containment + aerial
  envelope.
- **S-PK1e** — RTM / AoU updates + ADR-0017 to Accepted; new-platform safety goals
  marked ENFORCED only here.

## 9. What this stage explicitly does NOT do

- It does **not** modify the frozen talisman (`validate_vehicle_command`).
- It does **not** build any doer (nav / SLAM / orchestration / behavior) — those
  are bounded *after* the trait exists.
- It does **not** add 3D containment (Tier B) or manipulator safety (Tier C, cut).
- It does **not** claim ENFORCED coverage for any new platform until S-PK1e.

## 10. Design decisions (RESOLVED — owner, 2026-06-24)

The three questions are one question seen from three angles — *how much of each
platform's shape leaks into the shared surface* — and all resolve the same way:
**keep the shared surface minimal and behavioral; per-platform shape stays in the
impl.**

**D1 — Verdict: associated type, NOT generalized-angular.**
`trait PlatformKinematics { type Verdict: PlatformVerdict; … }`, with
`Ackermann::Verdict = EnforceAction` **untouched** (the frozen talisman / audit
reason strings / QNX `deny_code_num` demand byte-identity). A generalized
angular verdict is a trap — it is the *second* platform's shape (linear+angular)
masquerading as the abstraction; it already fails omni (`vx, vy, ω`) and aerial
(6-DOF). The shared bound is tiny, so audit / posture / consumer-safe-stop act on
any verdict uniformly without knowing its actuation shape:

```rust
/// Uniform safety view over any platform's per-command verdict.
pub trait PlatformVerdict {
    /// True iff the command was admitted (possibly clamped) — NOT a breach.
    fn is_admitted(&self) -> bool;
    /// Byte-stable audit/deny token when the verdict denies, else None.
    fn deny_reason(&self) -> Option<&'static str>;
}
```

**D2 — Trait surface: `evaluate` + `footprint()` + the few cross-check primitives.**
Exactly the union of what the three existing sibling checks consume — no more:
- `evaluate(&Command, &State) -> Verdict` — the per-command kinematic verdict.
- `footprint() -> VehicleFootprint` — for SG2 containment.
- the small kinematic *limits* the decel-to-stop gate / RSS reason about:
  `max_speed_mps()`, `max_brake_mps2()`, `stop_epsilon_mps()`.

NOT evaluate-only (that starves RSS and the decel gate, which are separate sibling
checks today — keep them separate; give them accessors). NOT the full accessor bag:
**mechanism stays private** (`wheelbase_m`, steering geometry, ICR live inside the
impl's `evaluate`). The moment `wheelbase_m` is on the trait, a checker can read it
and Ackermann has leaked into the abstraction. The trait exposes safety
*primitives*; it hides kinematic *mechanism*.

**D3 — Scalar `KirraKernelGovernor`: the composable PRIMITIVE, not a platform.**
Folding it in is a category error: a scalar channel has **no footprint and no
spatial containment**, so `footprint()` would return a meaningless degenerate
value the containment check must special-case — exactly the silent-degenerate a
safety surface must not have. It is a different *level*: `PlatformKinematics` is "a
robot the governor bounds spatially"; the scalar kernel is "clamp one scalar with a
rate limit" — a primitive the platform impls **compose** (a diff-drive's `evaluate`
scalar-clamps per channel), and the lean ASIL-D / C-FFI surface (#404) keeps using
it directly. One abstraction *for platforms*; one primitive *platforms are built
from*. Cleaner layering than collapsing two different things under a trait with a
meaningless footprint.

### How they compose

- **Ackermann** = verbatim adapter: `Verdict = EnforceAction`,
  `evaluate = validate_vehicle_command`, `footprint = VehicleFootprint::from`.
  Zero behaviour change — provable by the existing talisman tests (S-PK1a).
- **DiffDrive** = sibling impl: its own `(linear, angular)` verdict, composing the
  scalar clamp/rate-limit primitive per channel + the converge-to-zero rule (#407)
  it already has in parko (S-PK1b).
- **Scalar kernel** = unchanged primitive, used by the impls and the FFI; never a
  platform.
