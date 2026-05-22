// src/gateway/policy_layer.rs
//
// Actuator safety envelope middleware for Aegis AV flight envelope protection.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::gateway::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};
use crate::verifier::FleetPosture;
// ServiceState is defined in posture_cache — all handlers use State<Arc<ServiceState>>
use crate::posture_cache::ServiceState;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Resolves the current FleetPosture from the SharedPostureCache.
///
/// None (cold start or expired cache) and a poisoned RwLock both map to
/// LockedOut — fail-closed in all ambiguous cases.
fn resolve_posture(svc: &ServiceState) -> FleetPosture {
    match svc.posture_cache.read() {
        Ok(guard) => match guard.as_ref() {
            Some(cached) => cached.propagated_status.clone(),
            None => FleetPosture::LockedOut,
        },
        Err(_) => FleetPosture::LockedOut,
    }
}

/// Actuator command safety envelope middleware.
///
/// Intercepts inbound actuator motion commands, resolves the active fleet posture,
/// selects the appropriate VehicleKinematicsContract, and enforces all physical
/// invariants before the request reaches any downstream handler.
///
/// Posture → Contract mapping:
///   Nominal   → nominal_reference_profile() — full operational envelope
///   Degraded  → mrc_fallback_profile()      — MRC crawl-speed envelope
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

    let contract: VehicleKinematicsContract = match posture {
        FleetPosture::Nominal => VehicleKinematicsContract::nominal_reference_profile(),
        FleetPosture::Degraded => VehicleKinematicsContract::mrc_fallback_profile(),
        FleetPosture::LockedOut => {
            tracing::error!(
                "Actuator command rejected: fleet posture is LockedOut — \
                 all actuator mutations are blocked until posture recovers"
            );
            return Err(StatusCode::FORBIDDEN);
        }
    };

    let (parts, body) = req.into_parts();

    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let proposed_cmd: ProposedVehicleCommand =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;

    match validate_vehicle_command(&proposed_cmd, &contract) {
        EnforceAction::Allow => {
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
            let rebuilt = Request::from_parts(parts, Body::from(serialized));
            Ok(next.run(rebuilt).await)
        }

        EnforceAction::DenyBreach(ref reason) => {
            tracing::error!(
                reason               = %reason,
                linear_velocity_mps  = %proposed_cmd.linear_velocity_mps,
                steering_angle_deg   = %proposed_cmd.steering_angle_deg,
                delta_time_s         = %proposed_cmd.delta_time_s,
                "Inadmissible actuator command rejected at kinematic safety perimeter"
            );

            let log_payload = serde_json::json!({
                "violation": reason,
                "proposed_command": {
                    "linear_velocity_mps": proposed_cmd.linear_velocity_mps,
                    "current_velocity_mps": proposed_cmd.current_velocity_mps,
                    "delta_time_s": proposed_cmd.delta_time_s,
                    "steering_angle_deg": proposed_cmd.steering_angle_deg,
                    "current_steering_angle_deg": proposed_cmd.current_steering_angle_deg,
                },
                "posture_at_rejection": format!("{posture:?}"),
            });

            // Disk-first (invariant #12): store write before memory update.
            // save_posture_event_chained takes &mut self — must lock the Mutex.
            if let Ok(mut store) = svc.app.store.lock() {
                let _ = store.save_posture_event_chained(
                    "actuator_safety_envelope",
                    "KINEMATIC_CONTRACT_VIOLATION",
                    &log_payload.to_string(),
                    Some("Proposed vehicle command violates non-physical invariants"),
                    now_ms(),
                );
            }

            Err(StatusCode::BAD_REQUEST)
        }
    }
}

#[cfg(test)]
mod actuator_middleware_tests {
    use super::*;
    use crate::gateway::kinematics_contract::{ProposedVehicleCommand, VehicleKinematicsContract};
    use crate::verifier::FleetPosture;

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
            EnforceAction::DenyBreach("INVALID_TIME_DELTA".to_string())
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
}
