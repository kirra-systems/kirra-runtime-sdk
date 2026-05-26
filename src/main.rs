// src/main.rs

use std::env;
use std::sync::mpsc::channel;
use kirra_runtime_sdk::config::KirraRuntimeConfig;
use kirra_runtime_sdk::gateway::KirraLiveGateway;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() <= 1 || args[1] != "gateway" {
        eprintln!("=========================================================================");
        eprintln!("Kirra Runtime Gateway Interposer Engine");
        eprintln!("=========================================================================");
        eprintln!("Usage Error: Missing or unrecognized runtime command sequence target.");
        eprintln!("Execution Path: cargo run -- gateway [path_to_asset_profile.json]");
        std::process::exit(1);
    }

    let config_path = args.get(2).map(|s| s.as_str()).unwrap_or("config/asset_profile.json");
    let runtime_config = KirraRuntimeConfig::load_and_validate(config_path).expect("BOOT_HALTED_INVALID_CONFIG");

    let raw_key_string = env::var("KIRRA_SUPERVISOR_RESET_KEY").expect("SECURITY_FAILURE_ENV_KEY_MISSING");

    if raw_key_string.is_empty() {
        eprintln!("[CRITICAL SECURITY FAILURE] KIRRA_SUPERVISOR_RESET_KEY exists but contains no token bytes.");
        std::process::exit(1);
    }
    if raw_key_string.len() > 64 {
        eprintln!("[CRITICAL SECURITY FAILURE] Administrative override token length exceeds maximum 64-byte bounds.");
        std::process::exit(1);
    }

    let secure_key = raw_key_string.into_bytes();

    let interposer = KirraLiveGateway::new(
        runtime_config.network.proxy_listen_port,
        runtime_config.network.plc_target_port,
        runtime_config.network.admin_reset_port,
        runtime_config.network.metrics_http_port,
        runtime_config.contract,
        secure_key,
        runtime_config.network.max_concurrent_connections,
        runtime_config.telemetry.log_directory,
    );

    let (tx, rx) = channel();
    interposer.spawn_mock_plc_target(tx);
    if rx.recv_timeout(std::time::Duration::from_millis(500)).is_ok() {
        println!("[SUCCESS] Kirra inline protection substrate active.");
        interposer.start_active_proxy_gateway();
    }
}
