//! Monotonic clock abstraction.
//!
//! The library never reads a clock itself: the caller injects one via
//! [`MonotonicClock`]. On the QNX target this binds the hypervisor boundary-domain
//! monotonic counter (HVCHAN-001 §5) at the integration shim — the contract
//! channel's "the target binds the region" discipline applied to time. On host /
//! CI the [`StdMonotonicClock`] (behind the `std` feature) backs campaigns.

/// A monotonic, non-decreasing nanosecond source used ONLY for differencing
/// (durations) — never as wall-clock or PTP time.
///
/// HVCHAN-001 §5 non-mixing rule: the value returned here is a *boundary-domain*
/// monotonic count; it must not be compared against system/PTP time. The crate
/// only ever subtracts two reads of the SAME clock to form an elapsed duration.
pub trait MonotonicClock {
    /// Monotonic nanoseconds since an arbitrary, fixed epoch. Successive calls
    /// must be non-decreasing on a given clock instance.
    fn now_nanos(&self) -> u64;

    /// Elapsed nanoseconds since `start` (a prior [`MonotonicClock::now_nanos`]).
    /// `saturating_sub` so a non-monotonic read can never produce a wrapped /
    /// negative-as-huge duration — it reads as `0`, which a downstream WCET gate
    /// treats conservatively rather than as a spurious tail sample.
    #[inline]
    fn elapsed_nanos_since(&self, start: u64) -> u64 {
        self.now_nanos().saturating_sub(start)
    }
}

/// An [`Instant`]-backed monotonic clock for host and CI campaigns.
///
/// Indicative-only by construction: pair it with [`crate::MeasurementEnv::Host`]
/// or [`crate::MeasurementEnv::CiRunner`] so reports are never mislabeled as WCET.
///
/// [`Instant`]: std::time::Instant
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy)]
pub struct StdMonotonicClock {
    epoch: std::time::Instant,
}

#[cfg(feature = "std")]
impl StdMonotonicClock {
    /// Anchor the epoch at "now". All `now_nanos` reads are relative to it.
    #[must_use]
    pub fn new() -> Self {
        Self { epoch: std::time::Instant::now() }
    }
}

#[cfg(feature = "std")]
impl Default for StdMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl MonotonicClock for StdMonotonicClock {
    #[inline]
    fn now_nanos(&self) -> u64 {
        // `Instant` is monotonic; `as u64` is safe for any realistic campaign
        // duration (u64 ns ≈ 584 years).
        self.epoch.elapsed().as_nanos() as u64
    }
}
