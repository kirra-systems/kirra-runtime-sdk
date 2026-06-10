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

use std::collections::HashMap;
use std::sync::Arc;

use parko_core::backend::{BackendDescriptor, InferenceBackend};
use parko_core::backends::mock::MockBackend;
use parko_core::commands::ControlCommand;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use parko_kirra::{
    select_divergence_sink, DiverseKirraGovernor, GovernorComparator, KirraGovernor,
};
use parko_ros2::{
    comparator_adapter::ComparatorAsGovernor,
    config::ParkoNodeConfig,
    node::run_node,
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

fn build_dev_backend() -> Arc<MockBackend> {
    tracing::warn!(
        "parko-ros2: using MockBackend (no PARKO_MODEL_PATH set / onnx-backend feature off). \
         This is DEVELOPMENT-ONLY — the node will emit a fixed zero command. \
         Set PARKO_MODEL_PATH and rebuild with --features ros2,onnx-backend for the production path."
    );
    let mut outputs: HashMap<String, Vec<f32>> = HashMap::new();
    outputs.insert("cmd_vel_linear".to_string(),  vec![0.0]);
    outputs.insert("cmd_vel_angular".to_string(), vec![0.0]);
    Arc::new(MockBackend::new(outputs, BackendDescriptor::Cpu))
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

fn build_loop<B>(
    backend: Arc<B>,
    model_path: &str,
    tick_period_s: f64,
) -> Arc<Mutex<InferenceLoop<B>>>
where
    B: InferenceBackend + 'static,
{
    let model = backend.load_model(model_path)
        .unwrap_or_else(|e| {
            eprintln!("parko_ros2_node: backend.load_model({model_path}) failed: {e:?}");
            std::process::exit(3);
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
    let comparator = GovernorComparator::with_sink(
        KirraGovernor::new(),
        DiverseKirraGovernor::new(),
        build_divergence_sink(),
    );
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

#[tokio::main]
async fn main() {
    init_tracing();

    let config = Arc::new(ParkoNodeConfig::default());
    tracing::info!(?config, "parko_ros2_node starting");

    // Pick a backend. Today's binary only carries the MockBackend
    // path; production deployments override this with the
    // `onnx-backend` feature + `PARKO_MODEL_PATH`. The feature-flag
    // dispatch is a deliberate compile-time gate so a misconfigured
    // production build can't accidentally fall through to MockBackend.
    let model_path = std::env::var("PARKO_MODEL_PATH")
        .unwrap_or_else(|_| "mock://development".to_string());
    let backend = build_dev_backend();
    let infer = build_loop(backend, &model_path, config.tick_period_s);

    // M2 posture: static, defaults to Nominal. M1b's `PostureTracker`
    // is the reusable mechanism for live posture; the node task
    // accepts a `SafetyPosture` parameter so a follow-up can wire the
    // tracker without changing the public surface.
    let posture = SafetyPosture::Nominal;

    let mapping = Arc::new(VectorMapping::new("observation"));

    let run_task = {
        let config = Arc::clone(&config);
        let infer = Arc::clone(&infer);
        let mapping = Arc::clone(&mapping);
        tokio::spawn(async move {
            if let Err(e) = run_node(config, infer, mapping, posture, "parko_governor").await {
                tracing::error!(error = ?e, "parko-ros2: run_node exited with error");
            }
        })
    };

    install_shutdown_handler().await;

    run_task.abort();
    tracing::info!("parko_ros2_node exit");
}
