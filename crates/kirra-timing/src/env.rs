//! Measurement environment tagging — the host-vs-target WCET invariant in types.

/// The environment a timing campaign ran in.
///
/// This enum is the **single source of truth** for the normative rule in
/// `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`: *host numbers are INDICATIVE,
/// never WCET; only QNX-target-under-FIFO numbers feed an FTTI claim.* Reports
/// (see [`crate::report`]) print an INDICATIVE banner and a non-certified
/// `wcet_status` unless [`MeasurementEnv::is_certified_wcet`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementEnv {
    /// A developer/shared host. Indicative only.
    Host,
    /// A CI runner (shared, virtualized). Indicative only — scheduler/hypervisor
    /// jitter dominates `max`; this is the `wcet_gate.rs` precedent.
    CiRunner,
    /// The QNX safety partition under `SCHED_FIFO`, certified toolchain, frozen
    /// partition config. The ONLY environment whose numbers are WCET evidence.
    QnxTargetFifo,
    /// Any other environment (e.g. QNX target NOT under FIFO). Indicative.
    Other,
}

impl MeasurementEnv {
    /// `true` only for [`MeasurementEnv::QnxTargetFifo`]. Everything else is
    /// indicative — never presented as WCET (methodology §4 host-indicative rule).
    #[must_use]
    pub const fn is_certified_wcet(self) -> bool {
        matches!(self, Self::QnxTargetFifo)
    }

    /// Short stable label for CSV / report rows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::CiRunner => "ci-runner",
            Self::QnxTargetFifo => "qnx-target-fifo",
            Self::Other => "other",
        }
    }

    /// The `wcet_status` token used in CSV exports — mirrors
    /// `tools/qnx-rtm-harness/wcet_measure.cpp`: `QNX-TARGET-MEASURED` on the
    /// certified target, `INDICATIVE-NOT-WCET` everywhere else.
    #[must_use]
    pub const fn wcet_status(self) -> &'static str {
        if self.is_certified_wcet() {
            "QNX-TARGET-MEASURED"
        } else {
            "INDICATIVE-NOT-WCET"
        }
    }
}
