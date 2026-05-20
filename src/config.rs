// src/config.rs
use crate::aegis_core::SafetyContractProfile;
use serde::{Deserialize, Serialize};
use std::fs::File;

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
    pub contract: SafetyContractProfile,
}

pub fn load_from_file(path: &str) -> Result<AegisRuntimeConfig, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let config: AegisRuntimeConfig = serde_json::from_reader(file)?;
    Ok(config)
}
