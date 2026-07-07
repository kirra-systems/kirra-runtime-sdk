# Mutation-Testing Baseline — the Checker Crate (WP-08 / MGA G-6)

**Date:** 2026-07-07 · **Tool:** cargo-mutants · **Scope:** `crates/kirra-trajectory`
**Status of this document:** living debt register — update on every targeted-kill PR and on every full re-baseline.

## 1. What gates, what ratchets

- **PR gate (CI `mutation-gate` lane):** `cargo mutants --in-diff` over the PR's
  diff of `crates/kirra-trajectory/src` — every mutant lying in NEW/CHANGED
  checker code must be killed by the suite, or the PR reds. A PR that does not
  touch the checker skips in seconds. This makes new survivor debt impossible
  without making the pre-existing debt block unrelated work.
- **Debt ratchet (this document + `mutation_baseline_missed_2026-07-07.txt`):**
  the surviving-mutant snapshot only shrinks — targeted-kill PRs retire
  clusters and update the snapshot; a full re-baseline that GROWS the list
  needs the growth explained (usually new code that predates the gate).
- **Test scope** (pinned in `.cargo/mutants.toml`): the checker's own tests PLUS
  `kirra-ros2-adapter`'s validation suite, where the checker's deepest tests
  live. This is load-bearing — see §2.

## 2. The scoping lesson (measured)

| Run | Test scope | Mutants | Caught | Missed | Unviable |
|---|---|---:|---:|---:|---:|
| run 1 | `kirra-trajectory` own tests only | 799 | 454 | **318** | 27 |
| run 2 | + `kirra-ros2-adapter` suite | 799 | 570 | **202** | 27 |

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
