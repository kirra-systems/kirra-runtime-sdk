# Issue Templates

> Copy the appropriate template when opening a GitHub issue.
> Every issue maps to one PARK-NNN task in work/backlog.md.
> Every coding issue must include a Claude Code Prompt section.

---

## Template 1 — Feature Task

```markdown
## PARK-NNN — <title>

**Epic:** <!-- epic:runtime-core / epic:hal / epic:behavioral-safety / epic:silicon-matrix / epic:packaging / epic:certification -->
**Milestone:** <!-- v0.1 / v0.2 / v0.3 / v0.4 / v1.2 / v2.0 -->
**Labels:** <!-- feat, control-loop -->

---

### Summary
<!-- 2–3 sentences: what this adds, why it matters, what crate it lives in. -->

### Acceptance Criteria
- [ ] <!-- Specific, testable. Reference function/file names. -->
- [ ] <!-- Specific, testable. -->
- [ ] All existing tests in the affected crate continue to pass
- [ ] `cargo test -p <crate>` exits 0
- [ ] No `unsafe` code unless explicitly justified here: <!-- or "N/A" -->

### Claude Code Prompt
<!-- Paste the full prompt from work/backlog.md. Must be self-contained. -->

```

### Files Likely Touched
- `<crate>/src/<file>.rs`

### Notes
<!-- Any context, gotchas, or cross-references to ADL entries. -->
```

---

## Template 2 — Backend Task

```markdown
## PARK-NNN — <title>

**Epic:** epic:hal <!-- or epic:silicon-matrix -->
**Milestone:** <!-- v0.2 / v0.4 -->
**Labels:** <!-- feat, backend-qnn -->

---

### Summary
<!-- What backend capability is being added. Stub vs. real backend. -->

### Target Hardware
| Field | Value |
|-------|-------|
| Platform | <!-- e.g. Qualcomm SA8295 / TI TDA4VM / Intel Xeon / AMD RX 6000 --> |
| SDK | <!-- e.g. QNN SDK 2.x / TIDL 9.x / OpenVINO 2024.x / ROCm 5.x --> |
| CI without hardware | <!-- Yes (stub only) / No (real backend, mark #[ignore]) --> |

### Acceptance Criteria
- [ ] Feature gate: `features = ["backend-<name>"]` in Cargo.toml
- [ ] Stub/no-hardware path passes CI on ubuntu-latest
- [ ] `BackendDescriptor::<Variant>` returned from `backend_descriptor()`
- [ ] `InferenceBackend::run` signature: `(&self, input: &[f32], output: &mut [f32]) -> Result<(), BackendError>`
- [ ] No heap allocation on the hot path (all scratch memory allocated at init)
- [ ] Real backend test marked `#[ignore]`: runs on hardware, asserts output matches ORT CPU reference within 1e-3

### Claude Code Prompt
<!-- Paste the full prompt from work/backlog.md. -->

### Notes
<!-- SDK version constraints, cross-compile target, linker flags. -->
```

---

## Template 3 — Safety Task

```markdown
## PARK-NNN — <title>

**Epic:** <!-- epic:behavioral-safety / epic:certification -->
**Milestone:** <!-- v0.3 / v2.0 -->
**Labels:** <!-- safety, behavioral-safety -->

---

### Summary
<!-- What safety property is being implemented or verified. -->

### Safety Claim
> <!-- One sentence. E.g.: "No RSS-violating command reaches the actuator
>      in any posture state under any valid input." -->

### Verification Methods
- [ ] Property test (proptest) — N ≥ <!-- 10_000 --> cases
- [ ] Scenario test (ScenarioRunner + VirtualClock)
- [ ] Type-system invariant / compiler enforcement
- [ ] RTM traceability entry added to KIRRA-RTM-001

### Acceptance Criteria
- [ ] Property test: N ≥ 10 000 cases, all three PostureState variants covered
- [ ] No `unsafe` bypass of the safety claim introduced anywhere in call stack
- [ ] Audit chain entry created for every violation event
- [ ] Posture transition documented: fault → Degraded, recovery → hysteresis
- [ ] `cargo test -p kirra-runtime-sdk` and `cargo test -p parko-core` both exit 0

### Claude Code Prompt
<!-- Paste the full prompt from work/backlog.md. -->

### Traceability
| Requirement | Source | Test |
|-------------|--------|------|
| <!-- AEGIS-SG-001 §X --> | <!-- src/kirra_core.rs:NN --> | <!-- test_rss_property --> |

### Notes
<!-- Reference IEEE 2846 sections, ISO 26262 clauses, or HARA entries. -->
```

---

## Template 4 — Documentation Task

```markdown
## PARK-NNN — <title>

**Epic:** epic:certification
**Milestone:** v2.0 — Certification-Ready Runtime
**Labels:** <!-- docs, certification -->

---

### Summary
<!-- What document, what audience, what "done" looks like. -->

### Document ID
<!-- e.g. KIRRA-RTM-001 / KIRRA-FMEA-001 / ADL-006 / docs/architecture.md -->

### Outline
1. <!-- Section 1 -->
2. <!-- Section 2 -->
3. <!-- Section 3 -->

### Acceptance Criteria
- [ ] Document in `docs/` (or `docs/safety/` for certification artifacts)
- [ ] All claims traceable to source code or test IDs
- [ ] No open TODOs
- [ ] Reviewed against ISO 26262 Part <!-- X --> checklist
- [ ] Table of contents present for documents > 3 sections

### Reference Standards
<!-- ISO 26262:2018 Part X, IEC 61508, IEEE 2846-2022, ISO 21448 -->

### Notes
<!-- Dependencies on other documents, expected reviewer, target auditor. -->
```

---

## Label Quick Reference

Apply exactly one type label + one or more domain labels + one status label.

**Type:** `feat` `fix` `test` `docs` `chore` `safety`

**Domain:** `control-loop` `backend-qnn` `backend-tidl` `backend-openvino`
`backend-rocm` `behavioral-safety` `kirra-integration` `posture-engine`
`packaging` `simulation` `certification`

**Status:** `backlog` → `ready` → `in-progress` → `done`
*(or `blocked` / `stale` / `needs-decision` / `needs-hardware` as overlays)*

---

## Branch Naming Convention

```
park-NNN/<short-description>

Examples:
  park-001/governor-builder
  park-003/posture-divergence-proptest
  park-015/rss-longitudinal-model
```

The `park-NNN` prefix triggers the board automation that moves the linked
issue to In Progress when the branch is pushed.

---

## PR Description Convention

```markdown
Closes #<issue-number>

## What
<!-- One paragraph summary of the change. -->

## Why
<!-- Reference PARK-NNN, ADL-NNN, or safety claim. -->

## Test Coverage
- [ ] Unit tests added / updated
- [ ] Property tests: N cases, all posture states covered
- [ ] `cargo test -p <crate>` passes

## Safety Impact
<!-- "None" / "Modifies safety-critical path: <describe>" -->
<!-- Safety-critical PRs require extra review before merge. -->
```
