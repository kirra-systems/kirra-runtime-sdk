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
| SQLite schema (forward migration expectations) | `src/verifier_store/` (module; tables in `mod.rs` + per-domain files) |
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
- CI builds on current stable; the MSRV claim is re-verified at release
  time (the release builds use the pinned toolchain expectations of the
  committed lockfiles).

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

## 6. Housekeeping

- `CHANGELOG.md` is updated in the PR that makes the change (an
  `[Unreleased]` section accumulates between releases and is renamed to the
  version section at release time).
- `docs/releases/v*.md` remains the home for long-form release narratives;
  the changelog links to them.
