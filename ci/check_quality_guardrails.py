#!/usr/bin/env python3
"""Fail CI on quality guardrail regressions for high-risk runtime modules."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
BASELINE_PATH = REPO / "ci" / "quality_guardrails_baseline.json"

PANIC_PATTERN = re.compile(r"unwrap\(|expect\(|panic!\(")
OWNERSHIP_FORBIDDEN_PATTERNS = {
    "State<Arc<AppState>> in verifier service runtime": re.compile(r"State<Arc<AppState>>"),
}


def runtime_text_without_cfg_tests(path: Path) -> str:
    """Best-effort stripping of #[cfg(test)] items from single Rust files."""
    lines = path.read_text(encoding="utf-8").splitlines()
    out: list[str] = []
    pending_cfg_test = False
    skipping = False
    skip_depth = 0
    saw_open_brace = False

    for line in lines:
        trimmed = line.strip()

        if not skipping and not pending_cfg_test and trimmed.startswith("#[cfg(test)]"):
            pending_cfg_test = True
            continue

        if pending_cfg_test:
            opens = line.count("{")
            closes = line.count("}")
            if opens > 0:
                skipping = True
                saw_open_brace = True
                skip_depth = opens - closes
                pending_cfg_test = False
                if skip_depth <= 0:
                    skipping = False
                    saw_open_brace = False
                continue
            continue

        if skipping:
            opens = line.count("{")
            closes = line.count("}")
            skip_depth += opens - closes
            if saw_open_brace and skip_depth <= 0:
                skipping = False
                saw_open_brace = False
            continue

        out.append(line)

    return "\n".join(out)


def line_count(path: Path) -> int:
    return sum(1 for _ in path.read_text(encoding="utf-8").splitlines())


def main() -> int:
    baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    errors: list[str] = []

    current_lines: dict[str, int] = {}
    for rel, max_lines in baseline["max_lines"].items():
        path = REPO / rel
        lines = line_count(path)
        current_lines[rel] = lines
        if lines > max_lines:
            errors.append(f"{rel}: {lines} lines > guardrail max {max_lines}")

    current_panic_counts: dict[str, int] = {}
    for rel, max_count in baseline["panic_budget"].items():
        path = REPO / rel
        count = len(PANIC_PATTERN.findall(runtime_text_without_cfg_tests(path)))
        current_panic_counts[rel] = count
        if count > max_count:
            errors.append(
                f"{rel}: panic/unwrap/expect count {count} > guardrail max {max_count}"
            )

    for description, pattern in OWNERSHIP_FORBIDDEN_PATTERNS.items():
        for rel in baseline["ownership_scope"]:
            path = REPO / rel
            if pattern.search(path.read_text(encoding="utf-8")):
                errors.append(f"{path}: ownership violation ({description})")

    print("=== Quality guardrail metrics ===")
    print(json.dumps({"lines": current_lines, "panic_budget": current_panic_counts}, indent=2))

    if errors:
        print("\n=== Guardrail violations ===")
        for err in errors:
            print(f"- {err}")
        print(
            "\nUpdate architecture/refactors first; if intentional, raise guardrails in "
            "ci/quality_guardrails_baseline.json with review justification."
        )
        return 1

    print("All quality guardrails satisfied.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
