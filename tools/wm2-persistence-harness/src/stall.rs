//! Characterising the ~29 second write stall (ADR-0041 open question 9).
//!
//! The merged R2 run recorded a single commit taking **29.27 s** under
//! `synchronous=NORMAL` at batch=64, on NVMe. It inverted the durability
//! ordering — `NORMAL` came out slower than `FULL` at the same batch size,
//! which no durability model predicts — and it accounted for ~91 % of that
//! stage's wall time, so the throughput figure for that row is not usable.
//!
//! At 10 Hz a 29 s stall is ~293 observations that cannot be recorded. That
//! matters more than the benchmark it corrupted, so it needs to be understood
//! rather than re-run until it goes away.
//!
//! # Why one sample could not answer it
//!
//! The candidate causes — ext4 journal commit, NVMe garbage collection,
//! thermal or power management — are indistinguishable from a latency number
//! alone. All of them produce "one commit took a long time". Telling them apart
//! needs two things the original run did not have:
//!
//! 1. **Repetition**, to establish whether the stall is reproducible and at
//!    what rate. A one-in-fifty event and a one-in-two event are different
//!    engineering problems even at identical magnitude.
//! 2. **Concurrent system state**, sampled across the stall. A stall with the
//!    block layer busy is not the same finding as a stall with the block layer
//!    idle and the CPU thermally capped.
//!
//! # Attribution is deliberately reluctant
//!
//! [`attribute_stall`] returns `Unattributed` unless the evidence is clear, and
//! that is the common case by design. A confident wrong attribution is worse
//! than an honest shrug: it closes the investigation. The signals here narrow
//! the field; they do not close it, and the drill says so.

use std::path::Path;

/// A stall worth investigating. Chosen well above the p99 of every healthy
/// configuration measured on target (`OFF`/64 max was 3.68 ms, `FULL`/64 max
/// 8.75 ms), so this cannot fire on ordinary tail latency.
pub const STALL_THRESHOLD_MS: f64 = 1_000.0;

/// System counters sampled around a commit.
///
/// All fields are `Option` because every one of them is absent on some
/// kernel, filesystem or container configuration, and a missing counter must
/// read as "not observed" rather than zero — zero is a measurement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemSample {
    /// `/proc/pressure/io` total stall microseconds (PSI). Monotonic.
    pub psi_io_total_us: Option<u64>,
    /// `/proc/diskstats` **field 13** summed: milliseconds spent doing I/O.
    ///
    /// Field 13 in the kernel's 1-based numbering (`f[12]` zero-indexed after
    /// major/minor/name) is *time spent doing I/Os*. Naming it correctly
    /// matters: an operator reading `IO-DEVICE` needs to know the attribution
    /// rests on device busy-time, not on a queue depth or a byte count.
    pub disk_io_ms: Option<u64>,
    /// `/proc/meminfo` `Dirty:` in kB.
    pub dirty_kb: Option<u64>,
    /// `/proc/meminfo` `Writeback:` in kB.
    pub writeback_kb: Option<u64>,
    /// Hottest `/sys/class/thermal/thermal_zone*/temp`, in milli-degrees C.
    pub thermal_max_mc: Option<u64>,
}

/// The difference across a commit, which is what carries information —
/// the absolute values of monotonic counters do not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemDelta {
    pub psi_io_stall_us: Option<u64>,
    pub disk_io_ms: Option<u64>,
    /// Peak dirty+writeback observed during the commit, in kB.
    pub peak_dirty_writeback_kb: Option<u64>,
    pub thermal_max_mc: Option<u64>,
}

/// A [`SystemDelta`] **together with the window it was measured over**.
///
/// # Why the window is not optional
///
/// Attribution asks "was the block layer busy for most of *the stall*". That
/// question only has an answer if the counters were accumulated across the
/// stall. The first version of this module sampled once before the whole append
/// run and once after, then compared that whole-run `disk_io_ms` against a
/// single commit's duration — a **denominator mismatch**. A 60 s run with 5 s of
/// ordinary device busy-time satisfies `5000 >= 1200 * 0.5` for a 1.2 s stall,
/// so it reported `IO-DEVICE` even when the device was idle for the entire
/// stall. That is how D-10 got "one `IO-DEVICE` attribution held loosely".
///
/// Carrying the window in the type makes the mistake hard to repeat: nothing
/// can be attributed without stating what span the counters cover, and
/// [`attribute_stall`] refuses outright when the span is materially wider than
/// the stall it claims to describe.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowedDelta {
    pub delta: SystemDelta,
    /// The stall being explained, milliseconds.
    pub stall_ms: f64,
    /// The span the counters actually cover, milliseconds. Ideally ≈ `stall_ms`;
    /// it is bounded by the sampler's cadence, so it is never exactly equal.
    pub covered_ms: f64,
    /// How many samples fell in the window. Below [`MIN_WINDOW_SAMPLES`] the
    /// window is not resolved well enough to attribute anything.
    pub samples: usize,
    /// Device busy-ms per ms over the rest of the run — the same measurement,
    /// on the same device, outside this stall. `None` when the series cannot
    /// supply one.
    ///
    /// This is the control the absolute threshold never had. See
    /// [`IO_BUSY_BASELINE_MULTIPLE`].
    pub baseline_busy_rate: Option<f64>,
}

impl WindowedDelta {
    /// Is this delta actually about the stall it names?
    ///
    /// Bounded on BOTH sides. Too wide and the counters describe more than the
    /// stall (the original defect). Too narrow and they describe only part of
    /// it, which is just as wrong and less obvious — a window covering 17 % of a
    /// commit is not "a slightly conservative measurement", it is a different
    /// measurement. [`delta_over_window`] already guarantees containment by
    /// construction; this repeats it so a hand-built `WindowedDelta` cannot
    /// claim to be usable without it.
    /// Device busy-ms per ms of window. Not a probability: `/proc/diskstats`
    /// field 13 accumulates per-I/O service time, so this has no fixed ceiling
    /// and only means something next to another measurement of the same thing.
    pub fn busy_rate(&self) -> Option<f64> {
        if self.covered_ms <= 0.0 {
            return None;
        }
        self.delta.disk_io_ms.map(|ms| ms as f64 / self.covered_ms)
    }

    /// Was the device at least as busy during the stall as it is when nothing
    /// is wrong? `None` when there is no baseline to compare against.
    pub fn busy_against_baseline(&self) -> Option<bool> {
        match (self.busy_rate(), self.baseline_busy_rate) {
            (Some(r), Some(b)) if b > 0.0 => Some(r >= b * IO_BUSY_BASELINE_MULTIPLE),
            _ => None,
        }
    }

    pub fn window_is_usable(&self) -> bool {
        self.samples >= MIN_WINDOW_SAMPLES
            && self.covered_ms > 0.0
            && self.covered_ms >= self.stall_ms
            && self.covered_ms <= self.stall_ms * MAX_WINDOW_OVERSHOOT
    }
}

/// How much wider than the stall the sampled window may be before the counters
/// stop being about the stall.
///
/// Not 1.0: the sampler ticks every [`SAMPLE_INTERVAL_MS`], so the bracketing
/// samples always sit slightly outside the commit. 1.5× leaves room for that at
/// the [`STALL_THRESHOLD_MS`] floor (where the overshoot is at most ~4 %) while
/// still rejecting a whole-run window by orders of magnitude.
pub const MAX_WINDOW_OVERSHOOT: f64 = 1.5;

/// How busy the device must be **relative to the same run's ordinary rate** for
/// device work to explain a stall.
///
/// `1.0` — at least as busy as when nothing is wrong. The reasoning is what
/// `IO-DEVICE` claims: that the block layer was doing the work the stall was
/// waiting on. A device genuinely stalling on garbage collection or an SLC
/// cliff is doing *more* work than usual, not less, and field 13 accumulates
/// service time so slower I/Os push it up rather than down.
///
/// # Why a ratio and not a bigger constant
///
/// [`IO_BUSY_FRACTION`] compares device busy-time against the stall's own
/// length, and that comparison does not transfer across window lengths. Measured
/// on target: the same drive read **2.1 %** and **1.05 %** across 30 s stalls but
/// **34.3 %** across a 5 s one, while ordinary non-stalling windows in those runs
/// read **74–100 %**. A fixed cut calibrated on the 30 s stalls sits far from the
/// 5 s one for no reason to do with the device.
///
/// A ratio against the run's own baseline is self-calibrating and dimensionally
/// honest — the same odd metric on both sides, same device, same run. Against it
/// the three measured stalls sit at ratios of roughly **0.02, 0.01 and 0.40**,
/// so the margin is wide rather than the coin-flip the absolute test had become.
///
/// This is applied **in addition to** [`IO_BUSY_FRACTION`], not instead of it:
/// one short-window measurement cannot recalibrate an absolute constant, and
/// the known failure mode is a FALSE `IO-DEVICE`, so the conservative direction
/// is the correct one to move in.
pub const IO_BUSY_BASELINE_MULTIPLE: f64 = 1.0;

/// Fewer samples than this and the window is a guess, not a measurement.
pub const MIN_WINDOW_SAMPLES: usize = 3;

/// How much of a run must lie outside the stall before the remainder can be
/// called "ordinary". Below this the control is too short to mean anything.
pub const MIN_BASELINE_MS: f64 = 1_000.0;

/// Background sampler cadence. At [`STALL_THRESHOLD_MS`] this yields ~50
/// samples across the shortest event that counts as a stall.
pub const SAMPLE_INTERVAL_MS: u64 = 20;

/// One sample, stamped against the same origin the append loop measures from.
#[derive(Debug, Clone, PartialEq)]
pub struct StampedSample {
    /// Milliseconds from the start of the append loop.
    pub at_ms: f64,
    pub sample: SystemSample,
}

/// What the evidence supports. Deliberately coarse: these are *directions to
/// look*, not root causes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallAttribution {
    /// The block layer was busy for most of the stall — device-side. On NVMe
    /// that points at garbage collection or an SLC-cache exhaustion cliff.
    IoDevice(String),
    /// A large dirty/writeback backlog was being flushed — the page cache and
    /// filesystem journal, not the device.
    WritebackBacklog(String),
    /// The CPU was thermally capped across the stall.
    Thermal(String),
    /// The signals do not discriminate. The expected outcome unless one cause
    /// is clear, and not a failure of the run.
    Unattributed(String),
    /// No stall occurred, so there is nothing to attribute.
    NoStall(String),
}

impl StallAttribution {
    pub fn token(&self) -> &'static str {
        match self {
            Self::IoDevice(_) => "IO-DEVICE",
            Self::WritebackBacklog(_) => "WRITEBACK-BACKLOG",
            Self::Thermal(_) => "THERMAL",
            Self::Unattributed(_) => "UNATTRIBUTED",
            Self::NoStall(_) => "NO-STALL",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::IoDevice(d)
            | Self::WritebackBacklog(d)
            | Self::Thermal(d)
            | Self::Unattributed(d)
            | Self::NoStall(d) => d,
        }
    }
}

/// Fraction of the stall the block layer must be busy for to call it device-side.
const IO_BUSY_FRACTION: f64 = 0.5;
/// PSI I/O stall must cover at least this fraction of the wall time.
const PSI_FRACTION: f64 = 0.5;
/// Dirty + writeback above this during the stall points at a flush backlog.
const WRITEBACK_BACKLOG_KB: u64 = 256 * 1024;
/// Sustained temperature (milli-degrees C) suggesting thermal capping.
const THERMAL_HOT_MC: u64 = 85_000;

/// Device busy-ms per ms over the parts of `series` OUTSIDE `[start_ms, end_ms]`
/// — the run's ordinary rate, measured on the same device in the same run.
///
/// Computed as the total busy-time and elapsed time of the series minus the
/// stall window's share, so it needs no second pass and cannot disagree with the
/// window it is the control for. Returns `None` when the series has no usable
/// counter, or when the stall covers essentially all of it — a "baseline" drawn
/// from the stall itself would be a control that isn't one.
pub fn baseline_busy_rate_excluding(
    series: &[StampedSample],
    start_ms: f64,
    end_ms: f64,
) -> Option<f64> {
    if series.len() < 2 {
        return None;
    }
    let (first, last) = (series.first()?, series.last()?);
    let total_ms = last.at_ms - first.at_ms;
    let total_busy = last
        .sample
        .disk_io_ms?
        .checked_sub(first.sample.disk_io_ms?)? as f64;

    let window = delta_over_window(series, start_ms, end_ms);
    let (win_ms, win_busy) = match &window {
        Some(w) => (w.covered_ms, w.delta.disk_io_ms.unwrap_or(0) as f64),
        None => (0.0, 0.0),
    };

    let rest_ms = total_ms - win_ms;
    // Guard the denominator AND demand enough of the run to be outside the
    // stall for the remainder to describe "ordinary" at all.
    if rest_ms < MIN_BASELINE_MS {
        return None;
    }
    Some(((total_busy - win_busy).max(0.0)) / rest_ms)
}

/// The stalling repetitions' durations, ascending, from every repetition's worst
/// commit.
///
/// A free function rather than three lines inline, so a test can exercise **this
/// code** instead of a copy of it. Both halves matter and neither is obvious
/// from the output alone: the sort is what makes a cluster distinguishable from
/// a spread at a glance, and the filter is what ties the array's length to
/// `stalls_observed`.
pub fn stall_durations_from(worst_per_run: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = worst_per_run
        .iter()
        .copied()
        .filter(|ms| *ms >= STALL_THRESHOLD_MS)
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Extract the counter delta across one commit's window from a sampled series.
///
/// This is the OQ9 instrument fix. `series` is the background sampler's output,
/// oldest first; `start_ms`/`end_ms` bound the commit being explained. The
/// bracketing samples are the last one at or before `start_ms` and the first one
/// at or after `end_ms`, so the window always *contains* the commit rather than
/// clipping it.
///
/// Returns `None` when the series cannot resolve the window at all — no
/// bracketing pair. That is a fail-closed answer: a caller with `None` has
/// nothing to attribute, which is correct, whereas a caller handed a
/// whole-series delta would attribute confidently and wrongly.
///
/// `peak_dirty_writeback_kb` is a peak *within the window*, not across the run,
/// for the same reason.
pub fn delta_over_window(
    series: &[StampedSample],
    start_ms: f64,
    end_ms: f64,
) -> Option<WindowedDelta> {
    // Non-finite bounds are rejected explicitly rather than left to comparison:
    // a NaN bound makes every `<=` false, which would silently select index 0
    // for both ends and produce a zero-width "measurement".
    if series.len() < 2 || !start_ms.is_finite() || !end_ms.is_finite() || end_ms < start_ms {
        return None;
    }
    // Both brackets must EXIST. Falling back to the first/last sample when one
    // is missing produces a window that does not contain the commit — e.g. a
    // series ending at 1 000 ms against a commit spanning 900-1 500 ms yields a
    // 100 ms window for a 600 ms stall, covering 17 % of it while looking like a
    // measurement. That is the same defect as the whole-run delta with the
    // inequality pointing the other way, so it fails closed too.
    let lo = series.iter().rposition(|s| s.at_ms <= start_ms)?;
    let hi = series.iter().position(|s| s.at_ms >= end_ms)?;
    if hi <= lo {
        return None;
    }
    let (a, b) = (&series[lo], &series[hi]);
    let peak = series[lo..=hi]
        .iter()
        .filter_map(|s| match (s.sample.dirty_kb, s.sample.writeback_kb) {
            (Some(d), Some(w)) => Some(d + w),
            _ => None,
        })
        .max();
    Some(WindowedDelta {
        delta: delta(&a.sample, &b.sample, peak),
        stall_ms: end_ms - start_ms,
        covered_ms: b.at_ms - a.at_ms,
        samples: hi - lo + 1,
        // Filled by `windowed_delta_with_baseline`; left None here so this stays
        // a pure window extraction and the baseline has exactly one origin.
        baseline_busy_rate: None,
    })
}

/// [`delta_over_window`] with the run's ordinary busy rate attached.
///
/// The pairing is the point: a busy figure without its control is the thing
/// that made `IO-DEVICE` unfalsifiable, so the two are produced together rather
/// than left for a caller to remember to combine.
pub fn windowed_delta_with_baseline(
    series: &[StampedSample],
    start_ms: f64,
    end_ms: f64,
) -> Option<WindowedDelta> {
    let mut w = delta_over_window(series, start_ms, end_ms)?;
    w.baseline_busy_rate = baseline_busy_rate_excluding(series, start_ms, end_ms);
    Some(w)
}

/// Attribute one stall from the counters measured **across it**.
///
/// The ordering matters. Device-side I/O is checked before writeback because a
/// writeback flush *also* drives the block layer busy, so "disk busy" alone
/// cannot distinguish them — the discriminator is whether a large dirty backlog
/// was present. Thermal is checked last and only in the absence of I/O
/// evidence, because a hot SoC during heavy I/O is a consequence, not a cause.
///
/// Before any of that, the window is checked. Counters covering materially more
/// than the stall cannot answer "was the device busy *during the stall*", and
/// this returns `Unattributed` rather than the answer they superficially
/// support. See [`WindowedDelta`] for the defect this closes.
pub fn attribute_stall(w: &WindowedDelta) -> StallAttribution {
    let stall_ms = w.stall_ms;
    let d = &w.delta;
    if stall_ms < STALL_THRESHOLD_MS {
        return StallAttribution::NoStall(format!(
            "worst commit {stall_ms:.1} ms is below the {STALL_THRESHOLD_MS:.0} ms threshold"
        ));
    }

    if !w.window_is_usable() {
        return StallAttribution::Unattributed(format!(
            "a {stall_ms:.0} ms stall whose counters cover {:.0} ms across {} sample(s) — the \
             window does not describe the stall, so nothing may be attributed from it \
             (need >= {MIN_WINDOW_SAMPLES} samples and <= {MAX_WINDOW_OVERSHOOT:.1}x the stall). \
             This is an INSTRUMENT limitation, not a finding about the device: reading \
             whole-run counters against a one-commit duration is the defect that produced \
             D-10's loosely-held IO-DEVICE attribution.",
            w.covered_ms, w.samples
        ));
    }

    let backlog = d
        .peak_dirty_writeback_kb
        .is_some_and(|kb| kb >= WRITEBACK_BACKLOG_KB);

    let io_busy = d
        .disk_io_ms
        .map(|ms| ms as f64 >= stall_ms * IO_BUSY_FRACTION);
    let psi_busy = d
        .psi_io_stall_us
        .map(|us| us as f64 / 1e3 >= stall_ms * PSI_FRACTION);
    let io_evidence_absolute = io_busy.unwrap_or(false) || psi_busy.unwrap_or(false);

    // The absolute test alone cleared `IO-DEVICE` for stalls where the device
    // was demonstrably quieter than usual, because its cut point does not
    // transfer across window lengths. Requiring BOTH keeps every refusal the
    // absolute test already made and adds the one it could not: was the device
    // actually busier than this run's own normal?
    let io_evidence = match (io_evidence_absolute, w.busy_against_baseline()) {
        (false, _) => false,
        (true, Some(vs_baseline)) => vs_baseline,
        // No baseline to compare against. The absolute test stands alone, which
        // is where it is weakest — so the detail string says so rather than
        // presenting the verdict as fully supported.
        (true, None) => true,
    };

    if io_evidence && !backlog {
        return StallAttribution::IoDevice(format!(
            "block layer busy for most of a {stall_ms:.0} ms stall with no large dirty \
             backlog (disk_io_ms={:?}, psi_io_stall_us={:?}, peak_dirty_writeback_kb={:?}); \
             busy rate {} vs this run's baseline {} ({}). \
             Device-side: on NVMe this points at garbage collection or an SLC-cache cliff. \
             Confirm with the drive's own SMART/health counters — this harness cannot read \
             them.",
            d.disk_io_ms,
            d.psi_io_stall_us,
            d.peak_dirty_writeback_kb,
            w.busy_rate().map_or("?".to_string(), |r| format!("{r:.3}")),
            w.baseline_busy_rate
                .map_or("unavailable".to_string(), |b| format!("{b:.3}")),
            match w.busy_against_baseline() {
                Some(true) => "at or above baseline",
                Some(false) => "BELOW baseline — should not have been attributed",
                None => "NO BASELINE: absolute test only, the weaker evidence",
            }
        ));
    }

    if io_evidence && backlog {
        return StallAttribution::WritebackBacklog(format!(
            "a {stall_ms:.0} ms stall with the block layer busy AND a large dirty/writeback \
             backlog (peak {} kB). The flush is the work, so this is page cache and \
             filesystem journal rather than the device. Re-check with a smaller \
             dirty_background_ratio before concluding.",
            d.peak_dirty_writeback_kb.unwrap_or(0)
        ));
    }

    if !io_evidence && d.thermal_max_mc.is_some_and(|t| t >= THERMAL_HOT_MC) {
        return StallAttribution::Thermal(format!(
            "a {stall_ms:.0} ms stall with the block layer NOT busy and the hottest zone at \
             {:.1} C. Thermal capping is the leading candidate; confirm against the Jetson's \
             power mode and tegrastats, which this harness does not read.",
            d.thermal_max_mc.unwrap_or(0) as f64 / 1000.0
        ));
    }

    StallAttribution::Unattributed(format!(
        "a {stall_ms:.0} ms stall whose concurrent counters do not discriminate \
         (disk_io_ms={:?}, psi_io_stall_us={:?}, peak_dirty_writeback_kb={:?}, \
         thermal_max_mc={:?}). This is the expected outcome unless one cause is clear, and \
         it is not a failed measurement — a confident wrong attribution would close the \
         investigation, which is worse.",
        d.disk_io_ms, d.psi_io_stall_us, d.peak_dirty_writeback_kb, d.thermal_max_mc
    ))
}

/// Summary of a repeated-configuration stall hunt.
pub struct StallResult {
    pub repeats: usize,
    pub completed: usize,
    pub stalls_observed: usize,
    /// Worst single-commit latency across every repetition, milliseconds.
    pub worst_commit_ms: f64,
    /// Median of the per-run worst commit — how bad a *typical* run gets.
    pub median_worst_ms: f64,
    /// Every stalling repetition's worst commit, milliseconds, ascending.
    ///
    /// **One entry per stalling repetition, not per stall** — `bench::append`
    /// reports a repetition's `max`, so a repetition containing two stalls
    /// contributes only its worse one. The length therefore equals
    /// `stalls_observed` by construction.
    ///
    /// Exists because a single `worst_commit_ms` cannot distinguish stalls
    /// *clustered* at a timeout bound from stalls *distributed* below it, and
    /// those imply different mechanisms: clustering says the bound is doing the
    /// recovering, spread says the fault often resolves on its own (D-15
    /// *Refinement*).
    pub stall_durations_ms: Vec<f64>,
    pub throughput_eps: Vec<f64>,
    pub attribution: StallAttribution,
    /// Counters across the worst commit's own window — **not** across the run.
    /// `covered_ms` and `samples` are emitted alongside so a reader can see how
    /// well the window was resolved rather than trusting it.
    pub delta_at_worst: WindowedDelta,
}

impl StallResult {
    /// Median throughput across repetitions.
    ///
    /// This is the figure the original benchmark could not produce. A single
    /// run reports one number, and one 29 s stall inside it dragged the
    /// `NORMAL`/batch=64 row down to 3 123 ev/s — below `NORMAL`/batch=1,
    /// inverting the durability ordering and making the row unusable. The
    /// median across repetitions is robust to exactly that: one pathological
    /// run cannot move it, so the throughput and the stall rate can be read as
    /// two separate facts instead of one contaminated one.
    pub fn median_throughput_eps(&self) -> f64 {
        if self.throughput_eps.is_empty() {
            return f64::NAN;
        }
        let mut v = self.throughput_eps.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    }

    /// Stalls per run, as a fraction.
    pub fn stall_rate(&self) -> f64 {
        if self.completed == 0 {
            f64::NAN
        } else {
            self.stalls_observed as f64 / self.completed as f64
        }
    }

    /// A run that completed no repetitions measured nothing.
    ///
    /// Note what does NOT fail: observing zero stalls. That is a real and
    /// useful result — it bounds the rate — and treating it as a failure would
    /// pressure the operator toward re-running until a stall appears, which is
    /// how a rate estimate becomes fiction.
    pub fn fails_the_run(&self) -> bool {
        self.completed == 0
    }
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

fn read_u64_after(path: &str, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix(key) {
            return rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
    }
    None
}

/// `/proc/pressure/io` — the `full` line's `total=` field.
///
/// `full` rather than `some`: it counts time when *every* runnable task was
/// stalled on I/O, which is the condition that halts a single-threaded writer.
fn read_psi_io_total_us() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/pressure/io").ok()?;
    for line in text.lines() {
        if line.starts_with("full") {
            for field in line.split_whitespace() {
                if let Some(v) = field.strip_prefix("total=") {
                    return v.parse().ok();
                }
            }
        }
    }
    None
}

/// Summed **field 13** of `/proc/diskstats` — milliseconds spent doing I/O.
///
/// 1-based kernel numbering: fields 1–3 are major, minor and device name, so
/// field 13 is `f[12]` here. It is the cumulative time the device had I/O in
/// flight, which is what "the block layer was busy" means in
/// [`attribute_stall`].
///
/// Partitions are skipped (they double-count their parent device) by taking
/// only entries whose name has no trailing digit-after-letter partition suffix
/// pattern; imperfect, but it errs toward under-counting, and an under-count
/// makes `IoDevice` *harder* to conclude rather than easier.
fn read_disk_io_ms() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/diskstats").ok()?;
    let mut total = 0u64;
    let mut any = false;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 13 {
            continue;
        }
        let name = f[2];
        // Skip loop/ram devices and obvious partitions.
        if name.starts_with("loop") || name.starts_with("ram") {
            continue;
        }
        let is_partition = name.chars().last().is_some_and(|c| c.is_ascii_digit())
            && (name.starts_with("sd") || name.contains('p'));
        if is_partition {
            continue;
        }
        if let Ok(v) = f[12].parse::<u64>() {
            total += v;
            any = true;
        }
    }
    any.then_some(total)
}

fn read_thermal_max_mc() -> Option<u64> {
    let dir = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut max: Option<u64> = None;
    for e in dir.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(e.path().join("temp")) {
            if let Ok(v) = t.trim().parse::<u64>() {
                max = Some(max.map_or(v, |m: u64| m.max(v)));
            }
        }
    }
    max
}

pub fn sample_system() -> SystemSample {
    SystemSample {
        psi_io_total_us: read_psi_io_total_us(),
        disk_io_ms: read_disk_io_ms(),
        dirty_kb: read_u64_after("/proc/meminfo", "Dirty:"),
        writeback_kb: read_u64_after("/proc/meminfo", "Writeback:"),
        thermal_max_mc: read_thermal_max_mc(),
    }
}

/// Difference two samples, with a peak dirty+writeback carried through.
///
/// Monotonic counters subtract; a counter that went *backwards* (a reset, or a
/// device that disappeared) yields `None` rather than a wrapped huge number.
pub fn delta(before: &SystemSample, after: &SystemSample, peak_dw_kb: Option<u64>) -> SystemDelta {
    fn sub(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (Some(x), Some(y)) if y >= x => Some(y - x),
            _ => None,
        }
    }
    SystemDelta {
        psi_io_stall_us: sub(before.psi_io_total_us, after.psi_io_total_us),
        disk_io_ms: sub(before.disk_io_ms, after.disk_io_ms),
        peak_dirty_writeback_kb: peak_dw_kb,
        thermal_max_mc: after.thermal_max_mc.max(before.thermal_max_mc),
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Repeat one append configuration and hunt for the stall.
#[allow(clippy::too_many_arguments)]
pub fn run(
    path: &Path,
    durability: crate::standin::Durability,
    events: u64,
    entities: u64,
    batch: usize,
    seed: u64,
    repeats: usize,
) -> StallResult {
    let mut worst_per_run: Vec<f64> = Vec::new();
    let mut throughput: Vec<f64> = Vec::new();
    let mut stalls = 0usize;
    let mut worst_overall = 0.0f64;
    let mut worst_windowed = WindowedDelta {
        delta: SystemDelta::default(),
        stall_ms: 0.0,
        covered_ms: 0.0,
        samples: 0,
        baseline_busy_rate: None,
    };

    for i in 0..repeats {
        // ONE origin, shared by the sampler and the commit windows. Two
        // `Instant::now()` calls would be two coordinate systems: `append` does
        // event generation and a hash calibration before its loop, so the gap
        // between them is seconds, and a window looked up across that gap lands
        // on an unrelated span of the series rather than a slightly shifted one.
        // That would leave the counters as wrong as the whole-run delta this
        // module exists to replace, while looking correct.
        let origin = std::time::Instant::now();
        let series = std::sync::Arc::new(std::sync::Mutex::new(Vec::<StampedSample>::new()));
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sampler = {
            use std::sync::atomic::Ordering;
            let series = std::sync::Arc::clone(&series);
            let running = std::sync::Arc::clone(&running);
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    let at_ms = origin.elapsed().as_secs_f64() * 1e3;
                    let sample = sample_system();
                    if let Ok(mut v) = series.lock() {
                        v.push(StampedSample { at_ms, sample });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(SAMPLE_INTERVAL_MS));
                }
            })
        };

        let r = crate::bench::append_from(
            path,
            durability,
            events,
            entities,
            batch,
            seed + i as u64,
            Some(origin),
        );

        running.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = sampler.join();
        let samples = series.lock().map(|v| v.clone()).unwrap_or_default();

        let Ok(r) = r else { continue };
        let max_ms = r.timing.max_us / 1e3;
        worst_per_run.push(max_ms);
        throughput.push(r.events_per_second);
        if max_ms >= STALL_THRESHOLD_MS {
            stalls += 1;
        }
        if max_ms > worst_overall {
            worst_overall = max_ms;
            // Windowed on the slowest commit only. If the window cannot be
            // resolved, carry a delta that declares that — never a whole-run
            // one, which would attribute confidently from the wrong span.
            let slowest = r.slowest_commit;
            worst_windowed = slowest
                .and_then(|w| windowed_delta_with_baseline(&samples, w.start_ms, w.end_ms))
                .unwrap_or(WindowedDelta {
                    delta: SystemDelta::default(),
                    // The commit's own duration, taken from the window bench
                    // measured, so the refusal message quotes the same number
                    // the timing does.
                    stall_ms: slowest.map_or(max_ms, |w| w.duration_ms()),
                    covered_ms: 0.0,
                    samples: 0,
                    baseline_busy_rate: None,
                });
        }
    }

    worst_per_run.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_worst = if worst_per_run.is_empty() {
        f64::NAN
    } else {
        worst_per_run[worst_per_run.len() / 2]
    };
    let stall_durations_ms = stall_durations_from(&worst_per_run);
    // The array and the count are two views of the same thing; a refactor that
    // let them disagree would read as a measurement gap rather than a
    // bookkeeping bug, which is the expensive kind of confusion.
    debug_assert_eq!(
        stall_durations_ms.len(),
        stalls,
        "stall_durations_ms must have one entry per stalling repetition"
    );

    let attribution = if worst_per_run.is_empty() {
        StallAttribution::Unattributed(
            "no repetition completed, so nothing was measured — raise --repeats or check the \
             database path"
                .into(),
        )
    } else {
        attribute_stall(&worst_windowed)
    };

    StallResult {
        repeats,
        completed: worst_per_run.len(),
        stalls_observed: stalls,
        worst_commit_ms: worst_overall,
        median_worst_ms: median_worst,
        stall_durations_ms,
        throughput_eps: throughput,
        attribution,
        delta_at_worst: worst_windowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A delta over a window that IS the stall — the good case. Tests about
    /// attribution logic should not have to think about the window; tests about
    /// the window say so explicitly by building a `WindowedDelta` directly.
    fn win(stall_ms: f64, d: SystemDelta) -> WindowedDelta {
        WindowedDelta {
            delta: d,
            stall_ms,
            covered_ms: stall_ms * 1.02,
            samples: (stall_ms / SAMPLE_INTERVAL_MS as f64) as usize + 2,
            // A quiet baseline, so these cases exercise the ABSOLUTE arm: any
            // real device activity clears it and the relative arm stays out of
            // the way. Tests about the baseline set it explicitly.
            baseline_busy_rate: Some(0.001),
        }
    }

    fn d(psi: Option<u64>, disk: Option<u64>, dw: Option<u64>, temp: Option<u64>) -> SystemDelta {
        SystemDelta {
            psi_io_stall_us: psi,
            disk_io_ms: disk,
            peak_dirty_writeback_kb: dw,
            thermal_max_mc: temp,
        }
    }

    #[test]
    fn below_threshold_is_no_stall() {
        let a = attribute_stall(&win(8.75, SystemDelta::default()));
        assert_eq!(a.token(), "NO-STALL");
    }

    #[test]
    fn the_measured_29_second_event_is_over_the_threshold() {
        // The event this module exists for: 29 271.78 ms.
        let a = attribute_stall(&win(29_271.78, SystemDelta::default()));
        assert_ne!(a.token(), "NO-STALL");
    }

    #[test]
    fn no_counters_at_all_is_unattributed_not_a_guess() {
        let a = attribute_stall(&win(29_271.78, SystemDelta::default()));
        assert_eq!(a.token(), "UNATTRIBUTED", "{}", a.detail());
    }

    #[test]
    fn busy_block_layer_without_a_backlog_points_at_the_device() {
        // 20 s of disk-busy inside a 29 s stall, small dirty set.
        let a = attribute_stall(&win(29_000.0, d(None, Some(20_000), Some(1_024), None)));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
        assert!(a.detail().contains("SMART"), "must say what it cannot read");
    }

    #[test]
    fn busy_block_layer_with_a_backlog_points_at_writeback_instead() {
        // Same disk-busy evidence, but a 1 GB dirty backlog explains it.
        let a = attribute_stall(&win(
            29_000.0,
            d(None, Some(20_000), Some(1_024 * 1_024), None),
        ));
        assert_eq!(a.token(), "WRITEBACK-BACKLOG", "{}", a.detail());
    }

    #[test]
    fn psi_alone_is_enough_io_evidence() {
        let a = attribute_stall(&win(29_000.0, d(Some(20_000_000), None, Some(1_024), None)));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
    }

    #[test]
    fn heat_with_an_idle_block_layer_points_at_thermal() {
        let a = attribute_stall(&win(
            29_000.0,
            d(Some(10), Some(5), Some(1_024), Some(96_000)),
        ));
        assert_eq!(a.token(), "THERMAL", "{}", a.detail());
    }

    #[test]
    fn heat_during_heavy_io_is_not_blamed_on_thermal() {
        // A hot SoC during sustained I/O is a consequence, not a cause.
        // Blaming thermal here would send the investigation the wrong way.
        let a = attribute_stall(&win(
            29_000.0,
            d(None, Some(25_000), Some(1_024), Some(96_000)),
        ));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
    }

    #[test]
    fn a_briefly_busy_disk_does_not_explain_a_long_stall() {
        // 100 ms of disk activity inside a 29 s stall explains nothing.
        let a = attribute_stall(&win(29_000.0, d(None, Some(100), Some(1_024), None)));
        assert_eq!(a.token(), "UNATTRIBUTED", "{}", a.detail());
    }

    // -----------------------------------------------------------------------
    // OQ9 instrument fix — the window must be about the stall
    // -----------------------------------------------------------------------

    /// Samples every 20 ms from 0, with the counters advancing only inside
    /// `[busy_from, busy_to)`. Everything outside that span is a quiet device.
    fn series(span_ms: u64, busy_from: f64, busy_to: f64) -> Vec<StampedSample> {
        let mut v = Vec::new();
        let (mut disk, mut psi) = (0u64, 0u64);
        let mut t = 0u64;
        while t <= span_ms {
            let at = t as f64;
            if at >= busy_from && at < busy_to {
                disk += SAMPLE_INTERVAL_MS; // fully busy
                psi += SAMPLE_INTERVAL_MS * 1_000;
            }
            v.push(StampedSample {
                at_ms: at,
                sample: SystemSample {
                    psi_io_total_us: Some(psi),
                    disk_io_ms: Some(disk),
                    dirty_kb: Some(1_024),
                    writeback_kb: Some(0),
                    thermal_max_mc: Some(50_000),
                },
            });
            t += SAMPLE_INTERVAL_MS;
        }
        v
    }

    #[test]
    fn whole_run_counters_can_no_longer_be_attributed_to_one_commit() {
        // THE DEFECT, pinned. A 60 s run with 20 s of ordinary device busy-time
        // and a 2 s stall. Under the old instrument the whole-run disk_io_ms
        // (20 000) was compared against the stall (2 000 x 0.5 = 1 000) and
        // cleared the bar easily -> a confident IO-DEVICE verdict, regardless of
        // whether the device did anything at all during those 2 seconds.
        let whole_run = WindowedDelta {
            delta: d(Some(20_000_000), Some(20_000), Some(1_024), None),
            stall_ms: 2_000.0,
            covered_ms: 60_000.0,
            samples: 3_000,
            baseline_busy_rate: None,
        };
        assert!(!whole_run.window_is_usable());
        let a = attribute_stall(&whole_run);
        assert_eq!(
            a.token(),
            "UNATTRIBUTED",
            "a whole-run window was attributed to a single commit: {}",
            a.detail()
        );
        assert!(
            a.detail().contains("INSTRUMENT"),
            "the refusal must say it is an instrument limit, not a device finding: {}",
            a.detail()
        );
    }

    #[test]
    fn a_device_idle_through_the_stall_is_not_blamed_for_it() {
        // The same run, now windowed properly: the device was busy from 0-20 s
        // and the stall ran 40 000-42 000 ms, with the device idle throughout.
        // The old instrument called this IO-DEVICE. It must not.
        let s = series(60_000, 0.0, 20_000.0);
        let w = delta_over_window(&s, 40_000.0, 42_000.0).expect("window is resolvable");
        assert!(w.window_is_usable(), "{w:?}");
        assert_eq!(
            w.delta.disk_io_ms,
            Some(0),
            "device was idle in this window"
        );
        assert_eq!(attribute_stall(&w).token(), "UNATTRIBUTED");
    }

    #[test]
    fn a_device_busy_through_the_stall_still_is() {
        // The fix must not simply suppress attribution. Same shape, but the
        // busy span now covers the stall.
        let s = series(60_000, 39_000.0, 43_000.0);
        let w = delta_over_window(&s, 40_000.0, 42_000.0).expect("window is resolvable");
        assert!(w.window_is_usable());
        assert_eq!(attribute_stall(&w).token(), "IO-DEVICE");
    }

    #[test]
    fn the_window_brackets_the_commit_rather_than_clipping_it() {
        // Bounds land between samples: the chosen pair must sit outside, so the
        // commit is fully contained. Clipping would undercount the very
        // counters the attribution turns on.
        let s = series(1_000, 0.0, 0.0);
        let w = delta_over_window(&s, 205.0, 315.0).expect("resolvable");
        assert!(
            w.covered_ms >= 110.0,
            "window {} ms does not contain the 110 ms commit",
            w.covered_ms
        );
        assert!(w.covered_ms <= 110.0 + 2.0 * SAMPLE_INTERVAL_MS as f64);
    }

    #[test]
    fn an_unresolvable_window_returns_none_rather_than_the_whole_series() {
        // Fail closed. Returning the whole-series delta here is exactly the old
        // behaviour, so `None` is the load-bearing answer.
        assert!(delta_over_window(&[], 0.0, 100.0).is_none());
        assert!(delta_over_window(&series(100, 0.0, 0.0)[..1], 0.0, 100.0).is_none());
        // A commit entirely after the last sample has no bracketing pair on the
        // right, and must not silently reuse the final sample as both ends.
        let s = series(100, 0.0, 0.0);
        let w = delta_over_window(&s, 5_000.0, 5_100.0);
        assert!(
            w.is_none() || !w.unwrap().window_is_usable(),
            "a window past the end of the series was treated as measured"
        );
    }

    #[test]
    fn a_window_that_covers_only_part_of_the_commit_is_refused() {
        // The mirror image of the original defect, and the easier one to miss:
        // the series ends at 1 000 ms but the commit runs 900-1 500 ms. The
        // first version fell back to the last sample, producing a 100 ms window
        // for a 600 ms stall — 17 % coverage — which passed the "not too wide"
        // guard and was attributed from.
        let s = series(1_000, 0.0, 1_000.0);
        assert!(
            delta_over_window(&s, 900.0, 1_500.0).is_none(),
            "a commit running past the end of the series was windowed anyway"
        );
        // Same on the left: a commit starting before the first sample.
        let late: Vec<StampedSample> = s.iter().filter(|x| x.at_ms >= 500.0).cloned().collect();
        assert!(
            delta_over_window(&late, 100.0, 700.0).is_none(),
            "a commit starting before the series was windowed anyway"
        );
        // And the guard holds even if such a delta is built by hand.
        let truncated = WindowedDelta {
            delta: d(None, Some(100), Some(1_024), None),
            stall_ms: 600.0,
            covered_ms: 100.0,
            samples: 6,
            baseline_busy_rate: None,
        };
        assert!(!truncated.window_is_usable());
    }

    // -----------------------------------------------------------------------
    // The busy-rate baseline: the control the absolute threshold never had
    // -----------------------------------------------------------------------

    /// A window whose busy rate and baseline are stated directly, so a test can
    /// say what it means without constructing a series.
    fn rates(stall_ms: f64, busy_rate: f64, baseline: Option<f64>) -> WindowedDelta {
        let covered = stall_ms * 1.02;
        WindowedDelta {
            delta: d(None, Some((busy_rate * covered) as u64), Some(1_024), None),
            stall_ms,
            covered_ms: covered,
            samples: (stall_ms / SAMPLE_INTERVAL_MS as f64) as usize + 2,
            baseline_busy_rate: baseline,
        }
    }

    #[test]
    fn the_measured_five_second_stall_is_not_blamed_on_the_device() {
        // The real numbers from the target mitigation run: 1 804 ms of device
        // busy-time across a 5 254 ms stall (0.343), against ordinary windows in
        // the same runs reading 74-100 % (baseline 0.85).
        //
        // 0.343 clears the ABSOLUTE test only barely-not — it is under 0.5, but
        // only by a third — while being less than half the run's own normal.
        // The relative arm makes the verdict robust instead of marginal.
        let w = rates(5_254.5, 0.343, Some(0.85));
        assert_eq!(w.busy_against_baseline(), Some(false));
        assert_eq!(attribute_stall(&w).token(), "UNATTRIBUTED");
    }

    #[test]
    fn a_short_window_that_would_clear_the_absolute_bar_still_needs_the_baseline() {
        // The failure this fix exists to prevent. Same stall, a busy rate of
        // 0.60 — over IO_BUSY_FRACTION, so the absolute test alone says
        // IO-DEVICE — but the device is normally at 0.95, so it was QUIETER than
        // usual. Attributing that to device work is exactly backwards.
        let w = rates(5_254.5, 0.60, Some(0.95));
        assert!(
            w.delta.disk_io_ms.unwrap() as f64 >= w.stall_ms * IO_BUSY_FRACTION,
            "this case must clear the absolute bar, or it tests nothing"
        );
        assert_eq!(w.busy_against_baseline(), Some(false));
        assert_eq!(
            attribute_stall(&w).token(),
            "UNATTRIBUTED",
            "a device quieter than its own normal was blamed for the stall"
        );
    }

    #[test]
    fn a_device_genuinely_busier_than_normal_is_still_attributed() {
        // The fix must not simply suppress IO-DEVICE. A device working at or
        // above its ordinary rate through the stall is what the verdict means,
        // and it must survive.
        let w = rates(5_254.5, 1.10, Some(0.85));
        assert_eq!(w.busy_against_baseline(), Some(true));
        assert_eq!(attribute_stall(&w).token(), "IO-DEVICE");
    }

    #[test]
    fn without_a_baseline_the_verdict_says_so_rather_than_hiding_it() {
        // No control available: the absolute test stands alone, which is where
        // it is weakest. The verdict is still reachable — refusing outright
        // would lose real findings on systems whose series cannot supply a
        // baseline — but the detail must not present it as fully supported.
        let w = rates(29_000.0, 0.90, None);
        assert_eq!(w.busy_against_baseline(), None);
        let a = attribute_stall(&w);
        assert_eq!(a.token(), "IO-DEVICE");
        assert!(
            a.detail().contains("NO BASELINE"),
            "an unsupported IO-DEVICE verdict did not disclose the missing control: {}",
            a.detail()
        );
    }

    #[test]
    fn the_baseline_excludes_the_stall_it_is_a_control_for() {
        // Busy 0-1000 ms, then a quiet stall 1000-3000 ms, then busy again.
        // A baseline that included the stall would be dragged down by it and
        // the comparison would flatter the device.
        let mut v = Vec::new();
        let (mut disk, mut t) = (0u64, 0u64);
        while t <= 5_000 {
            if !(1_000..3_000).contains(&t) {
                disk += SAMPLE_INTERVAL_MS; // fully busy outside the stall
            }
            v.push(StampedSample {
                at_ms: t as f64,
                sample: SystemSample {
                    disk_io_ms: Some(disk),
                    dirty_kb: Some(1_024),
                    writeback_kb: Some(0),
                    ..Default::default()
                },
            });
            t += SAMPLE_INTERVAL_MS;
        }
        let b = baseline_busy_rate_excluding(&v, 1_000.0, 3_000.0).expect("resolvable");
        assert!(
            b > 0.9,
            "baseline {b:.3} was dragged down by the stall it should exclude"
        );
        let w = windowed_delta_with_baseline(&v, 1_000.0, 3_000.0).expect("resolvable");
        assert_eq!(w.busy_against_baseline(), Some(false));
        assert_eq!(attribute_stall(&w).token(), "UNATTRIBUTED");
    }

    #[test]
    fn a_run_that_is_almost_all_stall_yields_no_baseline() {
        // Fail closed: a "baseline" drawn from a run that is mostly the stall is
        // a control that isn't one.
        let s = series(1_500, 0.0, 0.0);
        assert!(baseline_busy_rate_excluding(&s, 100.0, 1_400.0).is_none());
        assert!(baseline_busy_rate_excluding(&[], 0.0, 100.0).is_none());
    }

    #[test]
    fn too_few_samples_is_not_a_measurement() {
        let thin = WindowedDelta {
            delta: d(None, Some(20_000), Some(1_024), None),
            stall_ms: 29_000.0,
            covered_ms: 29_000.0,
            samples: MIN_WINDOW_SAMPLES - 1,
            baseline_busy_rate: None,
        };
        assert!(!thin.window_is_usable());
        assert_eq!(attribute_stall(&thin).token(), "UNATTRIBUTED");
    }

    #[test]
    fn the_peak_backlog_is_taken_inside_the_window_only() {
        // A backlog spike elsewhere in the run must not turn this stall into
        // WRITEBACK-BACKLOG — same denominator error, different counter.
        let mut s = series(60_000, 39_000.0, 43_000.0);
        for x in s.iter_mut() {
            if x.at_ms < 10_000.0 {
                x.sample.dirty_kb = Some(4 * 1_024 * 1_024); // huge, far from the stall
            }
        }
        let w = delta_over_window(&s, 40_000.0, 42_000.0).expect("resolvable");
        assert!(
            w.delta.peak_dirty_writeback_kb.unwrap_or(0) < WRITEBACK_BACKLOG_KB,
            "a backlog outside the window leaked into it"
        );
        assert_eq!(attribute_stall(&w).token(), "IO-DEVICE");
    }

    #[test]
    fn a_counter_that_went_backwards_yields_none_not_a_wrapped_value() {
        let before = SystemSample {
            psi_io_total_us: Some(1_000),
            disk_io_ms: Some(500),
            ..Default::default()
        };
        let after = SystemSample {
            psi_io_total_us: Some(10),
            disk_io_ms: Some(5),
            ..Default::default()
        };
        let dd = delta(&before, &after, None);
        assert_eq!(dd.psi_io_stall_us, None);
        assert_eq!(dd.disk_io_ms, None);
    }

    #[test]
    fn observing_zero_stalls_is_a_result_not_a_failure() {
        // Failing here would push an operator to re-run until a stall appears,
        // which turns a rate estimate into fiction.
        let r = StallResult {
            repeats: 20,
            completed: 20,
            stalls_observed: 0,
            worst_commit_ms: 12.0,
            median_worst_ms: 9.0,
            throughput_eps: vec![30_000.0; 20],
            attribution: attribute_stall(&win(12.0, SystemDelta::default())),
            stall_durations_ms: Vec::new(),
            delta_at_worst: win(0.0, SystemDelta::default()),
        };
        assert!(!r.fails_the_run());
        assert_eq!(r.stall_rate(), 0.0);
        assert_eq!(r.attribution.token(), "NO-STALL");
    }

    #[test]
    fn median_throughput_is_robust_to_one_pathological_run() {
        // The exact failure this reports around: one 29 s stall dragged the
        // NORMAL/batch=64 row to 3123 ev/s, below NORMAL/batch=1, inverting
        // the durability ordering. A median over repetitions cannot be moved
        // by one such run, so throughput and stall rate become two separate
        // facts rather than one contaminated one.
        let r = StallResult {
            repeats: 5,
            completed: 5,
            stalls_observed: 1,
            worst_commit_ms: 29_271.78,
            median_worst_ms: 11.0,
            throughput_eps: vec![36_000.0, 35_800.0, 3_123.0, 36_200.0, 35_900.0],
            attribution: StallAttribution::Unattributed(String::new()),
            stall_durations_ms: Vec::new(),
            delta_at_worst: win(0.0, SystemDelta::default()),
        };
        assert_eq!(r.median_throughput_eps(), 35_900.0);
        assert_eq!(r.stall_rate(), 0.2);
    }

    #[test]
    fn median_throughput_of_nothing_is_not_zero() {
        let r = StallResult {
            repeats: 3,
            completed: 0,
            stalls_observed: 0,
            worst_commit_ms: 0.0,
            median_worst_ms: f64::NAN,
            throughput_eps: vec![],
            attribution: StallAttribution::Unattributed(String::new()),
            stall_durations_ms: Vec::new(),
            delta_at_worst: win(0.0, SystemDelta::default()),
        };
        assert!(r.median_throughput_eps().is_nan());
    }

    #[test]
    fn a_run_that_completed_nothing_fails() {
        let r = StallResult {
            repeats: 20,
            completed: 0,
            stalls_observed: 0,
            worst_commit_ms: 0.0,
            median_worst_ms: f64::NAN,
            throughput_eps: vec![],
            attribution: StallAttribution::Unattributed(String::new()),
            stall_durations_ms: Vec::new(),
            delta_at_worst: win(0.0, SystemDelta::default()),
        };
        assert!(r.fails_the_run());
        assert!(r.stall_rate().is_nan());
    }

    #[test]
    fn stall_rate_distinguishes_rare_from_frequent() {
        let mk = |stalls| StallResult {
            repeats: 20,
            completed: 20,
            stalls_observed: stalls,
            worst_commit_ms: 29_000.0,
            median_worst_ms: 10.0,
            throughput_eps: vec![],
            attribution: StallAttribution::Unattributed(String::new()),
            stall_durations_ms: Vec::new(),
            delta_at_worst: win(0.0, SystemDelta::default()),
        };
        assert_eq!(mk(1).stall_rate(), 0.05);
        assert_eq!(mk(10).stall_rate(), 0.5);
    }

    #[test]
    fn the_stall_durations_are_exactly_the_repetitions_that_stalled() {
        // Calls `stall_durations_from` — the function `run` uses — with input
        // deliberately OUT OF ORDER. An earlier version of this test filtered a
        // pre-sorted array itself, so it asserted "ascending" about a list that
        // arrived ascending and would have passed even if the real code stopped
        // sorting. A test that reimplements the logic tests the
        // reimplementation.
        let worst_per_run = [30_182.4, 4.0, 8_496.0, 12.5, STALL_THRESHOLD_MS];
        let durations = stall_durations_from(&worst_per_run);

        // Sorted output from unsorted input: the sort is real, not inherited.
        assert_eq!(durations, vec![STALL_THRESHOLD_MS, 8_496.0, 30_182.4]);
        assert!(durations.windows(2).all(|w| w[0] <= w[1]));

        // Length ties to what `stalls_observed` counts, by the same predicate.
        assert_eq!(
            durations.len(),
            worst_per_run
                .iter()
                .filter(|ms| **ms >= STALL_THRESHOLD_MS)
                .count(),
            "the array length must equal stalls_observed by construction"
        );
        // The threshold is inclusive, matching the `>=` that counts a stall.
        assert!(durations.contains(&STALL_THRESHOLD_MS));
        assert!(stall_durations_from(&[]).is_empty());
        assert!(stall_durations_from(&[4.0, 999.999]).is_empty());
    }

    #[test]
    fn every_attribution_token_is_distinct() {
        let tokens = [
            StallAttribution::IoDevice(String::new()).token(),
            StallAttribution::WritebackBacklog(String::new()).token(),
            StallAttribution::Thermal(String::new()).token(),
            StallAttribution::Unattributed(String::new()).token(),
            StallAttribution::NoStall(String::new()).token(),
        ];
        let unique: std::collections::HashSet<_> = tokens.iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn sampling_never_panics_on_this_host() {
        // Every field is Option precisely because these paths are absent on
        // some kernels and in some containers.
        let s = sample_system();
        let _ = delta(&s, &s, None);
    }
}
