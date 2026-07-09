//! WS-3.1 / WS-3.3 — the scenario-KPI + SOTIF-coverage CI gate binary.
//!
//! Two gates, one job (so both ride the existing CI step):
//!
//! 1. **KPI gate (WS-3.1):** load the reviewed thresholds
//!    (`ci/scenario_kpi_thresholds.json`, or the path given as the first
//!    argument), run the generated corpora through the existing metric
//!    harnesses, print the scorecard, red on any KPI breach.
//! 2. **SOTIF coverage gate (WS-3.3):** check that every ISO 21448 triggering
//!    condition in `docs/safety/OCCY_SOTIF.md §3` maps — in the reviewed
//!    `ci/sotif_trigger_coverage.json` manifest — to a live scenario, a
//!    documented AoU, or an explicit referenced external/deferred artifact.
//!    Red on an orphan/spurious TC or a dangling scenario/AoU reference.
//!
//! Both are deterministic: a red run is a real regression. The process exits
//! non-zero if EITHER gate fails.

use kirra_kpi_gate::montecarlo::{MonteCarloPolicy, Profile};
use kirra_kpi_gate::sotif_coverage::{
    check_sotif_coverage, live_corpus_scenario_names, parse_aou_ids, parse_trigger_ids,
    SotifCoverageManifest,
};
use kirra_kpi_gate::{run_gate, run_montecarlo_gate, KpiThresholds};

const MC_POLICY_PATH: &str = "ci/scenario_kpi_montecarlo.json";

fn main() {
    let thresholds_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ci/scenario_kpi_thresholds.json".to_string());

    let kpi_ok = run_kpi_gate(&thresholds_path);
    println!();
    // WP-23: the seeded Monte-Carlo campaign. Opt-in via KIRRA_KPI_MC_PROFILE
    // (per_pr | nightly) so the deterministic gate above stays the default; CI
    // sets per_pr on PRs and nightly on the scheduled full-corpus run. Absent →
    // skipped (the campaign is additive, never a regression to the base gate).
    let mc_ok = run_montecarlo_campaign();
    println!();
    let sotif_ok = run_sotif_gate();

    if kpi_ok && mc_ok && sotif_ok {
        std::process::exit(0);
    }
    std::process::exit(1);
}

/// The WP-23 Monte-Carlo campaign gate. Returns `true` on pass OR when not
/// requested (opt-in). Exits(2) on an unreadable/unparseable policy or an
/// unknown profile (a configuration error, distinct from a breach).
fn run_montecarlo_campaign() -> bool {
    let profile = match std::env::var("KIRRA_KPI_MC_PROFILE") {
        Err(_) => return true, // not requested — skip (the default per-PR is the deterministic gate)
        Ok(p) => match p.trim().to_ascii_lowercase().as_str() {
            "per_pr" | "per-pr" | "perpr" => Profile::PerPr,
            "nightly" => Profile::Nightly,
            other => {
                eprintln!("scenario_kpi_gate: unknown KIRRA_KPI_MC_PROFILE={other:?} (want per_pr | nightly)");
                std::process::exit(2);
            }
        },
    };

    let raw = match std::fs::read_to_string(MC_POLICY_PATH) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("scenario_kpi_gate: cannot read MC policy at {MC_POLICY_PATH}: {e}");
            std::process::exit(2);
        }
    };
    let policy: MonteCarloPolicy = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("scenario_kpi_gate: MC policy at {MC_POLICY_PATH} does not parse: {e}");
            std::process::exit(2);
        }
    };

    let report = run_montecarlo_gate(&policy, profile);
    println!("=== Monte-Carlo scenario campaign (WP-23) ===");
    println!(
        "profile: {} — seed {}, {} doer scenarios, {} perception frames (95% CI bounds)",
        if profile == Profile::Nightly {
            "nightly"
        } else {
            "per_pr"
        },
        report.seed,
        report.doer_samples,
        report.perception_samples,
    );
    for row in &report.rows {
        let (dir, bound) = row.bound_display();
        println!(
            "  [{}] {:<34} wilson=[{:.4},{:.4}] cp=[{:.4},{:.4}] (must be {dir} {bound:.4})",
            if row.pass { "PASS" } else { "FAIL" },
            row.name,
            row.wilson.lo,
            row.wilson.hi,
            row.exact.lo,
            row.exact.hi,
        );
    }
    if report.passed() {
        println!("Monte-Carlo campaign: PASS");
        true
    } else {
        eprintln!(
            "Monte-Carlo campaign: FAIL — a KPI's confidence bound crossed its reviewed floor. \
             Fix the regression; if intentional and reviewed, update {MC_POLICY_PATH} with a \
             justification in the PR."
        );
        false
    }
}

/// The WS-3.1 KPI gate. Returns `true` on pass. Exits(2) on an unreadable /
/// unparseable thresholds file (a configuration error, distinct from a breach).
fn run_kpi_gate(path: &str) -> bool {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("scenario_kpi_gate: cannot read thresholds at {path}: {e}");
            eprintln!(
                "  (run from the repo root, or pass the thresholds path as the first argument)"
            );
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
        true
    } else {
        eprintln!(
            "KPI gate: FAIL — a fleet-safety KPI regressed past its reviewed threshold. \
             Fix the regression; if the change is intentional and reviewed, update {path} \
             with a justification in the PR."
        );
        false
    }
}

const SOTIF_MD_PATH: &str = "docs/safety/OCCY_SOTIF.md";
const AOU_MD_PATH: &str = "docs/safety/ASSUMPTIONS_OF_USE.md";
const SOTIF_MANIFEST_PATH: &str = "ci/sotif_trigger_coverage.json";

/// The WS-3.3 SOTIF trigger-coverage gate. Returns `true` on pass. Exits(2) on a
/// missing/unparseable input file (a configuration error, distinct from a
/// coverage failure).
fn run_sotif_gate() -> bool {
    let sotif_md = read_or_exit(SOTIF_MD_PATH);
    let aou_md = read_or_exit(AOU_MD_PATH);
    let manifest_raw = read_or_exit(SOTIF_MANIFEST_PATH);
    let manifest: SotifCoverageManifest = match serde_json::from_str(&manifest_raw) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "scenario_kpi_gate: SOTIF manifest at {SOTIF_MANIFEST_PATH} does not parse: {e}"
            );
            std::process::exit(2);
        }
    };

    let doc_tcs = parse_trigger_ids(&sotif_md);
    let aous = parse_aou_ids(&aou_md);
    let corpus = live_corpus_scenario_names();
    let report = check_sotif_coverage(&doc_tcs, &manifest, &corpus, &aous);

    println!("=== SOTIF trigger-coverage gate (WS-3.3) ===");
    println!(
        "catalog: {} triggering conditions ({} manifest entries)",
        doc_tcs.len(),
        manifest.triggers.len()
    );
    for row in &report.rows {
        match &row.reason {
            None => println!("  [PASS] {:<8} {} — {}", row.tc, row.kind, row.detail),
            Some(why) => println!(
                "  [FAIL] {:<8} {} — {} ({why})",
                row.tc, row.kind, row.detail
            ),
        }
    }
    for tc in &report.missing_from_manifest {
        println!(
            "  [FAIL] {tc:<8} — triggering condition in OCCY_SOTIF.md §3 has NO coverage entry"
        );
    }
    for tc in &report.extra_in_manifest {
        println!("  [FAIL] {tc:<8} — manifest entry for a TC that is NOT in OCCY_SOTIF.md §3");
    }

    if report.passed() {
        println!("SOTIF coverage gate: PASS");
        true
    } else {
        eprintln!(
            "SOTIF coverage gate: FAIL — a triggering condition is unmapped, spuriously mapped, \
             or references a scenario/AoU that does not exist. Update {SOTIF_MANIFEST_PATH} (and, \
             if a new TC was added, OCCY_SOTIF.md §3) so every trigger maps to a real artifact."
        );
        false
    }
}

fn read_or_exit(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("scenario_kpi_gate: cannot read {path}: {e}");
            eprintln!("  (run from the repo root)");
            std::process::exit(2);
        }
    }
}
