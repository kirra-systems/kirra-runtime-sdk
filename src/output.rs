// src/output.rs
use crate::aegis_core::{CausalFlightRecorder, TrustMode};
use crate::industrial_proxy::RealTimeTimingAnalytics;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutiveSummary {
    pub processed_traffic_count: u32,
    pub attempted_unsafe_actions: u32,
    pub policy_enforced_actions: u32,
    pub rate_limited_actions: u32,
    pub final_trust_mode: TrustMode,
    pub asset_in_safe_control_state: bool,
    pub final_process_value: f64,
    pub latest_timing_snapshot: Option<RealTimeTimingAnalytics>,
}

pub fn save_replay_json(recorder: &CausalFlightRecorder, base_dir: &str, filename: &str) -> std::io::Result<()> {
    let target_path = Path::new(base_dir).join(filename);
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(recorder)?;
    let mut file = File::create(target_path)?;
    file.write_all(serialized.as_bytes())?;
    Ok(())
}

pub fn save_summary_json(summary: &ExecutiveSummary, base_dir: &str, filename: &str) -> std::io::Result<()> {
    let target_path = Path::new(base_dir).join(filename);
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(summary)?;
    let mut file = File::create(target_path)?;
    file.write_all(serialized.as_bytes())?;
    Ok(())
}

pub fn save_brute_force_counter(count: u32, base_dir: &str) -> std::io::Result<()> {
    let target_path = Path::new(base_dir).join("aegis_brute_force.dat");
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = File::create(target_path)?;
    file.write_all(count.to_string().as_bytes())?;
    Ok(())
}

pub fn load_brute_force_counter(base_dir: &str) -> u32 {
    let target_path = Path::new(base_dir).join("aegis_brute_force.dat");
    if let Ok(mut file) = File::open(target_path) {
        let mut contents = String::new();
        if std::io::Read::read_to_string(&mut file, &mut contents).is_ok() {
            if let Ok(val) = contents.trim().parse::<u32>() {
                return val;
            }
        }
    }
    0
}
