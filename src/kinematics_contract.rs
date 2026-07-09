// src/kinematics_contract.rs

use crate::SafetyContract;

#[derive(Debug, Clone, Copy)]
pub struct KinematicContract {
    pub max_linear_velocity: f64,
    pub max_angular_velocity: f64,
    pub max_linear_acceleration: f64,
    pub fallback_linear_speed: f64,
}

impl SafetyContract for KinematicContract {
    #[inline]
    fn min_bound(&self) -> f64 {
        -self.max_linear_velocity
    }
    #[inline]
    fn max_bound(&self) -> f64 {
        self.max_linear_velocity
    }
    #[inline]
    fn max_angular_rate(&self) -> f64 {
        self.max_angular_velocity
    }
    #[inline]
    fn max_rate(&self) -> f64 {
        self.max_linear_acceleration
    }
    #[inline]
    fn fallback(&self) -> f64 {
        self.fallback_linear_speed
    }
    #[inline]
    fn scale_factor(&self) -> f64 {
        1.0
    }
}
