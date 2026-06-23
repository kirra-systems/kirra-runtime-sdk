// src/capture.rs
//
// Learning-loop capture channel — Phase 1 (docs/CAPTURE_PIPELINE_SPEC.md, #190);
// Phase 1.5 slow-loop emit (#192); Stage A schema extraction
// (docs/COLLECTOR_DESIGN.md [C1]).
//
// Records the "correction" half of the corrective-supervision triple — what Kirra
// DECIDED and the safe value it imposed — as a NON-BLOCKING side channel, so a
// Linux-side collector can later join it with bus telemetry. This is a SIBLING of
// `src/audit_writer.rs`, mirroring it one-for-one:
//   - a bounded mpsc channel + a single spawn_blocking drain task,
//   - the producer (the actuator gateway / the adapter slow loop) only
//     `try_send`s a small fixed-shape record — wait-free, drop-on-full, NEVER
//     blocking the verdict path,
//   - default OFF behind `KIRRA_CAPTURE_ENABLED` (mirrors the perception-derate
//     default-off env gate).
//
// SCHEMA LOCATION (Stage A): the on-disk record TYPES live in the governor-free
// `kirra-capture-schema` crate and are re-exported below (`pub use
// kirra_capture_schema::*;`) so every `crate::capture::*` path keeps resolving.
// This module keeps the BUILDERS (which touch governor types) and the writer.
// The split is what lets the offline collector reuse the exact schema without
// linking the verdict path (§0).
//
// HARD INVARIANTS (this module + its call sites uphold):
//   * Verdict path byte-identical — capture is additive; it reads the
//     already-computed `EnforceAction` / `TrajectoryDecision` and emits. It never
//     lives in, or alters, `src/gateway/kinematics_contract.rs`.
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

use tokio::sync::mpsc;

use crate::kinematics_contract::{EnforceAction, ProposedVehicleCommand};
use crate::FleetPosture;

// The capture record wire schema lives in the governor-free
// `kirra-capture-schema` crate (docs/COLLECTOR_DESIGN.md [C1]); re-export it so
// every existing `crate::capture::{CaptureRecord, ...}` /
// `kirra_runtime_sdk::capture::*` path keeps resolving, and so the SDK and the
// offline collector share ONE authoritative definition with no drift.
pub use kirra_capture_schema::*;

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

/// SDK-side mirror of the adapter's slow-loop `TrajectoryVerdict`. The real
/// type lives DOWNSTREAM in `kirra-ros2-adapter` and cannot be referenced
/// here without a dependency cycle (the adapter depends on this crate, not
/// the reverse). The adapter maps its `TrajectoryVerdict` onto this at the
/// emit site; keeping the enum here (NOT in the wire-schema crate) lets the
/// verdict→outcome mapping below be unit-tested where the builders live — it is
/// a constructor INPUT, never a serialized field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryDecision {
    /// Promoted as-is.
    Accept,
    /// Promoted as a speed-derated variant (per-pose Clamp).
    Clamp,
    /// Refused / collapsed to MRC (also covers the adapter's `Pending`).
    MrcFallback,
}

/// Build a record from the already-computed gateway verdict + context. Pure;
/// performs no I/O. Called at the gateway emit site (off the verdict path).
///
/// Free function (not an inherent method) because `CaptureRecord` now lives in
/// `kirra-capture-schema` — the orphan rule forbids an inherent impl here. The
/// `ProposedVehicleCommand → ProposedCommandSnapshot` mapping is inlined for the
/// same reason (a `From` impl would have to live in one crate or the other and
/// can do neither without dragging the governor into the schema crate).
#[must_use]
pub fn record_from_verdict(
    decision_seq: u64,
    t_wall_ms: u64,
    verdict: &EnforceAction,
    posture: FleetPosture,
    proposed: &ProposedVehicleCommand,
    derate_enabled: bool,
) -> CaptureRecord {
    let (outcome, deny_code, safe_value) = match verdict {
        EnforceAction::Allow => (CaptureOutcome::Allow, None, None),
        EnforceAction::ClampLinear(v) => (CaptureOutcome::ClampLinear, None, Some(*v)),
        EnforceAction::ClampSteering(d) => (CaptureOutcome::ClampSteering, None, Some(*d)),
        EnforceAction::DenyBreach(code) => {
            (CaptureOutcome::Deny, Some(code.reason().to_string()), None)
        }
    };
    CaptureRecord {
        decision_seq,
        t_mono_ns: mono_ns(),
        t_wall_ms,
        source: CaptureSource::CommandGateway,
        // Inlined ProposedVehicleCommand → ProposedCommandSnapshot mapping
        // (the former `From` impl — see doc comment above).
        proposed: Some(ProposedCommandSnapshot {
            linear_velocity_mps: proposed.linear_velocity_mps,
            current_velocity_mps: proposed.current_velocity_mps,
            steering_angle_deg: proposed.steering_angle_deg,
            current_steering_angle_deg: proposed.current_steering_angle_deg,
            delta_time_s: proposed.delta_time_s,
        }),
        traj: None,
        outcome,
        deny_code,
        safe_value,
        // Degraded posture admits commands only through the decel-to-stop-and-HOLD
        // (MRC) envelope; LockedOut is short-circuited before the gateway verdict.
        mrc: matches!(posture, FleetPosture::Degraded),
        posture: posture_token(posture).to_string(),
        derate_enabled,
    }
}

/// Build a record from the adapter's already-computed slow-loop trajectory
/// verdict + a BOUNDED trajectory summary. Pure; performs no I/O. Called at the
/// adapter's slow-loop emit site, OFF the verdict path (after
/// `validate_trajectory_slow_capped` has already returned).
///
/// Verdict → outcome mapping (the slow-loop analogue of `record_from_verdict`):
///   - `Accept`      → `Allow`        (promoted as-is)
///   - `Clamp`       → `ClampLinear`  (promoted speed-derated)
///   - `MrcFallback` → `Deny` (`mrc = true`, `deny_code = TRAJECTORY_MRC_FALLBACK`)
///
/// `mrc` is also set whenever the posture is `Degraded` (decel-to-stop
/// envelope), matching the gateway record's semantics.
#[must_use]
pub fn record_from_trajectory_verdict(
    decision_seq: u64,
    t_wall_ms: u64,
    decision: TrajectoryDecision,
    posture: FleetPosture,
    traj: TrajectoryCaptureExt,
    derate_enabled: bool,
) -> CaptureRecord {
    let (outcome, deny_code) = match decision {
        TrajectoryDecision::Accept => (CaptureOutcome::Allow, None),
        TrajectoryDecision::Clamp => (CaptureOutcome::ClampLinear, None),
        TrajectoryDecision::MrcFallback => {
            (CaptureOutcome::Deny, Some("TRAJECTORY_MRC_FALLBACK".to_string()))
        }
    };
    CaptureRecord {
        decision_seq,
        t_mono_ns: mono_ns(),
        t_wall_ms,
        source: CaptureSource::SlowLoopTrajectory,
        proposed: None,
        traj: Some(traj),
        outcome,
        deny_code,
        // The slow loop has no single substituted scalar (the correction is a
        // whole-trajectory derate/refusal); the target speed lives in the
        // bounded summary instead.
        safe_value: None,
        mrc: matches!(decision, TrajectoryDecision::MrcFallback)
            || matches!(posture, FleetPosture::Degraded),
        posture: posture_token(posture).to_string(),
        derate_enabled,
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
    use crate::kinematics_contract::DenyCode;

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
    fn record_from_verdict_maps_each_arm() {
        let c = cmd();
        let allow = record_from_verdict(0, 1000, &EnforceAction::Allow, FleetPosture::Nominal, &c, false);
        assert_eq!(allow.outcome, CaptureOutcome::Allow);
        assert_eq!(allow.deny_code, None);
        assert_eq!(allow.safe_value, None);
        assert!(!allow.mrc);
        assert_eq!(allow.posture, "NOMINAL");

        let cl = record_from_verdict(1, 1000, &EnforceAction::ClampLinear(5.0), FleetPosture::Nominal, &c, true);
        assert_eq!(cl.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(cl.safe_value, Some(5.0));
        assert!(cl.derate_enabled);

        let cs = record_from_verdict(2, 1000, &EnforceAction::ClampSteering(3.0), FleetPosture::Degraded, &c, false);
        assert_eq!(cs.outcome, CaptureOutcome::ClampSteering);
        assert_eq!(cs.safe_value, Some(3.0));
        assert!(cs.mrc, "Degraded → MRC envelope");
        assert_eq!(cs.posture, "DEGRADED");

        let dn = record_from_verdict(3, 1000, &EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity), FleetPosture::Nominal, &c, false);
        assert_eq!(dn.outcome, CaptureOutcome::Deny);
        assert_eq!(dn.deny_code.as_deref(), Some("NAN_INF_LINEAR_VELOCITY"));
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
    fn record_from_trajectory_verdict_maps_each_decision() {
        let accept = record_from_trajectory_verdict(
            0, 1000, TrajectoryDecision::Accept, FleetPosture::Nominal, traj_ext(), false);
        assert_eq!(accept.outcome, CaptureOutcome::Allow);
        assert_eq!(accept.deny_code, None);
        assert!(!accept.mrc);
        assert_eq!(accept.source, CaptureSource::SlowLoopTrajectory);
        assert!(accept.proposed.is_none(), "trajectory record carries no command proposal");
        assert_eq!(accept.traj.as_ref().unwrap().trajectory_id, 7);
        assert_eq!(accept.traj.as_ref().unwrap().objects_ms, 123_456);

        let clamp = record_from_trajectory_verdict(
            1, 1000, TrajectoryDecision::Clamp, FleetPosture::Nominal, traj_ext(), true);
        assert_eq!(clamp.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(clamp.deny_code, None);
        assert!(clamp.derate_enabled);

        let mrc = record_from_trajectory_verdict(
            2, 1000, TrajectoryDecision::MrcFallback, FleetPosture::Nominal, traj_ext(), false);
        assert_eq!(mrc.outcome, CaptureOutcome::Deny);
        assert_eq!(mrc.deny_code.as_deref(), Some("TRAJECTORY_MRC_FALLBACK"));
        assert!(mrc.mrc, "MRCFallback → controlled stop");

        // Degraded posture forces mrc even on an Accept decision.
        let degraded = record_from_trajectory_verdict(
            3, 1000, TrajectoryDecision::Accept, FleetPosture::Degraded, traj_ext(), false);
        assert!(degraded.mrc, "Degraded posture → MRC envelope");
        assert_eq!(degraded.posture, "DEGRADED");
    }

    #[test]
    fn gateway_record_omits_traj_and_keeps_proposed_in_json() {
        // The command-gateway record must serialize WITH `proposed` and
        // WITHOUT `traj` (skip_serializing_if). The trajectory record is the
        // mirror image. (The wire shape itself is pinned in the schema crate.)
        let gw = record_from_verdict(
            0, 1, &EnforceAction::Allow, FleetPosture::Nominal, &cmd(), false);
        let gw_json = serde_json::to_string(&gw).unwrap();
        assert!(gw_json.contains("\"source\":\"COMMAND_GATEWAY\""));
        assert!(gw_json.contains("\"proposed\""));
        assert!(!gw_json.contains("\"traj\""), "gateway record omits traj");

        let tj = record_from_trajectory_verdict(
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
        let rec = record_from_verdict(0, 1, &EnforceAction::Allow, FleetPosture::Nominal, &cmd(), false);
        assert!(tx.try_send(rec.clone()).is_ok());
        match tx.try_send(rec) {
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
            other => panic!("expected Full at capacity, got {other:?}"),
        }
    }
}
