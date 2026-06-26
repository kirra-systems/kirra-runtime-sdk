// parko/crates/parko-ros2/src/config.rs
//
// Node configuration — topic names, sensor input shape, tick period,
// staleness budget. Built from env / CLI in the binary; threaded
// through the rest of the crate.

use std::time::Duration;

use crate::platform_profile::CourierPlatformProfile;

/// Configuration for the Parko ROS 2 node. Constructed by the binary
/// from env vars / CLI; passed by `Arc<ParkoNodeConfig>` to the drain
/// tasks so they can read it without lock contention.
#[derive(Debug, Clone)]
pub struct ParkoNodeConfig {
    /// ROS 2 topic the node subscribes to for sensor observations.
    /// The integrator maps the topic's payload to a `SensorFrame` via
    /// the `SensorInputMapping` in `sensor_mapping.rs`. Default:
    /// `~/input/observation` (a project-local sensor.msg/Observation
    /// topic placeholder).
    pub sensor_topic: String,
    /// ROS 2 topic the node publishes gated `OutgoingTwist` commands
    /// to. Default: `~/output/cmd_vel`. The integrator maps this to
    /// their vehicle interface (typically `geometry_msgs::Twist`).
    pub command_topic: String,
    /// Tick period, seconds. Defaults to 0.05 s (20 Hz) — matches
    /// `parko_core::InferenceLoop`'s default. The governor receives
    /// this as `delta_time_s` for rate-of-change checks.
    pub tick_period_s: f64,

    /// Sensor-input staleness budget, ms. If `now - frame.timestamp_ms`
    /// exceeds this, the tick pipeline emits a stopped command rather
    /// than running inference on a stale frame. Default 200 ms = ~4×
    /// the tick period @ 20 Hz; the integrator should tighten this
    /// per platform.
    /// **TODO:** revisit per platform after measuring real
    /// observation-arrival jitter; this is a placeholder analogous to
    /// `POSTURE_STALENESS_TIMEOUT_MS` in M1b.
    pub sensor_staleness_budget_ms: u64,

    /// MRC fallback published when a sensor input is stale or
    /// inference fails. Always a stopped twist (linear = angular = 0)
    /// per the M1 / parko-kirra discipline. Stored as a field so the
    /// integrator may override (e.g. a controlled-deceleration
    /// MRC instead of a pure stop) without forking the node.
    pub mrc_command: OutgoingTwistDefaults,

    /// Inference thread count — the ONLY configurable execution-posture knob
    /// (fp32 / ACCURACY / LATENCY are fixed). **Default 1**: bitwise-reproducible
    /// inference on the robot's fixed SoC. Raising it trades that determinism
    /// for latency headroom and is a logged production choice. The binary
    /// threads `inference_threads()` into BOTH the ORT and OpenVINO backends
    /// from this one value, so their thread counts cannot diverge (the #152
    /// cross-backend asymmetry guard).
    pub num_threads: usize,

    /// SG6 (#309) optional IMU topic (`sensor_msgs/Imu`). When set, the node
    /// subscribes and feeds the per-tick deceleration spike into the clearance
    /// loop's impact detection. `None` → no IMU source: the decel trigger is
    /// inactive (reduced detection coverage, logged at startup — never a
    /// fabricated spike).
    pub imu_topic: Option<String>,

    /// SG6 (#309) optional contact-sensor topic (`std_msgs/Bool`). When set, a
    /// `true` reading is a definitive impact and latches the loop. `None` → no
    /// contact source: that trigger is inactive (reduced coverage, logged).
    pub contact_topic: Option<String>,

    /// SG6 (#309) IMU deceleration-spike threshold (m/s²) — the
    /// [`parko_core::ImpactCfg`] `spike_threshold_mps2`. **Deployment-tunable,
    /// VALIDATION-PENDING** (parko-core default 30.0 m/s² — a hard,
    /// collision-grade decel, NOT a certified value). The tick reads the IMU
    /// linear-acceleration vector magnitude (gravity-inclusive) and latches when
    /// it exceeds this; the default sits well above the ~9.81 m/s² gravity
    /// baseline, so a static vehicle never latches. Tune on track-test / SOTIF.
    pub spike_threshold_mps2: f64,

    /// SG6 (#324) IMU staleness window, ms. When an IMU source IS configured
    /// (`imu_topic.is_some()`), a sample older than this — or no sample yet — is a
    /// SENSOR FAULT: the gate forces the MRC (stop), since it cannot detect a
    /// hard-decel impact without fresh IMU. The IMU analogue of
    /// `sensor_staleness_budget_ms` (which gates the sensor FRAME). Default 500 ms
    /// (≈10 ticks @ 20 Hz) — VALIDATION-PENDING; tighten per the real IMU rate. No
    /// effect when no `imu_topic` is set (an UNCONFIGURED IMU is reduced coverage,
    /// never a forced stop — the watchdog is not armed).
    pub imu_staleness_window_ms: u64,

    /// ADR-0029 Phase 2 — the differential-drive courier deployment profile.
    /// `Some(profile)` parameterizes the live governor with the courier's SOTIF
    /// angular envelope ([`CourierPlatformProfile::angular_governor`]) instead of
    /// the `KirraGovernor::new()` conservative default, and supplies the courier
    /// footprint to the [`DiffDrivePlatform`](parko_kirra::platform::DiffDrivePlatform)
    /// checker. **Default `None`** → the node keeps building `KirraGovernor::new()`
    /// (conservative default), byte-identical to pre-Phase-2 behaviour. Opt-in,
    /// fail-safe: an uncharacterized deployment still gets the tighter generic bound.
    pub platform_profile: Option<CourierPlatformProfile>,
}

/// Placeholder for an MRC fallback override. Today's MRC is always
/// `OutgoingTwist::stopped()`; this struct exists so a future
/// integrator can provide a controlled-deceleration MRC without
/// having to fork the crate. Stays empty until that need is real.
#[derive(Debug, Clone, Default)]
pub struct OutgoingTwistDefaults;

impl Default for ParkoNodeConfig {
    fn default() -> Self {
        Self {
            sensor_topic: "~/input/observation".to_string(),
            command_topic: "~/output/cmd_vel".to_string(),
            tick_period_s: 0.05,
            sensor_staleness_budget_ms: 200,
            mrc_command: OutgoingTwistDefaults,
            num_threads: 1,
            // SG6 detection sources default OFF — a missing sensor is reduced
            // coverage, stated loudly at startup, never fabricated.
            imu_topic: None,
            contact_topic: None,
            // parko-core ImpactCfg default (VALIDATION-PENDING, deployment-tunable).
            spike_threshold_mps2: parko_core::ImpactCfg::default().spike_threshold_mps2,
            // SG6 (#324) IMU staleness window (VALIDATION-PENDING; only armed when
            // an imu_topic is configured).
            imu_staleness_window_ms: 500,
            // ADR-0029 Phase 2: no platform profile by default → conservative
            // default angular bound (byte-identical to pre-Phase-2). Opt-in.
            platform_profile: None,
        }
    }
}

impl ParkoNodeConfig {
    #[must_use]
    pub fn tick_period(&self) -> Duration {
        Duration::from_secs_f64(self.tick_period_s.max(0.001))
    }

    /// The single inference-thread configuration the binary threads into BOTH
    /// backends — one source, so ORT and OpenVINO can never run different
    /// thread counts.
    #[must_use]
    pub fn inference_threads(&self) -> parko_core::InferenceThreads {
        parko_core::InferenceThreads::new(self.num_threads)
    }

    /// The SG6 [`parko_core::ImpactCfg`] this node uses for impact fusion,
    /// built from the (deployment-tunable) `spike_threshold_mps2`. One source,
    /// so the tick and any diagnostics read the same threshold.
    #[must_use]
    pub fn impact_cfg(&self) -> parko_core::ImpactCfg {
        parko_core::ImpactCfg {
            spike_threshold_mps2: self.spike_threshold_mps2,
            // #321: M/N confirmation defaults (single-tick) come from the default;
            // the node-config surface only tunes the threshold today.
            ..parko_core::ImpactCfg::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tick_period_is_20hz() {
        let cfg = ParkoNodeConfig::default();
        assert!((cfg.tick_period_s - 0.05).abs() < 1e-9);
        assert_eq!(cfg.tick_period(), Duration::from_millis(50));
    }

    #[test]
    fn default_num_threads_is_single_threaded_reproducible() {
        let cfg = ParkoNodeConfig::default();
        assert_eq!(cfg.num_threads, 1, "default must be single-threaded (preserves #152)");
        // The ONE source both backends read.
        let t = cfg.inference_threads();
        assert_eq!(t.num_threads, 1);
        assert!(t.bitwise_reproducible());
    }

    #[test]
    fn inference_threads_reflects_configured_count() {
        let cfg = ParkoNodeConfig { num_threads: 4, ..ParkoNodeConfig::default() };
        assert_eq!(cfg.inference_threads().num_threads, 4);
        assert!(!cfg.inference_threads().bitwise_reproducible(),
            "raising threads gives up bitwise reproducibility");
    }

    #[test]
    fn sg6_detection_sources_default_off_and_threshold_is_parko_core_default() {
        let cfg = ParkoNodeConfig::default();
        assert!(cfg.imu_topic.is_none(), "IMU source off by default (reduced coverage, stated)");
        assert!(cfg.contact_topic.is_none(), "contact source off by default");
        // The threshold mirrors parko-core's ImpactCfg default, and impact_cfg()
        // round-trips it.
        let core_default = parko_core::ImpactCfg::default().spike_threshold_mps2;
        assert!((cfg.spike_threshold_mps2 - core_default).abs() < 1e-9);
        assert!((cfg.impact_cfg().spike_threshold_mps2 - core_default).abs() < 1e-9);
    }

    #[test]
    fn impact_cfg_reflects_a_tuned_threshold() {
        let cfg = ParkoNodeConfig { spike_threshold_mps2: 22.5, ..ParkoNodeConfig::default() };
        assert!((cfg.impact_cfg().spike_threshold_mps2 - 22.5).abs() < 1e-9,
            "impact_cfg() must carry the deployment-tuned threshold");
    }

    #[test]
    fn default_has_no_platform_profile_preserving_conservative_bound() {
        // ADR-0029 Phase 2 is opt-in: the default config carries no courier
        // profile, so the node keeps building KirraGovernor::new() (the
        // conservative-default angular bound) — byte-identical to pre-Phase-2.
        let cfg = ParkoNodeConfig::default();
        assert!(
            cfg.platform_profile.is_none(),
            "platform_profile must default to None (opt-in, fail-safe)"
        );
    }

    #[test]
    fn a_courier_profile_can_be_configured() {
        let cfg = ParkoNodeConfig {
            platform_profile: Some(CourierPlatformProfile::courier_reference()),
            ..ParkoNodeConfig::default()
        };
        assert_eq!(
            cfg.platform_profile,
            Some(CourierPlatformProfile::courier_reference())
        );
    }

    #[test]
    fn tick_period_clamps_floor_to_avoid_zero_duration() {
        let cfg = ParkoNodeConfig {
            tick_period_s: 0.0,
            ..ParkoNodeConfig::default()
        };
        // The seconds_f64 → Duration round-trip would yield 0 ns at
        // exactly 0.0, which would spin the timer; the floor of 1 ms
        // guards against config typos.
        assert!(cfg.tick_period() >= Duration::from_micros(900),
            "tick_period must floor above zero to avoid a busy-spin");
    }
}
