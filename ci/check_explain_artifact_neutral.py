#!/usr/bin/env python3
"""Tier 4 box 4c — the capability gate on the World→Mick explanation seam.

THE RULE
--------

    The explanation artifact is PRESENTATION-ONLY. Its public API may carry
    rendered text, opaque digests, categorical state, counts and positions; it
    may not carry a Kirra World COORDINATE that could be used to query Kirra
    World again.

WHY THIS AND NOT "kirra-mick does not depend on kirra-world*"
-------------------------------------------------------------

That property is necessary and it is not sufficient, which is the whole point of
`KIRRA-WM-EXPLAIN-PLACEMENT-001`. The Cargo graph can be spotless while the
artifact hands Mick a `root_generation` and an `at_generation` -- and those are
exactly the two arguments `WorldStore::provenance_tree(root, at, spec)` takes. A
renderer holding them can ask Kirra World another question over IPC, at runtime,
and reconstruct the dependency the fence refused. The boundary would exist on
paper only.

So the ban is structural, in the shape `kirra-proposal-context` established: a
seam is made safe by having NOWHERE TO PUT the dangerous thing. There the
dangerous thing is a checker bound and the ban is on numeric magnitudes; here it
is a query handle and the ban is on coordinates.

THE BAN IS ON NUMBERS, with a short justified allowlist
-------------------------------------------------------

The first draft of this gate banned only 64-bit integers and coordinate-sounding
names, reasoning that an explanation legitimately needs SOME numbers -- `depth`
indents, `carriers` counts, `node` positions -- and that making each one write a
justification would produce an allowlist too long to read.

That reasoning was wrong, and its own mutation check showed it: `anchor: u32`
passed. A u32 is a perfectly serviceable query handle for any store with fewer
than four billion events, so the narrow-width exemption was an open door under an
innocuous name.

The allowlist turned out to be FIVE entries, and they were already written. So
the rule is the strict one after all -- every primitive numeric field needs an
entry -- and it costs nothing, because the legitimate numbers here are few and
each can write its justification. A coordinate cannot: "addresses a row in
world_events" is the sentence that fails review.

The name check is kept alongside, for the case the width check cannot see: a
coordinate hidden inside an allowlisted-looking name.

WHAT IS AND IS NOT CHECKED
--------------------------

CHECKED: fields of public structs and payloads of public enum variants in the
guarded crate -- the values that CROSS the seam.

NOT CHECKED: function parameters and locals. The projection on the World side
necessarily handles generations; it is what converts them into labels. The rule
bites on what is CARRIED, because that is what a renderer can act on.

THE RESIDUAL, stated rather than papered over: a producer could encode a
generation into a `DisplayLabel` string ("generation 42") and a renderer could
parse it back. Nothing here would catch that, and nothing cheap would. What this
removes is doing it by accident, doing it in a type, and doing it without a
reviewer seeing a diff in a file whose only purpose is to say no.

Self-tests: `python3 ci/check_explain_artifact_neutral.py --self-test`.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# The crate this gate guards. A list, so a second neutral artifact crate
# inherits the rule by being added here rather than by remembering to -- the
# same affordance `check_proposal_context_symbolic.py` provides.
GUARDED_CRATES = ("crates/kirra-explain-types",)

# ---------------------------------------------------------------------------
# The dependency half of the rule
# ---------------------------------------------------------------------------
#
# The property is NOT "this manifest is empty". It used to be stated that way,
# and the manifest said so too, which was a stronger sentence than the invariant
# it was protecting. `serde` had to go in the moment these types became the wire
# contract (the derive must sit on the type DEFINITION -- a `#[serde(remote)]`
# mirror in a sibling crate drifts silently, which is the defect class this tier
# exists to close). A rule that overstates itself is broken the first time it is
# inconvenient, and then it protects nothing.
#
# So the rule is the narrower, checkable one:
#
#     These presentation types stay NEUTRAL. They may not acquire a dependency
#     on Kirra World, on Mick, on the doer (planner/map/perception), on the
#     checker/governor, or on anything carrying actuation authority.
#
# That is strictly stronger than emptiness was in practice, because emptiness
# was an accident that no check could distinguish from a deliberate boundary.
#
# WHY THE FAMILIES ARE IMPORTED, NOT RETYPED
# ------------------------------------------
# `check_kirra_world_bidirectional_fence` already classifies the actuation
# family, and `kirra-explain-types` is already inside its FENCE_A_EXTRA_PACKAGES,
# so Fence A check 2 walks this crate's CLOSURE against those names -- a
# strictly stronger check than the direct one below. Retyping the list here
# would create two statements of one classification, and the one that stops
# being updated is the one that silently stops matching. That is the same
# failure mode as the serde mirror, one level up. So they are IMPORTED: a crate
# added to the fence's actuation set is forbidden here the same day.
#
# The direct check is kept anyway, for the reason the World ban was always
# direct: `KIRRA-WM-EXPLAIN-PLACEMENT-001` is an invariant about THIS manifest,
# and an invariant that holds only through a chain of other facts is an
# invariant one refactor away from being false. A direct hit also names the
# offending line instead of a closure path.
_CI_DIR = Path(__file__).resolve().parent
if str(_CI_DIR) not in sys.path:
    sys.path.insert(0, str(_CI_DIR))

from check_kirra_world_bidirectional_fence import (  # noqa: E402
    ACTUATION_EXTERNAL,
    ACTUATION_PACKAGES,
    WORLD_PACKAGE_EXACT,
    WORLD_PACKAGE_PREFIX,
)

# Families the FENCE does not classify, named here because this seam is where
# they matter. Each entry is the reason the dependency would break the ruling,
# printed verbatim when the gate fires.
FORBIDDEN_PACKAGES: dict[str, str] = {
    "kirra-mick": (
        "the renderer -- an arrow from the shared contract INTO Mick makes "
        "kirra-world-service depend on Mick transitively, which is the coupling "
        "the placement ruling exists to prevent, reversed"
    ),
    "kirra-sidecars": (
        "depends on kirra-mick, so it reconstructs the Mick edge one hop out"
    ),
    "kirra-planner": "the doer -- a planner type here would make the artifact plan-shaped",
    "kirra-map": "doer-side lane graph; carries World-adjacent coordinates",
    "kirra-taj": "doer-side perception",
    "kirra-core": "the checker's home (KirraKernelGovernor, the frozen kinematics talisman)",
    "kirra-trajectory": "the checker (validate_trajectory_slow)",
    "kirra-safety-authority": "the posture DAG -- safety authority",
    "kirra-policy-types": "command classification -- authorization, not presentation",
    "kirra-persistence": "the store; a persistence type on this wire is a coordinate by another name",
    "kirra-proposal-context": "the SANCTIONED answer boundary -- it depends on kirra-world-service",
    "kirra-mission-orchestrator": "sits above kirra-proposal-context, same reach",
}

FORBIDDEN_PREFIXES: tuple[tuple[str, str], ...] = (
    (
        WORLD_PACKAGE_PREFIX,
        "Kirra World itself -- the artifact would carry the store it is meant to "
        "stand in for",
    ),
)

# The integer widths a Kirra World coordinate is. `generation` is `i64` in
# `world_events` (it is the table's INTEGER PRIMARY KEY), and every bitemporal
# instant in this system is `i64` milliseconds. A 64-bit integer crossing this
# seam is therefore a coordinate until proven otherwise.
COORDINATE_WIDTHS = {"i64", "u64", "i128", "u128", "isize", "usize"}

# Field names that say "coordinate" whatever their type. The name check exists
# because `generation: u32` would sidestep the width check, and a u32 is a
# perfectly serviceable query handle for any store with fewer than four billion
# events.
COORDINATE_NAMES = (
    "generation",
    "observation_id",
    "event_id",
    "txn_time",
    "valid_from",
    "valid_to",
    "at_ms",
    "as_of",
    "cursor",
    "offset",
)

# Legitimate carried values, each justified. Kept SHORT on purpose: this is the
# list a reviewer reads to see what the seam admits, and it stops being read the
# moment it stops being short.
ALLOWLIST: dict[str, str] = {
    "ExplanationArtifact.version": (
        "the artifact contract version -- identifies THIS crate's shape, and "
        "nothing in Kirra World can be looked up with it"
    ),
    "ExplanationNode.depth": (
        "indentation for a rendering; no Kirra World call takes a depth"
    ),
    "ExplanationNode.parent": (
        "artifact-local node position; addresses this document, not the store"
    ),
    "BranchState.Plural.carriers": (
        "how many events a citation could not be narrowed between -- the FACT "
        "the plural state carries, and a cardinality rather than an identity"
    ),
    "BranchContinuation.Expanded.node": (
        "artifact-local node position, same as ExplanationNode.parent"
    ),
}

# Every primitive that can address a row. `bool` and `char` are absent
# deliberately: neither can index anything, which is the whole question.
NUMERIC_PRIMITIVES = COORDINATE_WIDTHS | {
    "u8",
    "u16",
    "u32",
    "i8",
    "i16",
    "i32",
    "f32",
    "f64",
}

_PUB_STRUCT = re.compile(r"pub\s+struct\s+(?P<name>\w+)\s*\{(?P<body>[^}]*)\}", re.S)
_PUB_ENUM = re.compile(r"pub\s+enum\s+(?P<name>\w+)\s*\{(?P<body>.*?)\n\}", re.S)
_VARIANT = re.compile(r"^\s*(?P<name>[A-Z]\w*)\s*\{(?P<body>[^}]*)\}", re.M)

# Fields are matched LINE BY LINE rather than by a comma-delimited regex. The
# first draft required a trailing comma and therefore missed
# `V { generation: i64 }` written on one line — caught by this gate's own
# self-test, which is what that test is for. Line-based also keeps a type
# containing a comma (`HashMap<K, V>`) intact instead of truncating it at the
# first comma and going blind to whatever followed.
#
# The residual: a field whose type is wrapped across lines is only examined one
# line at a time. Stated rather than hidden; rustfmt does not wrap the short
# types this seam admits.
_FIELD_LINE = re.compile(r"^\s*(?:pub\s+)?(?P<name>\w+)\s*:\s*(?P<ty>.+?)\s*,?\s*$")


class GateError(Exception):
    """A violation worth failing CI over."""


def strip_comments(text: str) -> str:
    """Drop `//` comments so a coordinate NAMED in prose is not a violation."""
    return re.sub(r"//[^\n]*", "", text)


def base_types(ty: str) -> set[str]:
    """Every identifier in a type expression, so `Option<i64>` yields `i64`."""
    return set(re.findall(r"\w+", ty))


def violations_in(text: str) -> list[str]:
    """Every carried field that could hold a Kirra World coordinate."""
    src = strip_comments(text)
    found: list[str] = []

    def check(owner: str, body: str) -> None:
        for line in body.splitlines():
            m = _FIELD_LINE.match(line)
            if not m:
                continue
            name, ty = m.group("name"), m.group("ty").strip()
            key = f"{owner}.{name}"
            if key in ALLOWLIST:
                continue
            types = base_types(ty)
            if types & NUMERIC_PRIMITIVES:
                width = "a 64-bit integer, which is what a Kirra World "
                width += "generation and every bitemporal instant are"
                narrow = "a numeric field, and a narrow integer is still a "
                narrow += "usable query handle"
                why = width if types & COORDINATE_WIDTHS else narrow
                found.append(
                    f"{key}: `{ty}` is {why} — allowlist it with a "
                    "justification if it cannot address the store"
                )
                continue
            if any(n in name.lower() for n in COORDINATE_NAMES):
                found.append(
                    f"{key}: the field NAME says coordinate, whatever its type"
                )

    for m in _PUB_STRUCT.finditer(src):
        check(m.group("name"), m.group("body"))

    for m in _PUB_ENUM.finditer(src):
        enum_name = m.group("name")
        for v in _VARIANT.finditer(m.group("body")):
            check(f"{enum_name}.{v.group('name')}", v.group("body"))

    return found


def declared_dependencies(manifest_text: str) -> list[str]:
    """Every package name declared as a dependency, comments stripped.

    Deliberately not `kirra-`-scoped: an external actuation transport
    (`r2r`, `serialport`, ...) is exactly as disqualifying as an internal one,
    and scoping the regex to `kirra-` was how the previous version could only
    ever have caught half the rule.
    """
    text = strip_comments_toml(manifest_text)
    return sorted(set(re.findall(r"^\s*([A-Za-z][\w-]*)\s*=", text, re.M)))


def forbidden_dependencies(manifest_text: str) -> list[str]:
    """The pure half: which declared dependencies break neutrality, and why.

    Pure and text-level ON PURPOSE. The path-reading wrapper below cannot be
    handed a crafted manifest, so a self-test written against it could only
    assert that the real crate passes -- which is true of a function that
    returns the empty list unconditionally. Taking text means the self-test can
    feed it a forbidden dependency and assert the gate FLAGS it.
    """
    problems: list[str] = []
    for dep in declared_dependencies(manifest_text):
        if dep in FORBIDDEN_PACKAGES:
            problems.append(f"`{dep}` — {FORBIDDEN_PACKAGES[dep]}")
        elif dep in WORLD_PACKAGE_EXACT:
            problems.append(f"`{dep}` — Kirra World itself")
        elif dep in ACTUATION_PACKAGES:
            problems.append(
                f"`{dep}` — {ACTUATION_PACKAGES[dep]}; actuation authority never "
                "rides a presentation type"
            )
        elif dep in ACTUATION_EXTERNAL:
            problems.append(
                f"`{dep}` — an actuator transport; a renderer inheriting it could "
                "reach a wheel from an explanation"
            )
        else:
            for prefix, why in FORBIDDEN_PREFIXES:
                if dep.startswith(prefix):
                    problems.append(f"`{dep}` — {why}")
                    break
    return problems


def check_dependency_neutrality(root: Path, crate: str) -> list[str]:
    """The direct half: the guarded crate's manifest must stay neutral."""
    manifest = root / crate / "Cargo.toml"
    if not manifest.is_file():
        raise GateError(f"guarded crate has no manifest: {crate}")
    return [
        f"{crate}/Cargo.toml depends on {p}"
        for p in forbidden_dependencies(manifest.read_text(encoding="utf-8"))
    ]


def strip_comments_toml(text: str) -> str:
    return re.sub(r"#[^\n]*", "", text)


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    problems: list[str] = []
    checked = 0

    for crate in GUARDED_CRATES:
        problems += check_dependency_neutrality(root, crate)
        src_dir = root / crate / "src"
        if not src_dir.is_dir():
            raise GateError(f"guarded crate has no src/: {crate}")
        files = sorted(src_dir.rglob("*.rs"))
        if not files:
            raise GateError(f"guarded crate has no sources: {crate}")
        for path in files:
            checked += 1
            for v in violations_in(path.read_text(encoding="utf-8")):
                problems.append(f"{path.relative_to(root)}: {v}")

    if not checked:
        # The silent-pass shape: a gate that examined nothing prints the same
        # OK as one that examined everything.
        raise GateError("examined zero files — the gate would pass vacuously")

    if problems:
        print("FAIL: explanation-artifact neutrality gate\n", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nAn explanation crosses a process boundary to a renderer that must "
            "not query\nKirra World.\n"
            "\n  A CARRIED FIELD: a coordinate is the argument that query would "
            "need. Carry a\n  DisplayLabel or an EvidenceDigest instead, or "
            "justify the field in ALLOWLIST if\n  it genuinely cannot address "
            "the store.\n"
            "\n  A DEPENDENCY: these types are shared by both sides, so anything "
            "they depend on,\n  both sides inherit. Neutrality is the rule — not "
            "an empty manifest. An\n  authority-free crate (serde and its like) "
            "is fine; World, Mick, the doer, the\n  checker and actuation are "
            "not, and no allowlist admits them.",
            file=sys.stderr,
        )
        return 1

    print(f"Explanation-artifact neutrality gate green ({checked} file(s)).")
    return 0


def _self_test() -> int:
    failures: list[str] = []

    def case(name: str, fn) -> None:
        try:
            fn()
        except AssertionError as exc:
            failures.append(f"{name}: {exc}")

    def t_a_generation_field_is_caught():
        src = "pub struct A {\n    pub generation: i64,\n}"
        assert violations_in(src), "an i64 generation was admitted"

    def t_a_narrow_generation_is_caught_by_name():
        src = "pub struct A {\n    pub generation: u32,\n}"
        assert violations_in(src), "a u32 named generation was admitted"

    def t_an_innocently_named_i64_is_caught_by_width():
        src = "pub struct A {\n    pub anchor: i64,\n}"
        assert violations_in(src), "an i64 under another name was admitted"

    def t_an_optioned_coordinate_is_caught():
        src = "pub struct A {\n    pub anchor: Option<i64>,\n}"
        assert violations_in(src), "a wrapped coordinate was admitted"

    def t_an_enum_payload_is_checked():
        src = "pub enum E {\n    V { at_generation: i64 },\n}"
        assert violations_in(src), "an enum variant payload was not checked"

    def t_an_unjustified_narrow_integer_is_caught():
        # The case that defeated the first draft of this gate: a usable query
        # handle wearing a name no coordinate list would flag.
        src = "pub struct A {\n    pub anchor: u32,\n}"
        assert violations_in(src), "an unjustified u32 handle was admitted"

    def t_an_allowlisted_count_is_admitted():
        src = "pub struct ExplanationNode {\n    pub depth: u16,\n}"
        assert not violations_in(src), "an allowlisted count was refused"

    def t_a_label_is_admitted():
        src = "pub struct A {\n    pub claim: DisplayLabel,\n}"
        assert not violations_in(src), "rendered text was refused"

    def t_a_coordinate_named_in_a_comment_is_not_a_violation():
        src = "pub struct A {\n    // never carry a generation: i64 here\n    pub claim: DisplayLabel,\n}"
        assert not violations_in(src), "prose was treated as a field"

    def t_the_real_crate_is_clean():
        root = Path(__file__).resolve().parent.parent
        for path in sorted((root / GUARDED_CRATES[0] / "src").rglob("*.rs")):
            assert not violations_in(
                path.read_text(encoding="utf-8")
            ), f"{path} violates its own gate"

    # --- the dependency half -------------------------------------------------
    #
    # These are the non-vacuity controls for the rule that changed. Each one
    # feeds a CRAFTED manifest and asserts the gate FLAGS it. The distinction
    # matters: a test that only ran the real manifest through and found nothing
    # would pass identically against a function that returns [] unconditionally,
    # which is how a widened rule can be shipped without ever being exercised.

    def t_a_world_dependency_is_flagged():
        src = '[dependencies]\nkirra-world-store = { path = "../kirra-world-store" }\n'
        assert forbidden_dependencies(src), "a kirra-world* dependency was admitted"

    def t_a_mick_dependency_is_flagged():
        src = '[dependencies]\nkirra-mick = { path = "../kirra-mick" }\n'
        assert forbidden_dependencies(src), "the renderer edge was admitted"

    def t_a_checker_dependency_is_flagged():
        src = '[dependencies]\nkirra-core = { path = "../kirra-core" }\n'
        assert forbidden_dependencies(src), "a checker dependency was admitted"

    def t_a_doer_dependency_is_flagged():
        src = '[dependencies]\nkirra-planner = { path = "../kirra-planner" }\n'
        assert forbidden_dependencies(src), "a planner dependency was admitted"

    def t_an_actuation_dependency_is_flagged():
        # Imported from the fence, so this also proves the import is live: if
        # the constant were renamed away, this case fails rather than the gate
        # silently checking an empty set.
        src = '[dependencies]\nkirra-release-token = { path = "../kirra-release-token" }\n'
        assert forbidden_dependencies(src), "an actuation dependency was admitted"

    def t_an_external_actuator_transport_is_flagged():
        # The old regex was scoped to `kirra-`, so this whole family was
        # invisible to it -- a serial port is not a kirra crate.
        src = '[dependencies]\nserialport = "4"\n'
        assert forbidden_dependencies(src), "an external actuator transport was admitted"

    def t_serde_is_admitted():
        # The rule is neutrality, not emptiness. If this ever fails, the gate
        # has drifted back to counting entries.
        src = '[dependencies]\nserde = { version = "1", features = ["derive"] }\n'
        assert not forbidden_dependencies(src), "serde was rejected by a neutrality rule"

    def t_a_commented_out_dependency_is_not_a_violation():
        src = '[dependencies]\n# kirra-mick = { path = "../kirra-mick" }\nserde = "1"\n'
        assert not forbidden_dependencies(src), "a commented-out dep was read as real"

    def t_the_real_manifest_is_neutral():
        root = Path(__file__).resolve().parent.parent
        text = (root / GUARDED_CRATES[0] / "Cargo.toml").read_text(encoding="utf-8")
        assert not forbidden_dependencies(text), "the guarded crate breaks its own rule"
        # ...and non-vacuously: it must actually be declaring something, or the
        # case above passes because the parser found nothing at all.
        assert "serde" in declared_dependencies(text), (
            "no serde in the guarded manifest — either the parser is broken or "
            "the wire contract lost its derive"
        )

    cases = [(n, f) for n, f in sorted(locals().items()) if n.startswith("t_") and callable(f)]
    for name, fn in cases:
        case(name, fn)

    expected = 19
    if len(cases) != expected:
        print(
            f"SELF-TEST HARNESS: discovered {len(cases)} cases, expected {expected}.",
            file=sys.stderr,
        )
        return 1
    if failures:
        for f in failures:
            print(f"SELF-TEST FAIL: {f}", file=sys.stderr)
        return 1
    print(f"self-tests OK ({len(cases)} cases)")
    return 0


if __name__ == "__main__":
    try:
        if "--self-test" in sys.argv:
            sys.exit(_self_test())
        sys.exit(main())
    except GateError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
