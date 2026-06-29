// src/gateway/policy_layer.rs
//
// Actuator safety envelope middleware for Kirra AV flight envelope protection.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::audit_writer::{
    fleet_posture_str, AuditWriteJob, KinematicViolationPayload, ProposedCommandPayload,
};
use crate::gateway::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, EnforceAction,
    ProposedVehicleCommand, VehicleKinematicsContract,
};
use crate::gateway::perception_monitor::{apply_perception_cap, resolve_perception_cap};
use crate::gateway::policy::{classify_http_command, OperationalCommand};
use crate::posture_cache::{
    now_ms as posture_now_ms, should_route_command, CachedFleetPosture, ServiceState,
};
use crate::verifier::FleetPosture;
use crate::verifier_store::FenceError;

/// Hard ceiling on an actuator-command request body. A `ProposedVehicleCommand`
/// is ~5 × f64 plus serde overhead — a few hundred bytes serialized. 16 KiB is
/// generous headroom. Anything larger is malformed or hostile; we reject fail
/// closed (413) before allocating the body. Bounding this prevents an
/// unbounded-allocation DoS on the actuator perimeter — the previous
/// `to_bytes(body, usize::MAX)` would happily buffer a multi-gigabyte stream.
const MAX_VEHICLE_COMMAND_BYTES: usize = 16 * 1024;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Resolves the current FleetPosture from the SharedPostureCache for the inner
/// actuator-envelope gate.
///
/// Auth-M1: this delegates to `resolve_posture_with_reason` at the single TTL
/// authority (`POSTURE_CACHE_TTL_MS`), so a STALE cache fails closed to LockedOut
/// here too — not just empty/poisoned. Previously the inner gate served a stale
/// `Nominal`/`Degraded` as current and relied entirely on the outer
/// `enforce_posture_routing` gate to catch staleness; now the inner safety gate is
/// independently fail-closed on stale/empty/poisoned, matching its doc and the
/// outer gate's behavior (defense-in-depth).
// SAFETY: SG8 SG9 | REQ: posture-resolve-fails-closed-locked-out | TEST: test_none_cache_denies_all_commands,test_empty_posture_cache_fails_closed_as_locked_out
// (Stale, missing, and poisoned-lock all map to LockedOut; SG8 = correct
//  MRC selection on degraded, SG9 = fail-closed on staleness/lock/cache anomaly.)
fn resolve_posture(svc: &ServiceState) -> FleetPosture {
    let (posture, _reason) = crate::posture_engine_v2::resolve_posture_with_reason(
        &svc.posture_cache,
        crate::posture_cache::POSTURE_CACHE_TTL_MS,
    );
    posture
}

/// Layer-3 HA actuator authority assertion.
///
/// The actuator route has no durable application write to hang the existing
/// in-transaction HA fence on, so it gets its own bounded assertion transaction
/// (`VerifierStore::assert_actuator_epoch_held`) before command admission. Any
/// ambiguity about role, epoch ownership, or disk health self-demotes this
/// process and rejects the command.
///
/// SAFETY: SG-009 / HA-L3 / REQ-HA-ACTUATOR-EPOCH-FENCE,
/// REQ-HA-DISK-WEDGE-DEMOTE.
pub async fn assert_actuator_epoch_or_demote(
    svc: &ServiceState,
    method: &'static str,
    path: &'static str,
) -> Result<(), StatusCode> {
    let held = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
    let outcome = svc
        .app
        .store
        .call(move |store| store.assert_actuator_epoch_held(held))
        .await;

    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(FenceError::EpochSuperseded { held, durable })) => {
            svc.app
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                method = method,
                path = path,
                held = held,
                durable = durable,
                ha_req = "REQ-HA-ACTUATOR-EPOCH-FENCE",
                "FENCED — actuator epoch assertion failed; self-demoting and rejecting command"
            );
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
        Ok(Err(FenceError::EpochUnreadable)) | Err(_) => {
            svc.app
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                method = method,
                path = path,
                held = held,
                ha_req = "REQ-HA-DISK-WEDGE-DEMOTE",
                "DISK-WEDGE — actuator epoch unreadable; self-demoting and rejecting command"
            );
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

/// The enforcement verdict the actuator middleware reached for one command,
/// threaded to the downstream handler via a request extension so the HTTP
/// RESPONSE can report it faithfully.
///
/// WHY THIS EXISTS (Phase 0 schema-coherence finding): the middleware clamps a
/// command by transparently rewriting the request body, so the handler could
/// not tell whether a clamp happened — it always reported `"Allow"`, and it
/// emitted the enforced value only under `linear_velocity_mps` /
/// `steering_angle_deg`. The ROS `cmd_vel_interceptor` reads `action` /
/// `enforced_linear_velocity_mps` / `enforced_steering_angle_deg`; finding none
/// of them, it fell back to forwarding the ORIGINAL (unclamped) command to the
/// motors — the gateway's clamp never reached the actuator. This type carries
/// the verdict + both original and enforced values so the response can speak
/// the interceptor's schema AND keep the legacy keys (accurately).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnforcementOutcome {
    pub action: EnforcementOutcomeKind,
    pub original_linear_velocity_mps: f64,
    pub original_steering_angle_deg: f64,
    pub enforced_linear_velocity_mps: f64,
    pub enforced_steering_angle_deg: f64,
}

/// The enforcement action a 200 response carries (deny is a 4xx, not a 200).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementOutcomeKind {
    /// Command was within the envelope — forwarded unchanged.
    Allow,
    /// Linear velocity was clamped to the envelope ceiling.
    ClampLinear,
    /// Steering angle was clamped to the envelope limit.
    ClampSteering,
}

impl EnforcementOutcomeKind {
    /// Stable wire string. Matches the values the ROS interceptor and the CARLA
    /// client already switch on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::ClampLinear => "ClampLinear",
            Self::ClampSteering => "ClampSteering",
        }
    }
}

impl EnforcementOutcome {
    fn allow(cmd: &ProposedVehicleCommand) -> Self {
        Self {
            action: EnforcementOutcomeKind::Allow,
            original_linear_velocity_mps: cmd.linear_velocity_mps,
            original_steering_angle_deg: cmd.steering_angle_deg,
            enforced_linear_velocity_mps: cmd.linear_velocity_mps,
            enforced_steering_angle_deg: cmd.steering_angle_deg,
        }
    }

    fn clamp_linear(cmd: &ProposedVehicleCommand, safe_speed: f64) -> Self {
        Self {
            action: EnforcementOutcomeKind::ClampLinear,
            original_linear_velocity_mps: cmd.linear_velocity_mps,
            original_steering_angle_deg: cmd.steering_angle_deg,
            enforced_linear_velocity_mps: safe_speed,
            enforced_steering_angle_deg: cmd.steering_angle_deg,
        }
    }

    fn clamp_steering(cmd: &ProposedVehicleCommand, safe_angle: f64) -> Self {
        Self {
            action: EnforcementOutcomeKind::ClampSteering,
            original_linear_velocity_mps: cmd.linear_velocity_mps,
            original_steering_angle_deg: cmd.steering_angle_deg,
            enforced_linear_velocity_mps: cmd.linear_velocity_mps,
            enforced_steering_angle_deg: safe_angle,
        }
    }

    /// The 200 response body. Carries BOTH the interceptor-aligned keys
    /// (`action`, `enforced_*`) — the fix — AND the legacy keys
    /// (`enforcement_action`, `linear_velocity_mps`, `steering_angle_deg`),
    /// now accurate, for the CARLA client and any existing reader. Also
    /// surfaces the original (pre-enforcement) values for observability.
    #[must_use]
    pub fn response_body(&self) -> serde_json::Value {
        serde_json::json!({
            // Interceptor-aligned keys (the schema-coherence fix):
            "action": self.action.as_str(),
            "enforced_linear_velocity_mps": self.enforced_linear_velocity_mps,
            "enforced_steering_angle_deg": self.enforced_steering_angle_deg,
            // Legacy keys, now accurate (CARLA client reads enforcement_action;
            // the value fields carry the ENFORCED command):
            "enforcement_action": self.action.as_str(),
            "linear_velocity_mps": self.enforced_linear_velocity_mps,
            "steering_angle_deg": self.enforced_steering_angle_deg,
            // Pre-enforcement values, for observability / interceptor logging:
            "original_linear_velocity_mps": self.original_linear_velocity_mps,
            "original_steering_angle_deg": self.original_steering_angle_deg,
        })
    }
}

/// Actuator command safety envelope middleware.
///
/// Intercepts inbound actuator motion commands, resolves the active fleet posture,
/// selects the appropriate VehicleKinematicsContract, and enforces all physical
/// invariants before the request reaches any downstream handler.
///
/// Posture → enforcement mapping:
///   Nominal   → nominal_reference_profile() — full operational envelope
///   Degraded  → controlled decel-to-stop-and-HOLD (Issue #70): the MRC
///               envelope as the decel-trajectory bound, PLUS a
///               non-increasing-speed + no-re-initiation gate
///               (`enforce_degraded_decel_to_stop`). A stopped vehicle is
///               held at rest; a moving vehicle may only bleed speed toward
///               zero. Speed increase / re-initiation → DenyBreach → MRC stop.
///   LockedOut → immediate 403 FORBIDDEN     — fail-closed, no physics evaluation
///
/// # Invariants
/// - Uses State<Arc<ServiceState>> (invariant #11)
/// - FleetPosture from crate::verifier
/// - SharedPostureCache accessed via svc.posture_cache
/// - LockedOut is always fail-closed
pub async fn enforce_actuator_safety_envelope(
    State(svc): State<Arc<ServiceState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let posture = resolve_posture(&svc);

    // SAFETY: SG8 | REQ: posture-to-contract-mrc-selection | TEST: test_degraded_posture_selects_mrc_contract,test_degraded_posture_clamps_high_speed_to_mrc_limit,test_locked_out_posture_has_no_contract,test_locked_out_rejects_zero_motion_command
    // (Nominal → nominal envelope; Degraded → decel-to-stop-and-HOLD gate
    //  over the MRC envelope (Issue #70); LockedOut → fail-closed 403 with no
    //  command body even parsed.) LockedOut short-circuits here so the
    //  fail-closed path never touches the request body.
    if posture == FleetPosture::LockedOut {
        tracing::error!(
            "Actuator command rejected: fleet posture is LockedOut — \
             all actuator mutations are blocked until posture recovers"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let (mut parts, body) = req.into_parts();

    // Bounded read — see MAX_VEHICLE_COMMAND_BYTES. axum::body::to_bytes
    // returns Err when the body exceeds the cap, so allocation is capped
    // at MAX_VEHICLE_COMMAND_BYTES regardless of what the client streams.
    // 413 (Payload Too Large) is the precise status for oversize; we use
    // it for both the oversize and the generic read-error cases (fail
    // closed either way — the command is rejected, never forwarded).
    let bytes = axum::body::to_bytes(body, MAX_VEHICLE_COMMAND_BYTES)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    let proposed_cmd: ProposedVehicleCommand =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;

    // §7: read the clock ONCE per request and thread it — the perception-cap
    // staleness read, the capture record, and the audit record all want "now" and
    // are microseconds apart, so a single `SystemTime::now()` syscall is both
    // cheaper and more consistent than the three separate reads this path used.
    let now = now_ms();

    // Issue #70: Nominal runs the full envelope; Degraded runs the
    // decel-to-stop-and-HOLD gate (non-increasing speed + no re-initiation)
    // over the MRC envelope. LockedOut was already short-circuited above.
    let verdict = match posture {
        FleetPosture::Nominal => {
            // KIRRA-OCCY-PMON-002 composition: read the perception-derate cap
            // O(1) (3-state resolver — None when the monitor is disabled, MRC
            // floor when an enabled monitor is stale/silent) and tighten the
            // Nominal contract via `apply_perception_cap` BEFORE the verdict
            // call. `validate_vehicle_command`/`effective_max_speed_mps` are
            // unchanged; the only added per-command cost is this O(1) read.
            let eff_cap = resolve_perception_cap(
                svc.perception_monitor_enabled,
                &svc.perception_cap,
                now,
            );
            let contract = apply_perception_cap(
                &VehicleKinematicsContract::nominal_reference_profile(),
                eff_cap,
            );
            validate_vehicle_command(&proposed_cmd, &contract)
        }
        FleetPosture::Degraded => enforce_degraded_decel_to_stop(
            &proposed_cmd,
            &VehicleKinematicsContract::mrc_fallback_profile(),
        ),
        FleetPosture::LockedOut => unreachable!("LockedOut short-circuited above"),
    };

    // Learning-loop capture (Phase 1, #190) — ADDITIVE, gated, wait-free.
    // Reads the already-computed `verdict` and records the correction half of
    // the corrective-supervision triple on a non-safety side channel. It NEVER
    // gates/delays/alters the verdict, the EnforcementOutcome, or the response
    // (INV-2) — note it sits BEFORE the response dispatch and only borrows the
    // outcome. Default OFF (INV-3): with no writer installed this is a no-op.
    // `try_send` drops on Full/Closed with a loud log (INV-4) — safety never
    // waits. A single site here captures EVERY arm (passes included, to avoid
    // downstream selection bias); it sits beside the Deny-arm audit `try_send`,
    // not replacing it. Gated SOLELY by writer presence (mirrors the audit
    // emit): the `KIRRA_CAPTURE_ENABLED` env decides INSTALLATION at startup, so
    // default-off / tests → `get()` is `None` → pure no-op (INV-3).
    if let Some(tx) = svc.app.capture_writer_tx.get() {
        let rec = crate::capture::record_from_verdict(
            svc.app
                .capture_decision_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            now,
            &verdict,
            posture,
            &proposed_cmd,
            svc.perception_monitor_enabled,
        );
        match tx.try_send(rec) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                svc.app.capture_drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    "capture queue FULL — dropping verdict record (best-effort; safety never waits)"
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                svc.app.capture_drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!("capture writer task GONE — verdict record dropped");
            }
        }
    }

    match verdict {
        EnforceAction::Allow => {
            // Thread the verdict to the handler so the response reports it.
            parts.extensions.insert(EnforcementOutcome::allow(&proposed_cmd));
            let rebuilt = Request::from_parts(parts, Body::from(bytes));
            Ok(next.run(rebuilt).await)
        }

        EnforceAction::ClampLinear(safe_speed) => {
            tracing::warn!(
                requested_mps = %proposed_cmd.linear_velocity_mps,
                clamped_mps   = %safe_speed,
                "Kinematic envelope breach: linear velocity clamped"
            );
            let mut clamped_cmd = proposed_cmd.clone();
            clamped_cmd.linear_velocity_mps = safe_speed;
            let serialized = serde_json::to_vec(&clamped_cmd)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            parts
                .extensions
                .insert(EnforcementOutcome::clamp_linear(&proposed_cmd, safe_speed));
            let rebuilt = Request::from_parts(parts, Body::from(serialized));
            Ok(next.run(rebuilt).await)
        }

        EnforceAction::ClampSteering(safe_angle) => {
            tracing::warn!(
                requested_deg = %proposed_cmd.steering_angle_deg,
                clamped_deg   = %safe_angle,
                "Kinematic envelope breach: steering angle clamped"
            );
            let mut clamped_cmd = proposed_cmd.clone();
            clamped_cmd.steering_angle_deg = safe_angle;
            let serialized = serde_json::to_vec(&clamped_cmd)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            parts
                .extensions
                .insert(EnforcementOutcome::clamp_steering(&proposed_cmd, safe_angle));
            let rebuilt = Request::from_parts(parts, Body::from(serialized));
            Ok(next.run(rebuilt).await)
        }

        EnforceAction::DenyBreach(code) => {
            tracing::error!(
                reason               = %code,
                linear_velocity_mps  = %proposed_cmd.linear_velocity_mps,
                steering_angle_deg   = %proposed_cmd.steering_angle_deg,
                delta_time_s         = %proposed_cmd.delta_time_s,
                "Inadmissible actuator command rejected at kinematic safety perimeter"
            );

            // Pass B2 + B3 (S3 / #115): build the audit job with byte-identical
            // typed payload and hand it to the writer task via try_send. The
            // verdict path now takes NO store.lock() here. Channel-full /
            // writer-gone are best-effort drops with LOUD logs (matches the
            // previous fire-and-best-effort behavior — the 400 response was
            // never gated on the audit write succeeding). Field-for-field
            // alphabetical ordering preserves audit-chain hash stability;
            // see audit_writer::byte_identity_tests.
            let job = AuditWriteJob {
                event_type: "KINEMATIC_CONTRACT_VIOLATION",
                payload: KinematicViolationPayload {
                    posture_at_rejection: fleet_posture_str(&posture),
                    proposed_command: ProposedCommandPayload {
                        current_steering_angle_deg: proposed_cmd.current_steering_angle_deg,
                        current_velocity_mps: proposed_cmd.current_velocity_mps,
                        delta_time_s: proposed_cmd.delta_time_s,
                        linear_velocity_mps: proposed_cmd.linear_velocity_mps,
                        steering_angle_deg: proposed_cmd.steering_angle_deg,
                    },
                    violation: code.reason(),
                },
                created_at_ms: now as i64,
                node_id: "actuator_safety_envelope",
                reason: "Proposed vehicle command violates non-physical invariants",
            };

            if let Some(tx) = svc.app.audit_writer_tx.get() {
                match tx.try_send(job) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        svc.app.audit_write_drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::error!(
                            reason = %code,
                            "audit queue FULL — dropping kinematic DenyBreach record; sequence gap will be detectable in the chain"
                        );
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        svc.app.audit_write_drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::error!(
                            reason = %code,
                            "audit writer task GONE — kinematic DenyBreach record dropped"
                        );
                    }
                }
            } else {
                // Writer not installed (test harness / transitional). Fall back
                // to the direct lock+save so existing tests observing chain
                // entries still pass. Production main always installs the
                // writer at startup; this branch is unreachable in deployment.
                let event_json = serde_json::to_string(&job.payload).unwrap_or_default();
                // SAFETY: SG-HA-3 — durable writes must never block the async runtime.
                // SAFETY: SG-HA-4 — DB errors are logged and command remains denied (fail-closed).
                match svc
                    .app
                    .store
                    .call(move |store| {
                        store.save_posture_event_chained(
                            job.node_id,
                            job.event_type,
                            &event_json,
                            Some(job.reason),
                            job.created_at_ms as u64,
                        )
                    })
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, reason = %code,
                            "AUDIT-CHAIN WRITE FAILED (fallback path) for kinematic DenyBreach");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, reason = %code,
                            "AUDIT fallback write task failed for kinematic DenyBreach");
                    }
                }
            }

            Err(StatusCode::BAD_REQUEST)
        }
    }
}

/// Paths exempt from the posture-routing gate so the service remains
/// liveness-probeable and observable regardless of fleet posture.
///
/// JUDGMENT-CALL refinement to "LockedOut blocks everything including
/// reads": a literal reading deadlocks cold start (posture cache is
/// initially `None`, which `should_route_command` blocks unconditionally)
/// and prevents external liveness probes from confirming the process is
/// alive. The minimal allowlist below is liveness + metrics only;
/// readiness MAY still reflect posture inside its own handler. The full
/// exemption registry — liveness/observability + the `/console` plane — and the
/// "`/console` is read-only EXCEPT the supervisor-key-gated grant" invariant are
/// documented in `docs/safety/SECURITY_BOUNDARIES.md` ("Posture-Routing Gate —
/// the Exemption Registry", #306). The set is pinned by
/// `console_exemption_set_is_pinned` below.
fn is_posture_exempt(path: &str) -> bool {
    matches!(path, "/health" | "/health/live" | "/ready" | "/metrics")
        // Operator console (#103 SG6 / Phase A): the observe-and-recover plane.
        // It MUST be reachable regardless of fleet posture — it is exactly the
        // plane an operator uses to SEE a LockedOut fleet and record a supervisor
        // clearance grant; a posture-gated console would lock the operator out of
        // the recovery affordance when it is most needed. Its one mutation
        // (`POST /console/clearance-grants`) is gated by the supervisor key IN THE
        // HANDLER (an out-of-band operator action, not a fleet command), not by
        // fleet posture. Reads under `/console` are QM.
        || path == "/console"
        || path.starts_with("/console/")
}

/// Global command-classification + posture-routing gate.
///
/// Mounts as the outermost layer of the assembled router. Every inbound
/// request is classified into an `OperationalCommand` via
/// `classify_http_command` and passed through `should_route_command`
/// against a fail-closed snapshot of the posture cache. A denied request
/// returns HTTP 503 SERVICE_UNAVAILABLE — posture denial is a transient
/// SERVER-STATE condition (LockedOut / Degraded / cold-or-stale cache),
/// retryable once posture recovers; matches `require_admin_token`'s 503
/// shape in this codebase rather than a per-client 403.
///
/// Fail-closed: a poisoned cache lock snapshots as `None`, which
/// `should_route_command` blocks.
///
/// Liveness / observability paths (`/health`, `/ready`, `/metrics`) are
/// allowlisted via `is_posture_exempt`; everything else, including
/// functional READS, is gated.
pub async fn enforce_posture_routing(
    State(svc): State<Arc<ServiceState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Borrow path + method as `&str` directly from the request. The `.to_string()`
    // calls these replaced allocated a `String` per request on the posture-routing
    // hot path. Borrows end at the last use below, before `next.run(req)` moves
    // the request, so NLL keeps the function well-formed (S3 / #115).
    let path = req.uri().path();
    if is_posture_exempt(path) {
        return Ok(next.run(req).await);
    }

    let method = req.method().as_str();
    let cmd = classify_http_command(method, path);

    // HA epoch fence (durable split-brain guard).
    //
    // For STATE-MUTATING commands only, compare our held epoch against the
    // DB epoch. If they diverge we have been fenced — another instance
    // claimed a higher epoch via the conditional UPDATE in
    // `try_claim_epoch`. Self-demote (mode_active → false) and reject
    // with the same 503 shape as other transient gate denials. Reads stay
    // exempt so a self-demoted node still serves health/metrics/reads.
    //
    // TOCTOU NOTE (closed for top-tier writes — issue #79): this gate reads a
    // CACHED epoch, and the actual write lands a moment later in the handler, so
    // a promotion that lands in that window could otherwise let ONE stale
    // mutation slip past this check. That window is now closed at the durable
    // layer: the top-tier durable writes (`save_federated_report_chained`,
    // `record_key_rotation`) re-check the held epoch INSIDE their write
    // transaction via `VerifierStore::assert_epoch_held`, so a superseded node's
    // commit is rejected (and the handler self-demotes) even if it passed this
    // gate. This gate remains the fast first-line fence; the in-transaction
    // re-check is the authoritative one.
    match cmd {
        // Layer 3 — ACTUATOR PATH gets an authoritative assertion transaction.
        // Unlike the top-tier durable writes (federation / key-rotation), an
        // actuator command performs no natural DB commit, so a cached epoch or a
        // read-replica observation leaves a residual failover window. The helper
        // below runs `BEGIN IMMEDIATE` + `assert_epoch_held` on the durable writer:
        // no heap allocation, O(1), bounded by SQLite's busy timeout, and
        // fail-closed on mismatch/unreadable epoch. The handler repeats the same
        // assertion immediately before its admitted response, after body parsing
        // and any audit work, so an epoch change during the actuator write is
        // caught at the final authority boundary too.
        OperationalCommand::ActuatorMotion => {
            assert_actuator_epoch_or_demote(&svc, "POST", "/actuator/motion/command").await?;
        }
        // Other mutations keep the fast CACHED epoch check (Pass B1): they ARE
        // backstopped by the in-transaction `assert_epoch_held` on their durable
        // write, so a stale cache here can at worst let one write REACH the durable
        // layer, where it is rejected. `cached_db_epoch` is re-stamped by
        // `perform_promotion` (post-CAS) and the heartbeat writer, both Release;
        // Acquire here pairs with both. A 0 cache (cold start) falls through to the
        // mode/posture checks below.
        OperationalCommand::WriteState | OperationalCommand::SystemMutation => {
            let held = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
            let db = svc.app.cached_db_epoch.load(std::sync::atomic::Ordering::Acquire);
            if db != 0 && held != 0 && held != db {
                svc.app
                    .mode_active
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                tracing::error!(
                    method = %method,
                    path   = %path,
                    held   = held,
                    db     = db,
                    "FENCED — held epoch stale; self-demoting and rejecting mutation (HA split-brain prevention)"
                );
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
        // ActuatorMotion with held == 0 (this instance never claimed an epoch —
        // cold start / standby) falls through to the mode/posture checks below,
        // which are fail-closed for mutations on a non-Active instance. Reads /
        // Unknown are not epoch-fenced.
        _ => {}
    }

    // Fail-closed snapshot: poisoned lock -> None -> block.
    let snapshot: Option<CachedFleetPosture> = match svc.posture_cache.read() {
        Ok(g) => *g,
        Err(_) => None,
    };

    if !should_route_command(&snapshot, posture_now_ms(), cmd) {
        tracing::warn!(
            method = %method,
            path = %path,
            command = ?cmd,
            "posture-routing gate denied command (fail-closed)"
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod actuator_middleware_tests {
    use super::*;
    use crate::fabric::causal_log::FabricCausalLog;
    use crate::fabric::router::FabricRouter;
    use crate::fabric::telemetry::FabricTelemetry;
    use crate::gateway::kinematics_contract::{ProposedVehicleCommand, VehicleKinematicsContract};
    use crate::gateway::perception_monitor::SharedPerceptionCap;
    use crate::posture_cache::{SharedPostureCache, POSTURE_CACHE_TTL_MS};
    use crate::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use crate::verifier_store::VerifierStore;
    use std::sync::atomic::Ordering;

    fn temp_db_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kirra-ha-actuator-{}-{}-{}.sqlite",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn service_from_store(store: VerifierStore) -> Arc<ServiceState> {
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture {
                posture: FleetPosture::Nominal,
                generated_at_ms: posture_now_ms(),
                ttl_ms: POSTURE_CACHE_TTL_MS,
                generation: 1,
            })));
        let perception_cap: SharedPerceptionCap = Arc::new(std::sync::RwLock::new(None));
        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: posture_now_ms(),
            audit_verifying_key: None,
            fabric_router: Arc::new(FabricRouter::new()),
            fabric_telemetry: Arc::new(FabricTelemetry::new()),
            fabric_causal_log: Arc::new(FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap,
            perception_monitor_enabled: false,
        })
    }

    fn claim_epoch(svc: &ServiceState, observed: u64, holder: &str, now_ms: u64) -> u64 {
        let claimed = svc
            .app
            .store
            .with(|store| store.try_claim_epoch(observed, holder, now_ms))
            .expect("epoch claim sql")
            .expect("epoch claim wins");
        svc.app.held_epoch.store(claimed, Ordering::SeqCst);
        claimed
    }

    /// Layer-3 HA: two instances can both have local `mode_active = true` during
    /// failover, but only the one whose held epoch matches the durable epoch may
    /// pass the actuator authority assertion. The old primary self-demotes before
    /// a command reaches the actuator response boundary.
    #[tokio::test]
    async fn ha_l3_two_writer_window_closed_on_actuator_path() {
        let path = temp_db_path("two-writer");
        let path_str = path.to_str().expect("utf8 path").to_string();

        let svc_old = service_from_store(VerifierStore::new(&path_str).expect("old store"));
        let old_epoch = claim_epoch(&svc_old, 0, "old-primary", 1_000);
        assert_eq!(old_epoch, 1);

        let svc_new = service_from_store(VerifierStore::new(&path_str).expect("new store"));
        let new_epoch = claim_epoch(&svc_new, 1, "new-primary", 2_000);
        assert_eq!(new_epoch, 2);

        assert!(svc_old.app.is_active(), "old primary still thinks it is Active");
        assert!(svc_new.app.is_active(), "new primary is Active");

        assert!(
            assert_actuator_epoch_or_demote(&svc_new, "POST", "/actuator/motion/command")
                .await
                .is_ok(),
            "the current epoch holder must be able to issue actuator commands"
        );
        assert!(
            assert_actuator_epoch_or_demote(&svc_old, "POST", "/actuator/motion/command")
                .await
                .is_err(),
            "the superseded old primary must be fenced on the actuator path"
        );
        assert!(
            !svc_old.app.is_active(),
            "fenced old primary must self-demote to stop issuing actuator commands"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }

    /// Layer-3 HA disk-wedge: if the Active primary cannot read the durable epoch,
    /// actuator authority is ambiguous. The gate denies and flips mode_active off.
    #[tokio::test]
    async fn ha_l3_disk_wedge_epoch_unreadable_self_demotes() {
        let svc = service_from_store(VerifierStore::new(":memory:").expect("store"));
        let epoch = claim_epoch(&svc, 0, "primary", 1_000);
        assert_eq!(epoch, 1);
        svc.app
            .store
            .with(|store| store.delete_ha_state_for_test());

        let res =
            assert_actuator_epoch_or_demote(&svc, "POST", "/actuator/motion/command").await;
        assert!(res.is_err(), "unreadable epoch must deny actuator command");
        assert!(
            !svc.app.is_active(),
            "unreadable epoch must self-demote the primary (fail-closed disk-wedge behavior)"
        );
    }

    /// Layer-3 HA: if the durable epoch changes after a node acquired its role but
    /// before an actuator command reaches the write boundary, the assertion
    /// rejects and the node transitions to safe/passive state.
    #[tokio::test]
    async fn ha_l3_epoch_change_during_actuator_write_is_rejected() {
        let svc = service_from_store(VerifierStore::new(":memory:").expect("store"));
        let held = claim_epoch(&svc, 0, "primary", 1_000);
        assert_eq!(held, 1);
        let advanced = svc
            .app
            .store
            .with(|store| store.try_claim_epoch(held, "standby", 2_000))
            .expect("advance sql")
            .expect("standby advances epoch");
        assert_eq!(advanced, 2);
        assert_eq!(
            svc.app.held_epoch.load(Ordering::SeqCst),
            1,
            "local held epoch remains stale until the actuator assertion fences it"
        );

        let res =
            assert_actuator_epoch_or_demote(&svc, "POST", "/actuator/motion/command").await;
        assert!(res.is_err(), "stale held epoch must reject actuator write");
        assert!(
            !svc.app.is_active(),
            "epoch-change fence must self-demote the stale writer"
        );
    }

    /// Pin the posture-exemption set in BOTH directions (#306). The failure modes
    /// it guards are asymmetric and both bad: silently GAINING an exemption
    /// un-gates a real path; silently LOSING the `/console` exemption locks the
    /// operator out of the recovery affordance exactly when the fleet is
    /// LockedOut — the worst regression this file can have. A refactor of
    /// `is_posture_exempt` must keep this green.
    #[test]
    fn console_exemption_set_is_pinned() {
        // EXEMPT: liveness/observability + the whole /console plane (reads AND the
        // supervisor-gated grant — the grant's gate is the key, not posture).
        for p in [
            "/health", "/health/live", "/ready", "/metrics",
            "/console", "/console/fleet", "/console/audit",
            "/console/escalations", "/console/clearance-grants",
        ] {
            assert!(is_posture_exempt(p), "{p} MUST be posture-exempt");
        }
        // NOT EXEMPT: prefix-confusion guard — a near-miss must not ride in on a
        // loose prefix, and a normal gated path stays gated.
        for p in [
            "/consoleX", "/console-x", "/consol", "/con",
            "/fleet/posture", "/attestation/register", "/",
        ] {
            assert!(!is_posture_exempt(p), "{p} must NOT be posture-exempt");
        }
    }

    /// The body cap must comfortably exceed a serialized worst-case
    /// ProposedVehicleCommand so the cap can never reject a legitimate
    /// vehicle command. f64::MAX serializes to ~25 chars apiece, so a
    /// command with every field at f64::MAX is the realistic upper bound
    /// — even then the wire payload is < 1 KiB. 16 KiB is generous
    /// headroom; this test fails loudly if the cap is ever lowered
    /// past the actual size of the command type.
    #[test]
    fn test_max_vehicle_command_bytes_cap_fits_worst_case_command() {
        let worst = ProposedVehicleCommand {
            linear_velocity_mps:        f64::MAX,
            current_velocity_mps:       f64::MAX,
            delta_time_s:               f64::MAX,
            steering_angle_deg:         f64::MAX,
            current_steering_angle_deg: f64::MAX,
        };
        let json = serde_json::to_vec(&worst).expect("serialize");
        assert!(json.len() < MAX_VEHICLE_COMMAND_BYTES,
            "worst-case ProposedVehicleCommand serializes to {} bytes — must fit \
             under MAX_VEHICLE_COMMAND_BYTES ({} bytes)",
            json.len(), MAX_VEHICLE_COMMAND_BYTES);
        // And the headroom must be substantial — a 2× factor over the
        // worst case is the minimum we expect.
        assert!(json.len() * 2 < MAX_VEHICLE_COMMAND_BYTES,
            "cap should be >= 2× worst-case ({} bytes) — got cap = {}",
            json.len(), MAX_VEHICLE_COMMAND_BYTES);
    }

    #[test]
    fn test_nominal_posture_selects_nominal_contract() {
        let contract = match FleetPosture::Nominal {
            FleetPosture::Nominal => VehicleKinematicsContract::nominal_reference_profile(),
            FleetPosture::Degraded => VehicleKinematicsContract::mrc_fallback_profile(),
            FleetPosture::LockedOut => panic!("should not reach LockedOut"),
        };
        assert_eq!(contract.max_speed_mps, 35.0);
        assert_eq!(contract.max_lateral_accel_mps2, 3.5);
    }

    #[test]
    fn test_degraded_posture_selects_mrc_contract() {
        let contract = match FleetPosture::Degraded {
            FleetPosture::Nominal => VehicleKinematicsContract::nominal_reference_profile(),
            FleetPosture::Degraded => VehicleKinematicsContract::mrc_fallback_profile(),
            FleetPosture::LockedOut => panic!("should not reach LockedOut"),
        };
        assert_eq!(contract.max_speed_mps, 5.0);
        assert_eq!(contract.max_lateral_accel_mps2, 1.5);
    }

    #[test]
    fn test_mrc_profile_rejects_nominal_speed_command() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 20.0,
            current_velocity_mps: 19.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &mrc),
            EnforceAction::ClampLinear(5.0)
        );
    }

    #[test]
    fn test_nominal_profile_passes_same_command() {
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 20.0,
            current_velocity_mps: 19.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &nominal), EnforceAction::Allow);
    }

    #[test]
    fn test_deny_breach_fires_for_non_physical_dt() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: -1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(crate::gateway::kinematics_contract::DenyCode::InvalidTimeDelta)
        );
    }

    #[test]
    fn test_highway_speed_high_steering_clamps_under_nominal_and_mrc() {
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 30.0,
            delta_time_s: 1.0,
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };

        match validate_vehicle_command(&cmd, &nominal) {
            EnforceAction::ClampSteering(a) => assert!(a < 20.0 && a > 0.0),
            other => panic!("nominal: expected ClampSteering, got {other:?}"),
        }
        match validate_vehicle_command(&cmd, &mrc) {
            EnforceAction::ClampLinear(v) => assert_eq!(v, 5.0),
            other => panic!("mrc: expected ClampLinear, got {other:?}"),
        }
    }

    // SAFETY: SG7 | REQ: doer-agnostic-verdict | TEST: sg7_doer_agnostic_verdict_byte_identical_across_ingress_paths
    /// SG7 parity test — the Governor's verdict is a pure function of
    /// `(ProposedVehicleCommand, VehicleKinematicsContract)`. Identical command
    /// bytes MUST produce identical verdicts regardless of which ingress path
    /// (planner vs teleoperator vs any future source) delivered the command.
    /// This is enforced structurally because `validate_vehicle_command` has NO
    /// `source` / `command_source` parameter — the only inputs are the command
    /// and the contract. If a future change introduces a source-typed
    /// parameter that would make the verdict source-dependent, this test
    /// either (a) still passes (parity preserved — fine) or (b) breaks here
    /// or fails to compile (regression caught LOUD).
    ///
    /// Same property holds for `classify_http_command(method, path)` — it
    /// takes no source field, so an identical (method, path) tuple from any
    /// ingress yields the same OperationalCommand.
    #[test]
    fn sg7_doer_agnostic_verdict_byte_identical_across_ingress_paths() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();

        // Two construction paths that mimic "planner submission" and
        // "teleoperator submission" framings: the actual ProposedVehicleCommand
        // values are byte-identical, which is exactly the doer-agnostic
        // contract.
        let cmd_from_planner = ProposedVehicleCommand {
            linear_velocity_mps: 12.5,
            current_velocity_mps: 10.0,
            delta_time_s: 0.05,
            steering_angle_deg: 8.0,
            current_steering_angle_deg: 0.0,
        };
        let cmd_from_teleop = ProposedVehicleCommand {
            linear_velocity_mps: 12.5,
            current_velocity_mps: 10.0,
            delta_time_s: 0.05,
            steering_angle_deg: 8.0,
            current_steering_angle_deg: 0.0,
        };

        let planner_verdict = validate_vehicle_command(&cmd_from_planner, &contract);
        let teleop_verdict = validate_vehicle_command(&cmd_from_teleop, &contract);

        assert_eq!(
            planner_verdict, teleop_verdict,
            "SG7 doer-agnostic property violated: identical command bytes \
             produced different verdicts — the Governor must NOT make the \
             verdict depend on which ingress path delivered the command"
        );

        // Cross-check the same property on classify_http_command: identical
        // method+path from any ingress must yield identical OperationalCommand.
        use crate::gateway::policy::classify_http_command;
        let planner_cmd = classify_http_command("POST", "/actuator/vehicle");
        let teleop_cmd = classify_http_command("POST", "/actuator/vehicle");
        assert_eq!(
            planner_cmd, teleop_cmd,
            "SG7 doer-agnostic property violated on classify_http_command"
        );

        // And an oversize/unsafe command behaves identically too — i.e., the
        // SOURCE doesn't relax the check, including for a clearly-bad input.
        let unsafe_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 100.0, // far above any contract ceiling
            current_velocity_mps: 10.0,
            delta_time_s: 0.05,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 0.0,
        };
        let planner_unsafe = validate_vehicle_command(&unsafe_cmd, &contract);
        let teleop_unsafe = validate_vehicle_command(&unsafe_cmd, &contract);
        assert_eq!(
            planner_unsafe, teleop_unsafe,
            "SG7 source-based relaxation: an unsafe command verdict must NOT depend on ingress path"
        );
        assert!(
            matches!(planner_unsafe, EnforceAction::ClampLinear(_)),
            "expected ClampLinear for over-cap unsafe input, got {planner_unsafe:?}"
        );
    }
}
