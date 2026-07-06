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
//!   literals MUST be in the set actually emitted in `src/` (scanned by
//!   [`scan_emitted_literals`]); a `parko`-scope SPI is emitted in the node-local
//!   chain (its numerator is external, its verifier-side denominator still
//!   resolves); the `integrity` SPI (SPI-G06) is the chain-verify precondition,
//!   not a tally. A dangling literal (a rename the catalogue missed) reds the gate.
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
    /// Which way is better — `"lower_better"` (the default for every §5.2 SPI) or
    /// `"higher_better"`. Selects the breach direction against `threshold`.
    pub direction: String,
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

/// Scan `.rs` files under `root` for double-quoted `UPPER_SNAKE_CASE` string
/// literals (len ≥ 6) — the universe of emitted audit `event_type` values. An
/// over-approximation (it also picks up other upper-snake constants), which is
/// safe for the gate: a *removed/renamed* event literal disappears from this set,
/// so a dangling registry reference is still caught; the extra tokens only ever
/// admit a reference, never wrongly reject one.
#[must_use]
pub fn scan_emitted_literals(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    scan_dir(root, &mut out);
    out
}

fn scan_dir(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            // Exclude this gate's own source: it is meta (its tests + the
            // reconciliation note mention literals it does not emit), so scanning
            // it would let a dangling reference resolve against itself.
            && path.file_name().is_none_or(|n| n != "spi_ledger.rs")
        {
            if let Ok(src) = std::fs::read_to_string(&path) {
                extract_upper_snake_literals(&src, out);
            }
        }
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
    /// set, else a raw count.
    pub value: f64,
    /// Whether `value` breached `threshold` in the SPI's `direction` (false when
    /// no threshold is set).
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
        let (value, breached) = match spi.source {
            SpiSource::Integrity => {
                // SPI-G06: 0 when the chain verified, 1 (breach) when it did not.
                let v = if chain_ok { 0.0 } else { 1.0 };
                (v, !chain_ok)
            }
            _ => {
                let numerator = count(&spi.event_types) as f64;
                let value = match &spi.denominator {
                    // Rate = numerator / max(denominator, 1) — a 0-denominator
                    // window can't manufacture a divide-by-zero spike.
                    Some(d) => numerator / (tallies.get(d).copied().unwrap_or(0).max(1) as f64),
                    None => numerator,
                };
                let breached = spi.threshold.is_some_and(|t| breaches(value, t, &spi.direction));
                (value, breached)
            }
        };
        values.push(SpiValue { id: spi.id.clone(), value, breached });
    }
    SpiRollup { chain_ok, values }
}

fn breaches(value: f64, threshold: f64, direction: &str) -> bool {
    match direction {
        "higher_better" => value < threshold,
        // Default (and every §5.2 SPI): lower is better → over the limit breaches.
        _ => value > threshold,
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
        let emitted = scan_emitted_literals(Path::new(SRC_DIR));
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
        let emitted = scan_emitted_literals(Path::new(SRC_DIR));
        for lit in ["MOTION_COMMAND_ADMITTED", "RSS_VIOLATION", "SYSTEM_POSTURE_TRANSITION", "DETECTION_RANGE_UNTRUSTED"] {
            assert!(emitted.contains(lit), "scanner must find {lit}");
        }
        // And it does NOT invent the catalogue's drifted names (which don't exist).
        assert!(!emitted.contains("DETECTION_RANGE_DEGRADED"), "the drifted name must not resolve");
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
                direction: "lower_better".into(),
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
            direction: "lower_better".into(),
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
                direction: "lower_better".into(),
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
                direction: "lower_better".into(),
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

    /// A ratio with a zero denominator does not manufacture a divide-by-zero spike.
    #[test]
    fn zero_denominator_does_not_spike() {
        let reg = SpiRegistry {
            comment: String::new(),
            spis: vec![SpiDef {
                id: "SPI-G02".into(),
                name: "veto rate".into(),
                kind: SpiKind::Lagging,
                source: SpiSource::Emitted,
                event_types: vec!["FABRIC_COMMAND_DENIED".into()],
                denominator: Some("MOTION_COMMAND_ADMITTED".into()),
                direction: "lower_better".into(),
                threshold: Some(0.5),
                note: "n".into(),
            }],
        };
        let mut t = BTreeMap::new();
        t.insert("FABRIC_COMMAND_DENIED".to_string(), 2);
        // No admits in the window → denominator clamped to 1, value = 2.0 (finite).
        let r = evaluate(&reg, &t, true);
        assert!(r.values[0].value.is_finite(), "value must be finite with a zero denominator");
    }
}
