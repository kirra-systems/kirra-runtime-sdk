// parko/crates/parko-ros2/src/config.rs
//
// Node configuration — topic names, sensor input shape, tick period,
// staleness budget. Built from env / CLI in the binary; threaded
// through the rest of the crate.

use std::time::Duration;

use parko_core::commit_zone::CommitZoneCfg;
use parko_core::water::WaterVetoConfig;

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

    /// WS-0.4 — per-tick inference deadline, ms (threaded into
    /// `InferenceLoop::with_inference_deadline_ms`). The HARD bound on how
    /// long a tick waits for the backend before failing closed to a stopped
    /// command; a backend that never returns (wedged driver / EP stall)
    /// previously stalled the drain loop forever. Always on; default
    /// `parko_core::scheduler::DEFAULT_INFERENCE_DEADLINE_MS` (1000 ms —
    /// generous, so it fires only on a genuine hang, never on latency
    /// jitter; the 150 ms latency THRESHOLD handles slow-but-completing
    /// ticks). Env: `PARKO_INFERENCE_DEADLINE_MS` (positive integer).
    pub inference_deadline_ms: u64,

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

    /// ADR-0029 Phase 3b — lidar topic for the live SG2 containment gate. `Some`
    /// wires a `sensor_msgs/LaserScan` subscription → Taj Phase-A → an ego-relative
    /// corridor fed to `apply_containment_gate` each tick. The gate is armed only
    /// when BOTH this and `platform_profile` (for the footprint) are set. **Default
    /// `None`** → no containment gate, byte-identical to pre-Phase-3.
    pub lidar_topic: Option<String>,
    /// Phase 3b — minimum corridor confidence for the gate's health check
    /// (`Corridor::min_confidence`). A live corridor below this fails closed (MRC).
    pub corridor_min_confidence: f32,
    /// Phase 3b — maximum corridor age (ms) for the gate's health check
    /// (`Corridor::max_age_ms`). A stale corridor fails closed (MRC).
    pub corridor_max_age_ms: u64,

    /// ADR-0029 Phase 3b (object axis) — enable the RSS object-avoidance gate
    /// (`taj_objects::apply_object_rss_gate`). When `true` AND lidar +
    /// `platform_profile` are configured, the node also feeds Taj's perceived
    /// OBJECTS through `compute_scene_rss` each tick (an object in the path → MRC;
    /// absent/stale object perception → fail-closed MRC). Reuses the lidar feed
    /// and `corridor_max_age_ms` as the object-perception staleness budget.
    /// **Default `false`** → byte-identical to the corridor-only Phase 3b.
    pub object_rss_enabled: bool,

    /// #309 remainder — enable the SG6 vanished-object detector. When `true` AND
    /// lidar + `platform_profile` are configured, the node sources an
    /// [`AgentScene`](parko_core::AgentScene) from Taj's perceived objects (the
    /// SAME snapshot the object-RSS gate uses) and feeds it to the node-owned
    /// `ClearanceLoop` each tick, so a close agent that VANISHES between frames
    /// (the person-under-vehicle case) latches `Normal → Latched` and immobilizes
    /// until an operator grant clears it. Opt-in and fail-safe: this arms a
    /// *latching auto-immobilizer*, so it stays OFF by default and is never
    /// silently enabled by `object_rss_enabled`. Missing/stale object perception →
    /// `AgentScene::Absent` (a gap — never a fabricated latch). **Default `false`.**
    pub vanished_detection_enabled: bool,

    /// WS-0.1 (#G2) — EXPLICIT operator acknowledgment that this deployment
    /// may MOVE without a scene-RSS producer. **Default `false` — fail-closed:**
    /// when the object-RSS gate is NOT armed (no lidar/profile or
    /// `object_rss_enabled = false`) and this is `false`, the node builds
    /// UNFED governors ([`parko_kirra::RssFeed::NeverFed`]) and the tick HOLDs
    /// at zero. Setting `true` builds externally-gated governors and is logged
    /// loudly at startup: the operator — not a silent construction default —
    /// owns the decision to drive blind.
    pub allow_motion_without_object_perception: bool,

    /// WS-0.1 — arm the RSS rule-iv occlusion gate
    /// (`scene_vetoes::apply_occlusion_gate`) at the publication seam. Armed
    /// only when a `platform_profile` is also configured (the RSS params).
    /// While armed, a missing/stale sightline scene (staleness budget:
    /// `corridor_max_age_ms`) fails CLOSED
    /// (`OcclusionScene::Absent` → cap 0.0 → stop): arm this only when a
    /// sightline producer feeds the node's occlusion slot. **Default `false`.**
    pub occlusion_gate_enabled: bool,

    /// WS-0.1 — arm the SG4 water-veto gate (`scene_vetoes::apply_water_gate`).
    /// While armed, a missing/stale water scene (staleness budget:
    /// `corridor_max_age_ms`) fails CLOSED
    /// (`WaterScene::Unknown` → veto): arm only with a water-detector producer.
    /// **Default `false`.**
    pub water_gate_enabled: bool,

    /// WS-0.1 — arm the SG5 commit-zone gate
    /// (`scene_vetoes::apply_commit_zone_gate`). While armed, a missing/stale
    /// zone scene (staleness budget: `corridor_max_age_ms`) fails CLOSED
    /// (`CommitZoneScene::Unknown` → veto): arm only
    /// with a map-anchored zone producer. **Default `false`.**
    pub commit_zone_gate_enabled: bool,

    /// #795 F6 — the SG4 water-veto bounds threaded into `apply_water_gate`
    /// (`max_exit_distance_m` / `max_puddle_extent_m`). Previously the drain loop
    /// hardcoded `WaterVetoConfig::default()`; carrying it on the config makes the
    /// (VALIDATION-PENDING) puddle bounds a deployment knob, one source shared by
    /// the gate and any diagnostics. **Default:** `WaterVetoConfig::default()`
    /// (byte-identical to the prior hardcode).
    pub water_veto_config: WaterVetoConfig,

    /// #795 F6 — the SG5 commit-zone geometry threaded into `apply_commit_zone_gate`
    /// (look-ahead / vehicle length / exit margin / clearance-time margin).
    /// Previously the drain loop hardcoded `CommitZoneCfg::default()`; carrying it
    /// here makes the (VALIDATION-PENDING) geometry a deployment knob. **Default:**
    /// `CommitZoneCfg::default()` (byte-identical to the prior hardcode).
    pub commit_zone_config: CommitZoneCfg,

    /// #1124 (SG5 go-live) — ego-pose topic (`nav_msgs/Odometry`, map frame)
    /// anchoring the map-anchored commit-zone producer. When set together with
    /// `commit_zone_spec`, the producer computes a fresh `CommitZoneScene` each
    /// tick from the latest pose (missing/stale/non-finite pose → `Unknown` →
    /// veto), and the commit-zone gate is no longer producer-less. **Default
    /// `None`.** Env: `PARKO_POSE_TOPIC`.
    pub pose_topic: Option<String>,

    /// #1124 (SG5 go-live) — the LOADED, VALIDATED commit-zone map spec (site-
    /// authored zone polygons; see `commit_zone_producer::parse_commit_zone_spec`).
    /// The binary loads it fail-closed from `PARKO_COMMIT_ZONE_MAP_PATH` (an
    /// unreadable or invalid spec ABORTS startup — never a partial zone map).
    /// Together with `pose_topic` this arms the producer
    /// ([`commit_zone_producer_armed`](Self::commit_zone_producer_armed)).
    /// **Default `None`.**
    pub commit_zone_spec: Option<crate::commit_zone_producer::CommitZoneSpec>,

    /// #795 F6 — EXPLICIT operator acknowledgment that a scene-veto gate
    /// (occlusion / water / commit-zone) may be ARMED even though this build ships
    /// **no producer** for its slot. An armed gate with no producer fails closed to
    /// a STOP every tick — a *permanent immobilization*. **Default `false` —
    /// fail-closed:** [`scene_gate_startup_check`](Self::scene_gate_startup_check)
    /// REFUSES startup (rather than silently immobilizing) when a producer-less gate
    /// is armed and this is unset. Set `true` only for a deliberate
    /// bring-up/immobilizer test; the operator — not a silent default — owns that
    /// choice. (When a real producer lands, that gate drops out of the guard.)
    pub allow_scene_gate_without_producer: bool,
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
            // WS-0.4: hung-backend deadline — always on, hang-scale default.
            inference_deadline_ms: parko_core::scheduler::DEFAULT_INFERENCE_DEADLINE_MS,
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
            // ADR-0029 Phase 3b: no lidar/containment gate by default. Opt-in.
            lidar_topic: None,
            corridor_min_confidence: 0.5,
            corridor_max_age_ms: 500,
            // ADR-0029 Phase 3b (object axis): object RSS gate off by default.
            // Opt-in; byte-identical to corridor-only Phase 3b when disabled.
            object_rss_enabled: false,
            // #309: SG6 vanished-object detection off by default — a latching
            // auto-immobilizer is opt-in, never silently enabled.
            vanished_detection_enabled: false,
            // WS-0.1 (#G2): fail-closed — motion without a scene-RSS producer
            // requires the operator's explicit acknowledgment.
            allow_motion_without_object_perception: false,
            // WS-0.1 scene-veto gates: armed-when-configured, off by default
            // (an armed gate with no producer fails closed to a stop).
            occlusion_gate_enabled: false,
            water_gate_enabled: false,
            commit_zone_gate_enabled: false,
            // #795 F6: veto configs plumbed (previously hardcoded in the drain
            // loop); default to their parko-core Default() → byte-identical.
            water_veto_config: WaterVetoConfig::default(),
            commit_zone_config: CommitZoneCfg::default(),
            // #1124: no pose channel / zone map by default — the commit-zone
            // producer arms only when BOTH are configured.
            pose_topic: None,
            commit_zone_spec: None,
            // #795 F6: a producer-less armed gate is a permanent immobilizer;
            // arming one requires the operator's explicit acknowledgment.
            allow_scene_gate_without_producer: false,
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

    /// The SG6 [`parko_core::VanishedCfg`] this node uses for the vanished-object
    /// frame-diff. The conservative defaults (close range 2.0 m, agent speed cap
    /// 3.0 m/s, 0.5 m band slack) are documented on `VanishedCfg`; the node-config
    /// surface does not tune them today (deployment-tunable when a need is real).
    #[must_use]
    pub fn vanished_cfg(&self) -> parko_core::VanishedCfg {
        parko_core::VanishedCfg::default()
    }

    /// Whether THIS node should source an object snapshot at all — true when the
    /// object-RSS gate OR the SG6 vanished detector needs it. The lidar task uses
    /// this to decide whether to populate the object slot (the vanished detector
    /// reuses the object-RSS snapshot, so either consumer arms the feed).
    #[must_use]
    pub fn needs_object_snapshot(&self) -> bool {
        self.object_rss_enabled || self.vanished_detection_enabled
    }

    /// #795 F3 — the SINGLE object-RSS-gate arming predicate.
    ///
    /// The scene-RSS object gate is ARMED only when the operator opted in
    /// (`object_rss_enabled`) AND both inputs it needs are configured: a lidar
    /// stream (`lidar_topic`, the object source) and a `platform_profile` (the
    /// footprint + RSS params). Missing either → the object slot would never be
    /// fed and the gate would MRC forever, so it stays DISARMED (fail-safe).
    ///
    /// This was previously re-derived independently in the bin's `main()` (which
    /// decides `with_external_rss_gate`) AND in `node.rs` (which decides whether
    /// the seam gate actually runs) — two textually-matched halves of ONE
    /// interlock with no shared function and no test. If they ever drifted, the
    /// governors could declare the scene tier externally-gated while the seam
    /// gate stayed dark (a latent fail-OPEN). Both now call this one method.
    #[must_use]
    pub fn object_gate_armed(&self) -> bool {
        self.object_rss_enabled && self.lidar_topic.is_some() && self.platform_profile.is_some()
    }

    /// #795 F6 — the scene-veto gates that are ARMED **and will actually
    /// immobilize**, in this build that ships no producers for their slots.
    ///
    /// Each such gate, with its slot never fed, fails closed to a STOP every tick
    /// — a permanent immobilization. Occlusion additionally needs a
    /// `platform_profile` (its RSS params) to arm at all: `occlusion_gate_enabled`
    /// with no profile is a *dark* flag (the gate is skipped, no immobilization),
    /// so it is NOT counted here — only a gate that will genuinely hold the ego at
    /// zero. Water and commit-zone need no profile, so the flag alone arms them.
    ///
    /// Returned by name (fixed order occlusion → water → commit-zone) so the
    /// startup guard can name exactly which gates are producer-less.
    ///
    /// #1124: commit-zone now has a REAL producer (the map-anchored
    /// `commit_zone_producer`), so an armed commit-zone gate counts as
    /// producer-less only when that producer is NOT configured
    /// ([`commit_zone_producer_armed`](Self::commit_zone_producer_armed)).
    #[must_use]
    pub fn producerless_armed_scene_gates(&self) -> Vec<&'static str> {
        let mut armed = Vec::new();
        if self.occlusion_gate_enabled && self.platform_profile.is_some() {
            armed.push("occlusion");
        }
        if self.water_gate_enabled {
            armed.push("water");
        }
        if self.commit_zone_gate_enabled && !self.commit_zone_producer_armed() {
            armed.push("commit_zone");
        }
        armed
    }

    /// #1124 — the SINGLE commit-zone-producer arming predicate (the same
    /// one-predicate discipline as [`object_gate_armed`](Self::object_gate_armed)):
    /// the map-anchored producer runs only when BOTH its inputs are configured —
    /// the validated zone spec (`commit_zone_spec`, loaded fail-closed by the
    /// binary) and the ego-pose channel (`pose_topic`, the anchor). Missing
    /// either → the producer cannot anchor the map prior, so it stays dark and
    /// an armed commit-zone gate is still counted producer-less (startup
    /// refusal, #795 F6). node.rs's per-tick fill and this guard both call this
    /// one method so the two halves cannot drift.
    #[must_use]
    pub fn commit_zone_producer_armed(&self) -> bool {
        self.commit_zone_spec.is_some() && self.pose_topic.is_some()
    }

    /// #795 F6 — fail-closed startup guard against SILENT permanent immobilization.
    ///
    /// A scene-veto gate armed with no producer feeding its slot fails closed to a
    /// STOP forever. Rather than let the node come up and hold the ego at zero with
    /// no diagnostic, REFUSE startup when any producer-less gate
    /// ([`producerless_armed_scene_gates`](Self::producerless_armed_scene_gates)) is
    /// armed — UNLESS the operator has explicitly acknowledged the immobilization
    /// via `allow_scene_gate_without_producer` (a deliberate bring-up/immobilizer
    /// test). Default config → no armed gates → `Ok`, so ordinary deployments are
    /// unaffected. When a real producer for a gate lands, that gate drops out of
    /// `producerless_armed_scene_gates` and no longer trips this guard.
    ///
    /// # Errors
    /// Returns [`SceneGateStartupRefusal`] naming the armed-without-producer gates
    /// when the acknowledgment is unset.
    pub fn scene_gate_startup_check(&self) -> Result<(), SceneGateStartupRefusal> {
        let armed = self.producerless_armed_scene_gates();
        if armed.is_empty() || self.allow_scene_gate_without_producer {
            Ok(())
        } else {
            SceneGateStartupRefusal::from_gates(&armed)
        }
    }
}

/// #795 F6 — a fail-closed startup refusal: one or more scene-veto gates are
/// ARMED but have no producer, so the node would come up permanently immobilized.
/// Names the offending gates so the operator can either configure a producer,
/// disarm the gate, or (deliberately) acknowledge the immobilization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneGateStartupRefusal {
    /// The armed-without-producer gate names (occlusion / water / commit_zone).
    pub armed_gates: Vec<String>,
}

impl SceneGateStartupRefusal {
    fn from_gates(gates: &[&str]) -> Result<(), Self> {
        Err(Self {
            armed_gates: gates.iter().map(|g| (*g).to_string()).collect(),
        })
    }
}

impl std::fmt::Display for SceneGateStartupRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scene-veto gate(s) [{}] are ARMED but this build ships no producer for \
             their slots — the node would come up PERMANENTLY IMMOBILIZED (fail-closed \
             STOP every tick). Configure a producer, disarm the gate(s), or set \
             PARKO_ALLOW_SCENE_GATE_WITHOUT_PRODUCER=1 to acknowledge the immobilization.",
            self.armed_gates.join(", ")
        )
    }
}

impl std::error::Error for SceneGateStartupRefusal {}

/// #795 F10 — classification of a boolean env-flag value. `1`/`true` → enabled;
/// `0`/`false`/empty → disabled; anything ELSE (`yes`, `on`, a typo) →
/// `Unrecognized`, so the reader can WARN instead of SILENTLY treating a
/// mis-spelled enable as "off" (which would leave a safety gate disarmed with
/// no signal). Lives HERE (non-`ros2`-gated) rather than in the `ros2`-only
/// node binary, so it is unit-tested in the default CI build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvFlagValue {
    Set(bool),
    Unrecognized,
}

/// Classify a raw env value (case-insensitive, trimmed). See [`EnvFlagValue`].
#[must_use]
pub fn classify_env_flag(raw: &str) -> EnvFlagValue {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => EnvFlagValue::Set(true),
        "0" | "false" | "" => EnvFlagValue::Set(false),
        _ => EnvFlagValue::Unrecognized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_recognizes_true_false_tokens() {
        for t in ["1", "true", "TRUE", " True ", "tRuE"] {
            assert_eq!(classify_env_flag(t), EnvFlagValue::Set(true), "{t:?}");
        }
        for f in ["0", "false", "FALSE", " ", ""] {
            assert_eq!(classify_env_flag(f), EnvFlagValue::Set(false), "{f:?}");
        }
    }

    /// #795 F10 — a mis-spelled enable is UNRECOGNIZED (not silently false), so
    /// the reader can warn instead of quietly leaving a safety gate disarmed.
    #[test]
    fn env_flag_misspelled_enables_are_unrecognized() {
        for u in ["yes", "on", "y", "enable", "enabled", "2", "tru"] {
            assert_eq!(classify_env_flag(u), EnvFlagValue::Unrecognized, "{u:?}");
        }
    }

    /// #795 F3 — the object-gate arming predicate is the AND of all three
    /// inputs, over the FULL {object_rss} × {lidar} × {profile} truth table.
    /// The bin and node.rs now share THIS, so the interlock cannot drift.
    #[test]
    fn object_gate_armed_is_the_and_of_all_three_inputs() {
        for object_rss in [false, true] {
            for has_lidar in [false, true] {
                for has_profile in [false, true] {
                    let cfg = ParkoNodeConfig {
                        object_rss_enabled: object_rss,
                        lidar_topic: has_lidar.then(|| "/lidar".to_string()),
                        platform_profile: has_profile
                            .then(CourierPlatformProfile::courier_reference),
                        ..Default::default()
                    };
                    let expected = object_rss && has_lidar && has_profile;
                    assert_eq!(
                        cfg.object_gate_armed(),
                        expected,
                        "object_rss={object_rss} lidar={has_lidar} profile={has_profile}"
                    );
                }
            }
        }
    }

    /// #795 F6 — the default (all gates disarmed) has nothing producer-less and
    /// the startup guard passes: ordinary deployments are unaffected.
    #[test]
    fn scene_gate_guard_passes_by_default() {
        let cfg = ParkoNodeConfig::default();
        assert!(cfg.producerless_armed_scene_gates().is_empty());
        assert!(cfg.scene_gate_startup_check().is_ok());
        // The veto configs default to parko-core Default() (byte-identical to the
        // prior drain-loop hardcode). WaterVetoConfig/CommitZoneCfg don't derive
        // PartialEq, so compare their fields.
        let w = WaterVetoConfig::default();
        assert!((cfg.water_veto_config.max_exit_distance_m - w.max_exit_distance_m).abs() < 1e-9);
        assert!((cfg.water_veto_config.max_puddle_extent_m - w.max_puddle_extent_m).abs() < 1e-9);
        let cz = CommitZoneCfg::default();
        assert!((cfg.commit_zone_config.look_ahead_m - cz.look_ahead_m).abs() < 1e-9);
        assert!((cfg.commit_zone_config.vehicle_length_m - cz.vehicle_length_m).abs() < 1e-9);
        assert!((cfg.commit_zone_config.exit_margin_m - cz.exit_margin_m).abs() < 1e-9);
        assert!(
            (cfg.commit_zone_config.clearance_time_margin_s - cz.clearance_time_margin_s).abs()
                < 1e-9
        );
        assert!(!cfg.allow_scene_gate_without_producer);
    }

    /// #795 F6 — arming water or commit-zone (no profile needed) makes them
    /// producer-less-armed and REFUSES startup; the refusal names the gate.
    #[test]
    fn arming_water_or_commit_zone_refuses_startup() {
        for (field, name) in [("water", "water"), ("commit_zone", "commit_zone")] {
            let cfg = ParkoNodeConfig {
                water_gate_enabled: field == "water",
                commit_zone_gate_enabled: field == "commit_zone",
                ..ParkoNodeConfig::default()
            };
            assert_eq!(cfg.producerless_armed_scene_gates(), vec![name]);
            let err = cfg
                .scene_gate_startup_check()
                .expect_err("an armed producer-less gate must refuse startup");
            assert_eq!(err.armed_gates, vec![name.to_string()]);
            assert!(err.to_string().contains(name));
        }
    }

    /// #795 F6 — the occlusion gate only immobilizes when it actually ARMS, which
    /// needs a platform_profile (its RSS params). The flag WITHOUT a profile is a
    /// dark flag (gate skipped) → not counted, guard passes; WITH a profile it is
    /// producer-less-armed → refused.
    #[test]
    fn occlusion_counts_only_when_it_actually_arms() {
        let dark = ParkoNodeConfig {
            occlusion_gate_enabled: true,
            platform_profile: None,
            ..ParkoNodeConfig::default()
        };
        assert!(
            dark.producerless_armed_scene_gates().is_empty(),
            "occlusion flag with no profile does not arm → not an immobilizer"
        );
        assert!(dark.scene_gate_startup_check().is_ok());

        let armed = ParkoNodeConfig {
            occlusion_gate_enabled: true,
            platform_profile: Some(CourierPlatformProfile::courier_reference()),
            ..ParkoNodeConfig::default()
        };
        assert_eq!(armed.producerless_armed_scene_gates(), vec!["occlusion"]);
        assert!(armed.scene_gate_startup_check().is_err());
    }

    /// #795 F6 — the explicit operator acknowledgment turns the refusal into an
    /// (allowed, deliberate) immobilizer: the guard passes even with gates armed.
    #[test]
    fn acknowledgment_allows_producerless_immobilizer() {
        let cfg = ParkoNodeConfig {
            water_gate_enabled: true,
            commit_zone_gate_enabled: true,
            allow_scene_gate_without_producer: true,
            ..ParkoNodeConfig::default()
        };
        // Still reported as producer-less (observability), but startup is allowed.
        assert_eq!(
            cfg.producerless_armed_scene_gates(),
            vec!["water", "commit_zone"]
        );
        assert!(
            cfg.scene_gate_startup_check().is_ok(),
            "an acknowledged immobilizer may start"
        );
    }

    /// #1124 — a minimal valid loaded zone spec for the producer-arming tests.
    fn loaded_spec() -> crate::commit_zone_producer::CommitZoneSpec {
        crate::commit_zone_producer::parse_commit_zone_spec(
            r#"{ "version": 1, "zones": [
                { "id": "z", "kind": "rail_crossing",
                  "polygon": [[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]] } ] }"#,
        )
        .expect("fixture spec parses")
    }

    /// #1124 — the producer arms only with BOTH inputs (spec AND pose topic);
    /// either alone stays dark (and an armed gate stays producer-less).
    #[test]
    fn commit_zone_producer_arming_truth_table() {
        for (spec, topic, expect) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let cfg = ParkoNodeConfig {
                commit_zone_spec: spec.then(loaded_spec),
                pose_topic: topic.then(|| "/odom".to_string()),
                ..ParkoNodeConfig::default()
            };
            assert_eq!(
                cfg.commit_zone_producer_armed(),
                expect,
                "spec={spec} topic={topic}"
            );
        }
    }

    /// #1124 — the go-live: an armed commit-zone gate WITH the producer
    /// configured is no longer producer-less, so startup passes with NO
    /// acknowledgment flag. A partially-configured producer (spec without pose
    /// topic, or vice versa) still refuses.
    #[test]
    fn commit_zone_gate_with_producer_passes_startup() {
        let cfg = ParkoNodeConfig {
            commit_zone_gate_enabled: true,
            commit_zone_spec: Some(loaded_spec()),
            pose_topic: Some("/odom".to_string()),
            ..ParkoNodeConfig::default()
        };
        assert!(cfg.producerless_armed_scene_gates().is_empty());
        assert!(
            cfg.scene_gate_startup_check().is_ok(),
            "an armed gate with a configured producer must start without the \
             immobilizer acknowledgment"
        );

        for (spec, topic) in [(true, false), (false, true)] {
            let partial = ParkoNodeConfig {
                commit_zone_gate_enabled: true,
                commit_zone_spec: spec.then(loaded_spec),
                pose_topic: topic.then(|| "/odom".to_string()),
                ..ParkoNodeConfig::default()
            };
            assert_eq!(
                partial.producerless_armed_scene_gates(),
                vec!["commit_zone"],
                "a half-configured producer (spec={spec} topic={topic}) must still \
                 count as producer-less"
            );
            assert!(partial.scene_gate_startup_check().is_err());
        }
    }

    /// #1124 — configuring the producer WITHOUT arming the gate is inert for the
    /// startup guard (nothing armed → nothing producer-less), and the water gate
    /// is unaffected by the commit-zone producer.
    #[test]
    fn commit_zone_producer_without_gate_is_inert() {
        let cfg = ParkoNodeConfig {
            commit_zone_spec: Some(loaded_spec()),
            pose_topic: Some("/odom".to_string()),
            ..ParkoNodeConfig::default()
        };
        assert!(cfg.producerless_armed_scene_gates().is_empty());
        assert!(cfg.scene_gate_startup_check().is_ok());

        let water = ParkoNodeConfig {
            water_gate_enabled: true,
            commit_zone_spec: Some(loaded_spec()),
            pose_topic: Some("/odom".to_string()),
            ..ParkoNodeConfig::default()
        };
        assert_eq!(
            water.producerless_armed_scene_gates(),
            vec!["water"],
            "the commit-zone producer must not vouch for the water gate"
        );
    }

    #[test]
    fn default_tick_period_is_20hz() {
        let cfg = ParkoNodeConfig::default();
        assert!((cfg.tick_period_s - 0.05).abs() < 1e-9);
        assert_eq!(cfg.tick_period(), Duration::from_millis(50));
    }

    #[test]
    fn default_num_threads_is_single_threaded_reproducible() {
        let cfg = ParkoNodeConfig::default();
        assert_eq!(
            cfg.num_threads, 1,
            "default must be single-threaded (preserves #152)"
        );
        // The ONE source both backends read.
        let t = cfg.inference_threads();
        assert_eq!(t.num_threads, 1);
        assert!(t.bitwise_reproducible());
    }

    #[test]
    fn inference_threads_reflects_configured_count() {
        let cfg = ParkoNodeConfig {
            num_threads: 4,
            ..ParkoNodeConfig::default()
        };
        assert_eq!(cfg.inference_threads().num_threads, 4);
        assert!(
            !cfg.inference_threads().bitwise_reproducible(),
            "raising threads gives up bitwise reproducibility"
        );
    }

    #[test]
    fn sg6_detection_sources_default_off_and_threshold_is_parko_core_default() {
        let cfg = ParkoNodeConfig::default();
        assert!(
            cfg.imu_topic.is_none(),
            "IMU source off by default (reduced coverage, stated)"
        );
        assert!(cfg.contact_topic.is_none(), "contact source off by default");
        // The threshold mirrors parko-core's ImpactCfg default, and impact_cfg()
        // round-trips it.
        let core_default = parko_core::ImpactCfg::default().spike_threshold_mps2;
        assert!((cfg.spike_threshold_mps2 - core_default).abs() < 1e-9);
        assert!((cfg.impact_cfg().spike_threshold_mps2 - core_default).abs() < 1e-9);
    }

    #[test]
    fn impact_cfg_reflects_a_tuned_threshold() {
        let cfg = ParkoNodeConfig {
            spike_threshold_mps2: 22.5,
            ..ParkoNodeConfig::default()
        };
        assert!(
            (cfg.impact_cfg().spike_threshold_mps2 - 22.5).abs() < 1e-9,
            "impact_cfg() must carry the deployment-tuned threshold"
        );
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
    fn default_has_no_lidar_containment_gate() {
        // ADR-0029 Phase 3b is opt-in: no lidar topic by default → the
        // containment gate is not armed (byte-identical to pre-Phase-3). The
        // health thresholds default to the kernel slow-loop values.
        let cfg = ParkoNodeConfig::default();
        assert!(
            cfg.lidar_topic.is_none(),
            "lidar/containment gate must be opt-in"
        );
        assert!((cfg.corridor_min_confidence - 0.5).abs() < 1e-9);
        assert_eq!(cfg.corridor_max_age_ms, 500);
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
        assert!(
            cfg.tick_period() >= Duration::from_micros(900),
            "tick_period must floor above zero to avoid a busy-spin"
        );
    }
}
