//! ENGINE-CACHE / WARM-UP PROBE — PARK-021 jetson-gated item #2 (engine build /
//! cache + warm-up), with a slice of #5 (perf). TensorRT builds a per-model/shape
//! engine on first use (slow), then caches it at `engine_cache_path`. The safety
//! concern: that multi-second build must NOT land on the first real command — a
//! startup warm-up has to pay it up front. This probe MEASURES the cost the
//! warm-up must hide, and confirms the on-disk cache actually short-circuits it:
//!
//!   * COLD: wipe the cache dir, then time `with_config` + first inference
//!     (includes the TRT engine build).
//!   * WARM: time a SECOND `with_config` + inference against the now-populated
//!     cache (engine deserialized, no rebuild).
//!
//! It also fingerprints the cached engine file (size + a stable content hash) —
//! the hook where the backend's `TrtPosture.engine_sha` (SHA-256) gets populated
//! once an engine exists (the real engine_sha is a backend follow-up; this probe
//! uses a non-cryptographic fingerprint to prove the artifact is stable).
//!
//! Honest scope: these are HOST-INDICATIVE timings on the Jetson, NOT a WCET/FTTI
//! claim. They size the warm-up budget and confirm cache reuse — nothing more.
//!
//! GATING — same as the other probes: self-skips wherever the TRT EP is
//! unavailable; `PARKO_TRT_REQUIRE_EP=1` makes the skip a hard failure. Inert in
//! the GPU-less sandbox / CPU-only CI job. Run on the Jetson with:
//!
//!   ORT_DYLIB_PATH=<venv>/…/libonnxruntime.so.1.23.0 \
//!     cargo test -p parko-tensorrt --test engine_cache_probe -- --nocapture

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::{Duration, Instant};

use parko_core::backend::{BackendError, InferenceBackend, TensorBatch, TensorStorage};
use parko_tensorrt::{TrtBackend, TrtConfig};

const MODEL: &str = "tests/data/mnist-12.onnx";
const INPUT_NAME: &str = "Input3";
const OUTPUT_NAME: &str = "Plus214_Output_0";
/// MNIST fixture input is [1,1,28,28] (asserted in positive_probe.rs).
const TOTAL: usize = 1 * 1 * 28 * 28;

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn argmax(scores: &[f32]) -> usize {
    scores
        .iter()
        .enumerate()
        .fold(0usize, |best, (i, &s)| if s > scores[best] { i } else { best })
}

/// Deterministic NON-zero input (matches the other probes).
fn deterministic_input(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i * 7 + 13) % 251) as f32 / 251.0).collect()
}

/// Construct a TRT backend against `cache_dir`, load the model, run one inference,
/// and return (logits, elapsed-wall-time) — the elapsed time spans `with_config`
/// (where the engine builds or deserializes) through the first `run`.
fn timed_build_and_run(cache_dir: &Path, input: &[f32]) -> Result<(Vec<f32>, Duration), BackendError> {
    let cfg = TrtConfig { engine_cache_path: cache_dir.to_string_lossy().into_owned() };
    let t0 = Instant::now();
    let trt = TrtBackend::with_config(MODEL, &cfg)?;
    let model = trt.load_model(MODEL)?;
    let mut named = HashMap::new();
    named.insert(INPUT_NAME.to_string(), TensorStorage::Borrowed(input));
    let batch = TensorBatch { named_tensors: named, metadata: HashMap::new() };
    let out = trt.run(&model, &batch)?;
    let elapsed = t0.elapsed();
    let logits = out
        .named_tensors
        .get(OUTPUT_NAME)
        .ok_or_else(|| BackendError::InitializationError("missing MNIST output".into()))?
        .as_slice()
        .to_vec();
    Ok((logits, elapsed))
}

/// (filename, size_bytes, non-crypto content fingerprint) of the largest file in
/// the cache dir — the serialized TRT engine. Stand-in for `TrtPosture.engine_sha`.
fn engine_cache_fingerprint(dir: &Path) -> Option<(String, u64, u64)> {
    let mut best: Option<(std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if let Ok(md) = entry.metadata() {
            if md.is_file() && best.as_ref().map_or(true, |(_, b)| md.len() > *b) {
                best = Some((entry.path(), md.len()));
            }
        }
    }
    let (path, size) = best?;
    let bytes = std::fs::read(&path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some((path.file_name()?.to_string_lossy().into_owned(), size, h.finish()))
}

#[test]
fn trt_engine_cache_cold_vs_warm() {
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?}) — \
             refusing (fail-closed).",
        );
        eprintln!("SKIP: ORT runtime lib not present ({dylib:?}) — engine-cache probe needs a Jetson/TRT ORT.");
        return;
    }

    let cache = std::env::temp_dir().join("parko_trt_engine_cache_probe");
    let input = deterministic_input(TOTAL);

    // COLD: guarantee no prior engine, then time build + first inference.
    let _ = std::fs::remove_dir_all(&cache);
    let (cold_logits, cold_dur) = match timed_build_and_run(&cache, &input) {
        Ok(x) => x,
        Err(e) => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable / cold build failed ({e:?}) — \
                 refusing (fail-closed).",
            );
            eprintln!("SKIP: TensorRT EP unavailable / cold build failed ({e:?}) — Jetson/TRT ORT only.");
            return;
        }
    };

    // The cache must now hold a built engine — that is the #2 artifact.
    let fingerprint = engine_cache_fingerprint(&cache);
    assert!(
        fingerprint.is_some(),
        "engine cache dir {cache:?} is empty after a cold build — engine caching did not persist",
    );

    // WARM: same populated cache, fresh backend — should deserialize, not rebuild.
    let (warm_logits, warm_dur) = timed_build_and_run(&cache, &input)
        .expect("warm build/run failed though the cold build succeeded");

    // Correctness: both finite, and the cached engine reproduces the decision.
    for (i, s) in cold_logits.iter().enumerate() {
        assert!(s.is_finite(), "non-finite cold score at class {i}: {s}");
    }
    for (i, s) in warm_logits.iter().enumerate() {
        assert!(s.is_finite(), "non-finite warm score at class {i}: {s}");
    }
    let (d_cold, d_warm) = (argmax(&cold_logits), argmax(&warm_logits));
    assert_eq!(
        d_cold, d_warm,
        "cold and warm must reach the same decision (cold→{d_cold}, warm→{d_warm})",
    );

    // The cache must not make startup SLOWER (engine reuse vs rebuild). The cold
    // path includes a multi-second engine build, so this holds with wide margin.
    assert!(
        warm_dur <= cold_dur,
        "warm startup ({warm_dur:?}) should not exceed cold ({cold_dur:?}) — cache reuse regressed",
    );

    let cold_ms = cold_dur.as_secs_f64() * 1000.0;
    let warm_ms = warm_dur.as_secs_f64() * 1000.0;
    let speedup = if warm_ms > 0.0 { cold_ms / warm_ms } else { f64::INFINITY };
    let max_diff = cold_logits
        .iter()
        .zip(&warm_logits)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);

    match fingerprint {
        Some((name, size, hash)) => println!(
            "ENGINE-CACHE PROBE — cold(build+run) {cold_ms:.1} ms vs warm(cached) {warm_ms:.1} ms \
             (≈{speedup:.1}× faster warm). The warm-up must hide ≈{:.0} ms of cold build. \
             Engine cache file '{name}' {size} B, fingerprint {hash:016x} (→ TrtPosture.engine_sha hook). \
             Decision class {d_cold}; cold/warm max logit diff {max_diff:e}.",
            cold_ms - warm_ms,
        ),
        None => unreachable!("fingerprint asserted Some above"),
    }
    println!("NOTE: host-indicative Jetson timings — sizes the warm-up budget; NOT a WCET/FTTI claim.");
}
