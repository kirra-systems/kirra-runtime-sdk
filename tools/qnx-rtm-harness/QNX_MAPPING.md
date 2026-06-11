# QNX_MAPPING — RTM-ID mapping & QNX cross-compile notes (#271 → #272 / #274)

## 1. SG-row → real RTM ID — PLACEHOLDER (tracked by #272)

The harness rows are labelled `SG-00 … SG-07`. **These are PLACEHOLDER IDs.**
Mapping them to the kernel's real Requirements Traceability IDs
(`docs/safety/REQUIREMENTS_TRACEABILITY.md`, the `SG-001 … SG-016` namespace) and
emitting the CSV in the column order `docs/safety/RTM_GAP_REPORT.md` expects is
the dedicated follow-up **issue #272** — not done here.

| Harness row | Injects | Verdict | Real RTM ID |
|---|---|---|---|
| SG-00 | valid command | `Ok` | _TBD (#272)_ |
| SG-01 | bad magic | `StaleHeader` | _TBD (#272)_ |
| SG-02 | sequence regress (strictly lower) | `SequenceRegress` | _TBD (#272)_ |
| SG-03 | deadline missed | `DeadlineMissed` | _TBD (#272)_ |
| SG-04 | corrupt payload (CRC) | `PayloadCorrupt` | _TBD (#272)_ |
| SG-05 | oversize payload | `PayloadOversize` (shim short-circuit) | _TBD (#272)_ |
| SG-06 | over-envelope command | `KinematicLimit` | _TBD (#272)_ |
| SG-07 | replay (sequence == last_accepted) | `SequenceRegress` | _TBD (#272)_ |

The harness already emits a machine-readable CSV (`id,fault_class,result,
expected_verdict,observed_verdict,p50_ns,p99_ns,max_ns`); #272 remaps `id` to the
real RTM IDs and reorders to the gap-report's expected columns.

## 2. The concern split (recap)

- **C++ shim = driver** — memory/transport safety: double-read tear detection,
  bounds (oversize short-circuits, never crosses the FFI), CRC over the payload.
- **Rust judge = checker** — the contract verdict (magic → sequence → deadline →
  integrity → kinematic) on the shim's stabilized snapshot.

This is **ADR-0006 Clause 3**'s documented C/C++ integration boundary — no longer
the governor hot path.

## 3. QNX cross-compile (real work = #274)

The host build uses native `g++` + `rustc`. For a QNX 8.0 target:

- **C++**: set a CMake toolchain file pointing at `q++` / `qcc` (the QNX SDP
  compilers); the `-fno-exceptions -fno-rtti` discipline already matches a
  freestanding-friendly build.
- **Rust judge**: build the `no_std` staticlib for the QNX target tuple, e.g.
  `--target x86_64-pc-nto-qnx8_0_0` or `aarch64-unknown-nto-qnx8_0_0`, keeping
  `-C panic=abort`. The judge is already `no_std` + zero-alloc + integer-only, so
  it carries no std/edition-2024 transitive surface of its own (contrast the
  iceoryx2 dependency tree — see EPIC #270 / #274).
- **FDIT/WCET**: re-run the harness on the target under **FIFO scheduling** and
  replace the indicative host p50/p99/max with **target-measured** percentiles —
  the numbers that actually back the FTTI claim. This is **#274**.

See `docs/adr/KIRRA_QNX_CROSSCOMPILE.md` for the existing cross-compile recipe
context.

## 4. WCET-TBD

Host timing in the harness output is **indicative only**. Certified WCET is
**TBD on the QNX target (#274)**; the harness banner states this and the PASS gate
is **verdict correctness**, never timing.
