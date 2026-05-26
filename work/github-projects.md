# GitHub Projects — Kirra Safety Runtime

> This file defines the complete Kanban board layout for the Kirra + Parko
> monorepo. Apply it manually at:
> github.com/justinlooney/kirra-runtime-sdk → Projects → New project → Board

---

## Board: Kirra Safety Runtime

**Description:**
Solo lean-agile board for a hardware-neutral, safety-critical autonomy runtime
targeting ASIL-D. Tracks work across parko-core, parko-onnx, parko-aegis, and
kirra-runtime-sdk from deterministic tick grid through certification-ready
packaging.

---

## Columns

### 1 — Backlog
**Description:** All accepted tasks not yet scheduled. Pulled into Ready when
the preceding task in the same increment is Done or when the task has no
dependency.

**Automation rules:**
- Issues labeled `backlog` are automatically added here when opened
- Issues re-opened from Done return here
- Issues that have a `blocked-by` cross-reference and the blocker is not
  closed stay here until blocker closes

**WIP limit:** None

---

### 2 — Ready
**Description:** Tasks with all dependencies resolved, fully specified, and
ready to start immediately. A task enters Ready only when its acceptance
criteria and Claude Code Prompt are written (see backlog.md).

**Automation rules:**
- Move here manually when pulling from Backlog
- Auto-move back to Backlog if a new `blocked-by` label is added after the
  task was moved to Ready

**WIP limit:** 5

---

### 3 — In Progress
**Description:** Active work. Max 3 cards at once (enforced by active.md).
Each card in this column must correspond to a task in `work/active.md`.

**Automation rules:**
- Auto-move here when a branch matching `park-NNN/*` is pushed
- Auto-move here when the issue is assigned
- If idle for > 3 days with no commit activity, add label `stale` and notify

**WIP limit:** 3

---

### 4 — Blocked
**Description:** Work that has started but is waiting on an external dependency
(hardware, third-party SDK, review, decision).

**Automation rules:**
- Adding label `blocked` to any In Progress card auto-moves it here
- Removing `blocked` label moves card back to In Progress
- Weekly: review all Blocked cards; any blocked > 7 days gets a `needs-decision`
  label and an ADL entry should be created in `work/decisions.md`

**WIP limit:** None

---

### 5 — Done
**Description:** Merged, tested, and verified. Task entry appended to
`work/done.md` weekly.

**Automation rules:**
- Auto-move here when the linked PR is merged to `main`
- Auto-move here when the issue is closed
- Closed issues with label `wont-fix` are moved here with that label preserved

**WIP limit:** None (archive after 30 days)

---

## Label Taxonomy

Apply these labels to all issues. Each issue gets exactly one **type** label,
one or more **domain** labels, and optionally one **status** label.

### Type labels
| Label | Color | Meaning |
|-------|-------|---------|
| `feat` | `#0075ca` | New capability or API |
| `fix` | `#e4e669` | Bug fix or correctness patch |
| `test` | `#bfd4f2` | Test-only change |
| `docs` | `#cfd3d7` | Documentation |
| `chore` | `#fef2c0` | Build, CI, packaging, tooling |
| `safety` | `#d93f0b` | Safety-critical path change (requires extra review) |

### Domain labels
| Label | Meaning |
|-------|---------|
| `control-loop` | parko-core ControlLoop / InferenceLoop |
| `backend-qnn` | Qualcomm AI Engine Direct / QNN |
| `backend-tidl` | TI TIDL / DSP |
| `backend-openvino` | Intel OpenVINO |
| `backend-rocm` | AMD ROCm / MIGraphX |
| `behavioral-safety` | RSS safe-distance model |
| `aegis-integration` | kirra-runtime-sdk ↔ parko-aegis boundary |
| `posture-engine` | kirra-runtime-sdk posture state machine |
| `packaging` | Binary, installer, systemd, Helm |
| `simulation` | ScenarioRunner, HIL, adversarial tests |
| `certification` | FMEA, RTM, coverage, DFA, SOTIF |

### Status labels
| Label | Meaning |
|-------|---------|
| `blocked` | Waiting on external dependency |
| `stale` | No activity for 3+ days |
| `needs-decision` | Blocked on an architectural decision |
| `needs-hardware` | Requires physical dev board to proceed |
| `wont-fix` | Accepted as out of scope |

---

## Milestones → Roadmap Increments

Create these milestones in GitHub (Issues → Milestones → New milestone):

| Milestone | Title | Description |
|-----------|-------|-------------|
| `v0.1` | Deterministic Runtime Core | ControlLoop + governor + proptest suite. Artifact: parko-core v0.1.0 |
| `v0.2` | Hardware Abstraction Layer | BackendDescriptor, 4 stub backends, latency watchdog. Artifact: parko-core v0.2.0 |
| `v0.3` | Behavioral Safety | RSS model, posture integration, 10k simulation. Artifact: parko-core v0.3.0 + kirra-runtime-sdk RSS |
| `v0.4` | Silicon Matrix Expansion | Real QNN + OpenVINO backends, CI matrix. Artifact: feature-gated binaries |
| `v1.2` | Safety OS Packaging | Unified binary, systemd, installer, dashboard panels. Artifact: kirra v1.2.0 release |
| `v2.0` | Certification-Ready Runtime | RTM, MC/DC, FMEA, DFA, offline verifier. Artifact: pre-assessment package |

---

## Card Templates

### Feature Task Card
```
## Summary
<!-- 2–3 sentences: what this adds and why it matters -->

## Acceptance Criteria
- [ ] <!-- specific, testable criterion -->
- [ ] <!-- specific, testable criterion -->
- [ ] All existing tests continue to pass
- [ ] `cargo test -p <crate>` exits 0

## Claude Code Prompt
<!-- Paste the ready-to-use prompt from work/backlog.md -->

## Files Likely Touched
- `<crate>/src/<file>.rs`

## Milestone
<!-- e.g. v0.1 — Deterministic Runtime Core -->

## Labels
<!-- type:feat, domain:control-loop -->
```

---

### Backend Task Card
```
## Summary
<!-- What backend, what capability, what “done” looks like -->

## Target Hardware
- Platform: <!-- e.g. Qualcomm SA8295, TI TDA4VM -->
- SDK version: <!-- e.g. QNN SDK 2.x, TIDL 9.x -->
- CI runnable without hardware: <!-- Yes (stub) / No (real backend) -->

## Acceptance Criteria
- [ ] Feature gate: `features = ["backend-<name>"]`
- [ ] Stub passes CI on ubuntu-latest without hardware
- [ ] Real backend integration test marked `#[ignore]` on CI
- [ ] Output matches ORT CPU reference within tolerance 1e-3
- [ ] `BackendDescriptor::<Variant>` returned from `backend_descriptor()`

## Claude Code Prompt
<!-- Paste the ready-to-use prompt from work/backlog.md -->

## Milestone
<!-- e.g. v0.2 — Hardware Abstraction Layer -->

## Labels
<!-- type:feat, domain:backend-qnn, needs-hardware (if real backend) -->
```

---

### Safety Task Card
```
## Summary
<!-- What safety property is being added or verified -->

## Safety Claim
<!-- e.g. "No RSS-violating command reaches the actuator in any posture state" -->

## Verification Method
- [ ] Property test (proptest, ≥ 10 000 cases)
- [ ] Scenario test (ScenarioRunner, VirtualClock)
- [ ] Formal assertion (type-system invariant)

## Acceptance Criteria
- [ ] Property test passes with N ≥ <!-- 10_000 --> cases
- [ ] All three posture states covered: Nominal, Degraded, LockedOut
- [ ] Audit chain entry created for every violation event
- [ ] Posture transition documented in KIRRA-RTM-001

## Claude Code Prompt
<!-- Paste the ready-to-use prompt from work/backlog.md -->

## Milestone
<!-- e.g. v0.3 — Behavioral Safety -->

## Labels
<!-- type:safety, domain:behavioral-safety, domain:aegis-integration -->
```

---

### Documentation Task Card
```
## Summary
<!-- What document, what audience, what “done” looks like -->

## Document ID
<!-- e.g. KIRRA-RTM-001, KIRRA-FMEA-001, ADL-006 -->

## Outline
- <!-- Section 1 -->
- <!-- Section 2 -->
- <!-- Section 3 -->

## Acceptance Criteria
- [ ] Document is in `docs/` (or `docs/safety/` for cert docs)
- [ ] All claims traceable to source code or tests
- [ ] No open TODOs
- [ ] Reviewed against ISO 26262 Part <!-- X --> checklist

## Milestone
<!-- e.g. v2.0 — Certification-Ready Runtime -->

## Labels
<!-- type:docs, domain:certification -->
```

---

## Epic → Increment Mapping

| Epic (GitHub Label) | Milestone | Tasks |
|---------------------|-----------|-------|
| `epic:runtime-core` | v0.1 | PARK-001, 002, 003, 004, 005, 006 |
| `epic:hal` | v0.2 | PARK-007, 008, 009, 010, 011, 012, 013, 014 |
| `epic:behavioral-safety` | v0.3 | PARK-015, 016, 017, 018, 019, 020, 021 |
| `epic:silicon-matrix` | v0.4 | PARK-022, 023, 024, 025, 026 |
| `epic:packaging` | v1.2 | PARK-027, 028, 029, 030, 031 |
| `epic:certification` | v2.0 | PARK-032, 033, 034, 035, 036, 037, 038, 039, 040 |

---

## Backlog → Card Mapping

| Task ID | Title | Type | Domain | Milestone | Priority |
|---------|-------|------|--------|-----------|----------|
| PARK-001 | Attach governor to ControlLoop | feat | control-loop | v0.1 | P0 |
| PARK-002 | Add test-only state setter | feat | control-loop | v0.1 | P0 |
| PARK-003 | Posture divergence property test | test | control-loop | v0.1 | P0 |
| PARK-004 | NaN/Inf rejection at tick boundary | safety | control-loop | v0.1 | P0 |
| PARK-005 | VirtualClock integration | feat | control-loop | v0.1 | P1 |
| PARK-006 | parko-core v0.1.0 release tag | chore | control-loop | v0.1 | P1 |
| PARK-007 | BackendDescriptor enum | feat | control-loop | v0.2 | P0 |
| PARK-008 | QNN stub backend | feat | backend-qnn | v0.2 | P0 |
| PARK-009 | TIDL stub backend | feat | backend-tidl | v0.2 | P0 |
| PARK-010 | OpenVINO stub backend | feat | backend-openvino | v0.2 | P0 |
| PARK-011 | ROCm stub backend | feat | backend-rocm | v0.2 | P0 |
| PARK-012 | Backend latency watchdog | safety | control-loop | v0.2 | P0 |
| PARK-013 | CI matrix: all four stub backends | chore | backend-qnn | v0.2 | P1 |
| PARK-014 | Real OpenVinoBackend | feat | backend-openvino | v0.2 | P1 |
| PARK-015 | RssSafeDistance::longitudinal | safety | behavioral-safety | v0.3 | P0 |
| PARK-016 | RssSafeDistance::lateral | safety | behavioral-safety | v0.3 | P0 |
| PARK-017 | RssState and posture integration | safety | posture-engine | v0.3 | P0 |
| PARK-018 | Wire RSS into KirraKernelGovernor | safety | aegis-integration | v0.3 | P0 |
| PARK-019 | RSS property test | test | behavioral-safety | v0.3 | P0 |
| PARK-020 | RssViolationEvent in audit chain | safety | aegis-integration | v0.3 | P1 |
| PARK-021 | 10k adversarial trajectory simulation | test | simulation | v0.3 | P1 |
| PARK-022 | Real QnnBackend | feat | backend-qnn | v0.4 | P0 |
| PARK-023 | Real TidlBackend | feat | backend-tidl | v0.4 | P0 |
| PARK-024 | Real RocmBackend | feat | backend-rocm | v0.4 | P1 |
| PARK-025 | BackendSelector runtime selection | feat | control-loop | v0.4 | P0 |
| PARK-026 | Cross-backend determinism validation | test | simulation | v0.4 | P1 |
| PARK-027 | Unified kirra_safety_runtime binary | feat | packaging | v1.2 | P0 |
| PARK-028 | systemd unit with watchdog | chore | packaging | v1.2 | P0 |
| PARK-029 | Backend-aware installer | chore | packaging | v1.2 | P0 |
| PARK-030 | Dashboard inference panels | feat | packaging | v1.2 | P1 |
| PARK-031 | v1.2.0 release pipeline | chore | packaging | v1.2 | P1 |
| PARK-032 | Complete RTM (KIRRA-RTM-001) | docs | certification | v2.0 | P0 |
| PARK-033 | MC/DC coverage report | test | certification | v2.0 | P0 |
| PARK-034 | FMEA (KIRRA-FMEA-001) | docs | certification | v2.0 | P0 |
| PARK-035 | DFA (KIRRA-DFA-001) | docs | certification | v2.0 | P0 |
| PARK-036 | Offline kirra_audit_verify binary | feat | aegis-integration | v2.0 | P0 |
| PARK-037 | SOTIF analysis (KIRRA-SOTIF-001) | docs | certification | v2.0 | P1 |
| PARK-038 | HIL test harness | test | simulation | v2.0 | P1 |
| PARK-039 | Helm chart: inference backend values | chore | packaging | v2.0 | P1 |
| PARK-040 | Architecture overview document | docs | certification | v2.0 | P1 |

---

## Automation Script (GitHub CLI)

Run this once to create all labels, milestones, and issues:

```bash
#!/usr/bin/env bash
# setup-board.sh — idempotent GitHub board bootstrap
# Requires: gh CLI authenticated as justinlooney
# Usage: bash work/setup-board.sh

REPO="justinlooney/kirra-runtime-sdk"

# ── Labels ────────────────────────────────────────────────────────────────────────────
gh label create "control-loop"       --color "0075ca" --repo $REPO 2>/dev/null || true
gh label create "backend-qnn"        --color "e4e669" --repo $REPO 2>/dev/null || true
gh label create "backend-tidl"       --color "bfd4f2" --repo $REPO 2>/dev/null || true
gh label create "backend-openvino"   --color "cfd3d7" --repo $REPO 2>/dev/null || true
gh label create "backend-rocm"       --color "fef2c0" --repo $REPO 2>/dev/null || true
gh label create "behavioral-safety"  --color "d93f0b" --repo $REPO 2>/dev/null || true
gh label create "aegis-integration"  --color "b60205" --repo $REPO 2>/dev/null || true
gh label create "posture-engine"     --color "e99695" --repo $REPO 2>/dev/null || true
gh label create "packaging"          --color "c2e0c6" --repo $REPO 2>/dev/null || true
gh label create "simulation"         --color "fbca04" --repo $REPO 2>/dev/null || true
gh label create "certification"      --color "0e8a16" --repo $REPO 2>/dev/null || true
gh label create "safety"             --color "d93f0b" --repo $REPO 2>/dev/null || true
gh label create "blocked"            --color "b60205" --repo $REPO 2>/dev/null || true
gh label create "needs-hardware"     --color "c5def5" --repo $REPO 2>/dev/null || true
gh label create "epic:runtime-core"  --color "0075ca" --repo $REPO 2>/dev/null || true
gh label create "epic:hal"           --color "e4e669" --repo $REPO 2>/dev/null || true
gh label create "epic:behavioral-safety" --color "d93f0b" --repo $REPO 2>/dev/null || true
gh label create "epic:silicon-matrix"    --color "bfd4f2" --repo $REPO 2>/dev/null || true
gh label create "epic:packaging"     --color "c2e0c6" --repo $REPO 2>/dev/null || true
gh label create "epic:certification" --color "0e8a16" --repo $REPO 2>/dev/null || true

# ── Milestones ──────────────────────────────────────────────────────────────────────
gh api repos/$REPO/milestones -f title="v0.1 — Deterministic Runtime Core" \
  -f description="ControlLoop + governor + proptest suite. Artifact: parko-core v0.1.0" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="v0.2 — Hardware Abstraction Layer" \
  -f description="BackendDescriptor, 4 stub backends, latency watchdog" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="v0.3 — Behavioral Safety" \
  -f description="RSS model, posture integration, 10k simulation" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="v0.4 — Silicon Matrix Expansion" \
  -f description="Real QNN + OpenVINO backends, CI matrix" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="v1.2 — Safety OS Packaging" \
  -f description="Unified binary, systemd, installer, dashboard" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="v2.0 — Certification-Ready Runtime" \
  -f description="RTM, MC/DC, FMEA, DFA, offline verifier" 2>/dev/null || true

echo "✓ Labels and milestones created. Open issues manually or use gh issue create."
```
