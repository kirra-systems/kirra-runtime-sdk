#!/usr/bin/env python3
"""Tier 3 box 3d — bounded queries, and one sanctioned way to ask.

THE RULES
---------
    1-4  A Tier 3 boundary query that sits on a request path may call only
         store methods whose result size is bounded by their arguments. Every
         public read method on the store must be CLASSIFIED, and an
         unclassified one fails.

    5    Every domain-query call outside the query-engine implementation must
         route through the typed engine. Direct family, store or projection
         reads are forbidden, except the store methods classified
         `operational`.

Rules 1-4 make a query bounded. Rule 5 makes a query the only way to ask, which
boundedness does not imply: a consumer holding a `&WorldStore` could build its
own view, call a family directly, and be perfectly bounded while bypassing
semantics versions, freshness classification and the no-bare-values rule.

Rule 5's PRIMARY enforcement is visibility rather than this file. `WorldView`
and the `resolve` methods on `LineageRef`/`AnswerRef` are `pub(crate)`, so a
consumer reaching past `QueryEngine::execute` gets a compile error at the point
of the mistake. What is here covers the rest: a new `pub` family method nobody
wrapped, a consumer reaching around the boundary into the store, and the
visibility pins that make widening `pub(crate)` back to `pub` a red build.

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

# Rule 5. The crates that IMPLEMENT the boundary — everything else that touches
# the world is a domain consumer and must go through the query engine.
# Crates that IMPLEMENT Kirra World's boundary rather than consume it. Rule 5
# asks whether application code reached past the query engine; these are not
# application code, so the question does not apply to them.
#
# `kirra-world-ingest` is here because it is the WRITE half of that boundary
# (box 2a). It asks the store nothing on anyone's behalf -- it surveys evidence
# it is about to propose from. Routing a producer through `QueryEngine` would
# mean inventing a domain request meaning "let me see the log so I can write to
# it", which is not a question the engine exists to answer.
#
# The exemption is scoped: rule 2 covers this crate's `src/` (see INGEST_SRC),
# so it is exempt from "consumers must use the engine" and NOT from "the
# unbounded reads stay out of the write path".
WORLD_CRATES = {
    "kirra-world",
    "kirra-world-store",
    "kirra-world-service",
    "kirra-world-ingest",
}

# The boundary internals the engine exists to be the only route past. These are
# `pub(crate)` in the source, so the compiler already refuses a consumer that
# names them; rule 5 exists so the day someone widens the visibility is the day
# CI says so, rather than the day a consumer quietly reaches past the engine.
BOUNDARY_INTERNALS = {
    "WorldView": "the boundary's internal view — the four non-lineage families",
}

# (file, exact source line that must still be present) — the visibility that
# makes the engine load-bearing. Asserted rather than assumed, because a
# one-word edit turns a compile error back into a convention.
VISIBILITY_PINS = [
    (
        "crates/kirra-world-service/src/read_view.rs",
        "pub(crate) struct WorldView<'a> {",
        "WorldView must stay crate-internal, or a consumer can build its own "
        "view and skip the engine entirely.",
    ),
    (
        "crates/kirra-world-service/src/lineage.rs",
        "pub(crate) fn resolve(&self, store: &WorldStore)",
        "LineageRef::resolve must stay crate-internal — the `Lineage` request "
        "is the sanctioned route to it.",
    ),
    (
        "crates/kirra-world-service/src/answer_ref.rs",
        "pub(crate) fn resolve(&self, store: &WorldStore)",
        "AnswerRef::resolve must stay crate-internal — the `ReplayAnswer` "
        "request is the sanctioned route to it.",
    ),
]

STORE_LIB = REPO_ROOT / "crates" / "kirra-world-store" / "src" / "lib.rs"
STORE_SNAPSHOT = REPO_ROOT / "crates" / "kirra-world-store" / "src" / "snapshot.rs"
STORE_SRC = REPO_ROOT / "crates" / "kirra-world-store" / "src"
BOUNDARY_SRC = REPO_ROOT / "crates" / "kirra-world-service" / "src"
# Box 2a's production write path. Not a request path, so rule 5 does not apply
# to it (see WORLD_CRATES) -- but its PRODUCTION code must still never reach for
# an unbounded read, and rule 2 is where that is enforced rather than asserted.
# An ingest pass runs unattended on a growing log; an unbounded fetch there is
# the same defect as one behind a bounded-looking query, with nobody watching.
INGEST_SRC = REPO_ROOT / "crates" / "kirra-world-ingest" / "src"

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


ANY_IMPL_RE = r"^impl(?:<[^>]*>)? .*\{"
CEILING_RE = re.compile(r"\bMAX_[A-Z0-9_]+\b")
# Every visibility and qualifier an impl method can carry, not just the two that
# happened to matter first. `pub(crate) fn` is the one that caught this out in
# review: the scan claimed to see EVERY impl method while matching only bare `fn`
# and `pub fn`, so a restricted-visibility query would have been invisible to
# rule 6 with the docstring insisting otherwise.
FN_ANY_RE = re.compile(
    r"\n    (?:pub(?:\s*\([^)]*\))?\s+)?(?:const\s+|async\s+|unsafe\s+)*"
    r"fn ([a-z_0-9]+)\s*(?:<[^>]*>)?\s*\("
)


def _any_fn_bodies(impl_body: str) -> list[tuple[str, str, str]]:
    """Like [`_fn_bodies`], but also sees methods that are not `pub`.

    Trait-impl methods carry no `pub`, and restricted ones carry `pub(crate)`, so
    a `pub fn` scan walked straight past both. That is not a hypothetical gap: `CitationLookup for
    WorldStore` is where the provenance walk gets ALL of its SQL, and the whole
    impl block was invisible to this gate — its block matched and it yielded
    zero functions, which reads identically to "nothing to flag".
    """
    out = []
    for m in FN_ANY_RE.finditer(impl_body):
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


def ceiling_bound_violation(body: str) -> bool:
    """True when a method knows a `MAX_*` ceiling but its SQL does not.

    Rule 4 asks whether a bound the CALLER passed reached the query. This asks
    the same question of a bound the method holds itself, which rule 4 cannot
    see because there is no page parameter to notice — the ceiling is a
    constant.

    It is the same defect in the same place: fetch everything, truncate in Rust,
    return an answer that is byte-identical to the bounded one. No behavioural
    test can tell the two apart, because the only difference is how much work
    the database did.
    """
    code = strip_comments(body)
    if not SELECT_RE.search(code):
        return False
    if not CEILING_RE.search(code):
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


def classification_failures(found: dict[str, set[str]], baseline: dict) -> list[str]:
    """Rule 1: the discovered read surface and the baseline must agree, exactly.

    A FUNCTION rather than inline in `run`, so the self-test can drive the real
    rule against a doctored baseline. It was inline, and the fixture that was
    supposed to prove it worked never called it — it removed a key from a dict
    and then asserted the key was absent, which is true by construction.
    """
    failures: list[str] = []
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
    return failures


def domain_consumer_crates() -> list[Path]:
    """Crates that touch Kirra World but do not implement its boundary.

    Membership is read from the manifests rather than listed here, so a NEW
    consumer is covered the day it is added. Over-inclusion is the safe
    direction: a crate that merely mentions the world in a comment gets scanned
    and finds nothing.
    """
    out = []
    for manifest in sorted((REPO_ROOT / "crates").glob("*/Cargo.toml")):
        if manifest.parent.name in WORLD_CRATES:
            continue
        text = manifest.read_text()
        if "kirra-world-service" in text or "kirra-world-store" in text:
            out.append(manifest.parent)
    return out


def scan_consumer(text: str, forbidden: set[str]) -> list[tuple[int, str, str]]:
    """Rule-5 violations in one consumer file: internals named, or reads called.

    Comment-stripped for the same reason every other scan here is — a consumer's
    docs should be able to say *"this used to call `WorldView::ask`"* without
    the sentence becoming the violation it describes.
    """
    hits = list(scan_interactive(text, forbidden))
    for n, line in enumerate(text.splitlines(), start=1):
        code = line.split("//")[0]
        for name in BOUNDARY_INTERNALS:
            if re.search(r"\b" + name + r"\b", code):
                hits.append((n, name, line.strip()))
    return sorted(hits)


def run(verbose: bool = False) -> list[str]:
    baseline = json.loads(BASELINE_PATH.read_text())
    failures: list[str] = []
    found = discover()

    # 1. Every public read method must be classified. FAIL-CLOSED.
    failures.extend(classification_failures(found, baseline))

    # 2. NOTHING in the boundary crate may call an unbounded method.
    #
    # The whole crate, not a list of "interactive" files. A per-file list is
    # exactly the hole a relocated call slips through: move the unbounded fetch
    # into a private helper in a fourth file and the gate goes green while the
    # invariant stays false. The boundary crate IS the request path, so every
    # file in it is checked and there is no file to move the call to.
    forbidden = unbounded_names(baseline)
    for root in (BOUNDARY_SRC, INGEST_SRC):
        for path in sorted(root.rglob("*.rs")):
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

    # ---- Rule 6: a ceiling the METHOD holds must reach the query too --------
    #
    # Rule 4 covers a bound the caller passes. A method can equally hold its own
    # ceiling as a constant — `MAX_COMPACTED_SPANS`, `MAX_CARRIERS` — and then
    # apply it in Rust after selecting the world. Same defect, same invisibility
    # to any behavioural test, and rule 4 cannot see it because there is no page
    # parameter to trigger on.
    #
    # Scans EVERY impl block, not just the two named surfaces, and sees non-`pub`
    # methods: the provenance walk's SQL all lives in a trait impl, which the
    # `pub fn` scan skipped silently.
    for path in sorted(STORE_SRC.rglob("*.rs")):
        text = path.read_text()
        for body in _impl_bodies(text, ANY_IMPL_RE):
            for fn_name, _sig, fn_body in _any_fn_bodies(body):
                if ceiling_bound_violation(fn_body):
                    failures.append(
                        f"{fn_name} ({path.name}) applies a MAX_* ceiling to a "
                        f"SELECT that has no LIMIT.\n"
                        f"    Truncating in Rust after an unbounded scan returns "
                        f"the same answer while doing work proportional to the "
                        f"store — the difference no test can observe.\n"
                        f"    Push the ceiling into the query."
                    )

    # 5. A DOMAIN QUERY MUST GO THROUGH THE TYPED ENGINE.
    #
    # Rules 1-4 make a query bounded. They do not make the query the only way to
    # ask: a consumer holding a `&WorldStore` could build its own view, call a
    # family directly, and be perfectly bounded while bypassing semantics
    # versions, freshness classification and the no-bare-values rule.
    # `mission_context` did exactly that, and it was the sanctioned route at the
    # time — which is why the negative fixture in the self-test is that real
    # call and not an invented one.
    #
    # The PRIMARY enforcement is visibility, not this rule: `WorldView` and the
    # two `resolve` methods are `pub(crate)`, so a consumer reaching past the
    # engine gets a compile error at the point of the mistake. This rule is
    # defence in depth for what visibility cannot reach — a new `pub` family
    # method nobody wrapped, a consumer reaching into the store directly — plus
    # the pins below, so widening the visibility is itself the red build.
    #
    # `operational` reads are the named carve-outs, and they are named by the
    # SAME classification rules 1-4 already require: integrity verification,
    # migration, folding, retention and measurement legitimately read the store
    # outside a query. A new store method cannot slip through as a carve-out,
    # because an unclassified one already reds rule 1.
    domain_reads = {
        name
        for surface in ("world_store", "read_snapshot")
        for name, spec in baseline[surface].items()
        if not name.startswith("_") and spec["class"] in ("bounded", "unbounded")
    }
    for crate in domain_consumer_crates():
        for path in sorted(crate.rglob("*.rs")):
            rel = path.relative_to(REPO_ROOT)
            for line_no, what, line in scan_consumer(path.read_text(), domain_reads):
                detail = BOUNDARY_INTERNALS.get(what, f"the store read `{what}`")
                failures.append(
                    f"{rel}:{line_no} — a domain consumer reaches past the query "
                    f"engine: {detail}.\n"
                    f"      {line}\n"
                    f"    Tier 3 box 3d: there is ONE sanctioned way for "
                    f"application code to ask Kirra World a domain question.\n"
                    f"    Use `QueryEngine::execute(..)` with the typed request "
                    f"for the family you want."
                )

    for rel, needle, why in VISIBILITY_PINS:
        text = (REPO_ROOT / rel).read_text()
        if needle not in text:
            failures.append(
                f"{rel} — the visibility pin `{needle}` is gone.\n"
                f"    {why}\n"
                f"    A compile error at the point of the mistake is stronger "
                f"than a gate finding after the fact; widening this trades the "
                f"first for the second."
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

    # -- rule 6: a ceiling the method holds must reach the query --------------
    #
    # The positive fixture is `compacted_spans` as it shipped on #1448: a real
    # ceiling, applied in Rust, over a SELECT with no LIMIT. It is the shape the
    # provenance walk actually had, not an invented one.
    unbounded_ceiling = """
        let mut stmt = self.conn.prepare(
            "SELECT lo_generation, hi_generation FROM compaction_citations")?;
        let mut spans = Vec::new();
        for row in mapped { spans.push(row?); }
        let truncated = spans.len() > provenance_graph::MAX_COMPACTED_SPANS;
        spans.truncate(provenance_graph::MAX_COMPACTED_SPANS);
    """
    if not ceiling_bound_violation(unbounded_ceiling):
        print("SELF-TEST FAIL: a MAX_* ceiling applied over a LIMIT-less SELECT "
              "was NOT caught — rule 6 is vacuous.")
        return 1
    print("self-test: ceiling applied in Rust over an unbounded SELECT → caught")

    bounded_ceiling = unbounded_ceiling.replace(
        'FROM compaction_citations")?;',
        'FROM compaction_citations WHERE lo_generation <= ?1 LIMIT ?2")?;')
    if ceiling_bound_violation(bounded_ceiling):
        print("SELF-TEST FAIL: rule 6 fired on a query that DOES carry a LIMIT.")
        return 1
    print("self-test: ceiling pushed into the query → not flagged")

    # A comment may not disarm it, for rule 4's reason: a structural rule that
    # prose can switch off is prose.
    commented = unbounded_ceiling.replace(
        "let mut spans = Vec::new();",
        "// bounded by LIMIT elsewhere\n        let mut spans = Vec::new();")
    if not ceiling_bound_violation(commented):
        print("SELF-TEST FAIL: a COMMENT mentioning LIMIT disarmed rule 6.")
        return 1
    print("self-test: comments cannot disarm rule 6")

    # And the gap that hid it: trait-impl methods carry no `pub`.
    trait_impl = """
impl provenance_graph::CitationLookup for WorldStore {
    fn compacted_spans(&self) -> Result<Vec<(i64, i64)>, Self::Error> {
        let mut stmt = self.conn.prepare("SELECT lo_generation FROM t")?;
        spans.truncate(provenance_graph::MAX_COMPACTED_SPANS);
    }
}
"""
    seen = [f for b in _impl_bodies(trait_impl, ANY_IMPL_RE) for f, _, _ in _any_fn_bodies(b)]
    if "compacted_spans" not in seen:
        print("SELF-TEST FAIL: a non-`pub` trait-impl method is still invisible "
              "to the scan — rule 6 would report green on an unscanned surface.")
        return 1
    print("self-test: trait-impl methods are visible to the ceiling scan")

    # ...and so is every other visibility an impl method can carry. Raised in
    # review of #1450: the scan matched only `fn` and `pub fn` while its own
    # docstring claimed it saw every impl method, which is the same shape of
    # false completeness rule 6 exists to catch.
    visibilities = """
impl Store {
    pub(crate) fn restricted(&self) -> Result<(), E> {
        let mut stmt = self.conn.prepare("SELECT a FROM t")?;
        rows.truncate(MAX_THING);
    }
    pub(super) fn super_scoped(&self) -> Result<(), E> { let _ = 1; }
    async fn asynchronous(&self) -> Result<(), E> { let _ = 1; }
    pub const fn constant(&self) -> usize { 1 }
    unsafe fn unsafely(&self) -> usize { 1 }
}
"""
    found = [f for b in _impl_bodies(visibilities, ANY_IMPL_RE) for f, _, _ in _any_fn_bodies(b)]
    for expected in ("restricted", "super_scoped", "asynchronous", "constant", "unsafely"):
        if expected not in found:
            print(f"SELF-TEST FAIL: `{expected}` is invisible to the method scan, "
                  f"so rule 6 would report green on an unscanned method. Saw: {found}")
            return 1
    print(f"self-test: all {len(found)} impl-method visibilities are scanned")

    # Non-vacuity: the restricted one must not merely be SEEN, it must be JUDGED.
    restricted_body = next(
        body for b in _impl_bodies(visibilities, ANY_IMPL_RE)
        for name, _, body in _any_fn_bodies(b) if name == "restricted")
    if not ceiling_bound_violation(restricted_body):
        print("SELF-TEST FAIL: a `pub(crate)` method with a MAX_* ceiling over a "
              "LIMIT-less SELECT was discovered but NOT flagged.")
        return 1
    print("self-test: a restricted-visibility ceiling violation is flagged, not just seen")

    # (h) RULE 5, THE HISTORICAL ROUTE: `mission_context` as it actually shipped
    #     before the query engine existed. Copied from the commit that this box
    #     migrated, not invented — a synthetic spelling would only prove the
    #     regex matches itself, whereas this proves the rule rejects the exact
    #     code that WAS the sanctioned way to consume Kirra World.
    domain_reads = {
        name
        for surface in ("world_store", "read_snapshot")
        for name, spec in baseline[surface].items()
        if not name.startswith("_") and spec["class"] in ("bounded", "unbounded")
    }
    pre_engine_mission_context = """
    let view = WorldView::new(
        store,
        FreshnessSource::Caller(match staleness_budget_ms {
            Some(max_age_ms) => FreshnessPolicy::Bounded { max_age_ms },
            None => FreshnessPolicy::Timeless,
        }),
    );

    let answers = match view.ask(subject.as_str(), now_ms)?.into_lookup() {
        WorldLookup::Answered(answers) => answers,
        WorldLookup::Unknown(reason) => return Ok(ProposalContext::silent(candidates, reason)),
    };
    """
    if not scan_consumer(pre_engine_mission_context, domain_reads):
        print("SELF-TEST FAIL: the pre-engine mission_context route was NOT "
              "caught — rule 5 does not reject the real code it exists for.")
        failed = 1
    else:
        print("self-test: pre-engine mission_context → WorldView::ask → caught")

    # (i) The migrated form must pass, or the rule forbids the fix as well as
    #     the defect and there is nowhere for a consumer to go.
    engine_mission_context = """
    let engine = QueryEngine::new(store, FreshnessSource::Caller(policy));
    let answers = match engine.execute(Ask { subject, now_ms })?.into_lookup() {
        WorldLookup::Answered(answers) => answers,
        WorldLookup::Unknown(reason) => return Ok(ProposalContext::silent(candidates, reason)),
    };
    """
    if scan_consumer(engine_mission_context, domain_reads):
        print("SELF-TEST FAIL: the MIGRATED mission_context was flagged — the "
              "rule would leave consumers with no sanctioned route.")
        failed = 1
    else:
        print("self-test: migrated mission_context → QueryEngine::execute → "
              "not flagged")

    # (j) A consumer reaching around the boundary INTO the store must fail too.
    #     Visibility cannot catch this one: `WorldStore` is legitimately public
    #     for operational use, so only the classification distinguishes a
    #     domain read from a rebuild.
    around_the_side = """
    let claims = store.current("package_17", now_ms)?;
    """
    if not scan_consumer(around_the_side, domain_reads):
        print("SELF-TEST FAIL: a consumer calling a bounded STORE read directly "
              "was not caught — the engine can be bypassed underneath.")
        failed = 1
    else:
        print("self-test: consumer reading the store directly → caught")

    # (k) …while an OPERATIONAL store call from a consumer must be allowed, or
    #     the rule bans fixture seeding and rebuild tooling.
    operational_use = """
    store.fold().expect("fold");
    store.verify_chain().expect("chain");
    """
    if scan_consumer(operational_use, domain_reads):
        print("SELF-TEST FAIL: an operational store call was flagged — the "
              "named carve-outs are not actually carved out.")
        failed = 1
    else:
        print("self-test: operational store calls from a consumer → allowed")

    # (l) RULE 1, THE GENERALISATION: an unclassified method must fail.
    #
    #     This fixture was broken twice over, and review caught it. It used to
    #     build `{k: v for k, v in baseline if k != "history"}` and then assert
    #     `"history" not in` the result — true by construction, so it never ran
    #     rule 1 at all. Then this box renamed `history` to `history_whole` and
    #     it stopped even removing anything, which is what made the emptiness
    #     visible.
    #
    #     So it now (a) runs the REAL rule via `classification_failures`, and
    #     (b) DERIVES the method to strip instead of naming one. A hardcoded
    #     name rots the moment that method is renamed — this box renamed two —
    #     and a rotted fixture reports success. The empty-pool guard is the
    #     other half: a fixture with nothing to strip must fail, not pass.
    found = discover()
    strippable = sorted(found["world_store"] & set(baseline["world_store"]))
    if not strippable:
        print("SELF-TEST FAIL: no classified method was discoverable to strip — "
              "the fixture has nothing to remove and would pass vacuously.")
        failed = 1
    else:
        probe = strippable[0]
        doctored = {
            **baseline,
            "world_store": {
                k: v for k, v in baseline["world_store"].items() if k != probe
            },
        }
        if not classification_failures(found, doctored):
            print(f"SELF-TEST FAIL: dropping the classification for `{probe}` was "
                  f"NOT reported — rule 1 is not fail-closed.")
            failed = 1
        else:
            print(f"self-test: dropping a classification ({probe}) → rule 1 "
                  f"reports it")

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
