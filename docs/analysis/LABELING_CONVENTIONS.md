# Issue & PR Labeling Conventions (proposed)

**Status:** Proposed — derived from the labels actually applied across the most-recent 100 issues (observed convention, not invented). Adopt/edit as the team sees fit.
**Date:** 2026-06-29
**Related:** `docs/analysis/ARCHITECTURE_REVIEW_2026-06.md`

This document normalizes the existing label taxonomy, fills two gaps (a `supply-chain` axis and a complete `severity:*` scale), and records two duplicate-label cleanups. It is descriptive first (what's in use) and prescriptive second (how to keep it consistent).

> **Tooling note.** The GitHub integration used can *apply* a label (which auto-creates it with a default gray `#ededed` color and empty description) but cannot set a label's color or description, nor list/rename/merge labels. The three labels added during the backfill effort (`supply-chain`, `severity:low`, `severity:critical`) therefore need a maintainer to set color + description in the repo's label settings, and the duplicate-collapse below must be done in the GitHub UI / API by a maintainer.

---

## Axes

Labels are organized into orthogonal axes. An issue typically carries **one label per relevant axis** (e.g. one `severity:*`, one `ws:*`, one or more component labels).

### 1. Severity — `severity:*`
How bad is it if unaddressed. **Currently sparse** (~9% of issues), so triage has leaned on `critical-path` instead. To make severity a first-class triage axis it must be backfilled and kept complete.

| Label | Use for |
|---|---|
| `severity:critical` | Safety/integrity defect that can cause an unsafe actuation, data-integrity loss, or auth bypass; fix before it governs a real system. *(added during the backfill effort)* |
| `severity:high` | Serious correctness/security gap; fix before the next release/integration. |
| `severity:medium` | Real defect with bounded/mitigated impact; schedule deliberately. |
| `severity:low` | Minor / defense-in-depth / cosmetic / docs. *(added during the backfill effort)* |

Pair severity with `critical-path` when the item also **blocks** other work — the two answer different questions (*how bad* vs *what it blocks*).

### 2. Type / kind
`bug`, `feat`, `enhancement`, `tech-debt`, `docs`, `test`, `adr`, `epic`, `epic-270-child`.
- **Collapse `documentation` → `docs`** (duplicate concept; `docs` is the dominant form).

### 3. Component / area
`occy`, `standards`, `backend`, `qnx-lane`, `console`, `kirra-governor`, `ros2`, `robot`, `infra`, `business`.
- **Collapse `qnx` → `qnx-lane`** (duplicate concept; `qnx-lane` is dominant).

### 4. Domain — `domain:*`
`domain:boundary` (the cross-partition / safety boundary & checker), `domain:autonomy-guest` (planner/perception guest stack), `domain:fleet` (multi-node/federation).

### 5. Workstream — `ws:*`
`ws:governor`, `ws:worldmodel`, `ws:infra`.

### 6. Phase — `phase:*`
`phase:0`, `phase:1`, `phase:2`.

### 7. Triage / status
`owner-action`, `critical-path`, `blocked`, `later`, `sandbox-tractable`, `hardware-blocked`, `upstream`.

### 8. Safety & certification
`safety`, `safety:teleop`, `safety-case`, `cert-evidence`, `review-gate`.

### 9. Security / supply-chain / hardening
`security`, `cybersecurity`, `hardening`, **`supply-chain`** *(added during the backfill effort)*.

`supply-chain` covers: GitHub Actions / base-image **pinning**, **Dependabot**, **digest** pinning, **`cargo audit` / `cargo deny`** / RUSTSEC advisories, dependency-vuln management, **artifact/release signing** (cosign/GPG/sigstore), lockfile / reproducible-build integrity, dependency provenance. Use it **in addition to** `security`/`hardening`, not instead of — it is the axis that makes supply-chain work findable as a body.

---

## Cleanups to perform (maintainer, in GitHub)

1. **Merge `documentation` → `docs`** (re-label the 2 `documentation` issues, delete `documentation`).
2. **Merge `qnx` → `qnx-lane`** (re-label the 1 `qnx` issue, delete `qnx`).
3. **Set color + description** on `supply-chain`, `severity:low`, `severity:critical` (created gray/blank during the backfill effort).
4. **Backfill `severity:*`** on the open backlog so triage-by-severity is reliable (currently ~9% coverage).
5. **Enable Dependabot** for the `github-actions` ecosystem so SHA-pins (see review BD1 / #686) stay fresh.

---

## Quick decision guide

- *Is it a defect?* → `bug` (+ `severity:*`). *A new capability?* → `feat`/`enhancement`. *Cleanup?* → `tech-debt`.
- *Does it block other work?* → add `critical-path`. *Can't proceed?* → `blocked` / `hardware-blocked` / `upstream`.
- *Touches the build/deps/release chain?* → `supply-chain` (+ `security`/`hardening`/`infra` as apt).
- *Safety-relevant?* → `safety` (+ `safety-case`/`cert-evidence` if it produces certification evidence).
- *Always add the most specific* `domain:*` / `ws:*` / component label so the project board views stay accurate.
