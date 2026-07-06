//! # SPI ledger — the residual-risk / Safety-Performance-Indicator gate (WS-3.3)
//!
//! `UL4600_SAFETY_CASE.md §5` defines a Safety-Performance-Indicator catalogue —
//! leading/lagging metrics computed over the hash-chained audit log — as the
//! *living* half of the safety case (UL 4600's continuous re-validation). But §5.2
//! is **prose**: nothing checks that each SPI's cited `event_type` literals are the
//! ones actually emitted on main, so a renamed/removed audit event silently
//! dangles the SPI, and there is no executable rollup.
//!
//! This module closes that: the reviewed registry (`ci/spi_registry.json`)
//! transcribes §5.2, and two pure functions gate + evaluate it —
//!
//! - [`check_resolution`] — every `emitted` / `derived` SPI's `event_type`
//!   literals MUST be in the set actually emitted in the workspace's NON-TEST code
//!   (the root `src/` plus `crates/*/src` — some `event_type`s are `DerateCode`
//!   reason tokens defined in `kirra-core`, scanned by [`scan_emitted_literals`]);
//!   a `parko`-scope SPI is emitted in the node-local chain (its numerator is
//!   external, its verifier-side denominator still resolves); the `integrity` SPI
//!   (SPI-G06) is the chain-verify precondition, not a tally. A dangling literal
//!   (a rename the catalogue missed) reds the gate.
//! - [`evaluate`] — the runtime rollup: given a window's `event_type` tallies and
//!   the chain-verify result, compute each SPI value (count or ratio) and its
//!   threshold breach. Per §5.1 the integrity precondition is load-bearing: a
//!   failed chain verify is itself the highest-severity SPI (SPI-G06) and
//!   **invalidates the window**.
//!
//! The evidence artifact is the same shape as the SOTIF trigger-coverage gate:
//! prose catalogue → reviewed manifest → machine-checked resolution + a tested
//! pure evaluator, so the audit chain becomes a first-class, drift-proof SPI source.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Leading (predicts a rising risk of intervention) vs lagging (counts a safety
/// reaction that already occurred). UL 4600 wants both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpiKind {
    Leading,
    Lagging,
}

/// Which way is better for an SPI — a validated enum, NOT a free-form string, so a
/// typo in the reviewed registry (`lower_beter`) fails to deserialize and reds the
/// gate rather than silently defaulting to lower-better and changing breach
/// semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Over the threshold breaches (every §5.2 SPI).
    LowerBetter,
    /// Under the threshold breaches.
    HigherBetter,
}

/// The data-source reality of an SPI (the §5 honesty tags, machine-enforced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpiSource {
    /// A real audit `event_type` exists on main — every referenced literal MUST
    /// resolve against the emitted set.
    Emitted,
    /// Computed from existing emitted events (its component literals resolve).
    Derived,
    /// Emitted in the parko node-local chain (external to `src/`): the numerator is
    /// not resolved here, but a verifier-side denominator still must.
    Parko,
    /// The chain-verify precondition (SPI-G06) — not a tally; carries no
    /// `event_type` and is evaluated from the verify result.
    Integrity,
}

/// One SPI's reviewed definition — a row of `UL4600_SAFETY_CASE.md §5.2`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpiDef {
    pub id: String,
    pub name: String,
    pub kind: SpiKind,
    pub source: SpiSource,
    /// Numerator audit `event_type` literals (summed).
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Optional ratio denominator `event_type` (a rate = numerator / denominator).
    #[serde(default)]
    pub denominator: Option<String>,
    /// Which way is better — selects the breach direction against `threshold`.
    /// A validated enum: an unknown value fails to parse (reds the gate).
    pub direction: Direction,
    /// A placeholder alarm limit (field-calibration TBD per §5); `None` → the SPI
    /// is monitored/reported but not alarm-gated yet.
    #[serde(default)]
    pub threshold: Option<f64>,
    /// Free-text linkage / rationale — travels with the numbers.
    pub note: String,
}

/// The reviewed SPI registry (`ci/spi_registry.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpiRegistry {
    #[serde(rename = "_comment")]
    pub comment: String,
    pub spis: Vec<SpiDef>,
}

// ---------------------------------------------------------------------------
// Resolution gate — every cited event_type must be real
// ---------------------------------------------------------------------------

/// One SPI's resolution outcome.
#[derive(Debug, Clone, Serialize)]
pub struct ResolutionRow {
    pub id: String,
    pub source: SpiSource,
    pub pass: bool,
    pub reason: Option<String>,
}

/// The resolution outcome over the whole registry.
#[derive(Debug, Clone, Serialize)]
pub struct ResolutionReport {
    pub rows: Vec<ResolutionRow>,
    /// SPI IDs that appear more than once (a registry must have unique IDs).
    pub duplicate_ids: Vec<String>,
}

impl ResolutionReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.duplicate_ids.is_empty() && self.rows.iter().all(|r| r.pass)
    }
}

/// Gate the registry against the set of `event_type` literals actually emitted in
/// `src/`. See the module docs for the per-source rule.
#[must_use]
pub fn check_resolution(reg: &SpiRegistry, emitted: &BTreeSet<String>) -> ResolutionReport {
    let mut seen = BTreeSet::new();
    let mut duplicate_ids = Vec::new();
    let mut rows = Vec::new();

    for spi in &reg.spis {
        if !seen.insert(spi.id.clone()) {
            duplicate_ids.push(spi.id.clone());
        }
        let (pass, reason) = resolve_one(spi, emitted);
        rows.push(ResolutionRow {
            id: spi.id.clone(),
            source: spi.source,
            pass,
            reason,
        });
    }
    ResolutionReport { rows, duplicate_ids }
}

fn resolve_one(spi: &SpiDef, emitted: &BTreeSet<String>) -> (bool, Option<String>) {
    let unresolved = |lits: &[&String]| -> Vec<String> {
        lits.iter()
            .filter(|l| !emitted.contains(l.as_str()))
            .map(|l| (*l).clone())
            .collect()
    };
    match spi.source {
        SpiSource::Emitted | SpiSource::Derived => {
            if spi.event_types.is_empty() {
                return (false, Some("an emitted/derived SPI must cite ≥ 1 event_type".into()));
            }
            let mut lits: Vec<&String> = spi.event_types.iter().collect();
            if let Some(d) = &spi.denominator {
                lits.push(d);
            }
            let missing = unresolved(&lits);
            if missing.is_empty() {
                (true, None)
            } else {
                (false, Some(format!("event_type(s) not emitted in src/: {}", missing.join(", "))))
            }
        }
        SpiSource::Parko => {
            if spi.event_types.is_empty() || spi.note.trim().is_empty() {
                return (false, Some("a parko-scope SPI must cite its event_type + a note".into()));
            }
            // The numerator lives in the node-local chain (external); the
            // verifier-side denominator, if any, still must resolve.
            if let Some(d) = &spi.denominator {
                if !emitted.contains(d.as_str()) {
                    return (false, Some(format!("denominator `{d}` not emitted in src/")));
                }
            }
            (true, None)
        }
        SpiSource::Integrity => {
            if spi.event_types.is_empty() {
                (true, None)
            } else {
                (false, Some("the integrity SPI is a verify precondition, not a tally — no event_type".into()))
            }
        }
    }
}

/// Scan the NON-TEST `.rs` code under `root` for double-quoted `UPPER_SNAKE_CASE`
/// string literals (len ≥ 6) — an approximation of the emitted audit `event_type`
/// universe.
///
/// To keep the gate meaningful, test code is excluded: dedicated test files
/// (`tests.rs` or anything under a `tests/` directory) are skipped, and each file
/// is truncated at its first `#[cfg(test)]` (the conventional trailing test
/// module). So a literal that appears ONLY in a test/assert no longer resolves —
/// a removed/renamed audit event whose old name lingers only in a test is caught.
/// This is still a slight over-approximation of *non-test* literals (it does not
/// prove the token is passed to an audit-write call), erring toward admitting a
/// reference rather than falsely rejecting one; strengthening it to per-call-site
/// emission matching is future work.
#[must_use]
pub fn scan_emitted_literals(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    scan_dir(root, &mut out);
    out
}

fn scan_dir(dir: &Path, out: &mut BTreeSet<String>) {
    // Skip dedicated test trees and build output entirely.
    if dir.file_name().is_some_and(|n| n == "tests" || n == "target") {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            // Exclude this gate's own source (meta: its tests + the reconciliation
            // note mention literals it does not emit) and dedicated test files.
            && path.file_name().is_none_or(|n| n != "spi_ledger.rs" && n != "tests.rs")
        {
            if let Ok(src) = std::fs::read_to_string(&path) {
                extract_upper_snake_literals(non_test_prefix(&src), out);
            }
        }
    }
}

/// The file content up to (not including) the first `#[cfg(test)]` — the
/// conventional trailing test module. Drops test literals from the scan.
fn non_test_prefix(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    }
}

fn extract_upper_snake_literals(src: &str, out: &mut BTreeSet<String>) {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let tok = &src[start..j];
                if is_upper_snake(tok) {
                    out.insert(tok.to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
}

fn is_upper_snake(s: &str) -> bool {
    s.len() >= 6
        && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && s.bytes().any(|b| b.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// Runtime rollup — the tested pure evaluator
// ---------------------------------------------------------------------------

/// One SPI's evaluated value over a window.
#[derive(Debug, Clone, Serialize)]
pub struct SpiValue {
    pub id: String,
    /// The computed value: a rate (numerator/denominator) when a denominator is
    /// set, else a raw count. Meaningful only when `evaluated`.
    pub value: f64,
    /// Whether the SPI could be evaluated over the window. A RATE SPI whose
    /// denominator event did not occur (0 traffic) is **undefined** for the
    /// window — it is not evaluated, so it never manufactures a false alarm.
    pub evaluated: bool,
    /// Whether `value` breached `threshold` in the SPI's `direction`. Always
    /// false when there is no threshold or the SPI was not evaluated.
    pub breached: bool,
}

/// The window rollup.
#[derive(Debug, Clone, Serialize)]
pub struct SpiRollup {
    /// The chain verified over the window (SPI-G06). When false the window is
    /// INVALID (§5.1 integrity precondition) — `values` are not trustworthy.
    pub chain_ok: bool,
    pub values: Vec<SpiValue>,
}

impl SpiRollup {
    /// Whether the window is trustworthy AND no SPI breached. A failed chain
    /// verify alone makes this false (the highest-severity lagging SPI).
    #[must_use]
    pub fn all_clear(&self) -> bool {
        self.chain_ok && self.values.iter().all(|v| !v.breached)
    }
}

/// Evaluate every SPI over a window's `event_type` tallies. `chain_ok` is the
/// result of `verify_audit_chain_full` for the window (SPI-G06 / §5.1 precondition).
#[must_use]
pub fn evaluate(reg: &SpiRegistry, tallies: &BTreeMap<String, u64>, chain_ok: bool) -> SpiRollup {
    let count = |lits: &[String]| -> u64 { lits.iter().map(|l| tallies.get(l).copied().unwrap_or(0)).sum() };

    let mut values = Vec::new();
    for spi in &reg.spis {
        let (value, evaluated, breached) = match spi.source {
            SpiSource::Integrity => {
                // SPI-G06: 0 when the chain verified, 1 (breach) when it did not.
                let v = if chain_ok { 0.0 } else { 1.0 };
                (v, true, !chain_ok)
            }
            _ => {
                let numerator = count(&spi.event_types) as f64;
                match &spi.denominator {
                    Some(d) => {
                        let denom = tallies.get(d).copied().unwrap_or(0);
                        if denom == 0 {
                            // A RATE over a window with no denominator traffic is
                            // undefined — do NOT clamp-and-manufacture a rate (a
                            // false alarm); mark it unevaluated, never breached.
                            (0.0, false, false)
                        } else {
                            let value = numerator / denom as f64;
                            let breached = spi.threshold.is_some_and(|t| breaches(value, t, spi.direction));
                            (value, true, breached)
                        }
                    }
                    // A raw count is always evaluable.
                    None => {
                        let breached = spi.threshold.is_some_and(|t| breaches(numerator, t, spi.direction));
                        (numerator, true, breached)
                    }
                }
            }
        };
        values.push(SpiValue { id: spi.id.clone(), value, evaluated, breached });
    }
    SpiRollup { chain_ok, values }
}

fn breaches(value: f64, threshold: f64, direction: Direction) -> bool {
    match direction {
        Direction::HigherBetter => value < threshold,
        Direction::LowerBetter => value > threshold,
    }
}

/// Parse the SPI IDs declared in `UL4600_SAFETY_CASE.md §5.2` — every catalogue
/// row starts `| SPI-XNN |`. Used by the completeness gate (registry ↔ catalogue).
#[must_use]
pub fn parse_catalogue_spi_ids(safety_case_md: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in safety_case_md.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("| SPI-") {
            let tail: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !tail.is_empty() {
                ids.insert(format!("SPI-{tail}"));
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ci/spi_registry.json");
    const SAFETY_CASE_MD: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/docs/safety/UL4600_SAFETY_CASE.md");
    const SRC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    const CRATES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/crates");

    /// The workspace emitted-literal universe for a `verifier`-scope SPI: the root
    /// crate `src/` plus the sibling `crates/*/src` (the audit event_types are
    /// defined/emitted across the verifier + its lean deps — e.g. the perception
    /// `DerateCode` strings live in `kirra-core`). `parko/` is a separate workspace
    /// (node-local chain), deliberately not scanned — hence SPI-L01 is parko-scope.
    fn workspace_emitted() -> BTreeSet<String> {
        let mut set = scan_emitted_literals(Path::new(SRC_DIR));
        set.extend(scan_emitted_literals(Path::new(CRATES_DIR)));
        set
    }

    fn committed_registry() -> SpiRegistry {
        let raw = std::fs::read_to_string(REGISTRY_JSON).expect("registry exists");
        serde_json::from_str(&raw).expect("registry parses")
    }

    fn ids(reg: &SpiRegistry) -> BTreeSet<String> {
        reg.spis.iter().map(|s| s.id.clone()).collect()
    }

    /// THE WS-3.3 SPI DoD (green): every emitted/derived SPI in the committed
    /// registry cites an `event_type` that is actually emitted in `src/`, and the
    /// registry's IDs exactly match the §5.2 catalogue.
    #[test]
    fn committed_registry_resolves_and_matches_the_catalogue() {
        let reg = committed_registry();
        let emitted = workspace_emitted();
        let report = check_resolution(&reg, &emitted);
        assert!(report.passed(), "committed SPI registry must resolve: {report:#?}");

        let catalogue = parse_catalogue_spi_ids(
            &std::fs::read_to_string(SAFETY_CASE_MD).expect("safety case exists"),
        );
        assert_eq!(
            ids(&reg),
            catalogue,
            "registry IDs must equal the §5.2 catalogue IDs (no orphan / no spurious SPI)"
        );
    }

    /// The scanner sees the load-bearing emitted literals (a sanity anchor that a
    /// green resolution isn't vacuous because the set came back empty).
    #[test]
    fn scanner_finds_known_emitted_literals() {
        let emitted = workspace_emitted();
        for lit in ["MOTION_COMMAND_ADMITTED", "RSS_VIOLATION", "SYSTEM_POSTURE_TRANSITION", "DETECTION_RANGE_UNTRUSTED"] {
            assert!(emitted.contains(lit), "scanner must find {lit}");
        }
        // And it does NOT invent an event that no non-test code emits — a
        // removed/renamed literal would vanish from this set and fail to resolve.
        assert!(!emitted.contains("TOTALLY_ABSENT_EVENT_XYZ"), "an unemitted token must not resolve");
    }

    /// A typo in the reviewed `direction` fails to deserialize — it can no longer
    /// silently default to lower-better and flip breach semantics.
    #[test]
    fn typo_in_direction_fails_to_parse() {
        let json = r#"{"_comment":"","spis":[{"id":"SPI-X","name":"n","kind":"leading","source":"integrity","direction":"lower_beter","note":"n"}]}"#;
        assert!(
            serde_json::from_str::<SpiRegistry>(json).is_err(),
            "an unknown direction value must fail to parse (reds the gate)"
        );
        // The correct spelling parses.
        let ok = json.replace("lower_beter", "lower_better");
        assert!(serde_json::from_str::<SpiRegistry>(&ok).is_ok());
    }

    /// A dangling `event_type` (a rename the catalogue missed) reds the gate.
    #[test]
    fn dangling_event_type_reds_the_gate() {
        let reg = SpiRegistry {
            comment: String::new(),
            spis: vec![SpiDef {
                id: "SPI-L99".into(),
                name: "phantom".into(),
                kind: SpiKind::Leading,
                source: SpiSource::Emitted,
                event_types: vec!["THIS_EVENT_DOES_NOT_EXIST".into()],
                denominator: None,
                direction: Direction::LowerBetter,
                threshold: None,
                note: "n".into(),
            }],
        };
        let mut emitted = BTreeSet::new();
        emitted.insert("MOTION_COMMAND_ADMITTED".to_string());
        let report = check_resolution(&reg, &emitted);
        assert!(!report.passed());
        assert!(report.rows[0].reason.as_deref().unwrap().contains("not emitted"));
    }

    /// A duplicate SPI ID reds the gate.
    #[test]
    fn duplicate_ids_red_the_gate() {
        let def = |id: &str| SpiDef {
            id: id.into(),
            name: "n".into(),
            kind: SpiKind::Lagging,
            source: SpiSource::Integrity,
            event_types: vec![],
            denominator: None,
            direction: Direction::LowerBetter,
            threshold: None,
            note: "n".into(),
        };
        let reg = SpiRegistry { comment: String::new(), spis: vec![def("SPI-G06"), def("SPI-G06")] };
        let report = check_resolution(&reg, &BTreeSet::new());
        assert!(!report.passed());
        assert_eq!(report.duplicate_ids, vec!["SPI-G06".to_string()]);
    }

    /// The evaluator computes a rate and trips a threshold; a clean window doesn't.
    #[test]
    fn evaluator_trips_a_ratio_threshold_and_stays_clean_otherwise() {
        let reg = SpiRegistry {
            comment: String::new(),
            spis: vec![SpiDef {
                id: "SPI-L02".into(),
                name: "rss near-miss rate".into(),
                kind: SpiKind::Leading,
                source: SpiSource::Emitted,
                event_types: vec!["RSS_VIOLATION".into()],
                denominator: Some("MOTION_COMMAND_ADMITTED".into()),
                direction: Direction::LowerBetter,
                threshold: Some(0.01),
                note: "n".into(),
            }],
        };
        // 3 violations / 100 admits = 0.03 > 0.01 → breach.
        let mut breach = BTreeMap::new();
        breach.insert("RSS_VIOLATION".to_string(), 3);
        breach.insert("MOTION_COMMAND_ADMITTED".to_string(), 100);
        let r = evaluate(&reg, &breach, true);
        assert!((r.values[0].value - 0.03).abs() < 1e-9);
        assert!(r.values[0].breached, "0.03 must breach the 0.01 limit");
        assert!(!r.all_clear());

        // 0 violations / 100 admits = 0.0 → clean.
        let mut clean = BTreeMap::new();
        clean.insert("MOTION_COMMAND_ADMITTED".to_string(), 100);
        let r = evaluate(&reg, &clean, true);
        assert!(!r.values[0].breached);
        assert!(r.all_clear());
    }

    /// The integrity precondition: a failed chain verify makes SPI-G06 breach and
    /// invalidates the whole window (§5.1), even with otherwise-clean tallies.
    #[test]
    fn failed_chain_verify_invalidates_the_window() {
        let reg = SpiRegistry {
            comment: String::new(),
            spis: vec![SpiDef {
                id: "SPI-G06".into(),
                name: "audit-chain integrity".into(),
                kind: SpiKind::Lagging,
                source: SpiSource::Integrity,
                event_types: vec![],
                denominator: None,
                direction: Direction::LowerBetter,
                threshold: Some(0.0),
                note: "n".into(),
            }],
        };
        let r = evaluate(&reg, &BTreeMap::new(), false);
        assert!(r.values[0].breached, "SPI-G06 must breach on a failed verify");
        assert!(!r.all_clear(), "a failed chain verify invalidates the window");

        let ok = evaluate(&reg, &BTreeMap::new(), true);
        assert!(ok.all_clear(), "a verified chain with no events is all-clear");
    }

    /// A RATE over a window with no denominator traffic is undefined — it is NOT
    /// evaluated and NEVER breaches (no clamped-denominator false alarm), even with
    /// a nonzero numerator.
    #[test]
    fn zero_denominator_is_unevaluated_not_a_false_breach() {
        let reg = SpiRegistry {
            comment: String::new(),
            spis: vec![SpiDef {
                id: "SPI-G02".into(),
                name: "veto rate".into(),
                kind: SpiKind::Lagging,
                source: SpiSource::Emitted,
                event_types: vec!["FABRIC_COMMAND_DENIED".into()],
                denominator: Some("MOTION_COMMAND_ADMITTED".into()),
                direction: Direction::LowerBetter,
                threshold: Some(0.5),
                note: "n".into(),
            }],
        };
        // 2 denials, 0 admits: a former max(1) clamp would compute 2.0 and FALSE-
        // breach the 0.5 limit. Now the window is unevaluated for this rate SPI.
        let mut t = BTreeMap::new();
        t.insert("FABRIC_COMMAND_DENIED".to_string(), 2);
        let r = evaluate(&reg, &t, true);
        assert!(!r.values[0].evaluated, "a zero-denominator rate window is not evaluated");
        assert!(!r.values[0].breached, "an unevaluated rate must not manufacture a breach");
        assert!(r.values[0].value.is_finite());
        assert!(r.all_clear(), "an empty-denominator window is not an alarm");

        // With traffic, the same SPI evaluates and can breach.
        t.insert("MOTION_COMMAND_ADMITTED".to_string(), 2); // 2/2 = 1.0 > 0.5
        let r = evaluate(&reg, &t, true);
        assert!(r.values[0].evaluated && r.values[0].breached, "a real 1.0 rate breaches 0.5");
    }
}
