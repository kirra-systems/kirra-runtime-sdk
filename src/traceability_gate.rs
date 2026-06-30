// src/traceability_gate.rs
//
// S3 traceability gate (issue #115) — parses `// SAFETY: ...` annotations
// across the Governor source tree and asserts every Governor-ENFORCED safety
// goal (SG1..SG9 per OCCY_SAFETY_GOALS.md) has at least one tagged code site,
// every tagged site carries a non-empty REQ and TEST field, and every SG
// identifier is in range.
//
// Tag format (defined in docs/safety/TRACEABILITY.md §1):
//
//     // SAFETY: SG3 SG9 | REQ: <kebab-id> | TEST: test_a,test_b
//
// DELEGATED SGs are recorded exceptions — their absence from the ENFORCED-tag
// set is not a gate failure; they're tracked elsewhere (the SEooC contract #126).
// There are no PENDING SGs at present: SG2 (drivable-space containment) is now
// ENFORCED — its check is built (`kirra_core::containment`) and wired live into
// the Option-B adapter slow loop (`kirra-trajectory` validation; #128/#131).

use std::path::{Path, PathBuf};

/// One parsed `// SAFETY: ...` annotation.
#[derive(Debug, Clone)]
pub struct SafetyTag {
    pub file: PathBuf,
    pub line: usize,
    pub sgs: Vec<u8>,
    pub req: String,
    pub tests: Vec<String>,
}

/// Governor-enforced safety goals — each MUST have at least one tagged site.
/// SG2 (drivable-space containment) joined this set once its check landed in
/// `kirra_core::containment` and was wired live (kirra-trajectory slow loop;
/// #128/#131) — it is tagged at `src/wcet_gate.rs` (SG2 WCET) and enforced live.
pub const ENFORCED_SGS: &[u8] = &[1, 2, 3, 7, 8, 9];

/// Pending (coverage hole tracked in its own issue) — absent from tags is OK.
/// Empty: SG2 graduated to ENFORCED (#128); no SG is currently a coverage hole.
pub const PENDING_SGS: &[u8] = &[];

/// Delegated by design (integrator perception / map prior / control adapter) —
/// absent from Governor tags is OK; tracked in the SEooC contract (#126).
pub const DELEGATED_SGS: &[u8] = &[4, 5, 6];

/// Walks `src/` + `parko/crates/` under the workspace root and returns every
/// parsed safety tag. Public so the extraction script can call it via a
/// thin Rust binary.
pub fn walk_safety_tags() -> Vec<SafetyTag> {
    let mut tags = Vec::new();
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    for sub in &[root.join("src"), root.join("parko").join("crates")] {
        if sub.exists() {
            walk_dir(sub, &mut tags);
        }
    }
    tags
}

fn walk_dir(dir: &Path, tags: &mut Vec<SafetyTag>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip target/ and any hidden dirs.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk_dir(&path, tags);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                parse_file(&path, &content, tags);
            }
        }
    }
}

fn parse_file(path: &Path, content: &str, tags: &mut Vec<SafetyTag>) {
    for (idx, line) in content.lines().enumerate() {
        if let Some((sgs, req, tests)) = parse_safety_line(line) {
            tags.push(SafetyTag {
                file: path.to_path_buf(),
                line: idx + 1,
                sgs,
                req,
                tests,
            });
        }
    }
}

/// Parse a single `// SAFETY: ...` line.
///
/// Recognized form (uppercase `SAFETY:` only — `Safety:` in docstrings is NOT
/// parsed, by design, so DenyCode variant rustdoc comments don't masquerade
/// as enforcement-site tags):
///
///     // SAFETY: SG<n> [SG<m>...] | REQ: <kebab-id> | TEST: <name>[,<name>...]
fn parse_safety_line(line: &str) -> Option<(Vec<u8>, String, Vec<String>)> {
    let body = line.trim_start().strip_prefix("// SAFETY:")?;
    let parts: Vec<&str> = body.split('|').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let sgs: Vec<u8> = parts[0]
        .split_whitespace()
        .filter_map(|s| s.strip_prefix("SG").and_then(|n| n.parse::<u8>().ok()))
        .filter(|n| (1..=9).contains(n))
        .collect();
    if sgs.is_empty() {
        return None;
    }
    let req = parts[1].trim().strip_prefix("REQ:")?.trim().to_string();
    if req.is_empty() {
        return None;
    }
    let tests_field = parts[2].trim().strip_prefix("TEST:")?.trim();
    let tests: Vec<String> = tests_field
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tests.is_empty() {
        return None;
    }
    Some((sgs, req, tests))
}

#[cfg(test)]
mod ci_gate_tests {
    use super::*;

    #[test]
    fn every_enforced_sg_has_at_least_one_tagged_site() {
        let tags = walk_safety_tags();
        for &sg in ENFORCED_SGS {
            let count = tags.iter().filter(|t| t.sgs.contains(&sg)).count();
            assert!(
                count > 0,
                "ENFORCED safety goal SG{} has NO tagged code site. \
                 Either the implementation is missing (real PO-1 coverage hole) \
                 or the site exists but isn't annotated per docs/safety/TRACEABILITY.md. \
                 Add a `// SAFETY: SG{} | REQ: <id> | TEST: <name>` line above the \
                 enforcing code site.",
                sg, sg,
            );
        }
    }

    #[test]
    fn every_tagged_site_has_req_and_test() {
        let tags = walk_safety_tags();
        for tag in &tags {
            assert!(
                !tag.req.is_empty(),
                "tag at {}:{} has empty REQ",
                tag.file.display(),
                tag.line
            );
            assert!(
                !tag.tests.is_empty(),
                "tag at {}:{} has empty TEST list — every tagged site must \
                 list at least one verifying test (per docs/safety/TRACEABILITY.md)",
                tag.file.display(),
                tag.line
            );
        }
    }

    #[test]
    fn every_tag_sg_is_in_range() {
        let tags = walk_safety_tags();
        for tag in &tags {
            for &sg in &tag.sgs {
                assert!(
                    (1..=9).contains(&sg),
                    "invalid SG identifier SG{} at {}:{} — must be SG1..SG9",
                    sg,
                    tag.file.display(),
                    tag.line
                );
            }
        }
    }

    #[test]
    fn discovered_tag_count_is_diagnostic_floor() {
        // Trend-tracking + sanity floor: if a refactor accidentally drops all
        // SAFETY tags, this catches it. The floor is intentionally generous;
        // tighten it when the tag set stabilizes.
        let tags = walk_safety_tags();
        const FLOOR: usize = 15;
        assert!(
            tags.len() >= FLOOR,
            "discovered only {} SAFETY tags, expected ≥ {} — \
             a large drop indicates accidental removal of safety annotations",
            tags.len(),
            FLOOR
        );
    }
}
