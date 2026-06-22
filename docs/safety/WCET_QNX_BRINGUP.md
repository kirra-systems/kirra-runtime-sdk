# WCET QNX Bring-Up (#274) — verdict-path WCET on a QNX SDP 8.0 target

| Field | Value |
|---|---|
| Status | **Draft — defined build, not run** (no QNX hardware in this task). |
| Date | 2026-06-21 |
| Owner | Project / safety-case owner |
| Issue | #274 (EPIC #270, QNX governor lane). RTM tracing #272 (done). |
| Scope | Cross-compile the no_std verdict **judge** (`tools/qnx-rtm-harness/kirra_judge.rs`) for a QNX target and measure its per-verdict WCET under `SCHED_FIFO`, replacing the harness placeholder `wcet_status = TBD-QNX-TARGET`. |
| Companions | `tools/qnx-rtm-harness/` (README, `QNX_MAPPING.md`, `CMakeLists.txt`, `kirra_judge.rs`, `kirra_ffi.h`, `wcet_measure.cpp`), `docs/adr/KIRRA_QNX_CROSSCOMPILE.md`, `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`, `src/wcet_gate.rs`, `ASSUMPTIONS_OF_USE.md` (AOU-HW-QNX-TARGET-001), ADR-0001. |

## Hard boundaries (read first)

**1. A Jetson cannot run QNX.** Jetson modules (Orin NX / AGX Orin / Jetson Thor) run **L4T / Linux** — they are the *doer-side* robotics lane (Parko inference, the Rosmaster R2 bring-up, Mick/Taj). They have **no QNX support** and **cannot** produce a QNX-target WCET row (AOU-HW-QNX-TARGET-001). The measurement runs on a **separate** QNX target. Never describe it as "on Jetson under QNX."

**2. Two QNX targets, two phases.** The Phase I target is **self-established** (a QNX SDP 8.0 eval/dev license + a target you stand up) — it is **not** gated on the DRIVE partner path, which is Phase II by design. The Phase I number is therefore a *setup task we control*, not a hardware-access wait.
- **Phase I (feasibility):** QNX SDP 8.0 on an **aarch64 eval board** (better ISA match → cleaner Phase I→II extrapolation; longer setup) or an **x86_64 VM** (lowest-friction, stands up in days — the pragmatic now-path on the proposal timeline). Either produces a *real* QNX-target-under-`SCHED_FIFO` number — far stronger than the host-Linux-indicative CI number, sufficient to substantiate the Phase I sub-100 µs verdict claim. **Not cert-grade.**
  - **Write-up discipline (ISA honesty).** An x86_64 VM number demonstrates *the bound holds on a real RTOS target under SCHED_FIFO* — frame it **exactly** that way. Do **not** call an x86_64 VM "representative edge hardware": the deployment ISA is aarch64 (Orin/Thor-class), so the aarch64-board and cert-grade DRIVE measurements are **Phase II**. The corrected proposal Task 1 already avoids this overclaim; keep the write-up aligned.
- **Phase II (cert-grade):** **NVIDIA DRIVE AGX Orin / Thor + QNX OS for Safety**, Ferrocene-qualified Rust — the certified FTTI number on the deployment ISA. (Ferrocene's *qualified* QNX target is `qnx710`, **not** `qnx800`; the cert Rust toolchain is its own decision — see `KIRRA_QNX_CROSSCOMPILE.md`.)

**3. Lane.** This is **checker-side Objective 1** — distinct from the Parko / §2-readiness work and the Mick / Taj / Linux robotics lane.

## Why this sidesteps the #189 / #66–#67 blockers

The WCET target is the **`#![no_std]`, zero-alloc, `core`-only** judge. It links `core` + a few platform symbols (`pthread / dl / m`) and uses **no `std` / `libc` / `socket2` / `tokio`**. So it does **not** depend on the QNX `std` package that #189 (nto80 libc) is blocked on, nor the async networking #66/#67 gate. Those block the **full async verifier**; they do **not** block the verdict core. The judge is dependency-free **by design** precisely so the WCET artifact is portable to a cert target ahead of the full runtime.

## 1. Cross-compile recipe

### 1a. Target triple — TBD pending the Phase-II / DRIVE-access decision

```
<TARGET_TRIPLE_TBD>
```

| Phase / target | Triple |
|---|---|
| Phase I eval board (aarch64) | `aarch64-unknown-nto-qnx800` |
| Phase I VM (x86_64) | `x86_64-pc-nto-qnx800` |
| Phase II DRIVE Orin / Thor (aarch64) | `aarch64-unknown-nto-qnx800` (cert Rust = Ferrocene `qnx710` — separate decision) |

Do **not** hard-pin the triple in the build until the target is confirmed; substitute `<TARGET_TRIPLE_TBD>` everywhere below.

### 1b. The judge — `rustc` → `libkirra_judge.a` (no_std staticlib, QNX target)

Host build today (from `CMakeLists.txt`):

```bash
rustc --edition 2021 --crate-type staticlib --crate-name kirra_judge \
      -C panic=abort -C opt-level=2 -C debuginfo=0 \
      -o libkirra_judge.a kirra_judge.rs
```

QNX cross-build — add the target; **nothing else about the judge changes** (it is no_std, so no QNX `std` is required):

```bash
# Source the QNX SDP 8.0 env (provides qcc + the nto target machinery):
source ~/qnx800/qnxsdp-env.sh

# Because the judge is no_std (core-only), prefer QNX SDP 8.0's bundled Rust
# (it ships the nto-qnx800 target). With upstream nightly instead, `core` is
# available without building std — `-Zbuild-std=core` — or via a custom
# target.json for the nto tuple. rustup ships NO prebuilt std for nto (and we
# need none here).
rustc --edition 2021 --target <TARGET_TRIPLE_TBD> \
      --crate-type staticlib --crate-name kirra_judge \
      -C panic=abort -C opt-level=2 -C debuginfo=0 \
      -o libkirra_judge.a kirra_judge.rs
```

### 1c. The C++ shim + harness + measurement — `qcc` / `q++`

C++17, exception-free, RTTI-free (the integration-boundary discipline). Build with QNX's `qcc` / `q++`:

```bash
qcc -V    # confirm the variant string in YOUR install, e.g. gcc_ntoaarch64_cxx

q++ -Vgcc_ntoaarch64_cxx -std=c++17 -Wall -Wextra -Werror -fno-exceptions -fno-rtti \
    -O2 -I tools/qnx-rtm-harness \
    tools/qnx-rtm-harness/kirra_shim.cpp tools/qnx-rtm-harness/wcet_measure.cpp \
    -o wcet_measure \
    libkirra_judge.a -lpthread
# QNX provides pthread/m/dl via libc; -ldl/-lm may be unneeded on QNX — confirm per `qcc -V`.
```

### 1d. CMake hook extension (gated; host build unchanged)

The existing hook is a **comment** in `CMakeLists.txt`. Make it real, gated by a `KIRRA_QNX_TARGET` option so the host build stays byte-identical when OFF:

```cmake
option(KIRRA_QNX_TARGET "Cross-compile the judge + harness for a QNX target" OFF)
set(KIRRA_RUSTC_TARGET "<TARGET_TRIPLE_TBD>" CACHE STRING "rustc nto-qnx800 tuple")

set(KIRRA_JUDGE_RUSTC_FLAGS --edition 2021 --crate-type staticlib
    --crate-name kirra_judge -C panic=abort -C opt-level=2 -C debuginfo=0)
if(KIRRA_QNX_TARGET)
  list(APPEND KIRRA_JUDGE_RUSTC_FLAGS --target ${KIRRA_RUSTC_TARGET})
  # select qcc/q++ via -DCMAKE_TOOLCHAIN_FILE=qnx.cmake, and add wcet_measure.cpp
  # as an executable linked against ${KIRRA_JUDGE_LINK}.
endif()
# ... feed ${KIRRA_JUDGE_RUSTC_FLAGS} into the rustc add_custom_command ...
```

## 2. SCHED_FIFO measurement wrapper

Drop-in: **`tools/qnx-rtm-harness/wcet_measure.cpp`** (in this PR). It times the verdict ENTRY `kirra_judge_assess` (`kirra_ffi.h`) and emits the `wcet_status` CSV row. Key properties it encodes:

- **WCET path = the OK / admissible view.** The judge runs `magic → sequence → deadline → integrity → kinematic` in order and returns early on the first failure — so the longest path is the all-pass case. A failing case would time a short-circuit, not the WCET. The wrapper asserts the view returns `KIRRA_VERDICT_OK` before measuring.
- **`SCHED_FIFO`, max priority, pinned isolated core.** POSIX `pthread_setschedparam(SCHED_FIFO, max)` + affinity (works on a Linux eval VM with `isolcpus=<cpu>`). On QNX, also set the runmask via `ThreadCtl(_NTO_TCTL_RUNMASK_GET_AND_SET_INHERIT, ...)`. SCHED_FIFO needs privilege; the wrapper warns and downgrades to INDICATIVE if not granted.
- **Cache / branch-predictor warm-up** (10 000 discarded iterations) before the measured 1 000 000.
- **Capture `max` + `p99.9`** (sorted-tail percentile) + `min` / `median` for context.
- **Monotonic high-res clock** (`clock_gettime(CLOCK_MONOTONIC)`); the constant clock self-overhead sits inside each sample, making the reported MAX a **conservative** (slightly over) bound — safe for a WCET. On QNX prefer `ClockCycles()` + `SYSPAGE` `cycles_per_sec` and subtract the measured clock overhead.

The wrapper is **not yet wired into CMake** — it is staged for the gated QNX build (§1d).

## 3. Output — the CSV row + FTTI linkage

```
metric,target,sched,n,warmup,max_ns,p999_ns,wcet_status
kirra_judge_assess,<TARGET_TRIPLE_TBD>,SCHED_FIFO,1000000,10000,<max>,<p999>,QNX-TARGET-MEASURED
```

- Replaces the harness placeholder `wcet_status = TBD-QNX-TARGET`.
- The **structural boundedness argument** (`src/wcet_gate.rs` — a finite WCET exists by construction; **target 100 µs**, host CI threshold 1000 µs) supplies *that a bound exists*; this measurement supplies *its magnitude on a real QNX / FIFO target*. Together they feed the FTTI: `verdict_WCET + actuation_latency < control_cycle < 0.5 s reaction`.
- **Phase I** number = feasibility (eval / VM). **Phase II** = cert-grade on DRIVE + QNX OS for Safety + Ferrocene-qualified Rust.

## 4. Phase-I acceptance criteria + feasibility signal

What the Phase-I run must show to substantiate the Objective-1 "sub-100 µs verdict" claim (and what a reviewer needs to see):

1. **The judge cross-compiles** to a `*-nto-qnx800` target with the §1 recipe — `libkirra_judge.a` links `core` + platform symbols only, no QNX `std` (this is the #189/#66–#67 sidestep made concrete).
2. **The FDIT/RTM matrix passes byte-identically on the target** — every `QNX_MAPPING.md` row's `verdict_observed == verdict_expected`. Cross-compilation must not change a single verdict; the gate is VERDICT CORRECTNESS, timing is reported alongside.
3. **A real `SCHED_FIFO` `max` + `p99.9` for `kirra_judge_assess`** on the OK/admissible (WCET-path) view, captured per §2, replacing `wcet_status = TBD-QNX-TARGET` with `QNX-TARGET-MEASURED`.
4. **`max < 100 µs`** (the `src/wcet_gate.rs` target). The structural argument already guarantees a finite bound *exists*; Phase I supplies its *magnitude* on a real QNX/FIFO target.

**Feasibility signal (INDICATIVE — never WCET; see Hard boundaries).** The host harness already runs the verdict per-call in the **sub-microsecond** range (`QNX_MAPPING.md` regression rows: p50 ≈ 0.5 µs, p99 ≈ 0.5 µs on the OK row), with even the scheduling-noise-inflated host *max* ≈ 27 µs — all comfortably under the 100 µs target. So the Phase-I target-FIFO measurement is expected to **confirm** feasibility, not discover a problem: the risk is "produce the certifiable number on the right OS," not "find out whether the bound is met." This is why Objective 1 is a **defined build, not research** — only a QNX target (the #274 blocker) stands between the recipe and the row.

## Done vs. remaining

| | State |
|---|---|
| no_std judge (C-ABI `kirra_judge_assess`) | done (`kirra_judge.rs`) |
| host-indicative WCET + CI regression gate | done (`src/wcet_gate.rs`) |
| FDIT/RTM matrix traced to kernel RTM | done (#272, `QNX_MAPPING.md`) |
| cross-compile recipe + measurement wrapper | **done (this PR — drafted, not run)** |
| run on a QNX target, capture the row, update CSV + methodology | **remaining (#274 — needs a QNX target)** |
