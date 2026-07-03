//! WS-3.1 — the scenario-KPI CI gate binary.
//!
//! Loads the reviewed thresholds (`ci/scenario_kpi_thresholds.json`, or the
//! path given as the first argument), runs the generated corpora through the
//! existing metric harnesses, prints the scorecard, and exits non-zero on
//! any KPI breach. Deterministic: a red run is a real KPI regression.

use kirra_kpi_gate::{run_gate, KpiThresholds};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ci/scenario_kpi_thresholds.json".to_string());

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("scenario_kpi_gate: cannot read thresholds at {path}: {e}");
            eprintln!("  (run from the repo root, or pass the thresholds path as the first argument)");
            std::process::exit(2);
        }
    };
    let thresholds: KpiThresholds = match serde_json::from_str(&raw) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("scenario_kpi_gate: thresholds at {path} do not parse: {e}");
            std::process::exit(2);
        }
    };

    let report = run_gate(&thresholds);

    println!("=== Scenario-KPI gate (WS-3.1) ===");
    println!(
        "corpus: {} doer scenarios, {} perception frames",
        report.doer_scenarios, report.perception_frames
    );
    // #777 F1: `*_seam_pinned` rows are a tautological harness smoke test (mock
    // detector fed its own truth) — NOT a measurement. `negctl_*` rows are the
    // real discriminance evidence: an injected detector fault MUST breach the
    // metric (an unsafe fault → high unsafe_miss; a phantom → over-conservative,
    // never unsafe). A red `negctl_*` row means the oracle was BLINDED.
    println!("  (rows: *_seam_pinned = tautological smoke test; negctl_* = fault-injection discriminance)");
    for row in &report.rows {
        println!(
            "  [{}] {:<34} {:.4} (must be {} {:.4})",
            if row.pass { "PASS" } else { "FAIL" },
            row.name,
            row.measured,
            row.direction,
            row.bound,
        );
    }

    if report.passed() {
        println!("KPI gate: PASS");
    } else {
        eprintln!(
            "KPI gate: FAIL — a fleet-safety KPI regressed past its reviewed threshold. \
             Fix the regression; if the change is intentional and reviewed, update {path} \
             with a justification in the PR."
        );
        std::process::exit(1);
    }
}
