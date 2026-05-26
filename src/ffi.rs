use std::sync::{Mutex, LazyLock};
use crate::kirra_core::KirraKernelGovernor;
use crate::kinematics_contract::KinematicContract;
use crate::{SafetyGovernor, SafetyContract};

static GLOBAL_GOVERNOR: LazyLock<Mutex<KirraKernelGovernor<KinematicContract>>> = LazyLock::new(|| {
    let contract = KinematicContract {
        max_linear_velocity: 2.0, max_angular_velocity: 1.0,
        max_linear_acceleration: 10.0, fallback_linear_speed: 0.0,
    };
    Mutex::new(KirraKernelGovernor::new(contract, 0.0, -2.0, 2.0))
});

#[no_mangle]
pub extern "C" fn kirra_filter_move_velocity(proposed_velocity: f64, dt: f64) -> f64 {
    GLOBAL_GOVERNOR.lock().map(|mut g| g.evaluate(proposed_velocity, dt).sanitized_scalar).unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn kirra_filter_rotate_velocity(proposed_angular: f64, _dt: f64) -> f64 {
    if let Ok(mut g) = GLOBAL_GOVERNOR.lock() {
        let max = g.contract.max_angular_rate();
        if proposed_angular.abs() > max {
            g.trust_engine.decay_trust(30);
            proposed_angular.clamp(-max, max)
        } else {
            g.trust_engine.register_safe_tick();
            proposed_angular
        }
    } else { 0.0 }
}

#[no_mangle]
pub extern "C" fn kirra_get_trust_score() -> u32 {
    GLOBAL_GOVERNOR.lock().map(|g| g.trust_engine.current_score).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn kirra_reset_state(token_ptr: *const u8, token_len: usize) -> i32 {
    if token_ptr.is_null() || token_len == 0 || token_len > 64 { return 0; }
    let key = match std::env::var("KIRRA_SUPERVISOR_RESET_KEY") {
        Ok(v) if !v.is_empty() => v.into_bytes(),
        _ => return 0,
    };
    let token = unsafe { std::slice::from_raw_parts(token_ptr, token_len) };
    if let Ok(mut g) = GLOBAL_GOVERNOR.lock() {
        g.trust_engine.authenticated_manual_reset(token, &key, 0).map(|_| 1).unwrap_or(0)
    } else { 0 }
}
