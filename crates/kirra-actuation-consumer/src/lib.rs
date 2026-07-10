//! ADR-0033 — the verifying motor-consumer core: the last hop between the
//! checker's verdict and the serial port, with fail-closed liveness.
//!
//! Composition (all verification lives in ONE place — this crate adds none):
//!
//! - **Gate**: [`RosReleaseGate`] (re-exported from
//!   `kirra-release-token::ros_twist`) — token → strict Ed25519 over the exact
//!   presented bytes → finite decode → freshness → strictly-advancing
//!   sequence. Refusals never advance the watermark.
//! - **Liveness** (SS-002 / ADR-0033 "safe state on the motor side"): no valid
//!   release within the deadline window (≈ [`DEFAULT_MISSED_PERIODS`] control
//!   periods) → **active commanded stop** (decel-to-zero ramp from the last
//!   released speed, at the deployed class's MRC decel rate), then **output
//!   silence**. **Never hold-last-command** — hold-last is the Cruise-drag
//!   failure mode SS-002 exists to prevent.
//! - 🔴 **A refusal must NOT reset the liveness window.** Refusals are not
//!   liveness: a flood of invalid tokens starves into the safe stop exactly
//!   as silence does (mirrors `ReleaseRefusal` never advancing the release
//!   watermark on the SHM path).
//! - **Serial seam**: [`MotorSerial`] — the `NvbootctrlRunner` command-seam
//!   precedent. The real Rosmaster serial protocol is deliberately NOT here
//!   (it needs the physical robot; do not guess a motor protocol). Everything
//!   above the seam is built, tested, and CI-gated now; hardware bringup
//!   implements one trait.

#![forbid(unsafe_code)]

pub use kirra_release_token::ros_twist::{
    RosReleaseGate, RosReleaseRefusal, RosTwistPayload, ROS_TWIST_PAYLOAD_LEN,
};
pub use kirra_release_token::ReleaseToken;

use ed25519_dalek::VerifyingKey;

/// ADR-0033: the liveness deadline is ≈ 3 missed control periods.
pub const DEFAULT_MISSED_PERIODS: u32 = 3;

/// The one hardware seam. The real implementation owns the Rosmaster
/// expansion-board serial device (dedicated user/group, mode 0600 — the
/// Tier-3 startup sentinel asserts this) and speaks the vendor protocol.
/// Until hardware access exists, tests drive a recording mock.
///
/// A `write_twist` call is ALREADY past the fence: implementations must not
/// second-guess, buffer-and-replay, or reorder — one call, one frame, in
/// call order.
pub trait MotorSerial {
    type Error: core::fmt::Debug;
    /// Drive the platform at exactly this twist.
    fn write_twist(&mut self, linear_mps: f64, angular_rad_s: f64) -> Result<(), Self::Error>;
}

/// Consumer configuration. Deliberately WITHOUT defaults for the physical
/// numbers — they come from the deployment, not this crate:
#[derive(Clone, Copy, Debug)]
pub struct ConsumerConfig {
    /// ADR-0033 decision-3 freshness window (proposed ≈ 2 control periods).
    /// Load-bearing across consumer restarts — see `RosReleaseGate` docs.
    pub freshness_window_ms: u64,
    /// The control period (the R2's `/cmd_vel` rate is 10–20 Hz → 50–100 ms).
    pub control_period_ms: u64,
    /// Liveness deadline in missed control periods (ADR-0033: ≈ 3;
    /// [`DEFAULT_MISSED_PERIODS`]).
    pub missed_periods: u32,
    /// The decel rate (m/s², > 0, finite) for the active commanded stop —
    /// supply the deployed vehicle class's MRC decel (the "last valid
    /// envelope" of SS-002; see `docs/CONTRACT_PROFILES.md`). There is
    /// deliberately no default: inventing a braking number here would bypass
    /// the class-profile provenance discipline (EP-09).
    pub stop_decel_mps2: f64,
}

/// Constructor-time config refusal — fail-closed before any frame flows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// `stop_decel_mps2` must be finite and > 0 (it authors the safe stop).
    InvalidStopDecel,
    /// `control_period_ms` and `missed_periods` must be non-zero (a zero
    /// deadline window would stop-on-every-tick; zero period divides time out
    /// of the ramp).
    InvalidDeadline,
}

/// What one ingest frame produced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameOutcome {
    /// Gate passed; the enforced twist was written to the serial seam.
    Released { sequence: u64 },
    /// Gate passed but the serial write itself failed. The watermark HAS
    /// advanced (the release was legitimate) and liveness HAS been fed (the
    /// governor is alive); the failure is a hardware fault surfaced to the
    /// supervisor, not a verification event.
    SerialError,
    /// Gate refused — nothing was written, the watermark is untouched, and
    /// the liveness window was NOT fed.
    Refused(RosReleaseRefusal),
}

/// Where the drive layer is in its life cycle. Motion states carry the last
/// RELEASED twist only as the starting point of a future stop ramp — it is
/// never re-emitted as-is (never hold-last-command).
#[derive(Clone, Copy, Debug, PartialEq)]
enum DriveState {
    /// Nothing valid has ever released: the platform has never been commanded
    /// to move, so starvation means SILENCE, not a stop ramp.
    NeverReleased,
    /// Last frame(s) released; `last_linear` is the ramp seed if we starve.
    Driving { last_linear: f64 },
    /// Starved: actively ramping to zero at `stop_decel_mps2`.
    Stopping { current_linear: f64 },
    /// Ramp complete (a final zero twist was written): output silence.
    Silent,
}

/// The consumer: gate + liveness + serial seam. Single-owner, single-thread —
/// exactly one instance per actuation path (the same discipline as the gate).
pub struct MotorConsumer<S: MotorSerial> {
    gate: RosReleaseGate,
    serial: S,
    cfg: ConsumerConfig,
    state: DriveState,
    /// Wall-clock (ms) of the last VALID release. Refusals never touch this.
    last_valid_at_ms: Option<u64>,
    releases: u64,
    refusals: u64,
    serial_errors: u64,
}

impl<S: MotorSerial> MotorConsumer<S> {
    pub fn new(
        governor_vk: VerifyingKey,
        cfg: ConsumerConfig,
        serial: S,
    ) -> Result<Self, ConfigError> {
        if !(cfg.stop_decel_mps2.is_finite() && cfg.stop_decel_mps2 > 0.0) {
            return Err(ConfigError::InvalidStopDecel);
        }
        if cfg.control_period_ms == 0 || cfg.missed_periods == 0 {
            return Err(ConfigError::InvalidDeadline);
        }
        Ok(Self {
            gate: RosReleaseGate::new(governor_vk, cfg.freshness_window_ms),
            serial,
            cfg,
            state: DriveState::NeverReleased,
            last_valid_at_ms: None,
            releases: 0,
            refusals: 0,
            serial_errors: 0,
        })
    }

    /// Ingest one frame from the bus: the 32-byte signed payload image plus
    /// the (optional) 96-byte token. Writes to the serial seam ONLY on a full
    /// gate pass — this call IS the chokepoint.
    pub fn on_frame(
        &mut self,
        payload_bytes: &[u8; ROS_TWIST_PAYLOAD_LEN],
        token: Option<&ReleaseToken>,
        now_ms: u64,
    ) -> FrameOutcome {
        match self.gate.release(payload_bytes, token, now_ms) {
            Ok(released) => {
                // Validity restored/confirmed: feed liveness, (re)enter Driving
                // — a valid release during a stop ramp legitimately resumes.
                self.last_valid_at_ms = Some(now_ms);
                self.state = DriveState::Driving {
                    last_linear: released.linear_mps,
                };
                self.releases += 1;
                match self
                    .serial
                    .write_twist(released.linear_mps, released.angular_rad_s)
                {
                    Ok(()) => FrameOutcome::Released {
                        sequence: released.sequence,
                    },
                    Err(_) => {
                        self.serial_errors += 1;
                        FrameOutcome::SerialError
                    }
                }
            }
            Err(refusal) => {
                // 🔴 Refusals are NOT liveness: last_valid_at_ms untouched.
                self.refusals += 1;
                FrameOutcome::Refused(refusal)
            }
        }
    }

    /// The liveness clock — call once per control period. Drives the SS-002
    /// safe state: past the deadline window with no valid release → active
    /// commanded stop (decel ramp), then silence. Idempotent in `Silent`.
    pub fn on_tick(&mut self, now_ms: u64) {
        let deadline_ms = self.cfg.control_period_ms * u64::from(self.cfg.missed_periods);
        let starved = match self.last_valid_at_ms {
            // Never released → nothing ever moved → silence IS the safe state.
            None => false,
            Some(t) => now_ms.saturating_sub(t) > deadline_ms,
        };
        if !starved {
            return;
        }
        // Transition Driving → Stopping exactly once, seeding the ramp from
        // the last RELEASED speed (the last valid envelope's bound).
        if let DriveState::Driving { last_linear } = self.state {
            self.state = DriveState::Stopping {
                current_linear: last_linear,
            };
        }
        if let DriveState::Stopping { current_linear } = self.state {
            let step = self.cfg.stop_decel_mps2 * (self.cfg.control_period_ms as f64 / 1000.0);
            let next = if current_linear.abs() <= step {
                0.0
            } else {
                current_linear - step * current_linear.signum()
            };
            // Active commanded stop: an explicit decreasing twist each period
            // (never a re-emit of the last command), zero angular.
            if self.serial.write_twist(next, 0.0).is_err() {
                self.serial_errors += 1;
            }
            self.state = if next == 0.0 {
                // The zero twist was the final frame; silence from here on.
                DriveState::Silent
            } else {
                DriveState::Stopping {
                    current_linear: next,
                }
            };
        }
        // NeverReleased / Silent: no writes — output silence.
    }

    /// True once the starve path has completed its ramp (final zero written).
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.state == DriveState::Silent
    }

    /// Valid releases (observability; Tier-2 asserts the refusal counter).
    #[must_use]
    pub fn release_count(&self) -> u64 {
        self.releases
    }

    /// Gate refusals — the counter the Tier-2 rogue-flood test watches.
    #[must_use]
    pub fn refusal_count(&self) -> u64 {
        self.refusals
    }

    /// Serial write failures on legitimately released frames.
    #[must_use]
    pub fn serial_error_count(&self) -> u64 {
        self.serial_errors
    }

    /// Borrow the serial seam (tests inspect the recording mock through this;
    /// the real consumer never needs it).
    #[must_use]
    pub fn serial(&self) -> &S {
        &self.serial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use kirra_release_token::ros_twist::issue_ros_release;

    /// Records every write; the Tier-1 assertion surface.
    #[derive(Default)]
    pub struct RecordingSerial {
        pub writes: Vec<(f64, f64)>,
    }
    impl MotorSerial for RecordingSerial {
        type Error = core::convert::Infallible;
        fn write_twist(&mut self, linear: f64, angular: f64) -> Result<(), Self::Error> {
            self.writes.push((linear, angular));
            Ok(())
        }
    }

    fn sk() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    // Test fixture values only (a real deployment sources stop_decel from its
    // class MRC profile — see ConsumerConfig docs).
    fn cfg() -> ConsumerConfig {
        ConsumerConfig {
            freshness_window_ms: 200,
            control_period_ms: 100,
            missed_periods: DEFAULT_MISSED_PERIODS,
            stop_decel_mps2: 1.0,
        }
    }

    fn consumer() -> MotorConsumer<RecordingSerial> {
        MotorConsumer::new(sk().verifying_key(), cfg(), RecordingSerial::default()).unwrap()
    }

    fn frame(seq: u64, issued: u64, linear: f64) -> ([u8; 32], ReleaseToken) {
        let p = RosTwistPayload {
            sequence: seq,
            issued_at_ms: issued,
            linear_mps: linear,
            angular_rad_s: 0.0,
        };
        (p.encode(), issue_ros_release(&p, &sk()))
    }

    #[test]
    fn config_is_fail_closed() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut c = cfg();
            c.stop_decel_mps2 = bad;
            assert_eq!(
                MotorConsumer::new(sk().verifying_key(), c, RecordingSerial::default()).err(),
                Some(ConfigError::InvalidStopDecel)
            );
        }
        let mut c = cfg();
        c.control_period_ms = 0;
        assert!(MotorConsumer::new(sk().verifying_key(), c, RecordingSerial::default()).is_err());
    }

    #[test]
    fn never_released_means_silence_not_a_stop_ramp() {
        let mut m = consumer();
        for t in (0..2_000).step_by(100) {
            m.on_tick(t);
        }
        assert!(m.serial.writes.is_empty(), "never commanded → never writes");
    }

    #[test]
    fn starvation_ramps_to_zero_then_goes_silent_never_holds_last() {
        let mut m = consumer();
        let (p, t) = frame(1, 10_000, 0.25);
        assert!(matches!(
            m.on_frame(&p, Some(&t), 10_000),
            FrameOutcome::Released { sequence: 1 }
        ));
        // Silence past the 300 ms deadline → ramp: 0.25 → 0.15 → 0.05 → 0 → silent.
        for k in 1..=10u64 {
            m.on_tick(10_000 + 300 + k * 100);
        }
        let w = &m.serial.writes;
        assert_eq!(w[0], (0.25, 0.0), "the released command itself");
        // Every post-starve write strictly decreases in magnitude and the
        // ramp ends at exactly zero — never a re-emit of the last command
        // (never hold-last), never a residual crawl.
        let ramp: Vec<f64> = w[1..].iter().map(|(l, _)| *l).collect();
        assert!(!ramp.is_empty(), "starvation must produce an ACTIVE stop");
        let mut prev = 0.25f64;
        for v in &ramp {
            assert!(
                v.abs() < prev.abs(),
                "ramp must strictly decrease: {ramp:?}"
            );
            prev = *v;
        }
        assert_eq!(*ramp.last().unwrap(), 0.0, "ramp must end at zero");
        assert!(
            w[1..].iter().all(|(_, a)| *a == 0.0),
            "zero angular in stop"
        );
        assert!(m.is_silent());
        // Silent means SILENT: further ticks write nothing.
        let n = m.serial.writes.len();
        m.on_tick(20_000);
        assert_eq!(m.serial.writes.len(), n);
    }

    /// 🔴 The invariant flag: a refusal must NOT reset the liveness window —
    /// a flood of invalid tokens starves into the safe stop exactly as
    /// silence does.
    #[test]
    fn refusal_flood_does_not_reset_the_liveness_window() {
        let mut m = consumer();
        let (p, t) = frame(1, 10_000, 0.25);
        m.on_frame(&p, Some(&t), 10_000);

        // A rogue floods unsigned frames every 50 ms, well past the deadline.
        let (rogue, _) = frame(99, 10_050, 3.0);
        let mut stop_started = false;
        for k in 1..=12u64 {
            let now = 10_000 + k * 50;
            assert!(matches!(
                m.on_frame(&rogue, None, now),
                FrameOutcome::Refused(RosReleaseRefusal::NoToken)
            ));
            m.on_tick(now);
            if m.serial.writes.len() > 1 {
                stop_started = true;
                // The first starve write must come at ~10_350 (deadline 300 ms
                // after the last VALID release at 10_000) — the flood did not
                // push it out.
                assert!(now >= 10_350, "stop must not start before the deadline");
                break;
            }
        }
        assert!(
            stop_started,
            "refusal flood must starve into the stop ramp: refusals are not liveness"
        );
        assert_eq!(m.release_count(), 1);
        assert!(m.refusal_count() >= 1);
    }

    #[test]
    fn valid_release_during_stop_ramp_resumes_driving() {
        let mut m = consumer();
        let (p1, t1) = frame(1, 10_000, 0.25);
        m.on_frame(&p1, Some(&t1), 10_000);
        m.on_tick(10_400); // starved → first ramp write
        assert!(!m.is_silent());
        // Governor recovers: a fresh, advancing release arrives.
        let (p2, t2) = frame(2, 10_500, 0.30);
        assert!(matches!(
            m.on_frame(&p2, Some(&t2), 10_500),
            FrameOutcome::Released { sequence: 2 }
        ));
        // Liveness fed again: the next tick inside the window writes nothing.
        let n = m.serial.writes.len();
        m.on_tick(10_600);
        assert_eq!(m.serial.writes.len(), n);
    }
}
