//! [`WcetChannel`] — the zero-alloc, integer-only per-stage measurement sink.
//!
//! One channel accumulates the timing samples for ONE execution-loop stage
//! (perception input, governor execution, parko evaluation, actuator publication,
//! or total loop). Recording a sample is `O(1)` and allocation-free; the
//! distribution lives in a fixed-size histogram chosen at construction.

/// Integer square root of a `u128` (Newton's method). Pure-integer so the whole
/// crate stays `no_std` with no `libm` / float dependency — stddev (jitter) is
/// computed without ever touching `f64`.
#[must_use]
fn isqrt_u128(n: u128) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    // x ≤ sqrt(u128::MAX) < 2^64, so the cast is lossless.
    x as u64
}

/// An immutable snapshot of a [`WcetChannel`]'s statistics. Non-generic and
/// `Copy` so reports can hold a uniform slice regardless of histogram size.
///
/// All fields are nanoseconds. `min`/`max`/`mean` are exact; `stddev` is the
/// (population) standard deviation computed from exact integer moments; the
/// percentiles are histogram-derived and CONSERVATIVE (rounded up to the bucket
/// upper edge, or to the exact `max` when they fall in the overflow region).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelStats {
    /// Number of samples recorded.
    pub count: u64,
    /// Smallest sample (ns); `0` when `count == 0`.
    pub min_ns: u64,
    /// Largest sample (ns) — exact. The WCET-relevant figure.
    pub max_ns: u64,
    /// Arithmetic mean (ns), integer-truncated.
    pub mean_ns: u64,
    /// Population standard deviation (ns) — the jitter figure.
    pub stddev_ns: u64,
    /// Peak-to-peak spread (`max - min`, ns) — the second jitter figure.
    pub peak_to_peak_ns: u64,
    /// Median (50th percentile), conservative bucket upper edge.
    pub p50_ns: u64,
    /// 99th percentile, conservative bucket upper edge.
    pub p99_ns: u64,
    /// 99.9th percentile, conservative bucket upper edge. The tail figure the
    /// `wcet_gate.rs` precedent gates on.
    pub p999_ns: u64,
}

impl ChannelStats {
    /// The zero snapshot (no samples).
    pub const EMPTY: Self = Self {
        count: 0,
        min_ns: 0,
        max_ns: 0,
        mean_ns: 0,
        stddev_ns: 0,
        peak_to_peak_ns: 0,
        p50_ns: 0,
        p99_ns: 0,
        p999_ns: 0,
    };
}

/// A per-stage WCET measurement sink with `BUCKETS` histogram buckets.
///
/// Exact streaming aggregates (`count`/`min`/`max`/`sum`/`sum_sq`) give exact
/// min/avg/max and jitter with no buffer; the fixed histogram gives bounded-memory
/// percentiles. Everything is `O(1)` per sample and allocation-free, so a channel
/// is safe to `record_nanos` from inside a deterministic loop (when the
/// `instrument` feature is on — see [`crate::wcet_measure`]).
#[derive(Debug, Clone)]
pub struct WcetChannel<const BUCKETS: usize> {
    bucket_width_ns: u64,
    buckets: [u64; BUCKETS],
    /// Samples at or beyond `BUCKETS * bucket_width_ns`.
    overflow: u64,
    count: u64,
    min_ns: u64,
    max_ns: u64,
    sum_ns: u128,
    sum_sq_ns: u128,
}

impl<const BUCKETS: usize> WcetChannel<BUCKETS> {
    /// Create an empty channel whose histogram bucket `i` covers
    /// `[i * bucket_width_ns, (i + 1) * bucket_width_ns)`. `bucket_width_ns` is
    /// clamped to at least `1` so binning is always well-defined.
    #[must_use]
    pub const fn new(bucket_width_ns: u64) -> Self {
        Self {
            bucket_width_ns: if bucket_width_ns == 0 { 1 } else { bucket_width_ns },
            buckets: [0; BUCKETS],
            overflow: 0,
            count: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            sum_ns: 0,
            sum_sq_ns: 0,
        }
    }

    /// Record one elapsed-duration sample (ns). `O(1)`, allocation-free.
    #[inline]
    pub fn record_nanos(&mut self, ns: u64) {
        self.count += 1;
        if ns < self.min_ns {
            self.min_ns = ns;
        }
        if ns > self.max_ns {
            self.max_ns = ns;
        }
        let ns128 = ns as u128;
        self.sum_ns += ns128;
        self.sum_sq_ns += ns128 * ns128;

        let bucket = (ns / self.bucket_width_ns) as usize;
        if bucket < BUCKETS {
            self.buckets[bucket] += 1;
        } else {
            self.overflow += 1;
        }
    }

    /// Number of samples recorded so far.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Reset to empty, keeping the configured bucket width — for a fresh campaign
    /// run without reallocating.
    pub fn reset(&mut self) {
        self.buckets = [0; BUCKETS];
        self.overflow = 0;
        self.count = 0;
        self.min_ns = u64::MAX;
        self.max_ns = 0;
        self.sum_ns = 0;
        self.sum_sq_ns = 0;
    }

    /// Conservative percentile (ns) for `per_mille` ∈ `[0, 1000]`
    /// (e.g. `500` → p50, `990` → p99, `999` → p99.9). Returns the upper edge of
    /// the bucket the percentile falls in (rounds up), or the exact `max` when it
    /// falls in the overflow region — never an optimistic value.
    #[must_use]
    pub fn percentile_nanos(&self, per_mille: u32) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let pm = per_mille.min(1000) as u64;
        // Ceil so e.g. p100 maps to the last sample, not one short.
        let threshold = (self.count * pm).div_ceil(1000);
        let mut cum = 0u64;
        let mut i = 0;
        while i < BUCKETS {
            cum += self.buckets[i];
            if cum >= threshold {
                // Upper edge of bucket i, but never report above the exact max.
                let edge = (i as u64 + 1).saturating_mul(self.bucket_width_ns);
                return edge.min(self.max_ns);
            }
            i += 1;
        }
        // Remaining mass is in the overflow region → the exact max bounds it.
        self.max_ns
    }

    /// Snapshot the current statistics. Off the hot path (compute when reporting).
    #[must_use]
    pub fn snapshot(&self) -> ChannelStats {
        if self.count == 0 {
            return ChannelStats::EMPTY;
        }
        let count128 = self.count as u128;
        let mean = self.sum_ns / count128; // integer-truncated
        // Population variance = E[x^2] - (E[x])^2, computed from exact integer
        // moments; saturating so truncation in `mean` can't underflow it.
        let mean_of_sq = self.sum_sq_ns / count128;
        let variance = mean_of_sq.saturating_sub(mean * mean);
        ChannelStats {
            count: self.count,
            min_ns: self.min_ns,
            max_ns: self.max_ns,
            mean_ns: mean as u64,
            stddev_ns: isqrt_u128(variance),
            peak_to_peak_ns: self.max_ns - self.min_ns,
            p50_ns: self.percentile_nanos(500),
            p99_ns: self.percentile_nanos(990),
            p999_ns: self.percentile_nanos(999),
        }
    }
}
