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

use crate::kinematics_contract::{
    EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use crate::FleetPosture;

// The capture record wire schema lives in the governor-free
// `kirra-capture-schema` crate (docs/COLLECTOR_DESIGN.md [C1]); re-export it so
// every existing `crate::capture::{CaptureRecord, ...}` /
// `kirra_verifier::capture::*` path keeps resolving, and so the SDK and the
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

/// Domain-separation tag for [`contract_digest_hex`]. Bumping this string is the
/// declared way to invalidate every previously recorded contract digest; it must
/// change if the field set or the field ORDER below changes, because either would
/// silently give the same envelope a different identity.
pub const CONTRACT_DIGEST_DOMAIN: &[u8] = b"KIRRA-CONTRACT-V1";

/// Identity of the RESOLVED kinematic contract a verdict was judged against:
/// domain-tagged SHA-256 over the IEEE bits of every field, in declaration
/// order. 64 lowercase hex chars.
///
/// This is the envelope the command was actually bounded by — on the Nominal arm
/// that is the class profile with the perception-derate cap already applied, so
/// it is strictly more informative than the vehicle class (which selects a
/// contract) or `derate_enabled` (which records only that a cap composed, never
/// its value).
///
/// `to_bits` rather than a float format: the point is a BIT-identical comparison
/// across two runs, and a decimal rendering would let two different envelopes
/// share a digest. `-0.0` and `0.0` therefore differ, as do distinct NaN
/// payloads — correct for an identity, and unreachable for a valid contract.
///
/// The `Option` is length-tagged (`0` / `1` + value) rather than encoded by
/// presence alone, so `None` can never collide with a `Some` whose bits happen to
/// start with the following field's.
#[must_use]
pub fn contract_digest_hex(contract: &VehicleKinematicsContract) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(CONTRACT_DIGEST_DOMAIN);
    // Declaration order. Adding a field here without bumping the domain tag is
    // the failure this constant exists to make explicit.
    for value in [
        contract.max_speed_mps,
        contract.max_accel_mps2,
        contract.max_brake_mps2,
        contract.max_steering_deg,
        contract.max_steering_rate_deg_s,
        contract.min_follow_distance_m,
        contract.max_lateral_accel_mps2,
        contract.wheelbase_m,
        contract.width_m,
        contract.length_m,
        contract.overhang_front_m,
        contract.overhang_rear_m,
    ] {
        h.update(value.to_bits().to_le_bytes());
    }
    match contract.odd_speed_cap_mps {
        None => h.update([0u8]),
        Some(cap) => {
            h.update([1u8]);
            h.update(cap.to_bits().to_le_bytes());
        }
    }
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        // Hand-rolled rather than pulling `hex` into the lean foundation for one
        // call site — and TOTAL: no indexing, no `Option`, no `expect`. The mask
        // makes the arithmetic in-range for every input, so there is no panic
        // branch here at all, reachable or otherwise. Capture is a side channel
        // that must never be able to take the process down.
        out.push(lower_hex_nibble(byte >> 4));
        out.push(lower_hex_nibble(byte));
    }
    out
}

/// The low nibble of `n` as a lowercase hex digit. Total by construction: the
/// mask bounds the input to `0..=15`, so both branches stay inside ASCII and
/// neither can overflow.
#[inline]
fn lower_hex_nibble(n: u8) -> char {
    let n = n & 0x0f;
    if n < 10 {
        (b'0' + n) as char
    } else {
        (b'a' + (n - 10)) as char
    }
}

/// Build a record from the already-computed gateway verdict + context. Pure;
/// performs no I/O. Called at the gateway emit site (off the verdict path).
///
/// `contract` is the RESOLVED envelope the verdict was produced against — the
/// caller must pass the same value it handed the checker, not a re-derivation,
/// or the recorded digest attests an envelope that did not bound the command.
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
    contract: &VehicleKinematicsContract,
) -> CaptureRecord {
    let (outcome, deny_code, safe_value) = match verdict {
        EnforceAction::Allow => (CaptureOutcome::Allow, None, None),
        EnforceAction::ClampLinear(v) => (CaptureOutcome::ClampLinear, None, Some(*v)),
        EnforceAction::ClampSteering(d) => (CaptureOutcome::ClampSteering, None, Some(*d)),
        // review H1: a both-axes clamp. The single `safe_value` field cannot
        // carry two corrections, so record it as `ClampLinear(linear)` — this
        // surfaces the LONGITUDINAL correction the pre-H1 code dropped entirely
        // (it recorded these as ClampSteering, losing the velocity clamp). The
        // steering correction stays derivable from the captured proposed command
        // + contract. Keeping the existing `CaptureOutcome` set avoids a
        // versioned wire-schema (kirra-capture-schema) bump for an off-by-default
        // observability path.
        EnforceAction::ClampBoth { linear, .. } => {
            (CaptureOutcome::ClampLinear, None, Some(*linear))
        }
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
        contract_digest: Some(contract_digest_hex(contract)),
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
        TrajectoryDecision::MrcFallback => (
            CaptureOutcome::Deny,
            Some("TRAJECTORY_MRC_FALLBACK".to_string()),
        ),
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
        // DELIBERATELY absent on the slow loop. The trajectory verdict is not
        // bounded by a single `VehicleKinematicsContract` — it composes
        // containment, per-pose kinematics, RSS and the occlusion/redundancy
        // caps — so there is no one envelope a digest here could honestly name.
        // Recording the fast-loop contract would attest an envelope this verdict
        // was not judged against, which is exactly the guess this crate refuses
        // to make; `None` says "not recorded" and the consumer fails closed.
        contract_digest: None,
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
    let sink_path =
        std::env::var(CAPTURE_SINK_PATH_ENV).unwrap_or_else(|_| "kirra_capture.jsonl".to_string());
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
        // H2: durability. Write each record atomically, then `sync_data()` once the
        // queue is momentarily drained (coalesced fsync per burst — not per record,
        // which would be needlessly heavy for best-effort training data). This bounds
        // the lost-tail-on-crash window to the in-flight burst while keeping the hot
        // producer path wait-free (producers only `try_send`). The final burst before
        // channel close is synced too, since `try_recv` drains all buffered records
        // before returning `Disconnected`.
        while let Some(rec) = rx.blocking_recv() {
            let mut wrote = write_one_capture(&mut sink, &rec);
            // Drain any immediately-available records (try_recv Err = Empty/Disconnected)
            // before syncing, so the fsync is coalesced per burst.
            while let Ok(rec) = rx.try_recv() {
                wrote |= write_one_capture(&mut sink, &rec);
            }
            if wrote {
                if let Err(e) = sink.sync_data() {
                    tracing::error!(error = %e, "capture writer: fsync (sync_data) failed");
                }
            }
        }
        tracing::info!("capture writer task exiting (channel closed)");
    });
    tx
}

/// Single-record write — JSONL line append. The only place serialization + I/O
/// for capture run (off the verdict path). Returns `true` if a record was written
/// (so the caller knows a deferred `sync_data` is pending).
///
/// H2: the line and its terminating `\n` are written in ONE `write_all` (not a
/// `writeln!`, which can issue body and newline as separate `write(2)` calls) so a
/// crash can never leave a torn final JSONL line that breaks downstream parsing.
fn write_one_capture(sink: &mut std::fs::File, rec: &CaptureRecord) -> bool {
    match serde_json::to_string(rec) {
        Ok(mut line) => {
            line.push('\n');
            if let Err(e) = sink.write_all(line.as_bytes()) {
                tracing::error!(error = %e, "capture writer: JSONL append failed; record dropped");
                false
            } else {
                true
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "capture writer: record serialize failed; dropped");
            false
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

    /// The reference envelope these builder tests record against. They assert the
    /// verdict→record MAPPING, not the envelope, so any concrete contract serves —
    /// the digest's own behaviour is pinned separately below.
    fn contract() -> VehicleKinematicsContract {
        VehicleKinematicsContract::nominal_reference_profile()
    }

    #[test]
    fn record_from_verdict_maps_each_arm() {
        let c = cmd();
        let allow = record_from_verdict(
            0,
            1000,
            &EnforceAction::Allow,
            FleetPosture::Nominal,
            &c,
            false,
            &contract(),
        );
        assert_eq!(allow.outcome, CaptureOutcome::Allow);
        assert_eq!(allow.deny_code, None);
        assert_eq!(allow.safe_value, None);
        assert!(!allow.mrc);
        assert_eq!(allow.posture, "NOMINAL");

        let cl = record_from_verdict(
            1,
            1000,
            &EnforceAction::ClampLinear(5.0),
            FleetPosture::Nominal,
            &c,
            true,
            &contract(),
        );
        assert_eq!(cl.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(cl.safe_value, Some(5.0));
        assert!(cl.derate_enabled);

        let cs = record_from_verdict(
            2,
            1000,
            &EnforceAction::ClampSteering(3.0),
            FleetPosture::Degraded,
            &c,
            false,
            &contract(),
        );
        assert_eq!(cs.outcome, CaptureOutcome::ClampSteering);
        assert_eq!(cs.safe_value, Some(3.0));
        assert!(cs.mrc, "Degraded → MRC envelope");
        assert_eq!(cs.posture, "DEGRADED");

        let dn = record_from_verdict(
            3,
            1000,
            &EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
            FleetPosture::Nominal,
            &c,
            false,
            &contract(),
        );
        assert_eq!(dn.outcome, CaptureOutcome::Deny);
        assert_eq!(dn.deny_code.as_deref(), Some("NAN_INF_LINEAR_VELOCITY"));
        assert_eq!(dn.safe_value, None);

        // review H1: ClampBoth records as ClampLinear carrying the LONGITUDINAL
        // correction (the axis the pre-H1 code dropped). No wire-schema bump.
        let cb = record_from_verdict(
            4,
            1000,
            &EnforceAction::ClampBoth {
                linear: 6.25,
                steering: 1.5,
            },
            FleetPosture::Nominal,
            &c,
            false,
            &contract(),
        );
        assert_eq!(cb.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(cb.safe_value, Some(6.25));
        assert_eq!(cb.deny_code, None);
    }

    fn traj_ext() -> TrajectoryCaptureExt {
        TrajectoryCaptureExt {
            asset_id: "ego".to_string(),
            trajectory_id: 7,
            objects_ms: 123_456,
            point_count: 12,
            object_count: 3,
            first_pose: Some(PoseSnapshot {
                x_m: 0.0,
                y_m: 0.0,
                heading_rad: 0.0,
            }),
            last_pose: Some(PoseSnapshot {
                x_m: 5.0,
                y_m: 1.0,
                heading_rad: 0.1,
            }),
            target_speed_mps: Some(8.0),
        }
    }

    #[test]
    fn record_from_trajectory_verdict_maps_each_decision() {
        let accept = record_from_trajectory_verdict(
            0,
            1000,
            TrajectoryDecision::Accept,
            FleetPosture::Nominal,
            traj_ext(),
            false,
        );
        assert_eq!(accept.outcome, CaptureOutcome::Allow);
        assert_eq!(accept.deny_code, None);
        assert!(!accept.mrc);
        assert_eq!(accept.source, CaptureSource::SlowLoopTrajectory);
        assert!(
            accept.proposed.is_none(),
            "trajectory record carries no command proposal"
        );
        assert_eq!(accept.traj.as_ref().unwrap().trajectory_id, 7);
        assert_eq!(accept.traj.as_ref().unwrap().objects_ms, 123_456);

        let clamp = record_from_trajectory_verdict(
            1,
            1000,
            TrajectoryDecision::Clamp,
            FleetPosture::Nominal,
            traj_ext(),
            true,
        );
        assert_eq!(clamp.outcome, CaptureOutcome::ClampLinear);
        assert_eq!(clamp.deny_code, None);
        assert!(clamp.derate_enabled);

        let mrc = record_from_trajectory_verdict(
            2,
            1000,
            TrajectoryDecision::MrcFallback,
            FleetPosture::Nominal,
            traj_ext(),
            false,
        );
        assert_eq!(mrc.outcome, CaptureOutcome::Deny);
        assert_eq!(mrc.deny_code.as_deref(), Some("TRAJECTORY_MRC_FALLBACK"));
        assert!(mrc.mrc, "MRCFallback → controlled stop");

        // Degraded posture forces mrc even on an Accept decision.
        let degraded = record_from_trajectory_verdict(
            3,
            1000,
            TrajectoryDecision::Accept,
            FleetPosture::Degraded,
            traj_ext(),
            false,
        );
        assert!(degraded.mrc, "Degraded posture → MRC envelope");
        assert_eq!(degraded.posture, "DEGRADED");
    }

    #[test]
    fn gateway_record_omits_traj_and_keeps_proposed_in_json() {
        // The command-gateway record must serialize WITH `proposed` and
        // WITHOUT `traj` (skip_serializing_if). The trajectory record is the
        // mirror image. (The wire shape itself is pinned in the schema crate.)
        let gw = record_from_verdict(
            0,
            1,
            &EnforceAction::Allow,
            FleetPosture::Nominal,
            &cmd(),
            false,
            &contract(),
        );
        let gw_json = serde_json::to_string(&gw).unwrap();
        assert!(gw_json.contains("\"source\":\"COMMAND_GATEWAY\""));
        assert!(gw_json.contains("\"proposed\""));
        assert!(!gw_json.contains("\"traj\""), "gateway record omits traj");

        let tj = record_from_trajectory_verdict(
            0,
            1,
            TrajectoryDecision::Accept,
            FleetPosture::Nominal,
            traj_ext(),
            false,
        );
        let tj_json = serde_json::to_string(&tj).unwrap();
        assert!(tj_json.contains("\"source\":\"SLOW_LOOP_TRAJECTORY\""));
        assert!(tj_json.contains("\"traj\""));
        assert!(
            !tj_json.contains("\"proposed\""),
            "trajectory record omits proposed"
        );
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
        let rec = record_from_verdict(
            0,
            1,
            &EnforceAction::Allow,
            FleetPosture::Nominal,
            &cmd(),
            false,
            &contract(),
        );
        assert!(tx.try_send(rec.clone()).is_ok());
        match tx.try_send(rec) {
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
            other => panic!("expected Full at capacity, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // contract_digest_hex — the identity of the RESOLVED envelope (Tier 2.5)
    // -----------------------------------------------------------------------

    /// Every field of the contract, as a named mutator. Used by the non-vacuity
    /// sweep below: a digest that ignores a field would let two different
    /// envelopes share an identity, which is the one thing it must never do.
    #[allow(clippy::type_complexity)]
    fn field_mutators() -> Vec<(&'static str, fn(&mut VehicleKinematicsContract))> {
        vec![
            ("max_speed_mps", |c| c.max_speed_mps += 1.0),
            ("max_accel_mps2", |c| c.max_accel_mps2 += 1.0),
            ("max_brake_mps2", |c| c.max_brake_mps2 += 1.0),
            ("max_steering_deg", |c| c.max_steering_deg += 1.0),
            ("max_steering_rate_deg_s", |c| {
                c.max_steering_rate_deg_s += 1.0
            }),
            ("min_follow_distance_m", |c| c.min_follow_distance_m += 1.0),
            ("max_lateral_accel_mps2", |c| {
                c.max_lateral_accel_mps2 += 1.0
            }),
            ("wheelbase_m", |c| c.wheelbase_m += 1.0),
            ("width_m", |c| c.width_m += 1.0),
            ("length_m", |c| c.length_m += 1.0),
            ("overhang_front_m", |c| c.overhang_front_m += 1.0),
            ("overhang_rear_m", |c| c.overhang_rear_m += 1.0),
            ("odd_speed_cap_mps", |c| c.odd_speed_cap_mps = Some(7.5)),
        ]
    }

    /// NON-VACUITY: changing ANY field changes the digest. A digest that misses a
    /// field would make the Tier 2.5 differential proof unsound in the worst
    /// direction — the bounds could move while the check reported them equal.
    #[test]
    fn every_contract_field_participates_in_the_digest() {
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let base_digest = contract_digest_hex(&base);
        for (name, mutate) in field_mutators() {
            let mut mutated = base;
            mutate(&mut mutated);
            assert_ne!(
                contract_digest_hex(&mutated),
                base_digest,
                "mutating `{name}` must change the contract digest"
            );
        }
    }

    /// Each mutated field yields a DISTINCT digest from every other — not merely
    /// "different from base". Catches a digest that folds two fields together
    /// (e.g. summing before hashing), which the per-field test alone would miss.
    #[test]
    fn distinct_field_changes_yield_distinct_digests() {
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let mut seen: Vec<(String, String)> = Vec::new();
        for (name, mutate) in field_mutators() {
            let mut mutated = base;
            mutate(&mut mutated);
            let digest = contract_digest_hex(&mutated);
            if let Some((other, _)) = seen.iter().find(|(_, d)| *d == digest) {
                panic!("`{name}` and `{other}` produced the same digest");
            }
            seen.push((name.to_string(), digest));
        }
    }

    /// Deterministic and 64 lowercase hex chars — the shape the wire field and
    /// every cross-run comparison assume.
    #[test]
    fn the_digest_is_deterministic_and_well_formed() {
        let c = VehicleKinematicsContract::nominal_reference_profile();
        let a = contract_digest_hex(&c);
        assert_eq!(a, contract_digest_hex(&c), "must be deterministic");
        assert_eq!(a.len(), 64, "{a}");
        assert!(
            a.chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
            "must be lowercase hex: {a}"
        );
    }

    /// The hand-rolled hex encoder agrees with the standard formatter for EVERY
    /// byte value. A hand-rolled encoder without an exhaustive check is a
    /// liability: a wrong nibble would corrupt digests silently, and every
    /// comparison downstream would still "pass" by comparing two corrupt values.
    #[test]
    fn the_hex_encoder_matches_the_standard_formatter_for_all_256_bytes() {
        for b in 0u8..=255 {
            let hand = format!("{}{}", lower_hex_nibble(b >> 4), lower_hex_nibble(b));
            assert_eq!(hand, format!("{b:02x}"), "byte {b}");
        }
    }

    /// The `Option` is length-tagged, so `None` and `Some` are never confusable.
    #[test]
    fn an_absent_odd_cap_is_distinguishable_from_a_present_one() {
        let mut none = VehicleKinematicsContract::nominal_reference_profile();
        none.odd_speed_cap_mps = None;
        let mut some = none;
        some.odd_speed_cap_mps = Some(0.0);
        assert_ne!(
            contract_digest_hex(&none),
            contract_digest_hex(&some),
            "None must not collide with Some(0.0)"
        );
    }

    /// THE PROPERTY TIER 2.5 DEPENDS ON: the perception-derate cap is part of the
    /// envelope's identity. `derate_enabled` records only THAT a cap composed;
    /// the digest records WHICH envelope resulted, so two runs whose caps differ
    /// cannot report identical bounds.
    #[test]
    fn the_perception_derate_cap_changes_the_digest() {
        use crate::perception_monitor::apply_perception_cap;
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let uncapped = apply_perception_cap(&base, None);
        let capped = apply_perception_cap(&base, Some(3.0));
        assert_eq!(
            contract_digest_hex(&uncapped),
            contract_digest_hex(&base),
            "no cap must leave the envelope identity untouched"
        );
        assert_ne!(
            contract_digest_hex(&capped),
            contract_digest_hex(&base),
            "a composed cap must change the envelope identity"
        );
        // And two DIFFERENT caps are two different envelopes.
        let capped_tighter = apply_perception_cap(&base, Some(2.0));
        assert_ne!(
            contract_digest_hex(&capped),
            contract_digest_hex(&capped_tighter)
        );
    }

    /// The Nominal and MRC profiles are different envelopes, so the digest
    /// separates the two posture arms without needing the posture field.
    #[test]
    fn nominal_and_mrc_profiles_have_different_digests() {
        assert_ne!(
            contract_digest_hex(&VehicleKinematicsContract::nominal_reference_profile()),
            contract_digest_hex(&VehicleKinematicsContract::mrc_fallback_profile())
        );
    }

    /// The domain tag is actually mixed in — a bare hash of the fields is a
    /// different value, so the tag cannot be dropped without this failing.
    #[test]
    fn the_domain_tag_is_part_of_the_digest() {
        use sha2::{Digest, Sha256};
        let c = VehicleKinematicsContract::nominal_reference_profile();
        let mut untagged = Sha256::new();
        for value in [
            c.max_speed_mps,
            c.max_accel_mps2,
            c.max_brake_mps2,
            c.max_steering_deg,
            c.max_steering_rate_deg_s,
            c.min_follow_distance_m,
            c.max_lateral_accel_mps2,
            c.wheelbase_m,
            c.width_m,
            c.length_m,
            c.overhang_front_m,
            c.overhang_rear_m,
        ] {
            untagged.update(value.to_bits().to_le_bytes());
        }
        untagged.update([0u8]);
        let untagged_hex: String = untagged
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_ne!(
            contract_digest_hex(&c),
            untagged_hex,
            "the domain tag must contribute"
        );
    }

    /// The gateway builder records the digest of the contract it was HANDED —
    /// not a re-derivation. This is what makes the recorded envelope the one that
    /// actually bounded the command.
    #[test]
    fn the_gateway_record_carries_the_digest_of_the_passed_contract() {
        use crate::perception_monitor::apply_perception_cap;
        // A capped contract, deliberately NOT equal to any class default, so a
        // re-derivation inside the builder would produce a different value.
        let capped = apply_perception_cap(
            &VehicleKinematicsContract::nominal_reference_profile(),
            Some(4.25),
        );
        let rec = record_from_verdict(
            0,
            1000,
            &EnforceAction::Allow,
            FleetPosture::Nominal,
            &cmd(),
            true,
            &capped,
        );
        assert_eq!(
            rec.contract_digest.as_deref(),
            Some(contract_digest_hex(&capped).as_str())
        );
    }

    /// The slow-loop builder records NO digest. No single `VehicleKinematicsContract`
    /// bounds a trajectory verdict, so naming one would attest an envelope the
    /// verdict was not judged against.
    #[test]
    fn the_slow_loop_record_carries_no_contract_digest() {
        let traj = TrajectoryCaptureExt {
            asset_id: "ego".to_string(),
            trajectory_id: 1,
            objects_ms: 10,
            point_count: 2,
            object_count: 0,
            first_pose: None,
            last_pose: None,
            target_speed_mps: Some(1.0),
        };
        let rec = record_from_trajectory_verdict(
            0,
            1000,
            TrajectoryDecision::Accept,
            FleetPosture::Nominal,
            traj,
            false,
        );
        assert_eq!(rec.contract_digest, None);
    }
}
