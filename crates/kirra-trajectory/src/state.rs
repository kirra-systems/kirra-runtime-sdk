// crates/kirra-trajectory/src/state.rs
//
// The trajectory CHECKER contract types (R1): `AcceptedTrajectory` (the per-asset
// accepted-trajectory record + its staleness/fail-closed policy) and `EgoOdom`.
// Relocated verbatim from `kirra-ros2-adapter::state` so the checker
// (`validation::check_command_conforms`) and downstream consumers depend on the
// contract, not the ROS integration crate.
//
// The ROS RUNTIME store (`AdaptorState`, the DashMap per-asset slots, subscription
// freshness stamps, `monotonic_now_ms`, `IncomingTrajectory`,
// `SUBSCRIPTION_STALENESS_TIMEOUT_MS`) STAYS in `kirra-ros2-adapter::state` and
// re-exports these contract types so its `crate::state::*` paths are unchanged.

// The lean trajectory/perception data types live in `kirra-core` (de-monolith
// Stage 6a); re-exported here so `crate::state::{PerceivedObject, Pose,
// TrajectoryPoint, TrajectoryVerdict}` resolves for the checker exactly as before.
pub use kirra_core::trajectory::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict};

use kirra_core::kinematics_contract::VehicleKinematicsContract;

/// S1 fix (#1024) — the checker's **posture-composed lateral envelope**, carried
/// from the slow loop to the fast-loop conformance gate.
///
/// Motivation: the fast loop is the actual per-cycle gate on the untrusted
/// controller's outgoing steering command, but it previously bounded steering
/// only against the STATIC `config.max_steering_rad` (the Nominal rack limit) and
/// never bounded the command's LATERAL ACCELERATION at all. So a within-rack
/// steer at ODD speed (`a_lat = v²·tan(δ)/L`, far above the rollover envelope)
/// passed conformance and was republished to the actuators verbatim. This
/// envelope lets `check_command_conforms` re-apply the kernel's own P5a/P6 bound
/// (posture-composed → tighter under Degraded) to the OUTGOING command.
///
/// Like the B1 velocity ceiling, it rides HERE on the heap-backed slow-loop
/// record, never on `TrajectoryVerdict` (which stays a pinned one byte). It is a
/// pure function of `(config, posture)` — the perception cap only tightens speed,
/// not the lateral limit — so the promote site builds it directly from the same
/// posture-composed `VehicleKinematicsContract` the slow loop enforced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LateralEnvelope {
    /// Posture-composed dynamic lateral-acceleration ceiling (bicycle model), m/s².
    pub max_lateral_accel_mps2: f64,
    /// Posture-composed hard steering-angle limit, radians. Tighter than the
    /// static `config.max_steering_rad` under Degraded (the MRC contract).
    pub max_steering_rad: f64,
    /// Wheelbase `L`, metres. Physical — posture-independent.
    pub wheelbase_m: f64,
}

impl LateralEnvelope {
    /// Extract the lateral envelope from the posture-composed kinematics
    /// contract the slow loop enforced (`config.to_posture_kinematics_contract`).
    /// The kernel stores the steering hard limit in DEGREES; the fast-loop
    /// conformance gate works in radians, so convert here once.
    #[must_use]
    pub fn from_contract(contract: &VehicleKinematicsContract) -> Self {
        Self {
            max_lateral_accel_mps2: contract.max_lateral_accel_mps2,
            max_steering_rad: contract.max_steering_deg.to_radians(),
            wheelbase_m: contract.wheelbase_m,
        }
    }
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
    /// B1 fix — the effective per-pose velocity ceiling the checker computed,
    /// aligned index-for-index with `points`. `Some` ONLY on a `Clamp`
    /// verdict (the slow loop derated at least one pose); `check_command_conforms`
    /// gates the command against `effective_velocity_ceiling[nearest]` instead
    /// of the ORIGINAL planner velocity, so a command at the unclamped speed on
    /// a `Clamp` verdict fails conformance → MRC. `None` on `Accept` (no derate)
    /// → the fast path is byte-identical to before this field existed.
    ///
    /// The derate rides HERE, on the heap-backed slow-loop record — never on
    /// `TrajectoryVerdict`, which stays a pinned one byte (the #893
    /// side-channel discipline; see `trajectory_verdict_stays_one_byte`).
    pub effective_velocity_ceiling: Option<Vec<f64>>,
    /// S1 fix (#1024) — the checker's posture-composed lateral envelope. `Some`
    /// on a freshly-validated Accept/Clamp record (the slow loop attaches it from
    /// the same contract it enforced); `check_command_conforms` then bounds the
    /// OUTGOING command's steering hard-limit AND lateral acceleration against it.
    /// `None` on a legacy record → the fast loop falls back to the static
    /// `config.max_steering_rad` only (byte-identical to before this field).
    pub effective_lateral_envelope: Option<LateralEnvelope>,
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
            effective_velocity_ceiling: None,
            effective_lateral_envelope: None,
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
            effective_velocity_ceiling: None,
            effective_lateral_envelope: None,
            promoted_at_ms,
            max_age_ms: DEFAULT_MAX_AGE_MS,
        }
    }

    /// Attach the checker's effective per-pose velocity ceiling (B1 fix). The
    /// slow loop calls this on a `Clamp` verdict with the envelope from
    /// [`crate::validation::validate_trajectory_slow_with_envelope`]; the fast
    /// loop's `check_command_conforms` then gates against it. Chainable off
    /// [`with_verdict`](Self::with_verdict). A `None` argument is a no-op — the
    /// `Accept` path — leaving conformance behaviour byte-identical to before
    /// this field existed (the field is present on every record; only its value
    /// changes).
    #[must_use]
    pub fn with_effective_ceiling(mut self, ceiling: Option<Vec<f64>>) -> Self {
        self.effective_velocity_ceiling = ceiling;
        self
    }

    /// Attach the checker's posture-composed lateral envelope (S1 fix, #1024).
    /// The slow loop calls this on every promoted Accept/Clamp record with the
    /// envelope derived from the same posture contract it enforced; the fast
    /// loop's `check_command_conforms` then bounds the outgoing command's
    /// steering hard-limit + lateral acceleration against it. Chainable off
    /// [`with_verdict`](Self::with_verdict) / [`with_effective_ceiling`]. A
    /// `None` argument is a no-op (leaves the static-limit fallback path).
    #[must_use]
    pub fn with_lateral_envelope(mut self, envelope: Option<LateralEnvelope>) -> Self {
        self.effective_lateral_envelope = envelope;
        self
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
            TrajectoryVerdict::Clamp => TrajectoryVerdict::Clamp,
            _ => TrajectoryVerdict::MRCFallback,
        }
    }
}

/// Minimal ego-odometry snapshot. Fixes the `current_steering_angle_deg = 0.0`
/// approximation in `validate_trajectory_slow` AND feeds the fast-loop
/// conformance check the current ego velocity for the staleness / nearest-point
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
        Self {
            linear_x_mps: 0.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_fresh_is_not_stale_and_holds_verdict() {
        let traj = AcceptedTrajectory::new_accepted("ego", 1, Vec::new(), 1_000);
        assert!(!traj.is_stale(1_000 + DEFAULT_MAX_AGE_MS - 1));
        assert_eq!(traj.fail_closed(1_000), TrajectoryVerdict::Accept);
    }

    #[test]
    fn accepted_beyond_max_age_fails_closed() {
        let traj = AcceptedTrajectory::new_accepted("ego", 1, Vec::new(), 1_000);
        assert!(traj.is_stale(1_000 + DEFAULT_MAX_AGE_MS));
        assert_eq!(
            traj.fail_closed(1_000 + DEFAULT_MAX_AGE_MS),
            TrajectoryVerdict::MRCFallback,
            "a stale slot must collapse to MRC regardless of its stored verdict"
        );
    }

    #[test]
    fn clamp_verdict_is_preserved_while_fresh_mrc_is_floored() {
        let clamp =
            AcceptedTrajectory::with_verdict("ego", 2, Vec::new(), TrajectoryVerdict::Clamp, 1_000);
        assert_eq!(clamp.fail_closed(1_000), TrajectoryVerdict::Clamp);

        let mrc = AcceptedTrajectory::with_verdict(
            "ego",
            3,
            Vec::new(),
            TrajectoryVerdict::MRCFallback,
            1_000,
        );
        assert_eq!(mrc.fail_closed(1_000), TrajectoryVerdict::MRCFallback);
    }

    #[test]
    fn backward_clock_skew_reads_not_stale_no_panic() {
        let traj = AcceptedTrajectory::new_accepted("ego", 1, Vec::new(), 5_000);
        // now < promoted_at: saturating_sub → 0 → not stale (fast-loop safe disposition).
        assert!(!traj.is_stale(1_000));
    }

    #[test]
    fn ego_odom_default_is_zeroed() {
        let o = EgoOdom::default();
        assert_eq!(o.linear_x_mps, 0.0);
        assert_eq!(o.yaw_rate_rads, 0.0);
        assert_eq!(o.stamp_ms, 0);
    }
}
