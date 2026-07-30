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

A companion, also `tan`-free:

> **P-CAP.** For every executable return, `|linear| <= effective_max_speed_mps()`
> within tolerance.

P-CAP matters for a specific reason given in Step 3: the naive fix violates it.

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
| P-COMPOSE | structural, `tan`-free | Kani harness (new) |
| P-CAP | structural, `tan`-free | Kani harness (new) |
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

### The naive fix introduces a NEW defect

Simply replacing the early return with `v = clamped; v_clamped = true;` and
falling through is **wrong**. Priority 3/4 reads the **raw command**, not the
running `v`, and overwrites `v` when the accel bound binds:

```
cmd.linear = 20.0, current = 5.0, dt = 0.1, cap = 5.225, max_accel = 5.0
  P2 (fall-through)  → v = 5.225
  P3/4 speeding_up   → v = 5.0 + 5.0*0.1 = 5.5      ← overwrites, EXCEEDS the cap
```

The result would satisfy the accel bound and violate the speed cap — trading one
envelope breach for another. This is precisely what P-CAP is for, and why it
belongs in the proof set alongside P-COMPOSE.

Also worth noting for review: no priority between 2 and 6 returns `DenyBreach`
(P3/4, P5a, P5b and P6 all clamp), so routing the speed cap through the pipeline
cannot turn a clamp into a refusal. Availability is unaffected in that direction.

## Step 4 — Smallest fix shape

Preserve priority ownership, do not reimplement P6, and route every executable
outcome through the one composition block:

1. hoist the `v` / `delta` / `v_clamped` / `delta_clamped` declarations above
   Priority 2;
2. make Priority 2 set `v` and `v_clamped` instead of returning;
3. enforce the cap as a **magnitude ceiling on the final `v`**, not a one-shot
   assignment, so P3/4 cannot overwrite it upward — the two longitudinal bounds
   compose as a minimum in magnitude, sign preserved;
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
- **reviewer approval**, named;
- an explicit statement that this re-pin acknowledges an **intentional kernel
  behaviour change** — commands on the speed-cap branch that previously returned
  `ClampLinear` now return `ClampBoth` — and is **not** formatting drift or an
  incidental re-hash.

The last line is the one that matters most. A pin whose history contains
unexplained re-hashes stops being evidence.
