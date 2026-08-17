#!/usr/bin/env python3
"""**Reducer semantics gate — Tier 3 box 3b.**

`crates/kirra-world-store/src/semantics.rs` declares a version for every
projection reducer, and `KIRRA-WM-REDUCER-VERSION-001` says the version changes
whenever behaviour changes in a way that can alter a derived answer. This gate
is what stops that being a promise.

The Rust conformance test (`tests/semantics_corpus.rs`) already proves the
declared `corpus_digest` is the reducer's *actual* digest. That leaves two holes
this gate closes, and they are different holes:

1.  **A silent edit the corpus is blind to.** The corpus discriminates the axes
    it exercises; an axis nobody thought of is not covered. So each reducer also
    carries a `source_pin` over its own source span, and any edit at all moves
    it. This is the frozen-talisman technique `validate_vehicle_command` uses,
    scoped to a marker-delimited region so ordinary churn elsewhere in the file
    does not trip it.

2.  **A behaviour change that updated the declaration but not the version.**
    This is the hole that makes a version decorative, and the Rust test *cannot*
    see it: re-pin `corpus_digest` and the conformance test goes green again
    with the version untouched. So the recorded history in
    `ci/world_semantics_baseline.json` holds what each version's digest was, and
    re-declaring a *different* digest for a version already on record is an
    error whose only clean resolution is a new version.

# The workflow this produces

| You did | Corpus digest | Source pin | Do |
|---|---|---|---|
| nothing | same | same | — |
| refactored a reducer | same | moved | re-pin `source_pin`, update the baseline row |
| changed behaviour | moved | moved | bump `version`, re-pin both, ADD a baseline row |

The middle row is why the two mechanisms are not redundant: it is the only case
where a pin moves and the version legitimately does not, and the corpus digest
holding is precisely the evidence for that.

# The residual, stated rather than implied

An author who edits the Rust declaration *and* rewrites the baseline's historical
row in the same commit still passes. No gate can force a human to increment an
integer. What this removes is doing it silently, doing it by accident, and doing
it without a reviewer seeing a diff in a file whose only purpose is to be that
record.

Self-tests: `python3 ci/check_world_semantics.py --self-test`.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_orphan_cores import strip_noncode  # noqa: E402

REPO = Path(__file__).resolve().parent.parent

# Two crates declare versioned rules: the store owns the three projection
# reducers, the service owns the answer boundary's admissibility rule. They are
# read by ONE parser and held to ONE baseline deliberately — a boundary rule
# checked by a second, differently-shaped gate is a boundary rule checked more
# weakly, and the whole point of `SemanticVersions` is that a reference carries
# rules from both crates in one set.
DECLARATIONS = (
    REPO / "crates/kirra-world-store/src/semantics.rs",
    REPO / "crates/kirra-world-service/src/semantics.rs",
)
BASELINE = REPO / "ci/world_semantics_baseline.json"

BEGIN = "SEMANTICS-PIN-BEGIN:"
END = "SEMANTICS-PIN-END:"

# One `RuleSpec { … }` literal. Anchored on the field NAMES rather than on
# position, so reordering fields inside the struct does not silently defeat the
# parser -- and `parse_declarations` refuses an empty parse, so a formatting
# change this pattern cannot follow reds instead of checking nothing.
_SPEC = re.compile(
    r"\w*RuleSpec\s*\{\s*"
    r"rule:\s*\w*RuleId::(?P<variant>\w+)\s*,\s*"
    r"version:\s*(?P<version>\d+)\s*,\s*"
    r'corpus_digest:\s*"(?P<corpus_digest>[0-9a-f]{64})"\s*,\s*'
    r'source_pin:\s*"(?P<source_pin>[0-9a-f]{64})"\s*,\s*'
    r'source_file:\s*"(?P<source_file>[^"]+)"\s*,\s*'
    r'span:\s*"(?P<span>[^"]+)"\s*,\s*'
    r"\}",
    re.S,
)

# The `RuleId::as_str` arms, so the gate learns each variant's STABLE name from
# the same place the Rust code does. Deriving it here by convention would let a
# rename drift the baseline key silently.
_AS_STR = re.compile(r"Self::(?P<variant>\w+)\s*=>\s*\"(?P<name>[a-z0-9_]+)\"")

# Just the row opener, used to COUNT rows so an unparseable one is an error
# rather than a silent omission. Deliberately not a second parser: it recognises
# where a row starts and nothing about its contents.
_SPEC_HEAD = re.compile(r"\w*RuleSpec\s*\{\s*rule:")

# `RuleSpec` and `BoundaryRuleSpec` are STRUCT DEFINITIONS, not rows. Their
# field declarations look enough like a row's assignments to be worth excluding
# explicitly rather than relying on the `:` versus `,` shape to differ.
_STRUCT_DEF = re.compile(r"pub\s+struct\s+\w*RuleSpec\s*\{.*?\n\}", re.S)


class GateError(Exception):
    """A violation worth failing CI over."""


def parse_declarations(text: str, where: str = "declaration") -> list[dict]:
    """Parse one file's rule declarations, keyed by each rule's stable name.

    Refuses an empty parse. A gate that silently finds nothing to check is the
    most dangerous shape a gate can take: it is indistinguishable from a gate
    that checked everything and was satisfied.
    """
    names = {m.group("variant"): m.group("name") for m in _AS_STR.finditer(text)}
    if not names:
        raise GateError(
            f"{where}: could not parse any `as_str` arm — the gate cannot learn "
            "the stable rule names, so it would check nothing"
        )

    rows = []
    for m in _SPEC.finditer(_STRUCT_DEF.sub("", text)):
        variant = m.group("variant")
        if variant not in names:
            raise GateError(
                f"{where}: rule `{variant}` is declared but has no `as_str` arm "
                "— it would have no stable baseline key"
            )
        rows.append(
            {
                "rule": names[variant],
                "version": int(m.group("version")),
                "corpus_digest": m.group("corpus_digest"),
                "source_pin": m.group("source_pin"),
                "source_file": m.group("source_file"),
                "span": m.group("span"),
            }
        )

    if not rows:
        raise GateError(
            f"{where}: parsed ZERO rule declarations. Either the table was "
            "emptied — which would leave its rules unversioned — or its "
            "formatting changed in a way this gate cannot follow. Both are "
            "failures; neither is a pass."
        )

    # Every `RuleSpec {` occurrence must have produced a row.
    #
    # `_SPEC` requires both digests to be 64 hex characters, so a row whose
    # digest is a placeholder, truncated, or hand-typed does not MATCH — and an
    # unmatched row is invisible to every check below rather than failing one.
    # That is the "silently found nothing" shape this function's docstring
    # already refuses at zero, reaching the same file one row at a time.
    #
    # Found by hitting it: a rule declared with `"TBD"` digests during
    # development passed this gate, which reported OK and listed every rule
    # except the one being added.
    present = len(_SPEC_HEAD.findall(_STRUCT_DEF.sub("", text)))
    if present != len(rows):
        raise GateError(
            f"{where}: found {present} `RuleSpec` row(s) but could parse only "
            f"{len(rows)}. A row this gate cannot read is a row it does not "
            "check — most likely a `corpus_digest` or `source_pin` that is not "
            "64 hex characters."
        )
    return rows


def parse_all(sources: dict[str, str]) -> list[dict]:
    """Every declaration across every declaring crate, with names unique."""
    rows: list[dict] = []
    for where, text in sources.items():
        rows.extend(parse_declarations(text, where))

    seen = [r["rule"] for r in rows]
    dupes = {r for r in seen if seen.count(r) > 1}
    if dupes:
        raise GateError(
            f"duplicate declarations for: {', '.join(sorted(dupes))} — two rules "
            "sharing a stable name would share one baseline history"
        )
    return rows


def extract_span(source: str, span: str) -> str:
    """The text between this span's begin and end markers.

    Both markers must appear exactly once. A duplicated marker would make the
    pinned region ambiguous, and an ambiguous region can be silently shrunk to
    exclude the line someone wanted to change.
    """
    starts = [i for i, line in enumerate(source.splitlines()) if f"{BEGIN} {span}" in line]
    ends = [i for i, line in enumerate(source.splitlines()) if f"{END} {span}" in line]
    if len(starts) != 1 or len(ends) != 1:
        raise GateError(
            f"span `{span}`: expected exactly one {BEGIN} and one {END} marker, "
            f"found {len(starts)} and {len(ends)}"
        )
    if ends[0] <= starts[0]:
        raise GateError(f"span `{span}`: {END} marker precedes {BEGIN}")
    lines = source.splitlines()[starts[0] + 1 : ends[0]]
    body = "\n".join(lines)
    if not body.strip():
        raise GateError(
            f"span `{span}`: the pinned region is empty — a pin over nothing is "
            "satisfied by any reducer"
        )
    return body


def pin_of(source: str, span: str) -> str:
    """The declared-form digest of a reducer span.

    Comments are stripped so ordinary comment churn does not trip the gate and
    train reflexive re-pinning; string literals are KEPT, because in these
    reducers a literal can be the behaviour. Whitespace is normalised so
    `cargo fmt` rewrapping a call is not mistaken for an edit.
    """
    stripped = strip_noncode(extract_span(source, span), keep_strings=True)
    normalised = " ".join(stripped.split())
    return hashlib.sha256(normalised.encode("utf-8")).hexdigest()


def check(declarations: list[dict], baseline: dict, read: callable) -> list[str]:
    """Every rule check. Returns a list of human-readable violations."""
    problems: list[str] = []
    history = baseline.get("rules", {})

    for row in declarations:
        rule, version = row["rule"], row["version"]

        # 1. The source pin must be the span's actual digest.
        try:
            actual = pin_of(read(row["source_file"]), row["span"])
        except GateError as exc:
            problems.append(f"{rule}: {exc}")
            continue
        if actual != row["source_pin"]:
            problems.append(
                f"{rule}: source pin does not match `{row['source_file']}` span "
                f"`{row['span']}`.\n"
                f"    declared: {row['source_pin']}\n"
                f"    actual:   {actual}\n"
                f"    The reducer was edited. If BEHAVIOUR changed the corpus "
                f"digest moved too and `version` must be bumped; if it did not, "
                f"this is a refactor — re-pin `source_pin` here and in the "
                f"baseline, leaving `version` and `corpus_digest` alone."
            )

        # 2. Recorded history must exist and be consistent.
        recorded = history.get(rule)
        if recorded is None:
            problems.append(
                f"{rule}: declared in SEMANTICS but absent from "
                f"{BASELINE.name}. Add its version history, or the version has "
                f"no record to be accountable to."
            )
            continue

        versions = [int(v["version"]) for v in recorded]
        if sorted(versions) != list(range(1, len(versions) + 1)):
            problems.append(
                f"{rule}: recorded versions {sorted(versions)} are not the "
                f"contiguous sequence 1..{len(versions)}. A gap or a repeat means "
                f"a version's history was lost or duplicated."
            )
            continue
        if version != max(versions):
            problems.append(
                f"{rule}: declares version {version} but the newest recorded "
                f"version is {max(versions)}. A version bump needs a baseline row."
            )
            continue

        current = next(v for v in recorded if int(v["version"]) == version)

        # 3. THE LOAD-BEARING CHECK. Behaviour may not move at a fixed version.
        if current["corpus_digest"] != row["corpus_digest"]:
            problems.append(
                f"{rule}: the corpus digest for version {version} changed.\n"
                f"    recorded: {current['corpus_digest']}\n"
                f"    declared: {row['corpus_digest']}\n"
                f"    A reducer's behaviour moved while its version stayed put. "
                f"That is exactly what a recorded AnswerRef cannot survive: it "
                f"would replay under the new rule while naming the old one.\n"
                f"    Bump `version` to {version + 1}, re-pin both digests, and "
                f"ADD a row to {BASELINE.name} — do not rewrite this one."
            )

        # A refactor is allowed to move the pin, but the baseline must say so,
        # or the record stops describing the code it claims to pin.
        if current["source_pin"] != row["source_pin"]:
            problems.append(
                f"{rule}: version {version}'s source pin differs from the "
                f"recorded one.\n"
                f"    recorded: {current['source_pin']}\n"
                f"    declared: {row['source_pin']}\n"
                f"    A refactor at a fixed version is legitimate — update the "
                f"baseline row's `source_pin` in the same commit so the record "
                f"still describes the code."
            )

    declared_names = {r["rule"] for r in declarations}
    for orphan in sorted(set(history) - declared_names):
        problems.append(
            f"{orphan}: recorded in {BASELINE.name} but no longer declared in "
            f"SEMANTICS. A reducer that lost its declaration is unversioned; if "
            f"it was genuinely removed, remove its history deliberately."
        )

    return problems


def main() -> int:
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    sources = {
        str(p.relative_to(REPO)): p.read_text(encoding="utf-8") for p in DECLARATIONS
    }

    try:
        declarations = parse_all(sources)
    except GateError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1

    problems = check(
        declarations,
        baseline,
        lambda rel: (REPO / rel).read_text(encoding="utf-8"),
    )
    if problems:
        print("FAIL: reducer semantics gate\n", file=sys.stderr)
        for p in problems:
            print(f"  - {p}\n", file=sys.stderr)
        return 1

    print(f"OK: {len(declarations)} reducer(s) versioned, pinned and recorded")
    for row in declarations:
        print(f"  {row['rule']} v{row['version']}")
    return 0


# ---------------------------------------------------------------------------
# Self-tests
# ---------------------------------------------------------------------------


def _self_test() -> int:
    failures = []

    def check_case(name, fn):
        try:
            fn()
        except AssertionError as exc:
            failures.append(f"{name}: {exc}")

    src = (
        "// SEMANTICS-PIN-BEGIN: demo\n"
        "pub fn r(a: i64) -> bool { a > 0 }\n"
        "// SEMANTICS-PIN-END: demo\n"
    )

    def t_span_extracts_between_markers():
        assert extract_span(src, "demo").strip() == "pub fn r(a: i64) -> bool { a > 0 }"

    def t_comment_churn_does_not_move_the_pin():
        before = pin_of(src, "demo")
        churned = src.replace("pub fn r", "// a new remark\npub fn r")
        assert pin_of(churned, "demo") == before, "a comment moved the pin"

    def t_reformatting_does_not_move_the_pin():
        before = pin_of(src, "demo")
        wrapped = src.replace("{ a > 0 }", "{\n    a > 0\n}")
        assert pin_of(wrapped, "demo") == before, "rewrapping moved the pin"

    def t_a_real_edit_moves_the_pin():
        before = pin_of(src, "demo")
        edited = src.replace("a > 0", "a >= 0")
        assert pin_of(edited, "demo") != before, "an operator change did not move the pin"

    def t_a_string_change_moves_the_pin():
        # The reason `keep_strings=True` is passed: in these reducers a literal
        # can be the behaviour.
        s = (
            "// SEMANTICS-PIN-BEGIN: demo\n"
            'pub fn t() -> &str { "merged" }\n'
            "// SEMANTICS-PIN-END: demo\n"
        )
        assert pin_of(s, "demo") != pin_of(s.replace("merged", "retired"), "demo")

    def t_missing_marker_is_an_error():
        try:
            extract_span("no markers here", "demo")
        except GateError:
            return
        raise AssertionError("a missing marker was not an error")

    def t_duplicate_marker_is_an_error():
        try:
            extract_span(src + src, "demo")
        except GateError:
            return
        raise AssertionError("a duplicated marker was not an error")

    def t_empty_span_is_an_error():
        try:
            extract_span(
                "// SEMANTICS-PIN-BEGIN: demo\n\n// SEMANTICS-PIN-END: demo\n", "demo"
            )
        except GateError:
            return
        raise AssertionError("an empty pinned region was not an error")

    def t_an_unparseable_row_is_an_error_not_a_silent_skip():
        # The hole this closes: the row is well-formed Rust and obviously a
        # declaration, but its digest is not 64 hex characters, so `_SPEC`
        # cannot match it.
        good = (
            'RuleSpec { rule: RuleId::Foo, version: 1, '
            f'corpus_digest: "{"a" * 64}", source_pin: "{"b" * 64}", '
            'source_file: "f.rs", span: "foo", }'
        )
        bad = (
            'RuleSpec { rule: RuleId::Bar, version: 1, '
            'corpus_digest: "TBD", source_pin: "TBD", '
            'source_file: "f.rs", span: "bar", }'
        )
        text = 'Self::Foo => "foo",\nSelf::Bar => "bar",\n' + good + bad
        try:
            parse_declarations(text)
        except GateError as exc:
            assert "could parse only" in str(exc), exc
            return
        raise AssertionError("an unparseable declaration row was silently skipped")

    def t_zero_declarations_is_an_error():
        try:
            parse_declarations('Self::Foo => "foo",\nconst SEMANTICS: &[RuleSpec] = &[];')
        except GateError:
            return
        raise AssertionError("an empty declaration table passed")

    def t_a_struct_definition_is_not_mistaken_for_a_row():
        # `pub struct RuleSpec { rule: RuleId, version: u32, ... }` is a TYPE,
        # not a declaration. Parsing it as one would invent a rule named after
        # a field type and fail the run for a reason that does not exist.
        defn = (
            'Self::Demo => "demo",\n'
            "pub struct RuleSpec {\n"
            "    pub rule: RuleId,\n"
            "    pub version: u32,\n"
            "}\n"
            "const S: &[RuleSpec] = &[RuleSpec { rule: RuleId::Demo, version: 1, "
            'corpus_digest: "%s", source_pin: "%s", source_file: "a.rs", '
            'span: "demo", }];' % ("a" * 64, "b" * 64)
        )
        rows = parse_declarations(defn)
        assert len(rows) == 1, f"expected one row, parsed {len(rows)}"
        assert rows[0]["rule"] == "demo"

    def t_two_crates_sharing_a_rule_name_is_an_error():
        one = (
            'Self::Demo => "demo",\n'
            "const S: &[RuleSpec] = &[RuleSpec { rule: RuleId::Demo, version: 1, "
            'corpus_digest: "%s", source_pin: "%s", source_file: "a.rs", '
            'span: "demo", }];' % ("a" * 64, "b" * 64)
        )
        try:
            parse_all({"a.rs": one, "b.rs": one})
        except GateError:
            return
        raise AssertionError("two rules sharing a stable name passed")

    def t_unparseable_names_is_an_error():
        try:
            parse_declarations("no as_str arms at all")
        except GateError:
            return
        raise AssertionError("a table with no stable names passed")

    # --- the behavioural checks, over synthetic declarations ----------------

    def decl(**over):
        row = {
            "rule": "demo",
            "version": 1,
            "corpus_digest": "a" * 64,
            "source_pin": pin_of(src, "demo"),
            "source_file": "demo.rs",
            "span": "demo",
        }
        row.update(over)
        return [row]

    def base(**over):
        row = {
            "version": 1,
            "corpus_digest": "a" * 64,
            "source_pin": pin_of(src, "demo"),
        }
        row.update(over)
        return {"rules": {"demo": [row]}}

    read = lambda _rel: src  # noqa: E731

    def t_a_consistent_declaration_passes():
        assert check(decl(), base(), read) == []

    def t_behaviour_change_at_a_fixed_version_fails():
        problems = check(decl(corpus_digest="b" * 64), base(), read)
        assert problems, "a corpus digest change at a fixed version passed"
        assert "behaviour moved" in problems[0] or "corpus digest" in problems[0]

    def t_behaviour_change_with_a_bump_and_a_row_passes():
        b = base()
        b["rules"]["demo"].append(
            {"version": 2, "corpus_digest": "b" * 64, "source_pin": pin_of(src, "demo")}
        )
        assert check(decl(version=2, corpus_digest="b" * 64), b, read) == []

    def t_a_bump_without_a_baseline_row_fails():
        assert check(decl(version=2), base(), read), "a bump with no record passed"

    def t_a_stale_source_pin_fails():
        assert check(decl(source_pin="c" * 64), base(source_pin="c" * 64), read), (
            "a source pin that does not match the span passed"
        )

    def t_a_missing_baseline_entry_fails():
        assert check(decl(), {"rules": {}}, read), "an unrecorded rule passed"

    def t_an_orphaned_baseline_entry_fails():
        b = base()
        b["rules"]["gone"] = [
            {"version": 1, "corpus_digest": "a" * 64, "source_pin": "a" * 64}
        ]
        assert check(decl(), b, read), "an orphaned history passed"

    def t_a_version_gap_fails():
        b = base()
        b["rules"]["demo"].append(
            {"version": 3, "corpus_digest": "b" * 64, "source_pin": "a" * 64}
        )
        assert check(decl(version=3, corpus_digest="b" * 64), b, read), "a gap passed"

    cases = [(n, f) for n, f in sorted(locals().items()) if n.startswith("t_") and callable(f)]
    for name, fn in cases:
        check_case(name, fn)

    # A harness that discovers zero cases prints the same "OK" as one that ran
    # them all — the identical silent-pass shape `parse_declarations` refuses.
    # The floor is asserted rather than merely reported for that reason.
    expected = 21
    if len(cases) != expected:
        print(
            f"SELF-TEST HARNESS: discovered {len(cases)} cases, expected {expected}. "
            "Update the count deliberately when adding or removing one.",
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
    if "--self-test" in sys.argv:
        sys.exit(_self_test())
    sys.exit(main())
