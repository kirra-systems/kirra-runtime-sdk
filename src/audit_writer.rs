// src/audit_writer.rs
//
// S3 WCET Pass B2 / B3 (issue #115) — decouples the verdict-path deny-arm
// audit write from the request handler. The deny arm (`policy_layer.rs`)
// builds an `AuditWriteJob` and `try_send`s it into a bounded mpsc queue;
// this module spawns a single writer task that drains the queue and
// performs the actual `save_posture_event_chained` call off the verdict
// path, on the blocking thread pool.
//
// Key invariants (per S3 Pass B discovery report):
//   - Single writer task only — preserves the chain-hash read-then-write
//     atomicity that `Arc<Mutex<VerifierStore>>` previously enforced.
//   - The writer task itself acquires the std::sync::Mutex on the store;
//     it runs in `tokio::task::spawn_blocking` so blocking the worker
//     thread is sound.
//   - Producers MUST NOT precompute prev_hash or sequence — only the
//     writer can, inside the locked critical section.
//   - Channel-full = best-effort drop + LOUD log. NEVER blocking_send.
//     (Matches the existing fire-and-best-effort behavior on DenyBreach:
//     the rejection response is not gated on the write succeeding.)
//   - `KinematicViolationPayload` field order is ALPHABETICAL to match
//     the byte output of the previous `serde_json::json!({...}).to_string()`
//     under serde_json's default `BTreeMap`-backed `Value::Object`.
//     The audit-chain hash binds the full `event_json` byte sequence
//     (per `audit_chain::compute_record_hash_v2`), so a change to the
//     serialized bytes would silently break verification of newly written
//     rows. The byte-identity property is enforced by the gating tests
//     in `#[cfg(test)] mod byte_identity_tests` at the bottom of this file.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::verifier::{AppState, FleetPosture};

/// Bounded capacity of the audit-writer channel. Sized to absorb bursts of
/// kinematic envelope violations (e.g. an upstream planner emitting a stream
/// of invalid commands during a fault). At steady state the queue drains
/// faster than it fills; "full" indicates real overload.
pub const AUDIT_QUEUE_BOUND: usize = 2048;

// ---------------------------------------------------------------------------
// Payload structs — alphabetical field order for byte-identity with the
// previous `serde_json::json!({...}).to_string()` path.
// ---------------------------------------------------------------------------

/// The deny-arm audit payload for a kinematic envelope violation.
///
/// Field order is ALPHABETICAL to match `serde_json`'s default
/// `BTreeMap`-backed `Value::Object` ordering. Changing field order here is
/// a versioned audit-format migration, not a transparent rewrite, because
/// the entire serialized byte sequence is bound into the chain hash
/// (`audit_chain::compute_record_hash_v2`, arg 3 `event_json`).
///
/// EP-17 format rev: `verdict_id` (the retrievable-verdict handle minted by
/// `crate::verdicts::mint_verdict_id` and returned in the 400 body) is bound
/// INTO the chained payload, so the operator artifact `GET /verdicts/{id}`
/// serves is tamper-evident under the same chain hash + signature as the
/// denial itself. Old records without the field verify unchanged (each chain
/// hash binds that record's own bytes).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KinematicViolationPayload {
    pub posture_at_rejection: &'static str,
    pub proposed_command: ProposedCommandPayload,
    pub verdict_id: String,
    pub violation: &'static str,
}

/// Nested proposed-command snapshot. Alphabetical field order, same rationale
/// as the parent payload.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct ProposedCommandPayload {
    pub current_steering_angle_deg: f64,
    pub current_velocity_mps: f64,
    pub delta_time_s: f64,
    pub linear_velocity_mps: f64,
    pub steering_angle_deg: f64,
}

// ---------------------------------------------------------------------------
// FleetPosture → &'static str — pins the rendering used in the audit
// payload's `posture_at_rejection` field. Previously this was
// `format!("{posture:?}")`; explicit mapping closes the silent-break risk if
// the `FleetPosture` `Debug` impl ever changes (e.g. derive-Debug field
// renames, manual impl tweaks).
// ---------------------------------------------------------------------------

/// Stable rendering of `FleetPosture` for the audit `posture_at_rejection`
/// field. The returned `&'static str` matches the current `Debug` output
/// (`Nominal` / `Degraded` / `LockedOut`) byte-for-byte.
pub const fn fleet_posture_str(p: &FleetPosture) -> &'static str {
    match p {
        FleetPosture::Nominal => "Nominal",
        FleetPosture::Degraded => "Degraded",
        FleetPosture::LockedOut => "LockedOut",
    }
}

// ---------------------------------------------------------------------------
// Job + writer task
// ---------------------------------------------------------------------------

/// A single deny-arm audit write to enqueue for the writer task.
///
/// All fields are owned (or `&'static str`); the producer captures the
/// event-time `created_at_ms` (the value bound into the chain hash) but
/// does NOT precompute prev_hash or sequence — those belong to the writer
/// task inside the locked critical section.
#[derive(Debug, Clone)]
pub struct AuditWriteJob {
    pub event_type: &'static str,
    pub payload: KinematicViolationPayload,
    pub created_at_ms: i64,
    pub node_id: &'static str,
    pub reason: &'static str,
}

/// Spawns the single audit-writer task on the blocking thread pool and
/// returns the bounded mpsc Sender for the deny arm to `try_send` into.
///
/// The task loops on `rx.blocking_recv()` (sound under
/// `tokio::task::spawn_blocking`) and exits cleanly when the last Sender is
/// dropped (channel closes → `None` from `blocking_recv`). On shutdown,
/// any remaining queued jobs are drained before exit.
pub fn spawn_audit_writer(app: Arc<AppState>) -> mpsc::Sender<AuditWriteJob> {
    let (tx, mut rx) = mpsc::channel::<AuditWriteJob>(AUDIT_QUEUE_BOUND);
    tokio::task::spawn_blocking(move || {
        tracing::info!(queue_bound = AUDIT_QUEUE_BOUND, "audit writer task started");
        // blocking_recv drains the queue serially; per Pass B discovery,
        // single-writer-only is what preserves chain-hash read-then-write
        // atomicity across concurrent verdict tasks.
        while let Some(job) = rx.blocking_recv() {
            write_one(&app, job);
        }
        tracing::info!("audit writer task exiting (channel closed)");
    });
    tx
}

/// Single-job write — the only place where `serde_json::to_string` for the
/// payload runs (the `String` allocation lives HERE, off the verdict path)
/// and the only place where the store lock is acquired for the deny-arm
/// audit chain entry.
fn write_one(app: &AppState, job: AuditWriteJob) {
    // Serialize off the verdict path. By Pass A + B1 + this pass, the
    // verdict thread does only `try_send(job)` — no JSON, no lock, no I/O.
    let event_json = match serde_json::to_string(&job.payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                error = %e,
                event_type = job.event_type,
                "audit writer: payload serialize failed; record dropped"
            );
            return;
        }
    };
    app.store.with(|store| {
        if let Err(e) = store.save_posture_event_chained(
            job.node_id,
            job.event_type,
            &event_json,
            Some(job.reason),
            job.created_at_ms as u64,
        ) {
            tracing::error!(
                error = %e,
                event_type = job.event_type,
                "AUDIT-CHAIN WRITE FAILED in writer task — event missing from tamper-evident log"
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Byte-identity gating tests (B3 guard)
// ---------------------------------------------------------------------------
//
// CRITICAL: these tests assert that
//   serde_json::to_string(&KinematicViolationPayload { ... })
// equals
//   serde_json::json!({ <the old payload> }).to_string()
// byte-for-byte across every FleetPosture variant + representative DenyCode
// and command-field values. If any assertion fails, the typed-struct path
// would silently produce a different audit-chain hash than the previous
// `json!` path — i.e., a versioned format migration is required and the
// Pass B2 decouple MUST NOT proceed until it's reconciled.
#[cfg(test)]
mod byte_identity_tests {
    use super::*;
    use crate::gateway::kinematics_contract::DenyCode;
    use serde_json::json;

    fn render_legacy(
        violation: &str,
        cmd: ProposedCommandPayload,
        posture_at_rejection: &str,
        verdict_id: &str,
    ) -> String {
        json!({
            "violation": violation,
            "proposed_command": {
                "linear_velocity_mps": cmd.linear_velocity_mps,
                "current_velocity_mps": cmd.current_velocity_mps,
                "delta_time_s": cmd.delta_time_s,
                "steering_angle_deg": cmd.steering_angle_deg,
                "current_steering_angle_deg": cmd.current_steering_angle_deg,
            },
            "posture_at_rejection": posture_at_rejection,
            // EP-17 format rev: the retrievable-verdict handle. `json!`'s
            // BTreeMap ordering slots it alphabetically, same as the struct.
            "verdict_id": verdict_id,
        })
        .to_string()
    }

    fn assert_byte_identical(cmd: ProposedCommandPayload, code: DenyCode, posture: FleetPosture) {
        let posture_str = fleet_posture_str(&posture);
        let verdict_id = "0123456789abcdef0123456789abcdef";
        let new = serde_json::to_string(&KinematicViolationPayload {
            posture_at_rejection: posture_str,
            proposed_command: cmd,
            verdict_id: verdict_id.to_string(),
            violation: code.reason(),
        })
        .expect("serialize must succeed");
        let legacy = render_legacy(code.reason(), cmd, posture_str, verdict_id);
        assert_eq!(
            new, legacy,
            "byte-identity failure for code={code:?} posture={posture:?} cmd={cmd:?}\n  new={new}\nlegacy={legacy}"
        );
    }

    fn sample_cmd() -> ProposedCommandPayload {
        ProposedCommandPayload {
            current_steering_angle_deg: 1.5,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            linear_velocity_mps: 12.5,
            steering_angle_deg: 3.0,
        }
    }

    #[test]
    fn byte_identity_all_posture_variants() {
        for posture in [
            FleetPosture::Nominal,
            FleetPosture::Degraded,
            FleetPosture::LockedOut,
        ] {
            assert_byte_identical(sample_cmd(), DenyCode::NanInfLinearVelocity, posture);
        }
    }

    #[test]
    fn byte_identity_all_deny_codes() {
        let posture = FleetPosture::Nominal;
        for code in [
            DenyCode::NanInfLinearVelocity,
            DenyCode::NanInfCurrentVelocity,
            DenyCode::NanInfSteeringAngle,
            DenyCode::NanInfCurrentSteering,
            DenyCode::NanInfDeltaTime,
            DenyCode::InvalidTimeDelta,
            DenyCode::AssetLockedOut,
        ] {
            assert_byte_identical(sample_cmd(), code, posture);
        }
    }

    #[test]
    fn byte_identity_numeric_edges() {
        // f64 cases that exercise serde_json's number formatter: zero,
        // negative, large, small. NaN/Inf are not valid JSON; the payload
        // is only constructed for COMMANDS that already passed the P0
        // NaN/Inf guard in validate_vehicle_command, so they cannot appear
        // here in practice.
        let cases: &[ProposedCommandPayload] = &[
            ProposedCommandPayload {
                current_steering_angle_deg: 0.0,
                current_velocity_mps: 0.0,
                delta_time_s: 0.0,
                linear_velocity_mps: 0.0,
                steering_angle_deg: 0.0,
            },
            ProposedCommandPayload {
                current_steering_angle_deg: -42.5,
                current_velocity_mps: -1.0,
                delta_time_s: 1e-9,
                linear_velocity_mps: -35.0,
                steering_angle_deg: -35.0,
            },
            ProposedCommandPayload {
                current_steering_angle_deg: 35.0,
                current_velocity_mps: 1e9,
                delta_time_s: 1.0,
                linear_velocity_mps: 1e-300,
                steering_angle_deg: 1.5,
            },
        ];
        for cmd in cases {
            for posture in [
                FleetPosture::Nominal,
                FleetPosture::Degraded,
                FleetPosture::LockedOut,
            ] {
                assert_byte_identical(*cmd, DenyCode::InvalidTimeDelta, posture);
            }
        }
    }

    #[test]
    fn fleet_posture_str_matches_current_debug() {
        assert_eq!(fleet_posture_str(&FleetPosture::Nominal), "Nominal");
        assert_eq!(fleet_posture_str(&FleetPosture::Degraded), "Degraded");
        assert_eq!(fleet_posture_str(&FleetPosture::LockedOut), "LockedOut");
        // Cross-check against the actual Debug output so any future
        // derive-Debug rename fails LOUDLY here, not silently in the
        // audit chain.
        assert_eq!(format!("{:?}", FleetPosture::Nominal), "Nominal");
        assert_eq!(format!("{:?}", FleetPosture::Degraded), "Degraded");
        assert_eq!(format!("{:?}", FleetPosture::LockedOut), "LockedOut");
    }
}

// ---------------------------------------------------------------------------
// Queue-behavior contract tests (B2 guard)
//
// Document and pin the `try_send` Full / Closed semantics the deny arm
// relies on for fail-closed-with-LOUD-log behavior. A future tokio mpsc
// change that altered these signatures would surface here.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod queue_behavior_tests {
    use super::*;

    fn sample_job() -> AuditWriteJob {
        AuditWriteJob {
            event_type: "TEST",
            payload: KinematicViolationPayload {
                posture_at_rejection: "Nominal",
                proposed_command: ProposedCommandPayload {
                    current_steering_angle_deg: 0.0,
                    current_velocity_mps: 0.0,
                    delta_time_s: 1.0,
                    linear_velocity_mps: 0.0,
                    steering_angle_deg: 0.0,
                },
                verdict_id: "0123456789abcdef0123456789abcdef".to_string(),
                violation: "TEST",
            },
            created_at_ms: 0,
            node_id: "test",
            reason: "test",
        }
    }

    #[tokio::test]
    async fn try_send_returns_full_at_capacity() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<AuditWriteJob>(2);
        assert!(tx.try_send(sample_job()).is_ok());
        assert!(tx.try_send(sample_job()).is_ok());
        let err = tx
            .try_send(sample_job())
            .expect_err("third send into a 2-bound channel must fail");
        assert!(
            matches!(err, tokio::sync::mpsc::error::TrySendError::Full(_)),
            "expected TrySendError::Full at capacity, got {err:?}"
        );
    }

    #[tokio::test]
    async fn try_send_returns_closed_when_receiver_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel::<AuditWriteJob>(8);
        drop(rx);
        let err = tx
            .try_send(sample_job())
            .expect_err("send into a closed channel must fail");
        assert!(
            matches!(err, tokio::sync::mpsc::error::TrySendError::Closed(_)),
            "expected TrySendError::Closed after receiver drop, got {err:?}"
        );
    }
}
