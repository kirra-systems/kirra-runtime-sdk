# Orphan-gate consumer-attribution audit — the fallout report

**Status: COMPLETE. Measurement → dispositions → rules A and B, all landed.
The gate now attributes every reference to a crate and skips whole `pub use`
statements. Numbers below are the "before" measurement that motivated them.**

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
| `kirra_planner::lanemap` | **intentionally standalone → DELETED** | Was a 6-line back-compat re-export shim (`pub use kirra_map::lanemap::*;`, de-monolith Stage 6b). Removed as a measured compatibility removal, not cleanup-by-taste — see the compile-surface check below. The crate-root API is unchanged: the same eight names are now re-exported straight from `kirra_map::lanemap`. |
| — | **false positive** | None. No currently-credited module was found to be wrongly *reported*; the errors all run the other way, toward hiding orphans. |
| — | **consumed, detected only textually** | Not measurable by this instrument — see class C. |

The 19 modules the gate reports as orphans today are all baselined with
justifications and were re-confirmed to carry zero evidence. No change.

### The compile-surface check that authorised the deletion

Required before removing a public path, and run over the whole repository rather
than inferred from the audit's own hit count (the audit measures module CREDIT,
not every spelling a caller could use):

| Spelling | Hits |
|---|---|
| `kirra_planner::lanemap` (any file type) | **0 code** — 2 prose mentions in `docs/COMPETITIVE_PLANNER_ANALYSIS.md` |
| `crate::lanemap` / `self::lanemap` / `super::lanemap` inside `kirra-planner` | **0** — 1 hit, inside the shim's own doc comment |
| bare `lanemap::` inside `kirra-planner` | only the `pub mod` declaration and the crate-root re-export, both removed or repointed |

The crate-root API is byte-for-byte the same set of names — `JunctionContext`,
`Lane`, `LaneControl`, `LaneCorridor`, `LaneEdge`, `LaneGraph`, `Occluder`,
`MAX_ROUTE_LANES` — now re-exported directly from `kirra_map::lanemap`. Workspace
builds clean and every dependent suite passes.

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

1. ~~This report. No gate change.~~ **done**
2. ~~Land the dispositions.~~ **done** — `lanemap` deleted; the other two
   baselined with explicit rationale. ← you are here
3. ~~Implement rules A and B.~~ **done** — landed with **zero new red**: the
   gate reports 21 orphans, 21 baselined, no `[NEW]` and no `[HEALED]`. Runtime
   went DOWN (25 s), because rule A skips whole files before scanning them.

## What landed

**Rule A** — `can_name()`: a hit counts only if the crediting file's crate is
the module's crate, or reaches it through the Cargo graph. Fail-closed on an
unattributable file, for the reason the gate already gives elsewhere: a missed
reference reports an orphan that is not one, which is visible and arguable,
while a false match hides an unwired core.

**Rule B** — `pub_use_lines()`: the whole `pub use` STATEMENT is skipped, not
its first line. Plain `use` continuations are deliberately kept, because
importing *is* consumption and only the `pub` form is a shelf.

## The mutation set, and the control that did not observe

Four mutations, each expected to die on its own control:

| Mutation | Died on |
|---|---|
| rule A disabled (`can_name` → always true) | the rule-A fixture **and** `pointcloud2_shim is an orphan (rule A)` |
| rule B disabled (no `pub use` spans) | the rule-B fixture **and** `backend_selector is an orphan (rule B)` |
| rule A over-reaches (refuse all cross-crate) | both positive controls |
| rule B over-reaches (skip plain `use` too) | **survived at first** — see below |

The fourth **survived**, and the reason is worth keeping. The positive control
imported `BackendChooser` in a wrapped `use` *and* named it again in a function
signature, so when the mutant wrongly swallowed the continuation line, the
signature line still supplied evidence and the test passed. The control existed
and observed nothing.

Fixed by isolating it: the module name now appears **only** on the continuation
line, and the body mentions nothing the module owns. The mutant then dies on
exactly that control.

That is the same defect class this whole audit exists to find, one level up — a
check that looks like coverage and measures something else. It is recorded here
rather than quietly corrected, because "the mutation survived" is the only
evidence that distinguishes a control that works from one that merely runs.

### Why the baselines landed BEFORE the rules

The two entries are for modules the CURRENT gate still credits, so it reports
them `[HEALED]` and advises removing them — the false credit each entry
documents is exactly the credit the gate still counts. That warning is expected,
non-fatal, and written into the justification strings so nobody acts on it.

Landing them first is deliberate: when rules A and B arrive, the two modules are
already dispositioned, so the stronger detector lands with **zero new red**
rather than a wall someone has to classify under time pressure. That is the same
property the boundedness gate's rollout had, and the reason for this ordering.

### The fixture set the stronger detector must classify

After the dispositions, the audit re-measures at 257 modules with the remaining
two mapping one-to-one onto the rules:

| Case | Must be classified as | By |
|---|---|---|
| `parko_ros2::pointcloud2_shim` | orphan (baselined) | rule A alone |
| `parko_core::backend_selector` | orphan (baselined) | rule B alone |
| `kirra_planner::lanemap` | **gone** — cannot regress | deletion |

A detector that merely "finds more things" fails this set: it has to get all
three right, and one of them no longer exists to be found.

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
