# ADR-0035 companion — v2.0.0 re-export shim removal plan (#1029)

**Status:** Plan (execute at the `>= 2.0.0` MAJOR). **Owner decision required:** cutting
the MAJOR. **Governs:** the 14 remaining ADR-0035 `pub use` re-export shims.

This is the actionable checklist for the final step of the shim-deprecation front
(#1029 / A1). It does **not** authorize itself — removal lands only in the
`>= 2.0.0` MAJOR release, per `docs/VERSIONING_POLICY.md` §2.1 + §5.1, which
co-schedules it with the version-line disambiguation (A4 / #1049). Until then the
shim set is frozen and shrinking-only by the `ci/check_reexport_shims.py` ratchet
(inventory: `ci/reexport_shims_baseline.json`).

## 0. Why this is safe and mechanical

Each shim is a pure `pub use <canonical-crate>::…;` — no logic, no types of its own.
The code already lives in the leaf crate. Removal is a **path rename** at every
consumer (`crate::<mod>::X` / `kirra_verifier::<mod>::X` → `<canonical>::X`) plus
deleting the shim module. The compiler is the ground truth: a missed site fails to
build. No behaviour, verdict, audit-byte, or WCET change is possible — this is the
`3f07976`/`9e26d3c` dead-shim removals repeated for the *live* shims, which differ
only in that they have consumers to repoint first.

## 1. The 14 shims → canonical paths (old → new)

Authoritative inventory lives in `ci/reexport_shims_baseline.json`. Consumer file
counts are the migration effort per shim, in three buckets: **int** = root-crate
library files (`crate::<mod>`), **bin** = the service binaries (`kirra_verifier::<mod>`,
separate compilation units), **xc** = sibling crates + integration tests
(`kirra_verifier::<mod>`).

| Shim (remove) | Canonical path (import instead) | int | bin | xc | Size |
|---|---|----:|----:|---:|---|
| `crate::adapters` | `kirra_industrial::adapters` | 0 | 4 | 0 | S |
| `crate::protocol_adapter` | `kirra_industrial::protocol_adapter` | 0 | 2 | 0 | S |
| `crate::federation_reconciliation` | `kirra_fleet_types::federation_reconciliation` | 0 | 2 | 0 | S |
| `crate::kinematics_sim` | `kirra_core::kinematics_sim` | 0 | 1 | 1 | S |
| `crate::gateway::containment` | `kirra_core::containment` | 1 | 0 | 0 | S |
| `crate::governor_guard` | `kirra_core::governor_guard` | 2 | 0 | 0 | S |
| `crate::federation` | `kirra_fleet_types::federation` | 1 | 1 | 1 | S |
| `crate::attestation` | `kirra_safety_authority::attestation` | 1 | 5 | 1 | M |
| `crate::capture` | `kirra_core::capture` *(needs the kirra-core `capture` feature)* | 3 | 1 | 2 | M |
| `crate::ota_campaign` | `kirra_ota_campaign` | 1 | 5 | 2 | M |
| `crate::fabric::asset` | `kirra_fabric_types::asset` *(keep `crate::fabric::{router,causal_log,governor,…}` — only `asset` is a shim)* | 4 | 4 | 2 | M |
| `crate::gateway::kinematics_contract` | `kirra_core::kinematics_contract` | 8 | 3 | 3 | L |
| `crate::gateway::perception_monitor` | `kirra_core::perception_monitor` | 5 | 15 | 4 | L |
| `crate::verifier_store` | `kirra_persistence` | 16 | 17 | 25 | XL |

`verifier_store` is ~58 file-touches and dwarfs the rest — do it last and alone.

## 2. Recommended ordering

Land in ascending blast-radius so each step is independently reviewable and the
ratchet ticks down visibly. Group by shared reviewer context where natural.

1. **Wave 1 — trivial, bin-or-single-file (14 → 7): ✅ DONE.** `adapters`,
   `protocol_adapter`, `federation_reconciliation`, `kinematics_sim`,
   `gateway::containment`, `governor_guard`, `federation` — repointed to their
   canonical crates and deleted; `max_shims` 14 → 7. (Version stays 1.x; the bump
   itself lands with the final wave.)
2. **Wave 2 — medium (7 → 3): ✅ DONE.** `attestation` → `kirra_safety_authority`,
   `capture` → `kirra_core`, `ota_campaign` → `kirra_ota_campaign`, `fabric::asset`
   → `kirra_fabric_types` — repointed and deleted; `max_shims` 7 → 3. The root lib +
   bins already carried all four leaf crates in `[dependencies]` (nameable from the
   bins, and — confirmed by the compiler — from the root `tests/` integration crates
   too), so the only manifest edit was adding `kirra-safety-authority` to
   `parko-kirra`'s dev-deps for the `verifier-sink`-gated `clearance_e2e` test that
   named `kirra_verifier::attestation`. `capture`'s `kirra-core/capture` feature was
   already on in the SDK manifest.
3. **Wave 3 — large (3 → 1): ✅ DONE.** `gateway::kinematics_contract` →
   `kirra_core::kinematics_contract`, `gateway::perception_monitor` →
   `kirra_core::perception_monitor` — repointed and deleted; `max_shims` 3 → 1. The
   FROZEN kinematics-contract talisman blob stays in `kirra_core` (its Kani pin
   `#[path]`-includes the leaf source, not the shim), so only the import path renamed —
   no talisman touch. `kirra-replay` already carried `kirra-core` as a direct dep; the
   only non-mechanical fix was lifting a nested `kirra_verifier::{ gateway::
   kinematics_contract::… }` group in `tests/actuator_middleware_integration.rs` into a
   standalone `use kirra_core::kinematics_contract::…`.
4. **Wave 4 — `verifier_store` (XL, alone): ✅ DONE.** `verifier_store` →
   `kirra_persistence` across ~64 files (root lib/bins/tests + the four sibling/detached
   consumers). Direct-dep additions: `kirra-persistence` to `kirra-fleet-transport`
   (dev-dep; its use is test-only), `kirra-verifier-pg` (declared direct like its sibling
   leaf deps), `parko-kirra` (optional, wired into the `verifier-sink` feature), and
   `parko-ros2`. One nested `kirra_verifier::{ verifier_store::… }` group lifted out.
   This wave carries the **`2.0.0` version bump**: `Cargo.toml` 1.1.2 → 2.0.0,
   `ci/version_floor.txt` → 2.0.0 (A4 forward ratchet; the ordering guard confirms the
   1.5.0 inversion is permanently cleared), the CHANGELOG `### Removed (BREAKING)` table,
   VERSIONING_POLICY §5.1 flipped to past tense, and `max_shims` → 0. The **release tag
   `v2.0.0` is deliberately NOT pushed here** — that is a separate deliberate release cut.

Each wave can be one PR or several; the ratchet enforces monotonic decrease
regardless of grouping.

## 3. Per-shim mechanical recipe

For a shim module `M` with canonical `C` (e.g. `M = verifier_store`, `C = kirra_persistence`):

1. **Repoint library sites** — in `src/` (excluding `src/bin/`):
   `crate::M::` → `C::` (and `use crate::M;` → `use C as M;` only if a bare `M::`
   alias is genuinely needed; prefer expanding to `C::`).
2. **Repoint bin sites** — in `src/bin/`: `kirra_verifier::M::` → `C::`, and add
   `use <C-crate>` deps to the bin's imports if not already present (they are, since
   the lib already depends on `C`).
3. **Repoint cross-crate + tests** — in `crates/**` and `tests/**`:
   `kirra_verifier::M::` → `C::`. **Add `C`'s crate as a direct `[dependencies]` /
   `[dev-dependencies]` entry** in each such consumer's `Cargo.toml` if it reached
   `C` only transitively through `kirra-verifier` before (e.g. `kirra-replay`,
   `kirra-verifier-pg`, the root `tests/`). This is the one non-mechanical step.
4. **Delete the shim** — remove `src/…/M.rs` and its `pub mod M;` line
   (`src/lib.rs`, `src/gateway/mod.rs`, or `src/fabric/mod.rs`).
5. **Tighten the ratchet** — drop `M`'s entry from
   `ci/reexport_shims_baseline.json` and lower `max_shims` by 1.
6. **Docs** — remove `M`'s row from the CLAUDE.md module map; add a `### Removed`
   line to `CHANGELOG.md` with the old→new path (see §5).
7. **Verify** — `cargo build --workspace --all-targets` (or per-crate if the
   container disk is tight — `cargo clean` first; the full-workspace build pulls
   zenoh/parko and is heavy), `cargo test` for the touched crates,
   `python3 ci/check_reexport_shims.py` (count decreased, ratchet green),
   `python3 ci/check_quality_guardrails.py`, `cargo fmt --check`.

### Submodule + glob caveats

- `adapters` re-exports **submodules** (`kirra_industrial::adapters::{canopen,dnp3,
  ethernet_ip}`); a consumer of `kirra_verifier::adapters::canopen::init_node_map_from_env`
  moves to `kirra_industrial::adapters::canopen::init_node_map_from_env` — same tail,
  new crate root.
- Most shims are `pub use C::*` globs. After removal, an `use C::*` at the consumer
  keeps a wildcard import; prefer naming the specific items brought in, which the
  compiler will require you to resolve anyway.
- `fabric::asset` is the **only** shim under `crate::fabric`; do not touch
  `fabric::router` / `causal_log` / `governor` (real modules).

## 4. End state

- `ci/reexport_shims_baseline.json`: `max_shims: 0`, `shims: {}`.
- The ratchet flips from "shrinking-only" to a permanent **zero-tolerance** guard —
  any future `pub use <crate>::…` shim module reds CI, so the indirection cannot
  return. (Keep `ci/check_reexport_shims.py` in the guardrails job.)
- ADR-0035 §"Shim deprecation" and `VERSIONING_POLICY.md` §5.1 updated to "removed
  in v2.0.0" past tense; the A4 version-line acknowledgment (§2.1) clears the same
  release.

## 5. External-consumer migration guide (CHANGELOG `### Removed`)

Downstream users importing via the root crate must repoint to the leaf crate and
add it as a dependency. Ship this table in the v2.0.0 CHANGELOG:

```
### Removed (BREAKING)
The ADR-0035 re-export shims are gone. Import from the leaf crate instead:

  kirra_verifier::verifier_store::*          → kirra_persistence::*
  kirra_verifier::gateway::kinematics_contract::* → kirra_core::kinematics_contract::*
  kirra_verifier::gateway::perception_monitor::*  → kirra_core::perception_monitor::*
  kirra_verifier::gateway::containment::*    → kirra_core::containment::*
  kirra_verifier::capture::*                 → kirra_core::capture::*   (enable feature "capture")
  kirra_verifier::kinematics_sim::*          → kirra_core::kinematics_sim::*
  kirra_verifier::governor_guard::*          → kirra_core::governor_guard::*
  kirra_verifier::attestation::*             → kirra_safety_authority::attestation::*
  kirra_verifier::adapters::*                → kirra_industrial::adapters::*
  kirra_verifier::protocol_adapter::*        → kirra_industrial::protocol_adapter::*
  kirra_verifier::ota_campaign::*            → kirra_ota_campaign::*
  kirra_verifier::fabric::asset::*           → kirra_fabric_types::asset::*
  kirra_verifier::federation::*              → kirra_fleet_types::federation::*
  kirra_verifier::federation_reconciliation::* → kirra_fleet_types::federation_reconciliation::*

Add the named crate to your [dependencies]; the types are byte-identical (they were
already the same types via the shim).
```

## 6. What this does NOT touch

- The `AppState` decomposition (the façade slices 3a–3k) — separate, already
  materially complete; this plan is only the shim *layer's* teardown.
- Any non-shim module that happens to re-export a few items alongside real code
  (the ratchet's `is_reexport_shim` excludes those; they are not in scope).
- Wire formats, schemas, verdict logic — none are involved in a path rename.
