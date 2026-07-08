//! # kirra-timing — certifiable WCET timing-instrumentation primitives (#274 / L1)
//!
//! A lean, `no_std`, zero-dependency, **zero-alloc** timing library for the
//! QNX-target WCET measurement campaign. It supplies the measurement *types* —
//! per-stage channels with exact min/avg/max + jitter and bounded-memory
//! percentiles ([`WcetChannel`]), an environment tag that encodes the
//! host-vs-WCET rule ([`MeasurementEnv`]), and deterministic CSV/table reports
//! ([`report::Report`]) — plus a [`wcet_measure!`] macro that is **zero overhead
//! when the `instrument` feature is off** (it expands to just the measured block,
//! with no clock read and no record call).
//!
//! ## Design constraints (all load-bearing, mirroring the repo precedents)
//!
//! - **`no_std`, zero deps, `#![forbid(unsafe_code)]`** — like
//!   `kirra-contract-channel`. Integer-only math (a pure-integer `isqrt` for
//!   jitter), so there is no `f64`/`libm` dependency anywhere.
//! - **No allocation on the hot path.** A channel is fixed-size; `record_nanos`
//!   is `O(1)`. Percentile/report computation is done off the hot path.
//! - **The crate never reads a clock.** The caller injects a [`MonotonicClock`];
//!   the QNX target binds the boundary-domain monotonic counter (HVCHAN-001 §5)
//!   at the integration shim. On host/CI use [`StdMonotonicClock`] (`std` feature).
//! - **Host numbers are INDICATIVE, never WCET** (WCET_MEASUREMENT_METHODOLOGY.md).
//!   [`MeasurementEnv::is_certified_wcet`] is the one source of truth and reports
//!   print an INDICATIVE banner unless the environment is the QNX target / FIFO.
//!
//! ## Usage
//!
//! ```
//! use kirra_timing::{WcetChannel, MonotonicClock, wcet_measure};
//!
//! // A test/host clock; on target this is the boundary-domain counter.
//! struct FakeClock(core::cell::Cell<u64>);
//! impl MonotonicClock for FakeClock {
//!     fn now_nanos(&self) -> u64 { self.0.get() }
//! }
//!
//! let clock = FakeClock(core::cell::Cell::new(0));
//! // 256 buckets × 100 ns = 0..25.6 µs histogram; samples beyond go to overflow.
//! #[allow(unused_mut)] // `mut` is used only when the `instrument` feature is on
//! let mut governor: WcetChannel<256> = WcetChannel::new(100);
//!
//! // With `instrument` ON this brackets the block with clock reads and records
//! // the elapsed time; with it OFF the macro expands to just `{ ... }`.
//! let verdict = wcet_measure!(governor, clock, {
//!     clock.0.set(420); // (stand-in for real work advancing the clock)
//!     true
//! });
//! assert!(verdict);
//! let _stats = governor.snapshot(); // consume off the hot path at campaign end
//! ```
//!
//! Scope note: this increment delivers the library and its tests. Wiring it into
//! the `ros2`-gated hot loops and the QNX harness is a separate, reviewed
//! follow-up (it needs CI / target validation, per the methodology).

#![cfg_attr(not(test), no_std)]
// Enforce the minimal-TCB guarantee at the crate root too (not only via the
// Cargo `[lints]` table) — robust even if lint settings change or are bypassed,
// matching `kirra-contract-channel`.
#![forbid(unsafe_code)]

// The crate is `no_std` for the target; the optional `std` feature (host/CI
// clock) needs `std` explicitly linked when not already the test sysroot.
#[cfg(all(feature = "std", not(test)))]
extern crate std;

mod channel;
mod clock;
mod env;
pub mod report;
// WP-22 (G-3): EVT/MBPTA tail-fitting — host-analysis only, needs f64/std. Compiled
// under the `evt` feature (which pulls `std`) OR under `test` (so the workspace suite
// exercises it); the production `no_std`/zero-alloc core is unaffected when off.
#[cfg(any(test, feature = "evt"))]
pub mod evt;

pub use channel::{ChannelStats, WcetChannel};
pub use clock::MonotonicClock;
#[cfg(feature = "std")]
pub use clock::StdMonotonicClock;
pub use env::MeasurementEnv;
pub use report::{Report, StageReport, CSV_HEADER};

/// Canonical labels for the five safety-critical execution-loop stages (#274).
/// Using these constants keeps stage names consistent across crates, the harness,
/// and the report CSV (so downstream tooling can join on them).
pub mod stage {
    /// Perception input ingest (e.g. trajectory/odometry arrival → validation entry).
    pub const PERCEPTION_INPUT: &str = "perception_input";
    /// Governor / checker execution (the verdict path) under the Nominal contract.
    pub const GOVERNOR_EXEC: &str = "governor_exec";
    /// Governor / checker execution under the MRC fallback contract (the
    /// decel-to-stop envelope verdict path) — a distinct label from
    /// [`GOVERNOR_EXEC`] so downstream tooling can join on either profile.
    pub const GOVERNOR_EXEC_MRC: &str = "governor_exec_mrc";
    /// Parko (ML) evaluation tick.
    pub const PARKO_EVAL: &str = "parko_eval";
    /// Actuator command publication.
    pub const ACTUATOR_PUBLISH: &str = "actuator_publish";
    /// End-to-end loop latency (input → govern → publish).
    pub const TOTAL_LOOP: &str = "total_loop";
}

/// Measure `block`, recording its elapsed nanoseconds into `channel` using
/// `clock` — **only when the `instrument` feature is enabled**.
///
/// Two cfg-gated definitions exist; exactly one is compiled:
/// - `instrument` ON — brackets `block` with monotonic clock reads via UFCS
///   (`MonotonicClock::now_nanos(&clock)`, so the trait need not be imported at
///   the call site, and a `&clock` works via the blanket `&T` impl) and records
///   the elapsed time. Evaluates to the block's value.
/// - `instrument` OFF (production default) — expands to **exactly `{ block }`**:
///   `channel` and `clock` are not evaluated or touched at all, so there is no
///   clock read, no record call, no branch. This is the certifiable
///   "instrumentation is campaign-only, never the shipped hot path" property.
///
/// `clock` must implement [`MonotonicClock`]; `channel` must be a mutable
/// [`WcetChannel`].
#[cfg(feature = "instrument")]
#[macro_export]
macro_rules! wcet_measure {
    ($channel:expr, $clock:expr, $block:block) => {{
        let __wcet_start = $crate::MonotonicClock::now_nanos(&$clock);
        let __wcet_result = $block;
        let __wcet_elapsed = $crate::MonotonicClock::elapsed_nanos_since(&$clock, __wcet_start);
        $channel.record_nanos(__wcet_elapsed);
        __wcet_result
    }};
}

/// Disabled form — see the enabled definition's docs. Expands to exactly the
/// measured block; `channel`/`clock` are not referenced.
#[cfg(not(feature = "instrument"))]
#[macro_export]
macro_rules! wcet_measure {
    ($channel:expr, $clock:expr, $block:block) => {{ $block }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct StepClock {
        now: Cell<u64>,
    }
    impl StepClock {
        fn new() -> Self {
            Self { now: Cell::new(0) }
        }
        fn set(&self, v: u64) {
            self.now.set(v);
        }
    }
    impl MonotonicClock for StepClock {
        fn now_nanos(&self) -> u64 {
            self.now.get()
        }
    }

    #[test]
    fn streaming_min_max_mean_are_exact() {
        let mut ch: WcetChannel<1024> = WcetChannel::new(1);
        for v in [10u64, 20, 30, 40, 50] {
            ch.record_nanos(v);
        }
        let s = ch.snapshot();
        assert_eq!(s.count, 5);
        assert_eq!(s.min_ns, 10);
        assert_eq!(s.max_ns, 50);
        assert_eq!(s.mean_ns, 30);
        assert_eq!(s.peak_to_peak_ns, 40);
    }

    #[test]
    fn stddev_matches_float_reference() {
        let mut ch: WcetChannel<4096> = WcetChannel::new(1);
        let data = [2u64, 4, 4, 4, 5, 5, 7, 9]; // textbook: mean 5, pop stddev 2
        for v in data {
            ch.record_nanos(v);
        }
        let s = ch.snapshot();
        assert_eq!(s.mean_ns, 5);
        assert_eq!(s.stddev_ns, 2);
    }

    #[test]
    fn variance_uses_exact_moments_not_truncated() {
        // [10, 11]: true population variance is 0.25 → integer floor 0 → stddev 0.
        // The OLD truncated-moment form (mean=10, mean_of_sq=110) gave variance 10
        // → stddev 3, a gross over-estimate. The exact (n·Σx²−(Σx)²)/n² form must
        // floor to 0 here. Regression guard for the deferred-division fix.
        let mut ch: WcetChannel<64> = WcetChannel::new(1);
        ch.record_nanos(10);
        ch.record_nanos(11);
        let s = ch.snapshot();
        assert_eq!(s.mean_ns, 10); // 21/2 truncated
        assert_eq!(s.stddev_ns, 0, "exact variance floors to 0, not the truncated 3");
    }

    #[test]
    fn percentiles_are_conservative_and_bounded_by_max() {
        // 1000 samples: 990 at 100ns, 10 at 5000ns. p99 ~100..200, p99.9 in tail.
        let mut ch: WcetChannel<256> = WcetChannel::new(100); // 0..25.6µs
        for _ in 0..990 {
            ch.record_nanos(100);
        }
        for _ in 0..10 {
            ch.record_nanos(5000);
        }
        let s = ch.snapshot();
        // p50 well within the 100ns mass; conservative upper edge ≤ max.
        assert!(s.p50_ns >= 100 && s.p50_ns <= s.max_ns);
        // p99 still in the low mass (only 1% is in the tail).
        assert!(s.p99_ns <= 5000);
        // p99.9 reaches into the tail.
        assert!(s.p999_ns >= 200, "p999 was {}", s.p999_ns);
        assert!(s.p999_ns <= s.max_ns);
        assert_eq!(s.max_ns, 5000);
    }

    #[test]
    fn overflow_samples_still_bound_max_and_tail() {
        // Bucket range is only 0..1000ns; a 9000ns sample lands in overflow.
        let mut ch: WcetChannel<10> = WcetChannel::new(100);
        for _ in 0..99 {
            ch.record_nanos(50);
        }
        ch.record_nanos(9000);
        let s = ch.snapshot();
        assert_eq!(s.max_ns, 9000); // exact even though it overflowed the histogram
        // p99.9 (top 0.1% of 100 samples = the one 9000ns) falls in overflow → max.
        assert_eq!(s.p999_ns, 9000);
    }

    #[test]
    fn empty_channel_snapshot_is_zero() {
        let ch: WcetChannel<8> = WcetChannel::new(100);
        assert_eq!(ch.snapshot(), ChannelStats::EMPTY);
        assert_eq!(ch.percentile_nanos(999), 0);
    }

    #[test]
    fn reset_clears_state() {
        let mut ch: WcetChannel<16> = WcetChannel::new(10);
        ch.record_nanos(123);
        assert_eq!(ch.count(), 1);
        ch.reset();
        assert_eq!(ch.count(), 0);
        assert_eq!(ch.snapshot(), ChannelStats::EMPTY);
    }

    #[test]
    fn env_gating_is_the_single_source_of_truth() {
        assert!(MeasurementEnv::QnxTargetFifo.is_certified_wcet());
        assert!(!MeasurementEnv::Host.is_certified_wcet());
        assert!(!MeasurementEnv::CiRunner.is_certified_wcet());
        assert!(!MeasurementEnv::Other.is_certified_wcet());
        assert_eq!(MeasurementEnv::QnxTargetFifo.wcet_status(), "QNX-TARGET-MEASURED");
        assert_eq!(MeasurementEnv::Host.wcet_status(), "INDICATIVE-NOT-WCET");
    }

    #[test]
    fn report_banner_reflects_environment() {
        extern crate std;
        use std::string::String;

        let stages = [StageReport::new(stage::GOVERNOR_EXEC, ChannelStats::EMPTY)];
        let host = Report::new(MeasurementEnv::Host, "host-default", &stages);
        let mut out = String::new();
        core::fmt::write(&mut out, format_args!("{host}")).unwrap();
        assert!(out.contains("INDICATIVE, NOT WCET"));
        assert!(!host.is_wcet_evidence());

        let target = Report::new(MeasurementEnv::QnxTargetFifo, "SCHED_FIFO", &stages);
        let mut out2 = String::new();
        core::fmt::write(&mut out2, format_args!("{target}")).unwrap();
        assert!(out2.contains("CERTIFIED"));
        assert!(target.is_wcet_evidence());
    }

    #[test]
    fn csv_has_stable_columns_and_status() {
        extern crate std;
        use std::string::String;

        let mut ch: WcetChannel<64> = WcetChannel::new(50);
        ch.record_nanos(100);
        let stages = [StageReport::new(stage::TOTAL_LOOP, ch.snapshot())];
        let report = Report::new(MeasurementEnv::CiRunner, "host-default", &stages);
        let mut csv = String::new();
        report.write_csv(&mut csv).unwrap();
        let mut lines = csv.lines();
        // The emitted header must be the canonical CSV_HEADER verbatim — the QNX
        // harness's wcet_measure row joins on exactly these columns, so a drift
        // here would silently break that union. Lock both the literal and the const.
        assert_eq!(
            lines.next().unwrap(),
            "metric,env,sched,n,min_ns,mean_ns,max_ns,stddev_ns,p50_ns,p99_ns,p999_ns,wcet_status"
        );
        assert_eq!(
            report::CSV_HEADER,
            "metric,env,sched,n,min_ns,mean_ns,max_ns,stddev_ns,p50_ns,p99_ns,p999_ns,wcet_status",
            "CSV_HEADER is the canonical schema the QNX harness mirrors — update both if it changes"
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("total_loop,ci-runner,host-default,1,"));
        assert!(row.ends_with(",INDICATIVE-NOT-WCET"));
    }

    #[test]
    fn macro_disabled_is_zero_overhead_passthrough() {
        // Without the `instrument` feature the macro must not touch the channel.
        // `mut` is only exercised on the enabled path, hence the allow.
        let clock = StepClock::new();
        #[allow(unused_mut)]
        let mut ch: WcetChannel<8> = WcetChannel::new(1);
        let out = wcet_measure!(ch, clock, {
            clock.set(999);
            7u32 + 1
        });
        assert_eq!(out, 8);
        #[cfg(not(feature = "instrument"))]
        assert_eq!(ch.count(), 0, "disabled macro must record nothing");
        #[cfg(feature = "instrument")]
        assert_eq!(ch.count(), 1, "enabled macro must record one sample");
    }

    #[cfg(feature = "instrument")]
    #[test]
    fn macro_enabled_records_elapsed() {
        let clock = StepClock::new();
        let mut ch: WcetChannel<1024> = WcetChannel::new(1);
        wcet_measure!(ch, clock, {
            clock.set(250); // elapsed 250ns
        });
        let s = ch.snapshot();
        assert_eq!(s.count, 1);
        assert_eq!(s.max_ns, 250);
    }
}
