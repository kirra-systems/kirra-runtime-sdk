#!/usr/bin/env python3
"""Fail CI on quality-guardrail regressions for high-risk runtime modules.

This is a *ratchet*: it pins per-file metrics to a baseline and fails only when
a metric grows past its recorded ceiling. Decreases are always allowed (and are
the point — they let you tighten the baseline as the monolith is decomposed).

Three guardrails, all read from ``ci/quality_guardrails_baseline.json``:

* ``max_lines``      — per-file line-count ceiling (keeps the entry-point
                       monolith from regrowing after it is split up).
* ``panic_budget``   — per-file ceiling on ``unwrap(`` / ``expect(`` /
                       ``panic!(`` occurrences in *code* (comments and escaped
                       double-quoted string-literal contents are stripped before
                       counting, so prose in those never trips the gate; raw
                       strings are intentionally NOT stripped — see
                       ``strip_rust_noise`` — so editing a raw-string body that
                       contains one of these tokens can still move the count).
                       This is an
                       interim ratchet; the strategic replacement is clippy's
                       ``unwrap_used`` / ``expect_used`` / ``panic`` lints with a
                       justified ``#[allow]`` at each sanctioned fail-closed
                       site (e.g. the deliberate ``.lock().unwrap()`` calls that
                       rely on ``panic = "abort"``). Tracked as a follow-up.
* ``ownership_scope``— files that must never reintroduce
                       ``State<Arc<AppState>>`` (handlers take
                       ``State<Arc<ServiceState>>`` — CLAUDE.md invariant #11).

Robustness notes (hardening over the original draft):
* A baseline path that no longer exists on disk is reported as a loud WARNING
  and skipped, never an unhandled ``FileNotFoundError``. Renaming/splitting a
  tracked file (exactly what the architecture-review refactors do) must not
  crash the gate; the warning tells the maintainer to update the baseline.
* All diagnostics are emitted with repo-relative paths for stable output.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
BASELINE_PATH = REPO / "ci" / "quality_guardrails_baseline.json"

PANIC_PATTERN = re.compile(r"unwrap\(|expect\(|panic!\(")
OWNERSHIP_FORBIDDEN_PATTERNS = {
    "State<Arc<AppState>> in handler runtime (use ServiceState — CLAUDE.md #11)": re.compile(
        r"State<Arc<AppState>>"
    ),
}


def strip_rust_noise(text: str) -> str:
    """Return ``text`` with Rust line/block comments and double-quoted string
    literal *contents* removed, so textual pattern counting sees only code.

    This is an approximate lexer, not a full Rust parser: it handles ``//`` line
    comments, ``/* ... */`` block comments (non-nested — the dominant case), and
    ``"..."`` strings with backslash escapes. Raw strings (e.g. ``r#"..."#``) are
    deliberately left as-is to avoid the added complexity of matching variable
    hash-delimiters. Note raw strings ARE common in the guarded files (HTTP/JSON
    response bodies), so a raw-string body that happens to contain ``unwrap(`` /
    ``expect(`` / ``panic!(`` would be counted. This is conservative (fail-safe):
    it can only over-count, never hide a real call — but it does mean editing
    such a raw-string literal can change the counted total even when no
    executable code changed."""
    out: list[str] = []
    i = 0
    n = len(text)
    in_line_comment = False
    in_block_comment = False
    in_string = False
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if in_line_comment:
            if c == "\n":
                in_line_comment = False
                out.append(c)
            i += 1
            continue
        if in_block_comment:
            if c == "*" and nxt == "/":
                in_block_comment = False
                i += 2
                continue
            if c == "\n":
                out.append(c)
            i += 1
            continue
        if in_string:
            if c == "\\":
                i += 2  # skip escaped char
                continue
            if c == '"':
                in_string = False
            i += 1
            continue
        # not in any comment/string
        if c == "/" and nxt == "/":
            in_line_comment = True
            i += 2
            continue
        if c == "/" and nxt == "*":
            in_block_comment = True
            i += 2
            continue
        if c == '"':
            in_string = True
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


def line_count(path: Path) -> int:
    return sum(1 for _ in path.read_text(encoding="utf-8").splitlines())


def panic_count(path: Path) -> int:
    code = strip_rust_noise(path.read_text(encoding="utf-8"))
    return len(PANIC_PATTERN.findall(code))


def main() -> int:
    baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    errors: list[str] = []
    warnings: list[str] = []

    def resolve(rel: str) -> Path | None:
        path = REPO / rel
        if not path.is_file():
            warnings.append(
                f"{rel}: tracked by the guardrail baseline but not found on disk "
                f"(renamed/removed?) — update ci/quality_guardrails_baseline.json"
            )
            return None
        return path

    current_lines: dict[str, int] = {}
    for rel, max_lines in baseline.get("max_lines", {}).items():
        path = resolve(rel)
        if path is None:
            continue
        lines = line_count(path)
        current_lines[rel] = lines
        if lines > max_lines:
            errors.append(f"{rel}: {lines} lines > guardrail max {max_lines}")

    current_panic_counts: dict[str, int] = {}
    for rel, max_count in baseline.get("panic_budget", {}).items():
        path = resolve(rel)
        if path is None:
            continue
        count = panic_count(path)
        current_panic_counts[rel] = count
        if count > max_count:
            errors.append(
                f"{rel}: panic/unwrap/expect count {count} > guardrail max {max_count}"
            )

    for description, pattern in OWNERSHIP_FORBIDDEN_PATTERNS.items():
        for rel in baseline.get("ownership_scope", []):
            path = resolve(rel)
            if path is None:
                continue
            # Strip comments/strings so an explanatory mention of the
            # anti-pattern (e.g. a "use ServiceState, not AppState" note) does
            # not register as a violation — only real code does.
            if pattern.search(strip_rust_noise(path.read_text(encoding="utf-8"))):
                errors.append(f"{rel}: ownership violation ({description})")

    print("=== Quality guardrail metrics ===")
    print(json.dumps({"lines": current_lines, "panic_budget": current_panic_counts}, indent=2))

    if warnings:
        print("\n=== Guardrail warnings (non-fatal) ===")
        for warn in warnings:
            print(f"- {warn}")

    if errors:
        print("\n=== Guardrail violations ===")
        for err in errors:
            print(f"- {err}")
        print(
            "\nFix the regression first. If the growth is intentional and reviewed, "
            "raise the relevant ceiling in ci/quality_guardrails_baseline.json with a "
            "justification in the PR. Decreases never need a baseline change."
        )
        return 1

    print("\nAll quality guardrails satisfied.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
