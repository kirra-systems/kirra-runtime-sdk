# Angular-Velocity Bound — SOTIF Derivation

**Doc ID:** KIRRA-OCCY-ANGULAR-SOTIF-001
**Issue:** #136
**Replaces:** the H1 placeholder constants
`MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER = 1.5` and
`MRC_ANGULAR_VELOCITY_CEILING_RAD_S = 0.5` (now deleted from
`parko-kirra`).

---

> # ⚠️ DRAFT — pending formal safety-engineer review
>
> This document is a **draft engineering analysis** that a human
> safety engineer must review and validate before it can be treated
> as authoritative. The improvement over the H1 status quo is real
> (defensible values with explicit reasoning where there were none),
> but **this is not yet a validated safety claim.**
>
> The numbers below were derived from first-principles physics +
> cited contact-velocity standards + the existing safety case's
> FTTI budget. They have not been bench-tested on a real platform
> and they have not been signed off by a safety engineer. Treat
> every "we choose X" as a starting point, not a settled value.

---

## 1. Scope

`parko-kirra::KirraGovernor` enforces an absolute upper bound on
`|angular_velocity|` for every command leaving the inference loop
(introduced in H1, issue #134). H1 shipped with two **placeholder**
numeric values flagged `// TODO(SOTIF)`; this document is the
derivation that replaces those placeholders.

The bound is now a function of the proposed command's linear
velocity:

  **ω_max(v) = min(ω_rollover(v), ω_sweep, ω_ftti)**

with `ω_rollover(v)` masked below a low-speed floor to avoid the
v → 0 singularity. The three constraints are derived in §2.

The implementation lives in
`parko/crates/parko-kirra/src/angular_bound.rs`. The integration into
the governor's evaluate path is in
`parko/crates/parko-kirra/src/lib.rs::KirraGovernor::nominal_angular_clamp`
and `apply_mrc_profile`.

## 2. The three constraints

### 2.1 Dynamic rollover (binds at high linear speed)

A rigid platform turning at linear velocity `v` and angular velocity
`ω` follows a circular path of radius `R = v / ω` with lateral
(centripetal) acceleration

```
a_lat = v · ω           [m/s²]
```

Static stability factor for a wheeled body with track width `t` and
centre-of-gravity height `h` (CoG assumed centred above the wheelbase
— see §3.1 for the assumption):

```
a_tip = g · (t / 2) / h           [m/s²]
```

where `g = 9.81 m/s²` is standard gravity.

Setting `a_lat ≤ a_tip`:

```
v · ω ≤ g · t / (2 · h)
⇒ ω_rollover(v) = g · t / (2 · h · v)        for v > 0
```

**v → 0 singularity:** `ω_rollover(v) → ∞` as `v → 0`. The formula
does not apply to in-place rotation (no forward motion means no
centripetal acceleration is built up before the rotation reverses
sign at the control loop's tick rate). The implementation masks
`ω_rollover` below `ROLLOVER_MIN_LINEAR_VELOCITY_MPS = 0.05 m/s`;
sweep + FTTI then bind.

### 2.2 Sweep / contact velocity (binds at low linear speed, including v=0)

The robot's outermost point at radius `r_extent` (the bounding circle
of the platform's footprint, including any cantilevered payload) moves
at tangential velocity

```
v_tangential = r_extent · ω
```

For safe contact with a human bystander, bound this by a safe
contact velocity `v_edge_safe`:

```
r_extent · ω ≤ v_edge_safe
⇒ ω_sweep = v_edge_safe / r_extent
```

**Basis for `v_edge_safe`:** [ISO/TS 15066:2016](https://www.iso.org/standard/62996.html)
§5.5.5 power-and-force-limiting contact-velocity envelopes for
collaborative robots. The standard gives a per-body-region table; the
**conservative end of the range covers vulnerable regions** (head,
neck, face — `v_edge_safe ≈ 0.05–0.10 m/s`). Less vulnerable regions
(upper arm, chest) tolerate up to ~0.25 m/s. **We choose the
conservative end for the default**; integrators with characterized
exposure profiles may relax.

ISO 13482:2014 (personal-care mobile robots) provides an alternative
basis with similar magnitudes; we cite ISO/TS 15066 because it has
explicit per-body-region values and is more commonly referenced in
collaborative-robot safety cases.

### 2.3 Perception / FTTI coupling (often binds at low v)

Rotating fast changes the robot's heading inside one fault-tolerant
time interval. If the heading change exceeds the validity range of
the perception / policy reasoning (the "look-where-you-haven't-looked"
problem), the safety case can no longer reason about what happens in
the next cycle. Bound the heading change to `θ_max` per FTTI `τ`:

```
ω · τ ≤ θ_max
⇒ ω_ftti = θ_max / τ_FTTI
```

**Basis for `θ_max`:** a heading uncertainty above which the
governor's policy-validity argument breaks down. For the parko
inference-loop tick rate, `θ_max = 5° ≈ 0.087 rad` per `τ = 0.1 s`
gives `ω_ftti = 0.87 rad/s`. Tighten for platforms with
narrow-field-of-view sensors or longer FTTI budgets.

### 2.4 Composite bound

```
ω_max(v) = min(ω_rollover(v), ω_sweep, ω_ftti)
```

with `ω_rollover(v) = +∞` whenever `v < ROLLOVER_MIN_LINEAR_VELOCITY_MPS`
(handles the v → 0 case cleanly).

## 3. Assumptions

- **3.1 Centred CoG.** The static stability factor `g·t/(2·h)`
  assumes the CoG is centred above the wheelbase. Off-centre CoGs
  produce a directional rollover bound (tighter in one yaw direction
  than the other). Out of scope for M1; a non-centred CoG should be
  modelled with a directional bound and is filed as a follow-up.
- **3.2 Rigid body. ⚠️ CORRECTION (pre-review hardening, this revision).**
  Suspension travel, tyre sidewall deflection, and payload compliance
  are ignored. A real platform tips at a **lower** `a_lat` than the
  rigid-body threshold (compliance moves the CoG outboard during the
  turn — the same effect NHTSA captures with a dynamic correction to
  the static stability factor). The rigid-body `a_tip = g·t/(2·h)` is
  therefore an **upper bound on the true tip-over threshold**.
  **A previous draft of this section claimed the resulting bound was
  *conservative*; that is backwards.** Enforcing `a_lat ≤ a_tip_rigid`
  permits lateral accelerations up to a ceiling that a compliant
  platform has *already exceeded its real tip threshold* below — so the
  rigid-body rollover bound is **optimistic (permissive), not
  conservative.** Mitigation: apply a **rollover safety factor**
  `k_roll ∈ (0, 1]` so the enforced ceiling is `k_roll · a_tip_rigid`
  (a NHTSA-style dynamic correction; `k_roll ≈ 0.6–0.7` is a common
  starting point pending the tilt-table validation in §8). **Scope of
  impact:** this affects ONLY platforms whose operating envelope reaches
  the rollover-binding regime (`v` above the §6 crossover). For the
  deployed sidewalk courier, sweep binds with a ~4.9× margin below that
  crossover (§6), so the courier bound is **unaffected** by this
  correction — but `k_roll` MUST be applied before any higher-speed
  platform relies on the rollover term. Tracked in §11.
- **3.3 Constant surface friction.** Loss of traction (`μ` drop)
  produces sideslip, not rollover, but it also caps the lateral
  acceleration the platform can build up. The rollover threshold is
  the binding constraint when `μ` is high; sideslip dominates when
  `μ` is low. This bound does not protect against low-`μ` slip
  events — that's a separate constraint (out of scope here).
- **3.4 v=0 rollover masking.** Below
  `ROLLOVER_MIN_LINEAR_VELOCITY_MPS = 0.05 m/s` we treat
  `ω_rollover` as non-binding. Rationale: the lateral acceleration
  `a_lat = v · ω` at very low `v` is bounded by `v` itself; even
  with very high `ω`, the centripetal force built up over one
  control-loop tick is negligible at v < 0.05 m/s. Sweep + FTTI
  bind the angular axis in the in-place-rotation regime; they are
  more appropriate constraints for that physics.
- **3.5 MRC posture factor.** Under Degraded posture we derate
  `v_edge_safe` and `θ_max` by `mrc_posture_factor` (default `0.5`).
  Rollover is **not** derated — the vehicle's geometry doesn't shrink
  in degraded posture. This may need revisiting: a Degraded sensor
  suite means the perception confidence interval widens, which
  arguably means `θ_max` should derate more aggressively than
  `v_edge_safe`. Flagged for safety-engineer review.

## 4. Worked reference example

`PlatformParams::urban_service_robot_reference()` — a small mobile
service robot approximately TurtleBot-4 scale. **This is no longer a
purely illustrative reference: per ADR-0029 it is the deployed
differential-drive sidewalk courier (Rosmaster R2) profile.** The same
numbers are cited-copied into the SDK slow-loop checker
(`VehicleConfig::courier()` / `CourierAngularBound::courier_reference()`,
`crates/kirra-ros2-adapter`) and into the live parko-ros2 node
(`CourierPlatformProfile`, ADR-0029 Phase 2). The two copies are locked
together by the cross-workspace drift guard
(`tests/parko_cited_copy_correspondence.rs`), which reads this crate's
source and fails CI if the SDK copy diverges from these values. So a
change to the table below is a change to the deployed courier bound:

| Parameter | Value | Source |
|---|---|---|
| `track_width_m`     | 0.50 | Typical small mobile base |
| `cog_height_m`      | 0.40 | Battery + chassis at mid-height |
| `robot_extent_m`    | 0.30 | Bounding-circle radius incl. payload |
| `v_edge_safe_mps`   | 0.25 | ISO/TS 15066 upper-arm/chest contact |
| `theta_max_rad`     | 0.087 (≈ 5°) | Sensor FoV + policy validity heuristic |
| `ftti_s`            | 0.10 | parko inference-loop tick budget |
| `mrc_posture_factor`| 0.5  | (this doc, §3.5 — pending review) |

### 4.1 ω_max(v) — Nominal posture

| v (m/s) | ω_rollover (rad/s) | ω_sweep (rad/s) | ω_ftti (rad/s) | **ω_max(v)** | Binding |
|---|---|---|---|---|---|
| 0.00 | — (masked) | 0.833 | 0.870 | **0.833** | sweep |
| 0.10 | 61.3       | 0.833 | 0.870 | **0.833** | sweep |
| 1.00 | 6.13       | 0.833 | 0.870 | **0.833** | sweep |
| 5.00 | 1.23       | 0.833 | 0.870 | **0.833** | sweep |
| 7.50 | 0.817      | 0.833 | 0.870 | **0.817** | rollover |
| 10.00| 0.613      | 0.833 | 0.870 | **0.613** | rollover |

For this platform, **sweep binds across the practical operating
range** (v ≤ ~7 m/s). Rollover only starts binding at extreme speeds
(7+ m/s), well past the urban-service-robot operating envelope.

### 4.2 ω_max(v) — MRC posture (derate by 0.5)

| v (m/s) | ω_sweep_eff | ω_ftti_eff | **ω_max(v)** |
|---|---|---|---|
| 0.00 | 0.417 | 0.435 | **0.417** |
| 1.00 | 0.417 | 0.435 | **0.417** |
| 5.00 | 0.417 | 0.435 | **0.417** |

Sweep (with halved `v_edge_safe`) binds throughout. The Degraded
posture's effective sweep bound `0.4167 rad/s` is about half the
Nominal `0.833 rad/s` — the envelope contracts ~2× when posture
degrades, matching the linear-axis MRC philosophy.

### 4.3 Comparison to the H1 placeholder

| | H1 placeholder | SOTIF-derived (urban ref) | Direction |
|---|---|---|---|
| Nominal | 1.500 rad/s | 0.833 rad/s | **Tighter** (1.8×) |
| MRC | 0.500 rad/s | 0.417 rad/s | **Tighter** (1.2×) |

The H1 placeholder was 1.8× too permissive on the Nominal axis for
the reference platform. The SOTIF-derived value is tighter and now
has reasoning behind it.

## 5. Conservative default — uncharacterised platforms

`PlatformParams::conservative_default()` is intended for deployments
that have NOT yet characterised their platform geometry. Every
parameter is chosen so the resulting `ω_max` is tighter than the
reference platform at every plausible v:

| Parameter | Default | Rationale |
|---|---|---|
| `track_width_m`     | 0.20  | Small base |
| `cog_height_m`      | 0.50  | Top-heavy |
| `robot_extent_m`    | 0.50  | Large payload assumption |
| `v_edge_safe_mps`   | 0.10  | ISO/TS 15066 conservative-end (vulnerable body regions) |
| `theta_max_rad`     | 0.05 (≈ 2.9°) | Tight perception/policy budget |
| `ftti_s`            | 0.10  | Same as urban reference |

Produces `ω_max(0) = min(∞, 0.20, 0.50) = 0.20 rad/s ≈ 11.5°/s` — a
slow, deliberate turn. A misconfigured / unprofiled deployment using
the default produces a tight bound that fails toward safe. Test
`omega_max_conservative_default_is_at_or_below_reference_platform`
pins this property.

## 6. Sensitivity analysis

Each candidate constraint is a **ratio of two parameters**, so its
relative sensitivity to each input is unit magnitude: for `ω = a / b`,

```
(δω/ω) / (δa/a) = +1        (δω/ω) / (δb/b) = −1
```

i.e. a fractional error `ε` in any single input moves that constraint
by `±ε`. No input is amplified.

| Constraint | `ω =` | +1 (looser when ↑) | −1 (tighter when ↑) |
|---|---|---|---|
| rollover | `g·t / (2·h·v)` | `t` | `h`, `v` |
| sweep    | `v_edge / r_extent` | `v_edge` | `r_extent` |
| ftti     | `θ_max / τ` | `θ_max` | `τ` |

### 6.1 Which constraint binds → which parameters matter

The bound is `min(rollover, sweep, ftti)`, so **only the inputs of the
*binding* constraint affect the enforced value**; the others are slack.
Rollover overtakes sweep (becomes the min) when

```
g·t / (2·h·v) < v_edge / r_extent
⇒ v > v× = g·t·r_extent / (2·h·v_edge)
```

For the deployed courier (`urban_service_robot_reference`):

```
v× = 9.81·0.50·0.30 / (2·0.40·0.25) = 7.36 m/s
```

The courier ODD ceiling is ~1.5 m/s — a **~4.9× margin** below the
rollover crossover — so **sweep binds across the entire courier
envelope** (and FTTI, 0.87 rad/s, never binds either since sweep 0.833 <
0.870 for all `v`).

**Consequence — the courier bound de-risks to two well-known inputs.**
Because sweep is the sole binding constraint in the courier ODD, the
enforced bound depends ONLY on `v_edge_safe` and `r_extent`:

- `track_width_m`, `cog_height_m` enter only the **rollover** term,
  which never binds for the courier. **The two assumptions a reviewer
  would scrutinise most — centred CoG (§3.1) and the rigid-body
  optimism (§3.2) — are therefore moot for the courier bound.** They
  re-enter only for a higher-speed platform.
- `v_edge_safe` is a **cited standard value** (ISO/TS 15066, §2.2), not
  a platform measurement → low, well-justified uncertainty.
- `r_extent` is a **directly measurable** geometric quantity (the
  bounding-circle radius incl. payload) → verifiable with a tape
  measure.

### 6.2 The one parameter whose under-measurement is unsafe

Sweep is `v_edge / r_extent`, so **under-measuring `r_extent` inflates
the bound** (permits faster spin than the real edge can safely sweep).
Worked example: if the true bounding circle — with a protruding payload
— is 0.36 m but `r_extent` is configured at 0.30 m, the configured bound
(0.833 rad/s) is **20% higher than the true safe bound** (0.25/0.36 =
0.694 rad/s), and the outer edge sweeps at `0.36·0.833 = 0.30 m/s` —
above the 0.25 m/s contact target.

**Mitigation:** measure `r_extent` to the outermost point including any
cantilevered payload, and round **up**; characterise with an explicit
margin (e.g. +10%). This is the single most safety-relevant parameter
for the courier and is the first acceptance item in §8.

## 7. Hazard traceability & SOTIF acceptance argument

ISO 21448 (SOTIF) framing: an unbounded angular axis is an
*unknown-unsafe* region — the upstream planner (the doer) could command
any `ω`. The bound converts it into a region that is *safe by
construction within the modelled constraints*. Each constraint mitigates
a specific hazardous behaviour:

| ID | Hazardous behaviour | Triggering condition | Mitigating constraint | Residual risk |
|---|---|---|---|---|
| H-ANG-1 | Platform tips over mid-turn | high `v` **and** high `ω` (aggressive cornering) | rollover (§2.1) | low-μ sideslip not covered (§3.3); rigid-body optimism until `k_roll` applied (§3.2) |
| H-ANG-2 | Outer edge sweeps into a bystander | high `ω` at low/zero `v` (in-place spin near a person) | sweep (§2.2) | contact above `v_edge` if `r_extent` under-measured (§6.2) |
| H-ANG-3 | Heading outruns perception validity | high `ω` (heading leaves the reasoned-about FoV within one FTTI) | ftti (§2.3) | only as good as `θ_max` matching the real sensor FoV + policy validity |

**Acceptance argument.** The enforced region `|ω| ≤ ω_max(v)` is
sound *iff* (a) each binding constraint's parameters are correctly
characterised for the platform, and (b) the residual risks above are
either out-of-ODD or separately mitigated. For the courier, (a) reduces
to `v_edge_safe` (cited) + `r_extent` (measured with margin) per §6, and
the rollover/CoG residuals are out-of-envelope (§6.1). The argument is
**not yet complete**: it is contingent on the bench validation in §8,
which is what moves this document from DRAFT to a validated claim.

## 8. Verification & validation plan

§11 item "bench validation" was previously open with no method. This
section makes it actionable: each constraint gets a validation method
and an explicit acceptance criterion. **Until these pass on the target
platform, the bound remains engineering analysis, not a validated
safety claim.**

| Constraint | Validation method | Acceptance criterion |
|---|---|---|
| sweep (H-ANG-2) | Measure the platform's true bounding-circle radius incl. payload at full articulation; command an in-place spin at the enforced `ω_max(0)` and measure the outer-edge tangential speed with a tracking marker. | Measured edge speed ≤ `v_edge_safe` (0.25 m/s courier). Equivalently `r_extent_configured ≥ r_extent_measured`. **First priority (§6.2).** |
| rollover (H-ANG-1) | Tilt-table to find the static tip angle → derive `a_tip_real`; OR steady-state circular drive at increasing `a_lat` until measured load transfer reaches the tip onset. | `a_tip_real ≥ a_tip_rigid` (else set `k_roll = a_tip_real / a_tip_rigid` and re-enforce `k_roll·a_tip_rigid`, §3.2). Only required if the platform's ODD reaches `v×` (§6.1). |
| ftti (H-ANG-3) | Measure the end-to-end perception→policy validity horizon (sensor FoV / tracker latency); confirm `θ_max` ≤ the heading change the pipeline can still reason about. | `θ_max ≤ FoV-derived heading-validity limit` at the measured FTTI. |
| MRC derate (§3.5) | Confirm the Degraded posture's contracted envelope is reached on a real posture transition; review whether `θ_max` needs a steeper derate than `v_edge` under sensor degradation. | Degraded `ω_max` ≤ 0.5× Nominal at the same `v` (already unit-tested); the split is a review decision, not a measurement. |

The pure-derivation and integration tests (§10) already pin the *math*;
this plan covers the *physical correspondence* the math assumes.

## 9. Implementation reference

- **Module:** `parko/crates/parko-kirra/src/angular_bound.rs`
- **Types:** `PlatformParams`, `AngularVelocityBound`,
  `ROLLOVER_MIN_LINEAR_VELOCITY_MPS`.
- **Governor wiring:**
  `parko/crates/parko-kirra/src/lib.rs::KirraGovernor::nominal_angular_clamp`
  and `apply_mrc_profile` both call `bound.omega_max(v_proposed)`
  per tick.
- **Builder API:**
  - `KirraGovernor::new()` — uses the conservative default.
  - `KirraGovernor::with_platform_params(PlatformParams)` —
    integrator passes platform-specific geometry + budgets.
  - `KirraGovernor::with_angular_bounds(nom, mrc)` — back-compat
    v-independent scalar override.
- **SAFETY tag:** `SG8 SG9 | REQ: angular-velocity-bound-sotif`.

## 10. Tests

### Pure derivation (`angular_bound::tests`)

- `omega_max_in_place_rotation_returns_sweep_or_ftti`
- `omega_max_at_v_zero_is_finite` (no singularity leak)
- `omega_max_at_v_below_rollover_floor_ignores_rollover`
- `omega_max_sweep_binds_at_low_v` / `omega_max_rollover_binds_at_high_v` / `omega_max_ftti_binds_when_theta_is_tight`
- `omega_max_conservative_default_is_at_or_below_reference_platform`
- `omega_max_mrc_is_tighter_than_nominal`
- `omega_max_scalar_variant_is_v_independent`
- `omega_max_mrc_at_v_zero_for_reference_platform`
- Param validation tests (`platform_params_validate_*`).

### Governor integration (`parko_kirra::tests`)

- `derived_bound_changes_verdict_between_platforms` — swapping
  PlatformParams changes the verdict for the same command.
- `derived_in_place_rotation_clamps_to_sweep_bound`
- `derived_mrc_in_place_rotation_is_tighter_than_nominal`
- `with_angular_bounds_scalar_back_compat_is_v_independent`
- All 8 H1 enforcement-logic tests (sign preservation, multi-axis
  ClampMotion, sticky behaviour) still pass under
  `legacy_scalar_gov()` (calls `with_angular_bounds(1.5, 0.5)` to
  preserve the H1 numeric values for those tests).

## 11. Open items

1. **Rollover safety factor `k_roll`** *(new — §3.2 correction)*. The
   rigid-body rollover threshold is optimistic for a compliant platform;
   a dynamic-correction factor `k_roll ∈ (0,1]` (≈0.6–0.7 starting
   point) must be applied before any platform relies on the rollover
   term. Moot for the courier (sweep binds, §6.1); blocking for
   higher-speed platforms. Validation method in §8.
2. **`r_extent` under-measurement** *(new — §6.2)*. The one parameter
   whose under-measurement makes the courier bound permissive. Measure
   to the outermost point incl. payload, round up, characterise with a
   margin. First acceptance item in §8.
3. **Off-centre CoG** — current derivation assumes centred CoG (§3.1).
   Directional rollover bound is a follow-up (rollover-regime platforms
   only).
4. **Low-μ sideslip** — not addressed; needs a separate friction-aware
   constraint.
5. **MRC posture factor split** — currently the same `0.5` for both
   `v_edge_safe` and `θ_max`. The argument that `θ_max` should derate
   more aggressively under sensor degradation (wider perception
   uncertainty) deserves separate analysis. Flagged for review (§8).
6. **Per-platform `v_edge_safe` characterisation** — the default
   uses the conservative end of the ISO/TS 15066 table.
   Platform-specific contact exposure profiles (which body regions
   can the platform realistically contact?) should refine this.
7. **Bench validation** — none of the derived numbers have been
   tested on a real platform. §8 now gives the per-constraint method +
   acceptance criteria; executing it on the target platform is what
   moves this document from DRAFT to a validated safety claim.

## 12. Document control

| Field | Value |
|---|---|
| Issue | #136 |
| Status | **DRAFT — pending formal safety-engineer review** (evidence strengthened pre-review: §6 sensitivity, §7 hazard/SOTIF traceability, §8 V&V plan added; §3.2 rollover-conservatism error corrected) |
| Author | engineering analysis (#136); pre-review hardening (ADR-0029 follow-up) |
| Review status | not yet reviewed — DRAFT banner stands until a human safety engineer signs off |
| Cross-refs | KIRRA-OCCY-SPEED-001 (linear analog), KIRRA-OCCY-OPTIONB-001, ADR-0029 (courier angular-channel seam — this is the deployed courier profile) |
| Code | `parko/crates/parko-kirra/src/angular_bound.rs`; SDK cited copy `crates/kirra-ros2-adapter/src/config.rs` (`CourierAngularBound`) |
| Tests | `angular_bound::tests`, `parko_kirra::tests::derived_*`; cross-workspace drift guard `kirra-ros2-adapter/tests/parko_cited_copy_correspondence.rs` |

This revision is a **pre-review hardening pass**: it adds the
analysis a reviewer needs (sensitivity, hazard traceability, a V&V plan
with acceptance criteria) and corrects the §3.2 rollover-conservatism
error. It does **not** constitute review or sign-off — the numbers are
still unvalidated on hardware (§8). The DRAFT banner stands.

---

When this document earns a safety-engineer sign-off, remove the
DRAFT banner at the top, change "Status" to "Reviewed", and add the
reviewer + date in §12.
