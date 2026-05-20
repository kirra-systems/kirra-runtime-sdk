// src/gateway.rs

use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::thread;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use std::sync::atomic::Ordering;

use crate::{ProtocolAdapter, SafetyGovernor, TrustMode};
use crate::aegis_core::{AegisKernelGovernor, ContractProfile, CausalFlightRecorder, GlobalSystemState};
use crate::modbus_adapter::ModbusTcpAdapter;
use crate::metrics::LockFreeMetricsAggregator;
use crate::output::{save_brute_force_counter, load_brute_force_counter, save_replay_json, save_summary_json, ExecutiveSummary};

struct ThreadPoolGuard { counter: Arc<std::sync::atomic::AtomicU32> }
impl ThreadPoolGuard { fn new(counter: Arc<std::sync::atomic::AtomicU32>) -> Self { Self { counter } } }
impl Drop for ThreadPoolGuard { fn drop(&mut self) { self.counter.fetch_sub(1, Ordering::SeqCst); } }

pub struct AegisLiveGateway {
    pub proxy_port: u16, pub plc_target_port: u16, pub admin_reset_port: u16, pub metrics_port: u16,
    pub runtime_config: ContractProfile, pub system_auth_key: Vec<u8>,
    pub max_allowed_workers: u32, pub log_directory: String, pub io_writer_lock: Arc<Mutex<()>>,
}

impl AegisLiveGateway {
    pub fn new(proxy_port: u16, plc_target_port: u16, admin_port: u16, metrics_port: u16, config: ContractProfile, auth_key: Vec<u8>, max_threads: u32, log_dir: String) -> Self {
        Self { proxy_port, plc_target_port, admin_reset_port: admin_port, metrics_port, runtime_config: config, system_auth_key: auth_key, max_allowed_workers: max_threads.max(1), log_directory: log_dir, io_writer_lock: Arc::new(Mutex::new(())) }
    }

    fn read_exact_frame<R: Read>(stream: &mut R, expected_len: usize, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        let mut total_read = 0;
        let start_time = Instant::now();
        while total_read < expected_len {
            if start_time.elapsed() > Duration::from_millis(500) {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Read deadline met"));
            }
            match stream.read(&mut buffer[total_read..expected_len]) {
                Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Stream closed unexpectedly")),
                Ok(n) => total_read += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => { thread::sleep(Duration::from_millis(5)); continue; }
                Err(e) => return Err(e),
            }
        }
        Ok(total_read)
    }

    pub fn start_active_proxy_gateway(&self) {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", self.proxy_port)).expect("FAIL_BIND");
        let initial_gov = AegisKernelGovernor::new(
            self.runtime_config,
            self.runtime_config.fallback_safe_setpoint,
            self.runtime_config.constraint_cap_min,
            self.runtime_config.constraint_cap_max,
        );

        let shared_governor = Arc::new(Mutex::new(initial_gov));
        let flight_recorder = Arc::new(Mutex::new(CausalFlightRecorder::new()));
        let metrics = Arc::new(LockFreeMetricsAggregator::new());
        let active_workers = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let admin_listener = TcpListener::bind(format!("127.0.0.1:{}", self.admin_reset_port)).expect("FAIL_ADMIN_BIND");
        let gov_admin_clone = Arc::clone(&shared_governor);
        let auth_key = self.system_auth_key.clone();
        let log_dir = self.log_directory.clone();
        let io_lock_admin = Arc::clone(&self.io_writer_lock);

        thread::spawn(move || {
            for stream in admin_listener.incoming() {
                if let Ok(mut socket) = stream {
                    let mut buffer = [0u8; 128];
                    if let Ok(n) = socket.read(&mut buffer) {
                        let mut len = n;
                        while len > 0 && (buffer[len-1] == b'\n' || buffer[len-1] == b'\r') { len -= 1; }
                        let token = &buffer[0..len];
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

                        let tracking_attempts = load_brute_force_counter(&log_dir);
                        let mut auth_res: Result<(), &'static str> = Err("MUTEX_LOCK_FAIL");
                        let mut captured_attempts_count = tracking_attempts;

                        {
                            if let Ok(mut gov) = gov_admin_clone.lock() {
                                gov.trust_engine.failed_reset_attempts = tracking_attempts;
                                auth_res = gov.trust_engine.authenticated_manual_reset(token, &auth_key, now);
                                captured_attempts_count = gov.trust_engine.failed_reset_attempts;
                            }
                        }

                        match auth_res {
                            Ok(_) => {
                                let _io_guard = io_lock_admin.lock().unwrap();
                                let _ = save_brute_force_counter(0, &log_dir);
                                let _ = socket.write_all(b"RESET_SUCCESS\n");
                            }
                            Err(msg) => {
                                {
                                    let _io_guard = io_lock_admin.lock().unwrap();
                                    let _ = save_brute_force_counter(captured_attempts_count, &log_dir);
                                }
                                let _ = socket.write_all(format!("RESET_FAIL: {}\n", msg).as_bytes());
                            }
                        }
                    }
                }
            }
        });

        let metrics_bind_port = self.metrics_port;
        let metrics_clone_http = Arc::clone(&metrics);
        let gov_health_clone = Arc::clone(&shared_governor);
        thread::spawn(move || {
            let http_listener = match TcpListener::bind(format!("127.0.0.1:{}", metrics_bind_port)) {
                Ok(l) => l,
                Err(_) => return,
            };
            for stream in http_listener.incoming() {
                if let Ok(mut socket) = stream {
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut request_buffer = [0u8; 512];
                    let bytes_read = match socket.read(&mut request_buffer) {
                        Ok(n) if n > 0 => n,
                        _ => continue,
                    };
                    let request_str = std::str::from_utf8(&request_buffer[..bytes_read]).unwrap_or("");
                    let first_line = request_str.lines().next().unwrap_or("");

                    let (status, body) = if first_line.starts_with("GET /metrics") {
                        let body = metrics_clone_http.format_prometheus_metrics("aegis-gateway");
                        ("200 OK", body)
                    } else if first_line.starts_with("GET /health/live") {
                        ("200 OK", "{\"alive\":true}".to_string())
                    } else if first_line.starts_with("GET /health/ready") {
                        let trust = {
                            let gov = gov_health_clone.lock().unwrap_or_else(|e| e.into_inner());
                            gov.trust_mode()
                        };
                        let ready = trust != crate::TrustMode::LockedOut;
                        let body = format!("{{\"ready\":{}}}", ready);
                        ("200 OK", body)
                    } else {
                        ("404 Not Found", "Not Found".to_string())
                    };

                    let response = format!(
                        "HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
                        status, body.len(), body
                    );
                    let _ = socket.write_all(response.as_bytes());
                }
            }
        });

        for stream in listener.incoming() {
            if let Ok(client) = stream {
                let mut current_threads = active_workers.load(Ordering::SeqCst);
                loop {
                    if current_threads >= self.max_allowed_workers { break; }
                    match active_workers.compare_exchange_weak(current_threads, current_threads + 1, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => break,
                        Err(actual) => current_threads = actual,
                    }
                }
                if current_threads >= self.max_allowed_workers { continue; }

                let mut mut_client = client;
                let plc_addr = format!("127.0.0.1:{}", self.plc_target_port);
                let gov_worker_clone = Arc::clone(&shared_governor);
                let recorder_worker_clone = Arc::clone(&flight_recorder);
                let metrics_clone = Arc::clone(&metrics);
                let workers_counter = Arc::clone(&active_workers);
                let adapter_clone = ModbusTcpAdapter::new(self.runtime_config.asset_register_offset, self.runtime_config.engineering_scale_factor);
                let io_lock_worker = Arc::clone(&self.io_writer_lock);
                let local_log_dir = self.log_directory.clone();

                thread::spawn(move || {
                    let _guard = ThreadPoolGuard::new(workers_counter);
                    let mut plc_socket = match TcpStream::connect(plc_addr) { Ok(s) => s, Err(_) => return };
                    let mut buf = [0u8; 512];
                    let mut plc_buf = [0u8; 512];

                    while let Ok(_) = Self::read_exact_frame(&mut mut_client, 6, &mut buf[0..6]) {
                        let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                        if len == 0 || len > 500 { break; }
                        if Self::read_exact_frame(&mut mut_client, len, &mut buf[6..6+len]).is_err() { break; }

                        let raw_frame = &buf[0..6+len];
                        let mut flush_payload: Option<CausalFlightRecorder> = None;

                        let out_bytes = match adapter_clone.decode_demand(raw_frame) {
                            Ok(demand) => {
                                let mut gov = gov_worker_clone.lock().unwrap();
                                let intercept = gov.evaluate(demand, 0.050);
                                let processed = metrics_clone.total_processed_frames.fetch_add(1, Ordering::Relaxed) + 1;

                                if intercept.was_unsafe_attempt {
                                    metrics_clone.envelope_clamping_events.fetch_add(1, Ordering::Relaxed);
                                }
                                if intercept.was_rate_breached {
                                    metrics_clone.rate_limiting_events.fetch_add(1, Ordering::Relaxed);
                                }

                                if processed % 100 == 0 {
                                    let mut rec = recorder_worker_clone.lock().unwrap();
                                    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

                                    let dynamic_resolution_text = if (intercept.sanitized_scalar - demand).abs() > 0.001 {
                                        "MUTATED_CLAMP"
                                    } else {
                                        "TRANSPARENT"
                                    };

                                    let system_state_enum = match gov.trust_mode() {
                                        TrustMode::FullAutonomy => GlobalSystemState::Normal,
                                        _ => GlobalSystemState::Degraded,
                                    };

                                    rec.log(now_ms, "NETWORK_PROXY", "SESSION_TOKEN", "MODBUS_WRITE", dynamic_resolution_text, system_state_enum, gov.trust_mode(), gov.trust_engine.current_score, intercept.mitigation_narrative.clone());
                                    flush_payload = Some(rec.clone());
                                }
                                adapter_clone.encode_response(intercept.sanitized_scalar, raw_frame)
                            }
                            Err(e) => {
                                let code = match e { crate::AdapterError::UnmonitoredRegisterTarget => 0x02, _ => 0x04 };
                                adapter_clone.encode_exception(raw_frame, code)
                            }
                        };

                        // RC13-M1: flush happens after all locks released — no gov lock held during disk I/O
                        if let Some(records_data) = flush_payload {
                            let _io_guard = io_lock_worker.lock().unwrap();
                            let _ = save_replay_json(&records_data, &local_log_dir, "aegis_replay.json");
                        }

                        if plc_socket.write_all(&out_bytes).is_err() { break; }
                        if Self::read_exact_frame(&mut plc_socket, 6, &mut plc_buf[0..6]).is_err() { break; }
                        let p_len = u16::from_be_bytes([plc_buf[4], plc_buf[5]]) as usize;
                        if Self::read_exact_frame(&mut plc_socket, p_len, &mut plc_buf[6..6+p_len]).is_err() { break; }
                        if mut_client.write_all(&plc_buf[0..6+p_len]).is_err() { break; }
                    }

                    // RC13-M1: snapshot needed values from gov, drop the lock, then take io_lock for disk write
                    let (final_trust, last_val) = {
                        let gov = gov_worker_clone.lock().unwrap_or_else(|e| e.into_inner());
                        (gov.trust_mode(), gov.last_output())
                    };

                    let total_traffic = metrics_clone.total_processed_frames.load(Ordering::Relaxed) as u32;
                    // RC13-M2: policy_enforced_actions = envelope clamps + rate limits (all frames where governor changed output)
                    let envelope_events = metrics_clone.envelope_clamping_events.load(Ordering::Relaxed) as u32;
                    let rate_events = metrics_clone.rate_limiting_events.load(Ordering::Relaxed) as u32;
                    let summary = ExecutiveSummary {
                        processed_traffic_count: total_traffic,
                        attempted_unsafe_actions: envelope_events,
                        policy_enforced_actions: envelope_events + rate_events,
                        rate_limited_actions: rate_events,
                        final_trust_mode: final_trust,
                        asset_in_safe_control_state: final_trust == TrustMode::FullAutonomy,
                        final_process_value_counts: last_val,
                    };

                    let _io_guard = io_lock_worker.lock().unwrap();
                    let _ = save_summary_json(&summary, &local_log_dir, "aegis_summary.json");
                });
            }
        }
    }

    pub fn spawn_mock_plc_target(&self, ready_tx: Sender<Result<(), String>>) {
        let port = self.plc_target_port;
        thread::spawn(move || {
            let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)) {
                Ok(l) => { let _ = ready_tx.send(Ok(())); l }
                Err(e) => { let _ = ready_tx.send(Err(e.to_string())); return; }
            };
            for stream in listener.incoming() {
                if let Ok(mut socket) = stream {
                    let mut buf = [0u8; 512];
                    while let Ok(_) = Self::read_exact_frame(&mut socket, 6, &mut buf[0..6]) {
                        let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                        if Self::read_exact_frame(&mut socket, len, &mut buf[6..6+len]).is_ok() {
                            let _ = socket.write_all(&buf[0..6+len]);
                        }
                    }
                }
            }
        });
    }
}
