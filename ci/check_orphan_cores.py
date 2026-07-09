#!/usr/bin/env python3
"""Wire-or-delete guard for orphan pure cores (DD review §8.2).

The 2026-07-08 due-diligence review found six "pure cores" merged as DONE with
zero non-test consumers (the WP-19 lease, WP-22 EVT, WP-24 model_lineage/ood/
model_targets, WP-18 store traits). All six are wired now (EP-03/04/05/06/10/21)
— this gate is the forcing function that keeps the pattern from recurring:

    a `pub mod` merged at a crate root must gain a NON-TEST consumer,
    or be listed (with a justification) in the baseline.

Mechanics
---------
* Scans every crate-root `lib.rs` (root crate, `crates/*`, `parko/crates/*`)
  for file-backed `pub mod NAME;` declarations.
* A module is CONSUMED when any non-test source file outside the module's own
  subtree references it (`use …NAME…` / `NAME::…`). Pure re-export lines
  (`pub use` / `pub mod` shims) do NOT count — a shelf with a label is still a
  shelf. Verification harnesses (fuzz/, verification/, kirra-loom-models) and
  `tests/` trees do not count either: "tested but never invoked" is exactly the
  smell this gate exists to catch.
* Orphans are compared against `ci/orphan_cores_baseline.json`:
    - a NEW orphan (not in the baseline)  → FAIL (wire it or baseline it with
      a justification in the PR that adds it);
    - a baseline entry that is no longer an orphan → WARN (remove the entry —
      decreases never need review).

Heuristic honesty: reference detection is textual, not semantic. Consumption
via a renamed re-export or macro-generated path can be missed — that is what
the baseline (with justification strings) is for. The gate is a ratchet, not a
proof.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASELINE_PATH = REPO / "ci" / "orphan_cores_baseline.json"

# Crate roots to scan for `pub mod NAME;` declarations.
LIB_GLOBS = ["src/lib.rs", "crates/*/src/lib.rs", "parko/crates/*/src/lib.rs"]

# Trees whose references DO count as consumption (non-test production source;
# examples count — they are the stated purpose of the eval-harness crates).
CONSUMER_GLOBS = [
    "src/**/*.rs",
    "crates/*/src/**/*.rs",
    "crates/*/examples/**/*.rs",
    "parko/crates/*/src/**/*.rs",
    "parko/crates/*/examples/**/*.rs",
]

# Trees that never count as consumers: test suites and verification harnesses.
NON_CONSUMER_MARKERS = (
    "/tests/",
    "/fuzz/",
    "/verification/",
    "/kirra-loom-models/",
    "/benches/",
)

PUB_MOD_RE = re.compile(r"^\s*pub\s+mod\s+([a-z_][a-z0-9_]*)\s*;", re.MULTILINE)
REEXPORT_RE = re.compile(r"^\s*pub\s+(use|mod)\b")


def crate_lib_name(lib_rs: Path) -> str:
    """The crate's lib identifier (Cargo `name` with `-` → `_`)."""
    manifest = lib_rs.parent.parent / "Cargo.toml"
    text = manifest.read_text(encoding="utf-8")
    m = re.search(r'^\s*name\s*=\s*"([^"]+)"', text, re.MULTILINE)
    return (m.group(1) if m else lib_rs.parent.parent.name).replace("-", "_")


def declared_pub_mods() -> list[tuple[str, str, Path]]:
    """(crate_lib, mod_name, lib_rs) for every file-backed root `pub mod`."""
    out = []
    for glob in LIB_GLOBS:
        for lib_rs in sorted(REPO.glob(glob)):
            src = lib_rs.read_text(encoding="utf-8")
            crate = crate_lib_name(lib_rs)
            for name in PUB_MOD_RE.findall(src):
                # Only file-backed modules that actually exist as files/dirs.
                moddir = lib_rs.parent
                if (moddir / f"{name}.rs").exists() or (moddir / name / "mod.rs").exists():
                    out.append((crate, name, lib_rs))
    return out


def module_own_paths(lib_rs: Path, mod_name: str) -> set[Path]:
    moddir = lib_rs.parent
    own = {moddir / f"{mod_name}.rs"}
    sub = moddir / mod_name
    if sub.is_dir():
        own.update(sub.rglob("*.rs"))
    return own


def consumer_files() -> list[Path]:
    files: list[Path] = []
    for glob in CONSUMER_GLOBS:
        files.extend(REPO.glob(glob))
    uniq = []
    seen = set()
    for f in files:
        p = f.resolve()
        s = str(p)
        if p in seen or any(m in s for m in NON_CONSUMER_MARKERS):
            continue
        seen.add(p)
        uniq.append(p)
    return uniq


PUB_ITEM_RE = re.compile(
    r"^\s*pub\s+(?:async\s+)?(?:unsafe\s+)?(?:fn|struct|enum|trait|const|static|type)\s+"
    r"([A-Za-z_]\w{3,})",
    re.MULTILINE,
)


def module_pub_items(lib_rs: Path, mod_name: str) -> set[str]:
    """The module's own top-level `pub` item identifiers (len ≥ 4).

    Consumers usually import a re-exported item (`use kirra_planner::
    LlmIntentParser`), not the module path — a purely path-based scan calls
    every such module an orphan. Scanning the module's own pub items covers
    every re-export shape (renamed, glob, multi-line) at once; the length
    floor keeps ubiquitous short names (`Pose`…) from faking consumption.
    """
    items: set[str] = set()
    for f in module_own_paths(lib_rs, mod_name):
        if f.exists():
            items.update(PUB_ITEM_RE.findall(f.read_text(encoding="utf-8")))
    return items


def is_consumed(crate: str, mod_name: str, lib_rs: Path, files: list[Path]) -> bool:
    # The module's own files never count. The declaring lib.rs DOES count —
    # sibling code there (a gate runner calling into the module) is real
    # consumption; only its re-export/comment lines are skipped below.
    own = {p.resolve() for p in module_own_paths(lib_rs, mod_name)}
    # `use crate::NAME` / `use some_crate::…NAME` / `NAME::item`
    use_re = re.compile(rf"^\s*(?:pub\s+)?use\s+[\w:{{}}, ]*\b{mod_name}\b")
    path_re = re.compile(rf"\b{mod_name}::")
    qualified_re = re.compile(rf"\b{crate}::{mod_name}\b")
    items = module_pub_items(lib_rs, mod_name)
    item_re = (
        re.compile(r"\b(" + "|".join(re.escape(i) for i in sorted(items)) + r")\b")
        if items
        else None
    )
    for f in files:
        if f in own:
            continue
        try:
            text = f.read_text(encoding="utf-8")
        except OSError:
            continue
        for line in text.splitlines():
            if REEXPORT_RE.match(line) or line.lstrip().startswith("//"):
                continue  # re-exports label the shelf; comments aren't code
            if qualified_re.search(line) or path_re.search(line):
                return True
            if use_re.match(line) and not REEXPORT_RE.match(line):
                return True
            if item_re and item_re.search(line):
                return True
    return False


def main() -> int:
    baseline = {}
    if BASELINE_PATH.exists():
        baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    allow = baseline.get("allowed_orphans", {})

    files = consumer_files()
    orphans = []
    for crate, name, lib_rs in declared_pub_mods():
        if not is_consumed(crate, name, lib_rs, files):
            orphans.append(f"{crate}::{name}")

    new = [o for o in orphans if o not in allow]
    healed = [o for o in allow if o not in orphans]

    print(f"orphan-core gate: {len(orphans)} orphan(s), {len(allow)} baselined")
    for o in sorted(orphans):
        tag = "BASELINED" if o in allow else "NEW"
        print(f"  [{tag}] {o}" + (f" — {allow[o]}" if o in allow else ""))
    for o in sorted(healed):
        print(f"  [HEALED] {o} — no longer an orphan; remove it from the baseline")

    if new:
        print(
            "\nFAIL: new orphan pure core(s) — a `pub mod` at a crate root has no "
            "non-test consumer. Wire it to a live caller, delete it, or add it to "
            "ci/orphan_cores_baseline.json with a justification.",
            file=sys.stderr,
        )
        return 1
    print("orphan-core gate green")
    return 0


if __name__ == "__main__":
    sys.exit(main())
