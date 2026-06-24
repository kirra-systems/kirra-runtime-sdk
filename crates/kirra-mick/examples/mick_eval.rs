//! **Score a Mick decision log against KIRRA's verdicts.** Reads the JSONL the demos write
//! (with `KIRRA_MICK_EVAL_ENABLED=1`) and prints the eval scorecard — acceptance / refusal /
//! hold rates and a per-intent-kind breakdown — so the brain's choices can be *measured*
//! against the checker, not just watched.
//!
//! Run it:
//!   KIRRA_MICK_EVAL_ENABLED=1 cargo run -p kirra-mick --example mick_intersection  # writes a log
//!   cargo run -p kirra-mick --example mick_eval [path]                             # scores it
//!
//! `path` defaults to `$KIRRA_MICK_EVAL_PATH`, else `kirra_mick_eval.jsonl` (the demos' sink).
//!
//! The scorecard never touches the verdict path — it is offline observability over a log that
//! was already produced. A higher acceptance rate / lower refusal rate means the brain is
//! choosing intents the checker can admit; a per-intent refusal spike says *which* maneuver
//! the model overreaches on.

use kirra_planner::MickEvalSummary;

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("KIRRA_MICK_EVAL_PATH").ok())
        .unwrap_or_else(|| "kirra_mick_eval.jsonl".to_string());

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mick_eval: cannot open {path}: {e}");
            eprintln!("  produce a log first, e.g.:");
            eprintln!("  KIRRA_MICK_EVAL_ENABLED=1 cargo run -p kirra-mick --example mick_intersection");
            std::process::exit(1);
        }
    };

    match MickEvalSummary::read_jsonl(std::io::BufReader::new(file)) {
        Ok(summary) => {
            println!("scored {path}\n{summary}");
        }
        Err(e) => {
            eprintln!("mick_eval: failed to score {path}: {e}");
            std::process::exit(1);
        }
    }
}
