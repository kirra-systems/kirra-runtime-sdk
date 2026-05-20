// src/gateway.rs
use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::thread;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use std::sync::atomic::Ordering;
use crate::aegis_core::{AegisUnifiedGovernor, SafetyContractProfile, LockFreeTelemetryBus, CausalFlightRecorder};
use crate::industrial_proxy::{LiveProtocolSimulator, RealTimeTimingAnalytics};
use crate::output::{ExecutiveSummary, save_replay_json, save_summary_json, save_brute_force_counter, load_brute_force_counter};

struct ThreadPoolGuard {
    counter: Arc<std::sync::atomic::AtomicU32>,
}
impl ThreadPoolGuard {
    fn new(counter: Arc<std::sync::atomic::AtomicU32>) -> Self { Self { counter } }
}
impl Drop for ThreadPoolGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct AegisLiveGateway {
    proxy_port: u16,
    plc_target_port: u16,
    admin_reset_port: u16,
    runtime_config: SafetyContractProfile,
    system_auth_key: Vec<u8>,
    fixed_dt_override: Option<f64>,
    max_allowed_workers: u32,
    log_directory: String,
    io_writer_lock: Arc<Mutex<()>>,
}

impl AegisLiveGateway {
    pub fn new(proxy_port: u16, plc_target_port: u16, admin_port: u16, config: SafetyContractProfile, auth_key: Vec<u8>, max_threads: u32, fixed_dt: Option<f64>, log_dir: String) -> Self {
        Self {
            proxy_port,
            plc_target_port,
            admin_reset_port: admin_port,
            runtime_config: config,
            system_auth_key: auth_key,
            fixed_dt_override: fixed_dt,
            max_allowed_workers: max_threads.max(1),
            log_directory: log_dir,
            io_writer_lock: Arc::new(Mutex::new(())),
        }
    }

    fn generate_epoch_timestamp_prefix() -> String {
        if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            return format!("UTC_EPOCH_SECS:{}", duration.as_secs());
        }
        "UTC_EPOCH_SECS:1747699200".to_string()
    }

    fn read_exact_frame<R: Read>(stream: &mut R, expected_len: usize, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        let mut total_read = 0;
        let start_time = Instant::now();
        while total_read < expected_len {
            if start_time.elapsed() > Duration::from_millis(500) {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Read execution budget exceeded"));
            }
            match stream.read(&mut buffer[total_read..expected_len]) {
                Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Stream truncated")),
                Ok(n) => total_read += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(total_read)
    }

    pub fn start_admin_reset_listener(&self, simulator: Arc<Mutex<LiveProtocolSimulator>>) {
        let listen_addr = format!("127.0.0.1:{}", self.admin_reset_port);
        let listener = TcpListener::bind(&listen_addr).expect("FAIL: Bind admin listener port");
        let auth_key = self.system_auth_key.clone();
        let log_dir_clone = self.log_directory.clone();
        let disk_io_lock = Arc::clone(&self.io_writer_lock);

        thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut socket) = stream {
                    let mut buffer = [0u8; 128];
                    if let Ok(n) = socket.read(&mut buffer) {
                        if n > 0 {
                            let mut len_trimmed = n;
                            while len_trimmed > 0 && (buffer[len_trimmed - 1] == b'\n' || buffer[len_trimmed - 1] == b'\r') {
                                len_trimmed -= 1;
                            }
                            let raw_token = &buffer[0..len_trimmed];
                            let current_epoch = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;

                            let mut sim = simulator.lock().unwrap_or_else(|e| e.into_inner());
                            sim.unified_governor.trust_evaluator.failed_reset_attempts = load_brute_force_counter(&log_dir_clone);

                            match sim.unified_governor.trust_evaluator.authenticated_manual_reset(raw_token, &auth_key, current_epoch) {
                                Ok(_) => {
                                    let _io_guard = disk_io_lock.lock().unwrap_or_else(|e| e.into_inner());
                                    let _ = save_brute_force_counter(0, &log_dir_clone);
                                    let _ = socket.write_all(b"RESET_SUCCESS\n");
                                }
                                Err(msg) => {
                                    let tracked_attempts = sim.unified_governor.trust_evaluator.failed_reset_attempts;
                                    {
                                        let _io_guard = disk_io_lock.lock().unwrap_or_else(|e| e.into_inner());
                                        let _ = save_brute_force_counter(tracked_attempts, &log_dir_clone);
                                    }
                                    eprintln!("[HexTokenAuth] [{}] [SECURITY ALERT] PRIVILEGED_RESET_DENIED | Count: {} | Reason: {}",
                                        Self::generate_epoch_timestamp_prefix(), tracked_attempts, msg);
                                    let _ = socket.write_all(format!("RESET_FAIL: {}\n", msg).as_bytes());
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn spawn_mock_plc_target(&self, ready_tx: Sender<Result<(), String>>) {
        let plc_port = self.plc_target_port;
        thread::spawn(move || {
            let listen_addr = format!("127.0.0.1:{}", plc_port);
            let listener = match TcpListener::bind(&listen_addr) {
                Ok(l) => { let _ = ready_tx.send(Ok(())); l }
                Err(e) => { let _ = ready_tx.send(Err(format!("PLC_BIND_ERROR: {}", e))); return; }
            };
            for stream in listener.incoming() {
                if let Ok(mut socket) = stream {
                    if socket.set_read_timeout(Some(Duration::from_millis(100))).is_ok() {
                        let mut buffer = [0u8; 512];
                        while let Ok(_) = Self::read_exact_frame(&mut socket, 6, &mut buffer[0..6]) {
                            let inner_pdu_len = u16::from_be_bytes([buffer[4], buffer[5]]) as usize;
                            if inner_pdu_len > 0 && inner_pdu_len < 500 {
                                if Self::read_exact_frame(&mut socket, inner_pdu_len, &mut buffer[6..6+inner_pdu_len]).is_ok() {
                                    let total_packet = 6 + inner_pdu_len;
                                    let _ = socket.write_all(&buffer[0..total_packet]);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn start_active_proxy_gateway(&self) {
        let proxy_addr = format!("127.0.0.1:{}", self.proxy_port);
        let listener = TcpListener::bind(&proxy_addr).expect("FAIL: Bind proxy socket.");
        println!("[{}] [AEGIS GATEWAY] Live proxy server active on {}...", Self::generate_epoch_timestamp_prefix(), proxy_addr);

        let plc_addr = format!("127.0.0.1:{}", self.plc_target_port);
        let initial_governor = AegisUnifiedGovernor::new(self.runtime_config, self.runtime_config.fallback_safe_setpoint);
        let shared_simulator = Arc::new(Mutex::new(LiveProtocolSimulator::new(initial_governor)));
        let atomic_telemetry_bus = Arc::new(LockFreeTelemetryBus::new());
        let active_worker_threads = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let fixed_dt_override = self.fixed_dt_override;
        let max_allowed_workers = self.max_allowed_workers;
        let log_dir_clone = self.log_directory.clone();
        let global_io_lock = Arc::clone(&self.io_writer_lock);

        self.start_admin_reset_listener(Arc::clone(&shared_simulator));

        for stream in listener.incoming() {
            if let Ok(client_socket) = stream {
                let mut current_threads = active_worker_threads.load(Ordering::SeqCst);
                loop {
                    if current_threads >= max_allowed_workers { break; }
                    match active_worker_threads.compare_exchange_weak(current_threads, current_threads + 1, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => break,
                        Err(actual) => current_threads = actual,
                    }
                }
                if current_threads >= max_allowed_workers { continue; }

                let plc_addr_clone = plc_addr.clone();
                let simulator_clone = Arc::clone(&shared_simulator);
                let telemetry_clone = Arc::clone(&atomic_telemetry_bus);
                let worker_pool_counter = Arc::clone(&active_worker_threads);
                let thread_log_dir = log_dir_clone.clone();
                let thread_io_lock = Arc::clone(&global_io_lock);

                thread::spawn(move || {
                    let _pool_guard = ThreadPoolGuard::new(worker_pool_counter);
                    let mut mut_client_socket = client_socket;

                    if mut_client_socket.set_read_timeout(Some(Duration::from_millis(250))).is_err() { return; }

                    let mut plc_socket = match TcpStream::connect(&plc_addr_clone) {
                        Ok(sock) => sock,
                        Err(_) => { return; }
                    };
                    if plc_socket.set_read_timeout(Some(Duration::from_millis(250))).is_err() { return; }

                    let mut buffer = [0u8; 512];
                    let mut plc_buffer = [0u8; 512];
                    let mut sim_clock_ms = 0;
                    let mut last_packet_intercept = Instant::now();
                    let loop_dt_fallback = 0.050;
                    let mut localized_processed_count = 0u32;

                    while let Ok(_) = Self::read_exact_frame(&mut mut_client_socket, 6, &mut buffer[0..6]) {
                        let pdu_len = u16::from_be_bytes([buffer[4], buffer[5]]) as usize;
                        if pdu_len == 0 || pdu_len > 500 { break; }
                        if Self::read_exact_frame(&mut mut_client_socket, pdu_len, &mut buffer[6..6+pdu_len]).is_err() { break; }
                        let total_frame_len = 6 + pdu_len;
                        let raw_packet = &buffer[0..total_frame_len];

                        let current_timestamp = Instant::now();
                        let loop_dt = current_timestamp.duration_since(last_packet_intercept).as_secs_f64();
                        last_packet_intercept = current_timestamp;

                        let actual_dt = match fixed_dt_override {
                            Some(val) => val,
                            None => if loop_dt <= 0.0 { loop_dt_fallback } else { loop_dt }
                        };

                        let mut flush_payload: Option<(CausalFlightRecorder, ExecutiveSummary)> = None;

                        let execution_plane_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let mut sim = simulator_clone.lock().unwrap_or_else(|e| e.into_inner());
                            let previous_validated_scalar = sim.unified_governor.last_validated_scalar;

                            let result = sim.process_wire_packet(raw_packet, sim_clock_ms, actual_dt);
                            localized_processed_count = localized_processed_count.saturating_add(1);

                            telemetry_clone.processed_traffic_count.fetch_add(1, Ordering::SeqCst);
                            if result.was_unsafe_attempt { telemetry_clone.attempted_unsafe_actions.fetch_add(1, Ordering::SeqCst); }
                            if result.was_mitigated { telemetry_clone.policy_enforced_actions.fetch_add(1, Ordering::SeqCst); }
                            if result.was_rate_breached { telemetry_clone.rate_limited_actions.fetch_add(1, Ordering::SeqCst); }

                            let real_dt_ms = loop_dt * 1000.0;
                            let target_dt_ms = 50.0;
                            let jitter_ms = (real_dt_ms - target_dt_ms).abs();
                            let expected_allowed_delta = sim.unified_governor.contract_config.max_rate_of_change_dt * actual_dt;
                            let actual_output_delta = (sim.unified_governor.last_validated_scalar - previous_validated_scalar).abs();
                            let tolerance_epsilon = expected_allowed_delta * 0.015;

                            let timing_check_applicable = result.was_rate_breached;
                            let within_tolerance = if timing_check_applicable {
                                actual_output_delta <= (expected_allowed_delta + tolerance_epsilon)
                            } else {
                                true
                            };

                            let timing_analytics = RealTimeTimingAnalytics {
                                real_dt_ms, target_dt_ms, jitter_ms, expected_allowed_delta,
                                actual_output_delta, tolerance_epsilon, within_tolerance, timing_check_applicable,
                            };

                            let summary = ExecutiveSummary {
                                processed_traffic_count: telemetry_clone.processed_traffic_count.load(Ordering::SeqCst),
                                attempted_unsafe_actions: telemetry_clone.attempted_unsafe_actions.load(Ordering::SeqCst),
                                policy_enforced_actions: telemetry_clone.policy_enforced_actions.load(Ordering::SeqCst),
                                rate_limited_actions: telemetry_clone.rate_limited_actions.load(Ordering::SeqCst),
                                final_trust_mode: sim.unified_governor.trust_evaluator.mode,
                                asset_in_safe_control_state: result.asset_in_safe_control_state,
                                final_process_value: sim.unified_governor.last_validated_scalar,
                                latest_timing_snapshot: Some(timing_analytics),
                            };

                            if (localized_processed_count % 100) == 0 {
                                flush_payload = Some((sim.recorder.clone(), summary));
                                localized_processed_count = 0;
                            }

                            result
                        }));

                        let result = match execution_plane_result {
                            Ok(data) => data,
                            Err(_) => { break; }
                        };

                        if let Some((recorder_data, summary_data)) = flush_payload {
                            let _io_guard = thread_io_lock.lock().unwrap_or_else(|e| e.into_inner());
                            let _ = save_replay_json(&recorder_data, &thread_log_dir, "aegis_replay.json");
                            let _ = save_summary_json(&summary_data, &thread_log_dir, "aegis_summary.json");
                        }

                        if plc_socket.write_all(&result.outbound_bytes).is_err() { break; }

                        if Self::read_exact_frame(&mut plc_socket, 6, &mut plc_buffer[0..6]).is_err() { break; }
                        let plc_pdu_len = u16::from_be_bytes([plc_buffer[4], plc_buffer[5]]) as usize;
                        if plc_pdu_len == 0 || plc_pdu_len > 500 { break; }
                        if Self::read_exact_frame(&mut plc_socket, plc_pdu_len, &mut plc_buffer[6..6+plc_pdu_len]).is_err() { break; }

                        let total_plc_frame_len = 6 + plc_pdu_len;
                        if mut_client_socket.write_all(&plc_buffer[0..total_plc_frame_len]).is_err() { break; }

                        sim_clock_ms += 50;
                        thread::sleep(Duration::from_millis(50));
                    }

                    let final_teardown_snapshot: Option<(CausalFlightRecorder, ExecutiveSummary)> = {
                        if let Ok(sim) = simulator_clone.lock() {
                            let summary = ExecutiveSummary {
                                processed_traffic_count: telemetry_clone.processed_traffic_count.load(Ordering::SeqCst),
                                attempted_unsafe_actions: telemetry_clone.attempted_unsafe_actions.load(Ordering::SeqCst),
                                policy_enforced_actions: telemetry_clone.policy_enforced_actions.load(Ordering::SeqCst),
                                rate_limited_actions: telemetry_clone.rate_limited_actions.load(Ordering::SeqCst),
                                final_trust_mode: sim.unified_governor.trust_evaluator.mode,
                                asset_in_safe_control_state: false,
                                final_process_value: sim.unified_governor.last_validated_scalar,
                                latest_timing_snapshot: None,
                            };
                            Some((sim.recorder.clone(), summary))
                        } else {
                            None
                        }
                    };

                    if let Some((recorder_data, summary_data)) = final_teardown_snapshot {
                        let _io_guard = thread_io_lock.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = save_replay_json(&recorder_data, &thread_log_dir, "aegis_replay.json");
                        let _ = save_summary_json(&summary_data, &thread_log_dir, "aegis_summary.json");
                    }
                });
            }
        }
    }
}
