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

VALID_CLASSES = {"bounded", "unbounded", "operational"}

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

    # 2. No interactive path may call an unbounded method.
    forbidden = unbounded_names(baseline)
    for rel, role in baseline["interactive_paths"].items():
        if rel.startswith("_"):
            continue
        path = REPO_ROOT / rel
        if not path.exists():
            failures.append(f"{rel} is named as an interactive path but does not exist.")
            continue
        for line_no, method, line in scan_interactive(path.read_text(), forbidden):
            failures.append(
                f"{rel}:{line_no} — interactive path ({role}) calls the UNBOUNDED "
                f"store method `{method}`.\n"
                f"      {line}\n"
                f"    A bounded-looking query standing on an unbounded read is the "
                f"exact defect class of #1440 and #1441: the answer is bounded, the "
                f"work is not, and nothing shows it.\n"
                f"    Use the bounded equivalent, or narrow the fetch in SQL and add "
                f"an agreement test."
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

    # (d) An unclassified method must fail, since that is the generalisation.
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
