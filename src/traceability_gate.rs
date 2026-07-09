// src/traceability_gate.rs
//
// S3 traceability gate (issue #115) — parses `// SAFETY: ...` annotations
// across the Governor source tree (`src/`, `crates/`, `parko/crates/` — the SAME
// three roots the extraction script scans, so the CI gate and the generated
// matrix stay in lock-step) and asserts every Governor-ENFORCED safety goal
// (SG1..SG9 per OCCY_SAFETY_GOALS.md) has at least one tagged code site, every
// tagged site carries a non-empty REQ and TEST field, every SG identifier is in
// range, and — crucially — every identifier-shaped `TEST:` name RESOLVES to a
// real `fn` in the workspace (a renamed/deleted test can no longer leave the RTM
// claim dangling; prose entries like `see §7` are a documented escape).
//
// It ALSO reconciles the MANUAL RTM (`REQUIREMENTS_TRACEABILITY.md`, TR-NNN) with
// reality: each named test's ✓ (verified) / ✗ (gap) marker must match whether the
// test actually exists — a ✓ with no such fn is a false coverage claim, a ✗ whose
// test now exists is stale pessimism; both fail the gate. So both the generated
// SG/REQ matrix AND the hand-maintained TR matrix are drift-proof.
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

use std::collections::BTreeSet;
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

/// Walks `src/` + `crates/` + `parko/crates/` under the workspace root and returns
/// every parsed safety tag. Public so the extraction script can call it via a
/// thin Rust binary. These are the SAME three roots
/// `scripts/extract_safety_traceability.sh` scans, so the CI gate and the
/// generated matrix stay in lock-step — a kernel-crate tag (under `crates/`) is
/// now gated, not just documented.
pub fn walk_safety_tags() -> Vec<SafetyTag> {
    let mut tags = Vec::new();
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    for sub in &[
        root.join("src"),
        root.join("crates"),
        root.join("parko").join("crates"),
    ] {
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

/// Collect every `fn <name>` identifier defined anywhere in the workspace source
/// (`src/`, `crates/`, `parko/`, and the root `tests/` integration dir) — the
/// universe a `TEST:` reference must resolve into. An over-approximation (it
/// includes non-test fns too), which is SAFE for the gate: a renamed/deleted test
/// disappears from this set, so a dangling `TEST:` reference is still caught; the
/// extra fn names only ever admit a reference, never wrongly reject one.
#[must_use]
pub fn walk_test_fn_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    for sub in &[
        root.join("src"),
        root.join("crates"),
        root.join("parko"),
        root.join("tests"),
    ] {
        if sub.exists() {
            walk_fn_dir(sub, &mut names);
        }
    }
    names
}

fn walk_fn_dir(dir: &Path, names: &mut BTreeSet<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk_fn_dir(&path, names);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                extract_fn_names(&content, names);
            }
        }
    }
}

/// Extract `fn <ident>` names from a source string. Matches `fn ` at a word
/// boundary (so `…afn` doesn't match), reads the following Rust identifier, and
/// requires it to be followed (after optional whitespace) by `(` or `<` — the
/// shape of a real fn signature (`fn f(`, `pub fn f(`, `async fn f(`,
/// `pub(crate) fn f<`). That last check rejects PROSE like `... extern "C" fn
/// that dereferences ...`, whose captured word (`that`) is followed by more prose,
/// not a signature — so a comment can't seed a false-positive into the universe.
fn extract_fn_names(content: &str, names: &mut BTreeSet<String>) {
    let bytes = content.as_bytes();
    let mut i = 0;
    while let Some(rel) = content[i..].find("fn ") {
        let pos = i + rel;
        let boundary = pos == 0 || bytes[pos - 1].is_ascii_whitespace();
        let mut j = pos + 3;
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        if boundary {
            let start = j;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > start {
                // A real fn signature has `(` or `<` after the name (mod ws).
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if k < bytes.len() && (bytes[k] == b'(' || bytes[k] == b'<') {
                    names.insert(content[start..j].to_string());
                }
            }
        }
        i = pos + 3;
    }
}

/// Whether a `TEST:` entry is a test-fn identifier (vs a prose reference like
/// `see §7`). Only identifier-shaped entries are resolved against the fn universe;
/// prose entries are a documented escape for evidence that isn't a named `#[test]`.
#[cfg(test)]
fn is_test_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extract the test-fn identifier(s) from a marker-bearing backtick token. Handles
/// the qualified forms the RTM uses, not just a bare `test_x`:
/// - bare:          `test_x`                          → `test_x`
/// - path/mod:      `tests/f.rs::test_x`, `m::test_x`  → `test_x` (last `::` segment)
/// - brace group:   `m::{test_a, test_b}`             → `test_a`, `test_b`
///
/// An abbreviating `...` inside a brace group (the doc's "and others") is not an
/// identifier and is dropped. Anything that still isn't identifier-shaped after
/// stripping the qualifier is ignored.
#[cfg(test)]
fn tests_in_token(tok: &str) -> Vec<String> {
    let last_segment = |s: &str| s.rsplit("::").next().unwrap_or(s).trim().to_string();
    // Brace group: names live inside `{ ... }`; the part before `{` is the module.
    if let (Some(open), Some(close)) = (tok.find('{'), tok.rfind('}')) {
        if open < close {
            return tok[open + 1..close]
                .split(',')
                .map(|s| last_segment(s.trim()))
                .filter(|s| is_test_identifier(s))
                .collect();
        }
    }
    let name = last_segment(tok.trim());
    if is_test_identifier(&name) {
        vec![name]
    } else {
        vec![]
    }
}

/// Parse the `(test-name, is_verified)` markers from the manual RTM
/// (`REQUIREMENTS_TRACEABILITY.md`): each TR row's Test column marks a named test
/// `` `test_x` ✓ `` (verified — the test exists) or `` `test_x` ✗ `` (a gap — no
/// such test). The marker follows a backtick token, which may be bare, path/mod-
/// qualified, or a brace group ([`tests_in_token`]); implementation refs carry no
/// marker and so are never captured.
#[cfg(test)]
fn parse_rtm_markers(md: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in md.lines() {
        if !line.trim_start().starts_with("| TR-") {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '`' {
                let mut j = i + 1;
                let mut tok = String::new();
                while j < chars.len() && chars[j] != '`' {
                    tok.push(chars[j]);
                    j += 1;
                }
                if j < chars.len() {
                    let mut k = j + 1;
                    while k < chars.len() && chars[k].is_whitespace() {
                        k += 1;
                    }
                    if k < chars.len() && (chars[k] == '✓' || chars[k] == '✗') {
                        let verified = chars[k] == '✓';
                        for name in tests_in_token(&tok) {
                            out.push((name, verified));
                        }
                    }
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
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
                sg,
                sg,
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
    fn every_named_test_resolves_to_a_real_fn() {
        // The RTM claim "this requirement is verified by `test_x`" is only true if
        // `test_x` exists. This closes the drift the non-empty-TEST check misses: a
        // renamed/deleted test leaves the tag pointing at nothing. Every
        // identifier-shaped TEST entry must resolve to a real `fn` in the tree;
        // prose entries (e.g. `see §7` — a documented escape for non-`#[test]`
        // evidence) are skipped.
        let tags = walk_safety_tags();
        let fns = walk_test_fn_names();
        let mut dangling = Vec::new();
        for tag in &tags {
            for t in &tag.tests {
                if is_test_identifier(t) && !fns.contains(t) {
                    dangling.push(format!(
                        "{}:{} [REQ: {}] -> TEST `{}` has no matching fn",
                        tag.file.display(),
                        tag.line,
                        tag.req,
                        t
                    ));
                }
            }
        }
        assert!(
            dangling.is_empty(),
            "SAFETY tags name tests that do not exist (renamed/deleted?) — update the \
             tag or restore the test:\n{}",
            dangling.join("\n")
        );
    }

    #[test]
    fn rtm_markers_match_test_existence() {
        // The manual RTM (`REQUIREMENTS_TRACEABILITY.md`) marks each named test ✓
        // (verified — the test exists) or ✗ (a gap — no such test). Reconcile the
        // markers with reality so the safety matrix cannot silently lie: a ✓ whose
        // test does NOT exist is a FALSE coverage claim; a ✗ whose test NOW exists
        // is stale pessimism (the coverage landed — flip it to ✓).
        let root = std::env::var("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        let md_path = root.join("docs/safety/REQUIREMENTS_TRACEABILITY.md");
        let md = std::fs::read_to_string(&md_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", md_path.display()));
        let fns = walk_test_fn_names();

        let mut false_check = Vec::new();
        let mut stale_cross = Vec::new();
        for (name, verified) in parse_rtm_markers(&md) {
            let exists = fns.contains(&name);
            if verified && !exists {
                false_check.push(name);
            } else if !verified && exists {
                stale_cross.push(name);
            }
        }
        assert!(
            false_check.is_empty(),
            "RTM marks these tests ✓ (verified) but no such fn exists — a FALSE \
             coverage claim; write the test or flip the marker to ✗:\n  {}",
            false_check.join("\n  ")
        );
        assert!(
            stale_cross.is_empty(),
            "RTM marks these tests ✗ (a gap) but the test NOW EXISTS — stale; flip \
             the marker to ✓:\n  {}",
            stale_cross.join("\n  ")
        );
    }

    #[test]
    fn rtm_marker_parser_reads_ticks_and_crosses() {
        let md = "| TR-001 | req | `src/x.rs:f` | `test_alpha` ✓, `test_beta` ✗ |\n\
                  not a tr row `test_gamma` ✓\n";
        let m = parse_rtm_markers(md);
        assert_eq!(
            m,
            vec![
                ("test_alpha".to_string(), true),
                ("test_beta".to_string(), false)
            ]
        );
        // An implementation ref (path/colon token) carries no marker → not captured.
        assert!(!m.iter().any(|(n, _)| n.contains('/')));
    }

    #[test]
    fn rtm_marker_parser_handles_qualified_and_brace_forms() {
        // path::test, mod::test, and mod::{a, b, ...} all reduce to bare fn names,
        // carrying the row's marker (Copilot #852) — so the gate reconciles every
        // marked test, not only bare-identifier ones.
        let md = "| TR-9 | r | i | `tests/f.rs::test_path_qualified` ✓ |\n\
                  | TR-9a | r | i | `m::sub::test_mod_qualified` ✗ |\n\
                  | TR-9b | r | i | `admin_tests::{test_a, test_b, ...}` ✓ |\n";
        let m = parse_rtm_markers(md);
        assert_eq!(
            m,
            vec![
                ("test_path_qualified".to_string(), true),
                ("test_mod_qualified".to_string(), false),
                ("test_a".to_string(), true),
                ("test_b".to_string(), true),
            ],
            "qualified/brace tokens must reduce to bare fn names with the marker; `...` is dropped"
        );
    }

    #[test]
    fn fn_name_extractor_finds_definitions_but_not_calls_or_prose() {
        let mut names = BTreeSet::new();
        extract_fn_names(
            "pub fn alpha() {}\n    async fn beta<T>() {}\nlet x = notfn_delta;\n\
             /// every extern \"C\" fn that dereferences a raw pointer must be unsafe\n\
             fn spread_across ()\n{}",
            &mut names,
        );
        assert!(names.contains("alpha"), "plain fn");
        assert!(names.contains("beta"), "async generic fn");
        assert!(
            names.contains("spread_across"),
            "whitespace before `(` still resolves"
        );
        assert!(
            !names.contains("delta"),
            "`notfn_` must not match at a word boundary"
        );
        // The prose `fn that dereferences ...` must NOT seed `that` — it is not
        // followed by `(` or `<` (Copilot #851): a comment can't create a fn name.
        assert!(
            !names.contains("that"),
            "a `fn` in prose must not be captured"
        );
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
