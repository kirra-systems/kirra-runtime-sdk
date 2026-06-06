// src/capture.rs
//
// Learning-loop capture channel — Phase 1 (docs/CAPTURE_PIPELINE_SPEC.md, #190).
//
// Records the "correction" half of the corrective-supervision triple — what Kirra
// DECIDED and the safe value it imposed — as a NON-BLOCKING side channel, so a
// Linux-side collector can later join it with bus telemetry. This is a SIBLING of
// `src/audit_writer.rs`, mirroring it one-for-one:
//   - a bounded mpsc channel + a single spawn_blocking drain task,
//   - the producer (the actuator gateway) only `try_send`s a small fixed-shape
//     record — wait-free, drop-on-full, NEVER blocking the verdict path,
//   - default OFF behind `KIRRA_CAPTURE_ENABLED` (mirrors the perception-derate
//     default-off env gate).
//
// HARD INVARIANTS (this module + its call site uphold):
//   * Verdict path byte-identical — capture is additive; it reads the
//     already-computed `EnforceAction` and emits. It never lives in, or alters,
//     `src/gateway/kinematics_contract.rs`.
//   * Verdicts/responses identical capture-on vs -off — the emit changes only the
//     side channel; it never gates/delays/alters the verdict, EnforcementOutcome,
//     or the HTTP response.
//   * Wait-free — `try_send`; Full/Closed → drop + LOUD log; safety never waits.
//
// Sink (Phase-1 DECISION): a plain JSONL append file (no tamper-evidence needed —
// this is training data, not the audit chain, so it deliberately does NOT reuse
// the audit SQLite hash-chain). A DDS telemetry topic is a later phase.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::gateway::kinematics_contract::{EnforceAction, ProposedVehicleCommand};
use crate::verifier::FleetPosture;

/// Bounded capture queue depth — mirrors `AUDIT_QUEUE_BOUND`.
pub const CAPTURE_QUEUE_BOUND: usize = 2048;

/// Env gate (default OFF). Mirrors `KIRRA_PERCEPTION_DERATE_ENABLED`.
pub const CAPTURE_ENABLED_ENV: &str = "KIRRA_CAPTURE_ENABLED";

/// Optional override for the JSONL sink path. Default: `kirra_capture.jsonl`
/// in the process CWD.
pub const CAPTURE_SINK_PATH_ENV: &str = "KIRRA_CAPTURE_SINK_PATH";

/// True iff capture is enabled. Default OFF — unset / falsey → no records.
/// Truthy = `1` / `true` / `yes` (case-insensitive), matching
/// `perception_derate_enabled`.
#[must_use]
pub fn capture_enabled() -> bool {
    std::env::var(CAPTURE_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Monotonic nanoseconds since first call (process-stable ordering source for
/// `t_mono_ns`, independent of wall-clock adjustments).
fn mono_ns() -> u128 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos()
}

/// The decision Kirra reached, as a stable token (the "correction" kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaptureOutcome {
    Allow,
    ClampLinear,
    ClampSteering,
    Deny,
}

/// Which enforcement point emitted the record. Phase 1 had one emit (the
/// command gateway, fast loop); Phase 1.5 (docs/CAPTURE_PIPELINE_SPEC.md §3)
/// adds the slow-loop trajectory verdict in the ROS 2 adapter. The Linux
/// collector keys on this to bucket fast- vs slow-loop corrections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaptureSource {
    /// The actuator command gateway (`policy_layer`), per-command fast loop.
    CommandGateway,
    /// The ROS 2 adapter's slow-loop trajectory validator.
    SlowLoopTrajectory,
}

/// SDK-side mirror of the adapter's slow-loop `TrajectoryVerdict`. The real
/// type lives DOWNSTREAM in `kirra-ros2-adapter` and cannot be referenced
/// here without a dependency cycle (the adapter depends on this crate, not
/// the reverse). The adapter maps its `TrajectoryVerdict` onto this at the
/// emit site; keeping the enum here lets the verdict→outcome mapping below
/// be unit-tested in the crate that owns `CaptureOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryDecision {
    /// Promoted as-is.
    Accept,
    /// Promoted as a speed-derated variant (per-pose Clamp).
    Clamp,
    /// Refused / collapsed to MRC (also covers the adapter's `Pending`).
    MrcFallback,
}

/// A snapshot of the doer's proposal (correlation context; the Linux collector
/// joins this with the bus-observed perception/ego/model-version).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProposedCommandSnapshot {
    pub linear_velocity_mps: f64,
    pub current_velocity_mps: f64,
    pub steering_angle_deg: f64,
    pub current_steering_angle_deg: f64,
    pub delta_time_s: f64,
}

impl From<&ProposedVehicleCommand> for ProposedCommandSnapshot {
    fn from(c: &ProposedVehicleCommand) -> Self {
        Self {
            linear_velocity_mps: c.linear_velocity_mps,
            current_velocity_mps: c.current_velocity_mps,
            steering_angle_deg: c.steering_angle_deg,
            current_steering_angle_deg: c.current_steering_angle_deg,
            delta_time_s: c.delta_time_s,
        }
    }
}

/// A single world-frame pose, the BOUNDED endpoints of a trajectory summary.
/// We record only the first + last pose, never the full point sequence — the
/// summary must stay O(1) so the slow-loop emit never regresses WCET.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PoseSnapshot {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// BOUNDED slow-loop trajectory summary + join keys. This is the trajectory
/// analogue of `ProposedCommandSnapshot`: it carries just enough to JOIN the
/// record Linux-side (asset/trajectory ids + the objects-snapshot freshness
/// stamp) plus a fixed-size shape summary (counts + endpoint poses + the
/// planner's target speed). It deliberately does NOT clone the full point or
/// object vectors — only their lengths and endpoints — so building it is O(1)
/// and stays off the slow loop's measured WCET.
#[derive(Debug, Clone, Serialize)]
pub struct TrajectoryCaptureExt {
    /// Join key — the per-asset id ("ego" in single-asset deployments).
    pub asset_id: String,
    /// Join key — the planner/adapter-assigned monotonic trajectory id.
    pub trajectory_id: u64,
    /// Join key — wall-clock ms of the objects snapshot the verdict used
    /// (the same freshness stamp the perception tick is keyed on); lets the
    /// collector line the record up with the bus-observed perception frame.
    pub objects_ms: u64,
    /// Number of points in the candidate trajectory (shape, not the points).
    pub point_count: usize,
    /// Number of perceived objects the verdict saw (shape, not the objects).
    pub object_count: usize,
    /// First pose of the candidate (None for an empty trajectory).
    pub first_pose: Option<PoseSnapshot>,
    /// Last pose of the candidate (None for an empty trajectory).
    pub last_pose: Option<PoseSnapshot>,
    /// The planner's commanded speed at the last point (m/s) — the "target"
    /// the slow loop validated against. None for an empty trajectory.
    pub target_speed_mps: Option<f64>,
}

/// The small, fixed-shape capture record. Carries the correction Kirra imposed +
/// the proposal context + join keys. Deliberately does NOT carry the doer's model
/// version — Kirra doesn't know it; it is joined Linux-side.
#[derive(Debug, Clone, Serialize)]
pub struct CaptureRecord {
    /// Monotonic per-decision sequence (the join/order key).
    pub decision_seq: u64,
    /// Monotonic ns since process start (ordering, skew-free).
    pub t_mono_ns: u128,
    /// Wall-clock ms (bus join).
    pub t_wall_ms: u64,
    /// Which enforcement point emitted this record (fast loop vs slow loop).
    pub source: CaptureSource,
    /// The doer's proposal (correlation + context). Present for the command
    /// gateway (`CommandGateway`); absent for the slow-loop trajectory record,
    /// which carries its context in `traj` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<ProposedCommandSnapshot>,
    /// Bounded slow-loop trajectory summary + join keys. Present only for
    /// `SlowLoopTrajectory` records; `None` (and omitted from JSON) for the
    /// command-gateway record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traj: Option<TrajectoryCaptureExt>,
    /// What Kirra decided.
    pub outcome: CaptureOutcome,
    /// Which check fired, if a deny (the `DenyCode` token); `None` otherwise.
    pub deny_code: Option<&'static str>,
    /// The safe value Kirra substituted on a clamp (m/s for linear, deg for
    /// steering); `None` for Allow/Deny.
    pub safe_value: Option<f64>,
    /// Controlled-stop substitution (Degraded → decel-to-stop-and-HOLD envelope).
    pub mrc: bool,
    /// Posture context.
    pub posture: &'static str,
    /// Whether the perception derate was enabled (so passes are attributable).
    pub derate_enabled: bool,
}

impl CaptureRecord {
    /// Build a record from the already-computed verdict + context. Pure; performs
    /// no I/O. Called at the gateway emit site (off the verdict path).
    #[must_use]
    pub fn from_verdict(
        decision_seq: u64,
        t_wall_ms: u64,
        verdict: &EnforceAction,
        posture: FleetPosture,
        proposed: &ProposedVehicleCommand,
        derate_enabled: bool,
    ) -> Self {
        let (outcome, deny_code, safe_value) = match verdict {
            EnforceAction::Allow => (CaptureOutcome::Allow, None, None),
            EnforceAction::ClampLinear(v) => (CaptureOutcome::ClampLinear, None, Some(*v)),
            EnforceAction::ClampSteering(d) => (CaptureOutcome::ClampSteering, None, Some(*d)),
            EnforceAction::DenyBreach(code) => (CaptureOutcome::Deny, Some(code.reason()), None),
        };
        Self {
            decision_seq,
            t_mono_ns: mono_ns(),
            t_wall_ms,
            source: CaptureSource::CommandGateway,
            proposed: Some(proposed.into()),
            traj: None,
            outcome,
            deny_code,
            safe_value,
            // Degraded posture admits commands only through the decel-to-stop-and-HOLD
            // (MRC) envelope; LockedOut is short-circuited before the gateway verdict.
            mrc: matches!(posture, FleetPosture::Degraded),
            posture: posture_token(posture),
            derate_enabled,
        }
    }

    /// Build a record from the adapter's already-computed slow-loop
    /// trajectory verdict + a BOUNDED trajectory summary. Pure; performs no
    /// I/O. Called at the adapter's slow-loop emit site, OFF the verdict path
    /// (after `validate_trajectory_slow_capped` has already returned).
    ///
    /// Verdict → outcome mapping (the slow-loop analogue of `from_verdict`):
    ///   - `Accept`      → `Allow`        (promoted as-is)
    ///   - `Clamp`       → `ClampLinear`  (promoted speed-derated)
    ///   - `MrcFallback` → `Deny` (`mrc = true`, `deny_code = TRAJECTORY_MRC_FALLBACK`)
    ///
    /// `mrc` is also set whenever the posture is `Degraded` (decel-to-stop
    /// envelope), matching the gateway record's semantics.
    #[must_use]
    pub fn from_trajectory_verdict(
        decision_seq: u64,
        t_wall_ms: u64,
        decision: TrajectoryDecision,
        posture: FleetPosture,
        traj: TrajectoryCaptureExt,
        derate_enabled: bool,
    ) -> Self {
        let (outcome, deny_code) = match decision {
            TrajectoryDecision::Accept => (CaptureOutcome::Allow, None),
            TrajectoryDecision::Clamp => (CaptureOutcome::ClampLinear, None),
            TrajectoryDecision::MrcFallback => {
                (CaptureOutcome::Deny, Some("TRAJECTORY_MRC_FALLBACK"))
            }
        };
        Self {
            decision_seq,
            t_mono_ns: mono_ns(),
            t_wall_ms,
            source: CaptureSource::SlowLoopTrajectory,
            proposed: None,
            traj: Some(traj),
            outcome,
            deny_code,
            // The slow loop has no single substituted scalar (the correction
            // is a whole-trajectory derate/refusal); the target speed lives in
            // the bounded summary instead.
            safe_value: None,
            mrc: matches!(decision, TrajectoryDecision::MrcFallback)
                || matches!(posture, FleetPosture::Degraded),
            posture: posture_token(posture),
            derate_enabled,
        }
    }
}

#[inline]
fn posture_token(p: FleetPosture) -> &'static str {
    match p {
        FleetPosture::Nominal => "NOMINAL",
        FleetPosture::Degraded => "DEGRADED",
        FleetPosture::LockedOut => "LOCKED_OUT",
    }
}

/// Spawns the single capture-writer task on the blocking pool and returns the
/// bounded mpsc Sender producers `try_send` into. Mirrors
/// `audit_writer::spawn_audit_writer`: `blocking_recv` drains serially; the task
/// exits when the last Sender drops. Both emit points (the verifier's command
/// gateway and the ROS 2 adapter's slow loop) call this — it takes no state, so
/// the adapter, which has no `AppState`, can spawn its own writer too.
pub fn spawn_capture_writer() -> mpsc::Sender<CaptureRecord> {
    let (tx, mut rx) = mpsc::channel::<CaptureRecord>(CAPTURE_QUEUE_BOUND);
    let sink_path = std::env::var(CAPTURE_SINK_PATH_ENV)
        .unwrap_or_else(|_| "kirra_capture.jsonl".to_string());
    tokio::task::spawn_blocking(move || {
        let mut sink = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink_path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(
                    error = %e, path = %sink_path,
                    "capture writer: could not open JSONL sink; capture records will be dropped"
                );
                // Drain and discard so producers' try_send never wedges on Full.
                while rx.blocking_recv().is_some() {}
                return;
            }
        };
        tracing::info!(
            queue_bound = CAPTURE_QUEUE_BOUND, path = %sink_path,
            "capture writer task started"
        );
        while let Some(rec) = rx.blocking_recv() {
            write_one_capture(&mut sink, &rec);
        }
        tracing::info!("capture writer task exiting (channel closed)");
    });
    tx
}

/// Single-record write — JSONL line append. The only place serialization + I/O
/// for capture run (off the verdict path).
fn write_one_capture(sink: &mut std::fs::File, rec: &CaptureRecord) {
    match serde_json::to_string(rec) {
        Ok(line) => {
            if let Err(e) = writeln!(sink, "{line}") {
                tracing::error!(error = %e, "capture writer: JSONL append failed; record dropped");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "capture writer: record serialize failed; dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::kinematics_contract::DenyCode;

    fn cmd() -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.0,
            delta_time_s: 0.1,
            steering_angle_deg: 2.0,
            current_steering_angle_deg: 1.0,
        }
    }

    #[test]
    fn from_verdict_maps_each_arm() {
        let c = cmd();
        let allow = CaptureRecord::from_verdict(0, 1000, &EnforceAction::Allow, FleetPosture::Nominal, &c, false);
        assert_eq!(allow.outcome, CaptureOutcome::Allow);
        assert_eq!(allow.deny_code, None);
        assert_eq!(allow.safe_value, None);
        assert!(!allow.mrc);
        assert_eq!(allow.posture, "NOMINAL");

        let cl = CaptureRecord::from_verdict(1, 1000, &EnforceAction::ClampLinear(5.0), FleetPosture::Nominal, &c, true);
        assert_eq!(cl.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(cl.safe_value, Some(5.0));
        assert!(cl.derate_enabled);

        let cs = CaptureRecord::from_verdict(2, 1000, &EnforceAction::ClampSteering(3.0), FleetPosture::Degraded, &c, false);
        assert_eq!(cs.outcome, CaptureOutcome::ClampSteering);
        assert_eq!(cs.safe_value, Some(3.0));
        assert!(cs.mrc, "Degraded → MRC envelope");
        assert_eq!(cs.posture, "DEGRADED");

        let dn = CaptureRecord::from_verdict(3, 1000, &EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity), FleetPosture::Nominal, &c, false);
        assert_eq!(dn.outcome, CaptureOutcome::Deny);
        assert_eq!(dn.deny_code, Some("NAN_INF_LINEAR_VELOCITY"));
        assert_eq!(dn.safe_value, None);
    }

    fn traj_ext() -> TrajectoryCaptureExt {
        TrajectoryCaptureExt {
            asset_id: "ego".to_string(),
            trajectory_id: 7,
            objects_ms: 123_456,
            point_count: 12,
            object_count: 3,
            first_pose: Some(PoseSnapshot { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 }),
            last_pose: Some(PoseSnapshot { x_m: 5.0, y_m: 1.0, heading_rad: 0.1 }),
            target_speed_mps: Some(8.0),
        }
    }

    #[test]
    fn from_trajectory_verdict_maps_each_decision() {
        let accept = CaptureRecord::from_trajectory_verdict(
            0, 1000, TrajectoryDecision::Accept, FleetPosture::Nominal, traj_ext(), false);
        assert_eq!(accept.outcome, CaptureOutcome::Allow);
        assert_eq!(accept.deny_code, None);
        assert!(!accept.mrc);
        assert_eq!(accept.source, CaptureSource::SlowLoopTrajectory);
        assert!(accept.proposed.is_none(), "trajectory record carries no command proposal");
        assert_eq!(accept.traj.as_ref().unwrap().trajectory_id, 7);
        assert_eq!(accept.traj.as_ref().unwrap().objects_ms, 123_456);

        let clamp = CaptureRecord::from_trajectory_verdict(
            1, 1000, TrajectoryDecision::Clamp, FleetPosture::Nominal, traj_ext(), true);
        assert_eq!(clamp.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(clamp.deny_code, None);
        assert!(clamp.derate_enabled);

        let mrc = CaptureRecord::from_trajectory_verdict(
            2, 1000, TrajectoryDecision::MrcFallback, FleetPosture::Nominal, traj_ext(), false);
        assert_eq!(mrc.outcome, CaptureOutcome::Deny);
        assert_eq!(mrc.deny_code, Some("TRAJECTORY_MRC_FALLBACK"));
        assert!(mrc.mrc, "MRCFallback → controlled stop");

        // Degraded posture forces mrc even on an Accept decision.
        let degraded = CaptureRecord::from_trajectory_verdict(
            3, 1000, TrajectoryDecision::Accept, FleetPosture::Degraded, traj_ext(), false);
        assert!(degraded.mrc, "Degraded posture → MRC envelope");
        assert_eq!(degraded.posture, "DEGRADED");
    }

    #[test]
    fn gateway_record_omits_traj_and_keeps_proposed_in_json() {
        // The command-gateway record must serialize WITH `proposed` and
        // WITHOUT `traj` (skip_serializing_if). The trajectory record is the
        // mirror image.
        let gw = CaptureRecord::from_verdict(
            0, 1, &EnforceAction::Allow, FleetPosture::Nominal, &cmd(), false);
        let gw_json = serde_json::to_string(&gw).unwrap();
        assert!(gw_json.contains("\"source\":\"COMMAND_GATEWAY\""));
        assert!(gw_json.contains("\"proposed\""));
        assert!(!gw_json.contains("\"traj\""), "gateway record omits traj");

        let tj = CaptureRecord::from_trajectory_verdict(
            0, 1, TrajectoryDecision::Accept, FleetPosture::Nominal, traj_ext(), false);
        let tj_json = serde_json::to_string(&tj).unwrap();
        assert!(tj_json.contains("\"source\":\"SLOW_LOOP_TRAJECTORY\""));
        assert!(tj_json.contains("\"traj\""));
        assert!(!tj_json.contains("\"proposed\""), "trajectory record omits proposed");
    }

    #[test]
    fn capture_enabled_defaults_off_when_unset() {
        // INV-13: no set_var in a multithreaded test runner; assert the unset
        // default contract (CI has it unset).
        if std::env::var(CAPTURE_ENABLED_ENV).is_err() {
            assert!(!capture_enabled(), "unset env must be disabled");
        }
    }

    #[tokio::test]
    async fn try_send_full_drops_without_blocking() {
        // INV-4: mirror audit_writer's full-drop test — at capacity, try_send
        // returns Full; the producer never blocks. Use a 1-slot channel with no
        // drain so it fills immediately.
        let (tx, _rx) = mpsc::channel::<CaptureRecord>(1);
        let rec = CaptureRecord::from_verdict(0, 1, &EnforceAction::Allow, FleetPosture::Nominal, &cmd(), false);
        assert!(tx.try_send(rec.clone()).is_ok());
        match tx.try_send(rec) {
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
            other => panic!("expected Full at capacity, got {other:?}"),
        }
    }
}
