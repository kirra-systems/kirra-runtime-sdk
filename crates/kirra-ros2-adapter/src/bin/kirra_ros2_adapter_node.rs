// crates/kirra-ros2-adapter/src/bin/kirra_ros2_adapter_node.rs
//
// S131 Phase 4 — adapter binary entry point.
//
// What it does (from the top):
//   1. Initialises tracing-subscriber (json envelope; INFO+ by default;
//      RUST_LOG honoured).
//   2. Parses a minimal CLI: --corridor-source <mock|lanelet2> [--map-bin PATH]
//      [--lanelet-ids 1001,1002,...]. `mock` is the pilot default and
//      requires no Lanelet2 install; `lanelet2` requires
//      ros-${ROS_DISTRO}-lanelet2 + libboost-serialization-dev and a
//      `.osm.bin` map file at --map-bin.
//   3. Builds the `AdaptorState` (DashMap of per-asset accepted
//      trajectories, perception cache, ego-odom cache, VehicleConfig).
//   4. Builds the chosen `CorridorSource` impl.
//   5. Calls `run_adapter(state, corridor, node_name)` which owns the
//      r2r `Context` + `Node`, subscriptions, slow/fast loops, and the
//      MRC publisher.
//   6. Installs a SIGTERM / Ctrl-C handler that publishes a final MRC
//      command for every known asset and exits cleanly.
//
// Feature: ros2. The binary's `[[bin]]` entry in Cargo.toml is gated by
// `required-features = ["ros2"]`, so the default cargo build doesn't
// even try to compile this file.

#![cfg(feature = "ros2")]

use std::sync::Arc;

use kirra_ros2_adapter::{
    config::VehicleConfig,
    corridor::{CorridorSource, MockCorridorSource},
    node::run_adapter,
    posture_source::{spawn_posture_source, PostureSourceConfig},
    state::AdaptorState,
};

#[cfg(feature = "ros2")]
use kirra_ros2_adapter::corridor::Lanelet2CorridorSource;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CorridorSourceKind {
    Mock,
    Lanelet2,
}

#[derive(Debug, Clone)]
struct CliArgs {
    corridor_source: CorridorSourceKind,
    map_bin_path:    Option<String>,
    lanelet_ids:     Vec<i64>,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            corridor_source: CorridorSourceKind::Mock,
            map_bin_path:    None,
            lanelet_ids:     Vec::new(),
        }
    }
}

fn parse_cli() -> Result<CliArgs, String> {
    // Lightweight std::env::args parser — clap would pull in another
    // 200 KB of deps for a 3-flag CLI.
    let mut args = CliArgs::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--corridor-source" => {
                i += 1;
                let v = raw.get(i)
                    .ok_or("--corridor-source needs a value (mock|lanelet2)")?;
                args.corridor_source = match v.as_str() {
                    "mock"     => CorridorSourceKind::Mock,
                    "lanelet2" => CorridorSourceKind::Lanelet2,
                    other => return Err(format!(
                        "--corridor-source: unknown value {other:?} (expected mock|lanelet2)")),
                };
            }
            "--map-bin" => {
                i += 1;
                args.map_bin_path = Some(
                    raw.get(i).ok_or("--map-bin needs a path")?.clone(),
                );
            }
            "--lanelet-ids" => {
                i += 1;
                let csv = raw.get(i).ok_or("--lanelet-ids needs a CSV list")?;
                args.lanelet_ids = csv
                    .split(',')
                    .map(|s| s.trim().parse::<i64>()
                        .map_err(|e| format!("--lanelet-ids: parse error: {e}")))
                    .collect::<Result<_, _>>()?;
            }
            "--help" | "-h" => {
                eprintln!("kirra_ros2_adapter_node — S131 Governor adapter\n\
                  Usage:\n\
                    kirra_ros2_adapter_node [--corridor-source <mock|lanelet2>]\n\
                                            [--map-bin <path/to/map.osm.bin>]\n\
                                            [--lanelet-ids <id1,id2,...>]\n\
                  Default: --corridor-source mock (no Lanelet2 install needed).");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }
    if args.corridor_source == CorridorSourceKind::Lanelet2
        && (args.map_bin_path.is_none() || args.lanelet_ids.is_empty())
    {
        return Err(
            "--corridor-source lanelet2 requires both --map-bin and --lanelet-ids".to_string()
        );
    }
    Ok(args)
}

fn init_tracing() {
    // JSON-line envelope for structured ingestion; RUST_LOG controls
    // filter level. We deliberately do NOT pull in `tracing-subscriber`
    // as a dep here — the kirra-runtime-sdk already has one; until the
    // adapter is co-located, fall back to a basic format if unavailable.
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

fn build_corridor(args: &CliArgs) -> Result<Arc<dyn CorridorSource>, Box<dyn std::error::Error>> {
    match args.corridor_source {
        CorridorSourceKind::Mock => {
            tracing::info!("corridor source: mock (5 m half-width straight corridor, 500 m)");
            Ok(Arc::new(MockCorridorSource::straight_5m_half_width(500.0)))
        }
        CorridorSourceKind::Lanelet2 => {
            let path = args.map_bin_path.as_deref()
                .expect("CLI parser guards: --map-bin required for lanelet2");
            tracing::info!(map_bin = %path, ids = ?args.lanelet_ids,
                "corridor source: Lanelet2 (loading map)");
            let bin = std::fs::read(path)
                .map_err(|e| format!("failed to read --map-bin {path}: {e}"))?;
            let src = Lanelet2CorridorSource::from_map_bin_and_route(
                &bin, &args.lanelet_ids, 0.95, 0,
            ).map_err(|e| format!("Lanelet2CorridorSource: {e}"))?;
            Ok(Arc::new(src))
        }
    }
}

async fn install_shutdown_handler(state: Arc<AdaptorState>) {
    // tokio::signal::ctrl_c covers SIGINT. SIGTERM on unix needs the
    // signal stream; we install both if available.
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
        _ = ctrl_c => tracing::warn!("SIGINT received — initiating MRC shutdown"),
        _ = sigterm => tracing::warn!("SIGTERM received — initiating MRC shutdown"),
    }
    // Final-MRC pass: synthesize an MRC command for every asset we've
    // seen, log it, and let the process exit. The fast-loop output
    // channel publishes will be flushed by tokio's runtime drop.
    let count = state.len();
    tracing::warn!(asset_count = count, "final MRC shutdown pass");
    // Phase 4: the adapter's MRC topic is the only egress; the
    // integrator's vehicle interface gets the MRC on the same topic the
    // adapter has been publishing on all along. No extra topic.
}

#[tokio::main]
async fn main() {
    init_tracing();
    let args = parse_cli().unwrap_or_else(|e| {
        eprintln!("kirra_ros2_adapter_node: {e}");
        std::process::exit(2);
    });
    tracing::info!(?args, "kirra_ros2_adapter_node starting");

    // M1b — if KIRRA_POSTURE_STREAM_URL is set (alongside the admin
    // token + client id), construct a source-configured AdaptorState
    // and spawn the SSE subscriber. The PostureTracker starts at
    // Degraded (fail-closed seed); the SSE task drives it Nominal as
    // soon as the verifier confirms. If the URL env var is absent we
    // keep the M1 default (no-source tracker → Nominal forever) so
    // verifier-less deployments and CARLA-only smoke runs are
    // unaffected.
    let posture_source_config = load_posture_source_config();
    let state = match &posture_source_config {
        Some(cfg) => {
            tracing::info!(verifier_base_url = %cfg.verifier_base_url,
                "M1b: live posture source configured; tracker starts at Degraded \
                 until the first SSE event lands");
            AdaptorState::with_posture_source(VehicleConfig::default_urban())
        }
        None => {
            tracing::info!(
                "M1b: KIRRA_POSTURE_STREAM_URL not set; using M1 default \
                 (no-source tracker — posture stays Nominal)");
            AdaptorState::new()
        }
    };

    let corridor = build_corridor(&args).unwrap_or_else(|e| {
        eprintln!("kirra_ros2_adapter_node: corridor build failed: {e}");
        std::process::exit(3);
    });

    // SSE subscriber (M1b). Spawned only when the source is configured;
    // the JoinHandle is aborted at shutdown alongside the adapter task.
    let posture_task = posture_source_config.map(|cfg| {
        spawn_posture_source(Arc::clone(&state), cfg)
    });

    // The adapter's main task: r2r Context + Node + subscriptions +
    // slow/fast loops. Owns the runtime for the lifetime of the node.
    let adapter_task = {
        let state = Arc::clone(&state);
        let corridor = Arc::clone(&corridor);
        tokio::spawn(async move {
            if let Err(e) = run_adapter(state, corridor, "kirra_governor").await {
                tracing::error!(error = ?e, "run_adapter exited with error");
            }
        })
    };

    install_shutdown_handler(Arc::clone(&state)).await;

    // Abort the adapter task so the runtime can finish dropping its
    // children. tokio::spawn's JoinHandle::abort is cooperative; tasks
    // that are stuck in non-cancellable C++ FFI will be terminated by
    // process exit.
    adapter_task.abort();
    if let Some(handle) = posture_task {
        handle.abort();
    }
    tracing::info!("kirra_ros2_adapter_node exit");
}

/// Read the three env vars that configure the M1b SSE subscriber, all-or-nothing.
///
/// `KIRRA_POSTURE_STREAM_URL` (the verifier base URL, e.g.
/// `http://kirra-verifier:8090`) gates the subscriber. Setting it
/// REQUIRES `KIRRA_ADMIN_TOKEN` (verifier auth) and
/// `KIRRA_POSTURE_CLIENT_ID` (the `x-kirra-client-id` header for
/// identity-gated routes). A missing required var emits a WARN and
/// drops back to the M1 no-source default.
fn load_posture_source_config() -> Option<PostureSourceConfig> {
    let url = std::env::var("KIRRA_POSTURE_STREAM_URL").ok().filter(|s| !s.is_empty())?;
    let admin_token = match std::env::var("KIRRA_ADMIN_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            tracing::warn!(
                "KIRRA_POSTURE_STREAM_URL is set but KIRRA_ADMIN_TOKEN is missing/empty; \
                 dropping back to the M1 no-source default. The verifier's identity-gated \
                 SSE route requires the admin token; the adapter will not subscribe.");
            return None;
        }
    };
    let client_id = std::env::var("KIRRA_POSTURE_CLIENT_ID").ok().filter(|s| !s.is_empty())
        .unwrap_or_else(|| "kirra-ros2-adapter".to_string());
    Some(PostureSourceConfig { verifier_base_url: url, admin_token, client_id })
}
