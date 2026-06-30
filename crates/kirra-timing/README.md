# kirra-timing

Certifiable WCET timing-instrumentation primitives for the QNX-target measurement
campaign (**#274 / roadmap L1**).

This crate is the reusable *measurement substrate* the WCET campaign builds on:
per-stage channels that capture exact min/avg/max + jitter and bounded-memory
percentiles, an environment tag that encodes the host-vs-WCET rule, deterministic
CSV/table reports, and a `wcet_measure!` macro that is **zero overhead when
disabled**. It is the missing foundation noted in
[`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`](../../docs/safety/WCET_MEASUREMENT_METHODOLOGY.md);
it does not change any safety logic.

## Properties (all load-bearing)

- **`no_std`, zero dependencies, `#![forbid(unsafe_code)]`** — the
  `kirra-contract-channel` minimal-TCB discipline. Integer-only math (a pure
  `isqrt` for jitter); no `f64`, no `libm`, no `serde`, no transport.
- **No allocation on the hot path.** A `WcetChannel<BUCKETS>` is fixed-size;
  `record_nanos` is `O(1)`. Percentiles/reports are computed off the hot path.
- **The crate never reads a clock.** The caller injects a `MonotonicClock`. The
  QNX target binds the **boundary-domain** monotonic counter
  ([HVCHAN-001 §5](../../docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md)) at the
  integration shim; host/CI uses `StdMonotonicClock` (`std` feature).
- **Host numbers are INDICATIVE, never WCET.** `MeasurementEnv::is_certified_wcet()`
  is the single source of truth; only `QnxTargetFifo` qualifies, and `Report`
  prints an INDICATIVE banner / `INDICATIVE-NOT-WCET` status otherwise.

## Features

| Feature | Default | Effect |
|---|---|---|
| `instrument` | off | `wcet_measure!` actually reads the clock and records. **Off → the macro expands to just the measured block** (no clock read, no record, no branch). Instrumentation is campaign-only, never the shipped hot path. |
| `std` | off | Provides `StdMonotonicClock` (an `Instant`-backed clock) for host/CI campaigns. Off keeps the crate `no_std` for the target. |

## Usage

```rust
use kirra_timing::{WcetChannel, MeasurementEnv, Report, StageReport, stage, wcet_measure};

let clock = my_boundary_domain_clock();          // impl MonotonicClock
let mut governor: WcetChannel<256> = WcetChannel::new(100); // 256 × 100 ns = 0..25.6 µs

let verdict = wcet_measure!(governor, clock, {
    run_governor_verdict(/* ... */)              // measured only when `instrument` is on
});

// Off the hot path, at campaign end:
let stages = [StageReport::new(stage::GOVERNOR_EXEC, governor.snapshot())];
let report = Report::new(MeasurementEnv::Host, "host-default", &stages);
println!("{report}");                            // human table + INDICATIVE banner
let mut csv = String::new();
report.write_csv(&mut csv).unwrap();             // machine CSV
```

## Reproducible measurement procedure

1. **Build with instrumentation on** for the campaign target only:
   - host/CI (indicative): `cargo test -p kirra-timing --features instrument,std`
   - QNX target: build the consuming binary with `--features instrument` and bind
     a `MonotonicClock` to the boundary-domain counter under `SCHED_FIFO`
     (target build is out of this crate; see `tools/qnx-rtm-harness/`).
2. **Tag the environment** with the true `MeasurementEnv`. Only `QnxTargetFifo`
   renders as WCET evidence; everything else is INDICATIVE by construction.
3. **Run the worst-case load** (methodology §3): max-size payload, max rate,
   concurrent fault injection; warm-up separated from the measured set.
4. **Export** the CSV (`write_csv`) as the campaign artifact and the table for
   humans. Columns: `metric,env,sched,n,min_ns,mean_ns,max_ns,stddev_ns,p50_ns,p99_ns,p999_ns,wcet_status`.
5. **Gate on the tail** (`p999_ns`), not `max` — the `wcet_gate.rs` precedent:
   `max` is dominated by scheduler/hypervisor jitter on shared hosts.

## Scope / status

This crate is the library + its tests. **Wiring it into the `ros2`-gated hot loops
and the QNX `wcet_measure` harness is a separate, reviewed follow-up** — that step
needs CI / target validation, and per the methodology the production hot path ships
with `instrument` **off**.
