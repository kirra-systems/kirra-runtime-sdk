#!/usr/bin/env python3
"""Every PR must be able to run CI — the "no silent blind spot" ratchet.

`.github/workflows/ci.yml` used to say:

    on:
      pull_request:
        branches: [main]

On `pull_request`, `branches:` filters by the PR's **base**, not its head. A PR
stacked on another branch therefore matched nothing and the workflow never ran.

What made that dangerous is the shape of the failure, not its size. There was no
error, no skipped-run entry, no "0 checks" warning — the PR looked like an
ordinary open PR with nothing behind it. **An absent check reads as "nothing to
report" when it actually means "nothing was tested."** #1307 and #1310 were both
merged into that blind spot before anyone noticed, and the repository had run
100+ consecutive PRs without ever stacking, so the gap had never once fired.
Zero incidence right up until it mattered.

Retargeting the PR to `main` did not repair it either: that fires
`pull_request.edited`, which is not in the default type set, so the PR stayed
untested until it was force-pushed or closed and reopened.

This gate makes the condition structural instead of remembered. For every
workflow that reacts to `pull_request`:

  1. NO `branches:` / `branches-ignore:` filter — the base must not decide
     whether a PR gets tested.
  2. If `types:` is narrowed, it must still include `opened`, `synchronize` and
     `reopened`. Dropping `synchronize` is the same bug wearing a different hat:
     the first push is tested and every later one is not.

Scope, honestly: this checks that a workflow is ELIGIBLE to run on a PR. It
cannot make a required check exist, and it does not stop a PR merging with zero
checks — that is branch protection's job, and it is configured outside this
repository. Passing here means "CI is able to run", not "CI ran".

Usage:
    python3 ci/check_workflow_pr_coverage.py
    python3 ci/check_workflow_pr_coverage.py --self-test
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent
WORKFLOW_DIR = ".github/workflows"

# Types that must remain live if `types:` is narrowed at all.
REQUIRED_TYPES = {"opened", "synchronize", "reopened"}

# Reviewed exceptions: workflow filename -> why a base filter is legitimate.
# Empty on purpose. A workflow that genuinely must be base-scoped can be added
# here, but it needs a stated reason — an unexplained entry is how the blind
# spot returns.
ALLOWED_BASE_FILTER: dict[str, str] = {}

EXIT_OK = 0
EXIT_VIOLATION = 1


def load_on_block(doc: dict) -> dict | None:
    """Return the `on:` mapping.

    YAML 1.1 parses a bare `on:` key as the BOOLEAN True, so a naive
    `doc.get("on")` silently finds nothing and this gate would pass vacuously on
    every workflow in the repository.
    """
    for key in ("on", True):
        if key in doc:
            value = doc[key]
            return value if isinstance(value, dict) else {}
    return None


def check_workflow(name: str, doc: dict) -> list[str]:
    on = load_on_block(doc)
    if on is None:
        return [f"{name}: no `on:` block — cannot tell what triggers it"]
    if "pull_request" not in on:
        return []  # push/schedule-only workflows are not in scope

    pr = on["pull_request"] or {}
    if not isinstance(pr, dict):
        return []  # `pull_request:` with no config = every PR, which is the goal

    problems: list[str] = []
    for key in ("branches", "branches-ignore"):
        if key in pr and name not in ALLOWED_BASE_FILTER:
            problems.append(
                f"{name}: `pull_request.{key}: {pr[key]}` filters by the PR's BASE, so a PR "
                f"stacked on another branch gets NO CI at all — silently, with zero checks "
                f"rather than a visible failure. Remove the filter."
            )

    types = pr.get("types")
    if types is not None:
        missing = REQUIRED_TYPES - set(types)
        if missing:
            problems.append(
                f"{name}: `pull_request.types` omits {sorted(missing)}. Without `synchronize` "
                f"only the first push is tested and every later one is not — the same blind "
                f"spot in a different place."
            )
    return problems


def check(root: Path) -> tuple[list[str], int]:
    wf_dir = root / WORKFLOW_DIR
    if not wf_dir.is_dir():
        return [f"{WORKFLOW_DIR} not found under {root}"], 0

    problems: list[str] = []
    pr_workflows = 0
    for path in sorted(wf_dir.iterdir()):
        if path.suffix not in {".yml", ".yaml"}:
            continue
        try:
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            problems.append(f"{path.name}: does not parse — NOT checked ({exc})")
            continue
        if not isinstance(doc, dict):
            continue
        on = load_on_block(doc)
        if on and "pull_request" in on:
            pr_workflows += 1
        problems.extend(check_workflow(path.name, doc))
    return problems, pr_workflows


# ---------------------------------------------------------------------------
# Self-test — a gate that cannot be made to fail proves nothing.
# ---------------------------------------------------------------------------

_GOOD = "name: x\non:\n  pull_request:\n  push:\n    branches: [main]\n"
_BASE_FILTERED = "name: x\non:\n  pull_request:\n    branches: [main]\n"
_IGNORE_FILTERED = "name: x\non:\n  pull_request:\n    branches-ignore: [wip/**]\n"
_NARROW_TYPES = "name: x\non:\n  pull_request:\n    types: [opened]\n"
_FULL_TYPES = "name: x\non:\n  pull_request:\n    types: [opened, synchronize, reopened, ready_for_review]\n"
_PUSH_ONLY = "name: x\non:\n  push:\n    branches: [main]\n"


def self_test() -> int:
    failures: list[str] = []

    def problems_for(src: str) -> list[str]:
        return check_workflow("fixture.yml", yaml.safe_load(src))

    if problems_for(_GOOD):
        failures.append("an unfiltered pull_request workflow should pass")
    if not problems_for(_BASE_FILTERED):
        failures.append("`branches: [main]` on pull_request must be detected")
    if not problems_for(_IGNORE_FILTERED):
        failures.append("`branches-ignore` on pull_request must be detected")
    if not problems_for(_NARROW_TYPES):
        failures.append("`types: [opened]` (no synchronize) must be detected")
    if problems_for(_FULL_TYPES):
        failures.append("types including opened/synchronize/reopened should pass")
    if problems_for(_PUSH_ONLY):
        failures.append("a push-only workflow is out of scope and should pass")

    # The YAML 1.1 `on:` -> True quirk. If this regressed, every check above
    # would pass vacuously against real workflow files.
    parsed = yaml.safe_load(_GOOD)
    if "on" in parsed:
        failures.append(
            "expected PyYAML to parse bare `on:` as the boolean True — "
            "the load_on_block workaround may now be masking a real lookup"
        )
    if load_on_block(parsed) is None or "pull_request" not in load_on_block(parsed):
        failures.append("load_on_block must find the trigger block despite the True-key quirk")

    if failures:
        print("SELF-TEST FAILED", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return EXIT_VIOLATION

    print("self-test OK — detects base filters, branches-ignore, and a types list")
    print("missing `synchronize`; passes unfiltered and push-only workflows; and")
    print("resolves the YAML `on:` -> True quirk that would make it vacuous.")
    return EXIT_OK


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--self-test", action="store_true", help="prove the detector is non-vacuous")
    ap.add_argument("--root", default=str(REPO), help="repository root")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()

    problems, pr_workflows = check(Path(args.root))
    print(f"workflows reacting to pull_request: {pr_workflows}")

    if problems:
        print(f"\nFAIL — {len(problems)} workflow trigger problem(s):\n")
        for p in problems:
            print(f"  {p}")
        print(
            "\nA PR that cannot run CI shows zero checks, which looks like "
            "'nothing to report'\nrather than 'nothing was tested'. That is the "
            "failure this gate exists to prevent."
        )
        return EXIT_VIOLATION

    print("OK — every pull_request workflow can run on a PR against any base.")
    print("(This proves CI is ABLE to run, not that it ran. Requiring a check "
          "before merge\nis branch protection's job, configured outside this repo.)")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
