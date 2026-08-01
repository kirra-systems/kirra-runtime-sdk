#!/usr/bin/env python3
"""Staging-completeness gate for robot/install/install_robot_units.sh.

THE BUG CLASS THIS PINS (found on the R2, 2026-08): the installer stages an
explicit list of robot scripts to /opt/kirra/robot, and the deployed units run
them FROM THERE — no repo checkout on sys.path. A robot script that grows a new
local import (`import rabbit_latency`) works in the repo, passes every host
test, installs cleanly — and then crash-loops AT THE SERVICE with
ModuleNotFoundError, because the imported module was never added to the
installer's list. The old repo-checkout unit masked exactly this for four
modules' worth of features.

The gate: parse the installer's `for f in …` list, compute the TRANSITIVE
closure of local `robot/*.py` imports (AST, top-level absolute imports) over
every staged .py, and require closure ⊆ staged list. Also: every listed file
must exist in robot/ (a stale entry is skipped-with-a-warning at install time —
silent payload loss), and the doctor package dir is handled separately by the
installer so `doctor.*` imports are exempt.

Runs standalone (`python3 robot/install/staging_completeness_test.py`, exit 1
on any failure); also importable under pytest.
"""
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent          # robot/install
ROBOT = HERE.parent                              # robot/
INSTALLER = HERE / "install_robot_units.sh"

# Local packages the installer stages as DIRECTORIES (not via the file list).
DIR_STAGED = {"doctor"}


def staged_list() -> set[str]:
    """The installer's explicit `for f in …; do` payload list."""
    m = re.search(r"for f in (.*?); do", INSTALLER.read_text(), re.S)
    assert m, "installer file list not found (for f in …; do)"
    return set(re.findall(r"[\w.]+\.(?:py|sh)", m.group(1)))


def local_imports(path: Path, local: dict[str, Path]) -> set[str]:
    """Top-level-resolvable local robot/ modules `path` imports. Conditional
    imports count too — a module that is sometimes needed must be staged."""
    try:
        tree = ast.parse(path.read_text())
    except SyntaxError as e:  # a staged script must at least parse
        raise AssertionError(f"{path.name} does not parse: {e}") from e
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            out |= {a.name.split(".")[0] for a in node.names}
        elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            out.add(node.module.split(".")[0])
    return {m for m in out if m in local}


def import_closure(staged: set[str]) -> set[str]:
    """Transitive closure of local imports over the staged .py scripts."""
    local = {p.stem: p for p in ROBOT.glob("*.py")}
    seen: set[str] = set()
    todo = [ROBOT / f for f in staged if f.endswith(".py") and (ROBOT / f).exists()]
    while todo:
        p = todo.pop()
        if p.stem in seen:
            continue
        seen.add(p.stem)
        todo.extend(local[m] for m in local_imports(p, local)
                    if m not in seen and m not in DIR_STAGED)
    return seen


def test_every_listed_payload_exists() -> None:
    """A listed-but-absent file is silently skipped at install time (the
    installer warns and continues) — that is payload loss, so it fails here."""
    missing = sorted(f for f in staged_list() if not (ROBOT / f).is_file())
    assert not missing, f"installer lists files absent from robot/: {missing}"


def test_staged_scripts_import_closure_is_fully_staged() -> None:
    staged = staged_list()
    needed = {f"{mod}.py" for mod in import_closure(staged)}
    missing = sorted(needed - staged)
    assert not missing, (
        "staged robot scripts import local modules the installer does NOT "
        f"stage (ModuleNotFoundError crash-loop at the service): {missing} — "
        "add them to install_robot_units.sh's file list")


def test_closure_is_nonvacuous() -> None:
    """Anchor the gate on the exact regression that motivated it: wake_word →
    rabbit_latency, and the conversational front-end's deeper chains. If these
    ever leave the closure the parser broke, not the dependency."""
    closure = import_closure(staged_list())
    for anchor in ("wake_word", "rabbit_latency", "rabbit_converse",
                   "rabbit_persona", "turn_state"):
        assert anchor in closure, f"closure lost its anchor {anchor!r} — parser broken?"


# --- standalone runner (house pattern) ----------------------------------------

def _run_all() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ok  {name}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL  {name}: {e}")
    print("staging_completeness_test:",
          "ALL OK" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_run_all())
