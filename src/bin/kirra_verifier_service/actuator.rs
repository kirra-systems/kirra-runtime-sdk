// src/bin/kirra_verifier_service/actuator.rs
// actuator route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use actuator::*`) lets build_app/tests name them unqualified.

use super::*;
use kirra_verifier::verifier_store::FenceError;

pub(crate) async fn handle_actuator_motion_command(
    State(svc): State<Arc<ServiceState>>,
    // Threaded by `enforce_actuator_safety_envelope` (the route always runs it
    // first). It carries the TRUE verdict — `cmd` below is already the enforced
    // (post-clamp) command, but only this tells us WHETHER a clamp happened, so
    // the response can report it instead of always claiming "Allow".
    Extension(outcome): Extension<EnforcementOutcome>,
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
    (StatusCode::OK, Json(outcome.response_body())).into_response()
}
