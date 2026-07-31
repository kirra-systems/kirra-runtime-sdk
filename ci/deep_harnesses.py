#!/usr/bin/env python3
"""Enumerate the Kani harnesses gated behind the `deep-proofs` feature.

WHY THIS EXISTS. The weekly deep lane used to run every deep harness through a
single `cargo kani --features deep-proofs`, which had one genuinely good
property: a newly-demoted harness joined the campaign with no workflow edit.
Splitting the lane into a per-harness matrix (#1260) would have thrown that
away and replaced it with a hand-maintained list — a silent cap, which this
repository's CI doctrine forbids. So the matrix is DERIVED from the source
instead, and there is nothing to drift.

That is not a hypothetical risk. The gate is spelled two different ways today:

    #[cfg(feature = "deep-proofs")]                 # K3, K8, R2
    #[cfg(all(kani, feature = "deep-proofs"))]      # K7

A hand-written list built by grepping the first spelling silently omits K7 —
the exact harness that consumed an entire six-hour run. Matching on the
substring `deep-proofs` anywhere in a `#[cfg(...)]` attribute covers both, and
any third spelling someone invents later.

WHAT COUNTS. A harness is a function that carries BOTH a `#[cfg(...)]`
attribute mentioning `deep-proofs` AND a `#[kani::proof]` attribute, in the
same attribute block. The `#[kani::proof]` requirement is load-bearing:
`proofs_rss.rs` also gates two `deep-proofs` HELPERS (`grid_speed`,
`grid_param`) that are not harnesses, and feeding either to
`cargo kani --harness` would fail the lane on a nonexistent harness.

FAIL-CLOSED. Finding zero harnesses is an ERROR, not an empty matrix. An empty
matrix produces a lane that passes while verifying nothing — the same vacuous
-pass failure mode the mutation gate guards against with its own non-zero
check. If every harness is legitimately promoted out of the deep tier, delete
the lane deliberately rather than letting it pass empty.

Emits a JSON array on stdout, sorted, for `fromJSON` in a matrix strategy.
"""
from __future__ import annotations

import json
import pathlib
import re
import sys

# Repository-relative so it behaves the same locally and in CI.
KANI_SRC = pathlib.Path(__file__).resolve().parent.parent / "verification" / "kani" / "src"

_ATTR = re.compile(r"^\s*#\[")
_CFG_DEEP = re.compile(r"^\s*#\[cfg\b.*deep-proofs")
_KANI_PROOF = re.compile(r"^\s*#\[kani::proof\b")
_FN = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")


def harnesses_in(text: str) -> list[str]:
    """Names of `deep-proofs`-gated `#[kani::proof]` functions in one file."""
    found: list[str] = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if not _CFG_DEEP.match(lines[i]):
            i += 1
            continue
        # Walk the contiguous attribute block: attributes may appear in any
        # order, and `#[kani::stub(...)]` lines sit between them in practice.
        saw_proof = False
        j = i
        while j < len(lines) and (_ATTR.match(lines[j]) or not lines[j].strip()):
            if _KANI_PROOF.match(lines[j]):
                saw_proof = True
            j += 1
        if saw_proof and j < len(lines):
            m = _FN.match(lines[j])
            if m:
                found.append(m.group(1))
        i = j if j > i else i + 1
    return found


def main() -> int:
    if not KANI_SRC.is_dir():
        print(f"error: {KANI_SRC} is not a directory", file=sys.stderr)
        return 1

    names: list[str] = []
    for path in sorted(KANI_SRC.glob("*.rs")):
        names.extend(harnesses_in(path.read_text(encoding="utf-8")))

    names = sorted(set(names))
    if not names:
        print(
            "error: no `deep-proofs`-gated #[kani::proof] harnesses found under "
            f"{KANI_SRC}. Refusing to emit an empty matrix — a deep lane with no "
            "harnesses passes while verifying nothing. If the deep tier is now "
            "empty, remove the lane deliberately.",
            file=sys.stderr,
        )
        return 1

    print(json.dumps(names))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
