// src/bin/kirra_verifier_service/actuator.rs
// actuator route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use actuator::*`) lets build_app/tests name them unqualified.

use super::*;
use kirra_verifier::gateway::contract_profiles::{contract_for, global_vehicle_class};
use kirra_verifier::governor_release::RosReleaseSigner;
use kirra_verifier::verifier_store::FenceError;

pub(crate) async fn handle_actuator_motion_command(
    State(svc): State<Arc<ServiceState>>,
    // Threaded by `enforce_actuator_safety_envelope` (the route always runs it
    // first). It carries the TRUE verdict — `cmd` below is already the enforced
    // (post-clamp) command, but only this tells us WHETHER a clamp happened, so
    // the response can report it instead of always claiming "Allow".
    Extension(outcome): Extension<EnforcementOutcome>,
    // ADR-0033 — the ROS-path release-token signer, layered onto the actuator
    // router at startup ONLY when `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` is
    // configured (fail-closed provisioning). Absent → the 200 response carries
    // no token (byte-identical legacy body) and a verifying consumer downstream
    // refuses everything into its safe stop — fail-closed at the boundary that
    // matters, without breaking non-consumer deployments.
    signer: Option<Extension<Arc<RosReleaseSigner>>>,
    Json(cmd): Json<ProposedVehicleCommand>,
) -> impl IntoResponse {
    let now = now_ms();

    tracing::info!(
        action              = %outcome.action.as_str(),
        linear_velocity_mps = %cmd.linear_velocity_mps,
        steering_angle_deg  = %cmd.steering_angle_deg,
        delta_time_s        = %cmd.delta_time_s,
        "Actuator motion command admitted through safety envelope"
    );

    // Record the verdict in the tamper-evident log, including whether a clamp
    // occurred and the original vs enforced values (previously a clamp was
    // logged indistinguishably from a plain admit).
    let audit = serde_json::json!({
        "action":                       outcome.action.as_str(),
        "original_linear_velocity_mps": outcome.original_linear_velocity_mps,
        "original_steering_angle_deg":  outcome.original_steering_angle_deg,
        "enforced_linear_velocity_mps": outcome.enforced_linear_velocity_mps,
        "enforced_steering_angle_deg":  outcome.enforced_steering_angle_deg,
        "current_velocity_mps":         cmd.current_velocity_mps,
        "current_steering_angle_deg":   cmd.current_steering_angle_deg,
        "delta_time_s":                 cmd.delta_time_s,
        "admitted_at_ms":               now,
    });
    // P1: audit write off the worker pool (materialize the payload first).
    //
    // Layer-3 HA final authority check: the outer posture/policy gate asserted
    // epoch ownership before body parsing, but the actuator response is the
    // command-release boundary. The blocking closure below re-asserts epoch
    // ownership immediately before writing the "admitted" audit event. If the
    // epoch changed during envelope/body work, it returns a fence error, writes
    // no admitted event, and the handler rejects without releasing a command.
    // SAFETY: SG-009 / HA-L3 / REQ-HA-ACTUATOR-EPOCH-FENCE.
    let audit_str = audit.to_string();
    let held = svc.app.held_epoch.load(std::sync::atomic::Ordering::SeqCst);
    let final_epoch_assertion = svc.app.store.call(move |store| {
        store.assert_actuator_epoch_held(held)?;
        if let Err(e) = store.save_posture_event_chained(
            "actuator_motion", "MOTION_COMMAND_ADMITTED",
            &audit_str, None, now,
        ) {
            tracing::error!(error=%e,
                "AUDIT-CHAIN WRITE FAILED for MOTION_COMMAND_ADMITTED — event missing from tamper-evident log");
        }
        Ok::<(), FenceError>(())
    }).await;

    match final_epoch_assertion {
        Ok(Ok(())) => {}
        Ok(Err(FenceError::EpochSuperseded { held, durable })) => {
            svc.app
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                held = held,
                durable = durable,
                ha_req = "REQ-HA-ACTUATOR-EPOCH-FENCE",
                "FENCED — final actuator epoch assertion failed; self-demoting and rejecting command"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Ok(Err(FenceError::EpochUnreadable)) | Err(_) => {
            svc.app
                .mode_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::error!(
                held = held,
                ha_req = "REQ-HA-DISK-WEDGE-DEMOTE",
                "DISK-WEDGE — final actuator epoch unreadable; self-demoting and rejecting command"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }

    // Response speaks the ROS interceptor's schema (action / enforced_*) AND
    // the legacy keys (now accurate). See `EnforcementOutcome::response_body`.
    let mut body = outcome.response_body();

    // ADR-0033 — mint the ROS-path release token, 200 arm ONLY, after the
    // epoch fence (a fenced request returns 503 above and never reaches this;
    // the deny paths are middleware 4xx and structurally cannot reach a signer).
    // The token binds the ENFORCED twist: the payload's little-endian image is
    // the wire truth the consumer verifies and decodes — the JSON floats in
    // this body are observability, never the trust path.
    if let Some(Extension(signer)) = signer {
        // The steering→angular mapping is the interceptor's bicycle relation
        // (`cmd_vel_interceptor.py:311`): angular_rad_s = tan(steering_rad) ×
        // |v| / wheelbase, with the wheelbase of the ACTIVE vehicle class —
        // a physical constant of the platform, identical across that class's
        // nominal and MRC profiles.
        let wheelbase_m = contract_for(global_vehicle_class()).wheelbase_m;
        let angular_rad_s = (outcome.enforced_steering_angle_deg.to_radians()).tan()
            * outcome.enforced_linear_velocity_mps.abs()
            / wheelbase_m;
        let (payload, token) =
            signer.mint(outcome.enforced_linear_velocity_mps, angular_rad_s, now);
        body["release"] = serde_json::json!({
            // The signed 32-byte payload image (hex) — the consumer verifies
            // the token over EXACTLY these bytes and decodes its twist FROM
            // them (no cross-language float re-canonicalization).
            "payload_hex": hex::encode(payload.encode()),
            // The canonical 96-byte token (hex): digest(32) || signature(64).
            "token_hex": hex::encode(token.to_bytes()),
            "sequence": payload.sequence,
            "issued_at_ms": payload.issued_at_ms,
            // Forensic key id (hex SHA-256 of the verifying key) — lets a
            // consumer log WHICH key signed without trusting this field.
            "key_id": signer.key_id(),
        });
    }

    (StatusCode::OK, Json(body)).into_response()
}
