//! Deterministic report rendering — CSV (machine) and a human-readable table.
//!
//! Rendering is `core::fmt`-only: no allocation, no file I/O. The caller writes
//! the rendered text wherever it wants (stdout, a file, a CI artifact). The
//! report carries its [`MeasurementEnv`] so every row self-declares whether it is
//! WCET evidence or merely indicative — the methodology's host-indicative rule,
//! enforced at render time.

use core::fmt;

use crate::channel::ChannelStats;
use crate::env::MeasurementEnv;

/// The canonical CSV header — the **single source of truth** for the machine
/// schema shared by every WCET measurement surface (this crate's
/// [`Report::write_csv`], the host `kirra-wcet-bench`, and the QNX
/// `tools/qnx-rtm-harness/wcet_measure` row). Downstream tooling joins on these
/// columns, so the harness emits this byte-for-byte rather than a divergent
/// subset. Keep [`Report::write_csv`]'s row column order identical to this.
pub const CSV_HEADER: &str =
    "metric,env,sched,n,min_ns,mean_ns,max_ns,stddev_ns,p50_ns,p99_ns,p999_ns,wcet_status";

/// One named stage's statistics within a [`Report`].
#[derive(Debug, Clone, Copy)]
pub struct StageReport {
    /// Stable stage label, e.g. `"perception_input"` or `"governor_exec"`.
    pub stage: &'static str,
    /// The stage's measured statistics.
    pub stats: ChannelStats,
}

impl StageReport {
    /// Pair a stage label with its stats.
    #[must_use]
    pub const fn new(stage: &'static str, stats: ChannelStats) -> Self {
        Self { stage, stats }
    }
}

/// A complete campaign report: an environment tag, the scheduler description, and
/// one [`StageReport`] per measured stage.
#[derive(Debug, Clone, Copy)]
pub struct Report<'a> {
    /// Where the measurement ran — gates the WCET-vs-indicative status.
    pub env: MeasurementEnv,
    /// Scheduler description, e.g. `"SCHED_FIFO"` (target) or `"host-default"`.
    pub sched: &'static str,
    /// One entry per measured stage.
    pub stages: &'a [StageReport],
}

impl<'a> Report<'a> {
    /// Build a report.
    #[must_use]
    pub const fn new(env: MeasurementEnv, sched: &'static str, stages: &'a [StageReport]) -> Self {
        Self { env, sched, stages }
    }

    /// `true` only when every row is certified WCET evidence (QNX target / FIFO).
    #[must_use]
    pub const fn is_wcet_evidence(&self) -> bool {
        self.env.is_certified_wcet()
    }

    /// Write the machine-readable CSV (header + one row per stage). The header is
    /// [`CSV_HEADER`], the canonical schema that the QNX
    /// `tools/qnx-rtm-harness/wcet_measure` row now emits byte-for-byte too, so a
    /// host report and an on-target report union into one table joinable on
    /// `metric` + `env`. Row column order matches [`CSV_HEADER`] exactly.
    ///
    /// # Errors
    /// Propagates any [`fmt::Error`] from the writer.
    pub fn write_csv(&self, w: &mut impl fmt::Write) -> fmt::Result {
        writeln!(w, "{CSV_HEADER}")?;
        for s in self.stages {
            let st = &s.stats;
            writeln!(
                w,
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                s.stage,
                self.env.label(),
                self.sched,
                st.count,
                st.min_ns,
                st.mean_ns,
                st.max_ns,
                st.stddev_ns,
                st.p50_ns,
                st.p99_ns,
                st.p999_ns,
                self.env.wcet_status(),
            )?;
        }
        Ok(())
    }
}

/// Human-readable rendering: a banner that states WCET-vs-indicative up front,
/// then a per-stage table. The banner is the methodology rule made visible —
/// anyone reading a host/CI report is told, unmissably, that it is NOT WCET.
impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_wcet_evidence() {
            writeln!(
                f,
                "=== WCET REPORT — CERTIFIED ({}, {}) ===",
                self.env.label(),
                self.sched
            )?;
        } else {
            writeln!(
                f,
                "=== TIMING REPORT — INDICATIVE, NOT WCET ({}, {}) ===",
                self.env.label(),
                self.sched
            )?;
            writeln!(
                f,
                "    Host/CI numbers are indicative only. Certified WCET requires the"
            )?;
            writeln!(
                f,
                "    QNX target under FIFO scheduling (#274). Do not cite as WCET evidence."
            )?;
        }
        writeln!(
            f,
            "{:<22} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "stage", "n", "min_ns", "mean_ns", "max_ns", "stddev", "p99_ns", "p999_ns"
        )?;
        for s in self.stages {
            let st = &s.stats;
            writeln!(
                f,
                "{:<22} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
                s.stage,
                st.count,
                st.min_ns,
                st.mean_ns,
                st.max_ns,
                st.stddev_ns,
                st.p99_ns,
                st.p999_ns,
            )?;
        }
        Ok(())
    }
}
