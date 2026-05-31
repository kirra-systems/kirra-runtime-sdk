# Occy / KIRRA — Governor Integrity Evidence Plan (S3)

**Issue:** S3 (#115) — Governor ASIL-D integrity evidence.
**Doc ID (proposed):** KIRRA-OCCY-INTEG-001.
**Status:** This is the evidence *plan* — the integrity requirements and how each
is satisfied. Producing the evidence (running WCET analysis, achieving MC/DC,
building with a qualified toolchain, writing the safety manual) is the
implementation work this plan scopes. Targets/tooling are proposals for review.

---

## 1. The claim this substantiates

The decomposition (ADR-0003 / OCCY_DFA.md) puts ASIL-D integrity in the Governor
as `D(D)`, with the planner as disciplined-`QM(D)`. That only holds if the
Governor *itself* is built and evidenced to ASIL-D rigor. This plan is that
evidence, pointed at the actual safety-critical modules:

- `src/gateway/kinematics_contract.rs::validate_vehicle_command` — the primary
  per-step check (Allow / Reject / Clamp; priority guards P0 NaN/Inf, P2 velocity
  ceiling, P6 lateral-accel).
- `src/gateway/cmd_vel.rs::validate_cmd_vel` — differential-drive path.
- `parko-core/src/rss.rs` — RSS longitudinal check (SG1).
- `src/posture_cache.rs` — posture gating + staleness fail-closed (SG5/8/9).
- the clamp/egress path (policy_layer) where verdicts actually rewrite egress.

---

## 2. Evidence elements

| Element | Target | Method / tooling | Code tie | State → action |
|---|---|---|---|---|
| **Bounded WCET** | Proven worst-case execution bound for the verdict path; ≤ the SG9 timeout; verdict+actuation < control cycle < 0.5 s reaction budget | Static WCET where feasible, else measurement-based + margin; requires panic-free/abort-to-safe, no heap alloc on the hot path, bounded loops (horizon, agent cap), input-size caps | validate_vehicle_command (per-step × bounded horizon), rss.rs (× capped N agents), posture read | Body-bound caps + NaN traps already enforce input bounds; ACTION: verify no-alloc + panic-freedom on the check path, then bound it |
| **MC/DC coverage** | 100% MC/DC on the safety-critical decision logic | LLVM MC/DC instrumentation (`-C instrument-coverage`, MC/DC mode) via cargo-llvm-cov; branch coverage as interim if MC/DC tooling not ready | the P0/P2/P6 guards + Allow/Reject/Clamp decisions; RSS violation decision; posture/staleness branches | ACTION: measure current MC/DC, extend tests to cover all condition/decision combinations |
| **Requirements traceability** | Bidirectional SG → safety requirement → code → test | Structured `Safety: SGx` tag convention + an extraction script producing the matrix | the 5 existing tags (P0→SG9, P2/P6→SG3, posture Unknown→SG9, staleness→SG8/9) are the seed | ACTION: complete the matrix for all SG1–SG9 and every check site; wire extraction into CI |
| **Freedom-from-interference** | Spatial + temporal + communication isolation from the planner | Physical separation (D3: separate compute / SoC); input copy+validate; verdict in-line on egress | posture read fail-closed; body-bound + NaN traps (inputs can't corrupt the check); D3/ADR-0003 | Largely satisfied by D3 separate compute; ACTION: document the isolation as FFI evidence |
| **Qualified toolchain** | ASIL-D-qualified Rust compiler for the Governor crate | **Ferrocene** (ISO 26262 ASIL-D / IEC 61508-qualified rustc); stock rustc fine for the QM planner | the Governor crate(s) only | ACTION: confirm target support (x86_64/aarch64 Linux/QNX); adopt Ferrocene for the Governor build |
| **Governor safety manual** | The SEooC integrity claims + assumptions of use + config constraints | Document (outline §4) | consolidates ARCH-001 input contract + the SG claims + this plan's evidence | ACTION: draft the manual once WCET/coverage land |

---

## 3. WCET budget (the loop closure)

The fail-closed timeout (SG9) **is** the WCET bound — and it has to fit inside
the reaction budget the speed cap is built on:

    verdict WCET  +  actuation latency  <  control-cycle period  <  0.5 s chain reaction budget

Allocation to prove:
- per-step kinematics check × bounded trajectory horizon (validate_vehicle_command),
- RSS check × capped agent count N (rss.rs),
- posture read (posture_cache),
- clamp/egress rewrite (policy_layer).

WCET-enabling code properties (verify, then bound): panic-free or
panic=abort→safe-state on the check path; **no heap allocation** on the hot path
(stack-only / bounded); **bounded loops** (horizon length, agent cap); input-size
caps (the 16 KiB body bound already in place). Once bounded, the proven WCET sets
the SG9 timeout and confirms the per-cycle FTTI for SG1/2/3/7/9 — and any change
that breaks the bound is a safety regression caught in CI.

---

## 4. Governor safety manual — outline

As an SEooC, the manual states the conditions under which the ASIL-D claim holds:
1. **Integrity claims** — which safety goals the Governor enforces and how
   (SG1–SG9 → check sites).
2. **Assumptions of use** — the Perception Input Contract (ARCH-001 §4): what the
   integrator's perception must deliver; runtime-verified items vs. documented
   assumptions; fail-safe on violation.
3. **Configuration constraints** — the speed-cap = f(validated range) rule
   (ADR-0001), the sub-ODD/condition-dependent cap (ADR-0002), the two-tier
   coverage model (ADR-0003).
4. **FFI requirements** — separate compute, in-line egress, input validation.
5. **Toolchain** — Ferrocene qualification scope.
6. **Coverage & WCET** — the achieved MC/DC and the proven WCET bound.

---

## 5. Actions (S3 checklist)

- [x] Verify no-alloc + panic-freedom on the Governor check path; bound the WCET — **done** on branch `s3-wcet-pass-b`. Pass A removed per-verdict heap allocs + set `panic = "abort"`. Pass B1+B2 made the verdict path lock-free in production. The structural boundedness argument lives in `src/wcet_gate.rs` (O(1) per call; no loops, no recursion, no alloc, no locks). CI-measured steady-state p99.9 = 170–352 ns; max with OS jitter ≤ 219 µs (target hardware re-measure under S8/#120).
- [x] Set the SG9 timeout to the proven WCET; wire a CI guard against regressions — **done**. `GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100` (deployment target). CI guard at `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS = 1000` (generous for shared-runner variance). Six tests in `wcet_gate::ci_gate_tests` cover Allow / P0-NaN-Deny / P2-Clamp / P6-Clamp / posture-route Nominal / posture-route Stale-FailClosed. Target re-validated on D3 independent compute under S8 (#120).
- [ ] Measure MC/DC on the safety-critical functions; extend tests to 100%
- [x] Complete the SG→requirement→code→test traceability matrix; extract in CI — **done** (S3 traceability build, commit `3026535`). `docs/safety/TRACEABILITY.md` defines the parseable `// SAFETY: SGx | REQ: ... | TEST: ...` convention; `docs/safety/TRACEABILITY_MATRIX.md` is auto-generated via `scripts/extract_safety_traceability.sh`; `src/traceability_gate.rs::ci_gate_tests` is the Rust CI gate (every ENFORCED SG has ≥ 1 tagged site; every tagged site has non-empty REQ + TEST; SG ids in range; tag-count floor).
- [x] Document FFI evidence (D3 separation + input validation) — **done**. See `docs/safety/OCCY_FFI_EVIDENCE.md` (KIRRA-OCCY-FFI-001) — spatial / temporal / communication isolation evidence consolidation; D3 independent-compute deployment is the assumption of use.
- [ ] Adopt Ferrocene for the Governor crate; confirm target support
- [ ] Draft the Governor Safety Manual (§4)

Cross-refs: OCCY_DFA.md / #114, OCCY_SAFETY_GOALS.md (SG1–SG9), SPEED_ENVELOPE.md
(reaction budget), OCCY_ARCHITECTURE_TIERS.md (input contract / manual), S8 / #120.
Register as KIRRA-OCCY-INTEG-001.
