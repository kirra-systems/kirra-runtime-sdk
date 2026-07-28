//! **R2 sensor liveness watchdog (#1211) — pure, no ROS.**
//!
//! ## The failure this exists to close
//!
//! Every watchdog the stack had before this one detects **silence**:
//! `telemetry_watchdog.rs` (`AV_TELEMETRY_TIMEOUT_MS`), the armed-but-silent
//! channel resolvers ([`crate::vru_channel`], [`crate::occlusion_channel`]), and
//! the `KIRRA_SUBSCRIPTION_STALENESS_MS` budget. They answer "is anything
//! arriving?".
//!
//! A LiDAR driver that keeps republishing its last frame answers *yes*. So does a
//! driver that died behind a latched publisher. The topic is fresh by timestamp
//! and silent by no metric; the world looks stationary and clear; every existing
//! check is satisfied and the checker admits motion.
//!
//! **Freshness is not liveness.** A frame that arrives on time carries no evidence
//! that anyone looked at the world to produce it. This module is the difference.
//!
//! ## The six checks
//!
//! Each yields a DISTINCT [`SensorFault`] with a stable [`code`](SensorFault::code),
//! never a collapsed "sensor bad" — an operator who cannot tell an unplugged sensor
//! from an iced one cannot fix either.
//!
//! | Check | Catches | Fault |
//! |---|---|---|
//! | Publisher count | driver process gone / never started | [`NoPublisher`] |
//! | Arrival silence | topic alive but nothing coming | [`NoMessages`] |
//! | Header stamp frozen | driver republishing, clock stalled | [`FrozenTimestamp`] |
//! | Frame fingerprint frozen | driver republishing a stale buffer | [`FrozenFrame`] |
//! | Valid-ray ratio | occluded / iced / unplugged optics | [`LowValidRayRatio`] |
//! | Measured frequency | driver starving, bus saturated | [`RateBelowFloor`] |
//!
//! [`NoPublisher`]: SensorFault::NoPublisher
//! [`NoMessages`]: SensorFault::NoMessages
//! [`FrozenTimestamp`]: SensorFault::FrozenTimestamp
//! [`FrozenFrame`]: SensorFault::FrozenFrame
//! [`LowValidRayRatio`]: SensorFault::LowValidRayRatio
//! [`RateBelowFloor`]: SensorFault::RateBelowFloor
//!
//! ## Frozen frame vs a genuinely static world
//!
//! The check that needs its threshold justified, because the conservative
//! direction is not obviously the safe one: refusing to drive because the room is
//! empty is a real availability cost, and a watchdog that cries wolf gets disabled.
//!
//! The separation does **not** come from the repeat count. It comes from
//! **bit-exactness**. A physical LiDAR observing a perfectly static scene still
//! produces different range values every frame — photon shot noise, timing jitter
//! and ADC quantisation move the low bits of essentially every return. Two
//! consecutive frames that are *bit-identical* across hundreds of rays are not a
//! static world; they are the same buffer handed out twice.
//!
//! So [`fingerprint_from_ranges`] hashes **raw IEEE bits** (`f32::to_bits`), never
//! a rounded or quantised value. A producer that rounds to centimetres before
//! hashing would destroy exactly the noise that carries the liveness evidence and
//! would make a static scene look frozen — that is why the fingerprint helper is
//! provided here rather than left to the integrator.
//!
//! Given bit-exactness, one repeat is already strong evidence. The default
//! [`FROZEN_FRAME_REPEATS`] of 3 is not there to separate static from frozen; it is
//! margin against a pathological driver that quantises internally, and against a
//! sensor pointed at a blank wall inside its minimum range where whole sectors
//! legitimately report an identical no-return constant.
//!
//! **The residual case is accepted deliberately.** A sensor whose every ray reads
//! the same no-return constant — optics fully blocked, or a genuinely empty room
//! beyond max range — produces bit-identical frames legitimately and will trip this
//! check. That is the conservative direction and it is the correct one: a scan
//! carrying no returns is also a scan carrying no evidence of clear space, and
//! [`LowValidRayRatio`] would refuse it regardless. Stopping is right in both
//! readings of that frame.
//!
//! ## Disarmed by default
//!
//! Like every sibling channel, the watchdog ships disarmed
//! ([`SENSOR_WATCHDOG_ENABLED_ENV`]) and is byte-identical to prior behaviour when
//! off. Arming it on a fleet whose LiDAR rate has never been characterised would
//! ground robots on a threshold nobody measured; the rate floor in particular is a
//! per-platform number.

use std::collections::VecDeque;

/// Environment gate for the sensor watchdog (mirrors
/// [`crate::occlusion_channel::OCCLUSION_CHANNEL_ENABLED_ENV`]). Truthy =
/// `1`/`true`/`yes` (case-insensitive); unset or anything else = disarmed. The
/// `std::env` READ lives in the adapter node (integration glue), not here — this
/// pure module keeps only the tested decision.
pub const SENSOR_WATCHDOG_ENABLED_ENV: &str = "KIRRA_SENSOR_WATCHDOG_ENABLED";

/// Consecutive bit-identical frames that constitute a freeze. See the module docs
/// for why the separation from a static scene comes from bit-exactness rather than
/// from this count.
pub const FROZEN_FRAME_REPEATS: u32 = 3;

/// Consecutive frames with a non-advancing header stamp that constitute a freeze.
/// Matched to [`FROZEN_FRAME_REPEATS`]: the two checks catch the same driver
/// pathology from different angles and there is no reason to trust one longer.
pub const FROZEN_STAMP_REPEATS: u32 = 3;

/// Healthy frames required before a recovered sensor's speed cap is released.
/// Deliberately the same value as the verifier's `AV_RECOVERY_STREAK_THRESHOLD`
/// — one recovery doctrine across the stack, not two.
pub const SENSOR_RECOVERY_STREAK: u32 = 5;

/// Window the recovery streak must complete within. Mirrors the verifier's
/// `AV_RECOVERY_WINDOW_MS`, and carries the same correction: a streak that is a
/// count alone is satisfied by five frames ten seconds apart, or by a driver
/// replaying buffered health. Both conditions must hold.
pub const SENSOR_RECOVERY_WINDOW_MS: u64 = 10_000;

/// Frames retained for the rate measurement. Ten samples at a nominal 10 Hz is a
/// ~1 s window — long enough that a single late frame does not read as starvation,
/// short enough to notice real starvation within a stopping distance.
pub const RATE_WINDOW_FRAMES: usize = 10;

/// Why a watchdog configuration was refused. Refused, never clamped: a silently
/// corrected threshold leaves the operator believing a bound is enforced that is
/// not the one they wrote.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchdogConfigError {
    /// `valid_ray_floor` outside `[0.0, 1.0]`, or not finite.
    ValidRayFloorOutOfRange { floor: f64 },
    /// `min_hz` not finite, or not strictly positive.
    RateFloorNotPositive { min_hz: f64 },
    /// `silence_timeout_ms` of zero — every tick would fault.
    SilenceTimeoutZero,
    /// A repeat/streak threshold of zero, which would fault (or clear) instantly.
    ThresholdZero { field: &'static str },
}

impl std::fmt::Display for WatchdogConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ValidRayFloorOutOfRange { floor } => write!(
                f,
                "valid_ray_floor {floor} is outside [0.0, 1.0] — a ratio floor \
                 cannot be satisfied or can never be violated"
            ),
            Self::RateFloorNotPositive { min_hz } => write!(
                f,
                "min_hz {min_hz} is not a positive finite rate — a non-positive \
                 floor disables the starvation check silently"
            ),
            Self::SilenceTimeoutZero => write!(
                f,
                "silence_timeout_ms of 0 would fault on every tick, including the \
                 tick a frame arrives on"
            ),
            Self::ThresholdZero { field } => write!(
                f,
                "{field} of 0 would trip (or clear) before any evidence is gathered"
            ),
        }
    }
}

impl std::error::Error for WatchdogConfigError {}

/// Per-platform watchdog thresholds. Constructed through [`SensorWatchdogConfig::new`],
/// which refuses a configuration it cannot enforce.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorWatchdogConfig {
    /// Arrival silence (ms) that constitutes [`SensorFault::NoMessages`].
    pub silence_timeout_ms: u64,
    /// Consecutive non-advancing header stamps that constitute a freeze.
    pub frozen_stamp_repeats: u32,
    /// Consecutive bit-identical fingerprints that constitute a freeze.
    pub frozen_frame_repeats: u32,
    /// Minimum fraction of finite, in-range returns. Below this the frame carries
    /// too little evidence to bound anything.
    pub valid_ray_floor: f64,
    /// Minimum measured publish rate (Hz). Per-platform; there is no universal value.
    pub min_hz: f64,
    /// Healthy frames required before the cap is released after a fault.
    pub recovery_streak: u32,
    /// Window the recovery streak must complete within (ms).
    pub recovery_window_ms: u64,
    /// Restart attempts permitted before the fault becomes terminal.
    pub max_restart_attempts: u32,
}

impl SensorWatchdogConfig {
    /// Check that every threshold can be enforced as written.
    ///
    /// Validation lives here rather than in a constructor because the fields are
    /// public: a private-field constructor would suggest a guarantee that a struct
    /// literal could sidestep. [`SensorWatchdog::new`] is the single point where a
    /// config becomes enforcement, and it calls this — so there is no way to obtain
    /// a watchdog running on thresholds nobody checked.
    ///
    /// # Errors
    ///
    /// Returns [`WatchdogConfigError`] for an out-of-range ratio floor, a
    /// non-positive rate floor, a zero silence timeout, or a zero repeat/streak
    /// threshold.
    pub fn validate(&self) -> Result<(), WatchdogConfigError> {
        if !self.valid_ray_floor.is_finite() || !(0.0..=1.0).contains(&self.valid_ray_floor) {
            return Err(WatchdogConfigError::ValidRayFloorOutOfRange {
                floor: self.valid_ray_floor,
            });
        }
        if !self.min_hz.is_finite() || self.min_hz <= 0.0 {
            return Err(WatchdogConfigError::RateFloorNotPositive {
                min_hz: self.min_hz,
            });
        }
        if self.silence_timeout_ms == 0 {
            return Err(WatchdogConfigError::SilenceTimeoutZero);
        }
        for (field, value) in [
            ("frozen_stamp_repeats", self.frozen_stamp_repeats),
            ("frozen_frame_repeats", self.frozen_frame_repeats),
            ("recovery_streak", self.recovery_streak),
        ] {
            if value == 0 {
                return Err(WatchdogConfigError::ThresholdZero { field });
            }
        }
        Ok(())
    }

    /// Thresholds for the R2's forward LiDAR at its nominal 10 Hz.
    ///
    /// The rate floor of 5 Hz is deliberately half nominal rather than just under
    /// it: the check exists to catch a starving driver, not to red on jitter, and a
    /// floor set at 9 Hz would spend its life flapping. The valid-ray floor of 0.15
    /// is likewise well under any healthy indoor scan — it catches blocked optics,
    /// not a sparse room.
    #[must_use]
    pub fn r2_lidar() -> Self {
        Self {
            silence_timeout_ms: 500,
            frozen_stamp_repeats: FROZEN_STAMP_REPEATS,
            frozen_frame_repeats: FROZEN_FRAME_REPEATS,
            valid_ray_floor: 0.15,
            min_hz: 5.0,
            recovery_streak: SENSOR_RECOVERY_STREAK,
            recovery_window_ms: SENSOR_RECOVERY_WINDOW_MS,
            max_restart_attempts: 3,
        }
    }
}

/// One observed frame. Produced by the ROS glue; this module never touches ROS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameObservation {
    /// The frame's own header stamp (ms), NOT the arrival time. The distinction is
    /// the point of [`SensorFault::FrozenTimestamp`].
    pub header_stamp_ms: u64,
    /// Bit-exact content hash — see [`fingerprint_from_ranges`].
    pub fingerprint: u64,
    /// Returns that are finite and within the sensor's configured range.
    pub valid_rays: u32,
    /// Total returns in the frame, valid or not.
    pub total_rays: u32,
}

/// One watchdog tick — the single funnel. Publisher count is observable whether or
/// not a frame arrived, so it rides the tick rather than the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorTick {
    /// Monotonic wall clock (ms). Supplied by the caller so this stays deterministic.
    pub now_ms: u64,
    /// Publishers currently advertising the topic.
    pub publisher_count: usize,
    /// The frame that arrived this tick, if any.
    pub frame: Option<FrameObservation>,
}

/// A distinct, operator-actionable sensor fault. Never collapsed into one code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SensorFault {
    /// Nobody is advertising the topic — the driver is gone or never started.
    /// Distinct from [`NoMessages`](Self::NoMessages): a subscriber with zero
    /// publishers is a wiring or process fault, not a slow sensor.
    NoPublisher,
    /// Publishers exist but nothing has arrived within the silence timeout.
    NoMessages { silent_for_ms: u64 },
    /// Frames keep arriving with a header stamp that is not advancing — or one that
    /// went backwards, which is a clock jump or a replayed recording.
    FrozenTimestamp { repeats: u32, stamp_ms: u64 },
    /// Consecutive bit-identical frames: the same buffer handed out repeatedly.
    FrozenFrame { repeats: u32, fingerprint: u64 },
    /// Too few finite, in-range returns to bound anything — blocked, iced or
    /// unplugged optics. Also covers a frame with no rays at all.
    LowValidRayRatio { ratio: f64, floor: f64 },
    /// Measured publish rate below the configured floor. Carries the measurement,
    /// so an operator sees *how* starved rather than merely *that* it is starved.
    RateBelowFloor { measured_hz: f64, floor_hz: f64 },
    /// The restart budget is spent and the sensor is still faulted. Terminal:
    /// motion stays refused until an operator [`reset`](SensorWatchdog::reset)s,
    /// mirroring the verifier's LockedOut human-reset doctrine.
    RestartsExhausted { attempts: u32 },
}

impl SensorFault {
    /// Stable machine code for logs, telemetry and reason plumbing.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoPublisher => "SENSOR_NO_PUBLISHER",
            Self::NoMessages { .. } => "SENSOR_NO_MESSAGES",
            Self::FrozenTimestamp { .. } => "SENSOR_FROZEN_TIMESTAMP",
            Self::FrozenFrame { .. } => "SENSOR_FROZEN_FRAME",
            Self::LowValidRayRatio { .. } => "SENSOR_LOW_VALID_RAY_RATIO",
            Self::RateBelowFloor { .. } => "SENSOR_RATE_BELOW_FLOOR",
            Self::RestartsExhausted { .. } => "SENSOR_RESTARTS_EXHAUSTED",
        }
    }
}

/// The watchdog's verdict for one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SensorHealth {
    /// Watchdog disarmed — no opinion, no cap. Byte-identical to prior behaviour.
    Disarmed,
    /// Every check passed and any recovery streak is complete.
    Healthy,
    /// A check failed. Motion is refused via the MRC-floor cap.
    Faulted(SensorFault),
    /// The sensor is producing healthy frames again but has not yet earned the
    /// streak. Motion stays refused — this is the state that stops a flapping
    /// sensor from being trusted the instant it blinks back.
    Recovering { healthy_streak: u32, required: u32 },
    /// Not yet enough frames to measure a rate. Other checks still apply; the rate
    /// check alone is suspended. Not a fault — a starting sensor is not a broken one,
    /// and genuine silence during warm-up is caught by [`SensorFault::NoMessages`].
    WarmingUp { frames: usize },
}

impl SensorHealth {
    /// The perception cap this verdict contributes, composed via
    /// [`crate::perception_redundancy::more_restrictive_cap`] into the same Track-C
    /// derate the sibling channels use.
    ///
    /// `Some(0.0)` — the MRC floor — for both [`Faulted`](Self::Faulted) and
    /// [`Recovering`](Self::Recovering). Recovering caps deliberately: releasing
    /// motion on the first healthy frame is what the streak exists to prevent.
    #[must_use]
    pub fn perception_cap(&self) -> Option<f64> {
        match self {
            Self::Faulted(_) | Self::Recovering { .. } => Some(0.0),
            Self::Disarmed | Self::Healthy | Self::WarmingUp { .. } => None,
        }
    }

    /// The fault, when this verdict carries one.
    #[must_use]
    pub fn fault(&self) -> Option<SensorFault> {
        match self {
            Self::Faulted(f) => Some(*f),
            _ => None,
        }
    }
}

/// Bit-exact content fingerprint over raw range returns (FNV-1a over
/// `f32::to_bits`).
///
/// **Raw bits, never rounded.** The sensor noise in the low bits is the liveness
/// evidence; quantising before hashing would make a static scene indistinguishable
/// from a frozen buffer and defeat [`SensorFault::FrozenFrame`] entirely. This
/// helper exists so that constraint lives in code rather than in a comment an
/// integrator may not read.
///
/// NaN is hashed by its exact bit pattern, so a repeated buffer of NaN still
/// fingerprints identically — a frozen all-invalid scan is caught, not excused.
#[must_use]
pub fn fingerprint_from_ranges(ranges: &[f32]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    // Ray COUNT is folded in first, so a truncated frame cannot collide with a
    // longer one that happens to share a prefix.
    for byte in (ranges.len() as u64).to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    for r in ranges {
        for byte in r.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

/// Stateful per-sensor liveness watchdog. Pure and deterministic — the caller
/// supplies every clock reading.
#[derive(Debug, Clone)]
pub struct SensorWatchdog {
    cfg: SensorWatchdogConfig,
    armed: bool,
    last_frame_at_ms: Option<u64>,
    last_stamp_ms: Option<u64>,
    stamp_repeats: u32,
    last_fingerprint: Option<u64>,
    fingerprint_repeats: u32,
    arrivals: VecDeque<u64>,
    healthy_streak: u32,
    streak_started_ms: Option<u64>,
    restart_attempts: u32,
    /// Set once the restart budget is spent; cleared only by [`reset`](Self::reset).
    terminal: bool,
    /// Whether the previous verdict was a fault, so recovery is only demanded of a
    /// sensor that actually faulted.
    was_faulted: bool,
}

impl SensorWatchdog {
    /// A watchdog in the given arm state. `armed = false` yields
    /// [`SensorHealth::Disarmed`] on every tick and never caps.
    ///
    /// This is where a configuration becomes enforcement, so it is where the
    /// thresholds are checked — refused, never clamped.
    ///
    /// # Errors
    ///
    /// Returns [`WatchdogConfigError`] when `cfg` fails
    /// [`SensorWatchdogConfig::validate`]. A disarmed watchdog is validated too: a
    /// config that would be refused on arming must not be silently accepted while
    /// disarmed, or arming it later becomes the failure.
    pub fn new(cfg: SensorWatchdogConfig, armed: bool) -> Result<Self, WatchdogConfigError> {
        cfg.validate()?;
        Ok(Self::unchecked(cfg, armed))
    }

    fn unchecked(cfg: SensorWatchdogConfig, armed: bool) -> Self {
        Self {
            cfg,
            armed,
            last_frame_at_ms: None,
            last_stamp_ms: None,
            stamp_repeats: 0,
            last_fingerprint: None,
            fingerprint_repeats: 0,
            arrivals: VecDeque::with_capacity(RATE_WINDOW_FRAMES),
            healthy_streak: 0,
            streak_started_ms: None,
            restart_attempts: 0,
            terminal: false,
            was_faulted: false,
        }
    }

    /// Observe one tick and return the verdict.
    #[must_use]
    pub fn observe(&mut self, tick: SensorTick) -> SensorHealth {
        if !self.armed {
            return SensorHealth::Disarmed;
        }
        if self.terminal {
            // Sticky. A sensor whose restart budget is spent does not talk its way
            // back by producing good frames; an operator confirms it.
            return SensorHealth::Faulted(SensorFault::RestartsExhausted {
                attempts: self.restart_attempts,
            });
        }

        match self.check(tick) {
            Some(fault) => {
                self.was_faulted = true;
                self.healthy_streak = 0;
                self.streak_started_ms = None;
                SensorHealth::Faulted(fault)
            }
            None => self.advance_recovery(tick.now_ms),
        }
    }

    /// Run the checks in order of fundamentality and return the first failure.
    ///
    /// The order is deterministic and deliberate: a missing publisher explains a
    /// silence, a silence explains a stalled rate, and reporting the *derived*
    /// symptom instead of the root cause sends the operator to the wrong place.
    /// Later checks are not skipped because they are less important — they are
    /// skipped because they would be redundant restatements of the same fault.
    fn check(&mut self, tick: SensorTick) -> Option<SensorFault> {
        if tick.publisher_count == 0 {
            return Some(SensorFault::NoPublisher);
        }

        let Some(frame) = tick.frame else {
            // No frame this tick. Silence is only a fault once it exceeds the
            // budget; between frames at a nominal rate this is the normal case.
            if let Some(last) = self.last_frame_at_ms {
                let silent_for = tick.now_ms.saturating_sub(last);
                if silent_for >= self.cfg.silence_timeout_ms {
                    return Some(SensorFault::NoMessages {
                        silent_for_ms: silent_for,
                    });
                }
            }
            // Before the first frame there is nothing to time from. The caller's
            // subscription-staleness budget covers a sensor that never starts;
            // inventing a fault here would fault every watchdog at construction.
            return None;
        };

        self.record_arrival(tick.now_ms);

        if let Some(fault) = self.check_stamp(frame.header_stamp_ms) {
            return Some(fault);
        }
        if let Some(fault) = self.check_fingerprint(frame.fingerprint) {
            return Some(fault);
        }
        if let Some(fault) = Self::check_valid_rays(&self.cfg, frame) {
            return Some(fault);
        }
        self.check_rate()
    }

    /// A header stamp that does not ADVANCE is frozen. Equal and backwards are both
    /// failures: equal is a stalled driver clock, backwards is a clock jump or a
    /// replayed recording, and neither may be treated as a fresh observation.
    fn check_stamp(&mut self, stamp_ms: u64) -> Option<SensorFault> {
        match self.last_stamp_ms {
            Some(prev) if stamp_ms <= prev => {
                self.stamp_repeats += 1;
                if self.stamp_repeats >= self.cfg.frozen_stamp_repeats {
                    return Some(SensorFault::FrozenTimestamp {
                        repeats: self.stamp_repeats,
                        stamp_ms,
                    });
                }
            }
            _ => {
                self.stamp_repeats = 0;
                self.last_stamp_ms = Some(stamp_ms);
            }
        }
        None
    }

    fn check_fingerprint(&mut self, fingerprint: u64) -> Option<SensorFault> {
        match self.last_fingerprint {
            Some(prev) if prev == fingerprint => {
                self.fingerprint_repeats += 1;
                if self.fingerprint_repeats >= self.cfg.frozen_frame_repeats {
                    return Some(SensorFault::FrozenFrame {
                        repeats: self.fingerprint_repeats,
                        fingerprint,
                    });
                }
            }
            _ => {
                self.fingerprint_repeats = 0;
                self.last_fingerprint = Some(fingerprint);
            }
        }
        None
    }

    /// A frame with no rays at all is a fault, not a division by zero: an empty
    /// scan carries exactly as little evidence as a fully blocked one.
    fn check_valid_rays(
        cfg: &SensorWatchdogConfig,
        frame: FrameObservation,
    ) -> Option<SensorFault> {
        if frame.total_rays == 0 {
            return Some(SensorFault::LowValidRayRatio {
                ratio: 0.0,
                floor: cfg.valid_ray_floor,
            });
        }
        let ratio = f64::from(frame.valid_rays) / f64::from(frame.total_rays);
        if ratio < cfg.valid_ray_floor {
            return Some(SensorFault::LowValidRayRatio {
                ratio,
                floor: cfg.valid_ray_floor,
            });
        }
        None
    }

    fn record_arrival(&mut self, now_ms: u64) {
        self.last_frame_at_ms = Some(now_ms);
        if self.arrivals.len() == RATE_WINDOW_FRAMES {
            self.arrivals.pop_front();
        }
        self.arrivals.push_back(now_ms);
    }

    /// Measured rate over the arrival window, or `None` while warming up.
    ///
    /// A window whose samples share one timestamp spans zero time. That is a
    /// clock-resolution artefact, not infinite throughput — but it is also not
    /// starvation, and reporting a fault there would red every deployment whose
    /// clock is coarser than its frame interval.
    #[must_use]
    pub fn measured_hz(&self) -> Option<f64> {
        if self.arrivals.len() < 2 {
            return None;
        }
        let first = *self.arrivals.front()?;
        let last = *self.arrivals.back()?;
        let span_ms = last.saturating_sub(first);
        if span_ms == 0 {
            return None;
        }
        let intervals = (self.arrivals.len() - 1) as f64;
        Some(intervals * 1000.0 / span_ms as f64)
    }

    fn check_rate(&self) -> Option<SensorFault> {
        let measured = self.measured_hz()?;
        if measured < self.cfg.min_hz {
            return Some(SensorFault::RateBelowFloor {
                measured_hz: measured,
                floor_hz: self.cfg.min_hz,
            });
        }
        None
    }

    /// A clean tick. Report warm-up, hold the cap through the recovery streak, or
    /// clear to healthy.
    fn advance_recovery(&mut self, now_ms: u64) -> SensorHealth {
        if !self.was_faulted {
            // Never faulted — nothing to recover from. Warm-up is reported so the
            // caller can see the rate check is not yet live, but it does not cap.
            if self.arrivals.len() < 2 {
                return SensorHealth::WarmingUp {
                    frames: self.arrivals.len(),
                };
            }
            return SensorHealth::Healthy;
        }

        // The streak is time-bounded as well as counted. A count alone is satisfied
        // by five frames arriving ten seconds apart, or by a driver replaying
        // buffered frames — the same correction `recovery_hysteresis.rs` records.
        match self.streak_started_ms {
            Some(started) if now_ms.saturating_sub(started) > self.cfg.recovery_window_ms => {
                // Window expired mid-streak: restart it from this frame rather than
                // crediting frames that are now too old to evidence liveness.
                self.healthy_streak = 1;
                self.streak_started_ms = Some(now_ms);
            }
            Some(_) => self.healthy_streak += 1,
            None => {
                self.healthy_streak = 1;
                self.streak_started_ms = Some(now_ms);
            }
        }

        if self.healthy_streak >= self.cfg.recovery_streak {
            self.was_faulted = false;
            self.healthy_streak = 0;
            self.streak_started_ms = None;
            self.restart_attempts = 0;
            return SensorHealth::Healthy;
        }
        SensorHealth::Recovering {
            healthy_streak: self.healthy_streak,
            required: self.cfg.recovery_streak,
        }
    }

    /// Whether a restart is warranted and the budget allows one. The watchdog
    /// decides; performing the restart is the integration layer's job.
    #[must_use]
    pub fn should_attempt_restart(&self) -> bool {
        self.armed && self.was_faulted && !self.terminal
    }

    /// Record that the integration layer restarted the sensor. Spending the last
    /// attempt makes the fault terminal — the ceiling exists so a driver that
    /// cannot come back is escalated to a human instead of restarted forever.
    pub fn record_restart_attempt(&mut self) {
        self.restart_attempts += 1;
        if self.restart_attempts >= self.cfg.max_restart_attempts {
            self.terminal = true;
        }
    }

    /// Attempts spent so far.
    #[must_use]
    pub fn restart_attempts(&self) -> u32 {
        self.restart_attempts
    }

    /// Whether the watchdog has escalated to terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Operator reset — clears the terminal latch and all history. The deliberate
    /// counterpart to the sticky terminal state.
    ///
    /// Infallible: this watchdog exists, so its config already passed validation.
    pub fn reset(&mut self) {
        *self = Self::unchecked(self.cfg, self.armed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed() -> SensorWatchdog {
        SensorWatchdog::new(SensorWatchdogConfig::r2_lidar(), true).expect("R2 defaults are valid")
    }

    /// A config built from the R2 defaults with individual fields overridden.
    fn cfg_with(f: impl FnOnce(&mut SensorWatchdogConfig)) -> SensorWatchdogConfig {
        let mut c = SensorWatchdogConfig::r2_lidar();
        f(&mut c);
        c
    }

    /// A healthy frame at `now_ms`, with a stamp and fingerprint that both advance.
    fn good_frame(seq: u64) -> FrameObservation {
        FrameObservation {
            header_stamp_ms: 1_000 + seq * 100,
            fingerprint: 0xAAAA_0000 + seq,
            valid_rays: 360,
            total_rays: 360,
        }
    }

    fn tick(now_ms: u64, frame: Option<FrameObservation>) -> SensorTick {
        SensorTick {
            now_ms,
            publisher_count: 1,
            frame,
        }
    }

    /// Drive `n` healthy frames at 10 Hz starting from `start_ms`/`seq`.
    fn run_healthy(w: &mut SensorWatchdog, start_ms: u64, seq: u64, n: u64) -> SensorHealth {
        let mut last = SensorHealth::Disarmed;
        for i in 0..n {
            last = w.observe(tick(start_ms + i * 100, Some(good_frame(seq + i))));
        }
        last
    }

    // ----- configuration is refused, not clamped -----

    #[test]
    fn the_r2_default_config_is_valid() {
        let cfg = SensorWatchdogConfig::r2_lidar();
        assert_eq!(cfg.validate(), Ok(()));
        assert_eq!(cfg.min_hz, 5.0);
        assert_eq!(cfg.recovery_streak, SENSOR_RECOVERY_STREAK);
    }

    /// The refusal is enforced at the point a config becomes enforcement, so an
    /// unvalidated struct literal cannot reach the checks by going around
    /// `validate`.
    #[test]
    fn an_unenforceable_config_cannot_build_a_watchdog() {
        let bad = cfg_with(|c| c.min_hz = 0.0);
        assert_eq!(
            SensorWatchdog::new(bad, true).err(),
            Some(WatchdogConfigError::RateFloorNotPositive { min_hz: 0.0 })
        );
        // …and a DISARMED watchdog is refused too: accepting it now would just
        // defer the failure to whenever someone arms it.
        assert!(SensorWatchdog::new(bad, false).is_err());
    }

    #[test]
    fn an_unenforceable_config_is_refused() {
        assert_eq!(
            cfg_with(|c| c.valid_ray_floor = 1.5).validate(),
            Err(WatchdogConfigError::ValidRayFloorOutOfRange { floor: 1.5 })
        );
        assert!(matches!(
            cfg_with(|c| c.valid_ray_floor = f64::NAN).validate(),
            Err(WatchdogConfigError::ValidRayFloorOutOfRange { .. })
        ));
        assert_eq!(
            cfg_with(|c| c.min_hz = 0.0).validate(),
            Err(WatchdogConfigError::RateFloorNotPositive { min_hz: 0.0 })
        );
        assert!(matches!(
            cfg_with(|c| c.min_hz = f64::INFINITY).validate(),
            Err(WatchdogConfigError::RateFloorNotPositive { .. })
        ));
        assert_eq!(
            cfg_with(|c| c.silence_timeout_ms = 0).validate(),
            Err(WatchdogConfigError::SilenceTimeoutZero)
        );
        assert_eq!(
            cfg_with(|c| c.frozen_stamp_repeats = 0).validate(),
            Err(WatchdogConfigError::ThresholdZero {
                field: "frozen_stamp_repeats"
            })
        );
        assert_eq!(
            cfg_with(|c| c.recovery_streak = 0).validate(),
            Err(WatchdogConfigError::ThresholdZero {
                field: "recovery_streak"
            })
        );
    }

    /// The boundaries of the ratio floor are inclusive-valid: 0.0 and 1.0 are both
    /// legitimate configurations (never check / demand every ray).
    #[test]
    fn the_ratio_floor_boundaries_are_accepted() {
        assert!(cfg_with(|c| c.valid_ray_floor = 0.0).validate().is_ok());
        assert!(cfg_with(|c| c.valid_ray_floor = 1.0).validate().is_ok());
    }

    // ----- disarmed is the byte-identical no-op -----

    #[test]
    fn disarmed_never_caps_whatever_arrives() {
        let mut w = SensorWatchdog::new(SensorWatchdogConfig::r2_lidar(), false)
            .expect("R2 defaults are valid");
        // Every one of these would fault an armed watchdog.
        for t in [
            SensorTick {
                now_ms: 0,
                publisher_count: 0,
                frame: None,
            },
            tick(10_000, None),
            tick(
                10_100,
                Some(FrameObservation {
                    header_stamp_ms: 0,
                    fingerprint: 7,
                    valid_rays: 0,
                    total_rays: 360,
                }),
            ),
        ] {
            let h = w.observe(t);
            assert_eq!(h, SensorHealth::Disarmed);
            assert_eq!(h.perception_cap(), None);
        }
    }

    // ----- the five checks each red -----

    #[test]
    fn zero_publishers_is_its_own_fault() {
        let mut w = armed();
        let h = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: Some(good_frame(0)),
        });
        assert_eq!(h, SensorHealth::Faulted(SensorFault::NoPublisher));
        assert_eq!(h.perception_cap(), Some(0.0));
        assert_eq!(h.fault().unwrap().code(), "SENSOR_NO_PUBLISHER");
    }

    /// A publisher that exists but has gone quiet is NOT the same fault as no
    /// publisher — the operator fix differs (restart a driver vs check wiring).
    #[test]
    fn silence_past_the_budget_is_a_distinct_fault() {
        let mut w = armed();
        assert!(w.observe(tick(0, Some(good_frame(0)))).fault().is_none());
        // Just inside the budget: not yet a fault.
        assert!(w.observe(tick(499, None)).fault().is_none());
        let h = w.observe(tick(500, None));
        assert_eq!(
            h,
            SensorHealth::Faulted(SensorFault::NoMessages { silent_for_ms: 500 })
        );
        assert_eq!(h.perception_cap(), Some(0.0));
    }

    /// THE headline failure: frames keep arriving, on time, with a stamp that never
    /// advances. Fresh by arrival, dead by observation.
    #[test]
    fn a_frozen_stamp_faults_even_though_frames_keep_arriving() {
        let mut w = armed();
        let frozen = FrameObservation {
            header_stamp_ms: 1_000,
            ..good_frame(0)
        };
        assert!(w.observe(tick(0, Some(frozen))).fault().is_none());
        // Fingerprint keeps changing, so ONLY the stamp check can fire here.
        for i in 1..=2 {
            let f = FrameObservation {
                header_stamp_ms: 1_000,
                fingerprint: 0xBEEF + i,
                ..good_frame(0)
            };
            let _ = w.observe(tick(i * 100, Some(f)));
        }
        let f = FrameObservation {
            header_stamp_ms: 1_000,
            fingerprint: 0xBEEF + 9,
            ..good_frame(0)
        };
        let h = w.observe(tick(900, Some(f)));
        assert!(
            matches!(
                h,
                SensorHealth::Faulted(SensorFault::FrozenTimestamp {
                    stamp_ms: 1_000,
                    ..
                })
            ),
            "got {h:?}"
        );
        assert_eq!(h.perception_cap(), Some(0.0));
    }

    /// A stamp going BACKWARDS is a clock jump or a replayed bag, and counts as
    /// non-advancing rather than as a fresh observation.
    #[test]
    fn a_backwards_stamp_counts_as_frozen() {
        let mut w = armed();
        let _ = w.observe(tick(0, Some(good_frame(5))));
        for i in 1..=3 {
            let f = FrameObservation {
                header_stamp_ms: 1_000, // earlier than good_frame(5)'s 1_500
                fingerprint: 0xC0DE + i,
                ..good_frame(0)
            };
            let h = w.observe(tick(i * 100, Some(f)));
            if i == 3 {
                assert!(
                    matches!(
                        h,
                        SensorHealth::Faulted(SensorFault::FrozenTimestamp { .. })
                    ),
                    "got {h:?}"
                );
            }
        }
    }

    /// The republished-buffer case: stamp advances (the driver restamps) but the
    /// content is bit-identical.
    #[test]
    fn a_repeated_fingerprint_faults_while_the_stamp_advances() {
        let mut w = armed();
        let mut last = SensorHealth::Disarmed;
        for i in 0..4 {
            let f = FrameObservation {
                header_stamp_ms: 1_000 + i * 100, // advancing — stamp check clean
                fingerprint: 0xDEAD_BEEF,         // …but the same buffer
                valid_rays: 360,
                total_rays: 360,
            };
            last = w.observe(tick(i * 100, Some(f)));
        }
        assert!(
            matches!(
                last,
                SensorHealth::Faulted(SensorFault::FrozenFrame {
                    fingerprint: 0xDEAD_BEEF,
                    ..
                })
            ),
            "got {last:?}"
        );
        assert_eq!(last.perception_cap(), Some(0.0));
    }

    /// Non-vacuity for the freeze check: the SAME watchdog, the same cadence, the
    /// same everything except that the fingerprint advances — stays healthy. Without
    /// this, the test above would pass against a watchdog that faults unconditionally.
    #[test]
    fn the_freeze_test_is_not_vacuous() {
        let mut w = armed();
        let h = run_healthy(&mut w, 0, 0, 8);
        assert_eq!(h, SensorHealth::Healthy);
        assert_eq!(h.perception_cap(), None);
    }

    #[test]
    fn a_blinded_scan_faults_on_the_valid_ray_ratio() {
        let mut w = armed();
        let f = FrameObservation {
            valid_rays: 10,
            total_rays: 360,
            ..good_frame(0)
        };
        let h = w.observe(tick(0, Some(f)));
        match h {
            SensorHealth::Faulted(SensorFault::LowValidRayRatio { ratio, floor }) => {
                assert!((ratio - 10.0 / 360.0).abs() < 1e-12);
                assert_eq!(floor, 0.15);
            }
            other => panic!("expected a ratio fault, got {other:?}"),
        }
        assert_eq!(h.perception_cap(), Some(0.0));
    }

    /// An all-`inf` scan — the fault-injection case the issue names — reaches the
    /// ratio check as zero valid rays.
    #[test]
    fn an_all_invalid_scan_faults() {
        let mut w = armed();
        let f = FrameObservation {
            valid_rays: 0,
            total_rays: 360,
            ..good_frame(0)
        };
        assert!(matches!(
            w.observe(tick(0, Some(f))),
            SensorHealth::Faulted(SensorFault::LowValidRayRatio { .. })
        ));
    }

    /// A frame with no rays at all divides by zero if handled naively. It is a
    /// fault, and specifically the same fault as a blinded scan: no evidence.
    #[test]
    fn an_empty_scan_faults_rather_than_dividing_by_zero() {
        let mut w = armed();
        let f = FrameObservation {
            valid_rays: 0,
            total_rays: 0,
            ..good_frame(0)
        };
        assert_eq!(
            w.observe(tick(0, Some(f))),
            SensorHealth::Faulted(SensorFault::LowValidRayRatio {
                ratio: 0.0,
                floor: 0.15
            })
        );
    }

    /// The rate fault reports a NUMBER, not a boolean — an operator needs to know
    /// how starved, not merely that it is starved.
    #[test]
    fn a_starved_rate_faults_and_reports_the_measurement() {
        let mut w = armed();
        // 2 Hz — well under the 5 Hz floor — with everything else healthy.
        let mut last = SensorHealth::Disarmed;
        for i in 0..4 {
            last = w.observe(tick(i * 500, Some(good_frame(i))));
        }
        match last {
            SensorHealth::Faulted(SensorFault::RateBelowFloor {
                measured_hz,
                floor_hz,
            }) => {
                assert!(
                    (measured_hz - 2.0).abs() < 1e-9,
                    "measured {measured_hz} should be 2 Hz"
                );
                assert_eq!(floor_hz, 5.0);
            }
            other => panic!("expected a rate fault, got {other:?}"),
        }
    }

    /// Warm-up suspends only the rate check, and does not cap. A starting sensor is
    /// not a broken one; genuine silence during warm-up is caught as NoMessages.
    #[test]
    fn warm_up_does_not_cap() {
        let mut w = armed();
        let h = w.observe(tick(0, Some(good_frame(0))));
        assert_eq!(h, SensorHealth::WarmingUp { frames: 1 });
        assert_eq!(h.perception_cap(), None);
    }

    /// Every fault carries a distinct code — the issue's "not a single collapsed
    /// sensor bad". Walks the real enum so a new variant that reuses a code reds.
    #[test]
    fn every_fault_code_is_distinct() {
        let codes = [
            SensorFault::NoPublisher.code(),
            SensorFault::NoMessages { silent_for_ms: 1 }.code(),
            SensorFault::FrozenTimestamp {
                repeats: 1,
                stamp_ms: 1,
            }
            .code(),
            SensorFault::FrozenFrame {
                repeats: 1,
                fingerprint: 1,
            }
            .code(),
            SensorFault::LowValidRayRatio {
                ratio: 0.0,
                floor: 0.1,
            }
            .code(),
            SensorFault::RateBelowFloor {
                measured_hz: 1.0,
                floor_hz: 5.0,
            }
            .code(),
            SensorFault::RestartsExhausted { attempts: 3 }.code(),
        ];
        let mut seen = codes.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), codes.len(), "fault codes must be distinct");
    }

    // ----- recovery streak -----

    /// The core of the recovery doctrine: one healthy frame after a fault does NOT
    /// release motion. The cap holds through the whole streak.
    #[test]
    fn a_recovered_sensor_stays_capped_until_the_streak_completes() {
        let mut w = armed();
        assert!(w
            .observe(SensorTick {
                now_ms: 0,
                publisher_count: 0,
                frame: None,
            })
            .fault()
            .is_some());

        for i in 1..SENSOR_RECOVERY_STREAK as u64 {
            let h = w.observe(tick(i * 100, Some(good_frame(i))));
            assert_eq!(
                h,
                SensorHealth::Recovering {
                    healthy_streak: i as u32,
                    required: SENSOR_RECOVERY_STREAK
                }
            );
            assert_eq!(h.perception_cap(), Some(0.0), "motion must stay refused");
        }
        let h = w.observe(tick(
            SENSOR_RECOVERY_STREAK as u64 * 100,
            Some(good_frame(9)),
        ));
        assert_eq!(h, SensorHealth::Healthy);
        assert_eq!(h.perception_cap(), None);
    }

    /// A fault mid-streak resets it — a flapping sensor never accumulates its way to
    /// trusted.
    #[test]
    fn a_fault_mid_streak_resets_it() {
        let mut w = armed();
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });
        let _ = w.observe(tick(100, Some(good_frame(1))));
        let _ = w.observe(tick(200, Some(good_frame(2))));
        // Flap.
        let _ = w.observe(SensorTick {
            now_ms: 300,
            publisher_count: 0,
            frame: None,
        });
        let h = w.observe(tick(400, Some(good_frame(4))));
        assert_eq!(
            h,
            SensorHealth::Recovering {
                healthy_streak: 1,
                required: SENSOR_RECOVERY_STREAK
            },
            "the streak must restart from one, not resume"
        );
    }

    /// The streak is time-bounded as well as counted: frames spread beyond the
    /// window restart it, so a driver replaying buffered frames slowly cannot earn
    /// recovery. This is the `recovery_hysteresis.rs` correction, applied here.
    ///
    /// Isolated deliberately. Stretching the R2 defaults past their 10 s window
    /// also drags the measured rate under the 5 Hz floor, so the watchdog faults on
    /// RATE and the assertion passes without the window logic ever running. This
    /// config gives the window a gap it can survive on every other axis — a long
    /// silence budget and a rate floor the gap cannot breach — so the only thing
    /// that can restart the streak here is the window itself.
    #[test]
    fn a_streak_spread_past_the_window_does_not_complete() {
        let cfg = cfg_with(|c| {
            c.silence_timeout_ms = 100_000;
            c.min_hz = 0.1;
            c.recovery_window_ms = 300;
        });
        let mut w = SensorWatchdog::new(cfg, true).expect("isolation config is valid");
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });

        for (i, expect) in [(100, 1), (200, 2), (300, 3)] {
            let h = w.observe(tick(i, Some(good_frame(i / 100))));
            assert_eq!(
                h,
                SensorHealth::Recovering {
                    healthy_streak: expect,
                    required: 5
                },
                "streak should be building cleanly before the gap"
            );
        }

        // 500 - 100 = 400 ms > the 300 ms window: the streak restarts at one rather
        // than continuing to four.
        let h = w.observe(tick(500, Some(good_frame(5))));
        assert_eq!(
            h,
            SensorHealth::Recovering {
                healthy_streak: 1,
                required: 5
            },
            "a window-expired streak must restart, not resume"
        );
        // …and nothing else faulted, so the restart is attributable to the window.
        assert!(h.fault().is_none());
    }

    // ----- bounded restart -----

    /// The budget is spent, not looped. Exhaustion is terminal and refuses motion.
    #[test]
    fn restarts_are_bounded_and_exhaustion_is_terminal() {
        let mut w = armed();
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });
        assert!(w.should_attempt_restart());

        for _ in 0..3 {
            w.record_restart_attempt();
        }
        assert!(w.is_terminal());
        assert!(
            !w.should_attempt_restart(),
            "a terminal watchdog must not keep restarting"
        );

        // …and good frames do NOT talk it back into service.
        let h = run_healthy(&mut w, 1_000, 10, 10);
        assert_eq!(
            h,
            SensorHealth::Faulted(SensorFault::RestartsExhausted { attempts: 3 })
        );
        assert_eq!(h.perception_cap(), Some(0.0));
    }

    /// Only an operator reset clears the terminal latch — the deliberate
    /// counterpart to stickiness.
    #[test]
    fn an_operator_reset_clears_the_terminal_latch() {
        let mut w = armed();
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });
        for _ in 0..3 {
            w.record_restart_attempt();
        }
        assert!(w.is_terminal());

        w.reset();
        assert!(!w.is_terminal());
        assert_eq!(w.restart_attempts(), 0);
        assert_eq!(run_healthy(&mut w, 0, 0, 8), SensorHealth::Healthy);
    }

    // -----------------------------------------------------------------------
    // Boundary pinning (mutation gate, WP-08)
    //
    // Every threshold below is a comparison whose OFF-BY-ONE form is still
    // plausible code. A gate that admits `<=` where it meant `<` is a gate that
    // fires one frame late or one frame early forever, so each boundary gets an
    // exact-equality case rather than only a comfortably-past-it one.
    // -----------------------------------------------------------------------

    /// A ratio EXACTLY at the floor is acceptable, not a fault. `0.5` and `150/300`
    /// are both exact in binary, so this pins the boundary without float slop.
    #[test]
    fn a_ratio_exactly_at_the_floor_is_not_a_fault() {
        let cfg = cfg_with(|c| c.valid_ray_floor = 0.5);
        let mut w = SensorWatchdog::new(cfg, true).expect("valid");
        let f = FrameObservation {
            valid_rays: 150,
            total_rays: 300,
            ..good_frame(0)
        };
        assert!(
            w.observe(tick(0, Some(f))).fault().is_none(),
            "a ratio exactly at the floor must pass — the floor is a minimum, not              a value to exceed"
        );
        // …and one ray below it does not.
        let f = FrameObservation {
            valid_rays: 149,
            total_rays: 300,
            ..good_frame(1)
        };
        assert!(matches!(
            w.observe(tick(100, Some(f))),
            SensorHealth::Faulted(SensorFault::LowValidRayRatio { .. })
        ));
    }

    /// A rate EXACTLY at the floor is acceptable. Frames 100 ms apart measure
    /// exactly 10.0 Hz, so a 10 Hz floor is met, not breached.
    #[test]
    fn a_rate_exactly_at_the_floor_is_not_a_fault() {
        let cfg = cfg_with(|c| c.min_hz = 10.0);
        let mut w = SensorWatchdog::new(cfg, true).expect("valid");
        assert!(w.observe(tick(0, Some(good_frame(0)))).fault().is_none());
        let h = w.observe(tick(100, Some(good_frame(1))));
        assert_eq!(
            h,
            SensorHealth::Healthy,
            "exactly the floor must pass, else every deployment running at its              nominal rate reds"
        );
        // …and a hair under it faults.
        let mut w = SensorWatchdog::new(cfg, true).expect("valid");
        let _ = w.observe(tick(0, Some(good_frame(0))));
        assert!(matches!(
            w.observe(tick(101, Some(good_frame(1)))),
            SensorHealth::Faulted(SensorFault::RateBelowFloor { .. })
        ));
    }

    /// A rate becomes measurable at EXACTLY two arrivals — one interval is enough
    /// to measure. Requiring three would leave the rate check dark for an extra
    /// frame on every start and every recovery.
    #[test]
    fn a_rate_is_measurable_from_exactly_two_arrivals() {
        let mut w = armed();
        let _ = w.observe(tick(0, Some(good_frame(0))));
        assert_eq!(w.measured_hz(), None, "one arrival spans no interval");
        let _ = w.observe(tick(100, Some(good_frame(1))));
        assert_eq!(
            w.measured_hz(),
            Some(10.0),
            "two arrivals 100 ms apart are exactly 10 Hz"
        );
    }

    /// …and two arrivals are also enough to leave warm-up. Warm-up ends when a
    /// rate can be computed, which is the same boundary.
    #[test]
    fn exactly_two_frames_leave_warm_up() {
        let mut w = armed();
        assert_eq!(
            w.observe(tick(0, Some(good_frame(0)))),
            SensorHealth::WarmingUp { frames: 1 }
        );
        assert_eq!(
            w.observe(tick(100, Some(good_frame(1)))),
            SensorHealth::Healthy,
            "the second frame makes a rate measurable, so warm-up is over"
        );
    }

    /// A streak frame arriving EXACTLY at the window boundary still counts. The
    /// window is a budget the streak must fit inside, so landing on the last
    /// millisecond is inside it.
    #[test]
    fn a_streak_frame_exactly_at_the_window_boundary_still_counts() {
        let cfg = cfg_with(|c| {
            c.silence_timeout_ms = 100_000;
            c.min_hz = 0.1;
            c.recovery_window_ms = 300;
        });
        let mut w = SensorWatchdog::new(cfg, true).expect("valid");
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });
        assert_eq!(
            w.observe(tick(100, Some(good_frame(1)))),
            SensorHealth::Recovering {
                healthy_streak: 1,
                required: 5
            }
        );
        // 400 - 100 == 300 == the window. Inside it, so the streak CONTINUES.
        assert_eq!(
            w.observe(tick(400, Some(good_frame(2)))),
            SensorHealth::Recovering {
                healthy_streak: 2,
                required: 5
            },
            "a frame landing exactly on the window boundary must not restart the              streak"
        );
        // One millisecond past it does restart.
        let mut w = SensorWatchdog::new(cfg, true).expect("valid");
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });
        let _ = w.observe(tick(100, Some(good_frame(1))));
        assert_eq!(
            w.observe(tick(401, Some(good_frame(2)))),
            SensorHealth::Recovering {
                healthy_streak: 1,
                required: 5
            }
        );
    }

    /// The restart budget is spent one attempt at a time. The FIRST attempt of
    /// three is not terminal — a ceiling that latched immediately would make the
    /// budget a lie and escalate to a human on the first retry.
    #[test]
    fn the_restart_budget_is_spent_one_attempt_at_a_time() {
        let mut w = armed();
        let _ = w.observe(SensorTick {
            now_ms: 0,
            publisher_count: 0,
            frame: None,
        });

        w.record_restart_attempt();
        assert_eq!(w.restart_attempts(), 1);
        assert!(!w.is_terminal(), "one of three attempts is not exhaustion");

        w.record_restart_attempt();
        assert_eq!(w.restart_attempts(), 2);
        assert!(!w.is_terminal(), "two of three attempts is not exhaustion");

        w.record_restart_attempt();
        assert_eq!(w.restart_attempts(), 3);
        assert!(w.is_terminal(), "the third attempt exhausts the budget");
    }

    /// Every config error renders a distinct, non-empty operator message naming
    /// the offending value. A refusal whose reason does not reach the operator is
    /// a silent refusal.
    #[test]
    fn every_config_error_renders_a_distinct_message() {
        let errors = [
            WatchdogConfigError::ValidRayFloorOutOfRange { floor: 1.5 },
            WatchdogConfigError::RateFloorNotPositive { min_hz: 0.0 },
            WatchdogConfigError::SilenceTimeoutZero,
            WatchdogConfigError::ThresholdZero {
                field: "recovery_streak",
            },
        ];
        let rendered: Vec<String> = errors.iter().map(ToString::to_string).collect();
        for text in &rendered {
            assert!(!text.is_empty(), "a config refusal must say why");
        }
        assert!(rendered[0].contains("1.5"), "{}", rendered[0]);
        assert!(rendered[0].contains("valid_ray_floor"), "{}", rendered[0]);
        assert!(rendered[1].contains("min_hz"), "{}", rendered[1]);
        assert!(rendered[2].contains("every tick"), "{}", rendered[2]);
        assert!(rendered[3].contains("recovery_streak"), "{}", rendered[3]);

        let mut unique = rendered.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            rendered.len(),
            "messages must be distinguishable"
        );
    }

    // ----- fingerprint -----

    /// The bit-exactness the freeze check depends on: two scans differing in the
    /// LOW BITS of a single ray — the sensor noise a static scene still produces —
    /// must fingerprint differently.
    #[test]
    fn the_fingerprint_separates_sensor_noise_from_a_frozen_buffer() {
        let base: Vec<f32> = (0..360).map(|i| 1.0 + i as f32 * 0.01).collect();
        let mut noisy = base.clone();
        // One ray, one ulp.
        noisy[17] = f32::from_bits(base[17].to_bits() + 1);

        assert_ne!(
            fingerprint_from_ranges(&base),
            fingerprint_from_ranges(&noisy),
            "a one-ulp difference must change the fingerprint, or a static scene \
             is indistinguishable from a frozen buffer"
        );
        // …and the same buffer twice is identical, which is what a freeze looks like.
        assert_eq!(
            fingerprint_from_ranges(&base),
            fingerprint_from_ranges(&base.clone())
        );
    }

    /// Known-answer test pinning the FNV-1a construction itself.
    ///
    /// The distinctness tests around this one assert that the fingerprint is
    /// injective on the samples they try — and a degraded mixer (one that ORs bits
    /// together instead of XORing, say) can still be injective on any finite
    /// sample while having a far worse collision distribution across the whole
    /// 2^64 space. Sampling cannot distinguish those; only pinning the algorithm
    /// can.
    ///
    /// These constants are the FNV-1a values for the stated inputs — offset basis
    /// `0xcbf29ce484222325`, prime `0x100000001b3`, ray count folded in first as
    /// eight little-endian bytes, then each `f32::to_bits` as four. A reviewer can
    /// recompute them from that description alone. They are not a stability
    /// promise to any consumer (the fingerprint is only ever compared within one
    /// process); changing the construction deliberately means changing them
    /// deliberately, which is the point.
    #[test]
    fn the_fingerprint_is_fnv1a_over_length_then_raw_bits() {
        assert_eq!(fingerprint_from_ranges(&[]), 0xa8c7_f832_281a_39c5);
        assert_eq!(fingerprint_from_ranges(&[1.0]), 0x60d7_5439_c3b4_02a9);
        assert_eq!(
            fingerprint_from_ranges(&[1.0, 2.0, 3.0]),
            0x722f_7faa_7b49_2d67
        );
    }

    /// The mixing step must actually MIX. A fingerprint whose combine operation
    /// only ever sets bits (`|`) or only ever clears them (`&`) degenerates: the
    /// accumulator saturates or collapses, distinct scans start sharing a
    /// fingerprint, and the freeze check silently stops distinguishing a repeated
    /// buffer from a changing one — failing OPEN, in the one place this module
    /// cannot afford it.
    ///
    /// Sweeping a single ray through many values across several positions catches
    /// that: under a degenerate combine these collapse into far fewer distinct
    /// values than inputs.
    #[test]
    fn the_fingerprint_mixes_rather_than_saturating() {
        let base: Vec<f32> = (0..64).map(|i| 1.0 + i as f32 * 0.25).collect();
        let mut seen = std::collections::HashSet::new();
        let mut inputs = 0;

        // Vary each of several positions — including the FIRST, which a combine
        // that collapses to "the last byte wins" would ignore entirely.
        for pos in [0usize, 1, 17, 31, 63] {
            // From 1: step 0 would leave the vector unmodified, producing the
            // same input at every position and a self-inflicted collision.
            for step in 1..64u32 {
                let mut v = base.clone();
                v[pos] = f32::from_bits(base[pos].to_bits() ^ step.wrapping_mul(2_654_435_761));
                seen.insert(fingerprint_from_ranges(&v));
                inputs += 1;
            }
        }
        assert_eq!(
            seen.len(),
            inputs,
            "every distinct scan must fingerprint distinctly; {} inputs collapsed              to {} fingerprints, so the combine is not mixing",
            inputs,
            seen.len()
        );
    }

    /// Ray count is folded in, so a truncated frame cannot collide with a longer one
    /// sharing its prefix.
    #[test]
    fn the_fingerprint_covers_the_ray_count() {
        let long: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let short = &long[..180];
        assert_ne!(
            fingerprint_from_ranges(&long),
            fingerprint_from_ranges(short)
        );
    }

    /// A repeated all-NaN buffer still fingerprints identically, so a frozen
    /// all-invalid scan is caught by the freeze check rather than excused by it.
    #[test]
    fn a_repeated_nan_buffer_still_fingerprints_identically() {
        let nans = vec![f32::NAN; 32];
        assert_eq!(
            fingerprint_from_ranges(&nans),
            fingerprint_from_ranges(&nans.clone())
        );
    }
}
