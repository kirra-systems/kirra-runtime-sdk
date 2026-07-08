// parko/crates/parko-ros2/src/ood_feed.rs
//
// EP-05 (M1) — the live per-tick feed for the WP-24/G-15 OOD monitor: the
// non-test consumer of `parko_core::ood`.
//
// The pure monitor (`OodMonitor::assess`) answers "has the live confidence
// distribution drifted from calibration?"; this module supplies its two
// missing runtime halves:
//
//   1. **The ring window** — `OodFeed` accumulates the per-tick perception
//      confidence (the node feeds the freshest corridor confidence each tick)
//      into a bounded window and assesses it against the frozen baseline.
//      The recommendation folds into the tick posture via
//      `posture.escalate(...)` — the SAME seam the redundancy comparator uses
//      (escalation-only; a drifted window can tighten the envelope, never
//      relax it).
//   2. **The calibration baseline file** — a newline-separated list of f64
//      calibration samples (`#` comments allowed) recorded from a nominal
//      corpus, binned over the confidence domain [0, 1). Loading is
//      fail-closed: an unreadable / unparsable / too-small corpus refuses to
//      construct a monitor (a baseline that cannot be trusted must not
//      silently license one).
//
// Env gate (read by the parko node PROCESS, like `KIRRA_MODEL_ALLOWLIST` —
// not a verifier-registry var):
//
//   KIRRA_OOD_ENABLED        — "1"/"true" arms the feed; unset/""/"0"/"false"
//                              → off (default, byte-identical tick). Any OTHER
//                              value is refused at startup (a typo must not
//                              silently disable a safety monitor).
//   KIRRA_OOD_BASELINE_PATH  — the calibration baseline file. REQUIRED when
//                              enabled; enabled-without-a-loadable-baseline is
//                              a fail-closed startup abort, never a monitor
//                              that silently isn't watching.
//
// The env split follows the house pattern: `ood_feed_from_env_values` is the
// pure routing (unit-tested with injected values — no `set_var`, INVARIANT
// #13); `ood_feed_from_env` is the one-line process-env reader the binary
// calls once at startup.

use std::collections::VecDeque;

use parko_core::ood::{CalibrationBaseline, OodAssessment, OodMonitor};
use parko_core::safety::SafetyPosture;

/// Env var arming the OOD feed ("1"/"true"; unset/"0"/"false" → off).
pub const KIRRA_OOD_ENABLED_ENV: &str = "KIRRA_OOD_ENABLED";
/// Env var naming the calibration baseline file (required when enabled).
pub const KIRRA_OOD_BASELINE_PATH_ENV: &str = "KIRRA_OOD_BASELINE_PATH";

/// The confidence domain the baseline and live window are binned over.
/// Perception confidences are proportions in `[0, 1)`.
pub const OOD_CONFIDENCE_LO: f64 = 0.0;
pub const OOD_CONFIDENCE_HI: f64 = 1.0;
/// Bin count for the PSI histogram (10 = the standard PSI decile convention).
pub const OOD_CONFIDENCE_BINS: usize = 10;
/// Default ring-window capacity: 128 ticks ≈ 6.4 s @ 20 Hz — enough above the
/// monitor's `DEFAULT_MIN_WINDOW` (30) that the assessment is evidence-backed,
/// short enough that a genuine shift dominates the window within seconds.
pub const DEFAULT_OOD_WINDOW_CAPACITY: usize = 128;

/// Why the OOD feed could not be constructed. Every variant is a fail-closed
/// startup abort when the gate is armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OodFeedError {
    /// `KIRRA_OOD_ENABLED` carried an unrecognized value (typo).
    InvalidEnable(String),
    /// Enabled but `KIRRA_OOD_BASELINE_PATH` is unset/empty.
    BaselinePathMissing,
    /// The baseline file could not be read.
    BaselineUnreadable { path: String, cause: String },
    /// The baseline file parsed/validated to nothing trustworthy (bad float,
    /// non-finite sample, too few samples, empty corpus).
    BaselineInvalid { path: String, cause: String },
}

impl core::fmt::Display for OodFeedError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OodFeedError::InvalidEnable(v) => write!(
                f,
                "{KIRRA_OOD_ENABLED_ENV}={v:?} is not a recognized value \
                 (use 1/true or 0/false); refusing to guess for a safety monitor"
            ),
            OodFeedError::BaselinePathMissing => write!(
                f,
                "{KIRRA_OOD_ENABLED_ENV} is set but {KIRRA_OOD_BASELINE_PATH_ENV} is \
                 unset/empty — an armed OOD monitor with no baseline cannot watch anything \
                 (fail-closed)"
            ),
            OodFeedError::BaselineUnreadable { path, cause } => {
                write!(f, "cannot read OOD baseline '{path}': {cause}")
            }
            OodFeedError::BaselineInvalid { path, cause } => {
                write!(f, "OOD baseline '{path}' is not a trustworthy corpus: {cause}")
            }
        }
    }
}
impl std::error::Error for OodFeedError {}

/// Parse a baseline corpus: one f64 sample per line; blank lines and
/// `#`-prefixed comments skipped. Any unparsable line is refused (fail-closed
/// — a half-read corpus would silently shift the baseline distribution).
pub fn parse_baseline_samples(text: &str) -> Result<Vec<f64>, String> {
    let mut samples = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.parse::<f64>() {
            Ok(v) => samples.push(v),
            Err(e) => return Err(format!("line {}: {e} ({line:?})", i + 1)),
        }
    }
    if samples.is_empty() {
        return Err("no samples (empty corpus)".to_string());
    }
    Ok(samples)
}

/// Load a calibration baseline from `path`, binned over the confidence domain.
/// Fail-closed end to end: unreadable file, bad line, non-finite sample, or a
/// corpus below `MIN_CALIBRATION_SAMPLES` all refuse.
pub fn load_baseline(path: &str) -> Result<CalibrationBaseline, OodFeedError> {
    let text = std::fs::read_to_string(path).map_err(|e| OodFeedError::BaselineUnreadable {
        path: path.to_string(),
        cause: e.to_string(),
    })?;
    let samples = parse_baseline_samples(&text)
        .map_err(|cause| OodFeedError::BaselineInvalid { path: path.to_string(), cause })?;
    CalibrationBaseline::from_samples(&samples, OOD_CONFIDENCE_BINS, OOD_CONFIDENCE_LO, OOD_CONFIDENCE_HI)
        .map_err(|e| OodFeedError::BaselineInvalid { path: path.to_string(), cause: e.to_string() })
}

/// The live feed: a frozen monitor plus the bounded ring window the node
/// pushes one perception confidence into per tick.
#[derive(Debug, Clone)]
pub struct OodFeed {
    monitor: OodMonitor,
    window: VecDeque<f64>,
    capacity: usize,
}

impl OodFeed {
    /// A feed over `monitor` with a bounded window (`capacity` forced ≥ 1).
    #[must_use]
    pub fn new(monitor: OodMonitor, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self { monitor, window: VecDeque::with_capacity(capacity), capacity }
    }

    /// Push one per-tick confidence sample, evicting the oldest at capacity.
    /// Non-finite values are kept deliberately: `assess` fails CLOSED on them
    /// (a NaN confidence is a broken producer, not a skippable sample).
    pub fn observe(&mut self, confidence: f64) {
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(confidence);
    }

    /// Assess the current window (see `OodMonitor::assess` for the fail-closed
    /// table: non-finite → LockedOut; under-filled → Nominal no-op; else PSI
    /// through the bands).
    #[must_use]
    pub fn assess(&self) -> OodAssessment {
        // The VecDeque may wrap; the window is ≤ capacity (≈128) f64s, so a
        // per-tick copy into a contiguous slice is negligible on this slow path.
        let window: Vec<f64> = self.window.iter().copied().collect();
        self.monitor.assess(&window)
    }

    /// Fold this tick's assessment into `source`: escalation-only, the same
    /// seam the redundancy comparator uses (`posture.escalate(...)`). Returns
    /// the effective posture plus the assessment for logging/audit.
    #[must_use]
    pub fn escalate(&self, source: SafetyPosture) -> (SafetyPosture, OodAssessment) {
        let assessment = self.assess();
        (source.escalate(assessment.recommended), assessment)
    }

    /// Current window fill (observability).
    #[must_use]
    pub fn window_len(&self) -> usize {
        self.window.len()
    }
}

/// Pure env routing (unit-testable with injected values; no `set_var`):
/// - gate unset/empty/`0`/`false` → `Ok(None)` (feed off, byte-identical tick)
/// - gate `1`/`true` → the baseline is REQUIRED and must load → `Ok(Some(feed))`
/// - anything else → `Err` (fail-closed: no guessing for a safety monitor)
pub fn ood_feed_from_env_values(
    enabled: Option<&str>,
    baseline_path: Option<&str>,
) -> Result<Option<OodFeed>, OodFeedError> {
    let armed = match enabled.map(str::trim) {
        None | Some("") => false,
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
        Some(v) => return Err(OodFeedError::InvalidEnable(v.to_string())),
    };
    if !armed {
        return Ok(None);
    }
    let path = match baseline_path.map(str::trim) {
        Some(p) if !p.is_empty() => p,
        _ => return Err(OodFeedError::BaselinePathMissing),
    };
    let baseline = load_baseline(path)?;
    Ok(Some(OodFeed::new(OodMonitor::new(baseline), DEFAULT_OOD_WINDOW_CAPACITY)))
}

/// Read the gate + baseline path from the process env once (startup) and
/// route through [`ood_feed_from_env_values`].
pub fn ood_feed_from_env() -> Result<Option<OodFeed>, OodFeedError> {
    let enabled = std::env::var(KIRRA_OOD_ENABLED_ENV).ok();
    let path = std::env::var(KIRRA_OOD_BASELINE_PATH_ENV).ok();
    ood_feed_from_env_values(enabled.as_deref(), path.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parko_core::ood::OodReason;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique temp path per call (parallel tests; no env mutation, no RNG).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    fn write_temp(content: &str) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir()
            .join(format!("parko_ep05_{}_{n}.baseline", std::process::id()));
        std::fs::File::create(&p).unwrap().write_all(content.as_bytes()).unwrap();
        p
    }

    /// A nominal corpus concentrated around 0.8 confidence (mirrors
    /// parko-core's ood test corpus), as baseline-file text.
    fn nominal_corpus_text() -> String {
        let mut out = String::from("# nominal calibration corpus\n");
        for i in 0..200 {
            // Deterministic spread over [0.6, 0.9].
            let v = 0.6 + 0.3 * ((i * 37 % 200) as f64 / 200.0);
            out.push_str(&format!("{v}\n"));
        }
        out
    }

    #[test]
    fn disabled_gate_is_a_noop() {
        assert_eq!(ood_feed_from_env_values(None, None).unwrap().map(|_| ()), None);
        assert_eq!(ood_feed_from_env_values(Some(""), None).unwrap().map(|_| ()), None);
        assert_eq!(ood_feed_from_env_values(Some("0"), None).unwrap().map(|_| ()), None);
        assert_eq!(ood_feed_from_env_values(Some("false"), Some("/x")).unwrap().map(|_| ()), None);
    }

    #[test]
    fn unrecognized_enable_value_is_refused() {
        let err = ood_feed_from_env_values(Some("yes"), Some("/x")).unwrap_err();
        assert!(matches!(err, OodFeedError::InvalidEnable(_)), "{err}");
    }

    #[test]
    fn enabled_without_a_baseline_path_fails_closed() {
        assert_eq!(
            ood_feed_from_env_values(Some("1"), None).unwrap_err(),
            OodFeedError::BaselinePathMissing
        );
        assert_eq!(
            ood_feed_from_env_values(Some("true"), Some("  ")).unwrap_err(),
            OodFeedError::BaselinePathMissing
        );
    }

    #[test]
    fn enabled_with_a_missing_file_fails_closed() {
        let err = ood_feed_from_env_values(Some("1"), Some("/nonexistent/ep05.baseline"))
            .unwrap_err();
        assert!(matches!(err, OodFeedError::BaselineUnreadable { .. }), "{err}");
    }

    #[test]
    fn a_corrupt_baseline_line_is_refused_not_skipped() {
        let mut text = nominal_corpus_text();
        text.push_str("0.7\nnot-a-float\n0.8\n");
        let p = write_temp(&text);
        let err = ood_feed_from_env_values(Some("1"), Some(p.to_str().unwrap())).unwrap_err();
        assert!(matches!(err, OodFeedError::BaselineInvalid { .. }), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_too_small_corpus_is_refused() {
        let p = write_temp("0.8\n0.7\n0.9\n");
        let err = ood_feed_from_env_values(Some("1"), Some(p.to_str().unwrap())).unwrap_err();
        assert!(matches!(err, OodFeedError::BaselineInvalid { .. }), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn enabled_with_a_good_baseline_builds_the_feed() {
        let p = write_temp(&nominal_corpus_text());
        let feed = ood_feed_from_env_values(Some("true"), Some(p.to_str().unwrap()))
            .unwrap()
            .expect("armed gate + good baseline → a live feed");
        assert_eq!(feed.window_len(), 0);
        std::fs::remove_file(&p).ok();
    }

    fn feed_over_nominal_baseline() -> OodFeed {
        let p = write_temp(&nominal_corpus_text());
        let feed = ood_feed_from_env_values(Some("1"), Some(p.to_str().unwrap()))
            .unwrap()
            .unwrap();
        std::fs::remove_file(&p).ok();
        feed
    }

    #[test]
    fn window_is_bounded_at_capacity() {
        let mut feed = feed_over_nominal_baseline();
        for _ in 0..(DEFAULT_OOD_WINDOW_CAPACITY + 50) {
            feed.observe(0.8);
        }
        assert_eq!(feed.window_len(), DEFAULT_OOD_WINDOW_CAPACITY);
    }

    #[test]
    fn nominal_stream_does_not_escalate() {
        let mut feed = feed_over_nominal_baseline();
        for i in 0..100 {
            feed.observe(0.6 + 0.3 * ((i * 37 % 100) as f64 / 100.0));
        }
        let (posture, a) = feed.escalate(SafetyPosture::Nominal);
        assert_eq!(posture, SafetyPosture::Nominal, "psi={}", a.psi);
        assert_eq!(a.reason, OodReason::Stable);
    }

    #[test]
    fn a_collapsed_confidence_stream_locks_out() {
        let mut feed = feed_over_nominal_baseline();
        for _ in 0..100 {
            feed.observe(0.05); // the detector collapsed to near-zero confidence
        }
        let (posture, a) = feed.escalate(SafetyPosture::Nominal);
        assert_eq!(posture, SafetyPosture::LockedOut, "psi={}", a.psi);
    }

    #[test]
    fn a_non_finite_confidence_is_a_hard_fault() {
        let mut feed = feed_over_nominal_baseline();
        for _ in 0..60 {
            feed.observe(0.8);
        }
        feed.observe(f64::NAN);
        let (posture, a) = feed.escalate(SafetyPosture::Nominal);
        assert_eq!(posture, SafetyPosture::LockedOut);
        assert_eq!(a.reason, OodReason::NonFiniteInput);
    }

    #[test]
    fn an_under_filled_window_is_a_noop() {
        let mut feed = feed_over_nominal_baseline();
        feed.observe(0.05); // low — but a single sample is no evidence
        let (posture, a) = feed.escalate(SafetyPosture::Nominal);
        assert_eq!(posture, SafetyPosture::Nominal);
        assert_eq!(a.reason, OodReason::InsufficientWindow);
    }

    #[test]
    fn escalation_never_relaxes_the_source_posture() {
        // A stable window folded into a LockedOut source stays LockedOut —
        // the feed is escalation-only, exactly like the comparator seam.
        let mut feed = feed_over_nominal_baseline();
        for i in 0..100 {
            feed.observe(0.6 + 0.3 * ((i * 37 % 100) as f64 / 100.0));
        }
        let (posture, _) = feed.escalate(SafetyPosture::LockedOut);
        assert_eq!(posture, SafetyPosture::LockedOut);
    }
}
