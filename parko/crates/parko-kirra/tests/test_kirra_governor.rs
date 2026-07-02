use parko_kirra::KirraGovernor;
use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};

#[test]
fn nominal_governor_allows_low_velocity_command() {
    // Envelope-tier test; RSS is out of scope → declare external gating
    // (the fail-closed unfed default would HOLD at zero — see RssFeed).
    let governor = KirraGovernor::nominal().with_external_rss_gate();
    let cmd = ControlCommand {
        linear_velocity: 0.1,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    };
    let action = governor.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal);
    match action {
        EnforcementAction::Allow => {}
        other => panic!("expected Allow for low velocity, got {:?}", other),
    }
}

#[test]
fn nominal_governor_clamps_excessive_velocity_command() {
    let governor = KirraGovernor::nominal();

    // 65.0 m/s is intentionally absurd (140 mph). Whatever
    // nominal_reference_profile's max_linear_velocity is, it will be less
    // than 65.0, so we expect the governor to clamp.
    let cmd = ControlCommand {
        linear_velocity: 65.0,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    };
    let action = governor.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal);

    match action {
        EnforcementAction::ClampLinearVelocity(clamped) => {
            // Property assertion: the clamped value must be strictly less
            // than the requested value and must be finite and non-negative.
            assert!(
                clamped < 65.0,
                "expected clamped value < 65.0, got {}",
                clamped
            );
            assert!(clamped.is_finite(), "expected finite clamp value");
            assert!(clamped >= 0.0, "expected non-negative clamp value");
        }
        EnforcementAction::Deny { reason } => {
            // A Deny is also an acceptable response to an absurd input;
            // the safety property (do not pass through 65.0 m/s) is upheld.
            println!("Governor denied 65.0 m/s with reason: {}", reason);
        }
        other => panic!(
            "expected ClampLinearVelocity or Deny for excessive velocity, got {:?}",
            other
        ),
    }
}

#[test]
fn mrc_fallback_governor_constructs_and_evaluates() {
    let governor = KirraGovernor::mrc_fallback();
    let cmd = ControlCommand {
        linear_velocity: 0.1,
        angular_velocity: 0.0,
        timestamp_ms: 0,
    };
    let action = governor.evaluate(&cmd, None, 0.05, SafetyPosture::Nominal);
    // We don't assert what MRC does at 0.1 m/s; just that it doesn't panic
    // and returns *some* valid EnforcementAction.
    let _ = action;
}
