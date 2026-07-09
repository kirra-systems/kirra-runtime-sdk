//! # SOTIF trigger-coverage gate (WS-3.3)
//!
//! The [ISO 21448] triggering-condition catalog lives in
//! `docs/safety/OCCY_SOTIF.md §3` (TC-01…TC-NN). That table already names a
//! verification artifact per row — but only as **prose**: nothing checks that a
//! TC actually maps to a real scenario / AoU, or that a newly-added TC can't
//! merge without a coverage decision. This module closes the WS-3 DoD clause —
//! *"every SOTIF trigger row maps to scenarios or a documented AoU"* — by making
//! that mapping a machine-checked, reviewed manifest.
//!
//! The reviewed manifest is `ci/sotif_trigger_coverage.json`. Each triggering
//! condition declares its coverage as exactly one of four buckets:
//!
//! - **`corpus`** — covered by named scenarios in the LIVE generated corpus
//!   (`generated_doer_corpus` ∪ `generated_perception_corpus`). Each declared
//!   prefix MUST match ≥ 1 scenario in the corpus — so deleting the covering
//!   scenarios reds the gate (regression-proof, not a prose promise).
//! - **`aou`** — covered by a documented Assumption of Use. The referenced ID
//!   MUST exist in `docs/safety/ASSUMPTIONS_OF_USE.md`.
//! - **`external`** — verified by a domain artifact OUTSIDE this doer/perception
//!   KPI corpus (a demo / injection test, in the verifier or parko domain),
//!   carrying its tracking reference. A transcription of the doc's Verify column,
//!   made completeness-checked — an honest "covered, but not by this corpus".
//! - **`deferred`** — an explicit, reviewed gap (a Phase-2 scenario not yet
//!   built), carrying its tracking reference. A visible, greppable gap — never
//!   silence.
//!
//! The gate is a PURE function of (doc TC set, manifest, live corpus scenario
//! names, known AoU IDs) → [`CoverageReport`]; it reds on an orphan TC (in the
//! doc, no manifest entry), a spurious mapping (manifest entry for a TC not in
//! the doc), a `corpus` prefix that resolves to nothing, or an `aou` reference
//! that isn't in the register. The thin binary side feeds it the real files.
//!
//! [ISO 21448]: https://www.iso.org/standard/77490.html

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One triggering condition's reviewed coverage decision. The `coverage` tag
/// selects the bucket and its required fields (`deny_unknown_fields` per variant
/// keeps a mistyped field from silently vanishing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "coverage", rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerCoverage {
    /// Covered by named scenarios in the live generated corpus. `scenarios` are
    /// name PREFIXES; each must match ≥ 1 live scenario.
    Corpus {
        scenarios: Vec<String>,
        note: String,
    },
    /// Covered by a documented Assumption of Use (must exist in the register).
    Aou { aou: String, note: String },
    /// Verified by a domain artifact outside this KPI corpus; carries a reference.
    External { reference: String, note: String },
    /// An explicit, reviewed gap (e.g. a Phase-2 scenario); carries a reference.
    Deferred { reference: String, note: String },
}

impl TriggerCoverage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Corpus { .. } => "corpus",
            Self::Aou { .. } => "aou",
            Self::External { .. } => "external",
            Self::Deferred { .. } => "deferred",
        }
    }
}

/// The reviewed SOTIF coverage manifest (`ci/sotif_trigger_coverage.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SotifCoverageManifest {
    /// Free-text rationale — travels with the mapping.
    #[serde(rename = "_comment")]
    pub comment: String,
    /// TC-id → coverage decision.
    pub triggers: BTreeMap<String, TriggerCoverage>,
}

/// One evaluated coverage row.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageRow {
    pub tc: String,
    pub kind: &'static str,
    pub detail: String,
    pub pass: bool,
    /// Why the row failed (absent when it passed).
    pub reason: Option<String>,
}

/// The full coverage outcome.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    pub rows: Vec<CoverageRow>,
    /// TCs in the doc catalog with NO manifest entry (orphan triggers).
    pub missing_from_manifest: Vec<String>,
    /// Manifest entries for a TC that is NOT in the doc catalog (spurious).
    pub extra_in_manifest: Vec<String>,
}

impl CoverageReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.missing_from_manifest.is_empty()
            && self.extra_in_manifest.is_empty()
            && self.rows.iter().all(|r| r.pass)
    }
}

/// Extract the triggering-condition IDs from `OCCY_SOTIF.md`: every catalog-table
/// row starts `| TC-NN |`. Restricting to that shape avoids picking up `TC-0N`
/// mentions in surrounding prose.
#[must_use]
pub fn parse_trigger_ids(sotif_md: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in sotif_md.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("| TC-") {
            // rest starts with the digits; take while numeric.
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                ids.insert(format!("TC-{digits}"));
            }
        }
    }
    ids
}

/// Extract the Assumption-of-Use IDs (`AOU-…`) declared in `ASSUMPTIONS_OF_USE.md`.
/// A token is an AoU ID if it starts `AOU-` and is followed by `[A-Z0-9-]+`.
#[must_use]
pub fn parse_aou_ids(aou_md: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let bytes = aou_md.as_bytes();
    let mut i = 0;
    while let Some(rel) = aou_md[i..].find("AOU-") {
        let start = i + rel;
        let mut end = start + 4;
        while end < bytes.len() {
            let c = bytes[end];
            if c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-' {
                end += 1;
            } else {
                break;
            }
        }
        // Trim a trailing '-' (e.g. from "AOU-" at a line end) so it isn't an ID.
        let id = aou_md[start..end].trim_end_matches('-');
        if id.len() > 4 {
            ids.insert(id.to_string());
        }
        i = end.max(start + 1);
    }
    ids
}

/// The pure coverage check. See the module docs for the four failure modes.
#[must_use]
pub fn check_sotif_coverage(
    doc_tcs: &BTreeSet<String>,
    manifest: &SotifCoverageManifest,
    corpus_scenarios: &BTreeSet<String>,
    known_aous: &BTreeSet<String>,
) -> CoverageReport {
    let manifest_tcs: BTreeSet<String> = manifest.triggers.keys().cloned().collect();

    let missing_from_manifest: Vec<String> = doc_tcs.difference(&manifest_tcs).cloned().collect();
    let extra_in_manifest: Vec<String> = manifest_tcs.difference(doc_tcs).cloned().collect();

    let mut rows = Vec::new();
    for (tc, cov) in &manifest.triggers {
        let (detail, pass, reason) = evaluate(cov, corpus_scenarios, known_aous);
        rows.push(CoverageRow {
            tc: tc.clone(),
            kind: cov.kind(),
            detail,
            pass,
            reason,
        });
    }

    CoverageReport {
        rows,
        missing_from_manifest,
        extra_in_manifest,
    }
}

/// Resolve a single coverage decision to (detail, pass, reason).
fn evaluate(
    cov: &TriggerCoverage,
    corpus_scenarios: &BTreeSet<String>,
    known_aous: &BTreeSet<String>,
) -> (String, bool, Option<String>) {
    match cov {
        TriggerCoverage::Corpus { scenarios, .. } => {
            if scenarios.is_empty() {
                return (
                    "corpus: <no prefixes>".to_string(),
                    false,
                    Some("a corpus mapping must name ≥ 1 scenario prefix".to_string()),
                );
            }
            let unresolved: Vec<&String> = scenarios
                .iter()
                .filter(|prefix| {
                    !corpus_scenarios
                        .iter()
                        .any(|name| name.starts_with(*prefix))
                })
                .collect();
            if unresolved.is_empty() {
                (format!("corpus: {}", scenarios.join(", ")), true, None)
            } else {
                (
                    format!("corpus: {}", scenarios.join(", ")),
                    false,
                    Some(format!(
                        "prefix(es) match no live scenario: {}",
                        unresolved
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                )
            }
        }
        TriggerCoverage::Aou { aou, .. } => {
            if known_aous.contains(aou) {
                (format!("aou: {aou}"), true, None)
            } else {
                (
                    format!("aou: {aou}"),
                    false,
                    Some(format!("AoU `{aou}` is not in the register")),
                )
            }
        }
        TriggerCoverage::External { reference, .. } => {
            if reference.trim().is_empty() {
                (
                    "external: <no reference>".to_string(),
                    false,
                    Some("an external mapping must carry a reference".to_string()),
                )
            } else {
                (format!("external: {reference}"), true, None)
            }
        }
        TriggerCoverage::Deferred { reference, .. } => {
            if reference.trim().is_empty() {
                (
                    "deferred: <no reference>".to_string(),
                    false,
                    Some("a deferred mapping must carry a reference".to_string()),
                )
            } else {
                (format!("deferred: {reference}"), true, None)
            }
        }
    }
}

/// The live corpus scenario-name universe: doer ∪ perception names. This is the
/// resolution target for every `corpus` mapping, so it can never drift from the
/// scenarios the KPI gate actually runs.
#[must_use]
pub fn live_corpus_scenario_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for s in crate::generated_doer_corpus() {
        names.insert(s.name);
    }
    for c in crate::generated_perception_corpus() {
        names.insert(c.name);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOTIF_MD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/safety/OCCY_SOTIF.md"
    );
    const AOU_MD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/safety/ASSUMPTIONS_OF_USE.md"
    );
    const MANIFEST_JSON: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../ci/sotif_trigger_coverage.json"
    );

    fn read(p: &str) -> String {
        std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
    }

    fn committed_manifest() -> SotifCoverageManifest {
        serde_json::from_str(&read(MANIFEST_JSON)).expect("manifest parses")
    }

    #[test]
    fn parses_the_eight_committed_trigger_ids() {
        let ids = parse_trigger_ids(&read(SOTIF_MD));
        // The catalog is living; assert the currently-committed set is present.
        for n in 1..=8u32 {
            assert!(
                ids.contains(&format!("TC-{n:02}")),
                "TC-{n:02} must parse from §3"
            );
        }
    }

    #[test]
    fn parses_known_aou_ids() {
        let ids = parse_aou_ids(&read(AOU_MD));
        assert!(
            ids.contains("AOU-LOCALIZATION-001"),
            "a known AoU must parse"
        );
        // The '-' trimming must not admit a bare 'AOU'.
        assert!(!ids.contains("AOU"));
    }

    /// THE WS-3.3 DoD (green half): the committed manifest covers every doc TC,
    /// with no orphan and no spurious entry, and every `corpus`/`aou` reference
    /// resolves against the LIVE corpus + AoU register.
    #[test]
    fn committed_manifest_fully_covers_the_catalog() {
        let doc = parse_trigger_ids(&read(SOTIF_MD));
        let aous = parse_aou_ids(&read(AOU_MD));
        let corpus = live_corpus_scenario_names();
        let report = check_sotif_coverage(&doc, &committed_manifest(), &corpus, &aous);
        assert!(
            report.passed(),
            "committed SOTIF coverage must be complete + resolve: {report:#?}"
        );
    }

    /// A doc TC with no manifest entry (an orphan trigger) reds the gate.
    #[test]
    fn orphan_trigger_reds_the_gate() {
        let mut doc = BTreeSet::new();
        doc.insert("TC-01".to_string());
        doc.insert("TC-99".to_string()); // no manifest entry
        let mut m = SotifCoverageManifest {
            comment: String::new(),
            triggers: BTreeMap::new(),
        };
        m.triggers.insert(
            "TC-01".to_string(),
            TriggerCoverage::Corpus {
                scenarios: vec!["Water_".to_string()],
                note: "n".to_string(),
            },
        );
        let report =
            check_sotif_coverage(&doc, &m, &live_corpus_scenario_names(), &BTreeSet::new());
        assert!(!report.passed());
        assert_eq!(report.missing_from_manifest, vec!["TC-99".to_string()]);
    }

    /// A manifest entry for a TC not in the doc (a spurious mapping) reds the gate.
    #[test]
    fn spurious_mapping_reds_the_gate() {
        let mut doc = BTreeSet::new();
        doc.insert("TC-01".to_string());
        let mut m = SotifCoverageManifest {
            comment: String::new(),
            triggers: BTreeMap::new(),
        };
        m.triggers.insert(
            "TC-01".to_string(),
            TriggerCoverage::External {
                reference: "#1".to_string(),
                note: "n".to_string(),
            },
        );
        m.triggers.insert(
            "TC-42".to_string(), // not in the doc
            TriggerCoverage::External {
                reference: "#2".to_string(),
                note: "n".to_string(),
            },
        );
        let report = check_sotif_coverage(&doc, &m, &BTreeSet::new(), &BTreeSet::new());
        assert!(!report.passed());
        assert_eq!(report.extra_in_manifest, vec!["TC-42".to_string()]);
    }

    /// A `corpus` mapping whose prefix matches no live scenario reds the gate —
    /// this is the regression-proofing edge: delete the covering scenarios and
    /// the gate catches the now-dangling claim.
    #[test]
    fn dangling_corpus_prefix_reds_the_gate() {
        let mut doc = BTreeSet::new();
        doc.insert("TC-01".to_string());
        let mut m = SotifCoverageManifest {
            comment: String::new(),
            triggers: BTreeMap::new(),
        };
        m.triggers.insert(
            "TC-01".to_string(),
            TriggerCoverage::Corpus {
                scenarios: vec!["NoSuchScenario_".to_string()],
                note: "n".to_string(),
            },
        );
        let report =
            check_sotif_coverage(&doc, &m, &live_corpus_scenario_names(), &BTreeSet::new());
        assert!(!report.passed());
        assert!(report.rows[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("no live scenario"));
    }

    /// An `aou` mapping to an ID not in the register reds the gate.
    #[test]
    fn unknown_aou_reds_the_gate() {
        let mut doc = BTreeSet::new();
        doc.insert("TC-01".to_string());
        let mut m = SotifCoverageManifest {
            comment: String::new(),
            triggers: BTreeMap::new(),
        };
        m.triggers.insert(
            "TC-01".to_string(),
            TriggerCoverage::Aou {
                aou: "AOU-DOES-NOT-EXIST-001".to_string(),
                note: "n".to_string(),
            },
        );
        let mut known = BTreeSet::new();
        known.insert("AOU-LOCALIZATION-001".to_string());
        let report = check_sotif_coverage(&doc, &m, &BTreeSet::new(), &known);
        assert!(!report.passed());
        assert!(report.rows[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("not in the register"));
    }

    /// The two corpus-backed TCs genuinely resolve against the live corpus (the
    /// scenarios that back the SOTIF claim actually exist).
    #[test]
    fn corpus_backed_triggers_resolve_against_live_scenarios() {
        let corpus = live_corpus_scenario_names();
        assert!(
            corpus.iter().any(|n| n.starts_with("Water_")),
            "water perception cases exist"
        );
        assert!(
            corpus.iter().any(|n| n.starts_with("Stopped_")),
            "stopped doer cases exist"
        );
        assert!(
            corpus.iter().any(|n| n.starts_with("StaticObstacle_")),
            "static-obstacle perception cases exist"
        );
    }
}
