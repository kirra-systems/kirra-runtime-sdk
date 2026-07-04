# Changelog

All notable changes to the Kirra Runtime SDK are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to the semantic-versioning rules defined in
[`docs/VERSIONING_POLICY.md`](docs/VERSIONING_POLICY.md) (which also defines
the versioned public surface, the MSRV policy, and the deprecation process).

> **Version-numbering note.** The first public release shipped as **v1.5.0
> under the previous "Aegis" branding** (2026-05-23). The rename to Kirra
> restarted the version line at **v1.1.2** (2026-05-27) — so v1.1.2 is
> *newer* than v1.5.0. Versions are monotonic from v1.1.2 onward; a future
> major bump will clear the ambiguity permanently. Release automation
> (`.github/workflows/release.yml`) extracts the section matching the pushed
> tag from this file — a release without a matching section **fails the
> release job** (WS-0.7; no silent "Release vX" fallback).

## [Unreleased]

Everything since v1.1.2 (May 2026 → present). The highlights, by stream —
detail lives in `docs/adr/`, `docs/safety/`, and the PR history:

### Added

- **Doer-checker planning & perception stack** — the swappable DOER
  (`kirra-planner`: geometric Occy, Mick intent seam incl. multi-junction
  `RouteTo`, learned Hydra-MDP-style planners) bounded by the CHECKER
  (`kirra-trajectory` / `kirra-ros2-adapter`: containment, per-pose
  kinematics, the RSS §4 conjunction, occlusion Rule 4, multi-modal
  predictive RSS, True-Redundancy perception cross-check), with the
  lanelet2-lite lane graph (`kirra-map`) and the Taj R2 perception layer
  (`kirra-taj`, safety-weighted eval harness).
- **QNX partition lane (EPIC #270)** — L3 end-to-end two-process
  doer→checker→release harness, POSIX-SHM hypervisor carrier
  (`kirra-hv-carrier`), release-token bridge (`kirra-release-token`,
  ADR-0031), frozen `GovernorContractView` contract channel, WCET
  measurement methodology + CI boundedness gate (`wcet_gate.rs`).
- **Diverse redundancy (CERT-006)** — `GovernorComparator` pairing the
  primary `KirraGovernor` with a structurally diverse shadow; two-axis
  divergence detection; divergence drives the live fleet posture.
- **Learned-doer quantization lane (Q-series)** — ONNX export, TensorRT
  INT8-QDQ on Orin, precision-selection ladder, per-tick perf contract.
- **Operator console & live observability** — `/console` plane
  (posture-exempt observe-and-recover), operator clearance grants,
  governor-routed e-stop requests (ADR-0013).
- **WS-0 truth-alignment series** (the seven-PR GATE A closer):
  - WS-0.1 — parko live RSS wiring; the `safe: true` construction default
    is dead (unfed governors HOLD at zero).
  - WS-0.2 — posture-generation persistence restored (restart-monotonic
    generations; federation ordering claim true again).
  - WS-0.3 — incident-class audit writes are power-loss durable at write
    time (FULL-sync WAL fsync on transitions; kill-after-incident test).
  - WS-0.4 — parko per-tick inference deadline + hung-backend watchdog;
    comparator angular reconciliation capped by the SOTIF MRC `ω_max(v)`.
  - WS-0.5 — `GET /metrics` Prometheus fleet-safety series (posture gauge,
    committed transitions, gate denials by reason, HA promotions, drop
    counters); scrape survives LockedOut.
  - WS-0.6 — cargo-deny supply-chain gate (licenses/bans/sources, both
    workspaces, one policy), CycloneDX release SBOMs, keyless cosign
    signatures over release artifacts and container images;
    `parko/Cargo.lock` is now committed.
  - WS-0.7 — this changelog, the versioning/MSRV/deprecation policy, and
    the fail-instead-of-fallback release-notes rule.
- **WS-1 (#G7) — per-principal API tokens & scoped RBAC (first PR).** A new
  `api_principals` registry (`POST/GET /system/principals`,
  `POST /system/principals/{id}/revoke`, admin-scoped) mints least-privilege
  bearer tokens across four roles (`admin` / `integrator` / `auditor` /
  `operator`). The gated route groups now terminate in a scope layer
  (`src/authz.rs`, `authorize_request`): the identity/integration surface,
  the actuator command, and a NEW read-only `auditor_routes` carve-out
  (audit verify / causal-verify / export) each admit their scoped role in
  addition to the admin token. Tokens are stored ONLY as their SHA-256 (looked
  up by hash, never plaintext); the plaintext is shown once at mint.
  `KIRRA_ADMIN_TOKEN` is RETAINED as the break-glass superuser and its
  fail-closed 503-when-absent root gate is unchanged (INVARIANT #1/#6), so an
  admin-token-only deployment is byte-compatible. (TPM-bound signing-key
  rotation and in-process TLS termination are the follow-up WS-1 PRs.)
  **Unification note:** this DB-backed system SUPERSEDES the short-lived
  env-configured registry (`KIRRA_PRINCIPAL_TOKENS`, #802/#803), which is
  **removed** — one token system, one RBAC model. Migrate an env-registry
  entry by minting a principal (`POST /system/principals`); `readonly` maps
  to the `auditor` role. The #804 admin-action attribution middleware and
  the #805 `KIRRA_REQUIRE_SECURE_TRANSPORT` transport-security gate carry
  over unchanged and compose with the scope layers (transport gate
  outermost; attribution names the resolved API principal or `root`).

### Changed

- Degraded posture is **decel-to-stop-and-HOLD** (issue #70) at all four
  enforcement points, with `ActuatorMotion` deferral under Degraded
  (ADR-0011 Option A) — not a sustained crawl.
- Attestation verifies a **per-node Ed25519 proof** over the challenge
  (issue #73); the admin-asserted HMAC proof is removed.
- Per-class kinematic contracts selected by `KIRRA_VEHICLE_CLASS`
  (fail-closed: no default class; #312).

### Security / supply chain

- Ed25519-signed hash-chained audit ledger with causal chain, key
  rotation, and tamper-evident export; HA epoch fencing closes the
  two-writer actuator window; deny gate + SBOM + cosign (WS-0.6).

## [v1.1.2] — 2026-05-27

First release under the **Kirra** name (see the version-numbering note
above). Full notes: [`docs/releases/v1.1.2.md`](docs/releases/v1.1.2.md).

### Fixed

- **LockedOut authority model (PARK-003, critical):** LockedOut is a hard
  stop (`Deny`, 0.0 m/s) — not an MRC cap. Corrected in the governor,
  property tests, and decision log. The three-tier authority model is:
  LockedOut → 0.0 hard stop (human reset); Degraded → MRC ceiling
  (5.0 m/s); Nominal → full kinematic envelope.

### Added

- **Parko workspace** — behavioral safety governor: RSS per
  IEEE 2846-2022 (longitudinal + lateral safe distance), `RssState` wired
  into the posture engine with recovery hysteresis, software-lockstep
  `GovernorComparator`, ONNX/OpenVINO inference backends behind the
  `InferenceBackend` seam.
- aarch64 cross-compilation support and release packaging.
- 340 new tests in the root workspace; 61 (incl. 70k proptest cases) in
  parko.

## [v1.5.0] — 2026-05-23 (Aegis era)

Published under the previous **Aegis** branding; numerically higher but
*older* than v1.1.2 (see the version-numbering note). Full notes:
[`docs/releases/v1.5.0.md`](docs/releases/v1.5.0.md).

### Added

- Multi-asset safety fabric (per-asset governors, fleet lockout
  propagation, federated causal log).
- ROS 2 safety interlock package (`cmd_vel` → governor → `cmd_vel_safe`).
- Industrial protocol adapters: EtherNet/IP, CANopen, DNP3.
- Ed25519 audit-chain signing with tamper detection.
- React operations dashboard; Helm deployment.
