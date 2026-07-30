# Talisman change plan — #1242 lateral-envelope composition

**Status: PLAN ONLY. No implementation.** The subject function
(`kirra_core::kinematics_contract::validate_vehicle_command`) is inside the
frozen kinematics talisman, whose pin is a git blob hash. This document answers
the questions that must be settled before it is edited, in order, and records
what evidence each step produces.

Companion documents: `CLAMP_APPLICATION_INVENTORY.md` (blast radius — complete),
`GOVERNOR_INTEGRITY_EVIDENCE.md` §2 (the pin), and the regression test
`crates/kirra-core/tests/speed_cap_lateral_envelope.rs` (red by design).

Compiled against `6a93c964`.

---

## The governing invariant

> Priority decisions may accumulate restrictions, but **no intermediate priority
> may finalize an executable command**. The final action must be composed once
> from values constrained by every applicable permanent bound.

This is the general statement of what #1242 violates, and it is worth keeping
above the specific defect: the speed-cap branch finalizes. Any future priority
added with an early `return` breaks the same invariant, which is why the proof
below is stated over *all* executable exits rather than over that one branch.

## Step 0 — Can the safety property be expressed in Kani?

**Partly. Not in its numeric form. Yes in a structural form that catches this
defect class.** This is the finding that shapes everything below, which is why
it comes first.

### What is already in place

`verification/kani/src/lib.rs` `#[path]`-includes the talisman **verbatim**:

```rust
#[path = "../../../crates/kirra-core/src/kinematics_contract.rs"]
```

So `validate_vehicle_command` is already under CBMC. A new harness goes in
`proofs_kinematics.rs` and needs **no change to the talisman** — which matters,
because adding proof modules inside it is forbidden.

### Why the numeric form is not provable

The property as stated depends on `a_lat = v²·|tan δ| / L`. `tan` is a
transcendental, and `GOVERNOR_INTEGRITY_EVIDENCE.md` already records this exact
limit as honest scope for the existing proofs:

> the P6 `tan`/`atan` path is excluded (transcendentals; covered by MC/DC +
> proptest)

Nothing about #1242 changes that. Promising a proof of the numeric envelope
property would be over-claiming, and would contradict a scope statement the
safety case already relies on.

### What IS provable, and why it is sufficient for this defect

The defect is **not** an arithmetic error in P6. It is that the speed-cap branch
**never reaches** P6. That is a control-flow property, and control flow is
exactly what CBMC is good at:

> **P-COMPOSE.** Every non-`DenyBreach` return of `validate_vehicle_command` is
> produced by the single terminal composition `match (v_clamped, delta_clamped)`.
> Equivalently: after the NaN/Inf and `delta_time_s` guards, no path returns an
> executable action by any other route.

P-COMPOSE is `tan`-free — it constrains *which code path returns*, not what the
transcendental computes. And it fails today: the Priority-2 early return is a
second executable exit. Proving it would have caught this defect, and would
catch any future priority added with the same shortcut.

Two companions, also `tan`-free:

> **P-CAP.** For every executable return, `|linear| <= effective_max_speed_mps()`
> within tolerance.

> **P-RACK.** For every executable return, `|steering| <= max_steering_deg`
> within tolerance.

P-CAP matters for a specific reason given in Step 3: the naive fix violates it.

**P-RACK is the sharpest tan-free consequence of the defect, and it was found
while writing the harnesses rather than while writing the plan.** The Priority-2
early return skips P5a as well as P6, so the RAW steering demand is executed.
Measured against today's kernel:

```
demand  50 deg -> ClampLinear(5.225), executed steering  50 deg  (rack 35)
demand  80 deg -> ClampLinear(5.225), executed steering  80 deg
demand 200 deg -> ClampLinear(5.225), executed steering 200 deg
```

This is worse than the lateral-envelope case and easier to prove. The lateral
envelope is a dynamic bound — exceeding it is aggressive. The rack limit is a
**physical hard stop**: a 200 deg demand is not achievable by the mechanism at
all. And because it is a pure magnitude comparison, it carries none of the
transcendental baggage that keeps the numeric property out of Kani.

### P-COMPOSE is not directly expressible — state its shadow instead

Correcting this plan's own earlier wording: Kani cannot assert *which* `return`
executed, so P-COMPOSE cannot be written as a harness. What is expressible is
its observable **shadow** — every executable return must satisfy every bound the
pipeline should have applied (P-CAP, P-RACK, and the numeric envelope). A
priority that finalizes early necessarily skips some of those bounds, so the
conjunction detects the violation even though no assertion mentions control flow.
P-CAP and P-RACK together already falsify the current control flow; that is the
form the proof takes.

### What the numeric property gets instead

The established fallback in this repo for a property whose exact instance
exceeds what the solver can do is an **exhaustive concrete mirror** — the R2
pattern: a full grid walk swept along every parameter axis, run as a normal test
and gating per-PR while the symbolic instance runs weekly or not at all. The
numeric envelope property should be discharged that way, extending the existing
four-angle regression into a swept grid over speed × steering × cap × wheelbase.

**Net claim to make, and no more:** *all valid kernel inputs across every
executable return branch* for the structural properties, plus an exhaustive
concrete grid for the numeric one. That already dominates the four-angle sweep
and can expose branches nobody has thought to probe. It is not "all f64", and it
should not be described that way.

### The grid and P-COMPOSE are not substitutes

To be stated explicitly in the PR, because the temptation is to present whichever
is cheaper as covering the other:

| | Proves | Blind to |
|---|---|---|
| concrete numeric grid | the returned values satisfy the envelope **at sampled valid points** | a branch nobody sampled; an unsampled parameter combination |
| P-COMPOSE | **no branch can bypass** the common enforcement composition, over the whole bounded valid-input domain | whether the composed arithmetic is numerically right — it never evaluates `tan` |

They fail in opposite directions. A grid can pass while a new early-return
branch goes unsampled; P-COMPOSE can pass while the P6 arithmetic is wrong.
Presenting either as sufficient alone would misdescribe the evidence, and the
#1242 defect is exactly a case the grid alone could have missed — it went
undetected for as long as it did because nobody sampled the speed-cap branch
with a steering demand attached.

### Step 0 addendum — the exclusion was not free, and it is now a model

Written after the harnesses were built and run, because the plan above got one
thing wrong in a way worth recording rather than editing away.

Step 0 said the `tan`/`atan` exclusion "costs nothing here" because the new
properties are `tan`-free. That was true of the *assertions* and false of the
*paths*. Kani fails a harness whenever an unsupported foreign call is
**reachable**, regardless of whether any assertion depends on its value. K1–K5
were written when Priority 2 RETURNED, so over-ceiling commands never reached
P6 and the exclusion cost nothing. Making P2 accumulate — the entire point of
this change — puts P6 on those paths. The result was four failing harnesses,
including **K3, which had been passing for the whole life of the proof set**:

```
Failed Checks: call to foreign "C" function `tan` is not currently supported
  File: .../library/std/src/sys/cmath.rs, line 20, in std::f64::<impl f64>::tan
  Location: crates/kirra-core/src/kinematics_contract.rs:681:52
VERIFICATION:- FAILED
```

No assertion was violated. This is a tooling boundary moving under a scope
change, and it is a general lesson for the talisman: **a change that widens
which paths are reachable can break an unrelated existing proof**, so "does the
new property reach a construct CBMC cannot model?" is the wrong question. The
right one is "does the change put such a construct on any path an existing
harness quantifies over?"

There is a second, subtler obstruction on the same paths. `v.powi(2)` lowers to
`llvm.powi.f64`, which CBMC models as the **uninterpreted** builtin
`__builtin_powi`. `v2` therefore becomes an arbitrary double, the P6 entry guard
`v2 > 1e-6` becomes undecidable, and `tan` is reachable even where the enforced
speed is orders of magnitude below the threshold. This one cost real time: a
diagnostic harness that pinned the ceiling at 0.0005 m/s (`v2 = 2.5e-7`,
two and a half orders below the guard) was built specifically to test the
reachability explanation, and it FAILED — which read as a refutation and led to
the reachability diagnosis being discarded. It was a broken instrument. Once
`powi` was modelled it passed, and the original diagnosis was correct all along.

**Resolution.** The transcendentals are now MODELLED rather than avoided, in the
proof crate only:

| Call | Model | Kind |
|---|---|---|
| `f64::tan` | nondet, `assume` finite (A1) | over-approximation |
| `f64::atan` | nondet, `assume` finite ∧ `|r| <= pi/2` (A2), plus A3 against the recorded `tan` pair | over-approximation |
| `f64::powi` | `x * x`, with an `assert!` that the exponent is 2 | exact |

A1/A2/A3 are stated as individual theorems about the real functions in
`proofs_kinematics.rs` so a reviewer can check them one at a time; A3 —
`|y| <= |tan(x)| ∧ |x| <= pi/2 ⟹ |atan(y)| <= |x|` — is the one P-RACK needs,
and it relates the two calls, so the model is a matched PAIR rather than two
independent nondet functions. Because the stubs are nondeterministic, a proof
that discharges under them holds for **every** implementation satisfying the
postconditions — strictly stronger than a proof about one libm.

Three consequences worth stating plainly:

1. **The talisman is not modified for the prover.** Everything above lives in
   `verification/kani/`. `powi` in particular could have been "fixed" by
   respelling it `v * v` in the kernel — that also works, and was measured to
   work — but it would have put a tooling-driven line into the frozen blob. The
   `powi(2) == v * v` bit-identity it relies on is measured instead, by
   `crates/kirra-core/tests/powi_square_bit_identity.rs`, across subnormals, the
   exponent extremes, the overflow and flush-to-zero boundaries, and the
   operating speed range.
2. **The residual exclusion is narrower and still real.** The P6 numeric
   lateral-envelope VALUE is still not proved. Under the model the proofs never
   evaluate a real `tan`, so they cannot see whether the P6 arithmetic is
   numerically right. That property keeps its concrete-grid discharge, and the
   two must not be presented as covering each other.
3. **`-Z stubbing` is declared in `verification/kani/Cargo.toml`**, not on the
   command line, so `cargo kani`, the per-PR lane and the weekly deep lane all
   get it. A proof cannot silently fall back to the unmodelled foreign calls
   because a flag was forgotten in one invocation.

## Step 1 — Property statement (for the record)

The acceptance property, unchanged from #1242:

> For every returned executable command, the lateral acceleration computed at
> the returned enforced velocity and steering does not exceed the active lateral
> envelope within the approved numerical tolerance.

`DenyBreach` satisfies it vacuously — nothing is executable. The property
constrains output, not availability.

Discharge split, per Step 0:

| Property | Form | Discharged by |
|---|---|---|
| P-COMPOSE | not directly expressible | its shadow: K6 ∧ K7 ∧ the numeric grid |
| P-CAP | structural, `tan`-free | Kani harness K6 (new) |
| P-RACK | structural, `tan`-free | Kani harness K7 (new) |
| numeric envelope | transcendental | exhaustive concrete grid + the existing regression test |

## Step 2 — Domain Kani can soundly cover

Follow the pattern already used by K1–K5 in `proofs_kinematics.rs`, rather than
inventing a second convention:

- **contract parameters** — integer-scaled `kani::any()` with `assume` bounds
  (the existing harness uses `max_speed_raw: u16` assumed `1..=1_000` for
  0.1..=100.0 m/s). This is a *contract-valid* domain, deliberately: an
  unconstrained `f64` contract admits a zero or negative envelope, which is not
  a configuration the system can hold;
- **command fields** — unconstrained `f64` `kani::any()`, as K1–K5 already do;
- **non-finite inputs** — covered by `f64::is_finite` case-split, the existing
  K1–K5 technique. The NaN/Inf guard is Priority 0, so these paths are
  `DenyBreach` and satisfy both properties vacuously;
- **wheelbase** — `assume` strictly positive and bounded. A zero wheelbase makes
  the bicycle model undefined; `SimState::lateral_accel_mps2` already guards
  `wheelbase_m <= 1e-6` and returns 0.0;
- **steering singularity** — `tan` near ±90° does not arise in P-COMPOSE/P-CAP
  (neither evaluates it). For the concrete grid, keep demands strictly inside the
  rack limit, and note that `(90.0_f64).to_radians().tan()` is a huge **finite**
  number (~1.6e16), not infinity — an `is_finite` guard does not catch it;
- **tolerance semantics** — reuse `SimState::lateral_accel_mps2`'s formula and
  `run_simulation`'s `FLOAT_TOLERANCE = 1e-6`, as the regression test already
  does. Do not introduce a second physics interpretation or a second tolerance.

Record honestly in the harness header which of these are `assume`d and why, so a
reader can see the proof's domain without reconstructing it.

## Step 3 — What the implementation actually does today

Read-only survey; nothing changed.

The function is a priority pipeline. **Priorities 3–6 accumulate** into `v` and
`delta` with `v_clamped` / `delta_clamped` flags, then compose once at the end:

```rust
match (v_clamped, delta_clamped) {
    (true,  true)  => EnforceAction::ClampBoth { linear: v, steering: delta },
    (false, true)  => EnforceAction::ClampSteering(delta),
    (true,  false) => EnforceAction::ClampLinear(v),
    (false, false) => EnforceAction::Allow,
}
```

That block is **already correct**, and an in-source comment already states the
intent — "never dropping the velocity correction just because steering …".

**Priority 2 (the speed cap) does not participate.** It early-returns:

```rust
let effective_max_speed = contract.effective_max_speed_mps();
if cmd.linear_velocity_mps.abs() > effective_max_speed {
    let clamped = effective_max_speed * cmd.linear_velocity_mps.signum();
    return EnforceAction::ClampLinear(clamped);   // skips P3/4, P5a, P5b, P6
}
```

So the answers to Step 3's questions:

- the accel-bounded branch **does** compose correctly — it sets `v_clamped` and
  falls through to the block above;
- the speed-cap branch **does** return early, before any steering enforcement;
- shared composition **can** be reused — it exists and needs no change;
- changing this control flow **does** touch more than an omitted steering
  restriction. See the next paragraph. This is the reason the fix is not a
  one-line deletion.

### CORRECTION — the naive fix does NOT breach the cap

An earlier revision of this plan claimed that falling through would let Priority
3/4 overwrite `v` above the ceiling, with a worked example ending
`v = 5.0 + 5.0*0.1 = 5.5`. **That was wrong.** Both P3/4 branches already
terminate in `.clamp(-effective_max_speed, effective_max_speed)`, so the worked
expression evaluates to `5.5.clamp(…) = 5.225` and the cap holds. The `.clamp()`
sat on a continuation line that the grep used to survey the function elided —
a reading error, not a code defect.

Consequence: **the fix is the simple shape after all.** Priority 2 records its
correction and falls through; no additional persistent ceiling is required,
because the accel bound is already ceiling-clamped and can never exceed the
value Priority 2 would have set (which IS the ceiling, the maximum possible).

P-CAP stays in the proof set regardless. It is no longer guarding against this
fix shape, but it pins the `.clamp()` calls that make the shape safe — if a
future change drops one, K6 fails.

Also worth noting for review: no priority between 2 and 6 returns `DenyBreach`
(P3/4, P5a, P5b and P6 all clamp), so routing the speed cap through the pipeline
cannot turn a clamp into a refusal. Availability is unaffected in that direction.

## Step 4 — Smallest fix shape

Preserve priority ownership, do not reimplement P6, and route every executable
outcome through the one composition block:

1. hoist the `v` / `delta` / `v_clamped` / `delta_clamped` declarations above
   Priority 2;
2. make Priority 2 set `v` and `v_clamped` instead of returning;
3. ~~enforce the cap as a magnitude ceiling on the final `v`~~ — **not needed**,
   see the correction above: P3/4's own `.clamp(-effective_max_speed, …)` already
   guarantees it;
4. leave P5a, P5b, P6 and the terminal `match` untouched.

Result: a speed-capped command that also violates P6 returns
`ClampBoth { linear, steering }` with the steering back-solved for the enforced
linear — which is what `ClampBoth`'s existing documentation already promises.

**The proof shape and the fix shape agree**, which is the payoff from doing Step 0
first: (2) and (3) are exactly what P-COMPOSE and P-CAP require, and (1) is the
mechanical enabler. Had the fix been written first, the natural instinct — delete
the `return` — would have satisfied neither.

## Step 5 — Evidence that must precede re-pinning

- [ ] the four-angle regression un-ignored and **green**
      (`speed_cap_lateral_envelope.rs`);
- [ ] the accel-bounded non-vacuity control still green **and still asserting
      `ClampBoth`** — if it stops asserting the variant it no longer proves the
      branch composes;
- [ ] P-COMPOSE and P-CAP harnesses green under `cargo kani`;
- [ ] existing K1–K5 green (the talisman is `#[path]`-included, so any behaviour
      change re-exercises them);
- [ ] exhaustive concrete grid green (speed × steering × cap × wheelbase);
- [ ] **mutation coverage** shows the repaired composition is observable — a
      mutant that restores the early return, or that drops the cap ceiling, must
      be CAUGHT. A fix no mutant can falsify is a fix with no test behind it;
- [ ] caller inventory revalidated as unchanged (expected: four mechanical apply
      sites, no edits — re-run the enumeration, do not assume);
- [ ] full workspace green plus the WCET gate (the pipeline gains work on a
      previously short-circuiting path — measure, do not argue);
- [ ] FDIT matrix re-baselined if the EP-01 released bytes change (apply site 2
      signs what it releases);
- [ ] `capture` mapping reviewed: `ClampBoth` records as
      `CaptureOutcome::ClampLinear` carrying only the longitudinal correction
      (review H1), so commands that shift variant gain a steering correction the
      schema does not record.

## Step 6 — Re-pin procedure

The re-pin is a deliberate act and must read as one. Record, in
`GOVERNOR_INTEGRITY_EVIDENCE.md` §2:

- **old blob hash** and **new blob hash**;
- the **exact source diff** the re-pin covers;
- the **verification commands run** and their output (`cargo kani`, the mirror
  tier, the regression, the workspace suite, the WCET gate);
- **proof results** per property, including which properties are discharged
  symbolically and which by concrete grid — with the transcendental exclusion
  restated so the scope stays honest;
- **regression results**, including the before/after of the previously-ignored
  test;
- **approval**, per `docs/safety/TALISMAN_AMENDMENT_POLICY.md` — either a named
  second-principal approval recorded BEFORE merge (§2.1), or a formally recorded
  **exception** carrying all five mandatory fields (§2.2). "Named reviewer" alone
  is no longer sufficient wording: it does not distinguish the two, and this
  amendment is the case that showed why — it took the exception path;
- an explicit statement that this re-pin acknowledges an **intentional kernel
  behaviour change** — commands on the speed-cap branch that previously returned
  `ClampLinear` now return `ClampBoth` — and is **not** formatting drift or an
  incidental re-hash.

The last line is the one that matters most. A pin whose history contains
unexplained re-hashes stops being evidence.
