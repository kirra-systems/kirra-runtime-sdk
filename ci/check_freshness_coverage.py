#!/usr/bin/env python3
"""**Freshness-coverage gate — Tier 3 box 3e.**

`KIRRA-WM-FRESHNESS-POLICY-001` says unclassified semantics refuse. That is
enforced at runtime and is fail-closed, so nothing unsafe happens when a class
is missing — the query refuses.

This gate exists for the *other* failure, which is silent: **the table starts
correct and quietly becomes incomplete.** Somebody adds a recency-sensitive
predicate, never rules it, and the refusal is discovered by whoever is holding
the pager. Nothing in the type system connects "a new claim kind was written" to
"its freshness disposition was decided".

So: every `(kind, predicate)` this repository actually WRITES must be either

* ruled in `crates/kirra-world-service/src/freshness.rs`'s `RULED` table, or
* listed in `ci/freshness_unruled_baseline.json` as deliberately unruled.

A new pair in neither reds. The baseline is not an escape hatch — it is where a
class says *"this is knowingly unruled and therefore knowingly refuses"*, which
is a decision somebody made rather than one nobody noticed.

Self-tests: `python3 ci/check_freshness_coverage.py --self-test`.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
POLICY = REPO / "crates/kirra-world-service/src/freshness.rs"
BASELINE = REPO / "ci/freshness_unruled_baseline.json"

# A `NewEvent { … }` literal, non-greedy to the closing brace of the literal.
# Both fields are read from the SAME literal so a `kind` from one event cannot
# be paired with a `predicate` from another.
_EVENT = re.compile(r"NewEvent\s*\{(.*?)\n\s*\}", re.S)
_KIND = re.compile(r'\bkind:\s*"([^"]*)"')
_KIND_VAR = re.compile(r"\bkind\s*,")
_PRED_SOME = re.compile(r'\bpredicate:\s*Some\("([^"]*)"\)')
_PRED_NONE = re.compile(r"\bpredicate:\s*None\b")

# One row of the RULED table.
_RULED_ROW = re.compile(
    r'SemanticClass\s*\{\s*kind:\s*"([^"]*)"\s*,\s*'
    r'predicate:\s*(?:Some\("([^"]*)"\)|None)\s*,?\s*\}',
    re.S,
)


class GateError(Exception):
    """A violation worth failing CI over."""


def ruled_classes(text: str) -> set[tuple[str, str | None]]:
    """The `(kind, predicate)` pairs the RULED table holds.

    Refuses an empty parse: a gate that reads zero rulings would report every
    class as unruled, which is noise rather than a signal — and the fix someone
    reaches for under deadline is to delete the gate.
    """
    rows = {(m.group(1), m.group(2)) for m in _RULED_ROW.finditer(text)}
    if not rows:
        raise GateError(
            f"{POLICY.name}: parsed ZERO ruled classes. Either the table was "
            "emptied, or its formatting changed in a way this gate cannot "
            "follow. Both are failures; neither is a pass."
        )
    return rows


def written_classes(sources: dict[str, str]) -> set[tuple[str, str | None]]:
    """Every `(kind, predicate)` this repository writes.

    A `kind` supplied by a variable rather than a literal is skipped and
    reported by the caller, not silently treated as covered — see `main`.
    """
    found: set[tuple[str, str | None]] = set()
    for text in sources.values():
        for m in _EVENT.finditer(text):
            body = m.group(1)
            kind_m = _KIND.search(body)
            if not kind_m:
                continue
            if _PRED_SOME.search(body):
                found.add((kind_m.group(1), _PRED_SOME.search(body).group(1)))
            elif _PRED_NONE.search(body):
                found.add((kind_m.group(1), None))
    return found


def dynamic_event_sites(sources: dict[str, str]) -> list[str]:
    """Files whose `NewEvent` literals take `kind` from a variable.

    Reported rather than ignored. This gate reads literals, so a variable
    `kind` is a class it cannot see — and a gate that quietly skipped them
    would advertise coverage it does not have.
    """
    out = []
    for name, text in sources.items():
        for m in _EVENT.finditer(text):
            if not _KIND.search(m.group(1)) and _KIND_VAR.search(m.group(1)):
                out.append(name)
                break
    return sorted(out)


def check(
    ruled: set[tuple[str, str | None]],
    written: set[tuple[str, str | None]],
    baselined: set[tuple[str, str | None]],
) -> list[str]:
    """Violations, most actionable first."""
    problems = []
    for kind, pred in sorted(written - ruled - baselined, key=lambda c: (c[0], c[1] or "")):
        problems.append(
            f"({kind}, {pred!r}) is written by this repository but has no "
            f"freshness disposition.\n"
            f"    Rule it in `RULED` (Timeless or Bounded, with the reasoning), "
            f"or add it to {BASELINE.name} as knowingly unruled — in which case "
            f"queries for it REFUSE, deliberately."
        )
    for kind, pred in sorted(baselined & ruled, key=lambda c: (c[0], c[1] or "")):
        problems.append(
            f"({kind}, {pred!r}) is BOTH ruled and baselined as unruled. The "
            f"baseline entry is stale — remove it, or the record disagrees with "
            f"the table about whether this class was decided."
        )
    for kind, pred in sorted(baselined - written, key=lambda c: (c[0], c[1] or "")):
        problems.append(
            f"({kind}, {pred!r}) is baselined as unruled but nothing writes it. "
            f"Remove the entry; a baseline listing classes that do not exist "
            f"grows until nobody reads it."
        )
    return problems


def main() -> int:
    sources = {
        str(p.relative_to(REPO)): p.read_text(encoding="utf-8")
        for p in (REPO / "crates").rglob("*.rs")
    }
    try:
        ruled = ruled_classes(POLICY.read_text(encoding="utf-8"))
    except GateError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1

    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    baselined = {(e["kind"], e["predicate"]) for e in baseline["unruled"]}
    written = written_classes(sources)

    problems = check(ruled, written, baselined)
    if problems:
        print("FAIL: freshness-coverage gate\n", file=sys.stderr)
        for p in problems:
            print(f"  - {p}\n", file=sys.stderr)
        return 1

    dynamic = dynamic_event_sites(sources)
    print(
        f"OK: {len(written)} written class(es); "
        f"{len(ruled)} ruled, {len(baselined)} knowingly unruled"
    )
    if dynamic:
        print(
            "  note: these files build `NewEvent` with a non-literal `kind`, "
            "so their classes are not visible to this gate:"
        )
        for d in dynamic:
            print(f"    {d}")
    return 0


def _self_test() -> int:
    failures = []

    def case(name, fn):
        try:
            fn()
        except AssertionError as exc:
            failures.append(f"{name}: {exc}")

    RULED_SRC = '''
    pub const RULED: &[(SemanticClass, FreshnessPolicy)] = &[
        (SemanticClass { kind: "mission", predicate: Some("last_seen_at") },
         FreshnessPolicy::Bounded { max_age_ms: 1 }),
        (SemanticClass { kind: "observation", predicate: None },
         FreshnessPolicy::Timeless),
    ];
    '''

    EVENT_SRC = '''
    store.append(&NewEvent {
        event_id: &e,
        kind: "mission",
        subject: "s",
        predicate: Some("last_seen_at"),
        object: None,
    })
    '''

    def t_ruled_rows_are_parsed_including_the_none_predicate():
        got = ruled_classes(RULED_SRC)
        assert got == {("mission", "last_seen_at"), ("observation", None)}, got

    def t_an_empty_ruled_table_is_an_error():
        try:
            ruled_classes("pub const RULED: &[(SemanticClass, FreshnessPolicy)] = &[];")
        except GateError:
            return
        raise AssertionError("an empty table passed")

    def t_written_classes_are_found():
        assert written_classes({"a.rs": EVENT_SRC}) == {("mission", "last_seen_at")}

    def t_kind_and_predicate_come_from_the_SAME_literal():
        # Two events; pairing across them would invent ("mission", "colour").
        src = EVENT_SRC + '''
        store.append(&NewEvent {
            kind: "observation",
            predicate: Some("colour"),
        })
        '''
        got = written_classes({"a.rs": src})
        assert got == {("mission", "last_seen_at"), ("observation", "colour")}, got

    def t_an_unruled_written_class_fails():
        problems = check({("mission", "last_seen_at")}, {("mission", "invented")}, set())
        assert problems and "no freshness disposition" in problems[0]

    def t_a_baselined_class_passes():
        assert check(set(), {("m", "p")}, {("m", "p")}) == []

    def t_a_class_both_ruled_and_baselined_fails():
        assert check({("m", "p")}, {("m", "p")}, {("m", "p")})

    def t_a_baseline_entry_nothing_writes_fails():
        assert check({("m", "p")}, {("m", "p")}, {("x", "y")})

    def t_a_dynamic_kind_is_reported_not_silently_covered():
        src = '''
        store.append(&NewEvent {
            event_id: &e,
            kind,
            predicate: Some("p"),
        })
        '''
        assert written_classes({"d.rs": src}) == set(), "a variable kind was invented"
        assert dynamic_event_sites({"d.rs": src}) == ["d.rs"]

    cases = [(n, f) for n, f in sorted(locals().items()) if n.startswith("t_") and callable(f)]
    for name, fn in cases:
        case(name, fn)

    expected = 9
    if len(cases) != expected:
        print(
            f"SELF-TEST HARNESS: discovered {len(cases)} cases, expected {expected}.",
            file=sys.stderr,
        )
        return 1
    if failures:
        print("SELF-TEST FAILURES:", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(f"self-tests OK ({len(cases)} cases)")
    return 0


if __name__ == "__main__":
    sys.exit(_self_test() if "--self-test" in sys.argv else main())
