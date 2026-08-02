#!/usr/bin/env python3
"""Website evidence-citation path gate (Tier 0).

The marketing site backs its claims with "evidence chips" that deep-link into
this repository:

    ${evRow("docs/protocol_adapters.md", "crates/kirra-industrial")}
    ${ev("src/wcet_gate.rs:92", "wcet_gate.rs")}

`website/_src/template.mjs` turns each one into
`https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/<path>`.

Nothing connected the site to the tree, so a rename on this side silently turned
a public evidence link into a 404. That is exactly the failure mode the evidence
chips exist to prevent: a visitor clicking "here is the code that proves it" and
landing on "404 — this is not the web page you are looking for". At the time
this gate was written NINE citations were already dead, broken by the v2.0.0
de-monolith (`src/federation.rs` -> `kirra-fleet-types`, `src/protocol_adapter.rs`
-> `kirra-industrial`) and by #1030 folding `crates/kirra-verifier-pg` into
`kirra-persistence`.

SCOPE — this gate checks that every cited PATH EXISTS. Deliberately not in
scope:

  * whether a `:line` suffix still points at the intended line (line drift).
    A citation records a coordinate, not a claim; the repo never wrote down what
    the cited line was supposed to contain, so nothing can verify it. Catching
    that needs an anchored citation format and is the Tier 2 follow-up.
  * whether the cited file actually supports the claim in the surrounding prose.
    That is a human review judgement and always will be.

So: a green run means every evidence link resolves, NOT that every evidence link
is apposite.

Usage:
    python3 ci/check_website_citations.py            # gate (exit 1 on a dead path)
    python3 ci/check_website_citations.py --self-test  # non-vacuity of the parser
    python3 ci/check_website_citations.py --list     # dump every resolved citation
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES_DIR = os.path.join("website", "_src", "pages")

# `ev(` / `evRow(` as a standalone identifier. The negative lookbehind keeps
# this from firing inside a longer name (`prev(`, `retrieve(`, `obj.ev(`).
CALL_RE = re.compile(r"(?<![A-Za-z0-9_$.])(ev|evRow)\s*\(")

EXIT_OK = 0
EXIT_VIOLATION = 1
EXIT_USAGE = 2


@dataclass(frozen=True)
class Citation:
    """One repo path cited by one evidence chip."""

    page: str  # page file, repo-relative
    line: int  # line within the page source
    raw: str  # the citation as authored, e.g. "src/wcet_gate.rs:92"
    path: str  # the repo path alone, ":line" stripped


def _split_args(src: str, start: int) -> tuple[list[str], int]:
    """Split a call's argument list into top-level argument sources.

    `start` indexes the character just past the opening paren. Returns the
    argument sources and the index just past the closing paren. Tracks quoting
    and nesting so a comma inside a string, a template literal, or a nested call
    does not split an argument. Returns ([], -1) if the call is unterminated.
    """
    args: list[str] = []
    depth = 0
    quote: str | None = None
    escaped = False
    cur = []
    i = start
    while i < len(src):
        ch = src[i]

        if quote is not None:
            cur.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == quote:
                quote = None
            # A `${` inside a template literal re-enters code; treat the whole
            # template as opaque, which is right for our purposes -- an
            # interpolated citation is not a string literal and gets counted as
            # unresolvable rather than silently dropped.
            i += 1
            continue

        if ch in "\"'`":
            quote = ch
            cur.append(ch)
        elif ch in "([{":
            depth += 1
            cur.append(ch)
        elif ch in ")]}":
            if ch == ")" and depth == 0:
                args.append("".join(cur))
                return [a for a in args if a.strip()], i + 1
            depth -= 1
            cur.append(ch)
        elif ch == "," and depth == 0:
            args.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
        i += 1

    return [], -1


def _string_literal(arg: str) -> str | None:
    """Return the value of `arg` if it is a plain single/double-quoted literal."""
    s = arg.strip()
    if len(s) < 2 or s[0] not in "\"'" or s[-1] != s[0]:
        return None
    body = s[1:-1]
    # A literal containing its own quote unescaped means we mis-sliced.
    if s[0] in body.replace("\\" + s[0], ""):
        return None
    return body.replace("\\" + s[0], s[0])


def scan_source(page: str, src: str) -> tuple[list[Citation], list[tuple[int, str]]]:
    """Extract citations from one page source.

    Returns (citations, unresolvable) where `unresolvable` records call sites
    whose path argument is not a plain string literal. Those are reported as
    coverage gaps rather than passed over in silence -- a gate that quietly
    skips what it cannot parse reads as "all clear" when it is not.
    """
    citations: list[Citation] = []
    unresolvable: list[tuple[int, str]] = []

    for m in CALL_RE.finditer(src):
        kind = m.group(1)
        args, _end = _split_args(src, m.end())
        line = src.count("\n", 0, m.start()) + 1
        if not args:
            unresolvable.append((line, f"{kind}(...) — could not parse argument list"))
            continue

        # ev(path, label?) cites only its first argument; evRow(...paths) cites
        # every argument.
        path_args = args if kind == "evRow" else args[:1]
        for arg in path_args:
            raw = _string_literal(arg)
            if raw is None:
                unresolvable.append((line, f"{kind}({arg.strip()[:60]}) — not a string literal"))
                continue
            # Strip a trailing ":<line>"; a colon elsewhere is part of the path.
            path = raw
            if ":" in raw:
                head, _, tail = raw.rpartition(":")
                if head and tail.isdigit():
                    path = head
            citations.append(Citation(page=page, line=line, raw=raw, path=path))

    return citations, unresolvable


def collect(root: str) -> tuple[list[Citation], list[tuple[str, int, str]]]:
    pages_dir = os.path.join(root, PAGES_DIR)
    if not os.path.isdir(pages_dir):
        raise SystemExit(f"{PAGES_DIR} not found under {root}")

    citations: list[Citation] = []
    unresolvable: list[tuple[str, int, str]] = []
    for name in sorted(os.listdir(pages_dir)):
        if not name.endswith(".mjs"):
            continue
        rel = os.path.join(PAGES_DIR, name)
        with open(os.path.join(pages_dir, name), encoding="utf-8") as fh:
            src = fh.read()
        page_cites, page_unres = scan_source(rel, src)
        citations.extend(page_cites)
        unresolvable.extend((rel, ln, why) for ln, why in page_unres)
    return citations, unresolvable


def check(root: str) -> tuple[list[Citation], list[Citation], list[tuple[str, int, str]]]:
    """Return (all citations, dead ones, unresolvable call sites)."""
    citations, unresolvable = collect(root)
    dead = [c for c in citations if not os.path.exists(os.path.join(root, c.path))]
    return citations, dead, unresolvable


# --------------------------------------------------------------------------
# Self-test — the detector must be non-vacuous. A gate that cannot be made to
# fail is not evidence of anything.
# --------------------------------------------------------------------------

_FIXTURE = '''
export const page = () => `
  ${evRow("README.md", "no/such/file.rs")}
  ${ev("Cargo.toml:12", "manifest")}
  ${ev(somePath)}
  <a class="evidence" href="company.html">not a citation</a>
  ${evRow("docs/adr", "also/missing")}
`;
'''


def self_test(root: str) -> int:
    cites, unres = scan_source("fixture.mjs", _FIXTURE)
    failures: list[str] = []

    got = sorted(c.raw for c in cites)
    want = sorted(["README.md", "no/such/file.rs", "Cargo.toml:12", "docs/adr", "also/missing"])
    if got != want:
        failures.append(f"extraction: expected {want}, got {got}")

    # The ":12" suffix must be stripped for the existence test, and only there.
    manifest = [c for c in cites if c.raw == "Cargo.toml:12"]
    if not manifest or manifest[0].path != "Cargo.toml":
        failures.append("line-suffix stripping: 'Cargo.toml:12' should resolve to path 'Cargo.toml'")

    # The href= chip is NOT an ev() call and must not be picked up.
    if any("company.html" in c.raw for c in cites):
        failures.append("false positive: an href= evidence link was treated as a citation")

    # A non-literal argument must surface as a coverage gap, not vanish.
    if not any("not a string literal" in why for _, why in unres):
        failures.append("coverage: ev(somePath) should be reported as unresolvable")

    # Non-vacuity: the two planted dead paths must be detected, and the two real
    # ones must not be.
    dead = {c.raw for c in cites if not os.path.exists(os.path.join(root, c.path))}
    if dead != {"no/such/file.rs", "also/missing"}:
        failures.append(f"detection: expected the two planted dead paths, got {sorted(dead)}")

    if failures:
        print("SELF-TEST FAILED", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return EXIT_VIOLATION

    print("self-test OK — parser extracts, strips line suffixes, ignores href chips,")
    print("reports unresolvable arguments, and detects planted dead paths.")
    return EXIT_OK


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true", help="prove the detector is non-vacuous, then exit")
    ap.add_argument("--list", action="store_true", help="print every resolved citation")
    ap.add_argument("--root", default=REPO_ROOT, help="repository root (default: this script's repo)")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test(args.root)

    citations, dead, unresolvable = check(args.root)

    if args.list:
        for c in sorted(citations, key=lambda c: (c.page, c.line)):
            print(f"{c.page}:{c.line}\t{c.raw}")

    pages = len({c.page for c in citations})
    paths = len({c.path for c in citations})
    print(f"website citations: {len(citations)} targets, {paths} distinct paths, across {pages} pages")

    if unresolvable:
        print(f"\n{len(unresolvable)} citation argument(s) could not be resolved statically (not gated):")
        for page, line, why in unresolvable:
            print(f"  {page}:{line}  {why}")

    if dead:
        print(f"\nFAIL — {len(dead)} cited path(s) do not exist; these are 404s on the live site:\n")
        for c in sorted(dead, key=lambda c: (c.page, c.line)):
            print(f"  {c.page}:{c.line}")
            print(f"      cites: {c.raw}")
        print(
            "\nRepoint each citation at the path that now holds the evidence, then\n"
            "regenerate the site:  node website/_src/generate.mjs\n"
            "Do not delete a citation to silence this gate — a claim on the site\n"
            "without evidence behind it is the thing the chips exist to prevent."
        )
        return EXIT_VIOLATION

    print("OK — every cited path resolves.")
    return EXIT_OK


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover - defensive
        print(f"internal error: {exc}", file=sys.stderr)
        sys.exit(EXIT_USAGE)
