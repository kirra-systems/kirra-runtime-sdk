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
| **Machine-checked proofs (EP-15)** | Formal proofs (not sampled tests) of checker-core invariants on the actuation path | **Kani** (CBMC model checking) over `verification/kani/` — the shipped sources `#[path]`-included VERBATIM (the frozen talisman blob `ed00f4da…` is under proof unmodified); `kani-proofs` CI lane (BLOCKING as of the L5 flip — a proof failure fails CI; only a Kani install-fetch flake is tolerated, skipping the proofs while the concrete mirror tests of every property still run BLOCKING). R2 alone runs in the WEEKLY `kani-deep-weekly` lane (`deep-proofs` feature, multi-hour solver budget): with the RSS squares respelled as exact IEEE multiplications its relational two-evaluation instance exceeds the per-PR 45-min budget on both CaDiCaL and kissat, so its per-PR gate is the exhaustive concrete mirror — the full 0–60 m/s grid walk swept along all four parameter axes; cross-axis interaction is the weekly proof's remit | **L1–L4** `src/lease.rs`: `from_ttl` totality + the `demote_before_promote` split-brain invariant for ALL u64 TTLs; promotion only strictly after holder lease expiry (window non-overlap + positive guard margin); clock-skew fails safe; on-cadence renewal never expires. **K1–K5** `kirra-core kinematics_contract` (talisman): SG9 NaN/Inf fail-closed totality over every f64 bit pattern in every field; SG3 non-positive dt denied; SG1 P2 speed-ceiling clamp exact (magnitude = ceiling, direction preserved, ODD-cap min honored); issue-#70 Degraded re-initiation + speed-increase denials for all finite inputs in their regions. **R1–R3** `parko-core rss.rs`: `longitudinal_safe_distance` fail-closed totality (finite ∧ ≥ 0) over the FULL f64 domain; closing-speed monotonicity on the integer-scaled operational grid — the precondition `occlusion_limited_speed`'s bisection relies on; invalid brake → exactly `RSS_FAILSAFE_DISTANCE_M` | **DONE (initial set, 12 properties)** — scope honestly excludes the P6 `tan`/`atan` bicycle-model path (transcendentals; covered by MC/DC + property tests above). DONE: lane flipped to blocking (L5). ACTION: extend toward the seqlock/contract-channel protocol (per the maturation roadmap) |
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
caps (the 16 KiB body bound already in place). Once bounded, the WCET bound sets
the SG9 timeout and confirms the per-cycle FTTI for SG1/2/3/7/9 — and any change
that breaks the bound is a safety regression caught in CI. Today's evidence for
that bound is the structural boundedness argument (`src/wcet_gate.rs`) plus the
host-indicative CI-measured p99.9 — **not a certified WCET**; the QNX/`SCHED_FIFO`
target measurement is tracked in #274.

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
