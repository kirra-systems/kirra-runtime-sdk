// crates/kirra-ros2-adapter/src/state.rs
//
// AcceptedTrajectory state machine + AdaptorState.
//
// The Governor's slow loop validates each new Trajectory candidate and, on
// Accept, promotes it to the per-asset `AcceptedTrajectory` slot. The fast
// loop conforms outgoing control commands to that slot. Absent or stale →
// fail-closed (MRCFallback).
//
// This module is the seam between the ROS 2 subscription side (which
// produces `AcceptedTrajectory` instances on Accept) and the verdict side
// (which reads the slot per cycle). It is ROS-feature-independent and
// builds in any configuration.
//
// Phase 1 scope: pure types + state machine + tests. No verdict logic, no
// validation. Phase 2 wires `validate_trajectory_containment` from
// `kirra-runtime-sdk` (added as a dep in that phase) and the slow loop in.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use kirra_core::FleetPosture;

use crate::config::VehicleConfig;
use kirra_core::frame_integrity::{
    resolve_frame_trust, FrameIntegrity, FrameIntegrityCfg, FrameTrust,
};
use kirra_core::posture_tracker::PostureTracker;

// The lean trajectory/perception data types now live in the `kirra-core` crate
// (de-monolith Stage 6a); re-exported here so every existing `crate::state::*` /
// `kirra_ros2_adapter::state::*` path keeps the SAME type. `AdaptorState` and
// `AcceptedTrajectory` (below, both heavy/DashMap-coupled — they stay) consume them
// unchanged.
pub use kirra_core::trajectory::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict};

// `Pose`, `TrajectoryPoint`, and `TrajectoryVerdict` were relocated verbatim to
// `kirra_core::trajectory` (de-monolith Stage 6a) and are re-exported above.

/// The per-asset accepted-trajectory record held in `AdaptorState`.
#[derive(Debug, Clone)]
pub struct AcceptedTrajectory {
    pub asset_id: String,
    /// Opaque monotonic id (planner-assigned or adapter-assigned). Used to
    /// detect duplicate publications of the same candidate.
    pub trajectory_id: u64,
    pub points: Vec<TrajectoryPoint>,
    pub verdict: TrajectoryVerdict,
    /// Wall-clock ms when this trajectory was promoted into the slot. The
    /// fast loop computes age against `now_ms` for staleness.
    pub promoted_at_ms: u64,
    /// Hard staleness cap. After `now_ms - promoted_at_ms >= max_age_ms`,
    /// `is_stale()` returns true and `fail_closed()` collapses to MRC.
    /// Default: one planning-cycle budget (~200 ms at 10 Hz, doubled for
    /// jitter — see `DEFAULT_MAX_AGE_MS`).
    pub max_age_ms: u64,
}

/// Default max age for an accepted trajectory: 200 ms. Sized so that one
/// missed planning cycle (at the typical Autoware 10 Hz planning rate)
/// still leaves headroom; a SECOND missed cycle exceeds it and the slot
/// fails closed. The design's per-trajectory FTTI budget (§4).
pub const DEFAULT_MAX_AGE_MS: u64 = 200;

/// Default subscription-staleness timeout (ms). Phase 4: the adapter's
/// own SG9 fail-closed path. If any of the REQUIRED upstream
/// subscriptions (trajectory / objects / odometry) hasn't delivered a
/// message within this window, the fast loop publishes MRC regardless
/// of any other state.
///
/// 500 ms = ~5× one planning cycle at 10 Hz; conservative without
/// being twitchy. Configurable at startup via
/// `KIRRA_SUBSCRIPTION_STALENESS_MS`.
pub const SUBSCRIPTION_STALENESS_TIMEOUT_MS: u64 = 500;

impl AcceptedTrajectory {
    /// Constructs a freshly-accepted trajectory record. The slow loop
    /// calls this on a verdict::Accept; the fast loop only reads.
    pub fn new_accepted(
        asset_id: impl Into<String>,
        trajectory_id: u64,
        points: Vec<TrajectoryPoint>,
        promoted_at_ms: u64,
    ) -> Self {
        Self {
            asset_id: asset_id.into(),
            trajectory_id,
            points,
            verdict: TrajectoryVerdict::Accept,
            promoted_at_ms,
            max_age_ms: DEFAULT_MAX_AGE_MS,
        }
    }

    /// Constructs a record with a specific verdict (Accept / Clamp /
    /// MRCFallback / Pending). Slow loop uses this to record the
    /// derate-only path (`Clamp`) without losing the trajectory bytes
    /// the audit chain needs.
    pub fn with_verdict(
        asset_id: impl Into<String>,
        trajectory_id: u64,
        points: Vec<TrajectoryPoint>,
        verdict: TrajectoryVerdict,
        promoted_at_ms: u64,
    ) -> Self {
        Self {
            asset_id: asset_id.into(),
            trajectory_id,
            points,
            verdict,
            promoted_at_ms,
            max_age_ms: DEFAULT_MAX_AGE_MS,
        }
    }

    /// Wall-clock staleness check. Uses `saturating_sub` so a clock skew
    /// that puts `now_ms` behind `promoted_at_ms` reads as "not yet
    /// stale" (the only safe disposition; the fail-closed direction would
    /// be a panic, which we never want on the fast loop).
    #[must_use]
    pub fn is_stale(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.promoted_at_ms) >= self.max_age_ms
    }

    /// The fail-closed collapse: anything other than a fresh Accept or
    /// fresh Clamp returns `MRCFallback`. Used by the fast loop when
    /// reading the slot; isolates the policy in one place so we never
    /// silently leak a stale Accept into a verdict.
    ///
    /// Clamp is permitted because the slow loop only emits Clamp on a
    /// trajectory that PASSED containment + RSS — the caller's per-pose
    /// velocity is derated, but staying on the corridor + collision-free
    /// at any speed ≤ derate is still safe. Phase 3 conformance enforces
    /// the derate.
    #[must_use]
    pub fn fail_closed(&self, now_ms: u64) -> TrajectoryVerdict {
        if self.is_stale(now_ms) {
            return TrajectoryVerdict::MRCFallback;
        }
        match self.verdict {
            TrajectoryVerdict::Accept => TrajectoryVerdict::Accept,
            TrajectoryVerdict::Clamp  => TrajectoryVerdict::Clamp,
            _ => TrajectoryVerdict::MRCFallback,
        }
    }
}

// `PerceivedObject` was relocated verbatim to `kirra_core::trajectory`
// (de-monolith Stage 6a) and is re-exported above.

/// Phase 4c — typed-payload envelope for a freshly-received trajectory
/// after the r2r-side parser has extracted the planner-published points.
/// The slow-loop receives this on the trajectory channel; the drain
/// task in `node.rs::run_adapter` builds it from
/// `autoware_planning_msgs::msg::Trajectory` via `parsing::parse_trajectory`.
///
/// `received_ms` is the wall-clock at the drain-task's receipt of the
/// r2r message (the same value stamped into `AdaptorState::last_trajectory_ms`
/// for the SG9 subscription-staleness path). Phase 4c uses it for
/// per-trajectory liveness logging; Phase 5 may use it for slow-loop
/// FTTI budget enforcement (drop trajectories that arrived too long ago
/// before the slow loop got to them).
#[derive(Debug, Clone)]
pub struct IncomingTrajectory {
    pub points: Vec<TrajectoryPoint>,
    pub received_ms: u64,
}

/// Minimal ego-odometry snapshot. Phase 3 introduces this to fix the
/// `current_steering_angle_deg = 0.0` approximation in
/// `validate_trajectory_slow` AND to feed the fast-loop conformance
/// check the current ego velocity for the staleness / nearest-point
/// lookup.
///
/// `linear_x_mps` is the ego longitudinal velocity in the vehicle frame
/// (from `nav_msgs::Odometry::twist.twist.linear.x`). `yaw_rate_rads`
/// is the angular velocity around the vertical axis (from
/// `twist.twist.angular.z`). `stamp_ms` is the message timestamp in
/// wall-clock ms — used to detect a stale odom snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EgoOdom {
    pub linear_x_mps: f64,
    pub yaw_rate_rads: f64,
    pub stamp_ms: u64,
}

impl Default for EgoOdom {
    fn default() -> Self {
        Self { linear_x_mps: 0.0, yaw_rate_rads: 0.0, stamp_ms: 0 }
    }
}

/// Per-asset accepted-trajectory store + perception cache + vehicle config.
/// DashMap on the trajectory side fits the existing AppState concurrency
/// model: the slow loop installs / updates entries; the fast loop reads
/// them per cycle without contention. The objects cache is an
/// `Arc<RwLock<Vec<_>>>` because perception ticks REPLACE the snapshot
/// rather than mutate in place — write contention is the perception
/// publisher only.
///
/// The Arc-wrapped public newtype keeps the trait object form `Send + Sync`
/// for the async tasks the adapter spawns.
#[derive(Debug)]
pub struct AdaptorState {
    by_asset: DashMap<String, AcceptedTrajectory>,
    /// Latest perception snapshot. Reads take a read lock at the start of
    /// validation and CLONE; the slow loop does NOT hold the lock across
    /// the computation. Writes replace the whole vector.
    pub objects_cache: Arc<RwLock<Vec<PerceivedObject>>>,
    /// SECOND, independent perception snapshot (the True-Redundancy analog, gap #2b). Written by
    /// an optional redundant PredictedObjects subscriber; the slow loop cross-checks it against
    /// `objects_cache` when the divergence monitor is enabled. Empty/never-written when no
    /// redundant channel is configured (the monitor stays inert).
    pub objects_cache_b: Arc<RwLock<Vec<PerceivedObject>>>,
    /// Per-asset vehicle config (Phase 2A: single config, shared). Phase
    /// 4 may make this per-asset.
    pub config: Arc<VehicleConfig>,
    /// Latest ego odometry snapshot. `Option` is `None` from boot until
    /// the first `nav_msgs::Odometry` lands. Slow loop reads it to derive
    /// `current_steering_angle_deg` for the FIRST pose-pair (Phase 3 fix);
    /// fast loop reads it for the conformance check.
    pub latest_odom: Arc<RwLock<Option<EgoOdom>>>,

    /// Wall-clock-ms when the LAST trajectory message arrived (0 = none
    /// yet). Phase 4 subscription staleness — the fast loop checks this
    /// against `SUBSCRIPTION_STALENESS_TIMEOUT_MS` and MRCs if exceeded.
    pub last_trajectory_ms: Arc<AtomicU64>,
    /// Wall-clock-ms when the LAST PredictedObjects message arrived.
    pub last_objects_ms:    Arc<AtomicU64>,
    /// Wall-clock-ms when the LAST *redundant* (channel-B) PredictedObjects message arrived
    /// (0 = none yet). The slow loop checks it against the staleness timeout: a configured
    /// redundant channel that goes silent is a redundancy loss → fail closed.
    pub last_objects_b_ms:  Arc<AtomicU64>,
    /// Wall-clock-ms when the LAST nav_msgs::Odometry message arrived.
    pub last_odom_ms:       Arc<AtomicU64>,

    /// Fail-closed fleet-posture state machine, consumed by
    /// `validate_trajectory_slow` to select the effective kinematics
    /// contract:
    ///
    ///   - `Nominal`   → `VehicleConfig::to_kinematics_contract()` (full envelope)
    ///   - `Degraded`  → `VehicleConfig::to_mrc_kinematics_contract()` (MRC cap)
    ///   - `LockedOut` → short-circuit to `TrajectoryVerdict::MRCFallback`
    ///
    /// **M1 path** (`AdaptorState::new` / `with_config`): the tracker is
    /// constructed in `nominal_default_no_source` mode — `current_posture`
    /// always returns `Nominal`, behaviour byte-for-byte unchanged from
    /// the pre-M1b era. Use this for verifier-less deployments and unit
    /// tests.
    ///
    /// **M1b path** (`AdaptorState::with_posture_source`): the tracker is
    /// constructed in `with_source` mode — pre-first-event seed
    /// = `Degraded`; staleness derates Nominal → Degraded; `LockedOut`
    /// is sticky-toward-safe. The SSE subscriber task in the binary
    /// drives `update_posture` whenever a posture event arrives.
    pub posture_tracker: Arc<RwLock<PostureTracker>>,

    /// Latest integrator-reported **frame integrity** (S-FI1 live source). `None` from boot
    /// AND until the first `update_frame_integrity` — which the slow loop reads as the
    /// AOU-LOCALIZATION-001 seam (`FrameTrust::Trusted`), i.e. the integrator asserts the
    /// frame is correct externally (backward-compatible with the pre-wiring always-Trusted).
    /// Once a source reports, the gate is LIVE: a poor / non-finite ε derates the containment
    /// margin (Trusted→Degraded) or refuses (Untrusted), and a source that goes SILENT past
    /// `max_age_ms` fails closed to `Untrusted`. See [`AdaptorState::snapshot_frame_trust`].
    pub latest_frame_integrity: Arc<RwLock<Option<FrameIntegrity>>>,
    /// Wall-clock-ms of the last `update_frame_integrity` (0 = a source has never reported →
    /// AoU seam). Used to fail closed when a once-live source goes silent.
    pub last_frame_integrity_ms: Arc<AtomicU64>,
}

#[inline]
fn now_ms_wall() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl AdaptorState {
    pub fn new() -> Arc<Self> {
        let config = VehicleConfig::default_urban();
        config.warn_if_missing_odd_cap();
        Arc::new(Self {
            by_asset: DashMap::new(),
            objects_cache: Arc::new(RwLock::new(Vec::new())),
            objects_cache_b: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(config),
            latest_odom: Arc::new(RwLock::new(None)),
            last_trajectory_ms: Arc::new(AtomicU64::new(0)),
            last_objects_ms:    Arc::new(AtomicU64::new(0)),
            last_objects_b_ms:  Arc::new(AtomicU64::new(0)),
            last_odom_ms:       Arc::new(AtomicU64::new(0)),
            posture_tracker:    Arc::new(RwLock::new(
                PostureTracker::nominal_default_no_source())),
            latest_frame_integrity: Arc::new(RwLock::new(None)),
            last_frame_integrity_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Constructs an `AdaptorState` with a specific vehicle config. Used
    /// when the integrator's vehicle profile diverges from
    /// `default_urban()` (e.g. shuttle, light truck).
    pub fn with_config(config: VehicleConfig) -> Arc<Self> {
        config.warn_if_missing_odd_cap();
        Arc::new(Self {
            by_asset: DashMap::new(),
            objects_cache: Arc::new(RwLock::new(Vec::new())),
            objects_cache_b: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(config),
            latest_odom: Arc::new(RwLock::new(None)),
            last_trajectory_ms: Arc::new(AtomicU64::new(0)),
            last_objects_ms:    Arc::new(AtomicU64::new(0)),
            last_objects_b_ms:  Arc::new(AtomicU64::new(0)),
            last_odom_ms:       Arc::new(AtomicU64::new(0)),
            posture_tracker:    Arc::new(RwLock::new(
                PostureTracker::nominal_default_no_source())),
            latest_frame_integrity: Arc::new(RwLock::new(None)),
            last_frame_integrity_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// **M1b** — constructs an `AdaptorState` with a live posture source
    /// configured. The `PostureTracker` starts in the source-configured
    /// mode (pre-first-event seed = `Degraded`); the SSE subscriber in
    /// the binary drives `update_posture` with each event from the
    /// verifier's `/system/posture/stream`. See
    /// `crate::posture_tracker::PostureTracker::with_source`.
    pub fn with_posture_source(config: VehicleConfig) -> Arc<Self> {
        config.warn_if_missing_odd_cap();
        Arc::new(Self {
            by_asset: DashMap::new(),
            objects_cache: Arc::new(RwLock::new(Vec::new())),
            objects_cache_b: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(config),
            latest_odom: Arc::new(RwLock::new(None)),
            last_trajectory_ms: Arc::new(AtomicU64::new(0)),
            last_objects_ms:    Arc::new(AtomicU64::new(0)),
            last_objects_b_ms:  Arc::new(AtomicU64::new(0)),
            last_odom_ms:       Arc::new(AtomicU64::new(0)),
            posture_tracker:    Arc::new(RwLock::new(
                PostureTracker::with_source())),
            latest_frame_integrity: Arc::new(RwLock::new(None)),
            last_frame_integrity_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Feeds the posture tracker with a fresh observation from the
    /// configured source. No-op for the no-source tracker (preserves
    /// the M1 default-Nominal behaviour). The SSE subscriber calls
    /// this on every received posture event.
    ///
    /// Poisoned-lock recovery: a panicked writer leaves the cell
    /// poisoned but readable. We mark-and-replace via `into_inner` of
    /// the guard so subsequent reads return the new value rather than
    /// inheriting the poison.
    pub fn update_posture(&self, posture: FleetPosture) {
        let now = now_ms_wall();
        match self.posture_tracker.write() {
            Ok(mut guard) => guard.observe(now, posture),
            Err(poisoned) => poisoned.into_inner().observe(now, posture),
        }
    }

    /// Reads the effective fleet posture from the tracker at the current
    /// wall-clock instant. Fail-closed: a poisoned lock returns
    /// `Degraded` rather than `Nominal` so a panic in the writer can
    /// never widen the envelope.
    pub fn current_posture(&self) -> FleetPosture {
        let now = now_ms_wall();
        match self.posture_tracker.read() {
            Ok(guard) => guard.current_posture(now),
            // Defensive — if the lock is poisoned and we can't read it,
            // assume the worst-tractable posture (Degraded) rather than
            // Nominal. A poisoned tracker on a source-configured
            // deployment would otherwise risk a stale Nominal leak.
            Err(_) => FleetPosture::Degraded,
        }
    }

    /// Replaces the ego-odometry snapshot. Called by the adapter's
    /// `nav_msgs::Odometry` subscriber on each tick.
    pub fn update_odom(&self, odom: EgoOdom) {
        if let Ok(mut guard) = self.latest_odom.write() {
            *guard = Some(odom);
        } else {
            tracing::error!(
                "latest_odom RwLock POISONED — ego-odometry snapshot dropped"
            );
        }
    }

    /// Read-and-clone of the latest ego-odometry snapshot. Fast-loop +
    /// slow-loop both use this. Returns `None` until the first odom
    /// message lands; callers MUST treat `None` as "no estimate
    /// available" and fall back conservatively.
    pub fn snapshot_odom(&self) -> Option<EgoOdom> {
        self.latest_odom.read().ok().and_then(|g| *g)
    }

    /// Feed the integrator's per-tick **frame-integrity** report (S-FI1 live source) — the
    /// integrator contract this gate was built for. Stamps arrival so a source that later goes
    /// silent fails closed. The integrator's localization / pose-quality stack calls this each
    /// tick it has an estimate; NOT calling it keeps the AOU-LOCALIZATION-001 seam (Trusted).
    pub fn update_frame_integrity(&self, report: FrameIntegrity, now_ms: u64) {
        if let Ok(mut guard) = self.latest_frame_integrity.write() {
            *guard = Some(report);
            // Never store 0 once a source has reported (0 means "never reported" → AoU seam).
            self.last_frame_integrity_ms.store(now_ms.max(1), Ordering::Relaxed);
        } else {
            tracing::error!("latest_frame_integrity RwLock POISONED — frame-integrity report dropped");
        }
    }

    /// Resolve the live [`FrameTrust`] the S-FI1 containment gate should use at `now_ms`:
    ///   - a source has NEVER reported (`last == 0`) → `Trusted`: the AOU-LOCALIZATION-001 seam
    ///     (the integrator asserts the frame externally) — byte-for-byte the pre-wiring default;
    ///   - a once-live source has gone SILENT (`now − last > max_age_ms`) → `Untrusted`,
    ///     fail-closed (an absent pose-quality signal is NOT "the pose is fine" — the #238 trap);
    ///   - else resolve the latest report through [`resolve_frame_trust`] (which itself fails
    ///     closed on a non-finite ε, a report-internal stale `age_ms`, or `ε > fallback`).
    #[must_use]
    pub fn snapshot_frame_trust(&self, now_ms: u64) -> FrameTrust {
        let cfg = FrameIntegrityCfg::default();
        let last = self.last_frame_integrity_ms.load(Ordering::Relaxed);
        if last == 0 {
            return FrameTrust::Trusted; // no source wired → AoU seam (backward-compatible)
        }
        if now_ms.saturating_sub(last) > cfg.max_age_ms {
            return FrameTrust::Untrusted; // a once-live source went silent → fail closed
        }
        match self.latest_frame_integrity.read().ok().and_then(|g| *g) {
            Some(report) => resolve_frame_trust(&report, &cfg),
            None => FrameTrust::Untrusted, // last>0 but no report → fail closed
        }
    }

    /// Subscription staleness check (SG9). Returns true if ANY of the
    /// three required upstream subscriptions has not delivered a
    /// message within `timeout_ms` of `now_ms` — including the
    /// "never received" case (`last_*_ms == 0` is treated as stale
    /// after `timeout_ms` of uptime, which is fail-closed: a freshly-
    /// started adapter with no subscriptions yet MUST MRC).
    ///
    /// `saturating_sub` makes a clock-skew `now_ms < last_ms` read as
    /// "no time elapsed" — the safe direction.
    pub fn any_subscription_stale(&self, now_ms: u64, timeout_ms: u64) -> bool {
        let t = self.last_trajectory_ms.load(Ordering::Relaxed);
        let o = self.last_objects_ms.load(Ordering::Relaxed);
        let d = self.last_odom_ms.load(Ordering::Relaxed);
        // For each subscription: if it has never been touched (== 0),
        // treat as stale immediately. Otherwise compare the lag.
        t == 0 || o == 0 || d == 0
            || now_ms.saturating_sub(t) > timeout_ms
            || now_ms.saturating_sub(o) > timeout_ms
            || now_ms.saturating_sub(d) > timeout_ms
    }

    /// Stamp the trajectory subscription as "fresh" (called by the
    /// trajectory subscriber on each tick).
    pub fn touch_trajectory(&self, now_ms: u64) {
        self.last_trajectory_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Stamp the objects subscription as "fresh".
    pub fn touch_objects(&self, now_ms: u64) {
        self.last_objects_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Stamp the odometry subscription as "fresh".
    pub fn touch_odom(&self, now_ms: u64) {
        self.last_odom_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Replaces the perception snapshot with a fresh one. Called by the
    /// adapter's PredictedObjects subscriber on each tick.
    pub fn update_objects(&self, objects: Vec<PerceivedObject>) {
        if let Ok(mut guard) = self.objects_cache.write() {
            *guard = objects;
        } else {
            // RwLock poisoning is fail-closed-by-extension: a poisoned
            // cache reads as an empty Vec next cycle (no objects → RSS is
            // trivially safe, but containment + posture cache failures
            // catch the bigger picture). Log loudly.
            tracing::error!(
                "objects_cache RwLock POISONED — perception snapshot dropped"
            );
        }
    }

    /// Read-and-clone of the latest perception snapshot. The slow loop
    /// uses this exactly once per validation; never holds the lock
    /// across the computation.
    pub fn snapshot_objects(&self) -> Vec<PerceivedObject> {
        self.objects_cache
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Replace the SECONDARY (redundant channel-B) perception snapshot and stamp its arrival at
    /// `now_ms` (the slow loop's staleness check reads that stamp). Called by the optional
    /// redundant PredictedObjects subscriber; mirrors [`update_objects`](Self::update_objects)
    /// including the fail-closed RwLock-poisoning handling. Freshness is stamped here (not via a
    /// separate `touch`) so a redundant channel is liveness-tracked with one call.
    pub fn update_objects_secondary(&self, objects: Vec<PerceivedObject>, now_ms: u64) {
        self.last_objects_b_ms.store(now_ms, Ordering::Relaxed);
        if let Ok(mut guard) = self.objects_cache_b.write() {
            *guard = objects;
        } else {
            tracing::error!("objects_cache_b RwLock POISONED — redundant perception snapshot dropped");
        }
    }

    /// Read-and-clone of the latest SECONDARY (channel-B) perception snapshot. The slow loop
    /// cross-checks this against [`snapshot_objects`](Self::snapshot_objects) when the divergence
    /// monitor is enabled.
    pub fn snapshot_objects_secondary(&self) -> Vec<PerceivedObject> {
        self.objects_cache_b
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Installs (or replaces) the accepted trajectory for `asset_id`.
    /// Called by the slow loop on verdict::Accept. Returns the previous
    /// trajectory if one existed (useful for tests / audit).
    pub fn install(&self, traj: AcceptedTrajectory) -> Option<AcceptedTrajectory> {
        let key = traj.asset_id.clone();
        self.by_asset.insert(key, traj)
    }

    /// Slow-loop verdict-driven update. Wraps the candidate trajectory in
    /// an `AcceptedTrajectory` with the slow loop's verdict, then
    /// installs it. On `MRCFallback`, this REMOVES any existing entry
    /// so the fast loop sees the absence (which collapses to MRC via
    /// `current_verdict`) rather than a stale prior Accept. On
    /// `Pending` (initial / transitional), removes too — Pending is
    /// reserved for absence.
    pub fn update_trajectory(
        &self,
        asset_id: impl Into<String>,
        trajectory_id: u64,
        points: Vec<TrajectoryPoint>,
        verdict: TrajectoryVerdict,
        now_ms: u64,
    ) {
        let asset_id = asset_id.into();
        match verdict {
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp => {
                let record = AcceptedTrajectory::with_verdict(
                    asset_id,
                    trajectory_id,
                    points,
                    verdict,
                    now_ms,
                );
                self.install(record);
            }
            TrajectoryVerdict::MRCFallback | TrajectoryVerdict::Pending => {
                self.by_asset.remove(&asset_id);
            }
        }
    }

    /// Reads the current per-asset verdict, collapsed through
    /// `fail_closed`. The fast loop's only entry point.
    ///
    /// Returns `MRCFallback` if:
    ///   - the asset has no entry yet (Pending → MRC), or
    ///   - the entry's verdict is anything other than fresh Accept.
    #[must_use]
    pub fn current_verdict(&self, asset_id: &str, now_ms: u64) -> TrajectoryVerdict {
        match self.by_asset.get(asset_id) {
            Some(entry) => entry.fail_closed(now_ms),
            None => TrajectoryVerdict::MRCFallback,
        }
    }

    /// Returns a clone of the current accepted-trajectory record for the
    /// asset (so the fast loop can read `.points` without holding the
    /// DashMap shard lock across the conformance check). Returns `None`
    /// if no entry exists.
    #[must_use]
    pub fn snapshot(&self, asset_id: &str) -> Option<AcceptedTrajectory> {
        self.by_asset.get(asset_id).map(|e| e.clone())
    }

    /// Count of registered assets; used by tests + Phase 4 metrics.
    pub fn len(&self) -> usize {
        self.by_asset.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_asset.is_empty()
    }
}

// ---------------------------------------------------------------------------
// State-machine tests — no ros2 feature needed.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64, v: f64, t: f64) -> TrajectoryPoint {
        TrajectoryPoint {
            pose: Pose { x_m: x, y_m: y, heading_rad: 0.0 },
            velocity_mps: v,
            time_from_start_s: t,
        }
    }

    /// The redundant (channel-B) perception snapshot round-trips through its own cache and stamps
    /// freshness, independently of the primary channel.
    #[test]
    fn test_secondary_perception_channel_round_trips_and_stamps() {
        let state = AdaptorState::new();
        // Fresh: empty and never stamped.
        assert!(state.snapshot_objects_secondary().is_empty());
        assert_eq!(state.last_objects_b_ms.load(Ordering::Relaxed), 0);

        let objs = vec![PerceivedObject {
            id: 5,
            pos: crate::corridor::Point { x_m: 12.0, y_m: -1.0 },
            velocity_mps: 3.0,
            heading_rad: 0.0,
            vel: crate::corridor::Point { x_m: 3.0, y_m: 0.0 },
        }];
        state.update_objects_secondary(objs.clone(), 7_000);
        let got = state.snapshot_objects_secondary();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, 5);
        assert_eq!(state.last_objects_b_ms.load(Ordering::Relaxed), 7_000, "B arrival stamped");
        // The primary channel is untouched by a secondary write.
        assert!(state.snapshot_objects().is_empty(), "channel A independent of channel B");
    }

    /// Phase-1 GAP: a fresh adapter has no trajectory for any asset and
    /// must MRC on any fast-loop read. The slot is "Pending" by absence.
    #[test]
    fn test_new_trajectory_starts_pending() {
        let state = AdaptorState::new();
        assert!(state.is_empty());
        let verdict = state.current_verdict("av_01", 1_000);
        assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
            "absent asset must collapse to MRCFallback (Pending → fail-closed)");
        assert!(state.snapshot("av_01").is_none());
    }

    /// On Accept, the slow loop installs the record; the fast loop
    /// retrieves it identically (verdict + points round-trip).
    #[test]
    fn test_accept_installs_trajectory() {
        let state = AdaptorState::new();
        let traj = AcceptedTrajectory::new_accepted(
            "av_01", 42, vec![pt(0.0, 0.0, 5.0, 0.0), pt(0.5, 0.0, 5.0, 0.1)], 1_000,
        );
        let prev = state.install(traj);
        assert!(prev.is_none(), "first install returns None");

        let snap = state.snapshot("av_01").expect("must round-trip");
        assert_eq!(snap.asset_id, "av_01");
        assert_eq!(snap.trajectory_id, 42);
        assert_eq!(snap.points.len(), 2);
        assert_eq!(snap.verdict, TrajectoryVerdict::Accept);
        assert_eq!(state.current_verdict("av_01", 1_050), TrajectoryVerdict::Accept,
            "fresh install must read back as Accept within max_age_ms");
    }

    /// Crosses the staleness boundary: `max_age_ms = 100` and `now -
    /// promoted_at_ms = 200` must collapse to MRC even though the
    /// stored verdict is Accept. This is the per-trajectory FTTI loop.
    #[test]
    fn test_stale_trajectory_fails_closed() {
        let state = AdaptorState::new();
        let mut traj = AcceptedTrajectory::new_accepted(
            "av_01", 1, vec![pt(0.0, 0.0, 1.0, 0.0)], 1_000,
        );
        traj.max_age_ms = 100;
        state.install(traj);

        // At t = 1_050 still fresh (50 ms < 100 ms).
        assert_eq!(state.current_verdict("av_01", 1_050), TrajectoryVerdict::Accept);

        // At t = 1_200 the age is 200 ms ≥ 100 ms cap → MRC.
        assert_eq!(state.current_verdict("av_01", 1_200), TrajectoryVerdict::MRCFallback,
            "after max_age_ms elapses the slot must fail closed");
    }

    /// Documented contract: an asset with no trajectory must be treated as
    /// MRCFallback by every caller — there is no other safe disposition.
    /// This test pins the contract; if any caller of `current_verdict`
    /// ever changes to return Pending or Accept for absent assets, this
    /// regression catches it.
    #[test]
    fn test_mrc_fallback_on_absent() {
        let state = AdaptorState::new();
        // No install for "ghost_av".
        let verdict = state.current_verdict("ghost_av", 99_999);
        assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
            "CONTRACT: absent asset must produce MRCFallback, not Accept or Pending");
    }

    /// State-machine direction is one-way: Pending → Accept is allowed by
    /// install; Accept → Pending is NOT (Pending is for absence only).
    /// `install()` overwriting with a new Accept is fine — that's the
    /// slow loop's normal cadence. Pinning the "never demote to Pending"
    /// rule.
    #[test]
    fn test_pending_promotion() {
        let state = AdaptorState::new();
        let traj = AcceptedTrajectory::new_accepted(
            "av_01", 1, vec![pt(0.0, 0.0, 1.0, 0.0)], 1_000,
        );
        state.install(traj);
        assert_eq!(state.current_verdict("av_01", 1_050), TrajectoryVerdict::Accept);

        // Slow loop accepts a fresh candidate; install replaces the old.
        let traj2 = AcceptedTrajectory::new_accepted(
            "av_01", 2, vec![pt(0.5, 0.0, 1.5, 0.0)], 1_100,
        );
        let prev = state.install(traj2);
        assert_eq!(prev.expect("prev").trajectory_id, 1,
            "install returns the displaced trajectory");
        assert_eq!(state.current_verdict("av_01", 1_150), TrajectoryVerdict::Accept);
        assert_eq!(state.snapshot("av_01").unwrap().trajectory_id, 2);

        // There is no API to demote Accept → Pending; `TrajectoryVerdict::Pending`
        // exists only for the absence-by-default case. Verify the type's variants
        // cover the documented state machine. (Compile-time pin via match
        // exhaustiveness; this assertion adds runtime confirmation.)
        let pending = TrajectoryVerdict::Pending;
        assert_ne!(pending, TrajectoryVerdict::Accept);
        assert_ne!(pending, TrajectoryVerdict::MRCFallback);
    }

    /// fail_closed's clock-skew handling: `now_ms < promoted_at_ms` must
    /// NOT panic and must NOT report stale (the safe direction in a clock
    /// skew). saturating_sub does the right thing.
    #[test]
    fn test_fail_closed_handles_clock_skew_safely() {
        let mut traj = AcceptedTrajectory::new_accepted(
            "av_01", 1, vec![pt(0.0, 0.0, 1.0, 0.0)], 1_000_000,
        );
        traj.max_age_ms = 200;
        // now is BEFORE promoted_at_ms — clock skew or restart.
        let verdict = traj.fail_closed(500_000);
        assert_eq!(verdict, TrajectoryVerdict::Accept,
            "clock skew must not falsely trigger staleness (saturating_sub → 0)");
    }

    // -----------------------------------------------------------------------
    // M1b amendment: misconfigured-source fail-CLOSED invariant
    // -----------------------------------------------------------------------
    //
    // The binary's decision tree (see `kirra_ros2_adapter_node::classify_posture_source`)
    // distinguishes three states. This test pins the most-easily-missed
    // one — URL set + token missing/unusable → construct
    // source-configured state WITHOUT spawning an SSE transport. The
    // `PostureTracker` seeds Degraded pre-first-event and never receives
    // one, so `current_posture()` MUST hold at Degraded forever rather
    // than dropping to the M1 no-source Nominal default.
    //
    // The previous (M1b initial) wiring treated missing token as a
    // reason to drop back to `AdaptorState::new()` — which returns
    // Nominal forever. That was a fail-OPEN path in the one case where
    // governance intent was explicit (the operator set the URL). This
    // test ensures it can never regress.
    //
    // SAFETY: SG8 SG9 | REQ: posture-source-fail-closed-misconfig

    #[test]
    fn url_set_but_token_missing_holds_degraded_not_nominal() {
        // The binary's behaviour on misconfiguration: build the
        // source-configured AdaptorState but do NOT spawn the SSE task.
        let state = AdaptorState::with_posture_source(VehicleConfig::default_urban());
        // No observe() will ever fire — the source is "intended but
        // unusable". The tracker must seed and HOLD at Degraded.
        assert_eq!(
            state.current_posture(),
            FleetPosture::Degraded,
            "misconfigured posture source must hold Degraded — \
             must NEVER fail open to Nominal"
        );
        // Read again after a no-op delay — still Degraded. The tracker's
        // pre-first-event seed is time-independent (see
        // tracker_source_pre_first_event_is_degraded in posture_tracker.rs).
        assert_eq!(state.current_posture(), FleetPosture::Degraded);
    }

    #[test]
    fn url_unset_no_source_default_is_still_nominal() {
        // Regression: the M1 no-source default is **untouched** by the
        // M1b amendment. Verifier-less deployments (URL env var unset)
        // continue to use `AdaptorState::new()` and see `Nominal`.
        let state = AdaptorState::new();
        assert_eq!(
            state.current_posture(),
            FleetPosture::Nominal,
            "URL-unset no-source default must remain Nominal (M1 path unchanged)"
        );
    }

    // ----- S-FI1 live frame-integrity source -----

    fn reported(lateral_error_95_m: f64, age_ms: u64) -> FrameIntegrity {
        FrameIntegrity::Reported {
            localization: kirra_core::frame_integrity::LocalizationChannel {
                lateral_error_95_m,
                age_ms,
            },
        }
    }

    #[test]
    fn frame_trust_defaults_to_trusted_when_no_source_reports() {
        // AOU-LOCALIZATION-001 seam: a node with no localization-quality source wired keeps the
        // prior always-Trusted behaviour (the integrator asserts the frame correct externally).
        let state = AdaptorState::new();
        assert_eq!(state.snapshot_frame_trust(1000), FrameTrust::Trusted);
    }

    #[test]
    fn a_live_source_drives_the_frame_trust_gate() {
        let state = AdaptorState::new();
        // Good, fresh report (ε ≤ 0.10 m) → Trusted (primary 0.40 m containment margin).
        state.update_frame_integrity(reported(0.05, 0), 1000);
        assert_eq!(state.snapshot_frame_trust(1000), FrameTrust::Trusted, "good ε → Trusted");
        // Borderline (0.10 < ε ≤ 0.30) → Degraded (fallback 0.75 m margin).
        state.update_frame_integrity(reported(0.20, 0), 2000);
        assert_eq!(state.snapshot_frame_trust(2000), FrameTrust::Degraded, "borderline ε → Degraded");
        // ε beyond the fallback bound → Untrusted (containment refuses).
        state.update_frame_integrity(reported(0.50, 0), 3000);
        assert_eq!(state.snapshot_frame_trust(3000), FrameTrust::Untrusted, "ε > fallback → Untrusted");
        // Non-finite ε → Untrusted (an unverifiable pose is no pose).
        state.update_frame_integrity(reported(f64::NAN, 0), 4000);
        assert_eq!(state.snapshot_frame_trust(4000), FrameTrust::Untrusted, "non-finite ε → Untrusted");
    }

    #[test]
    fn a_once_live_source_going_silent_fails_closed() {
        // Once a source has reported, its SILENCE is a fault: past max_age_ms (500) the gate is
        // Untrusted, not frozen at the last-good value — an absent signal is not "the pose is fine".
        let state = AdaptorState::new();
        state.update_frame_integrity(reported(0.05, 0), 1000);
        assert_eq!(state.snapshot_frame_trust(1400), FrameTrust::Trusted, "still fresh at +400 ms");
        assert_eq!(state.snapshot_frame_trust(1600), FrameTrust::Untrusted, "silent past 500 ms → fail closed");
    }
}
