# Mutation-Testing Baseline — the Checker Crate (WP-08 / MGA G-6)

**Date:** 2026-07-07 · **Tool:** cargo-mutants · **Scope:** `crates/kirra-trajectory`
+ the checker modules of `crates/kirra-core` (widened #1196 — see §1)
**Status of this document:** living debt register — update on every targeted-kill PR and on every full re-baseline.

## 1. What gates, what ratchets

- **PR gate (CI `mutation-gate` lane):** `cargo mutants --in-diff` over the PR's
  diff of the checker sources — every mutant lying in NEW/CHANGED checker code
  must be killed by the suite, or the PR reds. A PR that does not touch the
  checker skips in seconds. This makes new survivor debt impossible without
  making the pre-existing debt block unrelated work.
- **Gate scope (#1196):** `crates/kirra-trajectory/src` PLUS the checker modules
  of `crates/kirra-core` — `containment.rs` (SG2 drivable space),
  `kinematics_contract.rs` (the frozen talisman), `perception_monitor.rs`
  (Track-C plausibility), `governor_guard.rs`, `frame_integrity.rs`,
  `platform_kinematics.rs`, `contract_consumer.rs`.

  It was `kirra-trajectory` alone until #1196, which left SG2 containment and
  the talisman — safety authority — outside the gate entirely. Four merged
  changes (#1192–#1195) touched `kirra-core`/`kirra-taj` and the lane reported
  "no checker changes" in ~5 seconds each. Measured on #1192's real diff: the
  old scope yielded **0** diff lines and **0** mutants; the widened scope yields
  626 lines and **181** mutants.

  Widening is safe for existing code *because* of `--in-diff`: only lines a PR
  changes are ever mutated, so pre-existing survivors in newly-covered files are
  not tested and cannot red anything. The cost lands only on PRs that change
  those files.

  `kirra-core` is enumerated module by module rather than as a whole crate: it
  also holds non-authority code (capture, kinematics_sim, posture_tracker,
  corridor/trajectory types) that should not carry a mutation obligation.
  Adding a module is a deliberate act.

  The talisman is in scope and that is safe: its blob pin is a CI **shell** step
  (`git hash-object`, rustfmt job), not a Rust test, and cargo-mutants mutates a
  scratch copy — so a mutant cannot trip the pin and produce a false "caught".
- **Debt ratchet (this document + `mutation_baseline_missed_2026-07-07.txt`):**
  the surviving-mutant snapshot only shrinks — targeted-kill PRs retire
  clusters and update the snapshot; a full re-baseline that GROWS the list
  needs the growth explained (usually new code that predates the gate).
- **Test scope** (pinned in `.cargo/mutants.toml` via `test_package`): the
  checker crates' own tests PLUS `kirra-ros2-adapter`'s validation suite, where the
  checker's deepest tests live. This is load-bearing — see §2. NOTE: the scope
  MUST use cargo-mutants' `test_package` key; the earlier
  `additional_cargo_test_args = ["--package", ...]` did NOT change which
  package's tests ran (cargo-mutants defaults to the mutated package's own
  tests only), so the adapter suite silently never executed under the harness.
  The same trap applies one crate over: because an explicit `test_package`
  REPLACES the mutated-package default, `kirra-core` had to be added to the list
  in #1196 — otherwise a kirra-core mutant would be judged by the trajectory +
  adapter suites alone, its own containment/talisman tests would never run, and
  it would report as a survivor.

## 1b. #1196 kill wave — scope widened, survivors partly cleared

Widening the scope surfaced **32 survivors** on #1192's diff, all in the C1
swept-footprint bound. Kill tests and justified equivalence exclusions cleared
the four functions first examined — `max_corner_radius_m`, `wrap_to_pi`,
`segment_sagitta_m`, `chord_clears_corridor` — to zero (92 caught, 1 timeout).

**A full end-to-end run over the whole diff then found 20 more: 145 caught, 20
missed.** The intermediate runs used a `--re` filter naming those four
functions, which silently omitted `segments_intersect` and
`segment_to_segment_dist_sq` — two further helpers the same PR introduced. A
targeted re-run is not evidence about a diff; only the diff-scoped run is. That
mistake is recorded here because it is the same shape as the vacuous-exclusion
one below: a filter that quietly narrows what is being checked while the
result still reads as a pass.

**Open survivors (20):**

| location | count | note |
|---|---|---|
| `validate_trajectory_containment:328` | 1 | `margin_sq = inflated * inflated` → `/`, giving a constant 1.0. STRICTER, so every existing test still passes; needs an admission case clearing by between 0.40 m and 1.0 m. |
| `segment_to_segment_dist_sq:619-627` | 5 | the four-way min selection |
| `segments_intersect:649-650` | 14 | the orientation-sign comparisons |

`segments_intersect` is load-bearing, not an optimisation: two segments crossing
in an X have all four endpoint-to-segment distances non-zero (a unit X gives 1.0
each), so without the crossing test a chord that cuts a boundary reports ample
clearance. The killing case is therefore a chord with its FIRST endpoint inside
(so PNPoly admits) crossing an edge whose endpoints are all far from it.

Each round needed a different insight, and the ones that mattered were things
reasoning got wrong and measurement got right:

1. **Composed tests do not cover helper arithmetic.** #1192's tests drove
   `validate_trajectory_containment` and asserted verdicts, against corridors
   wide enough that a wrong helper never flipped one. `max_corner_radius_m -> 0.0`
   survived — it shrinks the sagitta and makes the bound LESS conservative.
   The dense-sweep oracle missed it too: it only samples geometries where the
   sagitta is small, so a wrong sagitta still passes.
2. **`a` at the origin hides differencing.** `b - a` is indistinguishable from
   `b + a` there. The sagitta tests now use an offset frame.
3. **PNPoly is only load-bearing far outside.** A point just outside an edge
   fails the distance test anyway, so it pins nothing. The killing cases are
   chords far beyond the corridor that clear every edge by a wide margin.
4. **Axis-aligned corridors make `x_cross` unreachable.** Every crossing edge
   has `e1.x - e0.x == 0`, so the formula collapses to `e0.x` regardless of its
   `*` and `/`. True of the rectangle AND of the L-bend — the slant, not the
   non-convexity, is what exercises the interpolation.

### Finding the last four by search, not argument

Two earlier geometric predictions were wrong, so the remaining survivors were
resolved by exhaustively searching three corridor shapes (rectangular, L-bend,
slanted parallelogram) for points where each mutant flips the inside/outside
verdict, recording how far each such point sits from the boundary. The two
answers differed, which is the point of the method:

- **`x_cross` division (`/` -> `*`, `/` -> `%`) — KILLED.** Points exist that
  flip the verdict while staying far from every edge: (299, 7) with 201 m of
  clearance and (76, 20) with 6.9 m, both genuinely outside the slanted band
  and both reported INSIDE by the mutant. Asserted in
  `the_x_cross_interpolation_decides_points_a_wrong_operator_would_admit`.
- **Half-open vertex comparison (`>` -> `>=`) — EQUIVALENT.** It differs only
  where a point's y coincides with a boundary vertex's y, i.e. exactly ON the
  boundary. The function returns `inside && min_dist_sq >= margin_sq`, so at
  clearance 0 the conjunction is false whatever `inside` says. Across all three
  shapes every differing point had clearance EXACTLY 0.0 and there were none
  elsewhere.

That the same search killed one pair and cleared the other is what makes the
equivalence claim credible — it separates "actually equivalent" from "not yet
killed" instead of excusing both. The same search should be pointed at the 20
remaining survivors rather than reasoning about their geometry.

### Equivalence exclusions added (#1196)

Each argued in `.cargo/mutants.toml`: the straight-segment epsilon (both
branches return exactly `0.0` because `1 - cos` underflows below it), the
front/rear corner selector (`>` vs `>=` return the same value when equal), the
sagitta's early finiteness guard (redundant with the final
`!sagitta.is_finite()` check), the PNPoly ray DIRECTION (`<` vs `>` — ray
casting is direction-independent for a simple polygon), the half-open vertex
comparison above, and two exact-tie comparisons of the same class as the
existing `CenterlineFrenet::project` entry.

### A near-miss worth remembering

The exclusion `"replace || with && in segment_sagitta_m"` reads as a literal but
is a REGEX: `||` is alternation with an empty branch, matching every mutant
description. It silently excluded all 399 containment mutants — a green, vacuous
gate on the exact file the scope was widened to cover. It parses as valid TOML
and would have survived review. The lane now lists mutants first and FAILS if a
non-empty checker diff yields zero, which catches the whole class.

## 2. The scoping lesson (measured)

| Run | Test scope | Mutants | Caught | Missed | Unviable |
|---|---|---:|---:|---:|---:|
| run 1 | `kirra-trajectory` own tests only | 799 | 454 | **318** | 27 |
| run 2 | + `kirra-ros2-adapter` suite | 799 | 570 | **202** | 27 |

**CAVEAT (2026-07-07):** runs 1 and 2 were measured by invoking `cargo test`
directly with the two `--package` flags — which DOES run both suites. The CI
`mutation-gate` lane, however, drove cargo-mutants whose `test_package` scope
was mis-configured (see §1), so until that fix the LANE effectively measured
own-tests-only. The run-2 (202-survivor) numbers are the correct target once
`test_package` is in force; re-baseline under the fixed harness will confirm.

116 "survivors" in run 1 were scoping artifacts — killed by the adapter suite
(e.g. the `posture == LockedOut` short-circuit at `validation.rs:213`, whose
`==`→`!=` mutant survived run 1 and dies in run 2). Any future mutation run
that omits the adapter suite will overstate the debt by ~50% and must not be
compared against this baseline.

## 3. Genuine survivor debt (191 mutants after the §4 starter kills)

| File | Survivors | Dominant clusters |
|---|---:|---|
| `validation.rs` | 92 | `predictive_rss_breach` (38), `validate_trajectory_slow_capped` (28), occlusion/steering helpers |
| `prediction.rs` | 41 | mode-rollout arithmetic |
| `vru.rs` | 17 | reachable-set arithmetic (bound armed-but-unfed until WP-10) |
| `redundancy_hardening.rs` | 15 | equivalence-tolerance arithmetic |
| `validation_hardening.rs` | 9 | |
| `config.rs` | 9 | `CourierAngularBound::omega_max`, contract-conversion arithmetic |
| `perception_redundancy.rs` | 8 | |

Full list: `docs/testing/mutation_baseline_missed_2026-07-07.txt` (machine
snapshot: run 2's `missed.txt` with `validation.rs` re-measured after the §4
starter kills, then the §4 predictive-rotation cluster — 191 entries).

**Reading the debt honestly:** a surviving arithmetic mutant means no test
distinguishes the correct formula from the corrupted one — usually because
every test drives the code at a degenerate point (zero heading, zero velocity,
axis-aligned frames). Survivors in the CHECKER's decision arithmetic are
test-quality debt against exactly the component whose correctness the safety
case leans on; they are NOT evidence the code is wrong.

## 4. Starter kills (retired with this baseline)

- **`validation.rs:496` ego-frame lateral rotation (3 mutants) + `415:38`
  world→ego position rotation (1 mutant, same root cause).** Every prior
  cut-in test used ego heading 0 (`sin_h = 0`), so corruptions of
  `-sin_h·vx + cos_h·vy` were invisible. Killed by
  `snapshot_rss_lateral_rotation_is_frame_correct_at_nonzero_heading`
  (`crates/kirra-ros2-adapter/tests/validation_tests.rs`): a 45°-heading
  parallel traveler that every corrupted rotation misreads as a phantom cut-in
  (Accept→MRC flip), plus a true diagonal cut-in the delete-`-` corruption
  reads as ~0 lateral motion (MRC→Accept flip).

- **`predictive_rss_breach` ego-frame rotation cluster (6 mutants:
  `769`, `770`, `783`, `817` — `* → /`) + the lateral brake-fraction
  multiply (`827`).** The predictive pass's rotation
  (`dx_ego`/`dy_ego`/`obj_lon_v`/`obj_lat_v`) and its
  `RSS_LAT_BRAKE_FRACTION * max_lateral_accel_mps2` were untested at a
  non-zero ego heading (every predictive test used heading 0, `sin_h = 0`).
  Killed by `predictive_rss_rotation_is_frame_correct_at_nonzero_ego_heading`
  (a 45°-heading diagonal cut-in that a corrupted rotation reads as clear +
  a parallel traveler it reads as a phantom cut-in) and
  `predictive_rss_lateral_brake_parameter_is_load_bearing` (a weak mid-band
  cut-in that admits under the correct brake-min 2.45 m/s² but breaches under
  the corrupted 0.2 m/s²). All 7 verified killed by hand-applied mutation.

## 5. Triage policy for the remaining debt

1. Prefer killing CLUSTERS with one behavioral test at a non-degenerate
   operating point (rotated frames, non-zero speeds, off-axis geometry) over
   one test per mutant.
2. Priority order: `validate_trajectory_slow_capped` decision arithmetic →
   `predictive_rss_breach` (same §4 gating, time-matched) → `prediction.rs`
   mode rollout → `vru.rs` (rises to top when WP-10 feeds the channel) →
   config/conversion helpers.
3. A survivor may be ACCEPTED (left in the snapshot with a written reason)
   only when the mutation is behavior-preserving in context (e.g. a formatting
   or logging path) — never for checker decision arithmetic.
4. Re-baseline (full run, both scopes recorded) after each wave of kills;
   parko-core's RSS primitives are the next crate to bring under the gate
   (separate workspace — needs its own lane scope).

## 6. Accepted EQUIVALENT mutants — EP-08 curved-RSS geometry (`frenet.rs`)

The EP-08 curved-geometry Frenet frame added `src/frenet.rs` plus the
per-class longitudinal-overlap gate. Its arithmetic — projection, tangent,
heading-change, both RSS reference frames, the overlap gate on BOTH the
snapshot and predictive paths — is pinned by exact value tests
(`frenet.rs` unit tests + `validation.rs::rss_frame_tests` +
`validation_tests.rs::{snapshot,predictive}_overlap_gate_*`); the `--in-diff`
gate confirmed those mutants die. What remains are a small set of **provably
equivalent** mutants that no test can kill because they do not change
observable behaviour. Per policy §3.3 (accept-with-reason only when the
mutation is behaviour-preserving), they are excluded in `.cargo/mutants.toml`
with a written justification:

| Mutant class | Location | Why equivalent |
|---|---|---|
| Duplicate-midpoint collapse predicate (`<`→`==`/`<=`/`>`) and its coordinate subtraction (`-`→`+`/`/`) | `CenterlineFrenet::from_boundaries` | The collapse only drops a zero-length centerline segment; `project`, `tangent_at` and `total_heading_change_rad` all already skip a zero-length segment, so every corridor the resampler can produce yields byte-identical output with or without the collapse. |
| Nearest-segment retention `dist2 < best` → `<=` | `CenterlineFrenet::project` | Differs only when two segments are exactly equidistant, where both choices return the same `(s, d)`. |
| Per-class overlap gate `|dy| < overlap` → `<=` | `validate_trajectory_slow_capped` (:553), `predictive_rss_breach` (:911) | Strict-vs-nonstrict on a float comparison differs only at exact bit-equality `|dy| == overlap` — a measure-zero boundary, behaviourally identical for any real input. |

Other EP-08 equivalent mutants were **removed at the source** rather than
excluded: two redundant length pre-checks (`left.len() < 2 || right.len() < 2`
and `pts.len() < 2`) whose job `cumulative_arc` already does fail-closed (a
polyline with < 2 vertices, or a fully-collapsed centerline, has zero total
length → `None`); the length normalization in `total_heading_change_rad` (the
`atan2(cross, dot)` turn angle is invariant to the segment-vector magnitudes);
and the duplicated Err-branch `.min(len - 2)` segment-index clamp in
`sample_at_fraction` / `tangent_at` (unified to one reachable site). These
simplifications delete the equivalent mutants instead of masking them.
