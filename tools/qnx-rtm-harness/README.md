# KIRRA QNX RTM Harness (#271)

C++ **shim** (driver) → Rust **judge** (checker) over a frozen `extern "C"`
contract ABI, with an automated **FDIT / RTM fault-injection** matrix that proves
**verdict correctness** across nine fault classes. Part of **EPIC #270**
(iceoryx2 transport / QNX governor lane). Each row is **traced to the kernel RTM**
(**#272** — see `QNX_MAPPING.md`): one genuine hit (SG-001/TR-001, proxy), seven
honest `NO-RTM-ID` transport-contract gaps, one control.

> Built with **g++ + rustc directly (no cargo)**. The Rust judge is a `no_std`
> `staticlib` — dependency-free, mirroring the QNX cross-compile shape.

## ADR-0006 Clause 3 — what this FFI *is*

This boundary is the concrete realization of **ADR-0006 Clause 3**: the C ABI /
FFI is retained **only as the documented integration boundary for C/C++
components** (DDS bridges, vendor stacks) — it is **no longer the governor hot
path**. The harness exists to give that boundary cert-grade evidence; it is not a
runtime component.

## The concern split (driver vs checker)

| Concern | Owner | What it does |
|---|---|---|
| memory / transport safety | **C++ shim** (`kirra_shim.*`) | odd/even generation-**seqlock** tear detection on the header (real `atomic_thread_fence(acquire)`, mirrors `kirra-contract-channel`); **bounds** rejection (oversize **short-circuits in the shim and NEVER crosses the FFI**); in-place **CRC** over the payload |
| contract verdict | **Rust judge** (`kirra_judge.rs`) | magic → sequence → deadline → integrity → kinematic, on the **stabilized snapshot** the shim hands it |

So `PayloadOversize` and `PayloadCorrupt` are produced by the **shim** (the judge
is never called — see the matrix, where SG-05/SG-04 are markedly faster); the
other verdicts come from the **judge**. The shim does **not** pre-filter equal
sequences — **replay rejection is the judge's job**.

## Build & run

```sh
cd tools/qnx-rtm-harness
cmake -S . -B build
cmake --build build
ctest --test-dir build            # a WRONG VERDICT fails the build
./build/rtm_harness               # prints the matrix + CSV
./build/kirra_demo                # minimal end-to-end demo incl. a replay
```

Flags: `-Wall -Wextra -Werror -fno-exceptions -fno-rtti`. The Rust staticlib is
built `--crate-type staticlib -C panic=abort` and linked into the C++ executables.

## The fault matrix (gate = verdict correctness only)

Nine rows, each named for **exactly** what it injects, each carrying its grounded
**RTM** mapping (the `row` column is the LOCAL harness index, **not** an RTM id —
the `rtm` column is the bridge). Host run:

```
row    fault class            rtm            ok     verdict             p50(ns)    p99(ns)    max(ns)
-------------------------------------------------------------------------------------------------
SG-00  valid                  CONTROL        PASS   Ok                      182        198      33456
SG-01  bad-magic              NO-RTM-ID      PASS   StaleHeader             182        185      28346
SG-02  sequence-regress       NO-RTM-ID      PASS   SequenceRegress         181        185      20384
SG-03  deadline-missed        NO-RTM-ID      PASS   DeadlineMissed          182        193      24340
SG-04  payload-corrupt (CRC)  NO-RTM-ID      PASS   PayloadCorrupt          180        183      21842
SG-05  payload-oversize       NO-RTM-ID      PASS   PayloadOversize          21         23       2911
SG-06  over-envelope          SG-001/TR-001  PASS   KinematicLimit          182        193      27398
SG-07  replay (seq==last)     NO-RTM-ID      PASS   SequenceRegress         182        185      45818
SG-08  torn-write (odd gen)   NO-RTM-ID      PASS   StaleHeader              21         22       2807

GATE (verdict correctness): PASS
```

The RTM mapping is **grounded** in `docs/safety/{SAFETY_GOALS,REQUIREMENTS_TRACEABILITY}.md`
(read-only). Only **over-envelope** has a genuine kernel TR home — **SG-001/TR-001**,
qualified as PROXY (proxy bound, reject-not-clamp). The other seven transport-contract
fault classes are honest **`NO-RTM-ID`** gaps (candidate new TRs for the EPIC #270
lane), and the valid row is the clean-accept **`CONTROL`**. Full per-row
justification + the surfaced coverage gaps: **`QNX_MAPPING.md`**.

- **SG-07 Replay** (`sequence == last_accepted`) is its **own row** and PASSes
  with `SequenceRegress`. The judge's rule is the corrected
  **`sequence <= last_accepted ⇒ reject`** (equal = replay, lower = regress;
  matching `tools/iceoryx2-spike/src/judge.rs`). `<` would be a replay hole and is
  **not** used.
- **SG-05** (and SG-04) short-circuit in the shim — the harness asserts the judge
  is **not** called for them (visible in the p50 ≈ 21 ns row).
- **SG-08 torn-write (odd generation)** exercises the shim's **odd/even generation
  seqlock** (HVCHAN-001 §3, mirroring `kirra-contract-channel`): an odd generation
  (write in progress) or a generation that changes across the copy is rejected as
  `StaleHeader` in the shim, before the FFI (so the judge is **not** called — it
  short-circuits like SG-05). This replaces the prior compiler-fence-only
  double-read-compare (review finding **S2**) with a real `atomic_thread_fence(acquire)`
  barrier + a monotonic counter — sound on weakly-ordered targets, not ABA-prone.

## Honesty (WCET-TBD)

The PASS gate is **verdict correctness**. The per-row p50/p99/max are
**indicative host FDIT timing, NOT certified WCET**. **Certified WCET must be
measured on the QNX 8.0 target under FIFO scheduling (#274)** — host numbers are
never presented as WCET.

The "QNX 8.0 target" is an NVIDIA **DRIVE** platform (DRIVE AGX Orin / DRIVE AGX
Thor) running DRIVE OS + QNX OS for Safety — **not** a Jetson module (Orin NX / AGX
Orin / Jetson Thor), which runs L4T/Linux and has no QNX support. The Jetson dev box
used for the parko inference bring-up therefore cannot produce a `TBD-QNX-TARGET`
row. See `docs/safety/ASSUMPTIONS_OF_USE.md` → **AOU-HW-QNX-TARGET-001**.

## The four corrections (vs the original draft)

1. **Sequence check** is `sequence <= last_accepted ⇒ SequenceRegress` (equal IS
   replay IS reject); SG-07 is its own named row, never folded into SG-02. `<` is
   not written.
2. The judge entry `kirra_judge_assess` is a **`pub unsafe extern "C" fn`** with a
   `# Safety` caller-contract section (CERT-005 RSR-001, `src/ffi.rs`); the null
   check + internal `// SAFETY:` deref are defense-in-depth, not a substitute.
3. The Rust file is **`kirra_judge.rs`**, not `kirra_core.rs` (the repo's
   `src/kirra_core.rs` is the governor — a grep collision).
4. The header struct is **naturally aligned, widest-first after the pointer** —
   **not** packed.

## PROXY constants

`PROXY_MAX_COMMANDED_VELOCITY` (in `kirra_judge.rs`) is a clearly-labelled
**PROXY**. The **certified** kinematic envelope lives in the untouched talisman
`src/gateway/kinematics_contract.rs`; this harness **references** it, never
imports it, and its number must never be read as a certified bound.

## QNX cross-build + on-target FDIT/WCET (#274)

The QNX cross-compile is now **real and gated**, not a comment. The host build is
**byte-identical** when `KIRRA_QNX_TARGET=OFF` (default) — `ctest` 2/2 as above.
To cross-build for an **x86_64 QNX SDP 8.0** target and run the FDIT matrix +
per-verdict WCET on it:

```sh
source ~/qnx800/qnxsdp-env.sh          # sets QNX_HOST/QNX_TARGET + qcc
tools/qnx-rtm-harness/run_qnx_fdit.sh  # builds judge (nto) + shim/harness/wcet (qcc)
# copy build-qnx/{rtm_harness,wcet_measure,kirra_demo} to the QNX target, then:
#   ./rtm_harness && echo PASS   # verdict-correctness gate, on-target
#   ./wcet_measure               # SCHED_FIFO WCET row (run as root); replaces TBD-QNX-TARGET
```

Pieces: `qnx.toolchain.cmake` (qcc/q++, x86_64 default), the `KIRRA_QNX_TARGET`
CMake path (adds `wcet_measure`, drops the host-only `-lpthread/-ldl/-lm`, skips
host ctest), and `run_qnx_fdit.sh` (handles the Rust-for-QNX toolchain: direct
`rustc --target`, else `cargo -Zbuild-std=core`, else a custom-target.json prompt).
Full recipe + Phase-I acceptance criteria: **`docs/safety/WCET_QNX_BRINGUP.md`**.

The harness→kernel-RTM tracing (**#272**, done) is in `QNX_MAPPING.md`; **adding
the candidate `NO-RTM-ID` TRs to the RTM itself is a separate `docs/safety/**`
change with its own review — not part of #272**, which only traces against the
RTM as it stands.
