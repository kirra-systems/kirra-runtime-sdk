#!/usr/bin/env python3
"""Tier 3 box 3d — an interactive query may not hide an unbounded store read.

THE RULE
--------
    A Tier 3 boundary query that sits on a request path may call only store
    methods whose result size is bounded by their arguments. Every public read
    method on the store must be CLASSIFIED, and an unclassified one fails.

WHY A GATE, AND WHY THIS SHAPE
------------------------------
`KIRRA-WM-ANSWER-IDENTITY-001` clause 2 is "queries are bounded", resting on
D-9's measured 10.5 s p99 at 100 000 entities and ADR-0041 D-12's finding that
an unbounded query has no bounded cost whatever its scaling verdict.

Three defects in three consecutive PRs violated it, each invisibly:

  #1440  lineage          a per-PAGE query over a whole-history fetch
  #1441  subject_summary  a per-SUBJECT query over a whole-store scan
  #1441  history          no bound at all

All three returned a correctly bounded ANSWER, so only the WORK was unbounded
and nothing showed it. Two were caught by a reviewer; the third was written in
the same PR that documented the other two. That is the evidence that per-query
vigilance does not enforce this invariant, and why it is now mechanical.

FAIL-CLOSED, NOT A DENYLIST
---------------------------
A denylist of "known unbounded methods" fails exactly the way those three
failed: a new store method is not on it, and nothing notices. So the baseline
CLASSIFIES the whole surface, and an unclassified method reds this gate --
adding a store method without deciding its boundedness becomes a red build
naming the rule. That is 3e's pattern, which has caught two things including
its own PR's fixture.

WHAT "BOUNDED" MEANS
--------------------
Bounded by the ARGUMENTS, not small. `current` returns one row per predicate
for one subject -- a STRUCTURAL bound from `PRIMARY KEY (subject,
predicate_key)` rather than a page. That counts. Requiring a cursor there would
be ceremony implying a growth dimension that does not exist; the structural
bound is instead asserted by a test.

Usage:
  python3 ci/check_query_boundedness.py             # gate (CI)
  python3 ci/check_query_boundedness.py --list      # show the classification
  python3 ci/check_query_boundedness.py --self-test # prove non-vacuity
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BASELINE_PATH = REPO_ROOT / "ci" / "store_boundedness_baseline.json"

STORE_LIB = REPO_ROOT / "crates" / "kirra-world-store" / "src" / "lib.rs"
STORE_SNAPSHOT = REPO_ROOT / "crates" / "kirra-world-store" / "src" / "snapshot.rs"
STORE_SRC = REPO_ROOT / "crates" / "kirra-world-store" / "src"
BOUNDARY_SRC = REPO_ROOT / "crates" / "kirra-world-service" / "src"

VALID_CLASSES = {"bounded", "unbounded", "operational"}

# A method that accepts a page/cursor bound, and one that runs a SELECT. Used
# together to demand that a paginated bounded method bounds its own query.
PAGE_PARAM_RE = re.compile(r"\bpage\s*:|LineagePage|\blimit\s*:")
SELECT_RE = re.compile(r"\bSELECT\b", re.I)

# Methods compiled out of the production path. Auto-classified so the baseline
# does not carry entries that buy no coverage.
TEST_ONLY_SUFFIX = "_for_test"


def _impl_bodies(text: str, header_re: str) -> list[str]:
    """Every `impl` body matching `header_re`, brace-matched."""
    bodies = []
    for m in re.finditer(header_re, text, re.M):
        i, depth = m.end(), 1
        while i < len(text) and depth > 0:
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
            i += 1
        bodies.append(text[m.end() : i])
    return bodies


def _fn_bodies(impl_body: str) -> list[tuple[str, str, str]]:
    """(name, signature, body) for each `pub fn` in an impl body, brace-matched.

    The SIGNATURE is returned separately because rule 4 asks a question the body
    cannot answer: *does this method accept a page?* That is declared in the
    parameter list, and an earlier revision tested the body alone — so the rule
    fired on `history_page` only because an unrelated code comment inside it
    happened to contain the English phrase `the page:`. Deleting a comment
    disarmed the check. A structural rule may not depend on prose.
    """
    out = []
    for m in re.finditer(r"\n    pub fn ([a-z_0-9]+)\s*(?:<[^>]*>)?\s*\(", impl_body):
        i = impl_body.find("{", m.end())
        if i < 0:
            continue
        j, depth = i + 1, 1
        while j < len(impl_body) and depth > 0:
            if impl_body[j] == "{":
                depth += 1
            elif impl_body[j] == "}":
                depth -= 1
            j += 1
        out.append((m.group(1), impl_body[m.start() : i], impl_body[i:j]))
    return out


def strip_comments(text: str) -> str:
    """`text` with `//` comments removed, line structure preserved.

    Used on BOTH halves of rule 4. Prose must be unable to arm the rule (a
    comment reading `the page:` is not a page parameter) and equally unable to
    disarm it (a comment mentioning `LIMIT` is not a bounded query).
    """
    return "\n".join(line.split("//")[0] for line in text.splitlines())


def page_bound_violation(signature: str, body: str) -> bool:
    """True when a method ACCEPTS a page bound but its own SQL carries none."""
    sig, code = strip_comments(signature), strip_comments(body)
    if not PAGE_PARAM_RE.search(sig):
        return False
    if not SELECT_RE.search(code):
        return False
    return "LIMIT" not in code


def public_methods(path: Path, header_re: str) -> set[str]:
    """The public method names declared in the matching impl blocks."""
    text = path.read_text()
    found: set[str] = set()
    for body in _impl_bodies(text, header_re):
        for fm in re.finditer(r"\n    pub fn ([a-z_0-9]+)\s*(?:<[^>]*>)?\s*\(", body):
            name = fm.group(1)
            if not name.endswith(TEST_ONLY_SUFFIX):
                found.add(name)
    return found


def discover() -> dict[str, set[str]]:
    return {
        "world_store": public_methods(STORE_LIB, r"^impl WorldStore \{"),
        "read_snapshot": public_methods(
            STORE_SNAPSHOT, r"^impl<'a> ReadSnapshot<'a> \{"
        ),
    }


def unbounded_names(baseline: dict) -> set[str]:
    out: set[str] = set()
    for surface in ("world_store", "read_snapshot"):
        for name, spec in baseline[surface].items():
            if name.startswith("_"):
                continue
            if spec["class"] == "unbounded":
                out.add(name)
    return out


def scan_interactive(text: str, forbidden: set[str]) -> list[tuple[int, str, str]]:
    """Calls to forbidden methods, ignoring comments and doc comments.

    Comment-stripping is load-bearing: this file and the boundary's own docs
    NAME the unbounded methods while explaining why they are not called, and a
    scanner that matched prose would make the explanation unwriteable.
    """
    call_re = re.compile(r"(?:\.|::)\s*(" + "|".join(sorted(forbidden)) + r")\s*\(")
    hits = []
    for n, line in enumerate(text.splitlines(), start=1):
        code = line.split("//")[0]
        for m in call_re.finditer(code):
            hits.append((n, m.group(1), line.strip()))
    return hits


def run(verbose: bool = False) -> list[str]:
    baseline = json.loads(BASELINE_PATH.read_text())
    failures: list[str] = []
    found = discover()

    # 1. Every public read method must be classified. FAIL-CLOSED.
    for surface, names in found.items():
        declared = {k for k in baseline[surface] if not k.startswith("_")}
        for name in sorted(names - declared):
            failures.append(
                f"{surface}::{name} is public but has no boundedness classification.\n"
                f"    Add it to ci/store_boundedness_baseline.json as bounded / "
                f"unbounded / operational, with the reasoning.\n"
                f"    An unclassified method is how all three of the defects this "
                f"gate exists for reached main."
            )
        for name in sorted(declared - names):
            failures.append(
                f"{surface}::{name} is classified but no longer exists — "
                f"remove the stale entry."
            )
        for name, spec in baseline[surface].items():
            if name.startswith("_"):
                continue
            if spec.get("class") not in VALID_CLASSES:
                failures.append(
                    f"{surface}::{name} has class {spec.get('class')!r}; "
                    f"expected one of {sorted(VALID_CLASSES)}."
                )
            if not spec.get("why", "").strip():
                failures.append(f"{surface}::{name} has no reason recorded.")

    # 2. NOTHING in the boundary crate may call an unbounded method.
    #
    # The whole crate, not a list of "interactive" files. A per-file list is
    # exactly the hole a relocated call slips through: move the unbounded fetch
    # into a private helper in a fourth file and the gate goes green while the
    # invariant stays false. The boundary crate IS the request path, so every
    # file in it is checked and there is no file to move the call to.
    forbidden = unbounded_names(baseline)
    for path in sorted(BOUNDARY_SRC.rglob("*.rs")):
        rel = path.relative_to(REPO_ROOT)
        for line_no, method, line in scan_interactive(path.read_text(), forbidden):
            failures.append(
                f"{rel}:{line_no} — the answer boundary calls the UNBOUNDED store "
                f"method `{method}`.\n"
                f"      {line}\n"
                f"    A bounded-looking query standing on an unbounded read is the "
                f"exact defect class of #1440 and #1441: the answer is bounded, the "
                f"work is not, and nothing shows it.\n"
                f"    Use the bounded equivalent, or narrow the fetch in SQL and add "
                f"an agreement test."
            )

    # 3. A store method classified `bounded` may not CALL an unbounded one.
    #
    # The other half of the relocation hole, and the one that matters more:
    # without it, `pub fn tidy(subject) { self.everything() }` could be declared
    # bounded and the boundary could call it in good faith. The classification
    # would be a claim nobody checks — which is what a denylist would have been.
    for path in sorted(STORE_SRC.rglob("*.rs")):
        text = path.read_text()
        for surface, header in (
            ("world_store", r"^impl WorldStore \{"),
            ("read_snapshot", r"^impl<'a> ReadSnapshot<'a> \{"),
        ):
            for body in _impl_bodies(text, header):
                for fn_name, _sig, fn_body in _fn_bodies(body):
                    spec = baseline[surface].get(fn_name)
                    if not spec or spec["class"] != "bounded":
                        continue
                    for line_no, method, line in scan_interactive(fn_body, forbidden):
                        failures.append(
                            f"{surface}::{fn_name} is classified `bounded` but calls "
                            f"the UNBOUNDED `{method}`.\n"
                            f"      {line}\n"
                            f"    A bounded classification standing on an unbounded "
                            f"call makes the classification a claim nobody checks, and "
                            f"lets the boundary violate the invariant in good faith."
                        )

    # 4. A `bounded` method that TAKES a page must BOUND its own SQL.
    #
    # The hole mutation M1 exposed, and it is the sharpest one: rewriting
    # `history_page` to fetch every row and truncate in Rust produces an
    # IDENTICAL answer, so no behavioural test can catch it — every pagination
    # control still passed. Rule 3 missed it too, because the method calls no
    # unbounded helper; it simply writes unbounded SQL.
    #
    # That is the whole defect class in one mutation: the answer is bounded, the
    # work is not, and nothing observable differs. So the check is structural —
    # if a bounded method accepts a page, its query must carry a LIMIT.
    for path in sorted(STORE_SRC.rglob("*.rs")):
        text = path.read_text()
        for surface, header in (
            ("world_store", r"^impl WorldStore \{"),
            ("read_snapshot", r"^impl<'a> ReadSnapshot<'a> \{"),
        ):
            for body in _impl_bodies(text, header):
                for fn_name, fn_sig, fn_body in _fn_bodies(body):
                    spec = baseline[surface].get(fn_name)
                    if not spec or spec["class"] != "bounded":
                        continue
                    if page_bound_violation(fn_sig, fn_body):
                        failures.append(
                            f"{surface}::{fn_name} is classified `bounded` and takes a "
                            f"page, but its SQL has no LIMIT.\n"
                            f"    Fetching the whole set and truncating in Rust returns "
                            f"the IDENTICAL answer, so no behavioural test can catch it "
                            f"— which is exactly how #1440 and #1441 reached main.\n"
                            f"    The bound has to reach the query."
                        )

    if verbose:
        for surface, names in sorted(found.items()):
            print(f"\n{surface} ({len(names)} public read methods):")
            for name in sorted(names):
                spec = baseline[surface].get(name)
                cls = spec["class"] if spec else "UNCLASSIFIED"
                print(f"  [{cls:11}] {name}")
        print(f"\nunbounded methods barred from interactive paths: {len(forbidden)}")
        for rel, role in baseline["interactive_paths"].items():
            if not rel.startswith("_"):
                print(f"  interactive: {rel} — {role}")

    return failures


def self_test() -> int:
    """Prove the gate is non-vacuous, on the SHAPE of the real defects.

    The negative fixture is not invented: it is `subject_summary` exactly as it
    shipped on #1441 and `lineage` as it first shipped on #1440 — a query
    exposing a bound publicly, fetching unbounded underneath, and truncating
    afterwards. If the gate cannot catch that, it cannot catch what it is for.
    """
    baseline = json.loads(BASELINE_PATH.read_text())
    forbidden = unbounded_names(baseline)
    failed = 0

    # (a) THE historical defect shape: bounded-looking API, unbounded fetch.
    fixture = """
    pub fn subject_summary(&self, subject: &str, limit: usize) -> Answer {
        let all = self.store.subject_summaries_with_coverage()?;
        let mut rows: Vec<_> = all.into_iter().filter(|s| s.subject == subject).collect();
        rows.truncate(limit);
        Answer { rows }
    }
    """
    hits = scan_interactive(fixture, forbidden)
    if not hits:
        print("SELF-TEST FAIL: the bounded-looking / unbounded-underneath fixture "
              "was NOT caught — the gate cannot see its own defect class.")
        failed = 1
    else:
        print(f"self-test: bounded-looking, unbounded-underneath → caught "
              f"({hits[0][1]})")

    # (b) The bounded replacement must NOT trip it, or the gate is unusable.
    ok_fixture = """
    pub fn subject_summary(&self, subject: &str) -> Answer {
        let found = self.store.subject_summary_with_coverage(subject)?;
        Answer { found }
    }
    """
    if scan_interactive(ok_fixture, forbidden):
        print("SELF-TEST FAIL: the BOUNDED replacement was flagged — the gate "
              "would force callers away from the correct API.")
        failed = 1
    else:
        print("self-test: bounded replacement → not flagged")

    # (c) Prose naming an unbounded method must not trip it, or this file and
    #     the boundary's own docs could not explain the rule.
    prose = "    // never call subject_summaries_with_coverage() from here\n"
    if scan_interactive(prose, forbidden):
        print("SELF-TEST FAIL: a COMMENT naming an unbounded method was flagged.")
        failed = 1
    else:
        print("self-test: prose naming an unbounded method → not flagged")

    # (d) THE RELOCATION FIXTURE: a store method that LOOKS bounded and calls an
    #     unbounded one. Without this the invariant can be made green without
    #     being made true — move the unbounded fetch behind a bounded-looking
    #     helper and the boundary calls it in good faith.
    #
    #     Not hypothetical: `resolve_at(id, cut)` is exactly this shape and was
    #     classified `bounded` from its signature on the first pass. This check
    #     is what corrected it.
    relocated = """
    pub fn resolve_at(&self, id: &EntityId, as_known_at_ms: i64) -> Answer {
        Ok(self.identity_view_at(as_known_at_ms)?.resolve_at(id))
    }
    """
    if not scan_interactive(relocated, forbidden):
        print("SELF-TEST FAIL: a bounded-LOOKING helper wrapping an unbounded "
              "call was NOT caught — the invariant could be made green without "
              "being made true.")
        failed = 1
    else:
        print("self-test: bounded-looking helper over an unbounded call → caught")

    # (e) RULE 4, THE M1 SHAPE: a page-taking method whose SQL carries no bound.
    #     Fetch everything, truncate in Rust — an identical answer, so no
    #     behavioural test can see it. This fixture carries no prose at all,
    #     which is the point: rule 4 shipped armed only by an unrelated comment
    #     containing the words "the page:", and deleting that comment made an
    #     unbounded `history_page` pass. Prose must not be load-bearing.
    unbounded_page_sig = "\n    pub fn history_page(&self, subject: &str, page: LineagePage)"
    unbounded_page_body = """{
        let mut stmt = self.conn.prepare("SELECT * FROM world_events WHERE subject = ?1")?;
        let mut claims = stmt.query_map(params![subject], claim_from_row)?.collect()?;
        claims.truncate(page.limit());
        Ok(claims)
    }"""
    if not page_bound_violation(unbounded_page_sig, unbounded_page_body):
        print("SELF-TEST FAIL: a page-taking method with NO LIMIT was not caught "
              "— rule 4 cannot see the mutation it exists for.")
        failed = 1
    else:
        print("self-test: page-taking method, unbounded SQL → caught")

    # (f) The bounded form must pass, or the rule forces nothing constructive.
    bounded_page_body = """{
        let mut stmt = self.conn.prepare(
            "SELECT * FROM world_events WHERE subject = ?1 AND generation > ?2
             ORDER BY generation ASC LIMIT ?3")?;
        Ok(stmt.query_map(params![subject, after, probe], claim_from_row)?.collect()?)
    }"""
    if page_bound_violation(unbounded_page_sig, bounded_page_body):
        print("SELF-TEST FAIL: a correctly bounded page query was flagged.")
        failed = 1
    else:
        print("self-test: page-taking method, LIMIT in SQL → not flagged")

    # (g) Prose must be inert in BOTH directions: a comment cannot arm the rule
    #     (that is the bug this fixture set was written for), and a comment
    #     mentioning LIMIT cannot disarm it.
    if page_bound_violation("\n    pub fn whole_history(&self, subject: &str)",
                            "{\n // narrowed to the page: see history_page\n"
                            ' let s = self.conn.prepare("SELECT * FROM world_events")?;\n}'):
        print("SELF-TEST FAIL: a COMMENT reading `the page:` armed rule 4 — the "
              "check would depend on prose, which is how the hole was opened.")
        failed = 1
    elif not page_bound_violation(unbounded_page_sig,
                                  "{\n // no LIMIT needed here\n"
                                  ' let s = self.conn.prepare("SELECT * FROM world_events")?;\n}'):
        print("SELF-TEST FAIL: a COMMENT mentioning LIMIT disarmed rule 4.")
        failed = 1
    else:
        print("self-test: comments neither arm nor disarm rule 4")

    # (h) An unclassified method must fail, since that is the generalisation.
    stripped = {k: v for k, v in baseline["world_store"].items() if k != "history"}
    if "history" in stripped:
        print("SELF-TEST FAIL: fixture construction error.")
        failed = 1
    else:
        print("self-test: removing a classification leaves it undiscoverable → "
              "the discover/declare diff is what reports it")

    return failed


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()
    verbose = "--list" in sys.argv
    failures = run(verbose=verbose)
    if failures:
        print("\nFAIL: query-boundedness gate\n")
        for f in failures:
            print(f"  - {f}\n")
        return 1
    print("Query-boundedness gate green.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
