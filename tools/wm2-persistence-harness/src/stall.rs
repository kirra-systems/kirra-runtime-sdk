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
    /// `/proc/diskstats` field 10 summed: milliseconds spent doing I/O.
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

/// Attribute one stall from its duration and the system delta across it.
///
/// The ordering matters. Device-side I/O is checked before writeback because a
/// writeback flush *also* drives the block layer busy, so "disk busy" alone
/// cannot distinguish them — the discriminator is whether a large dirty backlog
/// was present. Thermal is checked last and only in the absence of I/O
/// evidence, because a hot SoC during heavy I/O is a consequence, not a cause.
pub fn attribute_stall(stall_ms: f64, d: &SystemDelta) -> StallAttribution {
    if stall_ms < STALL_THRESHOLD_MS {
        return StallAttribution::NoStall(format!(
            "worst commit {stall_ms:.1} ms is below the {STALL_THRESHOLD_MS:.0} ms threshold"
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
    let io_evidence = io_busy.unwrap_or(false) || psi_busy.unwrap_or(false);

    if io_evidence && !backlog {
        return StallAttribution::IoDevice(format!(
            "block layer busy for most of a {stall_ms:.0} ms stall with no large dirty \
             backlog (disk_io_ms={:?}, psi_io_stall_us={:?}, peak_dirty_writeback_kb={:?}). \
             Device-side: on NVMe this points at garbage collection or an SLC-cache cliff. \
             Confirm with the drive's own SMART/health counters — this harness cannot read \
             them.",
            d.disk_io_ms, d.psi_io_stall_us, d.peak_dirty_writeback_kb
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
    pub throughput_eps: Vec<f64>,
    pub attribution: StallAttribution,
    pub delta_at_worst: SystemDelta,
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

/// Summed field 10 of `/proc/diskstats` — milliseconds spent doing I/O.
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
    let mut worst_delta = SystemDelta::default();

    for i in 0..repeats {
        let before = sample_system();
        let peak_dw = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sampler = {
            use std::sync::atomic::Ordering;
            let peak_dw = std::sync::Arc::clone(&peak_dw);
            let running = std::sync::Arc::clone(&running);
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    let s = sample_system();
                    if let (Some(d), Some(w)) = (s.dirty_kb, s.writeback_kb) {
                        peak_dw.fetch_max(d + w, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
        };

        let r = crate::bench::append(path, durability, events, entities, batch, seed + i as u64);

        running.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = sampler.join();
        let after = sample_system();

        let Ok(r) = r else { continue };
        let max_ms = r.timing.max_us / 1e3;
        worst_per_run.push(max_ms);
        throughput.push(r.events_per_second);
        if max_ms >= STALL_THRESHOLD_MS {
            stalls += 1;
        }
        if max_ms > worst_overall {
            worst_overall = max_ms;
            let peak = peak_dw.load(std::sync::atomic::Ordering::Relaxed);
            worst_delta = delta(&before, &after, (peak > 0).then_some(peak));
        }
    }

    worst_per_run.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_worst = if worst_per_run.is_empty() {
        f64::NAN
    } else {
        worst_per_run[worst_per_run.len() / 2]
    };

    let attribution = if worst_per_run.is_empty() {
        StallAttribution::Unattributed(
            "no repetition completed, so nothing was measured — raise --repeats or check the \
             database path"
                .into(),
        )
    } else {
        attribute_stall(worst_overall, &worst_delta)
    };

    StallResult {
        repeats,
        completed: worst_per_run.len(),
        stalls_observed: stalls,
        worst_commit_ms: worst_overall,
        median_worst_ms: median_worst,
        throughput_eps: throughput,
        attribution,
        delta_at_worst: worst_delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let a = attribute_stall(8.75, &SystemDelta::default());
        assert_eq!(a.token(), "NO-STALL");
    }

    #[test]
    fn the_measured_29_second_event_is_over_the_threshold() {
        // The event this module exists for: 29 271.78 ms.
        let a = attribute_stall(29_271.78, &SystemDelta::default());
        assert_ne!(a.token(), "NO-STALL");
    }

    #[test]
    fn no_counters_at_all_is_unattributed_not_a_guess() {
        let a = attribute_stall(29_271.78, &SystemDelta::default());
        assert_eq!(a.token(), "UNATTRIBUTED", "{}", a.detail());
    }

    #[test]
    fn busy_block_layer_without_a_backlog_points_at_the_device() {
        // 20 s of disk-busy inside a 29 s stall, small dirty set.
        let a = attribute_stall(29_000.0, &d(None, Some(20_000), Some(1_024), None));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
        assert!(a.detail().contains("SMART"), "must say what it cannot read");
    }

    #[test]
    fn busy_block_layer_with_a_backlog_points_at_writeback_instead() {
        // Same disk-busy evidence, but a 1 GB dirty backlog explains it.
        let a = attribute_stall(29_000.0, &d(None, Some(20_000), Some(1_024 * 1_024), None));
        assert_eq!(a.token(), "WRITEBACK-BACKLOG", "{}", a.detail());
    }

    #[test]
    fn psi_alone_is_enough_io_evidence() {
        let a = attribute_stall(29_000.0, &d(Some(20_000_000), None, Some(1_024), None));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
    }

    #[test]
    fn heat_with_an_idle_block_layer_points_at_thermal() {
        let a = attribute_stall(29_000.0, &d(Some(10), Some(5), Some(1_024), Some(96_000)));
        assert_eq!(a.token(), "THERMAL", "{}", a.detail());
    }

    #[test]
    fn heat_during_heavy_io_is_not_blamed_on_thermal() {
        // A hot SoC during sustained I/O is a consequence, not a cause.
        // Blaming thermal here would send the investigation the wrong way.
        let a = attribute_stall(29_000.0, &d(None, Some(25_000), Some(1_024), Some(96_000)));
        assert_eq!(a.token(), "IO-DEVICE", "{}", a.detail());
    }

    #[test]
    fn a_briefly_busy_disk_does_not_explain_a_long_stall() {
        // 100 ms of disk activity inside a 29 s stall explains nothing.
        let a = attribute_stall(29_000.0, &d(None, Some(100), Some(1_024), None));
        assert_eq!(a.token(), "UNATTRIBUTED", "{}", a.detail());
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
            attribution: attribute_stall(12.0, &SystemDelta::default()),
            delta_at_worst: SystemDelta::default(),
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
            delta_at_worst: SystemDelta::default(),
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
            delta_at_worst: SystemDelta::default(),
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
            delta_at_worst: SystemDelta::default(),
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
            delta_at_worst: SystemDelta::default(),
        };
        assert_eq!(mk(1).stall_rate(), 0.05);
        assert_eq!(mk(10).stall_rate(), 0.5);
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
