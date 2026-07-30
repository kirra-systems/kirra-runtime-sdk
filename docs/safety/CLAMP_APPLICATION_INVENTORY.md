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
- [x] **mutation gate on the talisman diff** — 19 in-diff mutants, 18 caught,
      1 unviable, 0 missed. Four survived the pre-existing suite: three were
      killed by new exact-boundary tests
      (`crates/kirra-core/tests/rate_limit_epsilon_boundary.rs` — they had
      survived because no test had ever landed ON the `± 1e-9` tolerance, not
      because the branch was untested), and one is a TRUE equivalence
      (`effective_max * signum` → `/`, where signum is only `±1.0` so both
      spellings are bit-identical) excluded with its premises pinned by a test
      rather than argued in prose. Recorded in `MUTATION_BASELINE.md` §8;
- [ ] caller-level tests where practical — sites 1–3 above;
- [x] Kani K1–K5 re-run, extended if the property is expressible there —
      **both halves done, and the re-run was not a formality.** Extended:
      K3b (composition reports both axes), K6 (P-CAP) and K7 (P-RACK) are new
      harnesses for the structural half of the acceptance property. Re-run:
      K3, which had passed for the life of the proof set, FAILED — not on its
      assertion but on `call to foreign "C" function 'tan' is not currently
      supported`, because making Priority 2 accumulate put the P6 bicycle model
      on paths K3 quantifies over. Resolved by MODELLING the transcendentals in
      the proof crate (nondeterministic stubs constrained to theorems about the
      real functions) rather than narrowing the harnesses; the talisman is not
      touched for the prover. K1–K6 verify per-PR (K3 22 s, K3b 32 s, K6 45 s);
      K7 is provisionally behind `deep-proofs` with a 306,180-point exhaustive
      grid as its blocking per-PR gate. Full account: Step 0 addendum in
      `TALISMAN_CHANGE_PLAN_1242.md`;
- [x] intentional talisman **blob-hash re-pin** — `ed00f4da` → `6a61b74f`
      (an intermediate `bbfe014b` was superseded when the Priority-3/4 guard was
      reshaped from a wrapped block to two guarded conditions), with
      the reason recorded in `docs/CAPTURE_PIPELINE_SPEC.md` (the authoritative
      pin). **FOUR locations must agree**, discovered one at a time and worth
      listing so the next re-pin is a checklist rather than an excavation:
      (1) the spec doc; (2) `ci/build_safety_case.py`'s `talisman_gate`, which
      greps the doc; (3) the `gateway::provenance` manifest/sidecar tests; and
      (4) the `rustfmt gate` workflow step, which HARDCODED the old prefix in its
      grep and so failed with an empty `pinned ` on the first legitimate re-pin —
      now anchored on the path instead of the value, matching (2)'s convention.
      Reviewer approval remains **PENDING**;
- [ ] FDIT matrix re-baseline for site 2 if released bytes change.

The regression test uses the simulator's own formula and tolerance
(`SimState::lateral_accel_mps2` + `FLOAT_TOLERANCE = 1e-6`) rather than a new
one, so the envelope is measured exactly as the existing harness measures it.

**Ready-made oracle:** `kinematics_sim::run_simulation` already asserts
`lat_accel <= contract.max_lateral_accel_mps2 + 1e-6` per step and records a
violation description. It is a harness, not a gate — but it would *detect* this
defect today, so it is the natural basis for the speed-cap regression test
rather than writing a new checker.
