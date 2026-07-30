# Clamp-application inventory — blast radius for #1242

**Purpose.** #1242 reports that `validate_vehicle_command` returns `ClampLinear`
alone on the **speed-cap** branch, leaving a steering demand that violates the
P6 lateral-acceleration envelope at the enforced speed. That function is inside
the frozen kinematics talisman, so before it is touched this inventory records
every consumer that applies its result, what each does with `ClampLinear`, and
whether anything independently re-checks the envelope.

Compiled against `3ff1b00c` (post-#1240). Method: enumerate every non-test
consumer that pattern-matches `EnforceAction::ClampLinear` and turns it into an
executable command.

---

## 1. Apply sites

Four, and **all four** set the linear velocity and carry the proposed steering
through unchanged. None performs an independent lateral-envelope check.

| # | Site | `ClampLinear` handling | Independent P6 check? | What reaches the actuator |
|---|---|---|---|---|
| 1 | `kirra_core::kinematics_sim::apply_enforce_action` | `linear_velocity_mps: *safe_v, ..cmd.clone()` | no | derated speed + **proposed steering** |
| 2 | `kirra_core::contract_consumer::apply` (EP-01 in-line SHM governor, `decide` / `decide_cycle`) | `c.linear_velocity_mps = v` → `GovernorOutcome::Actuate(c)` | no | derated speed + **proposed steering**, then a release token is minted over those bytes |
| 3 | `gateway::policy_layer::enforce_actuator_safety_envelope` (HTTP `POST /actuator/motion/command`) | `clamped_cmd.linear_velocity_mps = safe_speed`, re-serialise, forward | no | derated speed + **proposed steering** |
| 4 | `kirra_core::kinematics_sim::apply_enforcement` | delegates to (1) | no | as (1) |

Producers, not apply sites — they compute or return an `EnforceAction` and leave
application to the above, so they are **not** in the blast radius:

- `src/fabric/governor.rs::evaluate_command` — routes posture → contract and
  returns the action; the verifier's fabric handler applies it via (1).
- `parko-ros2::containment_gate` — *produces* an `EnforceAction` of its own; it
  does not consume the kernel's. `parko` has no apply site.

## 2. The one independent envelope check

`kirra_trajectory::validation::check_command_conforms` **bound D2**
(`command_within_lateral_envelope`, S1/#1024) re-solves P6 at the command's own
velocity and refuses. This is why the fast-loop path is protected today.

Two limits on that protection:

- it is **gated on `effective_lateral_envelope` being present**. A legacy record
  with `None` falls back to D1 (the rack limit only), where a 34° demand passes;
- it protects only the fast-loop conformance path. Apply sites 1–3 have no
  equivalent, so on those paths an over-envelope lateral acceleration reaches
  the actuator.

## 3. A documented invariant that the defect violates

`apply_enforce_action`'s own contract states:

> the returned command carries the SAFE values and is within envelope **even if
> the caller ignores the action label**

On the speed-cap branch that is false. This matters for where the fix belongs:
callers are *entitled* by this contract to apply the returned pair without
re-deriving anything, so the omission has to be repaired in the kernel rather
than patched per-consumer.

## 4. Is the kernel fix sufficient on its own?

**Yes, for correctness.** Every apply site is mechanical — it applies whichever
variant it is handed and sets exactly the fields that variant carries. A kernel
that returns `ClampBoth { linear, steering }` on this branch is therefore
honoured everywhere without a single caller change. No consumer contains logic
that treats `ClampLinear` as a positive assertion of steering safety; the
structural assumption (`..cmd.clone()`) is about the kernel's **completeness**,
not an independent safety judgement.

Two caveats worth carrying into the change:

1. **Release-token binding (site 2).** The EP-01 station signs the bytes it
   releases. Once the kernel also clamps steering there, the released bytes
   change — correct, and the token covers them by construction, but the FDIT
   fault matrix rows that pin exact released values will need re-baselining.
2. **Capture mapping.** `kirra_core::capture` records `ClampBoth` as
   `CaptureOutcome::ClampLinear` carrying the longitudinal correction (review
   H1). Commands that shift from `ClampLinear` to `ClampBoth` keep the same
   capture outcome but gain a steering correction that the schema does not
   record; check whether the supervised-learning consumer needs that.

## 5. Acceptance property (branch-independent)

> Every returned executable command satisfies the active lateral-acceleration
> envelope, regardless of which priority or clamp variant produced it.

Stated over the *returned pair* rather than per-branch, so it closes the class
instead of the one path.

## 6. Closure evidence required

Talisman work, so the bar is higher than a normal fix:

- [x] direct regression tests for the **speed-cap** path (the currently
      unprotected branch), asserting the returned pair is in envelope —
      `crates/kirra-core/tests/speed_cap_lateral_envelope.rs`. **Red against
      today's kernel** and `#[ignore]`d for exactly that reason; removing the
      `#[ignore]` is the flip that closes this box. Measured today:
      `ClampLinear(5.225)` executes 24 deg at 5.225 m/s → 4.34 m/s^2 against a
      3.5 envelope, and it fails on the SMALLEST demand in the sweep
      (24/28/30/34 deg), so the defect spans the range rather than one angle.
      The accel-bounded companion in the same file is NOT ignored and passes —
      the non-vacuity control proving the property is satisfiable and the oracle
      correct;
- [ ] caller-level tests where practical — sites 1–3 above;
- [ ] Kani K1–K5 re-run, extended if the property is expressible there;
- [ ] intentional talisman **blob-hash re-pin** with the reason recorded in
      `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2;
- [ ] FDIT matrix re-baseline for site 2 if released bytes change.

The regression test uses the simulator's own formula and tolerance
(`SimState::lateral_accel_mps2` + `FLOAT_TOLERANCE = 1e-6`) rather than a new
one, so the envelope is measured exactly as the existing harness measures it.

**Ready-made oracle:** `kinematics_sim::run_simulation` already asserts
`lat_accel <= contract.max_lateral_accel_mps2 + 1e-6` per step and records a
violation description. It is a harness, not a gate — but it would *detect* this
defect today, so it is the natural basis for the speed-cap regression test
rather than writing a new checker.
