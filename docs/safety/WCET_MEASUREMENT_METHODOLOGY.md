# WCET Measurement Methodology — Governor Timing Evidence Strategy

| Field | Value |
|---|---|
| Doc ID | **KIRRA-OCCY-WCET-METH-001** |
| Status | **DRAFT — pending formal safety-engineer review** |
| Date | 2026-06-11 |
| Deciders | Project owner (methodology); a human safety engineer is the review gate |
| Issues | #274 (QNX-target measurement + hardware bring-up); #279 (fault-injection campaign — shares this methodology's timing criteria); EPIC #270 |
| Builds on | `GOVERNOR_INTEGRITY_EVIDENCE.md` (KIRRA-OCCY-INTEG-001) §3 WCET budget; `HYPERVISOR_CONTRACT_CHANNEL.md` (KIRRA-OCCY-HVCHAN-001) §4; the #271 harness + #273 spike host-indicative rule |
| Governs | every timing number that backs an FTTI / SG9 claim (scope table, §6) |

> # ⚠️ DRAFT — methodology, not yet-collected evidence
>
> This document specifies **how** the Governor's worst-case execution time (WCET)
> evidence is to be produced so that the eventual numbers are **defensible**. It is
> the measurement *protocol*, not a results record. **No number here is a WCET
> claim** — the on-target campaigns (#274 / #279) produce the numbers, against this
> bar. A human safety engineer must review and accept this methodology before any
> campaign's numbers are treated as certification evidence.

---

## 0. Question zero — the evidence-class declaration

**Before any number is collected, the class of timing evidence must be declared,
because the class determines what the number is allowed to claim.** Measuring is
not proving; the methodology is what turns a measurement into evidence.

**Chosen position: MEASUREMENT-BASED TIMING ANALYSIS** — high-water-mark
observation under *engineered worst-case load*, plus a *justified safety margin*.
This is **NOT** static / analytical WCET.

**Why measurement-based, not static:**
- **No qualified static-WCET tool is in the toolchain.** There is no aiT / OTAWA /
  qualified equivalent integrated or qualified for the target. Asserting a static
  WCET bound without a qualified tool would be a *less* defensible claim than an
  honest measurement-based one.
- **The code paths are short, loop-free or explicitly bounded** — the property that
  makes measurement-based analysis tractable and a static bound *eventually*
  feasible. The boundedness is already argued structurally in `src/wcet_gate.rs`:
  the verdict path is a **fixed linear pipeline of P0..P6 guards on a single
  command — O(1), no loops, no recursion, no heap allocation, no locks**; the judge
  (`tools/qnx-rtm-harness/kirra_judge.rs`) runs a **fixed check order** (magic →
  sequence → deadline → integrity → kinematic) with no data-dependent iteration;
  the gateway `validate_vehicle_command` and `validate_cmd_vel` are **O(1) scalar
  math**. There is no horizon loop or agent-count loop on the per-command verdict
  path (those bounded loops live in the planner-side paths, capped by input-size
  bounds).

**Margin policy: VALIDATION-PENDING.** The safety margin applied to the observed
high-water-mark is **deliberately unset here** — it is fixed **with the FTTI
budget** (the SG9 timeout closure, `GOVERNOR_INTEGRITY_EVIDENCE.md` §3) on the
target, in the #274 campaign. The margin is not a free parameter: it must be
justified against the observed run-to-run dispersion (§4) and the residual
between a measured high-water-mark and a true worst case (below).

**The honest limit of this evidence class (stated, not buried):**
- A **measured high-water-mark is a LOWER BOUND on the true WCET** — you have
  observed *at least* this, never *at most* this. A path slower than any observed
  run can always exist in principle.
- The **margin + the worst-case-load engineering (§3) are what close that gap**: by
  driving the measured path with the genuinely worst input/load/contention, the
  observed high-water-mark is pushed as close to the true worst case as the
  environment allows, and the margin covers the residual.
- **Known possible escalation (named, not a surprise):** for the highest-ASIL items
  (the ASIL-D verdict path), **an assessor may require static / analytical WCET**
  in addition to measurement. This is a recognized possible outcome of safety-engineer
  review, not a defect of this plan. The structural O(1)/no-loop/no-alloc argument in
  `src/wcet_gate.rs` is precisely the groundwork a static-analysis escalation would
  build on; the qualified-toolchain prerequisite for it is tracked separately
  (`GOVERNOR_INTEGRITY_EVIDENCE.md` §6, Ferrocene; and the cert-stage `qnx710` vs
  `qnx800` decision in `docs/adr/KIRRA_QNX_CROSSCOMPILE.md`).

---

## 1. The six assessor questions (answered per environment)

A measurement-based timing claim survives assessment only if six questions are
answered for the environment that produced it. The two environments differ in
**evidential weight**: the Governor partition is the only one whose numbers may
back an FTTI claim; the guest is diagnostic-only.

| # | Assessor question | **Governor partition (QNX)** | **Guest (Ubuntu / Autoware)** |
|---|---|---|---|
| 1 | **Tool** — what measures the time? | The instrumented QNX kernel + **tracelogger / System Analysis Toolkit (SAT)** on the target; a monotonic counter in the **boundary clock domain** (HVCHAN §5 R-HV-3 / AOU-TIMESYNC-001) for in-path stamping. *(Not yet covered by `KIRRA_QNX_CROSSCOMPILE.md`, which is SDP-8.0 build/deploy only — establishing + verifying the trace toolchain on target is **#274 work**.)* | **LTTng / perf.** |
| 2 | **Tool overhead** — what does measuring cost? | Tracepoint + counter-read cost; **must be measured on target**, not assumed. | LTTng/perf probe cost. |
| 3 | **Overhead bounded?** — is that cost bounded & accounted? | **YES, normative — the observer-effect rule (§2).** Overhead is measured, bounded, and either **subtracted with justification** or **included conservatively**; production builds **strip or bound** tracing. | N/A for evidence — guest numbers back no claim. |
| 4 | **Repeatability** — does it reproduce? | Run-to-run dispersion reported with a stated **repeatability bound** (§4); environment frozen per campaign (§4). | Indicative only; reproducibility noted but not gating. |
| 5 | **Worst-case load applied?** — was the path driven to its worst case? | **YES** — the §3 worst-case-load definition, executed as the **same measurement** as the #279 fault-injection campaign. | Whatever load the diagnostic scenario imposes — not engineered to worst case. |
| 6 | **Environment-representativeness** — is the measured environment the deployed one? | The **certified target**: QNX under **FIFO scheduling**, the partition config of HVCHAN §5, on representative hardware. Cert-stage caveat: the prototype runs SDP-8.0 / `qnx800` QM Rust; the *certified* artifact is the qualified toolchain (`KIRRA_QNX_CROSSCOMPILE.md` caveats) — a campaign records which it ran on. | **NON-representative** of the Governor partition by construction. |

### Guest environment is explicitly NON-EVIDENCE for Governor WCET

LTTng / perf on the Ubuntu/Autoware guest is **diagnostic only**. Its legitimate
role is **locating guest-side latency** (which subsystem, which callback, where a
stall originates) — never backing a Governor FTTI claim. A guest number must never
be presented as, or silently rolled into, a Governor WCET. This mirrors the
two-domain separation of HVCHAN §5: the guest lives in *system timing*; only the
*boundary domain* on the Governor partition produces timing evidence.

---

## 2. The observer-effect rule (normative)

Instrumentation perturbs the thing it measures. For a measurement-based claim this
is not a footnote — it is question 3, and it is **normative**:

1. **Measure the instrumentation overhead** on the target (tracepoint cost,
   counter-read cost) — do not assume it negligible.
2. **Bound it.** State the measured overhead as a bounded quantity per measured
   path.
3. **Account for it,** one of two ways, the choice justified per campaign:
   - **Subtract** the bounded overhead from the observed time — *only* with an
     explicit justification that the overhead is stable and its bound is sound; or
   - **Include** it conservatively (do not subtract) — the simpler, more defensible
     default, which over-states the path slightly and therefore never under-states
     WCET.
4. **Production builds strip or bound tracing.** Tracing runs **in campaigns, not in
   the certified hot path** — the deployed Governor does not carry campaign
   instrumentation. (Consistent with `src/wcet_gate.rs` making the verdict path
   lock-free / alloc-free in production; instrumentation must not reintroduce on the
   certified path what the WCET argument removed.)

---

## 3. Worst-case load definition (what "worst case" means per path)

"Engineered worst-case load" must be *defined per measured path*, or the
high-water-mark is just a typical-case number wearing a worst-case label. The
timing criteria here and the **#279 fault-injection catalogue are the SAME
measurement** — a single instrumented campaign produces both the fault verdict
(does the barrier hold) and the timing (how long under that load), never two
disjoint runs.

**For the cross-partition boundary (HVCHAN snapshot → bounds → CRC → judge →
digest):**
- **Max-size payload** — `command_len = MAX_COMMAND_BYTES` so the CRC and the
  bounds/copy path run over their largest input (CRC cost scales with payload).
- **Max-rate publication** — the guest publisher writing at its highest sustained
  generation rate, to maximize seqlock retry pressure (HVCHAN §3 step 3, the
  `MAX_SNAPSHOT_RETRIES` path) and contention on the shared region.
- **Concurrent fault injection** — the #279 catalogue's faults driven *during* the
  timing run (torn-write churn, retry-exhaustion, CRC/bounds rejects, replay/regress,
  clock-skew / cross-domain timestamp). The worst-case path is often a *rejection*
  path under churn, not the clean-accept path — so timing must cover the §4
  failure-semantics rows, each attributed to its owning barrier (HVCHAN §4 / #279
  taxonomy), not only the happy path.
- **Cache policy stated.** Each campaign records **cache-cold vs cache-warm**: the
  conservative bound is the **cache-cold first-touch** path (no warm caches, no
  branch-predictor priming); a cache-warm steady-state number is reported alongside
  but is **not** the WCET basis. The warmup discipline (§4) makes this explicit.

**For the kinematic gateway / judge** (`validate_vehicle_command`,
`kirra_judge_assess`): the worst case is the **longest fixed path through the P0..P6
/ check-order pipeline** — the input that forces evaluation of every guard before a
verdict (e.g. a command that passes P0..P5 and is decided at P6), measured
cache-cold, under whatever scheduling contention the partition config permits.

---

## 4. Measurement protocol

Every campaign that produces a timing number for the scope table (§6) follows this
protocol; deviations are recorded.

- **Warm-up.** A recorded number of warm-up iterations precedes the measured set,
  *and* a separate **cache-cold** measurement set is taken (§3) — the cold set is the
  conservative basis, the warm set is reported for context only.
- **Iteration count.** A recorded, sufficient iteration count (the #271 harness uses
  `kWarmup = 1000`, `kIters = 8000` as a host precedent; the target count is set per
  campaign to achieve a stable tail).
- **Percentile reporting.** Report **p50, p99, p99.9, and max** — the tail and the
  max are the WCET-relevant statistics; p50 is context. (The #271 harness reports
  p50/p99/max today; **p99.9 is added here** as the tail the FTTI argument needs, per
  the S3 precedent which already reports p99.9.)
- **Run-to-run repeatability bound.** Re-run the campaign; report the dispersion of
  the max/p99.9 across runs against a stated **repeatability bound**. A campaign whose
  tail does not reproduce within the bound is not yet evidence.
- **Environment freeze.** Record, per campaign: the **kernel build/version, the
  scheduling config (FIFO + priorities), the partition/hypervisor config (HVCHAN §5),
  the toolchain (`qnx800` QM vs qualified), the hardware**, and the clock primitive +
  its measured read cost. A number is only interpretable against its frozen
  environment.
- **The host-indicative rule (already enforced — do not regress it).** Host numbers
  are **INDICATIVE, never WCET.** The #271 harness and #273 spike already state this
  in-banner: *"Certified WCET must be measured on the QNX target under FIFO
  scheduling (#274); host numbers are NEVER presented as WCET"* — and the harness CSV
  carries `wcet_status = TBD-QNX-TARGET`. **Only target-measured-under-FIFO numbers
  feed an FTTI claim.** This methodology generalizes that rule to every path in §6.

### 4a. Probabilistic WCET (pWCET) via EVT/MBPTA — WP-22 (G-3 software half)

The max/p99.9 above are ORDER STATISTICS of the observed set — a longer campaign
can always exceed them. Measurement-based probabilistic timing analysis (MBPTA)
instead fits the tail and **extrapolates** an execution time exceeded with only a
target probability `p`. Implemented in `kirra_timing::evt` (feature-gated `evt`,
host-analysis only — the certifiable `no_std`/zero-alloc core is untouched):

- **Peaks-over-threshold (POT).** Choose a high threshold `u`; the exceedances
  `x − u` are fit to a **Generalized Pareto Distribution** `(ξ, σ)` by method of
  moments (`fit_gpd_pot`, valid `ξ < 0.5`, fail-closed on degenerate input).
- **pWCET return level.** `pwcet_return_level` gives `x_p = u + (σ/ξ)[(p/ζ)^{−ξ}−1]`
  (`ζ = n_exceed/n_total`), refused unless the target `p` is rarer than `ζ` (a true
  extrapolation). Reported ALONGSIDE — never instead of — the HWM/p99.9.
- **Representativity gates.** A fit is only meaningful on i.i.d., stationary data:
  `lag1_autocorrelation` (≈0) and `stationarity_split_mean_ratio` (≈1) are computed
  and reported with every estimate; `pwcet_converged` checks the estimate has
  stabilized over a growing campaign. An unmet gate keeps the number INDICATIVE.
- **The host-indicative rule is UNCHANGED.** A pWCET fitted from HOST samples is
  still INDICATIVE — the evidence class is fixed by the measurement ENVIRONMENT
  (`MeasurementEnv::is_certified_wcet`), not by the statistics. **The CI wcet-gate
  keeps p99.9; no gate uses a pWCET curve until target-under-FIFO data exists.**

---

## 5. Precedent reconciliation — the existing S3 gateway evidence

The Governor already has timing evidence that **predates this methodology**, and it
must be reconciled honestly — **not silently grandfathered**.

**What exists.** `GOVERNOR_INTEGRITY_EVIDENCE.md` (KIRRA-OCCY-INTEG-001) §5 records,
for the `validate_vehicle_command` verdict path: **CI-measured steady-state p99.9 =
170–352 ns; max with OS jitter ≤ 219 µs.** The deployment target constant is
`GOVERNOR_VERDICT_WCET_TARGET_MICROS = 100` with a CI regression guard at
`GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS = 1000` (`src/wcet_gate.rs`).

**How it was produced.** Two halves:
1. A **structural boundedness argument** (`src/wcet_gate.rs`): O(1) per call, no
   loops, no recursion, no heap allocation, no locks; `panic = "abort"`; the verdict
   path made lock-free in production (the S3 "Pass A / Pass B" work).
2. A **measurement**: CI-measured on **shared CI runners** (the threshold is
   explicitly "generous for shared-runner variance"), with the document itself
   flagging **"target hardware re-measure under S8/#120."**

**Does it meet this methodology's bar?** Split verdict:
- **The structural argument CONFORMS** — and is in fact the *foundation* this
  methodology's evidence-class rests on (§0): the O(1)/no-loop/no-alloc/lock-free
  property is exactly what makes measurement-based analysis valid and a future static
  bound feasible. It carries forward unchanged.
- **The NUMBERS do not yet conform** — they are **host/CI-runner** measurements, not
  **QNX-target-under-FIFO** numbers, and they predate the §1–§4 discipline (no frozen
  QNX environment, no observer-effect accounting on target, no FIFO config, host not
  representative). Under this methodology's own host-indicative rule (§4), CI numbers
  are **indicative**.

**Disposition: RE-MEASURE-UNDER-METHODOLOGY.** The S3 numbers
(170–352 ns p99.9 / ≤ 219 µs max) stand as **indicative** evidence and as the CI
regression guard, **not** as the certified WCET. They are **re-measured on the QNX
target under FIFO at the next campaign (#274)**, against §1–§4, and the
target-measured tail — not the CI tail — becomes the FTTI basis. The structural
argument is grandfathered (it is environment-independent); the *numbers* are not.

---

## 6. Scope — paths this methodology governs

| Path | What it is | Target environment | Where the numbers land |
|---|---|---|---|
| **Kinematic gateway / verdict** | `validate_vehicle_command` / `validate_cmd_vel` — the P0..P6 verdict pipeline (`src/wcet_gate.rs`) | QNX Governor partition, FIFO | #274 (re-measure §5); CI guard `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS` holds the regression line |
| **HVCHAN snapshot→validate→digest** | The cross-partition trust chain: seqlock snapshot → bounds → CRC → judge → digest → release token (HVCHAN §3) | QNX Governor partition, FIFO; HVCHAN §5 partition config | #278 (the HVCHAN hardware half) + #274 |
| **Judge (contract checker)** | `kirra_judge_assess` fixed check order (#271 harness today on host) — a **PROXY**: integer-only, `no_std`, crypto-free, FP-free, talisman-free, with a proxy velocity bound | QNX Governor partition per #274 (host = indicative only) | #271 harness reports host-indicative now; #274 produces the target-under-FIFO numbers. **NOTE (W1 #1027): this proxy does NOT represent the assembled release loop below** |
| **Assembled release loop (EP-01)** | `kirra_inline_governor::govern_and_release` — the DEPLOYED enforced cycle: verdict (P0..P6 + the FP `validate_vehicle_command` talisman) **+ release token** (2× SHA-256 + Ed25519 **sign** + Ed25519 **verify_strict**) | QNX Governor + Actuator partitions, FIFO, pages locked (W2 #1028) | **REMAINDER (W1 #1027): NOT yet target-measured-under-FIFO.** The `kirra_judge_assess` proxy (row above) does NOT stand in for it. Host-indicative today via `wcet_gate::regression_inline_loop_full_step` against the **SEPARATE** `GOVERNOR_RELEASE_TOKEN_WCET_*` budget (W3 #1032); porting the assembled loop to the #271/#274 harness is the open obligation |
| **FFI / fault-injection timing criteria** | The timing half of the #279 fault-injection catalogue — measured as the SAME campaign as the fault verdicts (§3) | QNX Governor partition, FIFO; HVCHAN §4 barrier attribution | #279 (timing criteria == this methodology, not a second standard) |

Each path's number is only a WCET once it is target-measured-under-FIFO per §1–§4,
margined per §0 (VALIDATION-PENDING until the FTTI closure), and accepted in
safety-engineer review.

**W1 (#1027) — the on-target proxy caveat (scoping the FTTI evidence honestly).**
The only on-QNX-target number today is `kirra_judge_assess` (row "Judge"): a
crypto-free, FP-free, talisman-free checker with a *proxy* velocity bound (min
31 ns / p99.9 79 ns, `tools/qnx-rtm-harness/results/`). It establishes the harness
and the verdict-**correctness** FDIT/RTM matrix on target — but it is structurally
UNLIKE the **deployed** enforced loop (row "Assembled release loop"), whose dominant
timing terms — 2× Ed25519, 2× SHA-256, and the floating-point
`validate_vehicle_command` talisman — have **zero target-under-FIFO timing
evidence**. Two consequences, stated plainly so the safety case does not over-claim:

- The FTTI **reaction** budget (0.5 s) is **not** at risk: the whole crypto term is
  ~0.06 % of it (see `GOVERNOR_INTEGRITY_EVIDENCE.md` §3 and the W3 #1032 budget
  split), so nothing here changes the fail-closed-within-FTTI argument.
- But **no tighter control-cycle bound is earned** for the assembled loop until it is
  ported to the #271/#274 harness and measured under FIFO with pages locked. Until
  then the assembled-loop timing is **HOST-INDICATIVE only** (`wcet_gate::
  regression_inline_loop_full_step`, gated on the separate `GOVERNOR_RELEASE_TOKEN_WCET_*`
  budget — W3 #1032), never a WCET, and the certified FTTI budget explicitly **scopes
  out** the release-token crypto pending that measurement. The proxy-judge number MUST
  NOT be cited as representative of the enforced path.

---

## 7. Traceability hooks

- **Safety-case registration:** registered as **KIRRA-OCCY-WCET-METH-001** in
  `docs/safety/SAFETY_CASE_INDEX.md` (this change).
- **Cross-references:**
  - `GOVERNOR_INTEGRITY_EVIDENCE.md` (KIRRA-OCCY-INTEG-001) §3 (WCET budget / SG9
    closure), §5 (the S3 precedent reconciled in §5 here), §6 (Ferrocene — the
    qualified-toolchain prerequisite for any static-analysis escalation).
  - `HYPERVISOR_CONTRACT_CHANNEL.md` (KIRRA-OCCY-HVCHAN-001) §3 (the measured trust
    chain), §4 (#279 barrier-attribution taxonomy — the same campaign as the timing),
    §5 (the boundary clock domain the measurements are stamped in).
  - `ASSUMPTIONS_OF_USE.md` **AOU-TIMESYNC-001** — the boundary-domain timestamp
    discipline; timing is measured in that domain, never wall/PTP.
  - `docs/adr/KIRRA_QNX_CROSSCOMPILE.md` — the QNX SDP-8.0 build/deploy/target-tuple
    environment; the cert-stage `qnx710`-qualified vs `qnx800`-prototype caveat. **The
    trace toolchain (tracelogger / SAT) + FIFO config are NOT yet covered there —
    establishing and verifying them on target is #274.**
  - `src/wcet_gate.rs` — the structural boundedness argument + the CI guard constants.
  - The #271 `tools/qnx-rtm-harness/` + #273 `tools/iceoryx2-spike/` — the
    host-indicative-never-WCET precedent this methodology generalizes.
  - **EPIC #270**; **#274** (QNX-target measurement / hardware bring-up); **#279**
    (the fault-injection campaign whose timing criteria are this methodology).
