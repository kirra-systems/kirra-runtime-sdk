# KIRRA QNX RTM Harness (#271)

C++ **shim** (driver) → Rust **judge** (checker) over a frozen `extern "C"`
contract ABI, with an automated **FDIT / RTM fault-injection** matrix that proves
**verdict correctness** across eight fault classes. Part of **EPIC #270**
(iceoryx2 transport / QNX governor lane). RTM-ID tracing is the follow-up **#272**.

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
| memory / transport safety | **C++ shim** (`kirra_shim.*`) | double-read **tear** detection on the header; **bounds** rejection (oversize **short-circuits in the shim and NEVER crosses the FFI**); in-place **CRC** over the payload |
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

Eight rows, each named for **exactly** what it injects. Host run:

```
id     fault class            ok     verdict             p50(ns)    p99(ns)    max(ns)
----------------------------------------------------------------------------------
SG-00  valid                  PASS   Ok                      501        520      20784
SG-01  bad-magic              PASS   StaleHeader             500        517      22050
SG-02  sequence-regress       PASS   SequenceRegress         503        529      34489
SG-03  deadline-missed        PASS   DeadlineMissed          500        517      23915
SG-04  payload-corrupt (CRC)  PASS   PayloadCorrupt          503        524      19721
SG-05  payload-oversize       PASS   PayloadOversize          39         55      17133
SG-06  over-envelope          PASS   KinematicLimit          504        525      17045
SG-07  replay (seq==last)     PASS   SequenceRegress         500        519      29045

GATE (verdict correctness): PASS
```

- **SG-07 Replay** (`sequence == last_accepted`) is its **own row** and PASSes
  with `SequenceRegress`. The judge's rule is the corrected
  **`sequence <= last_accepted ⇒ reject`** (equal = replay, lower = regress;
  matching `tools/iceoryx2-spike/src/judge.rs`). `<` would be a replay hole and is
  **not** used.
- **SG-05** (and SG-04) short-circuit in the shim — the harness asserts the judge
  is **not** called for them (visible in the p50 ≈ 39 ns row).

## Honesty (WCET-TBD)

The PASS gate is **verdict correctness**. The per-row p50/p99/max are
**indicative host FDIT timing, NOT certified WCET**. **Certified WCET must be
measured on the QNX 8.0 target under FIFO scheduling (#274)** — host numbers are
never presented as WCET.

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

## QNX

`CMakeLists.txt` notes the QNX cross-compile hook as a comment; the real
cross-compile + on-target FDIT/WCET work is **#274**. The SG-0N → real-RTM-ID
mapping is **#272** — see `QNX_MAPPING.md`.
