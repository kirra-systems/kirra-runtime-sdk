// parko/crates/parko-ros2/src/bin/parko_ros2_node.rs
//
// M2 — Parko ROS 2 node binary entry point. Feature-gated on `ros2`.
// The `[[bin]]` entry in `Cargo.toml` is `required-features = ["ros2"]`,
// so the default cargo build does not even try to compile this file.
//
// What it does:
//   1. `init_tracing()` — JSON envelope, INFO+ by default, RUST_LOG honoured.
//   2. Resolves `ParkoNodeConfig` from env / defaults.
//   3. Builds an `InferenceLoop` with a `GovernorComparator` attached.
//      Backend selection:
//        - if the `onnx-backend` feature is enabled AND
//          `PARKO_MODEL_PATH` is set → parko-onnx OrtBackend
//          (production path; requires `ORT_DYLIB_PATH`).
//        - otherwise → MockBackend with a hardcoded zero output
//          (development / smoke path; logs an explicit WARN so the
//          operator can never confuse it with production).
//   4. Starts `parko_ros2::node::run_node`.
//   5. Installs the SIGINT / SIGTERM shutdown handler.

#![cfg(feature = "ros2")]

use std::sync::Arc;

use parko_core::backend::InferenceBackend;
use parko_core::commands::ControlCommand;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use parko_kirra::{
    select_divergence_sink, DiverseKirraGovernor, GovernorComparator, KirraGovernor,
};
use parko_ros2::{
    clearance_gate::NodeClearance,
    comparator_adapter::ComparatorAsGovernor,
    config::ParkoNodeConfig,
    node::run_node,
    platform_profile::CourierPlatformProfile,
    sensor_mapping::VectorMapping,
};
use tokio::sync::{mpsc, Mutex};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

/// CERT-006: resolve the comparator divergence sink from the environment,
/// fail-closed.
///
///   - `PARKO_DIVERGENCE_AUDIT_DB` — SQLite path for the durable, hash-chained
///     audit ledger. Unset → divergences are buffered in memory only
///     (ephemeral; NOT certification-grade) and a loud WARN is emitted.
///   - `KIRRA_LOG_SIGNING_KEY` — base64 Ed25519 signing key. REQUIRED when the
///     audit DB is set; the durable chain must be signed to be tamper-evident.
///
/// If a durable audit was requested but cannot be made signed + persistent
/// (missing/invalid key, or the store will not open), that is a FATAL
/// misconfiguration: the node exits non-zero rather than run with divergences
/// going unaudited while the operator believes they are captured.
fn build_divergence_sink() -> Arc<dyn parko_kirra::comparator::DivergenceEventSink> {
    let db = std::env::var("PARKO_DIVERGENCE_AUDIT_DB").ok();
    let key = std::env::var("KIRRA_LOG_SIGNING_KEY").ok();

    let durable_requested = db.as_deref().is_some_and(|s| !s.is_empty());

    match select_divergence_sink(db, key) {
        Ok(sink) => {
            if durable_requested {
                tracing::info!(
                    "parko-ros2: CERT-006 comparator divergences → durable, signed audit chain \
                     (PARKO_DIVERGENCE_AUDIT_DB)"
                );
            } else {
                tracing::warn!(
                    "parko-ros2: PARKO_DIVERGENCE_AUDIT_DB is unset — comparator divergences are \
                     buffered IN MEMORY ONLY and are LOST on restart. This is NOT a \
                     certification-grade configuration; set PARKO_DIVERGENCE_AUDIT_DB and \
                     KIRRA_LOG_SIGNING_KEY for a durable, signed divergence record."
                );
            }
            sink
        }
        Err(e) => {
            tracing::error!(
                "parko-ros2: FATAL — cannot configure the CERT-006 divergence audit sink: {e}. \
                 A durable audit was requested but cannot be made signed + persistent. Refusing \
                 to start with divergences unaudited."
            );
            std::process::exit(1);
        }
    }
}

/// Resolve the ONE co-located store path (#304). The node and the vehicle's
/// verifier share a single SQLite store; this reads `KIRRA_DB_PATH` first and
/// falls back to the divergence sink's `PARKO_DIVERGENCE_AUDIT_DB`. If BOTH are
/// set and DIFFER, that is a co-location error — warn once and prefer
/// `KIRRA_DB_PATH` (the verifier's canonical var). See docs/CONSOLE_RUNBOOK.md
/// "On the vehicle".
fn resolve_one_store_db() -> Option<String> {
    let kirra = std::env::var("KIRRA_DB_PATH").ok().filter(|s| !s.is_empty());
    let divergence = std::env::var("PARKO_DIVERGENCE_AUDIT_DB").ok().filter(|s| !s.is_empty());
    if let (Some(k), Some(d)) = (&kirra, &divergence) {
        if k != d {
            tracing::warn!(
                kirra_db = %k, divergence_db = %d,
                "parko-ros2: KIRRA_DB_PATH and PARKO_DIVERGENCE_AUDIT_DB are BOTH set and DIFFER. \
                 The co-located node + verifier are ONE store (one vehicle, one SQLite file) — \
                 using KIRRA_DB_PATH. Point both env vars at the SAME file."
            );
        }
    }
    kirra.or(divergence)
}

/// Build the node-owned clearance gate (#304 Phase-B), mirroring the divergence
/// sink's env-gated warn-and-continue shape.
///
///   - the store path (`KIRRA_DB_PATH`, or `PARKO_DIVERGENCE_AUDIT_DB`) AND
///     `KIRRA_LOG_SIGNING_KEY` present → delivery is REQUESTED.
///   - `KIRRA_NODE_ID` — REQUIRED when delivery is requested. There is NO safe
///     default: a wrong-node grant pickup must be impossible. Missing → FATAL.
///   - either env input missing → delivery DISABLED (the dev lane): warn naming
///     what is off (console grants will not be delivered) and return `None`.
///
/// The node NEVER reads `KIRRA_SUPERVISOR_RESET_KEY` — operator authentication is
/// the SERVICE's job (the #255 grant form); the node only delivers what the
/// verifier already recorded + signed.
fn build_node_clearance(config: &ParkoNodeConfig) -> Option<NodeClearance> {
    let db = resolve_one_store_db();
    let key = std::env::var("KIRRA_LOG_SIGNING_KEY").ok().filter(|s| !s.is_empty());

    let (db, key) = match (db, key) {
        (Some(db), Some(key)) => (db, key),
        _ => {
            tracing::warn!(
                "parko-ros2: clearance delivery DISABLED — set KIRRA_DB_PATH (the one co-located \
                 store), KIRRA_LOG_SIGNING_KEY, and KIRRA_NODE_ID to enable it. Console-recorded \
                 operator clearance grants will NOT be delivered to this node."
            );
            return None;
        }
    };

    // Delivery requested → the node id is mandatory and has no default.
    let node_id = match std::env::var("KIRRA_NODE_ID").ok().filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => {
            tracing::error!(
                "parko-ros2: FATAL — clearance delivery requested (KIRRA_DB_PATH + \
                 KIRRA_LOG_SIGNING_KEY set) but KIRRA_NODE_ID is unset/empty. There is no safe \
                 default node id — a wrong-node grant pickup must be impossible. Set KIRRA_NODE_ID \
                 to THIS vehicle's node id."
            );
            std::process::exit(1);
        }
    };

    match NodeClearance::open_signed(&db, &node_id, &key) {
        Ok(c) => {
            tracing::info!(
                node_id = %node_id, db = %db,
                spike_threshold_mps2 = config.spike_threshold_mps2,
                "parko-ros2: clearance delivery ENABLED — console-recorded operator grants are \
                 delivered on this node's own tick (poll_and_deliver per tick); the post-collision \
                 loop holds the vehicle stopped until a grant clears it. SG6 detection armed per \
                 the configured impact threshold (see the IMU/contact ARMED lines)."
            );
            // #309: thread the (deployment-tunable) impact-fusion config into the gate.
            let mut c = c.with_impact_cfg(config.impact_cfg());
            // #324: arm the IMU staleness watchdog ONLY when an IMU source is
            // configured — a silent/stale configured IMU then forces the MRC, while
            // an unconfigured IMU stays reduced-coverage (no forced stop).
            if config.imu_topic.is_some() {
                c = c.with_imu_staleness(config.imu_staleness_window_ms);
                tracing::info!(
                    window_ms = config.imu_staleness_window_ms,
                    "parko-ros2: SG6 IMU staleness watchdog ARMED (#324) — a silent/stale IMU \
                     forces the MRC (stop) until fresh samples resume."
                );
            }
            Some(c)
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "parko-ros2: FATAL — clearance delivery requested but the co-located store could \
                 not be opened / the signing key is invalid. Refusing to start with delivery \
                 silently broken."
            );
            std::process::exit(1);
        }
    }
}

fn build_loop<B>(
    backend: Arc<B>,
    model_path: &str,
    tick_period_s: f64,
    platform_profile: Option<&CourierPlatformProfile>,
) -> Arc<Mutex<InferenceLoop<B>>>
where
    B: InferenceBackend + 'static,
{
    let model = backend.load_model(model_path)
        .unwrap_or_else(|e| {
            eprintln!("parko_ros2_node: backend.load_model({model_path}) failed: {e:?}");
            std::process::exit(3);
        });

    // PARK-021 #2: warm up the backend BEFORE the loop serves any command, so a
    // multi-second first-use cost (e.g. the TensorRT engine build) never lands on
    // the first real command. No-op for backends that need no warm-up (mock, CPU
    // ORT). FAIL-CLOSED: if warm-up fails the backend is not ready, so refuse to
    // start rather than serve against an unbuilt/cold engine.
    backend.warm_up(&model)
        .unwrap_or_else(|e| {
            eprintln!("parko_ros2_node: backend.warm_up failed — refusing to start (fail-closed): {e:?}");
            std::process::exit(4);
        });

    let (actuator_tx, mut actuator_rx) = mpsc::channel::<ControlCommand>(8);

    // The scheduler's actuator_tx receives the post-governor command
    // internally; the node's drain task additionally publishes the
    // command via `OutgoingTwist`. We drain the rx in the background
    // to keep the channel from filling and back-pressuring the loop.
    // (The drain task is fire-and-forget; we log every Nth command for
    // observability.)
    tokio::spawn(async move {
        let mut n: u64 = 0;
        while let Some(cmd) = actuator_rx.recv().await {
            n = n.wrapping_add(1);
            if n.is_multiple_of(50) {
                tracing::debug!(?cmd, "parko-ros2: actuator_tx drain — sample command");
            }
        }
    });

    // CERT-006: diverse redundancy — primary KirraGovernor paired with a
    // structurally diverse DiverseKirraGovernor shadow, so the comparator can
    // catch implementation-level systematic faults, not just random ones. Every
    // divergence is routed to the selected sink (durable+signed if configured).
    //
    // ADR-0029 Phase 2: when a courier platform profile is configured, BOTH
    // comparator arms take the courier's SOTIF angular params — so they agree
    // by construction (a mismatched shadow would spuriously diverge on in-place
    // rotation between the two bounds and escalate the posture). With no
    // profile, both keep KirraGovernor/DiverseKirraGovernor `::new()` — the
    // conservative default, byte-identical to pre-Phase-2.
    let comparator = match platform_profile {
        Some(profile) => GovernorComparator::with_sink(
            profile.angular_governor(),
            DiverseKirraGovernor::new().with_platform_params(profile.angular_params.clone()),
            build_divergence_sink(),
        ),
        None => GovernorComparator::with_sink(
            KirraGovernor::new(),
            DiverseKirraGovernor::new(),
            build_divergence_sink(),
        ),
    };
    let infer = InferenceLoop::new(backend, model, actuator_tx)
        .with_governor(ComparatorAsGovernor(comparator))
        .with_tick_period(tick_period_s);
    Arc::new(Mutex::new(infer))
}

async fn install_shutdown_handler() {
    use tokio::signal;
    let ctrl_c = signal::ctrl_c();
    #[cfg(unix)]
    let sigterm = async {
        use signal::unix::{signal, SignalKind};
        let mut s = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        s.recv().await;
    };
    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::warn!("parko-ros2: SIGINT received — initiating shutdown"),
        _ = sigterm => tracing::warn!("parko-ros2: SIGTERM received — initiating shutdown"),
    }
}

/// Build the node config from env, overriding the SG6 detection knobs (#309):
///   - `PARKO_IMU_TOPIC`     — `sensor_msgs/Imu` topic (decel trigger).
///   - `PARKO_CONTACT_TOPIC` — `std_msgs/Bool` topic (contact trigger).
///   - `PARKO_IMPACT_SPIKE_THRESHOLD_MPS2` — decel threshold (deployment-tunable,
///     VALIDATION-PENDING; parko-core default 30 m/s²). A non-numeric value is
///     ignored with a warning (the safe default stands).
fn build_config() -> ParkoNodeConfig {
    let imu_topic = std::env::var("PARKO_IMU_TOPIC").ok().filter(|s| !s.is_empty());
    let contact_topic = std::env::var("PARKO_CONTACT_TOPIC").ok().filter(|s| !s.is_empty());
    let mut config = ParkoNodeConfig {
        imu_topic,
        contact_topic,
        ..ParkoNodeConfig::default()
    };
    if let Ok(raw) = std::env::var("PARKO_IMPACT_SPIKE_THRESHOLD_MPS2") {
        match raw.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => config.spike_threshold_mps2 = v,
            _ => tracing::warn!(
                value = %raw,
                "parko-ros2: PARKO_IMPACT_SPIKE_THRESHOLD_MPS2 is not a positive finite number — \
                 keeping the default {} m/s²",
                config.spike_threshold_mps2
            ),
        }
    }
    // SG6 (#324): IMU staleness window override (only effective when imu_topic set).
    if let Ok(raw) = std::env::var("PARKO_IMU_STALENESS_WINDOW_MS") {
        match raw.trim().parse::<u64>() {
            Ok(v) if v > 0 => config.imu_staleness_window_ms = v,
            _ => tracing::warn!(
                value = %raw,
                "parko-ros2: PARKO_IMU_STALENESS_WINDOW_MS is not a positive integer — keeping the \
                 default {} ms",
                config.imu_staleness_window_ms
            ),
        }
    }
    config
}

#[tokio::main]
async fn main() {
    init_tracing();

    let config = Arc::new(build_config());
    tracing::info!(?config, "parko_ros2_node starting");

    // Pick the backend compiled into THIS build (compile-time feature gate, so a
    // misconfigured production build can't fall through to the mock). The mock lane
    // tolerates the dev sentinel; the onnx/tensorrt lanes require a real
    // PARKO_MODEL_PATH and FAIL-CLOSED if their runtime/EP is absent — never a
    // silent substitution. See `parko_ros2::backend_select`.
    let model_path = std::env::var("PARKO_MODEL_PATH")
        .unwrap_or_else(|_| "mock://development".to_string());
    tracing::info!(
        backend = parko_ros2::backend_select::SELECTED_BACKEND,
        %model_path,
        "parko-ros2: selected backend"
    );
    let backend = parko_ros2::backend_select::select_backend(&model_path)
        .unwrap_or_else(|e| {
            eprintln!(
                "parko_ros2_node: backend construction failed ({}) for {model_path:?} — refusing to \
                 start (fail-closed, no fallback): {e:?}",
                parko_ros2::backend_select::SELECTED_BACKEND,
            );
            std::process::exit(2);
        });
    let infer = build_loop(
        backend,
        &model_path,
        config.tick_period_s,
        config.platform_profile.as_ref(),
    );

    // M2 posture: static, defaults to Nominal. M1b's `PostureTracker`
    // is the reusable mechanism for live posture; the node task
    // accepts a `SafetyPosture` parameter so a follow-up can wire the
    // tracker without changing the public surface.
    let posture = SafetyPosture::Nominal;

    let mapping = Arc::new(VectorMapping::new("observation"));

    // #304 Phase-B: the node-owned clearance gate (delivery + the SG6 stop
    // tie-in). #309: armed with the live impact-detection config. `None` when
    // delivery is not configured (the dev lane).
    let clearance = build_node_clearance(&config);

    let run_task = {
        let config = Arc::clone(&config);
        let infer = Arc::clone(&infer);
        let mapping = Arc::clone(&mapping);
        tokio::spawn(async move {
            if let Err(e) = run_node(config, infer, mapping, posture, clearance, "parko_governor").await {
                tracing::error!(error = ?e, "parko-ros2: run_node exited with error");
            }
        })
    };

    install_shutdown_handler().await;

    run_task.abort();
    tracing::info!("parko_ros2_node exit");
}
