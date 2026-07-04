// src/kinematics_sim.rs
//
// Kinematic forward simulator for Kirra safety verification.
//
// PURPOSE
// =======
// validate_vehicle_command() verifies that a proposed command is physically
// admissible. But it cannot answer: "if we apply clamped commands for 100
// timesteps at highway speed, does the vehicle stay within safe physical
// bounds throughout?"
//
// This module closes that gap. VehicleState::step() applies a
// ProposedVehicleCommand (post-enforcement) and advances the vehicle's
// position, heading, velocity, and steering angle using the standard
// kinematic bicycle model. No neural networks, no perception — pure
// deterministic physics.
//
// This lets the scenario harness assert physical outcomes, not just
// enforcement decisions:
//
//   let mut state = VehicleState::at_rest();
//   for cmd in command_sequence {
//       let enforced = enforce(cmd, &contract);  // Kirra enforcement
//       state = state.step(&enforced, DT);
//       assert!(state.lateral_accel_mps2(contract.wheelbase_m)
//               <= contract.max_lateral_accel_mps2 + FLOAT_EPSILON);
//   }
//
// BICYCLE MODEL
// =============
// Uses the standard kinematic bicycle model (front-wheel steering,
// rear-wheel drive reference point):
//
//   x'     = v * cos(ψ)
//   y'     = v * sin(ψ)
//   ψ'     = (v / L) * tan(δ)
//   v'     = (v_cmd - v) / dt  (clamped by validate_vehicle_command upstream)
//   δ'     = (δ_cmd - δ) / dt  (clamped upstream)
//
// Where:
//   (x, y)  = position (meters)
//   ψ       = heading (radians, 0 = East, positive = counterclockwise)
//   v       = forward speed (m/s)
//   δ       = front wheel steering angle (radians)
//   L       = wheelbase (meters)
//
// The simulator is intentionally simple — it matches the same bicycle model
// approximation used in validate_vehicle_command's lateral acceleration check.
// More accurate tire models (Pacejka, etc.) are future work.
//
// LIMITATIONS
// ===========
// - No slip angle, no tire saturation, no load transfer
// - Euler integration (first-order) — sufficient for dt ≤ 0.1s
// - No collision geometry, no road boundaries
// - Reverse motion (v < 0) is geometrically correct but untested against
//   real vehicle behavior

use crate::kinematics_contract::{
    EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
    validate_vehicle_command,
};

// ---------------------------------------------------------------------------
// Vehicle state
// ---------------------------------------------------------------------------

/// Complete kinematic state of a vehicle at a point in time.
///
/// All angles in radians internally. Conversion to/from degrees at the
/// boundary with ProposedVehicleCommand (which uses degrees per the contract).
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleState {
    /// X position (meters, East-positive)
    pub x_m: f64,
    /// Y position (meters, North-positive)
    pub y_m: f64,
    /// Heading (radians, 0 = East, positive = counterclockwise / left turn)
    pub heading_rad: f64,
    /// Forward speed (m/s, negative = reversing)
    pub velocity_mps: f64,
    /// Front wheel steering angle (degrees, positive = left turn, ISO 8855)
    /// Stored in degrees to match ProposedVehicleCommand convention.
    pub steering_angle_deg: f64,
}

impl VehicleState {
    /// Creates a vehicle at the origin, facing East, at rest.
    pub fn at_rest() -> Self {
        Self {
            x_m: 0.0,
            y_m: 0.0,
            heading_rad: 0.0,
            velocity_mps: 0.0,
            steering_angle_deg: 0.0,
        }
    }

    /// Creates a vehicle at a specific position with given speed and heading.
    pub fn new(x_m: f64, y_m: f64, heading_deg: f64, velocity_mps: f64) -> Self {
        Self {
            x_m,
            y_m,
            heading_rad: heading_deg.to_radians(),
            velocity_mps,
            steering_angle_deg: 0.0,
        }
    }

    /// Advances the vehicle state by one timestep using Euler integration
    /// of the kinematic bicycle model.
    ///
    /// The command is applied as-is — this is the post-enforcement state.
    /// Callers should pass the result of `apply_enforcement()` rather than
    /// raw planner output to test that Kirra enforcement keeps the vehicle
    /// within physical bounds.
    ///
    /// # Arguments
    /// - `cmd`: The command to apply. Must have `delta_time_s > 0`.
    /// - `wheelbase_m`: Vehicle wheelbase from `VehicleKinematicsContract`.
    ///
    /// # Returns
    /// The new vehicle state after `cmd.delta_time_s` seconds.
    ///
    /// # Panics
    /// Does not panic. Returns `self` unchanged if `delta_time_s <= 0`
    /// (mirrors the Priority 1 check in `validate_vehicle_command`).
    pub fn step(&self, cmd: &ProposedVehicleCommand, wheelbase_m: f64) -> Self {
        if cmd.delta_time_s <= 0.0 || !cmd.delta_time_s.is_finite() {
            return self.clone();
        }

        let dt = cmd.delta_time_s;
        let v  = self.velocity_mps;
        let psi = self.heading_rad;
        let delta_rad = self.steering_angle_deg.to_radians();

        // Bicycle model kinematics (Euler integration)
        let new_x = self.x_m + v * psi.cos() * dt;
        let new_y = self.y_m + v * psi.sin() * dt;

        // Heading update: ψ' = (v / L) * tan(δ)
        let heading_rate = if wheelbase_m > 1e-6 {
            (v / wheelbase_m) * delta_rad.tan()
        } else {
            0.0
        };
        let new_heading = psi + heading_rate * dt;

        // Velocity and steering: instantaneously set to commanded values.
        // The ramp rate was already enforced by validate_vehicle_command upstream.
        let new_velocity = cmd.linear_velocity_mps;
        let new_steering = cmd.steering_angle_deg;

        Self {
            x_m: new_x,
            y_m: new_y,
            heading_rad: new_heading,
            velocity_mps: new_velocity,
            steering_angle_deg: new_steering,
        }
    }

    /// Computes the implied lateral acceleration at the current kinematic state
    /// using the bicycle model approximation:
    ///
    ///   a_lat = (v² × |tan(δ)|) / L
    ///
    /// This is the same formula used in `validate_vehicle_command` priority 6.
    /// At near-zero speed, returns 0.0 (matches the `v2 > 1e-6` guard there).
    pub fn lateral_accel_mps2(&self, wheelbase_m: f64) -> f64 {
        let v2 = self.velocity_mps.powi(2);
        if v2 <= 1e-6 || wheelbase_m <= 1e-6 {
            return 0.0;
        }
        let delta_rad = self.steering_angle_deg.to_radians();
        (v2 * delta_rad.tan().abs()) / wheelbase_m
    }

    /// Returns the total distance traveled from the origin.
    pub fn distance_from_origin(&self) -> f64 {
        (self.x_m.powi(2) + self.y_m.powi(2)).sqrt()
    }

    /// Returns the heading in degrees (0 = East, positive = counterclockwise).
    pub fn heading_deg(&self) -> f64 {
        self.heading_rad.to_degrees()
    }
}

// ---------------------------------------------------------------------------
// Enforcement application
// ---------------------------------------------------------------------------

/// Applies Kirra enforcement to a proposed command and returns the
/// post-enforcement command ready for simulation.
///
/// - `Allow`        → command passes through unchanged
/// - `ClampLinear`  → linear velocity replaced with clamped value
/// - `ClampSteering`→ steering angle replaced with clamped value
/// - `DenyBreach`   → returns None (command dropped; caller handles safe stop)
///
/// This is the bridge between the enforcement layer and the simulator.
/// Use this in simulation loops rather than calling `validate_vehicle_command`
/// manually to avoid duplicating the clamp application logic.
pub fn apply_enforcement(
    cmd: &ProposedVehicleCommand,
    contract: &VehicleKinematicsContract,
) -> Option<ProposedVehicleCommand> {
    apply_enforce_action(cmd, &validate_vehicle_command(cmd, contract))
}

/// Apply an ALREADY-COMPUTED [`EnforceAction`] to a command, producing the
/// enforced (post-clamp) command, or `None` on `DenyBreach`.
///
/// This is the single clamp-application implementation (the contract-driven
/// [`apply_enforcement`] delegates to it). Callers that already hold a verdict
/// — e.g. the fabric command handler, which gets an `EnforceAction` from
/// `FabricRouter::route_command` and must apply the SAME verdict rather than
/// re-deriving one against a different contract — use this directly so the
/// returned command carries the SAFE values and is within envelope even if the
/// caller ignores the action label. Lives here (NOT in `kinematics_contract`,
/// which only COMPUTES the verdict) so the verdict computation stays untouched.
pub fn apply_enforce_action(
    cmd: &ProposedVehicleCommand,
    action: &EnforceAction,
) -> Option<ProposedVehicleCommand> {
    match action {
        EnforceAction::Allow => Some(cmd.clone()),

        EnforceAction::ClampLinear(safe_v) => Some(ProposedVehicleCommand {
            linear_velocity_mps: *safe_v,
            ..cmd.clone()
        }),

        EnforceAction::ClampSteering(safe_delta) => Some(ProposedVehicleCommand {
            steering_angle_deg: *safe_delta,
            ..cmd.clone()
        }),

        EnforceAction::ClampBoth { linear, steering } => Some(ProposedVehicleCommand {
            linear_velocity_mps: *linear,
            steering_angle_deg: *steering,
            ..cmd.clone()
        }),

        EnforceAction::DenyBreach(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Simulation runner
// ---------------------------------------------------------------------------

/// Result of a multi-step simulation run.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Final vehicle state after all steps
    pub final_state: VehicleState,
    /// Maximum lateral acceleration observed across all steps (m/s²)
    pub peak_lateral_accel_mps2: f64,
    /// Maximum speed observed (m/s)
    pub peak_speed_mps: f64,
    /// Number of steps where the command was clamped (not passed through as-is)
    pub clamp_count: usize,
    /// Number of steps where the command was denied (DenyBreach)
    pub deny_count: usize,
    /// Total steps executed
    pub step_count: usize,
    /// Whether any physical invariant was violated post-enforcement.
    /// True means the enforced command still produced out-of-contract physics —
    /// which would indicate a bug in validate_vehicle_command.
    pub invariant_violated: bool,
    /// Description of the first invariant violation, if any.
    pub violation_description: Option<String>,
}

/// Runs a sequence of proposed commands through Kirra enforcement and the
/// kinematic forward simulator.
///
/// For each command:
///   1. Applies Kirra enforcement (`validate_vehicle_command`)
///   2. Steps the vehicle state forward using the bicycle model
///   3. Checks physical invariants against the contract
///   4. Records statistics
///
/// On `DenyBreach`, the vehicle state is held at the last valid state
/// (safe-stop behavior — no position change for that timestep).
///
/// Returns a `SimulationResult` with peak values, violation flags, and
/// final state. Panics in tests if `invariant_violated` is true and
/// `panic_on_violation` is set.
pub fn run_simulation(
    initial_state: VehicleState,
    commands: &[ProposedVehicleCommand],
    contract: &VehicleKinematicsContract,
    panic_on_violation: bool,
) -> SimulationResult {
    let mut state = initial_state;
    let mut peak_lat = 0.0_f64;
    let mut peak_speed = 0.0_f64;
    let mut clamp_count = 0;
    let mut deny_count = 0;
    let mut invariant_violated = false;
    let mut violation_description: Option<String> = None;

    let wb = contract.wheelbase_m;

    for (i, cmd) in commands.iter().enumerate() {
        // Update current-state fields to match simulator state before enforcement.
        let cmd_with_current = ProposedVehicleCommand {
            current_velocity_mps: state.velocity_mps,
            current_steering_angle_deg: state.steering_angle_deg,
            ..cmd.clone()
        };

        match validate_vehicle_command(&cmd_with_current, contract) {
            EnforceAction::Allow => {
                state = state.step(&cmd_with_current, wb);
            }
            EnforceAction::ClampLinear(safe_v) => {
                clamp_count += 1;
                let clamped = ProposedVehicleCommand {
                    linear_velocity_mps: safe_v,
                    ..cmd_with_current.clone()
                };
                state = state.step(&clamped, wb);
            }
            EnforceAction::ClampSteering(safe_delta) => {
                clamp_count += 1;
                let clamped = ProposedVehicleCommand {
                    steering_angle_deg: safe_delta,
                    ..cmd_with_current.clone()
                };
                state = state.step(&clamped, wb);
            }
            EnforceAction::ClampBoth { linear, steering } => {
                clamp_count += 1;
                let clamped = ProposedVehicleCommand {
                    linear_velocity_mps: linear,
                    steering_angle_deg: steering,
                    ..cmd_with_current.clone()
                };
                state = state.step(&clamped, wb);
            }
            EnforceAction::DenyBreach(reason) => {
                deny_count += 1;
                tracing::debug!(step = i, reason = %reason, "Command denied — holding position");
            }
        }

        let lat_accel = state.lateral_accel_mps2(wb);
        let speed = state.velocity_mps.abs();

        peak_lat = peak_lat.max(lat_accel);
        peak_speed = peak_speed.max(speed);

        const FLOAT_TOLERANCE: f64 = 1e-6;
        if lat_accel > contract.max_lateral_accel_mps2 + FLOAT_TOLERANCE {
            let desc = format!(
                "Step {i}: lateral_accel {:.6} > contract max {:.6} (excess: {:.6})",
                lat_accel,
                contract.max_lateral_accel_mps2,
                lat_accel - contract.max_lateral_accel_mps2
            );
            if !invariant_violated {
                violation_description = Some(desc.clone());
            }
            invariant_violated = true;
            if panic_on_violation {
                panic!("INVARIANT VIOLATION: {desc}");
            }
        }

        if speed > contract.max_speed_mps + FLOAT_TOLERANCE {
            let desc = format!(
                "Step {i}: speed {:.6} > contract max {:.6}",
                speed, contract.max_speed_mps
            );
            if !invariant_violated {
                violation_description = Some(desc.clone());
            }
            invariant_violated = true;
            if panic_on_violation {
                panic!("INVARIANT VIOLATION: {desc}");
            }
        }
    }

    SimulationResult {
        final_state: state,
        peak_lateral_accel_mps2: peak_lat,
        peak_speed_mps: peak_speed,
        clamp_count,
        deny_count,
        step_count: commands.len(),
        invariant_violated,
        violation_description,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod kinematics_sim_tests {
    use super::*;
    use crate::kinematics_contract::{
        ProposedVehicleCommand, VehicleKinematicsContract,
    };

    const DT: f64 = 0.1;
    const WB: f64 = 2.8;

    fn cmd(v: f64, delta: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: v,
            delta_time_s: DT,
            steering_angle_deg: delta,
            current_steering_angle_deg: delta,
        }
    }

    #[test]
    fn test_straight_line_motion_advances_x_position() {
        // Pre-populate velocity to match commanded velocity so step() integrates over the
        // active velocity. The one-tick lag in the state model is intentional — see
        // validate_vehicle_command's acceleration contract.
        let state = VehicleState::new(0.0, 0.0, 0.0, 10.0);
        let c = cmd(10.0, 0.0);
        let next = state.step(&c, WB);
        assert!((next.x_m - 1.0).abs() < 1e-9, "x must advance by v*dt");
        assert!(next.y_m.abs() < 1e-9, "y must not change for straight motion");
        assert_eq!(next.velocity_mps, 10.0);
        assert_eq!(next.steering_angle_deg, 0.0);
    }

    #[test]
    fn test_zero_velocity_no_position_change() {
        let state = VehicleState::at_rest();
        let c = cmd(0.0, 5.0);
        let next = state.step(&c, WB);
        assert_eq!(next.x_m, 0.0);
        assert_eq!(next.y_m, 0.0);
        assert_eq!(next.heading_rad, 0.0);
    }

    #[test]
    fn test_left_turn_increases_heading() {
        // Pre-populate both velocity and steering so step() integrates with the active
        // kinematic state. The one-tick lag is intentional — see validate_vehicle_command's
        // acceleration contract.
        let state = VehicleState { steering_angle_deg: 10.0, ..VehicleState::new(0.0, 0.0, 0.0, 10.0) };
        let c = cmd(10.0, 10.0);
        let next = state.step(&c, WB);
        assert!(next.heading_rad > 0.0, "left turn must increase heading");
    }

    #[test]
    fn test_right_turn_decreases_heading() {
        // Pre-populate both velocity and steering so step() integrates with the active
        // kinematic state. The one-tick lag is intentional — see validate_vehicle_command's
        // acceleration contract.
        let state = VehicleState { steering_angle_deg: -10.0, ..VehicleState::new(0.0, 0.0, 0.0, 10.0) };
        let c = cmd(10.0, -10.0);
        let next = state.step(&c, WB);
        assert!(next.heading_rad < 0.0, "right turn must decrease heading");
    }

    #[test]
    fn test_invalid_dt_returns_unchanged_state() {
        let state = VehicleState::at_rest();
        let mut c = cmd(10.0, 0.0);
        c.delta_time_s = 0.0;
        let next = state.step(&c, WB);
        assert_eq!(next, state, "zero dt must return unchanged state");

        c.delta_time_s = -1.0;
        let next2 = state.step(&c, WB);
        assert_eq!(next2, state, "negative dt must return unchanged state");
    }

    #[test]
    fn test_zero_speed_lateral_accel_is_zero() {
        let state = VehicleState {
            velocity_mps: 0.0,
            steering_angle_deg: 30.0,
            ..VehicleState::at_rest()
        };
        assert_eq!(state.lateral_accel_mps2(WB), 0.0);
    }

    #[test]
    fn test_zero_steering_lateral_accel_is_zero() {
        let state = VehicleState {
            velocity_mps: 20.0,
            steering_angle_deg: 0.0,
            ..VehicleState::at_rest()
        };
        assert_eq!(state.lateral_accel_mps2(WB), 0.0);
    }

    #[test]
    fn test_lateral_accel_matches_contract_formula() {
        let v = 10.0_f64;
        let delta_deg = 15.0_f64;
        let state = VehicleState {
            velocity_mps: v,
            steering_angle_deg: delta_deg,
            ..VehicleState::at_rest()
        };
        let expected = (v.powi(2) * delta_deg.to_radians().tan().abs()) / WB;
        let actual = state.lateral_accel_mps2(WB);
        assert!((actual - expected).abs() < 1e-12,
            "lateral_accel_mps2 must match contract formula: expected {expected}, got {actual}");
    }

    #[test]
    fn test_allow_passes_command_through_unchanged() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let c = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.8,
            delta_time_s: DT,
            steering_angle_deg: 2.0,
            current_steering_angle_deg: 1.9,
        };
        let enforced = apply_enforcement(&c, &contract).expect("must not deny");
        assert_eq!(enforced.linear_velocity_mps, 10.0);
        assert_eq!(enforced.steering_angle_deg, 2.0);
    }

    #[test]
    fn test_clamp_linear_applies_safe_velocity() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let c = ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.0,
            delta_time_s: DT,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let enforced = apply_enforcement(&c, &contract).expect("must clamp, not deny");
        assert_eq!(enforced.linear_velocity_mps, 35.0);
        assert_eq!(enforced.steering_angle_deg, 0.0);
    }

    #[test]
    fn test_deny_breach_returns_none() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let c = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: -1.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert!(apply_enforcement(&c, &contract).is_none(), "DenyBreach must return None");
    }

    // --- apply_enforce_action: applying an ALREADY-COMPUTED verdict (#86) ---

    fn sample_cmd() -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.0,
            delta_time_s: DT,
            steering_angle_deg: 7.0,
            current_steering_angle_deg: 6.0,
        }
    }

    #[test]
    fn test_apply_action_allow_is_unchanged() {
        let c = sample_cmd();
        let out = apply_enforce_action(&c, &EnforceAction::Allow).expect("Allow yields a command");
        assert_eq!(out.linear_velocity_mps, c.linear_velocity_mps);
        assert_eq!(out.steering_angle_deg, c.steering_angle_deg);
    }

    #[test]
    fn test_apply_action_clamp_linear_substitutes_safe_velocity() {
        let c = sample_cmd();
        let out = apply_enforce_action(&c, &EnforceAction::ClampLinear(12.5)).expect("clamp yields a command");
        assert_eq!(out.linear_velocity_mps, 12.5, "enforced command carries the SAFE velocity");
        assert!(out.linear_velocity_mps < c.linear_velocity_mps, "within envelope (below the proposal)");
        assert_eq!(out.steering_angle_deg, c.steering_angle_deg, "steering untouched on a linear clamp");
    }

    #[test]
    fn test_apply_action_clamp_steering_substitutes_safe_angle() {
        let c = sample_cmd();
        let out = apply_enforce_action(&c, &EnforceAction::ClampSteering(1.5)).expect("clamp yields a command");
        assert_eq!(out.steering_angle_deg, 1.5, "enforced command carries the SAFE steering");
        assert_eq!(out.linear_velocity_mps, c.linear_velocity_mps, "linear untouched on a steering clamp");
    }

    #[test]
    fn test_apply_action_deny_is_none() {
        let c = sample_cmd();
        assert!(
            apply_enforce_action(&c, &EnforceAction::DenyBreach(crate::kinematics_contract::DenyCode::NanInfLinearVelocity)).is_none(),
            "DenyBreach yields no enforced command (caller fail-closes)"
        );
    }

    #[test]
    fn test_100_steps_at_nominal_speed_straight_never_violates_invariants() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let commands: Vec<_> = (0..100).map(|_| cmd(25.0, 0.0)).collect();
        // Start at cruise speed so the first step doesn't trigger the accel clamp.
        let initial = VehicleState::new(0.0, 0.0, 0.0, 25.0);
        let result = run_simulation(initial, &commands, &contract, true);

        assert!(!result.invariant_violated);
        assert_eq!(result.clamp_count, 0, "straight cruise at 25 m/s must need no clamping");
        assert_eq!(result.deny_count, 0);
        assert!(result.final_state.x_m > 0.0, "must have advanced");
    }

    #[test]
    fn test_highway_speed_max_steering_clamped_to_safe_angle() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let commands: Vec<_> = (0..100).map(|_| cmd(30.0, 20.0)).collect();
        let result = run_simulation(VehicleState::at_rest(), &commands, &contract, true);

        assert!(result.clamp_count > 0, "high-speed high-steering must trigger clamping");
        assert!(!result.invariant_violated,
            "clamped commands must not produce lateral accel violations");
        assert!(result.peak_lateral_accel_mps2 <= contract.max_lateral_accel_mps2 + 1e-6,
            "peak lat accel {:.4} must be <= contract max {:.4}",
            result.peak_lateral_accel_mps2, contract.max_lateral_accel_mps2);
    }

    #[test]
    fn test_mrc_profile_caps_speed_across_100_steps() {
        let contract = VehicleKinematicsContract::mrc_fallback_profile();
        let commands: Vec<_> = (0..100).map(|_| cmd(20.0, 5.0)).collect();
        let result = run_simulation(VehicleState::at_rest(), &commands, &contract, true);

        assert!(!result.invariant_violated);
        assert!(result.peak_speed_mps <= contract.max_speed_mps + 1e-6,
            "MRC must cap speed at {}, got {:.4}",
            contract.max_speed_mps, result.peak_speed_mps);
        assert_eq!(result.clamp_count, 100, "every step must be clamped under MRC");
    }

    #[test]
    fn test_acceleration_from_rest_ramp_stays_within_contract() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let commands: Vec<_> = (0..100).map(|i| {
            let target = (i as f64 * 0.5).min(30.0);
            ProposedVehicleCommand {
                linear_velocity_mps: target,
                current_velocity_mps: ((i as f64 - 1.0) * 0.5).clamp(0.0, 30.0),
                delta_time_s: DT,
                steering_angle_deg: 0.0,
                current_steering_angle_deg: 0.0,
            }
        }).collect();

        let result = run_simulation(VehicleState::at_rest(), &commands, &contract, true);
        assert!(!result.invariant_violated,
            "accelerating ramp must not produce invariant violations after clamping");
    }

    #[test]
    fn test_denied_command_holds_vehicle_position() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let initial = VehicleState::at_rest();

        let nan_cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 0.0,
            delta_time_s: DT,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };

        let result = run_simulation(initial.clone(), &[nan_cmd], &contract, false);
        assert_eq!(result.deny_count, 1);
        assert_eq!(result.final_state.x_m, initial.x_m);
        assert_eq!(result.final_state.y_m, initial.y_m);
    }

    #[test]
    fn test_circular_path_lateral_accel_bounded_by_contract() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let commands: Vec<_> = (0..200).map(|_| cmd(15.0, 15.0)).collect();
        let result = run_simulation(VehicleState::at_rest(), &commands, &contract, true);

        assert!(!result.invariant_violated,
            "sustained circular turn must be clamped to safe angle throughout");
        assert!(result.peak_lateral_accel_mps2 <= contract.max_lateral_accel_mps2 + 1e-6);
    }

    #[test]
    fn test_low_speed_large_steering_allowed_by_bicycle_model() {
        // a_lat = (1^2 * tan(30°)) / 2.8 ≈ 0.206 m/s² — well within 3.5 limit
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let commands: Vec<_> = (0..50).map(|_| cmd(1.0, 30.0)).collect();
        // Pre-populate both velocity and steering to match the commanded state so the
        // first step doesn't trigger the accel or steering-rate clamps.
        let initial = VehicleState { velocity_mps: 1.0, steering_angle_deg: 30.0, ..VehicleState::at_rest() };
        let result = run_simulation(initial, &commands, &contract, true);

        assert!(!result.invariant_violated);
        assert_eq!(result.clamp_count, 0,
            "parking-speed large steering must not be clamped by bicycle model");
    }

    #[test]
    fn test_simulation_result_statistics_are_accurate() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let nan_cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 0.0,
            delta_time_s: DT,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let over_speed_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.9,
            delta_time_s: DT,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let safe_cmd = cmd(10.0, 0.0);

        let result = run_simulation(
            VehicleState::at_rest(),
            &[nan_cmd, over_speed_cmd, safe_cmd],
            &contract,
            false,
        );

        assert_eq!(result.step_count, 3);
        assert_eq!(result.deny_count, 1);
        // The NaN command is denied and leaves the vehicle at rest (v=0). The over-speed
        // command is clamped to 35 m/s by P2, setting state velocity to 35. The "safe"
        // 10 m/s command then arrives with current_v=35 from state, implying 250 m/s²
        // deceleration >> max_brake 4.5 m/s², so it is also clamped by P4. Both clamps
        // are correct enforcement behavior; the counter accurately reflects this.
        assert_eq!(result.clamp_count, 2);
    }
}
