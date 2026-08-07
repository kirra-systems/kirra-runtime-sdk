#!/usr/bin/env python3
"""Q1 seam measurement instrument — ADR-0040's revisit trigger, made repeatable.

ADR-0040 deferred open question 1 (the `kirra-world` / `kirra-world-store` seam)
with a revisit trigger asking to *"measure what the seam actually carries"* on
completion of `WM_SCOPE.md` Tier 1. Three measurements have now been taken
against that trigger, and each re-derived the method by hand.

`WM_Q1_SEAM_BASELINE.md`'s own Measurement 2 says why that is the wrong shape:

    the baseline's *number* has a much shorter shelf life than its *method*,
    and the method is what should be reused.

This is that method. Running it reproduces every figure in Measurement 3 --
including one the hand count got wrong: a constructor tally of 11 that was
really 10, inflated by a doc-comment mention of `TrustAxes::new` that the
by-hand pass excluded for variant references and forgot to exclude here. The
error ran in the direction of the measurement's own conclusion, which is the
specific way a hand-counted series goes bad.

## Why this is NOT wired into CI

A gate reds when a number changes. Every number here is *supposed* to change —
the seam filling up is the outcome the trigger was written to detect, so a gate
would red on progress and be silenced within a week. It is a reporting
instrument, run when a measurement is being taken.

The one exception is `--check-population`, which is a genuine invariant rather
than a measurement: if a third crate starts depending on `kirra-world`, the
independence unit has changed and the next measurement is not comparable to the
recorded ones without saying so. That check is offered, deliberately unwired,
for whoever decides a measurement is due.

## Units

Counting unit, unchanged from the baseline so the series stays comparable:
one `use`/`pub use` item naming a `kirra_world` path, in the `src/` tree of a
crate that depends on `kirra-world`. Independence unit: the consuming crate.

The baseline's unit is insensitive to a braced list growing — `{A, B}` becoming
`{A, B, C, D, E, F}` does not move it. So `distinct core paths named` is
reported beside it rather than instead of it. Swapping units between
measurements would destroy the comparison the baseline exists to enable.

Test-only lines are counted separately: a seam carrying only test traffic is
still near-empty in the sense the ruling cares about.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import signal
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent


# The population as recorded by the baseline. Not a hardcoded answer to the
# measurement -- a hardcoded record of who was measured, so a newcomer is
# reported rather than silently averaged in.
RECORDED_CONSUMERS = ["kirra-world-store", "kirra-world-service"]

USE_ITEM = re.compile(r"^\s*(pub )?use kirra_world::")
CFG_TEST = "#[cfg(test)]"

# Core items whose appearance in a consumer means something stronger than a
# name crossing the seam. Kept explicit rather than derived from the core's
# public surface: a derived list would silently grow and make measurements
# incomparable, which is the failure this whole file exists to prevent.
CORE_TYPES = [
    "EventId", "ObservationId", "FrameId", "MapId", "EntityId", "ReferenceError",
    "TrustAxes", "Validity", "ValidityWindow", "Origin", "Corroboration",
    "Adjudication", "TrustGrade", "trust_grade", "validity_at",
    "RetentionPolicy", "RetentionDecision", "RetentionError", "RetentionSurvey",
    "Eligibility", "Blocker", "CompactablePrefix",
    "DomainInstant", "ClockDomain", "ResolutionOutcome",
]
CORE_ENUMS = [
    "RetentionDecision", "Eligibility", "Blocker", "CompactablePrefix",
    "Origin", "Corroboration", "Adjudication", "Validity", "TrustGrade",
    "ResolutionOutcome", "ClockDomain",
]
CONSTRUCTIBLE = [
    "EventId", "ObservationId", "FrameId", "MapId", "EntityId",
    "TrustAxes", "ValidityWindow", "RetentionPolicy", "DomainInstant",
]

CORE_PAT = re.compile(r"\b(" + "|".join(CORE_TYPES) + r")\b")
CTOR_PAT = re.compile(
    r"\b(" + "|".join(CONSTRUCTIBLE) + r")::(new|try_new|from_[a-z_]+|parse)\b"
)
VARIANT_PAT = re.compile(r"\b(" + "|".join(CORE_ENUMS) + r")::[A-Z]")
FIELD_PAT = re.compile(r"^\s*pub [a-z_0-9]+\s*:")
FN_PAT = re.compile(r"^\s*pub fn ")


def assert_repo_found() -> None:
    """Refuse to measure a tree that is not there.

    `REPO` is derived from this file's own location, so a copy run from
    elsewhere finds no crates and every count comes out zero. Zero is not a
    neutral result here -- "the seam is near-empty" is precisely the finding
    that authorizes collapsing it, so a lost tree must not be reported as an
    empty one. Fail loudly instead.
    """
    if not (REPO / "crates" / "kirra-world").is_dir():
        sys.exit(
            f"cannot find crates/kirra-world under {REPO}\n"
            "This script locates the repository relative to itself; run it "
            "from its place in the tree rather than from a copy.\n"
            "Refusing to measure, because a missing tree and an empty seam "
            "produce the same numbers and mean opposite things."
        )


def split_at_tests(lines: list[str]) -> tuple[list[str], list[str]]:
    """Split a source file at its first `#[cfg(test)]`.

    Crude on purpose. A real parse would be more accurate and less auditable,
    and every file in the measured population puts its test module last.
    """
    for i, line in enumerate(lines):
        if line.strip() == CFG_TEST:
            return lines[:i], lines[i:]
    return lines, []


def is_comment(line: str) -> bool:
    s = line.strip()
    return s.startswith("//")


def use_items(lines: list[str]) -> list[str]:
    """Whole `use` statements, joined across line breaks at the `;`."""
    out, i = [], 0
    while i < len(lines):
        if USE_ITEM.match(lines[i]):
            stmt, j = lines[i], i
            while ";" not in stmt and j + 1 < len(lines):
                j += 1
                stmt += " " + lines[j].strip()
            out.append(stmt)
            i = j
        i += 1
    return out


def names_in(stmt: str) -> set[str]:
    """The distinct `kirra_world` paths a single `use` item names."""
    body = stmt.split("kirra_world::", 1)[1].rstrip().rstrip(";")
    if "{" in body:
        prefix = body.split("{")[0]
        inner = body[body.index("{") + 1 : body.rindex("}")]
        raw = [n.strip() for n in inner.split(",") if n.strip()]
    else:
        prefix, raw = "", [body.strip()]
    # `X as Y` still names X; the rename is the consumer's business.
    return {prefix + n.split(" as ")[0].strip() for n in raw if n.strip()}


def fn_signatures(lines: list[str]) -> list[str]:
    """Public fn signatures, joined to the opening brace."""
    out, i = [], 0
    while i < len(lines):
        if FN_PAT.match(lines[i]):
            sig, j = lines[i], i
            while "{" not in sig and ";" not in sig and j + 1 < len(lines):
                j += 1
                sig += " " + lines[j].strip()
            out.append(sig.split("{")[0])
            i = j
        i += 1
    return out


def measure(crate: str) -> dict:
    src = REPO / "crates" / crate / "src"
    m = {
        "lines_non_test": 0, "lines_test": 0, "names": set(),
        "ctors": 0, "variants": 0, "pub_fields": [], "pub_fns": [],
    }
    for f in sorted(src.glob("*.rs")):
        lines = f.read_text().splitlines()
        prod, test = split_at_tests(lines)

        items = use_items(prod)
        m["lines_non_test"] += len(items)
        m["lines_test"] += len(use_items(test))
        for stmt in items:
            m["names"] |= names_in(stmt)

        for n, line in enumerate(prod, start=1):
            if is_comment(line):
                continue
            if CTOR_PAT.search(line):
                m["ctors"] += 1
            m["variants"] += len(VARIANT_PAT.findall(line))
            if FIELD_PAT.match(line) and CORE_PAT.search(line):
                m["pub_fields"].append(f"{f.name}:{n}")

        for sig in fn_signatures(prod):
            if CORE_PAT.search(sig):
                m["pub_fns"].append(" ".join(sig.split())[:100])
    return m


def declared_consumers() -> list[str]:
    """Crates whose manifest declares a `kirra-world` path dependency."""
    found = []
    for manifest in sorted((REPO / "crates").glob("*/Cargo.toml")):
        crate = manifest.parent.name
        if crate == "kirra-world":
            continue
        for line in manifest.read_text().splitlines():
            s = line.strip()
            if s.startswith("#"):
                continue
            if re.match(r"^kirra-world\s*=", s):
                found.append(crate)
                break
    return found


def main() -> int:
    # This prints a long report and will be piped to `head`. Without this,
    # SIGPIPE surfaces as a traceback, which reads as the measurement having
    # failed rather than the reader having stopped early.
    try:
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    except (AttributeError, ValueError):
        pass  # not POSIX, or not the main thread; the traceback is cosmetic

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--check-population",
        action="store_true",
        help="exit non-zero if the set of direct kirra-world consumers has "
             "changed since the recorded measurements",
    )
    args = ap.parse_args()

    assert_repo_found()
    actual = declared_consumers()
    if args.check_population:
        if sorted(actual) != sorted(RECORDED_CONSUMERS):
            print("POPULATION CHANGED -- prior measurements are not comparable")
            print(f"  recorded: {sorted(RECORDED_CONSUMERS)}")
            print(f"  actual  : {sorted(actual)}")
            print("Update RECORDED_CONSUMERS and say so in the measurement.")
            return 1
        print(f"population unchanged: {sorted(actual)}")
        return 0

    if sorted(actual) != sorted(RECORDED_CONSUMERS):
        print("!! population differs from the recorded measurements; "
              "see --check-population\n")

    print("Q1 seam measurement -- ADR-0040 revisit trigger\n")
    print(f"{'consumer':24} {'non-test':>9} {'test-only':>10} {'names':>7}")
    tot_n = tot_t = 0
    all_names: set[str] = set()
    results = {}
    for crate in actual:
        m = measure(crate)
        results[crate] = m
        tot_n += m["lines_non_test"]
        tot_t += m["lines_test"]
        all_names |= m["names"]
        print(f"{crate:24} {m['lines_non_test']:>9} {m['lines_test']:>10} "
              f"{len(m['names']):>7}")
    print(f"{'TOTAL':24} {tot_n:>9} {tot_t:>10} {len(all_names):>7}")

    print("\nWhat the seam does (non-test, doc comments excluded):")
    print(f"{'consumer':24} {'ctors':>7} {'variants':>9} "
          f"{'pub fields':>11} {'pub fns':>8}")
    for crate, m in results.items():
        print(f"{crate:24} {m['ctors']:>7} {m['variants']:>9} "
              f"{len(m['pub_fields']):>11} {len(m['pub_fns']):>8}")

    print("\nDistinct core paths named:")
    for n in sorted(all_names):
        print(f"  {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
