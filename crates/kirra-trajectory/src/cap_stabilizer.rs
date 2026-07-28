//! **Speed-cap stabilization, inside the checker (#1212) — pure, no ROS.**
//!
//! ## Why this moved
//!
//! This state machine already existed, in `ros2_ws/.../perception_cap.py` — Python,
//! in the doer-side ROS layer, outside the safety authority. That is a worse
//! position than absent, because it *looks* done: a reader of the ROS tree sees cap
//! stabilization and concludes the behaviour is enforced. It was not enforced, it
//! was **proposed**, by a component the architecture explicitly does not trust for
//! safety, and nothing stopped a doer-side edit from relaxing it.
//!
//! The behaviour is ported faithfully — the asymmetry, the epsilon, the streak, the
//! rate limit and every fail-closed arm are the Python's, so this is a change of
//! *authority*, not of policy. Three things the Python never had are added below,
//! each called out where it appears.
//!
//! ## The asymmetry is the whole design
//!
//! **Down is immediate. Up is earned.**
//!
//! A tightening cap applies on the frame it is observed, with no smoothing, ever.
//! Smoothing a restriction means driving at a speed the world no longer supports for
//! as long as the filter takes to catch up — the filter would be inventing clearance
//! that perception just withdrew.
//!
//! A loosening cap has to be earned twice over: a streak of consecutive clear frames
//! before the cap may rise at all, and then a bounded climb rather than a step. One
//! optimistic frame is not evidence the hazard is gone; it is evidence of one frame.
//!
//! That is the same shape as the Degraded decel-to-stop doctrine (SS-002) and the
//! sensor watchdog's recovery streak: the system may always become more conservative
//! instantly, and may only become less conservative on accumulated evidence.
//!
//! ## What is new here, beyond the port
//!
//! 1. **Limiting-object persistence.** A confirmed hazard absent from exactly one
//!    scan is retained rather than treated as departed. A single missed detection is
//!    an ordinary sensor dropout — the coasting lifecycle in `kirra-taj` models the
//!    same thing — while two consecutive misses is evidence the object actually
//!    left. Retaining forever would deadlock the cap at its floor; retaining for
//!    zero frames lets a flickering detection release it. One frame is the smallest
//!    grace that distinguishes a dropout from a departure.
//!
//! 2. **Enter/exit hysteresis on the release side** ([`release_margin_mps`]). The
//!    Python's `restriction_epsilon` is jitter tolerance on the way *down*; it does
//!    nothing to stop an object hovering exactly at the geometric boundary from
//!    toggling the cap. A frame only counts as clear if the raw cap exceeds the
//!    standing stabilized cap by this margin, so the release threshold sits strictly
//!    above the engage threshold and a boundary-straddling object cannot oscillate
//!    the output.
//!
//! 3. **The limiting object's identity travels with the decision**, so a transition
//!    can be logged as "cap held at 0.8 by object 41" rather than an unattributable
//!    number.
//!
//! ## WCET
//!
//! This is perception-path only and **must never enter the per-pose
//! `validate_vehicle_command` hot path** — nothing in `validation.rs` calls it, and
//! the Nominal per-pose path is byte-identical. The work here is O(1) per scan, at
//! scan rate, not per trajectory pose.
//!
//! [`release_margin_mps`]: CapStabilizerConfig::release_margin_mps

/// Default bounded climb rate once a release has been earned (m/s per second).
pub const DEFAULT_RISE_RATE_MPS_PER_S: f64 = 0.50;
/// Default jitter tolerance: a raw-cap decrease smaller than this clamps the output
/// but does not count as a fresh restriction event.
pub const DEFAULT_RESTRICTION_EPSILON_MPS: f64 = 0.03;
/// Default consecutive clear frames required before the cap may rise at all.
pub const DEFAULT_CLEAR_CONFIRMATIONS: u32 = 5;
/// Default inter-frame gap beyond which continuity is lost and the cap fails closed.
pub const DEFAULT_MAX_DT_MS: u64 = 250;
/// Default release hysteresis margin — see the module docs, item 2.
pub const DEFAULT_RELEASE_MARGIN_MPS: f64 = 0.05;
/// Scans a previously-limiting object may be absent before it counts as departed.
/// One: enough to ride out a dropout, not enough to ride out a departure.
pub const LIMITING_OBJECT_GRACE_FRAMES: u32 = 1;

/// Why a configuration was refused. Refused, never clamped — a silently corrected
/// threshold leaves the operator believing a bound is enforced that is not the one
/// they wrote.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CapConfigError {
    /// `rise_rate_mps_per_s` not finite, or not strictly positive.
    RiseRateNotPositive { rate: f64 },
    /// `restriction_epsilon_mps` not finite, or negative.
    EpsilonNegative { epsilon: f64 },
    /// `release_margin_mps` not finite, or negative.
    ReleaseMarginNegative { margin: f64 },
    /// `clear_confirmations` of zero — a release would need no evidence at all.
    ClearConfirmationsZero,
    /// `max_dt_ms` of zero — every frame would read as a continuity break.
    MaxDtZero,
    /// `initial_cap_mps` not finite, or negative.
    InitialCapNegative { cap: f64 },
}

impl std::fmt::Display for CapConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RiseRateNotPositive { rate } => {
                write!(f, "rise_rate_mps_per_s {rate} must be finite and positive")
            }
            Self::EpsilonNegative { epsilon } => write!(
                f,
                "restriction_epsilon_mps {epsilon} must be finite and non-negative"
            ),
            Self::ReleaseMarginNegative { margin } => write!(
                f,
                "release_margin_mps {margin} must be finite and non-negative"
            ),
            Self::ClearConfirmationsZero => write!(
                f,
                "clear_confirmations of 0 would release the cap on no evidence"
            ),
            Self::MaxDtZero => {
                write!(f, "max_dt_ms of 0 would treat every frame as a gap")
            }
            Self::InitialCapNegative { cap } => {
                write!(f, "initial_cap_mps {cap} must be finite and non-negative")
            }
        }
    }
}

impl std::error::Error for CapConfigError {}

/// Stabilizer thresholds. Public fields; validated in [`CapStabilizer::new`], which
/// is the single point where a configuration becomes enforcement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapStabilizerConfig {
    /// Bounded climb rate once a release is earned (m/s per second).
    pub rise_rate_mps_per_s: f64,
    /// Raw-cap decreases smaller than this clamp the output without counting as a
    /// fresh restriction (sensor jitter, not a new hazard).
    pub restriction_epsilon_mps: f64,
    /// Consecutive clear frames required before any rise.
    pub clear_confirmations: u32,
    /// Inter-frame gap beyond which continuity is lost → fail closed.
    pub max_dt_ms: u64,
    /// Cap before any observation. Zero: start stopped, earn motion.
    pub initial_cap_mps: f64,
    /// Release hysteresis — a frame counts as clear only if the raw cap exceeds the
    /// standing stabilized cap by at least this much.
    pub release_margin_mps: f64,
}

impl Default for CapStabilizerConfig {
    fn default() -> Self {
        Self {
            rise_rate_mps_per_s: DEFAULT_RISE_RATE_MPS_PER_S,
            restriction_epsilon_mps: DEFAULT_RESTRICTION_EPSILON_MPS,
            clear_confirmations: DEFAULT_CLEAR_CONFIRMATIONS,
            max_dt_ms: DEFAULT_MAX_DT_MS,
            initial_cap_mps: 0.0,
            release_margin_mps: DEFAULT_RELEASE_MARGIN_MPS,
        }
    }
}

impl CapStabilizerConfig {
    /// Check that every threshold can be enforced as written.
    ///
    /// # Errors
    ///
    /// Returns [`CapConfigError`] for a non-positive rise rate, a negative epsilon
    /// or margin, a zero confirmation count, a zero gap budget, or a negative
    /// initial cap.
    pub fn validate(&self) -> Result<(), CapConfigError> {
        if !self.rise_rate_mps_per_s.is_finite() || self.rise_rate_mps_per_s <= 0.0 {
            return Err(CapConfigError::RiseRateNotPositive {
                rate: self.rise_rate_mps_per_s,
            });
        }
        if !self.restriction_epsilon_mps.is_finite() || self.restriction_epsilon_mps < 0.0 {
            return Err(CapConfigError::EpsilonNegative {
                epsilon: self.restriction_epsilon_mps,
            });
        }
        if !self.release_margin_mps.is_finite() || self.release_margin_mps < 0.0 {
            return Err(CapConfigError::ReleaseMarginNegative {
                margin: self.release_margin_mps,
            });
        }
        if self.clear_confirmations == 0 {
            return Err(CapConfigError::ClearConfirmationsZero);
        }
        if self.max_dt_ms == 0 {
            return Err(CapConfigError::MaxDtZero);
        }
        if !self.initial_cap_mps.is_finite() || self.initial_cap_mps < 0.0 {
            return Err(CapConfigError::InitialCapNegative {
                cap: self.initial_cap_mps,
            });
        }
        Ok(())
    }
}

/// Why the stabilizer produced this output. Distinct codes, not a collapsed
/// "capped" — an operator reading a log needs to tell a hazard from a clock fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapReason {
    /// First observation: holding the initial floor while continuity is established.
    InitialHold,
    /// The raw cap tightened. Applied immediately.
    Restricted,
    /// Clear frames are accumulating but the streak is not yet satisfied.
    Confirming,
    /// Streak satisfied; climbing at the bounded rate toward the raw cap.
    Rising,
    /// Output equals the raw cap; nothing to do.
    Pass,
    /// The previously-limiting object was absent this scan but is inside its grace
    /// window, so it is retained and the cap does not rise.
    LimitingObjectRetained,
    /// Raw cap non-finite or negative → floor.
    Invalid,
    /// Monotonic clock went backwards → floor.
    TimeBackward,
    /// Inter-frame gap exceeded the budget; continuity lost → floor.
    TimeGap,
}

impl CapReason {
    /// Stable machine code for logs and metrics.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InitialHold => "CAP_INITIAL_HOLD",
            Self::Restricted => "CAP_RESTRICTED",
            Self::Confirming => "CAP_CONFIRMING",
            Self::Rising => "CAP_RISING",
            Self::Pass => "CAP_PASS",
            Self::LimitingObjectRetained => "CAP_LIMITING_OBJECT_RETAINED",
            Self::Invalid => "CAP_INVALID",
            Self::TimeBackward => "CAP_TIME_BACKWARD",
            Self::TimeGap => "CAP_TIME_GAP",
        }
    }

    /// Whether this outcome floored the cap on a fault rather than on geometry.
    #[must_use]
    pub fn is_fault(&self) -> bool {
        matches!(self, Self::Invalid | Self::TimeBackward | Self::TimeGap)
    }
}

/// One observation handed to the stabilizer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapObservation {
    /// The raw assured-clear-distance cap perception computed this scan (m/s).
    pub raw_cap_mps: f64,
    /// Identity of the object that bound the raw cap, when one did. `None` means
    /// nothing limited this frame — an open corridor — NOT "unknown".
    pub limiting_object: Option<u64>,
    /// Monotonic timestamp (ms). Monotonic, not wall clock: a clock step must not
    /// release the cap.
    pub now_ms: u64,
}

/// The stabilizer's decision for one scan. Carries both caps so a consumer can log
/// what perception said *and* what was enforced — they are different numbers and
/// conflating them is how the Python version's behaviour became invisible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapDecision {
    /// What perception proposed this scan.
    pub raw_cap_mps: f64,
    /// What the checker enforces. Never above `raw_cap_mps`.
    pub stabilized_cap_mps: f64,
    pub reason: CapReason,
    pub clear_streak: u32,
    /// The object currently holding the cap down, including one retained through a
    /// dropout.
    pub limiting_object: Option<u64>,
}

/// Asymmetric temporal stabilizer for the raw ACD speed cap.
///
/// Pure and deterministic: the caller supplies every clock reading.
#[derive(Debug, Clone)]
pub struct CapStabilizer {
    cfg: CapStabilizerConfig,
    stabilized_mps: f64,
    clear_streak: u32,
    last_now_ms: Option<u64>,
    last_raw_mps: Option<f64>,
    /// The object holding the cap down, and how many consecutive scans it has been
    /// missing. `misses` only advances while the object is absent.
    limiting: Option<(u64, u32)>,
}

impl CapStabilizer {
    /// Build a stabilizer.
    ///
    /// # Errors
    ///
    /// Returns [`CapConfigError`] when `cfg` fails [`CapStabilizerConfig::validate`].
    pub fn new(cfg: CapStabilizerConfig) -> Result<Self, CapConfigError> {
        cfg.validate()?;
        Ok(Self {
            stabilized_mps: cfg.initial_cap_mps,
            cfg,
            clear_streak: 0,
            last_now_ms: None,
            last_raw_mps: None,
            limiting: None,
        })
    }

    /// The cap currently enforced.
    #[must_use]
    pub fn stabilized_cap_mps(&self) -> f64 {
        self.stabilized_mps
    }

    /// Consecutive clear frames accumulated toward a release.
    #[must_use]
    pub fn clear_streak(&self) -> u32 {
        self.clear_streak
    }

    /// Return to the starting state: the configured initial cap, no accumulated
    /// evidence, no continuity, no limiting object.
    ///
    /// Mirrors the Python's `reset()`. A restart must not inherit a cap that was
    /// earned against observations this instance can no longer see — the evidence
    /// and the clock history are gone, so the permission they bought goes with them.
    ///
    /// **Hazard note on `initial_cap_mps`.** With the default configuration this
    /// lands on the fail-closed floor (0.0): start stopped, earn motion. The field
    /// is configurable because the port is faithful, but any non-zero value means a
    /// fresh or reset stabilizer permits motion having observed *nothing* — which
    /// contradicts every other decision in this module. Treat a non-zero initial cap
    /// as a deliberate, reviewed exception, not a tuning knob.
    pub fn reset(&mut self) {
        self.stabilized_mps = self.cfg.initial_cap_mps;
        self.clear_streak = 0;
        self.last_now_ms = None;
        self.last_raw_mps = None;
        self.limiting = None;
    }

    /// Drop to the floor and forget continuity. Used by every fault arm: a
    /// stabilizer that cannot trust its inputs must not keep a standing cap that
    /// was derived from them.
    fn fail_closed(&mut self, raw: f64, reason: CapReason, now_ms: Option<u64>) -> CapDecision {
        self.stabilized_mps = 0.0;
        self.clear_streak = 0;
        self.last_now_ms = now_ms;
        self.last_raw_mps = None;
        self.limiting = None;
        CapDecision {
            raw_cap_mps: raw,
            stabilized_cap_mps: 0.0,
            reason,
            clear_streak: 0,
            limiting_object: None,
        }
    }

    /// Track the limiting object across a scan, applying the one-frame grace.
    ///
    /// Returns `true` when a previously-limiting object is being RETAINED through a
    /// dropout — the caller must then refuse to raise the cap, because the hazard
    /// that set it has not been shown to have left.
    fn track_limiting(&mut self, observed: Option<u64>) -> bool {
        match (self.limiting, observed) {
            // Same object still present, or a new object takes over: reset misses.
            (_, Some(id)) => {
                self.limiting = Some((id, 0));
                false
            }
            // Nothing limiting now, but something was.
            (Some((id, misses)), None) => {
                let misses = misses + 1;
                if misses <= LIMITING_OBJECT_GRACE_FRAMES {
                    // Inside the grace window — a dropout, not a departure.
                    self.limiting = Some((id, misses));
                    true
                } else {
                    // Two consecutive misses: it has actually left.
                    self.limiting = None;
                    false
                }
            }
            (None, None) => false,
        }
    }

    /// Apply one observation.
    ///
    /// The order below is the safety argument in sequence: reject what cannot be
    /// trusted, then apply any restriction immediately, then — and only then —
    /// consider whether a release has been earned.
    #[must_use]
    pub fn observe(&mut self, obs: CapObservation) -> CapDecision {
        let raw = obs.raw_cap_mps;

        if !raw.is_finite() || raw < 0.0 {
            return self.fail_closed(raw, CapReason::Invalid, Some(obs.now_ms));
        }

        let Some(last_now) = self.last_now_ms else {
            // First observation. Establish continuity; do not release anything on
            // the strength of a single frame.
            self.last_now_ms = Some(obs.now_ms);
            self.last_raw_mps = Some(raw);
            self.track_limiting(obs.limiting_object);

            if raw < self.stabilized_mps {
                self.stabilized_mps = raw;
                return self.decision(raw, CapReason::Restricted, 0);
            }
            self.clear_streak = 1;
            return self.decision(raw, CapReason::InitialHold, 1);
        };

        if obs.now_ms < last_now {
            return self.fail_closed(raw, CapReason::TimeBackward, Some(obs.now_ms));
        }
        let dt_ms = obs.now_ms - last_now;
        if dt_ms > self.cfg.max_dt_ms {
            return self.fail_closed(raw, CapReason::TimeGap, Some(obs.now_ms));
        }

        let prev_raw = self.last_raw_mps;
        self.last_now_ms = Some(obs.now_ms);
        self.last_raw_mps = Some(raw);
        let retained = self.track_limiting(obs.limiting_object);

        // A drop in the RAW envelope is restrictive even when the stabilized output
        // already sits below the new raw cap — perception withdrew clearance, and
        // that resets the evidence for any pending release.
        let raw_tightened =
            prev_raw.is_some_and(|prev| raw < prev - self.cfg.restriction_epsilon_mps);
        if raw_tightened {
            self.stabilized_mps = self.stabilized_mps.min(raw);
            self.clear_streak = 0;
            return self.decision(raw, CapReason::Restricted, 0);
        }

        // The output may never exceed the newest raw bound. A decrease inside the
        // jitter epsilon clamps immediately but does not destroy streak history —
        // otherwise ordinary sensor noise would make a release unreachable.
        if raw < self.stabilized_mps {
            let drop = self.stabilized_mps - raw;
            self.stabilized_mps = raw;
            if drop > self.cfg.restriction_epsilon_mps {
                self.clear_streak = 0;
                return self.decision(raw, CapReason::Restricted, 0);
            }
            let streak = self.clear_streak;
            return self.decision(raw, CapReason::Pass, streak);
        }

        // From here the raw cap is at or above the stabilized output, so a release
        // is conceivable. Everything below decides whether it has been EARNED.

        if retained {
            // The hazard that set this cap is missing from exactly one scan. That is
            // a dropout, not a departure; hold, and do not accrue clear evidence.
            self.clear_streak = 0;
            return self.decision(raw, CapReason::LimitingObjectRetained, 0);
        }

        // Enter/exit hysteresis: a frame is EVIDENCE OF CLEARANCE only if it clears
        // the standing cap by a margin. Without this an object hovering exactly at
        // the geometric boundary alternates clear/limiting and walks the cap up and
        // down.
        //
        // The margin gates the EVIDENCE, not the climb. Resetting the streak here
        // unconditionally would strand the cap permanently up to `release_margin`
        // below the raw value: as the climb closed in, frames would stop counting as
        // clear and the rise would stall just short, so `stabilized == raw` became
        // unreachable and a robot in an open corridor would sit forever below the
        // cap perception actually allows. Once a release has been EARNED, the climb
        // finishes.
        let clear_enough = raw > self.stabilized_mps + self.cfg.release_margin_mps;
        let release_earned = self.clear_streak >= self.cfg.clear_confirmations;

        if clear_enough {
            self.clear_streak += 1;
        } else if !release_earned {
            self.clear_streak = 0;
            return self.decision(raw, CapReason::Pass, 0);
        }
        // else: an already-earned release is finishing its final approach, where the
        // remaining gap is smaller than the margin. Letting it finish is what stops
        // the cap stranding permanently just below the raw value.

        if self.clear_streak < self.cfg.clear_confirmations {
            let streak = self.clear_streak;
            return self.decision(raw, CapReason::Confirming, streak);
        }

        // Earned. Climb at the bounded rate — never a step, even now.
        let max_rise = self.cfg.rise_rate_mps_per_s * (dt_ms as f64) / 1000.0;
        self.stabilized_mps = raw.min(self.stabilized_mps + max_rise);

        if self.stabilized_mps >= raw {
            // Caught up: this release is COMPLETE, so the evidence that earned it is
            // spent. Any further rise must clear the margin again from scratch —
            // otherwise a single historic release would license every later one, and
            // a boundary-straddling object could walk the cap up indefinitely.
            self.clear_streak = 0;
            return self.decision(raw, CapReason::Pass, 0);
        }
        let streak = self.clear_streak;
        self.decision(raw, CapReason::Rising, streak)
    }

    fn decision(&self, raw: f64, reason: CapReason, streak: u32) -> CapDecision {
        CapDecision {
            raw_cap_mps: raw,
            stabilized_cap_mps: self.stabilized_mps,
            reason,
            clear_streak: streak,
            limiting_object: self.limiting.map(|(id, _)| id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stab() -> CapStabilizer {
        CapStabilizer::new(CapStabilizerConfig::default()).expect("defaults are valid")
    }

    fn obs(raw: f64, now_ms: u64) -> CapObservation {
        CapObservation {
            raw_cap_mps: raw,
            limiting_object: None,
            now_ms,
        }
    }

    fn obs_with(raw: f64, now_ms: u64, limiting: Option<u64>) -> CapObservation {
        CapObservation {
            raw_cap_mps: raw,
            limiting_object: limiting,
            now_ms,
        }
    }

    /// Drive clear frames at 10 Hz until the stabilized output has fully caught up
    /// to `cap`, returning the timestamp of the last frame.
    ///
    /// Needed by most tests below because the stabilizer **starts stopped and earns
    /// motion** — the initial cap is 0.0 and a single permissive frame does not
    /// establish anything. Assuming otherwise is what made the first draft of these
    /// tests wrong.
    fn settle_at(s: &mut CapStabilizer, cap: f64, mut t: u64) -> u64 {
        for _ in 0..500 {
            t += 100;
            let d = s.observe(obs(cap, t));
            if (d.stabilized_cap_mps - cap).abs() < 1e-9 {
                return t;
            }
        }
        panic!("never settled at {cap}");
    }

    // ----- the asymmetry -----

    /// The core doctrine: a tightening cap applies on the frame it is observed.
    /// Smoothing a restriction would mean driving at a speed the world no longer
    /// supports for as long as the filter takes to catch up.
    #[test]
    fn a_restriction_applies_immediately_and_is_never_smoothed() {
        let mut s = stab();
        // Earn a real cap first, or "it dropped to 0.2" proves nothing.
        let t = settle_at(&mut s, 1.5, 0);
        assert!((s.stabilized_cap_mps() - 1.5).abs() < 1e-9);

        let d = s.observe(obs(0.2, t + 100));
        assert_eq!(d.reason, CapReason::Restricted);
        assert_eq!(
            d.stabilized_cap_mps, 0.2,
            "the tightened cap must apply in full on this frame"
        );
        assert_eq!(
            d.clear_streak, 0,
            "a restriction destroys pending release evidence"
        );
    }

    /// …and up is earned: one optimistic frame is evidence of one frame, not of a
    /// departed hazard.
    #[test]
    fn a_single_clear_frame_does_not_release_the_cap() {
        let mut s = stab();
        let _ = s.observe(obs(0.0, 0));
        let d = s.observe(obs(2.0, 100));
        assert_eq!(d.reason, CapReason::Confirming);
        assert_eq!(
            d.stabilized_cap_mps, 0.0,
            "no rise on the first clear frame"
        );
    }

    /// The streak must be satisfied before ANY rise, and the rise is then bounded
    /// rather than a step.
    #[test]
    fn release_requires_the_streak_then_climbs_at_the_bounded_rate() {
        let cfg = CapStabilizerConfig::default();
        let mut s = stab();
        let d0 = s.observe(obs(2.0, 0));
        assert_eq!(d0.reason, CapReason::InitialHold);
        assert_eq!(
            d0.stabilized_cap_mps, 0.0,
            "one permissive frame releases nothing"
        );

        let mut t = 0;
        let mut first_rise = None;
        for _ in 0..10 {
            t += 100;
            let d = s.observe(obs(2.0, t));
            if d.reason == CapReason::Rising {
                first_rise = Some(d);
                break;
            }
            assert_eq!(d.reason, CapReason::Confirming);
            assert_eq!(
                d.stabilized_cap_mps, 0.0,
                "the cap must not move before the streak is satisfied"
            );
        }

        let d = first_rise.expect("a continuously clear stream must eventually rise");
        assert_eq!(
            d.clear_streak, cfg.clear_confirmations,
            "the rise begins exactly when the streak is satisfied"
        );
        let expected = cfg.rise_rate_mps_per_s * 0.1;
        assert!(
            (d.stabilized_cap_mps - expected).abs() < 1e-9,
            "one 100 ms step must rise exactly {expected}, got {}",
            d.stabilized_cap_mps
        );
        assert!(
            d.stabilized_cap_mps < 2.0,
            "the cap must not step straight to the raw value"
        );
    }

    // ----- THE gate test the issue asks for -----

    /// **Alternating hazard/clear frames at sensor rate cannot produce full-speed
    /// oscillation.** This is the acceptance criterion stated as an envelope
    /// assertion, not a "was it called" check.
    ///
    /// An object flickering in and out of the corridor at 10 Hz is the classic way a
    /// naive filter produces a robot that surges and brakes. Every restrictive frame
    /// slams the cap back to the hazard value, and the clear frames in between can
    /// never accumulate a streak, so the enforced cap stays pinned near the hazard.
    #[test]
    fn alternating_hazard_and_clear_frames_cannot_oscillate_the_cap() {
        let mut s = stab();
        let _ = s.observe(obs(0.4, 0));

        let mut t = 0;
        let mut peak: f64 = 0.0;
        for i in 0..200 {
            t += 100;
            let raw = if i % 2 == 0 { 0.4 } else { 2.5 };
            let d = s.observe(obs(raw, t));
            peak = peak.max(d.stabilized_cap_mps);
        }

        assert!(
            peak <= 0.4 + 1e-9,
            "100 clear frames interleaved with 100 hazard frames must not walk the \
             cap above the hazard bound; peaked at {peak}"
        );
    }

    /// Non-vacuity for the gate above: the same stream WITHOUT the hazard frames
    /// does rise, so the assertion is bounding real behaviour rather than a
    /// stabilizer that never rises at all.
    #[test]
    fn the_oscillation_gate_is_not_vacuous() {
        let mut s = stab();
        let _ = s.observe(obs(0.4, 0));
        let mut t = 0;
        let mut peak: f64 = 0.0;
        for _ in 0..200 {
            t += 100;
            let d = s.observe(obs(2.5, t));
            peak = peak.max(d.stabilized_cap_mps);
        }
        assert!(
            peak > 0.4 + 1e-9,
            "a continuously clear stream must eventually rise, else the gate test \
             above proves nothing; peaked at {peak}"
        );
    }

    // ----- limiting-object persistence -----

    /// A confirmed hazard absent from exactly one scan is retained. A single missed
    /// detection is an ordinary dropout, and releasing the cap on it would mean
    /// accelerating toward something perception merely blinked at.
    #[test]
    fn a_hazard_missing_from_one_scan_is_retained() {
        let mut s = stab();
        let t = settle_at(&mut s, 0.5, 0);
        // Re-assert the limiting object on the settled frame, then drop it.
        let _ = s.observe(obs_with(0.5, t + 100, Some(41)));
        let d = s.observe(obs_with(2.0, t + 200, None));
        assert_eq!(d.reason, CapReason::LimitingObjectRetained);
        assert_eq!(
            d.limiting_object,
            Some(41),
            "identity travels with the decision"
        );
        assert_eq!(d.clear_streak, 0, "a dropout accrues no clear evidence");
        assert!(
            (d.stabilized_cap_mps - 0.5).abs() < 1e-9,
            "the cap does not rise on a dropout, got {}",
            d.stabilized_cap_mps
        );
    }

    /// Two consecutive misses is a departure, not a dropout — the object is dropped
    /// and clear frames start accruing again. Retaining forever would deadlock the
    /// cap at its floor.
    #[test]
    fn a_hazard_missing_from_two_scans_has_departed() {
        let mut s = stab();
        let t = settle_at(&mut s, 0.5, 0);
        let _ = s.observe(obs_with(0.5, t + 100, Some(41)));
        assert_eq!(
            s.observe(obs_with(2.0, t + 200, None)).reason,
            CapReason::LimitingObjectRetained
        );
        let d = s.observe(obs_with(2.0, t + 300, None));
        assert_ne!(d.reason, CapReason::LimitingObjectRetained);
        assert_eq!(
            d.limiting_object, None,
            "the object is released after 2 misses"
        );
        assert_eq!(d.clear_streak, 1, "clear evidence resumes accruing");
    }

    /// A different object taking over resets the miss count — the cap is now held by
    /// a hazard that IS present, so no grace applies.
    #[test]
    fn a_new_limiting_object_takes_over_cleanly() {
        let mut s = stab();
        let t = settle_at(&mut s, 0.5, 0);
        let _ = s.observe(obs_with(0.5, t + 100, Some(41)));
        let d = s.observe(obs_with(0.5, t + 200, Some(77)));
        assert_eq!(d.limiting_object, Some(77));
        assert_ne!(d.reason, CapReason::LimitingObjectRetained);
    }

    // ----- hysteresis -----

    /// An object hovering exactly at the geometric boundary must not walk the cap.
    /// A raw cap barely above the standing cap does not count as a clear frame.
    #[test]
    fn a_boundary_straddling_reading_does_not_accrue_clear_evidence() {
        let mut s = stab();
        let mut t = settle_at(&mut s, 1.0, 0);
        // Raw sits just inside the release margin — not clear enough to count.
        let margin = CapStabilizerConfig::default().release_margin_mps;
        for _ in 0..20 {
            t += 100;
            let d = s.observe(obs(1.0 + margin * 0.5, t));
            assert_eq!(
                d.clear_streak, 0,
                "a reading inside the release margin is not evidence of clearance"
            );
        }
        assert!(s.stabilized_cap_mps() <= 1.0 + 1e-9);
    }

    /// …and clearing the margin does count, so the hysteresis is a threshold rather
    /// than a wall.
    #[test]
    fn clearing_the_release_margin_does_accrue_evidence() {
        let mut s = stab();
        let t = settle_at(&mut s, 1.0, 0);
        let margin = CapStabilizerConfig::default().release_margin_mps;
        let d = s.observe(obs(1.0 + margin * 2.0, t + 100));
        assert_eq!(d.clear_streak, 1);
        assert_eq!(d.reason, CapReason::Confirming);
    }

    /// The cap must reach the raw value EXACTLY, not stall just below it.
    ///
    /// Regression test for a bug in this module's first draft: the release margin
    /// gated the climb as well as the evidence, so as the cap closed in, frames
    /// stopped counting as clear and the rise stalled up to `release_margin` short.
    /// A robot in an open corridor would have sat forever below the speed perception
    /// actually allowed — a permanent, invisible availability loss.
    #[test]
    fn the_cap_reaches_the_raw_value_exactly_and_does_not_strand() {
        let mut s = stab();
        let mut t = 0;
        for _ in 0..200 {
            t += 100;
            let _ = s.observe(obs(1.0, t));
        }
        assert_eq!(
            s.stabilized_cap_mps(),
            1.0,
            "a continuously clear stream must converge exactly onto the raw cap"
        );
    }

    /// …and the evidence that earned a release is SPENT when the climb completes.
    ///
    /// The counterpart bug to the one above: carrying the streak past completion
    /// meant one historic release licensed every later one, so a boundary-straddling
    /// object could walk the cap up indefinitely without ever clearing the margin.
    #[test]
    fn a_completed_release_spends_its_evidence() {
        let mut s = stab();
        let t = settle_at(&mut s, 1.0, 0);
        assert_eq!(
            s.clear_streak(),
            0,
            "reaching the raw cap consumes the streak that earned the climb"
        );

        // A nudge inside the margin must now be refused a rise, exactly as if no
        // release had ever happened.
        let margin = CapStabilizerConfig::default().release_margin_mps;
        let d = s.observe(obs(1.0 + margin * 0.5, t + 100));
        assert_eq!(d.clear_streak, 0);
        assert!(
            (d.stabilized_cap_mps - 1.0).abs() < 1e-12,
            "a within-margin nudge must not move a settled cap"
        );
    }

    // ----- restart / reset -----

    /// A fresh stabilizer begins stopped, having observed nothing. Motion is earned,
    /// never assumed.
    #[test]
    fn a_fresh_stabilizer_begins_at_the_fail_closed_floor() {
        let s = stab();
        assert_eq!(s.stabilized_cap_mps(), 0.0);
        assert_eq!(s.clear_streak(), 0);
    }

    /// …and a reset returns to exactly that state, discarding a cap that had been
    /// earned. The evidence and the clock history are gone, so the permission they
    /// bought goes with them — a restart must not inherit clearance it can no longer
    /// justify.
    #[test]
    fn a_reset_discards_an_earned_cap_and_its_evidence() {
        let mut s = stab();
        let t = settle_at(&mut s, 1.5, 0);
        assert!(
            s.stabilized_cap_mps() > 0.0,
            "precondition: a cap was earned"
        );

        s.reset();
        assert_eq!(
            s.stabilized_cap_mps(),
            0.0,
            "the earned cap does not survive"
        );
        assert_eq!(s.clear_streak(), 0);

        // Continuity is gone too: the next frame is a first observation again, so it
        // cannot release anything on its own.
        let d = s.observe(obs(1.5, t + 100));
        assert_eq!(d.reason, CapReason::InitialHold);
        assert_eq!(d.stabilized_cap_mps, 0.0);
        assert_eq!(d.limiting_object, None);
    }

    /// A reset clears a retained limiting object as well — the identity was evidence
    /// about a hazard, and it does not outlive the state it belonged to.
    #[test]
    fn a_reset_clears_the_limiting_object() {
        let mut s = stab();
        let t = settle_at(&mut s, 0.5, 0);
        let d = s.observe(obs_with(0.5, t + 100, Some(41)));
        assert_eq!(d.limiting_object, Some(41));

        s.reset();
        let d = s.observe(obs(2.0, t + 200));
        assert_eq!(d.limiting_object, None);
    }

    // ----- limiting-object identity is absent unless something limits -----

    /// Identity is absent when nothing limits — not a stale id, not a placeholder.
    /// A consumer logging "held by object N" must be able to trust that N is real.
    #[test]
    fn identity_is_absent_when_nothing_limits() {
        let mut s = stab();
        let mut t = 0;
        for _ in 0..30 {
            t += 100;
            let d = s.observe(obs_with(2.0, t, None));
            assert_eq!(
                d.limiting_object, None,
                "an unlimited stream must never report a limiting object"
            );
        }
    }

    /// A fail-closed arm clears the identity too. The stabilizer no longer trusts
    /// its inputs, so it must not keep asserting which object was responsible.
    #[test]
    fn a_fail_closed_arm_clears_the_limiting_object() {
        let mut s = stab();
        let t = settle_at(&mut s, 0.5, 0);
        let d = s.observe(obs_with(0.5, t + 100, Some(41)));
        assert_eq!(
            d.limiting_object,
            Some(41),
            "precondition: an object is limiting"
        );

        let d = s.observe(obs_with(f64::NAN, t + 200, Some(41)));
        assert_eq!(d.reason, CapReason::Invalid);
        assert_eq!(
            d.limiting_object, None,
            "a stabilizer that distrusts its input must not attribute the refusal              to an object"
        );
    }

    /// Negative time is unrepresentable rather than merely rejected: `now_ms` is a
    /// `u64`, so the type forbids what the Python had to validate at runtime. This
    /// test documents that the guarantee is structural.
    #[test]
    fn negative_time_is_structurally_impossible() {
        let obs = CapObservation {
            raw_cap_mps: 1.0,
            limiting_object: None,
            now_ms: u64::MIN,
        };
        assert_eq!(
            obs.now_ms, 0,
            "the clock type has no negative values to reject"
        );
    }

    // ----- fail-closed arms -----

    /// A clock step backwards must not release the cap. Recovery is keyed on
    /// monotonic time precisely so a wall-clock correction cannot look like progress.
    #[test]
    fn a_backwards_clock_fails_closed() {
        let mut s = stab();
        let _ = s.observe(obs(1.0, 1_000));
        let d = s.observe(obs(1.0, 900));
        assert_eq!(d.reason, CapReason::TimeBackward);
        assert_eq!(d.stabilized_cap_mps, 0.0);
    }

    /// A gap longer than the budget means continuity is lost — the standing cap was
    /// derived from observations that are no longer adjacent to this one.
    #[test]
    fn an_oversized_frame_gap_fails_closed() {
        let mut s = stab();
        let _ = s.observe(obs(1.0, 0));
        let d = s.observe(obs(1.0, 1_000));
        assert_eq!(d.reason, CapReason::TimeGap);
        assert_eq!(d.stabilized_cap_mps, 0.0);
    }

    #[test]
    fn a_malformed_raw_cap_fails_closed() {
        for bad in [f64::NAN, f64::INFINITY, -0.1] {
            let mut s = stab();
            let _ = s.observe(obs(1.0, 0));
            let d = s.observe(obs(bad, 100));
            assert_eq!(d.reason, CapReason::Invalid, "raw {bad}");
            assert_eq!(d.stabilized_cap_mps, 0.0);
        }
    }

    /// Jitter tolerance: a raw decrease smaller than the epsilon clamps the output
    /// but is NOT reported as a restriction. Sensor noise is not a hazard, and
    /// emitting `Restricted` for every ripple would bury real restrictions in a log
    /// full of noise.
    ///
    /// The streak is deliberately not asserted here. Once the cap has caught up to
    /// the raw value the streak is zero anyway — evidence only accrues while there
    /// is headroom to climb into — so a "streak preserved" claim would be vacuous.
    /// The load-bearing property is the reason code and the clamp.
    #[test]
    fn sub_epsilon_jitter_clamps_without_reporting_a_restriction() {
        let cfg = CapStabilizerConfig::default();
        let mut s = stab();
        let t = settle_at(&mut s, 1.0, 0);

        let dipped = 1.0 - cfg.restriction_epsilon_mps * 0.5;
        let d = s.observe(obs(dipped, t + 100));
        assert_eq!(
            d.reason,
            CapReason::Pass,
            "a sub-epsilon ripple is not a restriction event"
        );
        assert!(
            (d.stabilized_cap_mps - dipped).abs() < 1e-12,
            "…but the output still clamps to it immediately"
        );
    }

    /// A material drop resets the streak even when the output was already below it.
    #[test]
    fn a_material_raw_drop_resets_the_streak() {
        let mut s = stab();
        let _ = s.observe(obs(0.0, 0));
        let mut t = 0;
        for _ in 0..3 {
            t += 100;
            let _ = s.observe(obs(3.0, t));
        }
        assert!(s.clear_streak() > 0);
        t += 100;
        let d = s.observe(obs(1.0, t));
        assert_eq!(d.reason, CapReason::Restricted);
        assert_eq!(d.clear_streak, 0);
    }

    // -----------------------------------------------------------------------
    // Boundary pinning (mutation gate, WP-08)
    //
    // Every threshold here is a comparison whose off-by-one form is still plausible
    // code, and several of them decide whether the robot moves. Each gets an
    // exact-equality case rather than only a comfortably-past-it one.
    // -----------------------------------------------------------------------

    /// A raw cap of exactly zero is a LEGITIMATE reading — "stop", not "malformed".
    /// Treating it as invalid would still stop the robot, but would report a sensor
    /// fault every time perception correctly saw a blocked corridor.
    #[test]
    fn a_raw_cap_of_exactly_zero_is_valid_not_invalid() {
        let mut s = stab();
        let t = settle_at(&mut s, 1.0, 0);
        let d = s.observe(obs(0.0, t + 100));
        assert_eq!(
            d.reason,
            CapReason::Restricted,
            "zero is a hazard, not garbage"
        );
        assert_eq!(d.stabilized_cap_mps, 0.0);
        // …and a hair below zero IS malformed.
        let d = s.observe(obs(-1e-9, t + 200));
        assert_eq!(d.reason, CapReason::Invalid);
    }

    /// Two frames sharing a timestamp are not a backwards clock. Clock resolution
    /// coarser than the frame interval is ordinary; faulting on it would red every
    /// such deployment.
    #[test]
    fn equal_timestamps_are_not_a_backwards_clock() {
        let mut s = stab();
        let _ = s.observe(obs(1.0, 500));
        let d = s.observe(obs(1.0, 500));
        assert_ne!(d.reason, CapReason::TimeBackward);
        assert!(!d.reason.is_fault(), "dt of zero is not a fault");
    }

    /// A gap of exactly the budget is still inside it. The budget is a maximum
    /// tolerated gap, not a value to stay strictly under.
    #[test]
    fn a_gap_of_exactly_the_budget_is_tolerated() {
        let cfg = CapStabilizerConfig::default();
        let mut s = stab();
        let _ = s.observe(obs(1.0, 0));
        let d = s.observe(obs(1.0, cfg.max_dt_ms));
        assert_ne!(
            d.reason,
            CapReason::TimeGap,
            "exactly the budget is within it"
        );
        // …one millisecond past it is not.
        let mut s = stab();
        let _ = s.observe(obs(1.0, 0));
        let d = s.observe(obs(1.0, cfg.max_dt_ms + 1));
        assert_eq!(d.reason, CapReason::TimeGap);
    }

    /// A raw drop of exactly the epsilon is jitter, not a restriction — the epsilon
    /// is the largest movement still considered noise.
    ///
    /// Uses a binary-exact epsilon (0.25) rather than the 0.03 default, because
    /// "exactly epsilon" is not representable at the default: `1.0 - 0.03` rounds
    /// such that the difference comes back as `0.030000000000000027`, which is
    /// strictly greater than the threshold. That makes the real boundary fuzzy at
    /// the ulp level — harmless for a jitter threshold, but it means a test claiming
    /// to sit exactly on it has to choose values that actually can.
    #[test]
    fn a_raw_drop_of_exactly_the_epsilon_is_still_jitter() {
        let cfg = CapStabilizerConfig {
            restriction_epsilon_mps: 0.25,
            ..CapStabilizerConfig::default()
        };
        let mut s = CapStabilizer::new(cfg).expect("valid");
        let t = settle_at(&mut s, 1.0, 0);

        // 1.0 - 0.25 = 0.75, all exact; the drop is exactly the epsilon.
        let d = s.observe(obs(0.75, t + 100));
        assert_eq!(
            d.reason,
            CapReason::Pass,
            "a drop of exactly epsilon is the largest movement still called noise"
        );
        assert_eq!(
            d.stabilized_cap_mps, 0.75,
            "…and it still clamps immediately"
        );

        // Strictly more than epsilon is a restriction.
        let mut s = CapStabilizer::new(cfg).expect("valid");
        let t = settle_at(&mut s, 1.0, 0);
        let d = s.observe(obs(0.75 - 1e-6, t + 100));
        assert_eq!(d.reason, CapReason::Restricted);
    }

    /// A raw cap exactly equal to the standing cap is not a restriction, and is not
    /// evidence of clearance either — it is simply steady state.
    #[test]
    fn a_raw_cap_equal_to_the_standing_cap_is_steady_state() {
        let mut s = stab();
        let t = settle_at(&mut s, 1.0, 0);
        let d = s.observe(obs(1.0, t + 100));
        assert_eq!(d.reason, CapReason::Pass);
        assert_eq!(
            d.clear_streak, 0,
            "steady state accrues no release evidence"
        );
        assert_eq!(d.stabilized_cap_mps, 1.0);
    }

    /// A raw cap exactly at the release margin does NOT clear it. The margin is the
    /// distance a reading must EXCEED to count, so an object parked precisely on the
    /// threshold cannot start walking the cap up.
    #[test]
    fn a_raw_cap_exactly_at_the_release_margin_does_not_clear_it() {
        let cfg = CapStabilizerConfig::default();
        let mut s = stab();
        let mut t = settle_at(&mut s, 1.0, 0);
        for _ in 0..10 {
            t += 100;
            let d = s.observe(obs(1.0 + cfg.release_margin_mps, t));
            assert_eq!(
                d.clear_streak, 0,
                "sitting exactly on the margin is not clearing it"
            );
        }
        assert!((s.stabilized_cap_mps() - 1.0).abs() < 1e-12);
    }

    /// A raw cap falling back to EXACTLY the standing cap is not progress toward a
    /// release — it discards accumulated clear evidence rather than preserving it.
    ///
    /// Arriving at the cap you are already enforcing tells you nothing new about
    /// clearance. Preserving the streak here would let a stream that repeatedly
    /// approaches and retreats bank evidence it never earned.
    ///
    /// Needs a wide jitter epsilon to reach: with the default, the drop back to the
    /// standing cap reads as a restriction and short-circuits earlier. That is the
    /// only way raw can equal the standing cap while a streak is still live.
    #[test]
    fn falling_back_to_exactly_the_standing_cap_discards_clear_evidence() {
        let cfg = CapStabilizerConfig {
            restriction_epsilon_mps: 1.0,
            ..CapStabilizerConfig::default()
        };
        let mut s = CapStabilizer::new(cfg).expect("valid");

        let _ = s.observe(obs(0.6, 0));
        let building = s.observe(obs(0.6, 100));
        assert!(
            building.clear_streak > 0,
            "precondition: evidence is accumulating, else the assertion below is              vacuous"
        );
        assert_eq!(building.stabilized_cap_mps, 0.0);

        // Raw drops back to exactly the standing cap — no restriction (inside the
        // wide epsilon), but no clearance either.
        let d = s.observe(obs(0.0, 200));
        assert_eq!(
            d.clear_streak, 0,
            "reaching the cap already enforced is not evidence of clearance"
        );
    }

    /// The first observation treats a raw cap equal to the initial floor as a hold,
    /// not a restriction — there is nothing to restrict from.
    #[test]
    fn a_first_observation_equal_to_the_floor_holds() {
        let mut s = stab();
        let d = s.observe(obs(0.0, 0));
        assert_eq!(d.reason, CapReason::InitialHold);
        assert_eq!(d.stabilized_cap_mps, 0.0);
    }

    /// A zero epsilon and a zero release margin are both legitimate configurations —
    /// "tolerate no jitter" and "no hysteresis" are choices, not errors.
    #[test]
    fn zero_epsilon_and_zero_margin_are_valid_configurations() {
        let c = CapStabilizerConfig {
            restriction_epsilon_mps: 0.0,
            release_margin_mps: 0.0,
            ..CapStabilizerConfig::default()
        };
        assert_eq!(c.validate(), Ok(()));
        assert!(CapStabilizer::new(c).is_ok());
    }

    /// Every config refusal renders a distinct, non-empty message naming the
    /// offending value. A refusal whose reason never reaches the operator is a
    /// silent refusal.
    #[test]
    fn every_config_error_renders_a_distinct_message() {
        let errors = [
            CapConfigError::RiseRateNotPositive { rate: 0.0 },
            CapConfigError::EpsilonNegative { epsilon: -1.0 },
            CapConfigError::ReleaseMarginNegative { margin: -2.0 },
            CapConfigError::ClearConfirmationsZero,
            CapConfigError::MaxDtZero,
            CapConfigError::InitialCapNegative { cap: -3.0 },
        ];
        let rendered: Vec<String> = errors.iter().map(ToString::to_string).collect();
        for text in &rendered {
            assert!(!text.is_empty());
        }
        assert!(
            rendered[0].contains("rise_rate_mps_per_s"),
            "{}",
            rendered[0]
        );
        assert!(rendered[1].contains("-1"), "{}", rendered[1]);
        assert!(
            rendered[2].contains("release_margin_mps"),
            "{}",
            rendered[2]
        );
        assert!(rendered[3].contains("no evidence"), "{}", rendered[3]);
        assert!(rendered[4].contains("gap"), "{}", rendered[4]);
        assert!(rendered[5].contains("-3"), "{}", rendered[5]);

        let mut unique = rendered.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), rendered.len());
    }

    // ----- config -----

    #[test]
    fn the_defaults_are_valid() {
        assert_eq!(CapStabilizerConfig::default().validate(), Ok(()));
    }

    #[test]
    fn an_unenforceable_config_is_refused() {
        let bad = |f: fn(&mut CapStabilizerConfig)| {
            let mut c = CapStabilizerConfig::default();
            f(&mut c);
            c.validate()
        };
        assert!(matches!(
            bad(|c| c.rise_rate_mps_per_s = 0.0),
            Err(CapConfigError::RiseRateNotPositive { .. })
        ));
        assert!(matches!(
            bad(|c| c.restriction_epsilon_mps = -0.1),
            Err(CapConfigError::EpsilonNegative { .. })
        ));
        assert!(matches!(
            bad(|c| c.release_margin_mps = f64::NAN),
            Err(CapConfigError::ReleaseMarginNegative { .. })
        ));
        assert_eq!(
            bad(|c| c.clear_confirmations = 0),
            Err(CapConfigError::ClearConfirmationsZero)
        );
        assert_eq!(bad(|c| c.max_dt_ms = 0), Err(CapConfigError::MaxDtZero));
        assert!(matches!(
            bad(|c| c.initial_cap_mps = -1.0),
            Err(CapConfigError::InitialCapNegative { .. })
        ));
        // …and an unenforceable config cannot build a stabilizer.
        let c = CapStabilizerConfig {
            clear_confirmations: 0,
            ..CapStabilizerConfig::default()
        };
        assert!(CapStabilizer::new(c).is_err());
    }

    /// Every reason renders a distinct code — an operator who cannot tell a clock
    /// fault from a hazard cannot diagnose either.
    #[test]
    fn every_reason_code_is_distinct() {
        let codes = [
            CapReason::InitialHold.code(),
            CapReason::Restricted.code(),
            CapReason::Confirming.code(),
            CapReason::Rising.code(),
            CapReason::Pass.code(),
            CapReason::LimitingObjectRetained.code(),
            CapReason::Invalid.code(),
            CapReason::TimeBackward.code(),
            CapReason::TimeGap.code(),
        ];
        let mut seen = codes.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), codes.len());
        // Only the three input-fault arms report as faults.
        assert!(CapReason::TimeGap.is_fault());
        assert!(!CapReason::Restricted.is_fault());
    }

    /// The enforced cap never exceeds what perception proposed, under any sequence.
    /// This is the invariant the whole module exists to preserve.
    #[test]
    fn the_stabilized_cap_never_exceeds_the_raw_cap() {
        let mut s = stab();
        let mut t = 0;
        // A deliberately awkward stream: rises, dips, jitter, faults, recovery.
        let stream = [
            0.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.0, 1.02, 0.98, 3.0, 3.0, 0.1, 0.1, 5.0,
        ];
        for raw in stream {
            t += 100;
            let d = s.observe(obs(raw, t));
            assert!(
                d.stabilized_cap_mps <= d.raw_cap_mps + 1e-12,
                "stabilized {} exceeded raw {}",
                d.stabilized_cap_mps,
                d.raw_cap_mps
            );
        }
    }
}
