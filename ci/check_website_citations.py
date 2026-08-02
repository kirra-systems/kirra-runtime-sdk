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

BOTH SIDES ARE SCANNED, because they can disagree. `website/_src/pages/*.mjs`
is the authored source, but the Pages deploy publishes `website/*.html` as-is
with no build step — so an author who fixes `_src` and forgets to regenerate
would leave the DEPLOYED site still serving the dead link while a source-only
gate went green. Scanning the HTML also reaches repo links written by hand
outside the `ev()` helper (the shared template's footer chips), which the source
scan cannot see at all.

Three assertions, therefore:

  1. every path cited in `_src` exists;
  2. every repo deep-link in the generated HTML exists;
  3. every path cited in `_src` appears in the generated HTML — i.e. the HTML is
     not stale with respect to the source.

(3) is deliberately one-directional: HTML legitimately contains repo links that
no page `ev()` call produces, so the reverse containment would false-positive.

ANCHORED CITATIONS (Tier 2). A bare `file.rs:206` records a COORDINATE, not a
claim -- the repo never wrote down what line 206 was supposed to contain, so
nothing could tell when it stopped containing it. Line numbers drifted silently
and badly: the homepage's "NaN or Inf anywhere is an immediate deny, proved for
all 2**64 bit patterns" pointed at `overhang_rear_m: 0.9,`, a fixture field.

A line citation therefore carries the text that must be AT that line:

    "crates/kirra-trajectory/src/validation.rs:235#pub fn validate_trajectory_slow("

which turns three previously-indistinguishable situations into three outcomes:

  * anchor is at the cited line          -> pass
  * anchor is elsewhere in the file      -> MOVED; `--fix` rewrites the number
  * anchor is gone from the file         -> MISSING; a human must repoint it,
                                            because the claim may no longer hold
  * anchor matches several lines         -> AMBIGUOUS; unfixable by construction,
                                            lengthen it until unique

Only the second is mechanical, and that is the common case -- which is what keeps
this sustainable on a file like ci.yml (46 commits in six months, 13 citations).

The anchor is authoring-time metadata: `ev()` strips it, so it never reaches the
rendered URL.

SCOPE — this gate checks that citations RESOLVE: the path exists, and the line
holds what the citation says it holds. Deliberately NOT in scope: whether the
cited code actually supports the claim in the surrounding prose. That is a human
review judgement and always will be. A green run means every evidence link
resolves to the named thing, NOT that every evidence link is apposite.

Usage:
    python3 ci/check_website_citations.py            # gate (exit 1 on a violation)
    python3 ci/check_website_citations.py --fix      # repoint drifted line numbers
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
SITE_DIR = "website"

# `ev(` / `evRow(` as a standalone identifier. The negative lookbehind keeps
# this from firing inside a longer name (`prev(`, `retrieve(`, `obj.ev(`).
CALL_RE = re.compile(r"(?<![A-Za-z0-9_$.])(ev|evRow)\s*\(")

# A repo deep-link in the generated HTML. `#` is excluded from the path class so
# an `#L92` line fragment can never be captured as part of the path; the fragment
# is captured separately because a stale LINE NUMBER in the deployed HTML is just
# as wrong as a stale path, and only comparing paths would miss it.
BLOB_RE = re.compile(r'href="https://github\.com/[^/"]+/[^/"]+/blob/[^/"]+/([^"#]+)(?:#L(\d+))?"')

# `generate.mjs` writes each page to `${mod.meta.slug}.html`, so the slug is what
# maps a page source to the HTML a visitor actually receives.
SLUG_RE = re.compile(r"""\bslug\s*:\s*["']([A-Za-z0-9_-]+)["']""")

EXIT_OK = 0
EXIT_VIOLATION = 1
EXIT_USAGE = 2


@dataclass(frozen=True)
class Citation:
    """One repo path cited by one evidence chip.

    A citation is authored as `path[:line[#anchor]]`. The anchor is the Tier 2
    addition: a literal substring that must appear on the cited line. Without it
    a line number is a bare coordinate that nothing can check -- see the module
    docstring.
    """

    page: str  # page file, repo-relative
    line: int  # line within the page source
    raw: str  # the citation as authored, e.g. "src/wcet_gate.rs:119#GOVERNOR_..."
    path: str  # the repo path alone, ":line" and "#anchor" stripped
    target_line: int | None = None  # the cited line within `path`
    anchor: str | None = None  # text required at that line


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
            path, target_line, anchor = parse_citation(raw)
            citations.append(
                Citation(page=page, line=line, raw=raw, path=path, target_line=target_line, anchor=anchor)
            )

    return citations, unresolvable


def parse_citation(raw: str) -> tuple[str, int | None, str | None]:
    """Split `path[:line[#anchor]]` into its parts.

    The anchor is taken as everything after the FIRST '#', so an anchor may
    itself contain '#' (a YAML comment, a Rust attribute). A '#' before the
    ':line' is treated as part of the path -- no repo path in this tree has one,
    and reading it as an anchor would silently drop a real directory name.
    """
    spec, sep, anchor = raw.partition("#")
    head, _, tail = spec.rpartition(":")
    if head and tail.isdigit():
        return head, int(tail), (anchor if sep else None)
    # No line number: an anchor would have nothing to anchor TO, so keep the
    # whole string as the path rather than inventing a target.
    return raw, None, None


def collect(root: str) -> tuple[list[Citation], list[tuple[str, int, str]], dict[str, str | None]]:
    """Scan every page source. Also returns each page's output slug."""
    pages_dir = os.path.join(root, PAGES_DIR)
    if not os.path.isdir(pages_dir):
        raise SystemExit(f"{PAGES_DIR} not found under {root}")

    citations: list[Citation] = []
    unresolvable: list[tuple[str, int, str]] = []
    slugs: dict[str, str | None] = {}
    for name in sorted(os.listdir(pages_dir)):
        if not name.endswith(".mjs"):
            continue
        rel = os.path.join(PAGES_DIR, name)
        with open(os.path.join(pages_dir, name), encoding="utf-8") as fh:
            src = fh.read()
        page_cites, page_unres = scan_source(rel, src)
        citations.extend(page_cites)
        unresolvable.extend((rel, ln, why) for ln, why in page_unres)
        m = SLUG_RE.search(src)
        slugs[rel] = m.group(1) if m else None
    return citations, unresolvable, slugs


def scan_html(page: str, src: str) -> list[Citation]:
    """Extract repo deep-links from one generated HTML page."""
    links: list[Citation] = []
    for m in BLOB_RE.finditer(src):
        path = m.group(1)
        target = int(m.group(2)) if m.group(2) else None
        line = src.count("\n", 0, m.start()) + 1
        links.append(Citation(page=page, line=line, raw=path, path=path, target_line=target))
    return links


def collect_html(root: str) -> list[Citation]:
    site_dir = os.path.join(root, SITE_DIR)
    if not os.path.isdir(site_dir):
        raise SystemExit(f"{SITE_DIR} not found under {root}")

    links: list[Citation] = []
    for name in sorted(os.listdir(site_dir)):
        if not name.endswith(".html"):
            continue
        rel = os.path.join(SITE_DIR, name)
        with open(os.path.join(site_dir, name), encoding="utf-8") as fh:
            links.extend(scan_html(rel, fh.read()))
    return links


@dataclass(frozen=True)
class AnchorIssue:
    """An anchored citation whose anchor is not where the citation says."""

    cite: Citation
    kind: str  # "moved" | "missing" | "ambiguous" | "unanchored"
    found_line: int | None = None  # where the anchor actually is (kind="moved")
    occurrences: int = 0  # how many times it appears (kind="ambiguous")

    @property
    def fixable(self) -> bool:
        return self.kind == "moved" and self.found_line is not None


def classify_anchor(lines: list[str], target_line: int, anchor: str) -> tuple[str, int | None, int]:
    """Pure core of the anchor check: (kind, found_line, occurrences).

    Split out from file IO so the self-test can exercise every branch against
    in-memory content. Testing it against this script's own source instead was
    self-referential -- the probe strings appeared in the test literals, turning
    a "unique" anchor into an ambiguous one.
    """
    hits = [i + 1 for i, text in enumerate(lines) if anchor in text]
    if not hits:
        return "missing", None, 0
    if len(hits) > 1:
        # Ambiguous even when the cited line is currently ONE of the matches.
        # Uniqueness is checked at authoring time deliberately: a non-unique
        # anchor is unfixable-in-waiting -- it looks fine until the file shifts,
        # and then nothing can say which occurrence was meant. Failing now, while
        # the author still remembers what they meant, beats failing later.
        return "ambiguous", None, len(hits)
    if target_line == hits[0]:
        return "ok", target_line, 1
    return "moved", hits[0], 1


def check_anchor(root: str, cite: Citation) -> AnchorIssue | None:
    """Verify one citation's anchor. Returns None when it is exactly right."""
    if cite.target_line is None:
        return None
    if not cite.anchor:
        # A line number with nothing to check it against is the Tier 1 status quo
        # this gate exists to end. Every line citation in the tree is anchored, so
        # this fails -- keeping the count at zero and stopping a new bare
        # coordinate from being introduced.
        return AnchorIssue(cite=cite, kind="unanchored")

    try:
        with open(os.path.join(root, cite.path), encoding="utf-8", errors="replace") as fh:
            lines = fh.read().split("\n")
    except OSError:
        return None  # a dead path is already reported by the existence check

    kind, found, count = classify_anchor(lines, cite.target_line, cite.anchor)
    if kind == "ok":
        return None
    return AnchorIssue(cite=cite, kind=kind, found_line=found, occurrences=count)


@dataclass
class Result:
    citations: list[Citation]  # from website/_src/pages/*.mjs
    html_links: list[Citation]  # from website/*.html
    dead_src: list[Citation]
    dead_html: list[Citation]
    stale: list[tuple[str, str, str]]  # (page source, output html, path)
    unresolvable: list[tuple[str, int, str]]
    slugless: list[str]  # page sources whose output slug could not be read
    anchors: list[AnchorIssue]

    @property
    def failed(self) -> bool:
        return bool(self.dead_src or self.dead_html or self.stale or self.slugless or self.anchors)


def check(root: str) -> Result:
    citations, unresolvable, slugs = collect(root)
    html_links = collect_html(root)

    def exists(p: str) -> bool:
        return os.path.exists(os.path.join(root, p))

    # Per-page HTML link sets. Comparing globally would let a stale page slip
    # through whenever its new citation happens to appear on some OTHER page --
    # the staleness is per-document, so the comparison has to be too.
    # Compared as (path, line), not path alone: after `--fix` repoints a drifted
    # line number, the source is right while the committed HTML still deep-links
    # to the OLD line. Comparing paths only would call that in sync.
    by_html: dict[str, set[tuple[str, int | None]]] = {}
    for link in html_links:
        by_html.setdefault(link.page, set()).add((link.path, link.target_line))

    stale: list[tuple[str, str, str]] = []
    slugless: list[str] = []
    for page, slug in sorted(slugs.items()):
        if slug is None:
            slugless.append(page)
            continue
        out = os.path.join(SITE_DIR, f"{slug}.html")
        rendered = by_html.get(out)
        cited = {(c.path, c.target_line) for c in citations if c.page == page}

        def label(item: tuple[str, int | None]) -> str:
            path, line = item
            return f"{path}:{line}" if line else path

        if rendered is None:
            # The page source exists but its HTML does not; every citation on it
            # is unreachable to a visitor.
            stale.extend((page, out, label(i)) for i in sorted(cited, key=label))
            continue
        stale.extend((page, out, label(i)) for i in sorted(cited - rendered, key=label))

    return Result(
        citations=citations,
        html_links=html_links,
        dead_src=[c for c in citations if not exists(c.path)],
        # Dedupe by path: one moved file otherwise reports once per page that
        # links it, burying the distinct failures.
        dead_html=list({link.path: link for link in html_links if not exists(link.path)}.values()),
        stale=stale,
        unresolvable=unresolvable,
        slugless=slugless,
        anchors=[i for i in (check_anchor(root, c) for c in citations) if i is not None],
    )


def apply_fixes(root: str, issues: list[AnchorIssue]) -> tuple[int, list[AnchorIssue]]:
    """Rewrite drifted line numbers in place. Returns (fixed count, unfixable)."""
    fixable = [i for i in issues if i.fixable]
    unfixable = [i for i in issues if not i.fixable]

    # Group by page so each file is read and written once.
    by_page: dict[str, list[AnchorIssue]] = {}
    for issue in fixable:
        by_page.setdefault(issue.cite.page, []).append(issue)

    fixed = 0
    for page, page_issues in by_page.items():
        full = os.path.join(root, page)
        with open(full, encoding="utf-8") as fh:
            src = fh.read()
        for issue in page_issues:
            cite = issue.cite
            new_raw = f"{cite.path}:{issue.found_line}"
            if cite.anchor:
                new_raw += f"#{cite.anchor}"
            # Replace the quoted literal so a bare substring match cannot clip a
            # longer path that happens to share this prefix.
            old, new = f'"{cite.raw}"', f'"{new_raw}"'
            if old in src:
                src = src.replace(old, new)
                fixed += 1
            else:
                unfixable.append(issue)
        with open(full, "w", encoding="utf-8") as fh:
            fh.write(src)

    return fixed, unfixable


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


_FIXTURE_HTML = """
<p class="evidence-row">
  <a class="evidence" href="https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/README.md">README.md</a>
  <a class="evidence" href="https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/Cargo.toml#L12">Cargo.toml:12</a>
  <a class="evidence" href="https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/gone/from/tree.rs">gone</a>
  <a class="evidence" href="certification.html">a same-site link, not a citation</a>
  <a href="https://github.com/kirra-systems/kirra-runtime-sdk/issues/1">an issue, not a blob</a>
</p>
"""


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

    # --- generated-HTML side ---
    html = scan_html("fixture.html", _FIXTURE_HTML)
    html_got = sorted(link.path for link in html)
    html_want = sorted(["README.md", "Cargo.toml", "gone/from/tree.rs"])
    if html_got != html_want:
        failures.append(f"html extraction: expected {html_want}, got {html_got}")

    # An `#L12` fragment must not bleed into the path, and a same-site link or a
    # non-blob GitHub URL must not be mistaken for a citation.
    if any("#" in link.path for link in html):
        failures.append("html: a #L line fragment leaked into the extracted path")
    if any("certification.html" in link.path or "issues" in link.path for link in html):
        failures.append("html false positive: a non-blob href was treated as a citation")

    html_dead = {link.path for link in html if not os.path.exists(os.path.join(root, link.path))}
    if html_dead != {"gone/from/tree.rs"}:
        failures.append(f"html detection: expected the planted dead link, got {sorted(html_dead)}")

    # --- staleness (this page's HTML does not carry what this page cites) ---
    html_paths = {link.path for link in html}
    stale = sorted({c.path for c in cites} - html_paths)
    if "docs/adr" not in stale:
        failures.append("staleness: a source citation missing from the HTML should be reported stale")
    if "README.md" in stale:
        failures.append("staleness false positive: a path present in the HTML was reported stale")

    # The slug is what maps a page source to the HTML a visitor receives; a page
    # that does not declare one must be reported, not silently skipped.
    if SLUG_RE.search('export const meta = { slug: "runtime", title: "x" };') is None:
        failures.append("slug: a literal slug declaration was not recognised")
    if SLUG_RE.search("export const meta = { title: 'x' };") is not None:
        failures.append("slug: a page with no slug should yield no match")

    # --- anchored-citation parsing (Tier 2) ---
    for raw, want in [
        ("a/b.rs", ("a/b.rs", None, None)),
        ("a/b.rs:12", ("a/b.rs", 12, None)),
        ("a/b.rs:12#pub fn x(", ("a/b.rs", 12, "pub fn x(")),
        # An anchor may itself contain '#' (a YAML comment, a Rust attribute):
        # only the FIRST '#' separates.
        ("ci.yml:5#- name: x # trailing", ("ci.yml", 5, "- name: x # trailing")),
        # No line number => nothing to anchor to; keep the whole string as path
        # rather than inventing a target.
        ("docs/a#b", ("docs/a#b", None, None)),
    ]:
        got = parse_citation(raw)
        if got != want:
            failures.append(f"parse_citation({raw!r}): expected {want}, got {got}")

    # --- anchor classification, over in-memory content ---
    #  1 fn alpha(
    #  2 body
    #  3 duplicated
    #  4 fn beta(
    #  5 duplicated
    probe_lines = ["fn alpha(", "body", "duplicated", "fn beta(", "duplicated"]
    for target, anchor, want in [
        (1, "fn alpha(", ("ok", 1)),  # exactly right
        (4, "fn alpha(", ("moved", 1)),  # drifted: unique elsewhere -> fixable
        (2, "fn gamma(", ("missing", None)),  # the evidence itself is gone
        (1, "duplicated", ("ambiguous", None)),  # cannot know which was meant
    ]:
        kind, found, _n = classify_anchor(probe_lines, target, anchor)
        if (kind, found) != want:
            failures.append(f"classify_anchor({target}, {anchor!r}): expected {want}, got {(kind, found)}")

    # Only "moved" may be repaired automatically; the other two need a human.
    for kind, should_fix in [("moved", True), ("missing", False), ("ambiguous", False), ("unanchored", False)]:
        got = AnchorIssue(
            cite=Citation("f.mjs", 1, "a:1#x", "a", 1, "x"),
            kind=kind,
            found_line=2 if kind == "moved" else None,
        ).fixable
        if got != should_fix:
            failures.append(f"fixable: kind={kind} should be {should_fix}, got {got}")

    # A bare line citation must be flagged, not silently accepted.
    bare = check_anchor(root, Citation("f.mjs", 1, "README.md:1", "README.md", 1, None))
    if not bare or bare.kind != "unanchored":
        failures.append(f"anchor: a bare line citation should be kind=unanchored, got {bare}")

    if failures:
        print("SELF-TEST FAILED", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return EXIT_VIOLATION

    print("self-test OK — source parser extracts, strips line suffixes, ignores href")
    print("chips, reports unresolvable arguments, and detects planted dead paths;")
    print("HTML scanner extracts blob links without their #L fragment, ignores")
    print("same-site and non-blob URLs, detects a planted dead link, and reports a")
    print("source citation absent from the HTML as stale; anchor parsing splits on")
    print("the first '#' only, and anchor checking separates correct from moved")
    print("(fixable) / missing / ambiguous / unanchored (all not fixable).")
    return EXIT_OK


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true", help="prove the detector is non-vacuous, then exit")
    ap.add_argument("--list", action="store_true", help="print every resolved citation")
    ap.add_argument("--fix", action="store_true", help="rewrite drifted line numbers whose anchor moved")
    ap.add_argument("--root", default=REPO_ROOT, help="repository root (default: this script's repo)")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test(args.root)

    r = check(args.root)

    if args.fix:
        fixed, left = apply_fixes(args.root, r.anchors)
        print(f"--fix: repointed {fixed} drifted citation(s).")
        if fixed:
            print("Regenerate the site so the HTML carries the new line numbers:")
            print("  node website/_src/generate.mjs")
        if left:
            print(f"\n{len(left)} issue(s) need a human decision:")
            for issue in left:
                print(f"  {issue.cite.page}:{issue.cite.line}  {issue.cite.raw}  [{issue.kind}]")
            return EXIT_VIOLATION
        return EXIT_OK

    if args.list:
        for c in sorted(r.citations, key=lambda c: (c.page, c.line)):
            print(f"{c.page}:{c.line}\t{c.raw}")

    pages = len({c.page for c in r.citations})
    paths = len({c.path for c in r.citations})
    html_pages = len({link.page for link in r.html_links})
    html_paths = len({link.path for link in r.html_links})
    print(f"source citations: {len(r.citations)} targets, {paths} distinct paths, across {pages} pages")
    print(f"generated HTML:   {len(r.html_links)} repo links, {html_paths} distinct paths, across {html_pages} pages")

    if r.unresolvable:
        print(f"\n{len(r.unresolvable)} citation argument(s) could not be resolved statically (NOT checked):")
        for page, line, why in r.unresolvable:
            print(f"  {page}:{line}  {why}")

    if r.dead_src:
        print(f"\nFAIL — {len(r.dead_src)} cited path(s) do not exist; these are 404s on the live site:\n")
        for c in sorted(r.dead_src, key=lambda c: (c.page, c.line)):
            print(f"  {c.page}:{c.line}")
            print(f"      cites: {c.raw}")
        print(
            "\nRepoint each citation at the path that now holds the evidence, then\n"
            "regenerate the site:  node website/_src/generate.mjs\n"
            "Do not delete a citation to silence this gate — a claim on the site\n"
            "without evidence behind it is the thing the chips exist to prevent."
        )

    if r.dead_html:
        print(f"\nFAIL — {len(r.dead_html)} repo link(s) in the DEPLOYED HTML do not exist:\n")
        for link in sorted(r.dead_html, key=lambda c: c.path):
            print(f"  {link.page}:{link.line}")
            print(f"      links: {link.path}")
        print(
            "\nThese ship to visitors as-is. If the link is written by hand in\n"
            "website/_src/template.mjs rather than via ev(), fix it there."
        )

    if r.slugless:
        print(f"\nFAIL — {len(r.slugless)} page source(s) with no readable output slug:\n")
        for page in r.slugless:
            print(f"  {page}")
        print(
            "\nWithout the slug this gate cannot tell which HTML file the page\n"
            "renders to, so it cannot check that page's citations reached the\n"
            "deployed site. Declare `slug: \"<name>\"` in the page's meta."
        )

    if r.stale:
        pages = len({page for page, _out, _p in r.stale})
        print(f"\nFAIL — {len(r.stale)} citation(s) on {pages} page(s) never reached the deployed HTML:\n")
        for page, out, path in r.stale:
            print(f"  {page}  ->  {out}")
            print(f"      cites: {path}")
        print(
            "\nThe committed HTML is stale with respect to the page sources, and the\n"
            "Pages deploy publishes website/ as-is with no build step — so the fix\n"
            "in _src is NOT what visitors get. Regenerate and commit the result:\n"
            "  node website/_src/generate.mjs"
        )

    if r.anchors:
        moved = [a for a in r.anchors if a.kind == "moved"]
        missing = [a for a in r.anchors if a.kind == "missing"]
        ambiguous = [a for a in r.anchors if a.kind == "ambiguous"]
        unanchored = [a for a in r.anchors if a.kind == "unanchored"]

        if moved:
            print(f"\nFAIL — {len(moved)} citation(s) whose anchor has MOVED:\n")
            for a in moved:
                print(f"  {a.cite.page}:{a.cite.line}")
                print(f"      cites: {a.cite.path}:{a.cite.target_line}  (anchor now at line {a.found_line})")
                print(f"      anchor: {a.cite.anchor}")
            print("\nThis is mechanical — the evidence did not move, only its line number:")
            print("  python3 ci/check_website_citations.py --fix && node website/_src/generate.mjs")

        if missing:
            print(f"\nFAIL — {len(missing)} citation(s) whose anchor is GONE from the file:\n")
            for a in missing:
                print(f"  {a.cite.page}:{a.cite.line}")
                print(f"      cites:  {a.cite.path}:{a.cite.target_line}")
                print(f"      anchor: {a.cite.anchor}")
            print(
                "\nNot mechanical. The thing the site points at was renamed or deleted, so\n"
                "the claim beside the chip may no longer be backed by anything. Re-read the\n"
                "claim, find what now supports it, and repoint -- do not just delete the\n"
                "citation to get to green."
            )

        if ambiguous:
            print(f"\nFAIL — {len(ambiguous)} anchor(s) that are not unique in their file:\n")
            for a in ambiguous:
                print(f"  {a.cite.page}:{a.cite.line}")
                print(f"      cites:  {a.cite.path}:{a.cite.target_line}")
                print(f"      anchor: {a.cite.anchor}  ({a.occurrences} occurrences)")
            print(
                "\nAn anchor that matches several lines cannot be repaired automatically --\n"
                "nothing says which one was meant. Lengthen it until it is unique."
            )

        if unanchored:
            print(f"\nFAIL — {len(unanchored)} line citation(s) with no anchor:\n")
            for a in unanchored:
                print(f"  {a.cite.page}:{a.cite.line}  cites {a.cite.raw}")
            print(
                "\nA bare `path:line` records a coordinate, not a claim: when the line moves\n"
                "nothing can tell. Add the text that must be on that line:\n"
                "  \"crates/kirra-trajectory/src/validation.rs:235#pub fn validate_trajectory_slow(\""
            )

    if r.failed:
        return EXIT_VIOLATION

    if r.unresolvable:
        # Never claim more than was actually checked.
        print(
            f"\nOK — every statically-resolvable path resolves, but {len(r.unresolvable)} citation"
            "\nargument(s) above were not checked. This is NOT a clean bill of health."
        )
        return EXIT_OK

    print("\nOK — every cited path resolves, in the sources and in the deployed HTML.")
    return EXIT_OK


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover - defensive
        print(f"internal error: {exc}", file=sys.stderr)
        sys.exit(EXIT_USAGE)
