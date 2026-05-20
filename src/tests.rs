// src/tests.rs
use crate::aegis_core::{AegisUnifiedGovernor, SafetyContractProfile, TrustMode};

fn make_test_profile() -> SafetyContractProfile {
    SafetyContractProfile {
        asset_identifier: 10,
        min_permissible_ceiling: 1000.0,
        max_permissible_ceiling: 3000.0,
        max_rate_of_change_dt: 100.0,
        fallback_safe_setpoint: 1200.0,
        constraint_cap_min: 1100.0,
        constraint_cap_max: 2000.0,
        engineering_scale_factor: 10.0,
    }
}

#[test]
fn test_unrestricted_autonomy_envelope_limit_clamping() {
    let profile = make_test_profile();
    let mut governor = AegisUnifiedGovernor::new(profile, 1500.0);
    let result = governor.evaluate_demand_scalar(4500.0, 1.0);
    assert_eq!(
        result.sanitized_scalar, 3000.0,
        "Demand 4500.0 must be clamped to max_permissible_ceiling 3000.0"
    );
    assert_eq!(
        governor.trust_evaluator.current_score, 70,
        "Envelope violation should penalize trust score by 30 (100 -> 70)"
    );
}

#[test]
fn test_moving_target_rate_abuse_lockout_trajectory() {
    let profile = make_test_profile();
    let mut governor = AegisUnifiedGovernor::new(profile, 1500.0);
    // 5 iterations accumulate continuous_rate_breach_ticks to 5 — penalty threshold not yet crossed
    for _ in 0..5 {
        governor.evaluate_demand_scalar(2500.0, 0.001);
    }
    assert_eq!(
        governor.trust_evaluator.current_score, 100,
        "Score unchanged after 5 rate breaches (threshold not exceeded)"
    );
    // 6th breach tips ticks above 5, triggering a 15-point trust penalty
    governor.evaluate_demand_scalar(2500.0, 0.001);
    assert_eq!(
        governor.trust_evaluator.current_score, 85,
        "Score should drop to 85 after 6th rate breach (penalty: 15)"
    );
    assert_eq!(
        governor.trust_evaluator.mode,
        TrustMode::ConstrainedAdvisory,
        "Mode should be ConstrainedAdvisory at score 85"
    );
}
