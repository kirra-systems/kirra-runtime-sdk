# Versioning, MSRV & Deprecation Policy

**Document ID:** KIRRA-POLICY-VERSIONING-001
**Status:** Active (WS-0.7)
**Applies to:** the whole repository — the root (`kirra-verifier`) workspace,
the `parko/` workspace, release artifacts, and container images.

---

## 1. What is versioned

There is **one product version** for the repository, held in the root
`Cargo.toml` (`kirra-verifier.version`) and released as git tag `v<version>`.
Workspace crates are proprietary and `publish = false`; their individual
Rust APIs are **not** a semver surface — the versioned **public contract**
is what an integrator can touch from outside:

| Surface | Where defined |
|---|---|
| HTTP API of the verifier service (routes, methods, status semantics, JSON shapes) | `src/bin/kirra_verifier_service.rs`, CLAUDE.md route matrix |
| Environment-variable configuration (names, semantics, defaults, fail-closed behavior) | CLAUDE.md env table |
| Prometheus exposition (metric names + label vocabularies) | `src/metrics.rs` (WS-0.5; label values are pinned by test) |
| C FFI (`include/kirra.h`, `src/ffi.rs`) | ADR-0006 Clause 3 boundary |
| Wire schemas: capture schema (`kirra-capture-schema`), governor wire (`kirra-wire-client`), hypervisor contract channel (`GovernorContractView`, frozen `#[repr(C)]`) | crate docs + `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` |
| SQLite schema (forward migration expectations) | `src/verifier_store/` (module; tables in `mod.rs` + per-domain files). **WP-18/G-20**: the schema is now version-stamped via `PRAGMA user_version` and gated by `verifier_store::migrations` (`SCHEMA_VERSION` = current target). `new()` FAIL-CLOSES the downgrade direction — a database whose `user_version` exceeds this binary's `SCHEMA_VERSION` (written by a newer binary) is REFUSED rather than opened (a destructive-migration MAJOR the safety-asymmetry clause below already implies). Additive change → bump `SCHEMA_VERSION` + push a `Migration` step (MINOR); destructive → MAJOR. |
| Release artifact layout (tarball structure, SBOM, signature bundles) | `.github/workflows/release.yml` |

## 2. Semantic versioning rules

Given `MAJOR.MINOR.PATCH`:

- **MAJOR** — any breaking change to the public contract above: removing or
  renaming a route/env var/metric/label, changing JSON or wire shapes
  incompatibly, a destructive SQLite schema migration, an FFI signature
  change, or completing a deprecation (removal).
- **MINOR** — additive, backward-compatible surface: new routes, new env
  vars (with fail-safe defaults), new metric families, additive schema
  migrations, new crates/binaries. **Raising the MSRV is a MINOR bump.**
  Introducing a deprecation (§5) is MINOR.
- **PATCH** — bug fixes and internal changes that do not alter the contract.

**Safety asymmetry (overrides the above):** a change that makes behavior
*more* fail-closed (tightening a bound, denying something previously and
wrongly admitted) may ship in a **PATCH** even though a client could observe
it — safety corrections are never held hostage to compatibility. The
reverse direction (relaxing an envelope, admitting more) is at minimum
MINOR, requires the safety-case review trail, and is never silent. The
CRITICAL SECURITY INVARIANTS in CLAUDE.md are not versionable surface — no
bump of any size may violate them.

### 2.1 The v1.5.0 → v1.1.2 renumbering

The first public release shipped as **v1.5.0 under the "Aegis" branding**
(2026-05-23). The Kirra rename restarted the line at **v1.1.2**
(2026-05-27), so v1.1.2 is *newer* than v1.5.0. Versions are monotonic from
v1.1.2 onward. Tooling that sorts the two tags semantically will order them
wrong; CHANGELOG.md is the authoritative timeline. The next MAJOR release
clears the ambiguity permanently (every active version will then be > 1.5).

**Machine-checked (A4 · #1049).** `ci/check_version_ordering.py` (wired into the
`action-pinning` CI gate) enforces this so the inversion can never recur or go
silent: the root version must stay `>=` `ci/version_floor.txt` (a **forward
ratchet** — a backward bump like 1.5.0 → 1.1.2 reds), and while the line is
still below the 1.5.0 high-water this §2.1 acknowledgment must remain present.
The day a `>= 2.0.0` MAJOR is cut, the guard confirms the inversion is cleared.
**Recommended: cut that disambiguating MAJOR at the next deliberate release** —
it is cheap insurance against version-pinning / SBOM-correlation confusion, and
it is the natural removal milestone for the §5.1 shims below.

## 3. MSRV (Minimum Supported Rust Version)

- **Current MSRV: 1.88** — pinned as `rust-version` in the root
  `Cargo.toml` and in `parko/crates/parko-core/Cargo.toml` (the base crate
  of the parko workspace), and verified empirically against both committed
  lockfiles (`cargo +1.88.0 check --workspace --locked`); the floor is set
  by locked dependencies (`time-core 0.1.9` requires 1.88), not by our own
  code (`u64::is_multiple_of` needs 1.87).
- Older toolchains fail loudly at build time (cargo enforces
  `rust-version`), instead of failing cryptically mid-compile.
- **Raising the MSRV is a deliberate act:** a MINOR version bump, a
  CHANGELOG entry stating the new floor and what forced it, and an update
  to the two `rust-version` fields plus this document. Routine `cargo
  update` must not silently raise the effective floor past the pinned MSRV
  — if it would, either pin the dependency back or raise the MSRV by this
  process.
- `rust-toolchain.toml` pins the BUILD toolchain to 1.94.1 for local/dev AND
  CI: `dtolnay/rust-toolchain` only sets rustup's *default*, and the file
  outranks the default, so CI lanes build on 1.94.1 too — EXCEPT lanes that
  override it with a higher-precedence selector. Two do: the `coverage` lane
  forces `nightly` via `RUSTUP_TOOLCHAIN` (it needs `-Z` instrumentation) and
  the `msrv` lane forces `1.88` via an explicit `+1.88.0`. (The release +
  QNX-judge lanes already request 1.94.1, matching the pin.) None of the build
  lanes *prove* the MSRV — that is the `msrv` lane's job: it runs
  `cargo +1.88.0 check --workspace --locked` (the `+1.88.0` outranks the pinned
  toolchain) against BOTH committed lockfiles (root and `parko/`). A PR that
  reaches past 1.88 reds that lane with a clear message instead of silently
  invalidating the floor; the same command reproduces it locally.

## 4. Release process contract

- Releases are cut by pushing tag `v<version>` (must equal the root
  `Cargo.toml` version).
- `CHANGELOG.md` **must** contain a `## [v<version>]` section. The release
  workflow extracts it for the release notes and **fails the release** if
  it is missing — there is no "Release vX" fallback (WS-0.7 DoD).
- Every release carries: per-target tarballs, CycloneDX SBOMs for the two
  shipped binaries, `SHA256SUMS`, and keyless cosign signature bundles for
  all of the above (WS-0.6). Container images are cosign-signed by digest.

## 5. Deprecation policy

For any element of the public contract (§1):

1. **Announce (MINOR):** mark deprecated in the CHANGELOG (`### Deprecated`)
   and in the surface's documentation; where technically possible, emit a
   startup or per-use `tracing::warn!` naming the replacement and this
   policy. The element keeps working unchanged.
2. **Grace window:** at least **one MINOR release** (and at least 90 days)
   between announcement and removal. Longer for fleet-facing wire schemas —
   a deployed fleet cannot flag-day.
3. **Remove (MAJOR only):** removal or behavior change lands only in a
   MAJOR release, listed under `### Removed`.
4. **Exceptions:** a surface that is itself a safety or security defect
   (fail-open behavior, an unauthenticated mutation) is corrected
   immediately under the §2 safety asymmetry — with a loud CHANGELOG entry
   — rather than deprecated on schedule.
5. **Never deprecated-in-place:** semantics of an existing name are not
   silently repurposed; a changed meaning gets a new name and the old one
   goes through this process.

### 5.1 Extracted-crate re-export shims (ADR-0035) — DEPRECATED, remove at next MAJOR

ADR-0035 relocated many types out of the root `kirra-verifier` crate into lean
leaf crates (`kirra-core`, `kirra-persistence`, `kirra-safety-authority`,
`kirra-policy-types`, `kirra-fabric-types`, `kirra-ota-campaign`,
`kirra-fleet-types`, `kirra-industrial`, `kirra-audit-hash`,
`kirra-release-token`, …). To keep every existing `crate::X` /
`kirra_verifier::X` path resolving during the migration, the root keeps **thin
`pub use` re-export shims** (~20 files whose whole body is a re-export, e.g.
`verifier_store.rs`, `ota_campaign.rs`, `capture.rs`, `federation*.rs`,
`gateway/{cmd_vel,containment,kinematics_contract,perception_monitor,policy}.rs`,
`fabric/asset.rs`, `protocol_adapter.rs`, `adapters.rs`, `attestation.rs`,
`posture_tracker.rs`, `kinematics_sim.rs`, `governor_guard.rs`). Inventory them
with `grep -rlE '^pub use (kirra_|crate::)' src/` (the *thin* ones; a few large
files carry a re-export line but also hold real code).

Per the A1 audit finding (#1029) these shims are **pure indirection**, not a
permanent layer. They are therefore **DEPRECATED as of this policy revision**:

1. **Announce (this document is the announcement):** every thin re-export shim
   above is deprecated. NEW code MUST import from the canonical leaf crate
   (e.g. `use kirra_persistence::VerifierStore`), never the root shim path.
2. **Grace window:** the shims keep working unchanged for the remainder of the
   current `1.x` line (they are pure `pub use`, so they cost nothing at runtime).
3. **Remove (MAJOR only):** the disambiguating `>= 2.0.0` MAJOR (§2.1) is the
   **removal milestone** — that release migrates internal call sites to the
   canonical crate paths and deletes the shims, listed under `### Removed`. This
   is deliberately co-scheduled with the version-line disambiguation so the two
   root-crate cleanups land together. The **actionable step-by-step** (old→new
   path table for all remaining shims, per-shim consumer counts + landing order,
   the mechanical recipe, and the downstream migration guide) is drafted in
   `docs/adr/0035-shim-removal-v2-plan.md`.

**Enforcement.** The shim set is frozen and shrinking-only between now and v2.0.0
by `ci/check_reexport_shims.py` (guardrails CI job; inventory in
`ci/reexport_shims_baseline.json`): a NEW `pub use` shim fails CI, and `max_shims`
only decreases. Three dead shims have already been removed pre-2.0 under the
dead-vs-live rule (a zero-consumer shim carries no live contract):
`gateway/interceptor.rs`, `gateway/cmd_vel.rs`, `posture_tracker.rs` — so the count
is **14** (not the ~20 named above, which predates the sweep). At v2.0.0 the
remaining 14 go and `max_shims` reaches 0.

The larger, separate item this does **not** close is the actual `AppState`
decomposition (still ADR-0035 **Proposed**); that is tracked as its own body of
work (#1029) and proceeds in bounded slices (cf. the completed ADR-0035 Stage
3a–3g). This subsection governs only the shim *layer's* lifecycle.

## 6. Housekeeping

- `CHANGELOG.md` is updated in the PR that makes the change (an
  `[Unreleased]` section accumulates between releases and is renamed to the
  version section at release time).
- `docs/releases/v*.md` remains the home for long-form release narratives;
  the changelog links to them.
