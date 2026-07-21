// src/gateway/mod.rs

pub mod contract_profiles;
pub mod policy;
pub mod policy_layer;

#[cfg(test)]
mod kinematics_proptest;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::kirra_core::{
    CausalFlightRecorder, ContractProfile, GlobalSystemState, KirraKernelGovernor,
};
use crate::metrics::LockFreeMetricsAggregator;
use crate::modbus_adapter::ModbusTcpAdapter;
use crate::output::{
    load_brute_force_counter, save_brute_force_counter, save_replay_json, save_summary_json,
    ExecutiveSummary,
};
use crate::{ProtocolAdapter, SafetyGovernor, TrustMode};

/// Nominal control period (s) used for the FIRST governed frame on a fresh proxy
/// connection sequence, which has no prior sample to measure a rate against.
/// Matches the legacy fixed timestep, so first-frame behaviour is unchanged.
const NOMINAL_CONTROL_PERIOD_S: f64 = 0.050;

/// Upper bound (s) on the measured inter-frame dt fed to the scalar rate governor
/// (B4). A long idle between frames would otherwise yield a large dt and hence a
/// large permitted single step (`max_rate * dt`), letting a post-idle frame jump
/// effectively unbounded and defeating the rate-of-change limit. Capping dt keeps
/// the limiter protective across an idle gap.
const MAX_GOVERNED_DT_S: f64 = 1.0;

/// Real elapsed dt (s) since the previous governed frame, for the scalar rate
/// governor (B4). The proxy previously fed a fabricated constant `0.050`, so the
/// rate-of-change limiter measured fictional rates: a slow legitimate change read
/// as a false breach, and a fast burst read as slower than reality (a missed
/// breach). `elapsed = None` (first frame, no prior sample) → the nominal period;
/// otherwise the true elapsed time, capped at `MAX_GOVERNED_DT_S`. A small real
/// dt is kept as-is — a large step over a short interval correctly trips the rate
/// clamp (conservative), and `evaluate` itself fail-closes a non-positive dt.
fn governed_dt_secs(elapsed: Option<Duration>) -> f64 {
    match elapsed {
        None => NOMINAL_CONTROL_PERIOD_S,
        Some(d) => d.as_secs_f64().min(MAX_GOVERNED_DT_S),
    }
}

struct ThreadPoolGuard {
    counter: Arc<std::sync::atomic::AtomicU32>,
    aggregator: Arc<LockFreeMetricsAggregator>,
}
impl ThreadPoolGuard {
    fn new(
        counter: Arc<std::sync::atomic::AtomicU32>,
        aggregator: Arc<LockFreeMetricsAggregator>,
    ) -> Self {
        aggregator
            .active_worker_threads
            .fetch_add(1, Ordering::SeqCst);
        Self {
            counter,
            aggregator,
        }
    }
}
impl Drop for ThreadPoolGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
        self.aggregator
            .active_worker_threads
            .fetch_sub(1, Ordering::SeqCst);
    }
}

/// The shared safety governor plus its governor-global dt anchor, guarded by a
/// SINGLE mutex. Folding `last_eval` in here (instead of a separate
/// `Arc<Mutex<Option<Instant>>>`) keeps the B4 invariant atomic — dt is measured
/// against the very `last_validated_scalar` rate anchor it is then evaluated
/// under — AND removes the one lock that used to be nested inside the governor
/// critical section. With no other lock acquired while this guard is held,
/// nothing can be poisoned-while-held and thereby poison the governor.
struct GovernorState {
    governor: KirraKernelGovernor<ContractProfile>,
    /// Instant of the previous governed frame; `None` before the first frame.
    last_eval: Option<Instant>,
}

pub struct KirraLiveGateway {
    pub proxy_port: u16,
    pub plc_target_port: u16,
    pub admin_reset_port: u16,
    pub metrics_port: u16,
    pub runtime_config: ContractProfile,
    pub system_auth_key: Vec<u8>,
    pub max_allowed_workers: u32,
    pub log_directory: String,
    pub io_writer_lock: Arc<Mutex<()>>,
}

/// Bundled construction parameters for [`KirraLiveGateway::new`].
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub proxy_port: u16,
    pub plc_target_port: u16,
    pub admin_port: u16,
    pub metrics_port: u16,
    pub config: ContractProfile,
    pub auth_key: Vec<u8>,
    pub max_threads: u32,
    pub log_dir: String,
}

impl KirraLiveGateway {
    pub fn new(cfg: GatewayConfig) -> Self {
        Self {
            proxy_port: cfg.proxy_port,
            plc_target_port: cfg.plc_target_port,
            admin_reset_port: cfg.admin_port,
            metrics_port: cfg.metrics_port,
            runtime_config: cfg.config,
            system_auth_key: cfg.auth_key,
            max_allowed_workers: cfg.max_threads.max(1),
            log_directory: cfg.log_dir,
            io_writer_lock: Arc::new(Mutex::new(())),
        }
    }

    fn read_exact_frame<R: Read>(
        stream: &mut R,
        expected_len: usize,
        buffer: &mut [u8],
    ) -> Result<usize, std::io::Error> {
        let mut total_read = 0;
        let start_time = Instant::now();
        while total_read < expected_len {
            if start_time.elapsed() > Duration::from_millis(500) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Read deadline met",
                ));
            }
            match stream.read(&mut buffer[total_read..expected_len]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Stream closed unexpectedly",
                    ))
                }
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

    /// The admin-reset listener thread (extracted from `start_active_proxy_gateway`).
    /// Lock behaviour is unchanged: a poisoned governor-state mutex leaves `auth_res`
    /// at its `MUTEX_LOCK_FAIL` default (no reset), and the IO-writer lock keeps its
    /// `.unwrap()` — on this safety binary a poisoned lock panics → (release
    /// `panic=abort`) process death → fail-closed, the intended behaviour.
    fn spawn_admin_listener(
        admin_listener: TcpListener,
        gov_admin_clone: Arc<Mutex<GovernorState>>,
        auth_key: Vec<u8>,
        log_dir: String,
        io_lock_admin: Arc<Mutex<()>>,
        metrics_admin_clone: Arc<LockFreeMetricsAggregator>,
    ) {
        thread::spawn(move || {
            for mut socket in admin_listener.incoming().flatten() {
                let mut buffer = [0u8; 128];
                if let Ok(n) = socket.read(&mut buffer) {
                    let mut len = n;
                    while len > 0 && (buffer[len - 1] == b'\n' || buffer[len - 1] == b'\r') {
                        len -= 1;
                    }
                    let token = &buffer[0..len];
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    let tracking_attempts = load_brute_force_counter(&log_dir);
                    let mut auth_res: Result<(), &'static str> = Err("MUTEX_LOCK_FAIL");
                    let mut captured_attempts_count = tracking_attempts;

                    {
                        if let Ok(mut st) = gov_admin_clone.lock() {
                            let gov = &mut st.governor;
                            gov.trust_engine.failed_reset_attempts = tracking_attempts;
                            auth_res = gov
                                .trust_engine
                                .authenticated_manual_reset(token, &auth_key, now);
                            captured_attempts_count = gov.trust_engine.failed_reset_attempts;
                            metrics_admin_clone
                                .trust_score
                                .store(gov.trust_engine.current_score as u64, Ordering::Relaxed);
                        }
                    }

                    match auth_res {
                        Ok(_) => {
                            let _io_guard = io_lock_admin.lock().unwrap();
                            let _ = save_brute_force_counter(0, &log_dir);
                            let _ = socket.write_all(b"RESET_SUCCESS\n");
                        }
                        Err(msg) => {
                            metrics_admin_clone
                                .authentication_failures
                                .fetch_add(1, Ordering::Relaxed);
                            {
                                let _io_guard = io_lock_admin.lock().unwrap();
                                let _ = save_brute_force_counter(captured_attempts_count, &log_dir);
                            }
                            let _ = socket.write_all(format!("RESET_FAIL: {}\n", msg).as_bytes());
                        }
                    }
                }
            }
        });
    }

    /// The metrics / health listener thread (extracted from `start_active_proxy_gateway`).
    /// The bind now FAILS GRACEFULLY (log + return) instead of `.expect()`-panicking
    /// inside the spawned thread — a metrics-port conflict disables the
    /// observability endpoint (fail-closed readiness still applies) rather than
    /// aborting. The governor `if let Ok(..)` read is unchanged.
    fn spawn_metrics_listener(
        metrics_bind_port: u16,
        metrics_http_clone: Arc<LockFreeMetricsAggregator>,
        gov_http_clone: Arc<Mutex<GovernorState>>,
        workers_http_clone: Arc<std::sync::atomic::AtomicU32>,
        max_workers_allowed: u32,
    ) {
        thread::spawn(move || {
            let http_listener = match TcpListener::bind(format!("127.0.0.1:{metrics_bind_port}")) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!(
                        "[CRITICAL] Failed to bind metrics listener on port {metrics_bind_port}: {err}. \
                         Metrics/health endpoint disabled; fail-closed readiness applies."
                    );
                    return;
                }
            };
            let mut request_buffer = [0u8; 1024];
            for mut socket in http_listener.incoming().flatten() {
                let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
                if let Ok(bytes_read) = socket.read(&mut request_buffer) {
                    if bytes_read == 0 {
                        continue;
                    }

                    let req_str = String::from_utf8_lossy(&request_buffer[..bytes_read]);
                    let first_line = req_str.lines().next().unwrap_or("");

                    if first_line.starts_with("GET /metrics") {
                        let payload =
                            metrics_http_clone.format_prometheus_metrics("kirra-core-active");
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload);
                        let _ = socket.write_all(response.as_bytes());
                    } else if first_line.starts_with("GET /health/live") {
                        let body = r#"{"status":"UP"}"#;
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                        let _ = socket.write_all(response.as_bytes());
                    } else if first_line.starts_with("GET /health/ready") {
                        let mut is_ready = false;
                        if let Ok(st) = gov_http_clone.lock() {
                            if st.governor.trust_mode() != TrustMode::LockedOut {
                                let active_conns = workers_http_clone.load(Ordering::SeqCst);
                                if active_conns < max_workers_allowed {
                                    is_ready = true;
                                }
                            }
                        }
                        if is_ready {
                            let body = r#"{"status":"READY"}"#;
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                            let _ = socket.write_all(response.as_bytes());
                        } else {
                            let body = r#"{"status":"NOT_READY","reason":"TRUST_LOCKOUT_OR_POOL_SATURATED"}"#;
                            let response = format!("HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                            let _ = socket.write_all(response.as_bytes());
                        }
                    } else {
                        let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = socket.write_all(response.as_bytes());
                    }
                }
            }
        });
    }

    pub fn start_active_proxy_gateway(&self) {
        let listener = match TcpListener::bind(format!("127.0.0.1:{}", self.proxy_port)) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!(
                    "[CRITICAL] Failed to bind proxy listener on port {}: {err}. Gateway startup aborted.",
                    self.proxy_port
                );
                // Fail-closed: exit non-zero so a supervisor (systemd
                // Restart=on-failure / k8s) treats the bind failure as a real
                // startup failure and restarts. A bare `return` here would let
                // `main` exit 0 and suppress the restart (regression from the
                // prior `.expect()` panic, which aborted non-zero).
                std::process::exit(1);
            }
        };
        let initial_gov = KirraKernelGovernor::new(
            self.runtime_config,
            self.runtime_config.fallback_safe_setpoint,
            self.runtime_config.constraint_cap_min,
            self.runtime_config.constraint_cap_max,
        );

        // B4: the governor and the instant of the last governed frame share ONE
        // mutex. The rate limiter's `last_validated_scalar` anchor is
        // governor-global, so the dt measured against it must be too — folding it
        // into the same lock keeps that measurement atomic with evaluate and means
        // the governor critical section nests no other (poisonable) lock.
        let shared_governor = Arc::new(Mutex::new(GovernorState {
            governor: initial_gov,
            last_eval: None,
        }));
        let flight_recorder = Arc::new(Mutex::new(CausalFlightRecorder::new()));
        let metrics = Arc::new(LockFreeMetricsAggregator::new());
        let active_workers = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let admin_listener = match TcpListener::bind(format!("127.0.0.1:{}", self.admin_reset_port))
        {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!(
                    "[CRITICAL] Failed to bind admin reset listener on port {}: {err}. Gateway startup aborted.",
                    self.admin_reset_port
                );
                // Fail-closed: exit non-zero (same rationale as the proxy bind
                // above) so the bind failure is not masked as a clean exit.
                std::process::exit(1);
            }
        };
        Self::spawn_admin_listener(
            admin_listener,
            Arc::clone(&shared_governor),
            self.system_auth_key.clone(),
            self.log_directory.clone(),
            Arc::clone(&self.io_writer_lock),
            Arc::clone(&metrics),
        );

        Self::spawn_metrics_listener(
            self.metrics_port,
            Arc::clone(&metrics),
            Arc::clone(&shared_governor),
            Arc::clone(&active_workers),
            self.max_allowed_workers,
        );

        for client in listener.incoming().flatten() {
            let mut current_threads = active_workers.load(Ordering::SeqCst);
            loop {
                if current_threads >= self.max_allowed_workers {
                    break;
                }
                match active_workers.compare_exchange_weak(
                    current_threads,
                    current_threads + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(actual) => current_threads = actual,
                }
            }
            if current_threads >= self.max_allowed_workers {
                continue;
            }

            let mut mut_client = client;
            let plc_addr = format!("127.0.0.1:{}", self.plc_target_port);
            let gov_worker_clone = Arc::clone(&shared_governor);
            let recorder_worker_clone = Arc::clone(&flight_recorder);
            let metrics_clone = Arc::clone(&metrics);
            let workers_counter = Arc::clone(&active_workers);
            let adapter_clone = ModbusTcpAdapter::new(
                self.runtime_config.asset_register_offset,
                self.runtime_config.engineering_scale_factor,
            );
            let io_lock_worker = Arc::clone(&self.io_writer_lock);
            let local_log_dir = self.log_directory.clone();

            thread::spawn(move || {
                let _guard = ThreadPoolGuard::new(workers_counter, Arc::clone(&metrics_clone));
                // Borrow (not move) so `plc_addr` can be named in the error log.
                let mut plc_socket = match TcpStream::connect(&plc_addr) {
                    Ok(s) => s,
                    Err(err) => {
                        eprintln!(
                            "[WARN] Unable to connect proxy worker to PLC target {plc_addr}: {err}"
                        );
                        return;
                    }
                };
                let mut buf = [0u8; 512];
                let mut plc_buf = [0u8; 512];

                while Self::read_exact_frame(&mut mut_client, 6, &mut buf[0..6]).is_ok() {
                    let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                    if len == 0 || len > 500 {
                        break;
                    }
                    if Self::read_exact_frame(&mut mut_client, len, &mut buf[6..6 + len]).is_err() {
                        break;
                    }

                    let raw_frame = &buf[0..6 + len];
                    let mut flush_payload: Option<CausalFlightRecorder> = None;

                    let out_bytes = match adapter_clone.decode_demand(raw_frame) {
                        Ok(demand) => {
                            // Worker-panic isolation (fail-closed): hold the shared
                            // governor-state lock for ONLY the panic-free dt + `evaluate`
                            // plus a snapshot of the values the bookkeeping below needs,
                            // then release it BEFORE touching the recorder / io locks.
                            // The governor critical section now nests NO other lock at all
                            // (the dt anchor lives in the SAME mutex), so a panic on the
                            // bookkeeping path can never be raised under the governor lock
                            // and therefore can never poison the governor. (In release
                            // `panic=abort` any panic aborts the process anyway —
                            // fail-closed; the un-nesting is the unwind/test-build
                            // guarantee, and also shortens the WCET-relevant hold.) The
                            // `.unwrap()` is KEPT: a genuinely poisoned governor still
                            // panics → fail-closed, never recovered onto corrupt state.
                            let (intercept, trust_mode, trust_score) = {
                                let mut st = gov_worker_clone.lock().unwrap();
                                // B4: measure dt against the previous governed frame and
                                // evaluate under the SAME guard, so dt stays consistent
                                // with the `last_validated_scalar` rate anchor;
                                // `saturating_*` so a non-monotonic clock reads 0 (→
                                // evaluate fail-closes), never a negative dt.
                                let now = Instant::now();
                                let elapsed =
                                    st.last_eval.map(|prev| now.saturating_duration_since(prev));
                                st.last_eval = Some(now);
                                let dt = governed_dt_secs(elapsed);
                                let intercept = st.governor.evaluate(demand, dt);
                                (
                                    intercept,
                                    st.governor.trust_mode(),
                                    st.governor.trust_engine.current_score,
                                )
                            };

                            let processed = metrics_clone
                                .total_processed_frames
                                .fetch_add(1, Ordering::Relaxed)
                                + 1;
                            metrics_clone
                                .trust_score
                                .store(trust_score as u64, Ordering::Relaxed);

                            if intercept.was_unsafe_attempt {
                                metrics_clone
                                    .envelope_clamping_events
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            if intercept.was_rate_breached {
                                metrics_clone
                                    .rate_limiting_events
                                    .fetch_add(1, Ordering::Relaxed);
                            }

                            if processed.is_multiple_of(100) {
                                // Recorder lock acquired with the governor guard already
                                // released; the entry is built entirely from the snapshot
                                // taken above, so it still reflects the evaluated frame.
                                let mut rec = recorder_worker_clone.lock().unwrap();
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis()
                                    as u64;

                                let dynamic_resolution_text =
                                    if (intercept.sanitized_scalar - demand).abs() > 0.001 {
                                        "MUTATED_CLAMP"
                                    } else {
                                        "TRANSPARENT"
                                    };

                                let system_state_enum = match trust_mode {
                                    TrustMode::FullAutonomy => GlobalSystemState::Normal,
                                    _ => GlobalSystemState::Degraded,
                                };

                                rec.log(crate::kirra_core::JournalLogEntry {
                                    ts: now_ms,
                                    actor: "NETWORK_PROXY",
                                    token: "SESSION_TOKEN",
                                    action: "MODBUS_WRITE",
                                    res: dynamic_resolution_text,
                                    state: system_state_enum,
                                    mode: trust_mode,
                                    score: trust_score,
                                    narrative: intercept.mitigation.to_string(),
                                });
                                flush_payload = Some(rec.clone());
                            }
                            adapter_clone.encode_response(intercept.sanitized_scalar, raw_frame)
                        }
                        Err(e) => {
                            let code = match e {
                                crate::AdapterError::UnmonitoredRegisterTarget => 0x02,
                                _ => 0x04,
                            };
                            adapter_clone.encode_exception(raw_frame, code)
                        }
                    };

                    if let Some(records_data) = flush_payload {
                        let _io_guard = io_lock_worker.lock().unwrap();
                        let _ =
                            save_replay_json(&records_data, &local_log_dir, "kirra_replay.json");
                    }

                    if plc_socket.write_all(&out_bytes).is_err() {
                        break;
                    }
                    if Self::read_exact_frame(&mut plc_socket, 6, &mut plc_buf[0..6]).is_err() {
                        break;
                    }
                    let p_len = u16::from_be_bytes([plc_buf[4], plc_buf[5]]) as usize;
                    // B2 (fail-closed): bound the PLC-declared length BEFORE slicing
                    // `plc_buf[6..6+p_len]`. `plc_buf` is `[0u8; 512]`, so a length
                    // `> 506` would index past the end → panic → (release `panic=abort`)
                    // process kill from a single malformed/hostile PLC response. This
                    // mirrors the identical client-request guard at line 227; without
                    // it the response path was the one unbounded slice in the proxy.
                    if p_len == 0 || p_len > 500 {
                        break;
                    }
                    if Self::read_exact_frame(&mut plc_socket, p_len, &mut plc_buf[6..6 + p_len])
                        .is_err()
                    {
                        break;
                    }
                    if mut_client.write_all(&plc_buf[0..6 + p_len]).is_err() {
                        break;
                    }
                }

                let summary_payload = {
                    if let Ok(st) = gov_worker_clone.lock() {
                        let gov = &st.governor;
                        let total_traffic =
                            metrics_clone.total_processed_frames.load(Ordering::Relaxed) as u32;
                        let clamp_events = metrics_clone
                            .envelope_clamping_events
                            .load(Ordering::Relaxed)
                            as u32;
                        let rate_events =
                            metrics_clone.rate_limiting_events.load(Ordering::Relaxed) as u32;
                        Some(ExecutiveSummary {
                            processed_traffic_count: total_traffic,
                            attempted_unsafe_actions: clamp_events,
                            policy_enforced_actions: clamp_events + rate_events,
                            rate_limited_actions: rate_events,
                            final_trust_mode: gov.trust_mode(),
                            asset_in_safe_control_state: gov.trust_mode()
                                == TrustMode::FullAutonomy,
                            final_process_value_counts: gov.last_output(),
                        })
                    } else {
                        None
                    }
                };

                if let Some(summary) = summary_payload {
                    let _io_guard = io_lock_worker.lock().unwrap();
                    let _ = save_summary_json(&summary, &local_log_dir, "kirra_summary.json");
                }
            });
        }
    }

    pub fn spawn_mock_plc_target(&self, ready_tx: Sender<Result<(), String>>) {
        let port = self.plc_target_port;
        thread::spawn(move || {
            let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)) {
                Ok(l) => {
                    let _ = ready_tx.send(Ok(()));
                    l
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            for mut socket in listener.incoming().flatten() {
                let mut buf = [0u8; 512];
                while Self::read_exact_frame(&mut socket, 6, &mut buf[0..6]).is_ok() {
                    let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                    if Self::read_exact_frame(&mut socket, len, &mut buf[6..6 + len]).is_ok() {
                        let _ = socket.write_all(&buf[0..6 + len]);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod governed_dt_tests {
    use super::{governed_dt_secs, MAX_GOVERNED_DT_S, NOMINAL_CONTROL_PERIOD_S};
    use std::time::Duration;

    #[test]
    fn first_frame_uses_nominal_period() {
        // B4: no prior sample → nominal period (legacy first-frame behaviour), NOT
        // a zero/garbage dt.
        assert_eq!(governed_dt_secs(None), NOMINAL_CONTROL_PERIOD_S);
    }

    #[test]
    fn normal_interval_is_passed_through_as_real_dt() {
        // A genuine 200 ms gap is reported as 0.2 s — not the fabricated 0.050.
        let dt = governed_dt_secs(Some(Duration::from_millis(200)));
        assert!((dt - 0.200).abs() < 1e-9, "got {dt}");
    }

    #[test]
    fn small_real_dt_is_kept_not_floored() {
        // A fast 5 ms frame stays 0.005 s, so a large step over it correctly reads
        // as a high rate (conservative) instead of being under-counted.
        let dt = governed_dt_secs(Some(Duration::from_millis(5)));
        assert!((dt - 0.005).abs() < 1e-9, "got {dt}");
    }

    #[test]
    fn long_idle_is_capped_so_rate_limit_survives() {
        // A 30 s idle must not grant an unbounded `max_rate * dt` single step.
        let dt = governed_dt_secs(Some(Duration::from_secs(30)));
        assert_eq!(dt, MAX_GOVERNED_DT_S);
    }

    #[test]
    fn exactly_zero_elapsed_yields_zero_dt_for_evaluate_to_failclose() {
        // A same-instant repeat → 0.0; `evaluate` fail-closes on dt <= 0 (Gov-M2),
        // so this must NOT be silently bumped to a positive fabricated value.
        assert_eq!(governed_dt_secs(Some(Duration::ZERO)), 0.0);
    }
}
