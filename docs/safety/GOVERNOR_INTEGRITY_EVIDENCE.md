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
| **MC/DC coverage** | 100% MC/DC on the safety-critical decision logic | LLVM MC/DC instrumentation (`-C instrument-coverage`, MC/DC mode) via cargo-llvm-cov; branch coverage as interim if MC/DC tooling not ready | the P0/P2/P6 guards + Allow/Reject/Clamp decisions; RSS violation decision; posture/staleness branches | **DONE** — see `docs/safety/OCCY_MCDC_EVIDENCE.md`. Pair-completing tests added across the SG-critical decision functions; branch coverage on the targeted functions ≥ 96.15% (cmd_vel / kinematics_contract / classify_http_command / should_route_command / containment / RSS — all 100% or close, residuals are tracing macros). MC/DC `--mcdc` flag fallback to `--branch` documented (rustc/cargo-llvm-cov flag-name mismatch on stable nightly). |
| **Requirements traceability** | Bidirectional SG → safety requirement → code → test | Structured `Safety: SGx` tag convention + an extraction script producing the matrix | the 5 existing tags (P0→SG9, P2/P6→SG3, posture Unknown→SG9, staleness→SG8/9) are the seed | ACTION: complete the matrix for all SG1–SG9 and every check site; wire extraction into CI |
| **Freedom-from-interference** | Spatial + temporal + communication isolation from the planner | Physical separation (D3: separate compute / SoC); input copy+validate; verdict in-line on egress | posture read fail-closed; body-bound + NaN traps (inputs can't corrupt the check); D3/ADR-0003 | Largely satisfied by D3 separate compute; ACTION: document the isolation as FFI evidence |
| **Qualified toolchain** | ASIL-D-qualified Rust compiler for the Governor crate | **Ferrocene** (ISO 26262 ASIL-D / IEC 61508-qualified rustc); stock rustc fine for the QM planner | the Governor crate(s) only | **DONE (plan + build-compat pre-check)** — see §6. x86_64-unknown-linux-gnu qualified since Ferrocene 24.05; no post-1.86 features in source; CriticalUp workflow drafted; productization (license + CI switch) tracked separately. |
| **Governor safety manual** | The SEooC integrity claims + assumptions of use + config constraints | Document (outline §4) | consolidates ARCH-001 input contract + the SG claims + this plan's evidence | ACTION: draft the manual once WCET/coverage land |
| **Machine-checked proofs (EP-15)** | Formal proofs (not sampled tests) of checker-core invariants on the actuation path | **Kani** (CBMC model checking) over `verification/kani/` — the shipped sources `#[path]`-included VERBATIM (the frozen talisman blob `851f3f44…` is under proof unmodified); `kani-proofs` CI lane (BLOCKING as of the L5 flip — a proof failure fails CI; only a Kani install-fetch flake is tolerated, skipping the proofs while the concrete mirror tests of every property still run BLOCKING). **ASSURANCE CHANGE (#1243): SG1 no longer has a per-PR symbolic proof.** #1243 corrects the kernel behaviour but increases the symbolic complexity of SG1 beyond the current per-PR Kani budget. The unconditional SG1 proof (K3) remains in the deep verification lane, while a concrete mirror becomes the standing per-PR gate. This is an explicit reduction in per-PR symbolic coverage and required approval as an assurance re-pin. Measured: the pre-#1243 form discharged in 22 s but is FALSE after the fix; the conditional and disjunctive true forms each produced no verdict at 15 minutes. The property was deliberately NOT weakened to regain runtime — a cheaper `|v| <= ceiling` was available, rejected because it duplicates K6 and would let this document read as though nothing had changed. FOUR harnesses are ASSIGNED to the WEEKLY `kani-deep-weekly` lane (`deep-proofs` feature) rather than per-PR, each with an exhaustive concrete mirror as its BLOCKING per-PR gate. **LANE HISTORY AND CURRENT PER-HARNESS STATE (#1260).** From 12 to 31 July 2026 this lane delivered NO coverage at all: its three scheduled runs were each killed at the GitHub-hosted SIX-HOUR per-job ceiling and reported `cancelled` (grey, not red, which is why it went unnoticed); the declared `timeout-minutes: 480` cannot exceed that ceiling and had no effect; and because all harnesses ran serially in one job, a single non-terminating harness starved everything behind it. #1262 repaired both defects (per-harness matrix; a timeout under the ceiling that fails RED). **The tier is now per-harness and must be read that way — do not treat "deep lane" as one uniform status:**

- **K3 (SG1) — ✅ PROVED, symbolic tier RESTORED.** First completing run (30667373086) discharged it in **67 m 13 s**: `0 of 310 failed`, `VERIFICATION:- SUCCESSFUL`. Weekly symbolic proof + blocking per-PR concrete mirror; SG1 holds both tiers. A per-PR symbolic proof is still absent (67 min > the 45-min budget), so #1243's recorded reduction is real but much smaller than the suspension it was first written as.
- **K7 (P-RACK) — ❌ DEMOTED out of the proof tier (#1268)**, diagnosed, not merely timed out. See its entry below.
- **K8 — ❌ DEMOTED out of the proof tier (#1260)**, profiled, not merely timed out. Full harness: symex 0.34 s, 404 → 36 VCCs, conversion 1.43 s, three solves totalling 101 s, then a fourth propositional conversion that never reaches a solver (43 of 45 min). Isolated to its one real assertion — 373 of 374 properties dropped — the CNF shrinks 0.54% (848,226 → 843,634), conversion completes in 1.0 s, the solver IS reached, and returns nothing in 40 min. Two failures at once, exactly K7's signature; fixing the conversion stall only exposes the hard instance. Per-PR enforcement unchanged: the 2,880-point physical-dt grid, asserting the ACCELERATION-space form the proof abandoned — so no coverage was lost.
- **R2 — ⏸ RESTRICTED, pending profiling.** Same: 300-minute isolated timeout.

**On R2, the wording is deliberate.** Its 300-minute isolated timeout excludes the old starvation explanation, but it is NOT a finding of intractability and this document must not be edited to say so. R2 is the one harness that never stalls in conversion: it reaches the solver and stays there. That is a **phase** difference, not a size one — and the sizes are worth stating carefully because they point opposite ways. R2's pre-solver metrics are far smaller (2,227 program-expression steps and 74 VCCs, against ~8,900 and ~400), but its **CNF is LARGER**: 1,221,711 clauses over 251,960 variables, against K7's 738,811 and K8's 848,226. "R2 is the small one" is therefore false about the thing the solver actually consumes and must not be offered as a reason to keep it.

**What does justify keeping it is measured solver progress.** CBMC's `--verbosity` does not reach an external solver, so this needed kissat's own telemetry captured through a wrapper. With it: R2's UNSAT solve sustains ~70 conflicts/s with no systematic collapse (oscillating 17–122/s), while inprocessing shrinks the instance monotonically — irredundant clauses 135k → 73k and "remaining" 54% → 29% over the first 376 s. Backbone probing, vivification and substitution are all active. That is a live search, not a plateau, so R2 is a budget/solver question (budget, solver choice, portfolio) rather than a modelling one. It does NOT establish that any particular budget suffices: at ~70 conflicts/s this is a slow, hard instance, and progress is not a termination guarantee. For R2 the concrete mirror remains the only tier, and the standing rule that every deep harness keeps a blocking per-PR mirror is the sole reason that is a coverage REDUCTION rather than a HOLE.

**The rule that governs all of these:** K3 was demoted on a 15-minute non-result and then discharged in 67 minutes. **A timeout is a lower bound on cost, never evidence of intractability.** K7 and K8 were demoted only after a phase diagnosis showed *where* the time goes and that the property set is not the cost; R2 has no such diagnosis and is therefore not demoted. **R2**: with the RSS squares respelled as exact IEEE multiplications its relational two-evaluation instance exceeds the per-PR 45-min budget on both CaDiCaL and kissat, so its per-PR gate is the full 0–60 m/s grid walk swept along all four parameter axes; cross-axis interaction is the weekly proof's remit. **K7** (#1242): 🔴 **NO LONGER IN THE PROOF TIER — demoted by #1268**, and no longer provisional. The earlier reading (no budget shown to suffice, so give it a bigger one) was tested and REFUTED. Isolating K7's two real assertions with CBMC `--property`, discarding 368 of its 370 properties, shrank the CNF by 0.5% — 738,811 → 735,393 clauses — so the property set is not the cost, the program encoding is; and in that isolated configuration CBMC reaches the solver and still returns nothing at 30 minutes. K7 therefore fails two ways at once: multi-property mode stalls in propositional conversion before a solver is invoked, and the underlying instance is solver-hard regardless, so fixing the first only exposes the second. ~735k clauses for an 8.8k-step program expression is IEEE-754 bit-blasting of two separately encoded f64 chains — additional runtime is not a credible remediation, and it must not be re-enabled on that theory. Confirmed by the first completing lane run (30667373086): K7 timed out at its full isolated 300-minute budget. Its source is retained behind the `outside-proof-envelope` feature, which NO lane enables, purely as a starting point for a future MODELLING redesign. **No coverage was lost**: P-RACK's enforcement is and remains the 306,180-point exhaustive grid over the executable return, blocking on every PR. **K8** (#1243): 🔴 **NO LONGER IN THE PROOF TIER — demoted by #1260**, on the same two-part diagnosis as K7 rather than on its earlier 25/55-minute timeouts. Full harness: three solves totalling 101 s, then a fourth propositional conversion that never reaches a solver (43 of 45 min). Isolated to its one real assertion, the CNF moves 0.54% (848,226 → 843,634) — the property set is not the cost — conversion completes in 1.0 s, the solver is reached, and returns nothing at 40 min. Its per-PR gate is unchanged: the 2,880-point physical-dt grid, asserting the ACCELERATION-space form the symbolic proof had to abandon (it was restated in velocity space after a denormal counterexample showed the quotient form asserts an error amplification the kernel never performs) — so the grid is not a weaker copy of the proof and **no coverage was lost**. A pattern emerged worth recording: assertions comparing a returned value against a CONTRACT FIELD discharge in tens of seconds (K3 22 s, K6 45 s), whereas RELATIONAL ARITHMETIC over two command fields does not finish (K7, K8) | **L1–L4** `src/lease.rs`: `from_ttl` totality + the `demote_before_promote` split-brain invariant for ALL u64 TTLs; promotion only strictly after holder lease expiry (window non-overlap + positive guard margin); clock-skew fails safe; on-cadence renewal never expires. **K1–K8** `kirra-core kinematics_contract` (talisman): SG9 NaN/Inf fail-closed totality over every f64 bit pattern in every field; SG3 non-positive dt denied; SG1 P2 speed ceiling — RESTATED by #1243 to "never exceeded, and exactly the ceiling with the request's sign when the ceiling is the only binding constraint", the former unconditional "exactly the ceiling, direction preserved" having become false once the rate bound ran on that path (both statements recorded at the harness); issue-#70 Degraded re-initiation + speed-increase denials for all finite inputs in their regions; #1242 K3b composed enforcement reports BOTH corrected axes, K6 (P-CAP) every executable return respects the effective speed ceiling, K7 (P-RACK) every executable return respects the absolute rack limit — the last two being the observable shadow of "no priority may finalize an executable command", which is what the #1242 defect violated; **#1243 K8** the accel/brake rate bound holds on EVERY executable return, over the domain `|current| <= ceiling` — outside it the ceiling itself forces a breach (a lawful -1.0 m/s^2 request from 40.0 m/s clamped to a 35.0 ceiling implies -50 m/s^2) and invariant 8 plus K6 jointly decide that the ceiling wins, so the domain restriction records a conflict between two enforced bounds rather than excluding an inconvenient case; the excluded region is pinned by an EXPECTED-BUT-UNDESIRED fixture. **R1–R3** `parko-core rss.rs`: `longitudinal_safe_distance` fail-closed totality (finite ∧ ≥ 0) over the FULL f64 domain; closing-speed monotonicity on the integer-scaled operational grid — the precondition `occlusion_limited_speed`'s bisection relies on; invalid brake → exactly `RSS_FAILSAFE_DISTANCE_M` | **DONE (16 properties)** — #1242 replaced the blanket P6 exclusion with a MODEL and a narrower exclusion. `tan`/`atan`/`powi` are stubbed IN THE PROOF CRATE by nondeterministic values constrained to postconditions that are theorems about the real functions (finiteness, the atan principal-branch range, and the inverse-monotone relation coupling the pair), so a discharged proof holds for every implementation meeting them — stronger than a proof about one libm. The talisman is NOT modified for the prover. What remains excluded is the P6 numeric lateral-envelope VALUE: under the model the proofs never evaluate a real `tan`, so they cannot see whether that arithmetic is right, and it stays discharged by the concrete grid + MC/DC + property tests above. The grid and the proofs are blind in OPPOSITE directions (a grid misses unsampled branches; the proofs cannot see the arithmetic) and neither may be presented as covering the other. Lesson recorded for future talisman work: widening which paths are reachable can break an existing, unrelated proof — K3 had passed for the life of the proof set and failed on a construct no assertion of its mentions. DONE: lane flipped to blocking (L5). ACTION: extend toward the seqlock/contract-channel protocol (per the maturation roadmap) |
| **Safety-case-as-code bundle (EP-18)** | Every release ships ONE versioned, hash-chained, self-verifying evidence bundle | `ci/build_safety_case.py` via `make safety-case` (release workflow, every tag): reviewed evidence manifests (EP-09 constants provenance, SOTIF coverage, SPI registry, KPI thresholds/MC config, quality ratchet) + the safety-case documents (this plan, UL 4600 case, RTM matrices, MC/DC + SOTIF evidence, RSS formal spec, HARA, AoU) + gates RE-EXECUTED at bundle time (constants match, ratchet, frozen-talisman blob pin) + referenced CI lanes (coverage/loom/fuzz/Miri/Kani/Postgres/KPI, with run URL when built in CI) | elements chained `h_i = SHA256(h_{i-1} ‖ sha256_i ‖ id_i)` → `bundle_digest` (content-addressed, wall-clock-free — same tree ⇒ same digest); `--verify` re-hashes + re-walks the chain; the tarball enters SHA256SUMS + keyless cosign with the platform artifacts | **DONE (initial bundle, 27 elements)** — ACTION: grow toward every §2 claim linking to a CI-verifiable element (the maturation roadmap's "machine-checkable safety case") |

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
caps (the 16 KiB body bound already in place). Once a target-measured WCET
exists, it sets the SG9 timeout and confirms the per-cycle FTTI for
SG1/2/3/7/9 — and any change that breaks the bound is a safety regression
caught in CI. Today the verdict time is bounded by construction (the structural
boundedness argument in `src/wcet_gate.rs`) with the host-indicative
CI-measured p99.9 as corroboration — **not a certified WCET**; the
QNX/`SCHED_FIFO` target measurement is tracked in #274.

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
6. **Coverage & WCET** — the measured decision coverage (100% branch-pair on
   the targeted check-path decisions; true MC/DC toolchain-blocked, #65) and
   the WCET evidence (structural boundedness + host-indicative p99.9 — not a
   certified WCET; target measurement tracked in #274).

---

## 5. Actions (S3 checklist)

- [x] Verify no-alloc + panic-freedom on the Governor check path; bound the WCET — **done** on branch `s3-wcet-pass-b`. Pass A removed per-verdict heap allocs + set `panic = "abort"`. Pass B1+B2 made the verdict path lock-free in production. The structural boundedness argument lives in `src/wcet_gate.rs` (O(1) per call; no loops, no recursion, no alloc, no locks). CI-measured steady-state p99.9 = 170–352 ns; max with OS jitter ≤ 219 µs (target hardware re-measure under S8/#120).
- [x] Set the SG9 timeout from the measured timing evidence (host-indicative — a certified WCET awaits the target measurement, #274); wire a CI guard against regressions — **done**. `GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100` (deployment target). CI guard at `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS = 1000` (generous for shared-runner variance). Six tests in `wcet_gate::ci_gate_tests` cover Allow / P0-NaN-Deny / P2-Clamp / P6-Clamp / posture-route Nominal / posture-route Stale-FailClosed. Target re-validated on D3 independent compute under S8 (#120).
- [x] Measure MC/DC on the safety-critical functions; extend tests to 100% — **done** on branch `s3-mcdc-ferrocene`. See `docs/safety/OCCY_MCDC_EVIDENCE.md` (KIRRA-OCCY-MCDC-001). Measurement under nightly llvm-cov fell back to `--branch` pair coverage (cargo-llvm-cov 0.8.7 `--mcdc` passes `-Z coverage-options=mcdc` to rustc, but `1.98.0-nightly` (`f8a08b688`, 2026-05-30) only accepts `block|branch|condition` — the value was renamed upstream and the driver has not yet been respun; the regression is documented in OCCY_MCDC_EVIDENCE.md §6.3). On the targeted Governor check-path decisions the pair table went from **49/56 → 56/56** branch pairs covered, with 17 added pair-completing tests in `src/gateway/cmd_vel.rs`, `src/gateway/containment.rs`, `src/gateway/policy.rs`, and `parko/crates/parko-core/src/rss.rs`. File-level branch coverage on those files: cmd_vel 100%, kinematics_contract 100%, policy 100%, posture_cache 100%, parko-core rss 100%. Residual unflipped file-level branches in containment / posture_engine_v2 are `tracing::warn!` macro expansions and helper-fn ray-cast clauses — not safety-critical condition flips. Every added test passes identically under stable rustc (`cargo test --workspace`, 399 + new in kirra; 72 + new in parko-core). The MC/DC INSTRUMENTATION is a measurement tool; production code ships unchanged on stable / Ferrocene.
- [x] Complete the SG→requirement→code→test traceability matrix; extract in CI — **done** (S3 traceability build, commit `3026535`). `docs/safety/TRACEABILITY.md` defines the parseable `// SAFETY: SGx | REQ: ... | TEST: ...` convention; `docs/safety/TRACEABILITY_MATRIX.md` is auto-generated via `scripts/extract_safety_traceability.sh`; `src/traceability_gate.rs::ci_gate_tests` is the Rust CI gate (every ENFORCED SG has ≥ 1 tagged site; every tagged site has non-empty REQ + TEST; SG ids in range; tag-count floor).
- [x] Document FFI evidence (D3 separation + input validation) — **done**. See `docs/safety/OCCY_FFI_EVIDENCE.md` (KIRRA-OCCY-FFI-001) — spatial / temporal / communication isolation evidence consolidation; D3 independent-compute deployment is the assumption of use.
- [x] Adopt Ferrocene for the Governor crate; confirm target support — **plan + build-compat pre-check landed; see §6 below.** S3 evidence element is the credible documented adoption plan + the pre-check that the workspace builds under Ferrocene 25.05 (rustc 1.86). Actual production switchover (CriticalUp pinning + CI pipeline change) is a tracked productization step, not a blocker on the S3 evidence.
- [x] Draft the Governor Safety Manual (§4) — **done**. See `docs/safety/GOVERNOR_SAFETY_MANUAL.md` (KIRRA-OCCY-GOVMAN-001 / SEooC deliverable).

Cross-refs: OCCY_DFA.md / #114, OCCY_SAFETY_GOALS.md (SG1–SG9), SPEED_ENVELOPE.md
(reaction budget), OCCY_ARCHITECTURE_TIERS.md (input contract / manual), S8 / #120.
Register as KIRRA-OCCY-INTEG-001.

---

## 6. Qualified toolchain — Ferrocene adoption plan

**Doc ID extension:** KIRRA-OCCY-FERROCENE-001 (this section).
**Selected qualified compiler:** Ferrocene (Ferrous Systems / Rust qualification),
ISO 26262 TCL 3 / ASIL D, IEC 61508 T3 / SIL 3, IEC 62304 Class C — qualified by
TÜV SÜD.

### 6.1 Target confirmation

| Aspect | Value | Source |
|---|---|---|
| Governor dev / CI / host target | `x86_64-unknown-linux-gnu` (glibc 2.31+) | observed `rustc --version --verbose` |
| Ferrocene qualification status | **Qualified** since Ferrocene 24.05.0; maintained in 25.05.0 (the current release at evidence time) | Ferrocene 25.05 release notes |
| Embedded deployment targets (S8 / #120 path) | Armv8-A bare metal (`aarch64-unknown-none`), Armv7E-M bare-metal (`thumbv7em-none-eabi[hf]` — new in 25.05.0), and QNX targets are also qualified | Ferrocene targets index |
| Anything outside the qualified set | "get in touch" path with Ferrous Systems for incremental qualification | Ferrocene qualification plan |

The dev/CI/host target on which the Governor verdict path is built and run is
covered by an existing qualified Ferrocene target. The expected production
deployment target (D3 independent compute — Armv8-A or x86-64 depending on
integrator hardware) is also covered.

### 6.2 Build-compat pre-check (no Ferrocene install needed)

Performed against the merged consolidated working tree using stable `rustc 1.94.1`.
Ferrocene 25.05 ships **upstream Rust 1.86.0**; the gap to certify across is
1.86 → 1.94.

| Concern | Finding | Disposition |
|---|---|---|
| Edition | `edition = "2021"` in every workspace `Cargo.toml` | OK — Ferrocene supports 2021 + 2024 |
| Declared MSRV (`rust-version`) | NONE declared in `kirra-runtime-sdk`, `parko-core`, `parko-kirra` | Pin an MSRV at the Ferrocene-targeted rustc version before switchover (productization step). |
| `let-chains` (`if let ... = ... && ...`, stable 1.88) | `grep -rE "if let .* = .* &&"` → 0 hits | OK |
| `Vec::extract_if` (stable 1.87) | not used | OK |
| `LazyLock` (stable 1.80) | used in `src/ffi.rs` | OK — included in Ferrocene 25.05 / Rust 1.86 |
| Other post-1.86 stdlib APIs (`to_canonical`, `offset_from_unsigned`, `hint::cold_path`, `advance_by`, `MaybeDangling`) | not used | OK |

**Conclusion** — no post-1.86 language/std features detected in the workspace
source. The workspace **should build** under Ferrocene 25.05.0. Caveat: a
small number of transitive deps may declare `rust-version > 1.86` and force
pinning or replacement; that's a cargo resolution exercise tracked separately,
not a Kirra-side blocker.

### 6.3 Licensing route

Ferrocene is fully open source under `Apache-2.0 OR MIT`, **including the
full qualification documents**. Two practical acquisition routes:

1. **Build from source** — the Apache/MIT license permits in-house builds;
   the qualification documents we cite (ISO 26262, IEC 61508 etc.) are
   themselves Apache/MIT.
2. **Prebuilt binaries** — `releases.ferrocene.dev` (customer/partner login
   via a Ferrocene account); installed/managed by **CriticalUp**, Ferrocene's
   installer + toolchain manager. Fully offline-capable; no license-server
   admin required.

### 6.4 CriticalUp adoption sketch (productization)

A `criticalup.toml` at the repo root pins a Ferrocene release; CI / dev
shells use `criticalup install` then `criticalup run cargo`. Reference
shape (productization will fill in the exact release pin):

```toml
# criticalup.toml — Ferrocene release pin for the Governor build
manifest-version = 1

[products.ferrocene]
release = "stable-25.05.0"

[products.ferrocene.packages]
"rustc-x86_64-unknown-linux-gnu" = []
"cargo-x86_64-unknown-linux-gnu" = []
"rust-std-x86_64-unknown-linux-gnu" = []
"rustfmt-x86_64-unknown-linux-gnu" = []
"clippy-x86_64-unknown-linux-gnu" = []
```

CI integration (proposed):

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ferrocene/criticalup/releases/latest/download/criticalup-installer.sh | sh
criticalup install
criticalup run cargo test --workspace
```

Authentication for prebuilt binaries: `CRITICALUP_TOKEN` env var holding
the Ferrocene account credentials. The build-from-source route does not
need this.

### 6.5 S3 evidence-element status

| Evidence sub-element | Status |
|---|---|
| Qualified target identified | ✅ `x86_64-unknown-linux-gnu`, qualified since Ferrocene 24.05 |
| Build-compat pre-check | ✅ no post-1.86 features in workspace source |
| Licensing route documented | ✅ open-source-build OR prebuilt-binaries |
| CriticalUp workflow drafted | ✅ §6.4 above |
| Productization (`criticalup.toml` commit + CI pipeline switch + license procurement) | ⏳ tracked as PRODUCTIZATION (separate from S3 evidence) |

The S3 / #115 box for "Adopt Ferrocene for the Governor crate" is addressed
as **evidence element via plan**. Actual switchover is a productization step
filed separately.
