//! WARM-UP FEATURE PROBE — PARK-021 #2. Exercises `TrtBackend::warm_up()` on real
//! silicon: it must force the engine build at startup, populate
//! `TrtPosture.engine_sha` with the engine's SHA-256, and be idempotent (a second
//! warm-up against the now-warm cache yields the SAME engine_sha).
//!
//! GATING — same as the other probes: self-skips wherever the TRT EP is
//! unavailable; `PARKO_TRT_REQUIRE_EP=1` makes the skip a hard failure. Inert in
//! the GPU-less sandbox / CPU-only CI job. Run on the Jetson with:
//!
//!   ORT_DYLIB_PATH=<venv>/…/libonnxruntime.so.1.23.0 \
//!     cargo test -p parko-tensorrt --test warmup_probe -- --nocapture

use parko_core::backend::InferenceBackend;
use parko_tensorrt::{TrtBackend, TrtConfig};

fn require_ep() -> bool {
    std::env::var("PARKO_TRT_REQUIRE_EP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[test]
fn trt_warm_up_builds_engine_and_populates_engine_sha() {
    let model_path = "tests/data/mnist-12.onnx";

    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if dylib.is_empty() || !std::path::Path::new(&dylib).exists() {
        assert!(
            !require_ep(),
            "STRICT (PARKO_TRT_REQUIRE_EP): no loadable ORT runtime at ORT_DYLIB_PATH ({dylib:?}) — \
             refusing (fail-closed).",
        );
        eprintln!("SKIP: ORT runtime lib not present ({dylib:?}) — warm-up probe needs a Jetson/TRT ORT.");
        return;
    }

    // Fresh cache dir so the first warm-up is a genuine COLD build.
    let cache = std::env::temp_dir().join("parko_trt_warmup_probe_cache");
    let _ = std::fs::remove_dir_all(&cache);
    let cfg = TrtConfig { engine_cache_path: cache.to_string_lossy().into_owned() };

    // &self warm-up (runs through Arc in the node) — no `mut` needed.
    let backend = match TrtBackend::with_config(model_path, &cfg) {
        Ok(b) => b,
        Err(e) => {
            assert!(
                !require_ep(),
                "STRICT (PARKO_TRT_REQUIRE_EP): TensorRT EP unavailable ({e:?}) — refusing (fail-closed).",
            );
            eprintln!("SKIP: TensorRT EP unavailable ({e:?}) — warm-up probe asserts only on a Jetson/TRT ORT.");
            return;
        }
    };

    // engine_sha is None until warm-up has built/located an engine on disk.
    assert_eq!(
        backend.engine_sha(),
        None,
        "engine_sha() must be None before warm-up (no engine built yet)",
    );

    let model = backend.load_model(model_path).expect("MNIST model introspection failed");

    // COLD warm-up — forces the engine build and captures its SHA-256.
    let cold = backend.warm_up_report(&model).expect("cold warm_up failed");
    let sha = cold
        .engine_sha256
        .clone()
        .expect("warm_up must capture the engine SHA-256 after a cold build");
    assert_eq!(sha.len(), 64, "SHA-256 hex must be 64 chars, got {}", sha.len());
    assert_eq!(
        backend.engine_sha(),
        Some(sha.as_str()),
        "warm_up must capture the engine SHA into the backend (engine_sha())",
    );
    assert!(cold.engine_bytes.unwrap_or(0) > 0, "engine file must be non-empty");

    // WARM warm-up — idempotent: same on-disk engine ⇒ identical SHA. Also exercise
    // the trait hook (returns ()), proving the generic startup path works.
    let warm = backend.warm_up_report(&model).expect("warm warm_up failed");
    assert_eq!(
        warm.engine_sha256.as_deref(),
        Some(sha.as_str()),
        "a second warm-up against the warm cache must yield the SAME engine_sha (idempotent)",
    );
    InferenceBackend::warm_up(&backend, &model).expect("trait warm_up hook must succeed when EP is present");
    assert_eq!(backend.engine_sha(), Some(sha.as_str()), "engine_sha() stable across the trait hook");

    println!(
        "WARM-UP PROBE PASSED — cold warm_up {} ms, warm warm_up {} ms. \
         engine '{}' {} B, engine_sha256={} (captured via engine_sha(); idempotent; trait hook ok).",
        cold.warmed_ms,
        warm.warmed_ms,
        cold.engine_file.as_deref().unwrap_or("<none>"),
        cold.engine_bytes.unwrap_or(0),
        sha,
    );
}
