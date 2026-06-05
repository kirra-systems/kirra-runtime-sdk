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
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::gateway::kinematics_contract::{EnforceAction, ProposedVehicleCommand};
use crate::verifier::{AppState, FleetPosture};

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
    /// The doer's proposal (correlation + context).
    pub proposed: ProposedCommandSnapshot,
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
            proposed: proposed.into(),
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
/// bounded mpsc Sender the gateway `try_send`s into. Exactly mirrors
/// `audit_writer::spawn_audit_writer`: `blocking_recv` drains serially; the task
/// exits when the last Sender drops.
pub fn spawn_capture_writer(_app: Arc<AppState>) -> mpsc::Sender<CaptureRecord> {
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
