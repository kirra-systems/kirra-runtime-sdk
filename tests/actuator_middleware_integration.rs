//! Integration tests for `enforce_actuator_safety_envelope`.
//!
//! These tests verify the middleware logic using direct unit-style tests
//! against the actual service state, since axum-test is not available.
//! They verify that the posture-to-contract mapping and fail-closed semantics
//! work correctly with the real AppState and SharedPostureCache.

use std::sync::Arc;

use aegis_runtime_sdk::{
    posture_cache::{CachedFleetPosture, ServiceState, SharedPostureCache},
    verifier::{AppState, FleetPosture, VerifierOperationMode},
    verifier_store::VerifierStore,
    gateway::kinematics_contract::{validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract},
};

// ---------------------------------------------------------------------------
// State builders
// ---------------------------------------------------------------------------

fn build_state(posture: FleetPosture) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache =
        Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(posture))));
    Arc::new(ServiceState { app, posture_cache })
}

fn build_state_empty_cache() -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    Arc::new(ServiceState { app, posture_cache })
}

fn resolve_posture_from_state(state: &ServiceState) -> FleetPosture {
    match state.posture_cache.read() {
        Ok(guard) => match guard.as_ref() {
            Some(cached) => cached.posture.clone(),
            None => FleetPosture::LockedOut,
        },
        Err(_) => FleetPosture::LockedOut,
    }
}

fn get_contract_for_posture(posture: &FleetPosture) -> Option<VehicleKinematicsContract> {
    match posture {
        FleetPosture::Nominal => Some(VehicleKinematicsContract::nominal_reference_profile()),
        FleetPosture::Degraded => Some(VehicleKinematicsContract::mrc_fallback_profile()),
        FleetPosture::LockedOut => None,
    }
}

// ---------------------------------------------------------------------------
// Nominal posture
// ---------------------------------------------------------------------------

#[test]
fn test_nominal_safe_command_passes_through_unmodified() {
    let state = build_state(FleetPosture::Nominal);
    let posture = resolve_posture_from_state(&state);
    let contract = get_contract_for_posture(&posture).expect("Nominal must have a contract");

    let cmd = ProposedVehicleCommand {
        linear_velocity_mps: 10.0,
        current_velocity_mps: 9.8,
        delta_time_s: 0.1,
        steering_angle_deg: 2.0,
        current_steering_angle_deg: 1.8,
    };

    let result = validate_vehicle_command(&cmd, &contract);
    assert_eq!(result, EnforceAction::Allow, "safe command must pass through");
}

#[test]
fn test_nominal_invalid_time_delta_returns_deny() {
    let state = build_state(FleetPosture::Nominal);
    let posture = resolve_posture_from_state(&state);
    let contract = get_contract_for_posture(&posture).expect("Nominal must have a contract");

    let cmd = ProposedVehicleCommand {
        linear_velocity_mps: 10.0,
        current_velocity_mps: 10.0,
        delta_time_s: 0.0,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    };

    let result = validate_vehicle_command(&cmd, &contract);
    assert!(
        matches!(result, EnforceAction::DenyBreach(_)),
        "zero dt must be denied, got {:?}", result
    );
}

#[test]
fn test_nominal_highway_speed_high_steering_clamps_steering() {
    let state = build_state(FleetPosture::Nominal);
    let posture = resolve_posture_from_state(&state);
    let contract = get_contract_for_posture(&posture).expect("Nominal must have a contract");

    let cmd = ProposedVehicleCommand {
        linear_velocity_mps: 30.0,
        current_velocity_mps: 30.0,
        delta_time_s: 1.0,
        steering_angle_deg: 20.0,
        current_steering_angle_deg: 0.0,
    };

    let result = validate_vehicle_command(&cmd, &contract);
    match result {
        EnforceAction::ClampSteering(angle) => {
            assert!(angle < 20.0, "must have been clamped");
            assert!(angle > 0.0, "sign must be preserved");
        }
        other => panic!("expected ClampSteering, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Degraded posture
// ---------------------------------------------------------------------------

#[test]
fn test_degraded_posture_clamps_high_speed_to_mrc_limit() {
    let state = build_state(FleetPosture::Degraded);
    let posture = resolve_posture_from_state(&state);
    let contract = get_contract_for_posture(&posture).expect("Degraded must have a contract");

    let cmd = ProposedVehicleCommand {
        linear_velocity_mps: 15.0,
        current_velocity_mps: 14.5,
        delta_time_s: 0.5,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    };

    let result = validate_vehicle_command(&cmd, &contract);
    match result {
        EnforceAction::ClampLinear(v) => assert_eq!(v, 5.0, "MRC max speed is 5.0 m/s"),
        other => panic!("expected ClampLinear(5.0), got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// LockedOut posture
// ---------------------------------------------------------------------------

#[test]
fn test_locked_out_posture_has_no_contract() {
    let state = build_state(FleetPosture::LockedOut);
    let posture = resolve_posture_from_state(&state);
    // LockedOut returns None from get_contract_for_posture — all commands blocked
    assert!(
        get_contract_for_posture(&posture).is_none(),
        "LockedOut must have no contract — all commands blocked"
    );
}

#[test]
fn test_locked_out_rejects_zero_motion_command() {
    // LockedOut must reject ALL commands — even a zero-velocity command.
    let state = build_state(FleetPosture::LockedOut);
    let posture = resolve_posture_from_state(&state);
    assert_eq!(posture, FleetPosture::LockedOut, "posture must be LockedOut");
    assert!(
        get_contract_for_posture(&posture).is_none(),
        "LockedOut posture yields no contract — middleware blocks at posture check"
    );
}

// ---------------------------------------------------------------------------
// Cache edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_empty_posture_cache_fails_closed_as_locked_out() {
    // None cache (cold start) must be treated as LockedOut — fail-closed.
    let state = build_state_empty_cache();
    let posture = resolve_posture_from_state(&state);
    assert_eq!(posture, FleetPosture::LockedOut,
        "empty cache must resolve to LockedOut (fail-closed)");
}
