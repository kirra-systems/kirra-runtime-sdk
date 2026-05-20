// src/config.rs

use crate::aegis_core::ContractProfile;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkConfig {
    pub proxy_listen_port: u16,
    pub plc_target_port: u16,
    pub admin_reset_port: u16,
    pub max_concurrent_connections: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TelemetryConfig {
    pub log_directory: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AegisRuntimeConfig {
    pub network: NetworkConfig,
    pub telemetry: TelemetryConfig,
    pub contract: ContractProfile,
}

impl AegisRuntimeConfig {
    pub fn validate_safety_invariants(&self) -> Result<(), &'static str> {
        let n = &self.network;
        let c = &self.contract;

        if n.proxy_listen_port == n.plc_target_port || n.proxy_listen_port == n.admin_reset_port || n.plc_target_port == n.admin_reset_port {
            return Err("CONFIG_INVALID: Network ports must be completely distinct loopback channels.");
        }
        if n.max_concurrent_connections == 0 || n.max_concurrent_connections > 128 {
            return Err("CONFIG_INVALID: Thread pool limits must fall within the range [1, 128].");
        }
        if c.min_permissible_ceiling >= c.max_permissible_ceiling {
            return Err("CONFIG_INVALID: Minimum boundary envelope cannot equal or exceed Maximum boundary limits.");
        }
        if c.engineering_scale_factor <= 0.0 {
            return Err("CONFIG_INVALID: Engineering scale factor calculations must be strictly positive non-zero parameters.");
        }
        if c.max_rate_of_change_dt <= 0.001 {
            return Err("CONFIG_INVALID: Maximum tracking acceleration steps must exceed minimum threshold zones.");
        }
        if c.max_angular_velocity_ceiling <= 0.0 {
            return Err("CONFIG_INVALID: Maximum permitted turning angular rates must be strictly positive values.");
        }
        if c.fallback_safe_setpoint < c.min_permissible_ceiling || c.fallback_safe_setpoint > c.max_permissible_ceiling {
            return Err("CONFIG_INVALID: Fallback safe setpoint maps outside permissible core tracking boundaries.");
        }
        if c.constraint_cap_min < c.min_permissible_ceiling || c.constraint_cap_max > c.max_permissible_ceiling {
            return Err("CONFIG_INVALID: Posture tracking caps expand past absolute hard engineering bounds.");
        }
        if c.constraint_cap_min >= c.constraint_cap_max {
            return Err("CONFIG_INVALID: Degraded processing bounds parameters are logically inverted or equivalent.");
        }

        Ok(())
    }

    pub fn load_and_validate<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("FILE_OPEN_ERROR: {}", e))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|e| format!("FILE_READ_ERROR: {}", e))?;

        let parsed: Self = serde_json::from_str(&contents).map_err(|e| format!("JSON_DESERIALIZE_ERROR: {}", e))?;
        parsed.validate_safety_invariants().map_err(|e| e.to_string())?;
        Ok(parsed)
    }
}
