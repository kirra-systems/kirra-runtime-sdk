//! The M-1 OFFLINE trainer (`parko/DOER_MODEL_SCALEUP.md` §2/§5.3): trains the
//! full-size v2 scorer by seeded SGD distillation and writes the versioned
//! weights artifact CI behavior-gates. Deterministic given the seed on a fixed
//! architecture; cross-arch float reproducibility is NOT guaranteed, which is
//! why CI gates the checked-in artifact's BEHAVIOR and never retrains.
//!
//! Run (release — the full schedule is minutes of f64 matmuls):
//!   cargo run --release -p kirra-planner --example train_v2 [out_path]
//! Default out_path: artifacts/doer-eval/planner_v2_weights.bin (repo root).

use std::time::Instant;

use kirra_planner::{train_planner_v2, ScorerConfigV2, Teacher, TrainConfigV2};

/// The one seed every doer-eval artifact derives from.
const SEED: u64 = 0xC0FFEE;

fn main() -> std::io::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}/../../artifacts/doer-eval/planner_v2_weights.bin",
            env!("CARGO_MANIFEST_DIR")
        )
    });

    let cfg = ScorerConfigV2::full();
    let tcfg = TrainConfigV2::full(SEED);
    println!(
        "training v2 (SafetyAware): vocab {} ({}x{}), hidden {:?}, scenes {}, epochs {}",
        cfg.vocab_size(),
        cfg.lateral_offsets.len(),
        cfg.speed_targets.len(),
        cfg.hidden,
        tcfg.scenes,
        tcfg.epochs,
    );

    let t0 = Instant::now();
    let (planner, final_loss) = train_planner_v2(&cfg, &tcfg, Teacher::SafetyAware);
    let bytes = planner.to_bytes();
    std::fs::write(&out, &bytes)?;

    println!(
        "trained in {:.1}s — final distillation MSE {final_loss:.6}; wrote {} ({} bytes)",
        t0.elapsed().as_secs_f64(),
        out,
        bytes.len(),
    );
    Ok(())
}
