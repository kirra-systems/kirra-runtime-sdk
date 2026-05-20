// src/main.rs
mod aegis_core;
mod config;
mod gateway;
mod industrial_proxy;
mod output;

#[cfg(test)]
mod tests;

use gateway::AegisLiveGateway;
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config/asset_profile.json".to_string());

    let runtime_config = match config::load_from_file(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[AEGIS FATAL] Failed to load config from '{}': {}", config_path, e);
            std::process::exit(1);
        }
    };

    let raw_key = match std::env::var("AEGIS_SUPERVISOR_RESET_KEY") {
        Ok(val) => val,
        Err(_) => {
            eprintln!("[AEGIS FATAL] AEGIS_SUPERVISOR_RESET_KEY environment variable not set. Refusing to start.");
            std::process::exit(1);
        }
    };

    if raw_key.is_empty() {
        eprintln!("[AEGIS FATAL] AEGIS_SUPERVISOR_RESET_KEY is empty. Refusing to start.");
        std::process::exit(1);
    }

    let key_bytes = raw_key.as_bytes();
    if key_bytes.len() > 64 {
        eprintln!("[AEGIS FATAL] AEGIS_SUPERVISOR_RESET_KEY exceeds maximum length of 64 bytes. Refusing to start.");
        std::process::exit(1);
    }

    let gateway = AegisLiveGateway::new(
        runtime_config.network.proxy_listen_port,
        runtime_config.network.plc_target_port,
        runtime_config.network.admin_reset_port,
        runtime_config.contract,
        key_bytes.to_vec(),
        runtime_config.network.max_concurrent_connections,
        None,
        runtime_config.telemetry.log_directory,
    );

    let (ready_tx, ready_rx) = mpsc::channel();
    gateway.spawn_mock_plc_target(ready_tx);

    match ready_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("[AEGIS FATAL] Mock PLC failed to bind: {}", e);
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[AEGIS FATAL] Mock PLC did not signal readiness within 500ms.");
            std::process::exit(1);
        }
    }

    gateway.start_active_proxy_gateway();
}
