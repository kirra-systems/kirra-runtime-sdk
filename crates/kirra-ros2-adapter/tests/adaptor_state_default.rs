// crates/kirra-ros2-adapter/tests/adaptor_state_default.rs
//
// The one validation_tests.rs pin that could NOT move to `kirra-trajectory`
// with the rest of the slow-loop battery (EP-07 gate follow-through — the
// checker tests now live with the checker code): `AdaptorState` is
// adapter-LOCAL (per the #131 Option-B split, `state.rs` stays here), so its
// construction-default regression stays here too.

use kirra_core::FleetPosture;
use kirra_ros2_adapter::state::AdaptorState;

#[test]
fn nominal_behavior_matches_prior_default() {
    // Regression: every prior test in the slow-loop battery passed Nominal
    // explicitly. This test pins the rule that Nominal is the construction
    // default for `AdaptorState::current_posture` — until M1b wires a
    // live posture source, the slow-loop verdict is byte-for-byte the
    // pre-M1 behaviour.
    let state = AdaptorState::new();
    assert_eq!(state.current_posture(), FleetPosture::Nominal,
        "AdaptorState must default to Nominal so pre-M1 callers see no behaviour change");
}
