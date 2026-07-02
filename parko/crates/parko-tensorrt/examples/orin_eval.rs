//! Q-1b ON-TARGET EVAL RUNNER — the doer performance-contract matrix on the
//! Jetson Orin (`parko/QUANTIZATION_Q1_SCOPE.md` §2, Q-1b exit criteria).
//!
//! For each precision row (FP32 / FP16 / INT8-QDQ) it: builds the TensorRT
//! backend at that posture, warm-ups (engine build off the hot path), measures
//! p50/p99 latency with `parko_core::perf_contract::run_latency`, JOINS the
//! root-workspace scorecard's quality/admissibility (the cross-workspace file
//! seam), and evaluates the row against the performance contract.
//!
//! HONESTY NOTES
//! - Latency here is ON-TARGET but still host-indicative (pin/warm per the
//!   design note §4); it is not a WCET/FTTI claim.
//! - The FP16 row is LATENCY-ONLY: its quality/admissibility are not yet
//!   measured (the scorecard carries fp32 + int8-ptq rows), so it is reported
//!   informationally and NOT contract-evaluated.
//! - The contract thresholds default to `PerfContract::illustrative()` — a
//!   documented placeholder, overridable via `KIRRA_P99_BUDGET_NS`.
//!
//! GATING: where the TensorRT EP is unavailable this prints SKIPPED and exits 0;
//! `PARKO_TRT_REQUIRE_EP=1` makes that a nonzero exit (the Orin strict lane).
//!
//! Run on the Orin (after exporting artifacts in the root workspace):
//!
//!   ORT_DYLIB_PATH=<...>/libonnxruntime.so PARKO_TRT_REQUIRE_EP=1 \
//!     cargo run -p parko-tensorrt --example orin_eval

use std::collections::HashMap;

use parko_core::backend::{InferenceBackend, PrecisionMode, TensorBatch, TensorStorage};
use parko_core::perf_contract::{evaluate, run_latency, EvalRow, LatencyStats, PerfContract};
use parko_tensorrt::{TrtBackend, TrtConfig, TrtPrecision};
use serde::Deserialize;

/// Minimal mirror of the kirra-doer-eval scorecard wire format (Q1 scope §4,
/// seam A: the FILE is the contract; each side owns its own (de)serializer).
#[derive(Debug, Deserialize)]
struct Scorecard {
    schema_version: u32,
    rows: Vec<ScorecardRow>,
}

#[derive(Debug, Deserialize)]
struct ScorecardRow {
    label: String,
    admissibility_rate: f64,
    argmax_agreement_rate: f64,
}

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// SKIP (exit 0) or REFUSE (exit 1, strict) — never a silent pass.
fn skip_or_refuse(reason: &str) -> ! {
    if require_ep() {
        eprintln!("REFUSED (PARKO_TRT_REQUIRE_EP): {reason}");
        std::process::exit(1);
    }
    println!("SKIPPED: {reason}");
    std::process::exit(0);
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    // --- Inputs: artifacts dir (root-workspace export) + measurement knobs. ---
    let artifacts = std::env::var("KIRRA_DOER_ARTIFACTS").unwrap_or_else(|_| {
        // Default: the checked-in artifacts, resolved from this crate's location.
        format!("{}/../../../artifacts/doer-eval", env!("CARGO_MANIFEST_DIR"))
    });
    let iters = env_u64("KIRRA_EVAL_ITERS", 1000) as usize;
    let warmup = env_u64("KIRRA_EVAL_WARMUP", 100) as usize;
    let contract = PerfContract {
        p99_latency_budget_ns: env_u64(
            "KIRRA_P99_BUDGET_NS",
            PerfContract::illustrative().p99_latency_budget_ns,
        ),
        ..PerfContract::illustrative()
    };

    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        skip_or_refuse(&format!("no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?})"));
    }

    let card: Scorecard = {
        let path = format!("{artifacts}/scorecard.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("scorecard missing at {path}: {e} — run the root-workspace export"));
        serde_json::from_str(&raw).expect("valid scorecard JSON")
    };
    assert_eq!(card.schema_version, 1, "unknown scorecard schema version");
    let find = |label: &str| card.rows.iter().find(|r| r.label == label);

    // --- The matrix rows: (name, model file, posture, precision, scorecard join). ---
    let fp32_model = format!("{artifacts}/planner_fp32.onnx");
    let qdq_model = format!("{artifacts}/planner_int8_qdq.onnx");
    let rows: [(&str, &str, TrtPrecision, PrecisionMode, Option<&ScorecardRow>); 3] = [
        ("fp32", &fp32_model, TrtPrecision::Full, PrecisionMode::FP32, find("fp32")),
        // FP16: latency-only (no measured quality row yet) — informational.
        ("fp16", &fp32_model, TrtPrecision::Fp16, PrecisionMode::FP16, None),
        ("int8-qdq", &qdq_model, TrtPrecision::Int8Qdq, PrecisionMode::INT8, find("int8-ptq")),
    ];

    // The same featurized scene for every row (latency depends on shape, not values).
    let features = [0.25f32, 0.7, 0.36, 1.0];

    let mut fp32_eval_row: Option<EvalRow> = None;
    println!("== Q-1b doer eval matrix (iters={iters}, warmup={warmup}) ==");
    for (name, model_path, trt_precision, precision, joined) in rows {
        // Distinct engine cache per precision — engines are precision-specific.
        let cache = std::env::temp_dir().join(format!("kirra_orin_eval_cache_{name}"));
        let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };

        let backend = match TrtBackend::with_precision(model_path, &cfg, trt_precision) {
            Ok(b) => b,
            Err(e) => skip_or_refuse(&format!("TensorRT EP unavailable for row {name}: {e:?}")),
        };
        let model = backend.load_model(model_path).expect("introspect exported model");
        let report = backend.warm_up_report(&model).expect("engine build must succeed");

        let mut named = HashMap::new();
        named.insert("features".to_string(), TensorStorage::Borrowed(&features[..]));
        let batch = TensorBatch { named_tensors: named, metadata: HashMap::new() };
        let latency: LatencyStats =
            run_latency(&backend, &model, &batch, iters, warmup).expect("latency measurement");

        println!(
            "row {name:9} engine_build={}ms p50={}µs p99={}µs max={}µs engine_sha={}",
            report.warmed_ms,
            latency.p50_ns / 1_000,
            latency.p99_ns / 1_000,
            latency.max_ns / 1_000,
            report.engine_sha256.as_deref().unwrap_or("-"),
        );

        let Some(sc) = joined else {
            println!("row {name:9} INFORMATIONAL (latency-only: no measured quality row)");
            continue;
        };
        let eval_row = EvalRow {
            descriptor: backend.descriptor(),
            precision,
            model_id: model.model_id.clone(),
            latency,
            quality: sc.argmax_agreement_rate,
            admissibility: sc.admissibility_rate,
        };
        if name == "fp32" {
            fp32_eval_row = Some(eval_row.clone());
        }
        let reference = fp32_eval_row.as_ref().expect("fp32 row runs first");
        let verdict = evaluate(&eval_row, reference, &contract);
        println!(
            "row {name:9} contract {} (quality={:.3} admissibility={:.3})",
            if verdict.passed() { "PASS" } else { "FAIL" },
            eval_row.quality,
            eval_row.admissibility,
        );
        for f in &verdict.failures {
            println!("row {name:9}   failure: {f:?}");
        }
    }
    println!("== done — latency is on-target-indicative, NOT a WCET/FTTI claim ==");
}
