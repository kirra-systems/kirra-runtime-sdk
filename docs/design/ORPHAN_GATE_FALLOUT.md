# Orphan-gate consumer-attribution audit — the fallout report

**Status: MEASUREMENT ONLY. No gate behaviour changes in this document or the
commit that adds it.**

`ci/check_orphan_cores.py` is the wire-or-delete guard: a `pub mod` at a crate
root must gain a non-test consumer, or be listed in
`ci/orphan_cores_baseline.json` with a justification. It decides *consumed* by
matching identifiers in consumer sources.

Tier 4 produced a concrete instance of that detector crediting a module on
evidence that does not establish a consumer path. This audit measures how far
that reaches **before** anything about CI changes, per the tier-order rule the
boundedness gate established: measure, classify, disposition — then tighten.

Instrument: `ci/audit_orphan_gate.py` (not run by CI, changes no verdict). It
replays `is_consumed`'s own rules but records every hit instead of
short-circuiting on the first, then asks a question the gate never asks.

---

## The question the gate never asks

> Could the crediting file **possibly name this module at all?**

A reference in crate B can only consume crate A's module if B depends on A, or B
*is* A. The gate's `path` rule is `\b{mod_name}::` — not scoped to a crate — so
`state::foo` anywhere in the workspace credits every root module named `state`.
Where no dependency edge exists, the credit is not a judgement call. It is
refuted by the Cargo graph.

## Counts

| | |
|---|---|
| root `pub mod`s scanned | **258** |
| credited as consumed today | 239 |
| reported orphan today (all baselined; gate is green) | 19 |
| **hidden orphans — credited, but no evidence survives attribution** | **3** |
| modules retaining valid evidence after both candidate rules | 236 |

Evidence lines by attribution: `same-crate` 1184 · `cross-crate-with-dep` 814 ·
**`PROVABLY-FALSE` 159**. The 159 refuted lines are concentrated: most modules
carrying some refuted credit also carry valid credit, so only 3 modules rest on
refuted evidence alone.

**The fallout is 3 modules. There is no red wall.**

---

## Three blind-spot classes, one representative case each

### A — the `path` and `item` rules are not crate-scoped

**`parko_ros2::pointcloud2_shim`** — 680 lines, a ROS `PointCloud2` decoder. Its
only reference outside its own file is `pub mod pointcloud2_shim;` in `lib.rs`.
It is credited because it exports a constant `INT8`, and `parko-core` writes
`PrecisionMode::INT8` — a variant of an unrelated enum, in a crate that does not
depend on `parko-ros2` at all.

The existing `ambiguous_item_names` guard cannot see this: it discounts names
exported by two *scanned modules*, and `PrecisionMode::INT8` is an enum variant,
not a root-module free item. This is the same shape as the
`entity_projection`/`fold_all` bug already recorded in the gate's docstring —
the guard was not weak in general, only blind to a name it could not attribute.

**`kirra_planner::lanemap`** — same class, different collision. Two crates own a
module called `lanemap`; `kirra-map`'s own `use crate::lanemap::{…}` credits
`kirra-planner`'s.

### B — a single-line re-export skip does not skip a wrapped re-export

**`parko_core::backend_selector`** — credited by exactly two lines, both inside
its own crate's `lib.rs`:

```rust
pub use backend_selector::{
    backend_permitted, current_platform, descriptor_from_env_str, register_backend_factory,
    BackendFactory, BackendSelector, TargetPlatform, KIRRA_BACKEND_ENV,
};
```

`REEXPORT_RE` is line-anchored. It skips line 1 and then credits the item names
`rustfmt` wrapped onto lines 2–3. A re-export is a re-export however it is
formatted — this is precisely the "shelf with a label" the gate says it
excludes, admitted through its own front door.

Confirmed unwired: zero non-comment references to any of its eight exported
items anywhere in the repository.

### C — textual reachability is not integration (observed, not yet measured)

During Tier 4 box 3b, replacing `render_explanation(explanation)` with a
hardcoded string left the orphan gate **green**: the `use crate::explain_render::{
render_explanation, Narration};` line still referenced the module, and `Narration`
was still genuinely used. The module was consumed; its *core* was dead.

This axis is **not measured here** and the report does not claim otherwise.
Measuring it needs per-ITEM liveness, not per-module — a materially bigger
instrument, and arguably a different gate. Recorded so it is not mistaken for
something this audit covered.

---

## Disposition of every exposed candidate

| Module | Bucket | Disposition |
|---|---|---|
| `parko_ros2::pointcloud2_shim` | **genuinely orphaned** | Baseline it. Its three siblings — `image_shim`, `odometry_shim`, `radar_shim` — are *already* baselined with the identical justification (awaiting the sensor integration, external track MGA G-4). It escaped only via the `INT8` collision. |
| `parko_core::backend_selector` | **genuinely orphaned** | Baseline it. The backend registry awaits a backend crate calling `register_backend_factory`; none does. Same family as the already-baselined `parko_core::detector`. Heals when a backend registers. |
| `kirra_planner::lanemap` | **intentionally standalone** | A 6-line back-compat re-export shim (`pub use kirra_map::lanemap::*;`, de-monolith Stage 6b). Author's call: baseline as an intentional shim, or delete. Note its stated purpose — keeping `crate::lanemap::*` paths working inside `kirra-planner` — is not currently exercised by any file. |
| — | **false positive** | None. No currently-credited module was found to be wrongly *reported*; the errors all run the other way, toward hiding orphans. |
| — | **consumed, detected only textually** | Not measurable by this instrument — see class C. |

The 19 modules the gate reports as orphans today are all baselined with
justifications and were re-confirmed to carry zero evidence. No change.

### A cross-gate scope gap, noticed in passing

`ci/check_reexport_shims.py` enforces `max_shims: 0` — zero tolerance for
`pub use <crate>::…` re-export shim modules — but `SRC_ROOT` is the **root
crate's `src/` only**. `kirra_planner::lanemap` is exactly such a shim, living in
`crates/`, and is invisible to it. Recorded, not acted on: widening that gate is
its own measurement.

---

## What the stronger test should be, derived from the cases

Let the measured cases decide, and they point at two changes that are cheap,
mechanical, and jointly expose 3 modules:

**Rule A — attribute every reference to a crate.** A hit counts only if the
crediting file's crate can name the module's crate: same crate, or a Cargo
dependency path to it. Exposes 2. This subsumes the `ambiguous_item_names`
patch-by-patch approach with a structural fact instead of a name heuristic.

**Rule B — skip the whole `use` statement, not its first line.** Attribute
continuation lines to the statement that opened them, and keep `pub use`
(a shelf) distinct from plain `use` (an import, which *is* consumption).
Exposes 1.

Both move the gate from *"the identifier appears somewhere"* toward
*"a consumer that could name this module referenced it in code shape"* — which
is what the existing `is_code_shaped` was already reaching for, one level up.

## Sequencing

1. **This report.** No gate change. ← you are here
2. Land the disposition: baseline the two genuinely-orphaned modules with
   justifications, decide `lanemap`.
3. Only then implement rules A and B, with the self-tests carrying the three
   measured cases as fixtures, so the gate's non-vacuity is anchored on real
   defects rather than invented ones.

---

## Two self-corrections in the instrument, kept

Both were caught by checking the instrument's own output against the source
rather than trusting it, and both would have put false findings in this report.

**Crate ownership read from `lib.rs`.** `kirra-wcet-bench` is bin-only, so its
files fell through longest-prefix matching to the repo-root crate, and its
perfectly ordinary `kirra_timing::evt::estimate_pwcet(..)` call was reported as
unattributable. Ownership is now read from manifests. Removed one false finding.

**Per-rule fallout recomputed from a truncated sample.** The JSON caps evidence
at 12 lines per module; a first pass recomputed the per-rule exposure counts
from that cap and reported **9** exposures where there are **2** — a module with
200 hits whose first 12 happen to be refuted reads as exposed while hits 13+ are
fine. Now computed over the full hit list.

The second is the same defect class this audit exists to find: a measurement
reading a truncated sample as the whole population. Worth the paragraph.
