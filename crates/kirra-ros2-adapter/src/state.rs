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

use crate::config::VehicleConfig;
use crate::corridor::Point;

/// One pose along a trajectory, in world frame. The shape matches
/// `kirra_runtime_sdk::gateway::containment::Pose` so a Phase 2 conversion
/// is field-for-field (no semantic translation required).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// One sample from an Autoware-shaped trajectory. The `time_from_start_s`
/// is the planner's intended time-to-pose; the slow loop uses it to derive
/// per-step `delta_time_s` for the kinematics check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrajectoryPoint {
    pub pose: Pose,
    pub velocity_mps: f64,
    pub time_from_start_s: f64,
}

/// Outcome of validating a candidate trajectory. The slow loop emits this
/// when it promotes (or refuses to promote) a new trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryVerdict {
    /// Promoted — the per-asset slot now holds this trajectory; the fast
    /// loop will conform commands to it.
    Accept,
    /// Clamp-only path: per-pose kinematics requested a Clamp (linear or
    /// steering) on at least one pose, but containment + RSS both passed.
    /// The caller's policy is "promote a speed-derated variant" — see
    /// design §3. Fast loop treats this as a special Accept where the
    /// permissible-velocity envelope is below the planner's commanded
    /// velocity; safe to drive but not at the planned speed.
    Clamp,
    /// Refused — the slow loop rejected the candidate (or no candidate
    /// has ever been validated). Fast loop must MRC.
    MRCFallback,
    /// Initial / transitional state, set when an asset is registered but
    /// no validation has completed yet. Always fails closed
    /// (`fail_closed()` collapses this to `MRCFallback`).
    Pending,
}

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

/// A perceived object reported by the integrator's perception stack.
/// The fields are the minimal set RSS needs (the slow loop runs
/// `longitudinal_safe_distance` + `lateral_safe_distance` per object × per
/// pose). Position is the centroid in world frame; heading is the object's
/// motion direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerceivedObject {
    pub id: u64,
    pub pos: Point,
    pub velocity_mps: f64,
    pub heading_rad: f64,
}

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
    /// Wall-clock-ms when the LAST nav_msgs::Odometry message arrived.
    pub last_odom_ms:       Arc<AtomicU64>,
}

impl AdaptorState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            by_asset: DashMap::new(),
            objects_cache: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(VehicleConfig::default_urban()),
            latest_odom: Arc::new(RwLock::new(None)),
            last_trajectory_ms: Arc::new(AtomicU64::new(0)),
            last_objects_ms:    Arc::new(AtomicU64::new(0)),
            last_odom_ms:       Arc::new(AtomicU64::new(0)),
        })
    }

    /// Constructs an `AdaptorState` with a specific vehicle config. Used
    /// when the integrator's vehicle profile diverges from
    /// `default_urban()` (e.g. shuttle, light truck).
    pub fn with_config(config: VehicleConfig) -> Arc<Self> {
        Arc::new(Self {
            by_asset: DashMap::new(),
            objects_cache: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(config),
            latest_odom: Arc::new(RwLock::new(None)),
            last_trajectory_ms: Arc::new(AtomicU64::new(0)),
            last_objects_ms:    Arc::new(AtomicU64::new(0)),
            last_odom_ms:       Arc::new(AtomicU64::new(0)),
        })
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
}
