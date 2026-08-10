#!/usr/bin/env python3
"""Tier 3 box 3d — domain consumers read the world through the ANSWER BOUNDARY.

THE RULE
--------
    A crate that asks the world DOMAIN questions must go through
    `kirra_world_service::WorldView`. It may not call the projection-read API on
    `WorldStore` directly.

WHY A GATE AND NOT A PARAGRAPH
------------------------------
This bypass is not hypothetical and the fixture is not synthetic: it is the code
that shipped. `mission_context` read `ProjectedClaim`'s public fields straight
off `store.current(..)` — no validity, no trust axes, no provenance handle, and
no identity resolution — until boxes 3a (#1430) and 3c (#1431) repaired it. The
audit that found it recorded WHY it was easy to miss: the boundary had no
consumer, so nothing noticed what it could not express.

Repairing a bypass without gating it means repairing it again. Six PRs from now
`store.current(..)` is one autocomplete away, it compiles, it returns plausible
data, and every negative control still passes — the audit measured exactly that
about the mechanical translation it predicted. The gate converts a silent
regression into a red build naming the rule.

    An invariant with no gate is prose. -- WM_SCOPE.md, Tier 3

SCOPE: WHO IS CHECKED
---------------------
Only crates that DEPEND on `kirra-world-store`. A crate without that dependency
cannot make these calls, so scanning it could only manufacture false positives
from unrelated methods that happen to be called `history` or `candidates`.

That scope is self-maintaining, which is the point: a NEW crate that takes the
dependency is checked from its first commit, without anyone remembering to add
it here. Undoing 3a by writing a fresh consumer is the same regression as
undoing it in place.

Permitted readers are named in the baseline with a reason each, so adding one is
a visible, argued diff rather than an edit to a regex.

SCOPE: WHAT IS CHECKED
----------------------
`src/` only — the production read path. A test that reads rows directly is
testing the store, and several deliberately do: `answer_boundary.rs` plants
`world_current` rows with raw SQL to reach a state the sanctioned write path
cannot produce. Forbidding that would block the tests that prove the boundary
fails closed. The bypass this gate exists to stop lived in `src/`, and `src/`
cannot call test code, so gating `src/` closes the production path completely.

OPERATIONAL READS ARE CARVED OUT BY CONSTRUCTION
------------------------------------------------
WM_SCOPE.md names `verify_chain`, `schema_version`, backup/export, the retention
driver, the compaction planner and the WM-2 harness as legitimate readers below
the engine, and says a rule forbidding them "would be false on the day it was
written". None of them appears in `DOMAIN_READ_METHODS` below: the list is the
domain-question entry points, not every method that touches a table. The carve-
out therefore needs no exception list to maintain, because those calls are never
matched in the first place.

Usage:
  python3 ci/check_world_answer_boundary.py             # gate (CI)
  python3 ci/check_world_answer_boundary.py --list      # show scope + findings
  python3 ci/check_world_answer_boundary.py --self-test # prove non-vacuity
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BASELINE_PATH = REPO_ROOT / "ci" / "world_answer_boundary_baseline.json"

# The DOMAIN-question entry points on the store. Reaching any of these is how a
# caller obtains a `ProjectedClaim` or an `IdentityView` without the boundary's
# validity / trust / provenance / identity wrapping.
#
# Deliberately NOT here: `verify_chain`, `schema_version`, `fold`, `append`,
# backup/export, retention and compaction. Those are operational or write-path
# calls that WM_SCOPE explicitly permits below the engine.
DOMAIN_READ_METHODS = (
    "current",
    "current_all",
    "as_of",
    "history",
    "candidates",
    "read_snapshot",
    "identity_view",
    "identity_view_at",
    "resolve_at",
    "load_entity_projection",
)

# Both call syntaxes. `.current(..)` is how anyone would write it; but
# `WorldStore::current(store, ..)` is the same call in UFCS form, and a regex
# that only knew the first would advertise zero tolerance while leaving a
# syntactic door open — the exact overclaim this gate exists to prevent
# elsewhere. `::` also catches the aliased (`WS::current`) and fully-qualified
# (`<WorldStore as T>::current`) spellings.
_CALL_RE = re.compile(r"(?:\.|::)\s*(" + "|".join(DOMAIN_READ_METHODS) + r")\s*\(")

# Dependency tables whose presence means the crate's `src/` can reach the store.
# `dev-dependencies` is deliberately NOT here: a dev-dependency is unavailable to
# `src/`, which is the only tree this gate scans, so a crate that dev-depends on
# the store cannot commit the violation. Including it would put crates in the
# scope report that were never at risk — `kirra-mission-orchestrator` is exactly
# that case, and its own module docs say so.
_DEP_TABLES = ("dependencies", "build-dependencies")


def _strip_comments_and_strings(text: str) -> str:
    """Blank out block comments, line comments and string literals.

    Without this a doc comment showing the anti-pattern — `read_view.rs` quotes
    `store.current("robot-01", now)?[0].payload` precisely to explain why the
    boundary exists — would be reported as the violation it is warning about.
    """
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    out: list[str] = []
    for raw in text.splitlines():
        # A line comment, doc comment or inner doc comment.
        line = re.sub(r"(?<!:)//.*$", "", raw)
        # String literals (non-greedy, escaped quotes tolerated).
        line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
        out.append(line)
    return "\n".join(out)


def crate_name(manifest: Path) -> str:
    for line in manifest.read_text(encoding="utf-8").splitlines():
        m = re.match(r'\s*name\s*=\s*"([^"]+)"', line)
        if m:
            return m.group(1)
    return manifest.parent.name


def depends_on_world_store(manifest: Path) -> bool:
    """True iff this manifest names `kirra-world-store` in a dependency table
    reachable from `src/`.

    Parsed, not substring-matched. A raw `"kirra-world-store" in text` search
    also matches the crate name in a COMMENT, and two manifests in this repo
    discuss the dependency in prose precisely to explain why they must not have
    it — `wm2-persistence-harness` was pulled into scope on the strength of a
    comment saying it stays out. A gate whose scope is decided by prose it does
    not understand reports a dependency graph that does not exist.

    Read from the manifest rather than via `cargo metadata` so the gate runs
    standalone in CI without a build.
    """
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    tables = [data.get(t, {}) for t in _DEP_TABLES]
    # `[target.'cfg(..)'.dependencies]` reaches `src/` just as plainly.
    for target in data.get("target", {}).values():
        tables.extend(target.get(t, {}) for t in _DEP_TABLES)
    return any("kirra-world-store" in table for table in tables)


def load_baseline() -> dict:
    return json.loads(BASELINE_PATH.read_text(encoding="utf-8"))


def scan_source(text: str, label: str) -> list[tuple[int, str, str]]:
    """Return (line_no, method, source_line) for each domain read in `text`."""
    cleaned = _strip_comments_and_strings(text)
    found: list[tuple[int, str, str]] = []
    for n, line in enumerate(cleaned.splitlines(), start=1):
        for m in _CALL_RE.finditer(line):
            found.append((n, m.group(1), line.strip()))
    return found


def in_scope_crates() -> list[tuple[str, Path]]:
    """Every crate that depends on `kirra-world-store`, as (name, crate_dir)."""
    out: list[tuple[str, Path]] = []
    for manifest in sorted(REPO_ROOT.glob("crates/*/Cargo.toml")) + sorted(
        REPO_ROOT.glob("tools/*/Cargo.toml")
    ):
        if depends_on_world_store(manifest):
            out.append((crate_name(manifest), manifest.parent))
    return out


def stale_exemptions() -> list[str]:
    """Permitted readers that are not actually in scope.

    An exemption list only stays honest if entries disappear when they stop
    being needed. This gate shipped with one such entry already —
    `wm2-persistence-harness`, admitted on a substring match against a comment —
    carrying a written justification for an exemption it never required. That is
    how a carve-out list rots: every entry looks reasoned, and nothing checks
    whether it is still load-bearing.
    """
    in_scope = {name for name, _ in in_scope_crates()}
    return sorted(set(load_baseline()["permitted_readers"]) - in_scope)


def collect() -> tuple[list[dict], list[str], list[str]]:
    """Returns (violations, checked_crates, permitted_crates_seen)."""
    baseline = load_baseline()
    permitted = set(baseline["permitted_readers"])

    violations: list[dict] = []
    checked: list[str] = []
    seen_permitted: list[str] = []

    for name, crate_dir in in_scope_crates():
        if name in permitted:
            seen_permitted.append(name)
            continue
        checked.append(name)
        src = crate_dir / "src"
        if not src.is_dir():
            continue
        for path in sorted(src.rglob("*.rs")):
            rel = path.relative_to(REPO_ROOT).as_posix()
            for line_no, method, line in scan_source(
                path.read_text(encoding="utf-8"), rel
            ):
                violations.append(
                    {"file": rel, "line": line_no, "method": method, "source": line}
                )
    return violations, checked, seen_permitted


# ---------------------------------------------------------------------------
# Self-test: the fixture is the code that actually shipped
# ---------------------------------------------------------------------------

# `mission_context` as it stood at 0ad203ee, before box 3a. Reproduced verbatim
# rather than approximated: a gate whose negative fixture is a synthetic
# `store.current()` proves it can find a string, not that it would have found
# THIS bug. The same discipline as the symbolic-seam gate's control 3.
PRE_FIX_MISSION_CONTEXT = '''
pub fn mission_context(
    store: &WorldStore,
    subject: &ContextId,
    relation: &ContextId,
    candidates: &[ContextId],
    now_ms: i64,
) -> Result<ProposalContext, ContextError> {
    let claims = store.current(subject.as_str(), now_ms)?;

    let preferred = claims.iter().find_map(|c| {
        let predicate = c.predicate.as_deref()?;
        if predicate != relation.as_str() {
            return None;
        }
        let object = c.object.as_deref()?;
        candidates.iter().find(|cand| cand.as_str() == object)
    });
    ...
}
'''

# The same function today: the store is still NAMED and still passed along, but
# every read goes through the boundary. A gate that flagged this would be
# unusable, because taking a `&WorldStore` is how you reach `WorldView::new`.
POST_FIX_MISSION_CONTEXT = '''
pub fn mission_context(
    store: &WorldStore,
    subject: &ContextId,
    relation: &ContextId,
    candidates: &[ContextId],
    now_ms: i64,
    staleness_budget_ms: Option<u64>,
) -> Result<ProposalContext, ContextError> {
    let view = WorldView::new(store, staleness_budget_ms);

    let answers = match view.ask(subject.as_str(), now_ms)?.into_lookup() {
        WorldLookup::Answered(answers) => answers,
        WorldLookup::Unknown(reason) => {
            return Ok(ProposalContext::silent(candidates, silence));
        }
    };
    ...
}
'''

RESULTS: list[tuple[bool, str, str]] = []


def record(ok: bool, name: str, detail: str = "") -> None:
    RESULTS.append((ok, name, detail))


def t01_the_real_pre_fix_bypass_is_caught() -> None:
    found = scan_source(PRE_FIX_MISSION_CONTEXT, "pre_fix")
    record(
        bool(found) and any(m == "current" for _, m, _ in found),
        "t01_the_real_pre_fix_bypass_is_caught",
        "" if found else "the shipped bypass would pass this gate",
    )


def t02_the_repaired_function_is_clean() -> None:
    found = scan_source(POST_FIX_MISSION_CONTEXT, "post_fix")
    record(
        not found,
        "t02_the_repaired_function_is_clean",
        "" if not found else f"false positive on the boundary-routed form: {found}",
    )


def t03_a_doc_comment_showing_the_antipattern_is_not_a_violation() -> None:
    src = '//! let payload = &store.current("robot-01", now)?[0].payload;\nfn f() {}'
    record(
        not scan_source(src, "doc"),
        "t03_a_doc_comment_showing_the_antipattern_is_not_a_violation",
        "the boundary's own docs quote the anti-pattern to explain it",
    )


def t04_a_string_literal_is_not_a_violation() -> None:
    src = 'fn f() { let sql = "SELECT .current( FROM x"; }'
    record(
        not scan_source(src, "str"),
        "t04_a_string_literal_is_not_a_violation",
    )


def t05_every_domain_read_method_is_detected() -> None:
    missed = [
        m for m in DOMAIN_READ_METHODS if not scan_source(f"fn f() {{ s.{m}(a); }}", m)
    ]
    record(
        not missed,
        "t05_every_domain_read_method_is_detected",
        f"declared but undetectable: {missed}" if missed else "",
    )


def t06_operational_reads_are_not_flagged() -> None:
    # The carve-out WM_SCOPE names explicitly. These must never match, and they
    # are not exceptions -- they are simply not domain reads.
    operational = [
        "verify_chain",
        "schema_version",
        "fold",
        "fold_entity_projection",
        "append",
        "compact_range",
        "projected_row_count",
    ]
    flagged = [m for m in operational if scan_source(f"fn f() {{ s.{m}(a); }}", m)]
    record(
        not flagged,
        "t06_operational_reads_are_not_flagged",
        f"a rule forbidding these would be false the day it was written: {flagged}"
        if flagged
        else "",
    )


def t07a_ufcs_is_caught_as_well_as_method_syntax() -> None:
    method = scan_source("fn f() { store.current(s, n); }", "m")
    ufcs = scan_source("fn f() { WorldStore::current(store, s, n); }", "u")
    aliased = scan_source("fn f() { WS::current(store, s, n); }", "a")
    qualified = scan_source("fn f() { <WorldStore as T>::current(store, s); }", "q")
    missing = [
        n
        for n, got in [
            ("method", method),
            ("ufcs", ufcs),
            ("aliased", aliased),
            ("fully-qualified", qualified),
        ]
        if not got
    ]
    record(
        not missing,
        "t07a_ufcs_is_caught_as_well_as_method_syntax",
        f"same call, undetected spelling(s): {missing}" if missing else "",
    )


def t07b_a_comment_mentioning_the_crate_is_not_a_dependency() -> None:
    import tempfile

    manifest = (
        '# The harness must not depend on `kirra-world-store`. Its manifest\n'
        '# says so, and this comment is the reason why.\n'
        '[package]\nname = "prose-only"\nversion = "0.1.0"\n'
        '[dependencies]\nserde = "1"\n'
    )
    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "Cargo.toml"
        path.write_text(manifest, encoding="utf-8")
        record(
            not depends_on_world_store(path),
            "t07b_a_comment_mentioning_the_crate_is_not_a_dependency",
            "a crate is in scope because a comment names the dependency it avoids",
        )


def t07c_a_real_dependency_is_still_detected() -> None:
    import tempfile

    manifest = (
        '[package]\nname = "real"\nversion = "0.1.0"\n'
        '[dependencies]\nkirra-world-store = { path = "../x" }\n'
    )
    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "Cargo.toml"
        path.write_text(manifest, encoding="utf-8")
        record(
            depends_on_world_store(path),
            "t07c_a_real_dependency_is_still_detected",
            "the parse tightened past the point of detecting anything",
        )


def t07d_no_permitted_reader_is_stale() -> None:
    stale = stale_exemptions()
    record(
        not stale,
        "t07d_no_permitted_reader_is_stale",
        f"exemptions carried for crates not in scope: {stale}" if stale else "",
    )


def t07_the_live_tree_is_clean() -> None:
    violations, checked, _ = collect()
    record(
        not violations and bool(checked),
        "t07_the_live_tree_is_clean",
        f"violations: {violations}"
        if violations
        else ("no crate is in scope — the gate is watching nothing" if not checked else ""),
    )


def t08_the_gate_watches_the_repaired_consumer() -> None:
    # The point of the whole exercise: `kirra-proposal-context` must be IN the
    # checked set. If it ever drifts into `permitted_readers`, this gate has
    # been neutered and the box it locks is open again.
    _, checked, _ = collect()
    record(
        "kirra-proposal-context" in checked,
        "t08_the_gate_watches_the_repaired_consumer",
        f"the repaired consumer is not being checked; checked = {checked}",
    )


ALL = [v for k, v in sorted(globals().items()) if k.startswith("t") and k[1:3].isdigit()]

# A case that is not COLLECTED is the most literal form of a test that cannot
# fail -- the same guard the symbolic-seam self-test carries, for the same
# reason it needed one.
_UNCOLLECTED = [
    k
    for k, v in globals().items()
    if k.startswith("t")
    and callable(v)
    and getattr(v, "__module__", None) == __name__
    and v not in ALL
]


def self_test() -> int:
    if _UNCOLLECTED:
        print(f"FAIL: these cases are never run: {sorted(_UNCOLLECTED)}")
        return 1
    for fn in ALL:
        try:
            fn()
        except Exception as exc:  # a crashing test is a failing test
            record(False, fn.__name__, f"raised {type(exc).__name__}: {exc}")

    failed = [r for r in RESULTS if not r[0]]
    for ok, name, detail in RESULTS:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
        if not ok and detail:
            print(f"        {detail}")
    print()
    print(f"{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return 1 if failed else 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()

    baseline = load_baseline()
    violations, checked, permitted_seen = collect()

    if "--list" in sys.argv:
        print("Answer-boundary gate scope (crates depending on kirra-world-store):")
        for name in permitted_seen:
            print(f"  [PERMITTED] {name} — {baseline['permitted_readers'][name]}")
        for name in checked:
            print(f"  [CHECKED]   {name}")
        print()
        if violations:
            for v in violations:
                print(f"  {v['file']}:{v['line']}  {v['method']}(..)  {v['source']}")
        else:
            print("  no direct domain reads")
        return 0

    stale = stale_exemptions()
    if stale:
        print("Answer-boundary gate FAILED — stale exemption(s) in the baseline.")
        print()
        for name in stale:
            print(f"  {name} is on `permitted_readers` but is not in scope.")
        print()
        print("  An exemption for a crate the gate never scans is a written")
        print("  justification for a decision nobody is making. Remove it, or")
        print("  record it under `not_listed_because_never_in_scope`.")
        return 1

    ceiling = baseline["max_violations"]
    if len(violations) > ceiling:
        print("Answer-boundary gate FAILED — direct domain read below the engine.")
        print()
        for v in violations:
            print(f"  {v['file']}:{v['line']}")
            print(f"      {v['source']}")
            print(f"      `{v['method']}(..)` reads a projection directly.")
        print()
        print("  A domain consumer must ask through the answer boundary:")
        print("      let view = WorldView::new(store, staleness_budget_ms);")
        print("      let answers = view.ask(subject, now_ms)?;")
        print()
        print("  This is the bypass boxes 3a and 3c repaired. Reading the row")
        print("  directly loses validity, trust axes, provenance and identity")
        print("  resolution — and compiles, and returns plausible data.")
        print()
        print(f"  ({len(violations)} found, ceiling {ceiling}.)")
        return 1

    if not checked:
        print("Answer-boundary gate FAILED — no crate is in scope.")
        print("  Every world-store dependant is on the permitted list, so this")
        print("  gate is watching nothing. That is how a ratchet dies quietly.")
        return 1

    print(
        f"Answer-boundary gate green: {len(checked)} domain crate(s) checked "
        f"({', '.join(checked)}), 0 direct projection reads."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
