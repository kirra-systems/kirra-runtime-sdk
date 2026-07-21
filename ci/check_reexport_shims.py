#!/usr/bin/env python3
"""ADR-0035 re-export shim ratchet (#1029 A1 — the shim-deprecation front).

The de-monolith relocated cohesive modules into lean leaf crates and left a thin
`pub use <lean-crate>::*;` re-export shim behind in `src/` so every existing
`crate::<mod>::*` / `kirra_verifier::<mod>::*` path resolves unchanged. That is a
DELIBERATE transition aid — but the review (#1029) flagged the shims as "pure
indirection cost" that must be treated as a DEPRECATION with a removal milestone,
NOT a permanent layer.

This guard makes that policy enforceable. It:

  1. Discovers every re-export shim in `src/` (a module file whose only code
     statements are `use` / `pub use` — zero item definitions).
  2. Compares the discovered set to the tracked inventory
     (`ci/reexport_shims_baseline.json`).
  3. FAILS if a NEW, untracked shim appears (so new indirection can't accrete
     silently — a new shim is a conscious decision that must be recorded here with
     its canonical target, or, better, avoided) or if the count exceeds the
     ceiling.
  4. Notes (does not fail) when a tracked shim is GONE — a removal win; the dev
     tightens the baseline to lock it (the ratchet only moves down).

Removal milestone: the whole set is scheduled for deletion at the next MAJOR
(`remove_in` in the baseline), when internal callers repoint to the canonical
crate path. See docs/adr/0035-verifier-crate-decomposition.md §Shim deprecation.

Usage:
  python3 ci/check_reexport_shims.py            # gate (CI)
  python3 ci/check_reexport_shims.py --list     # print the discovered shim set
  python3 ci/check_reexport_shims.py --self-test # prove the detector is non-vacuous
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SRC_ROOT = REPO_ROOT / "src"
BASELINE_PATH = REPO_ROOT / "ci" / "reexport_shims_baseline.json"

# A statement that, if present as a top-level item, means the file carries real
# code and is therefore NOT a pure re-export shim.
_DEFINITION_RE = re.compile(
    r"^(pub(\s*\([^)]*\))?\s+)?"
    r"(fn|struct|enum|trait|impl|const|static|type|mod|union|macro_rules!)\b"
)


def _strip_inline_comment(line: str) -> str:
    """Drop a trailing `// ...` comment. Rust `use` paths use `::`, never `//`,
    and shim files hold no string literals, so a `//` not preceded by `:` starts a
    comment."""
    return re.sub(r"(?<!:)//.*$", "", line)


def is_reexport_shim(path: Path) -> bool:
    """True iff `path` is a pure `pub use` re-export shim (>=1 `pub use`, and every
    code statement is a `use`/`pub use` — no item definitions)."""
    text = path.read_text(encoding="utf-8")
    # Remove block comments (shims use line comments, but be robust).
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)

    code_lines: list[str] = []
    for raw in text.splitlines():
        s = _strip_inline_comment(raw).strip()
        if not s:
            continue
        if s.startswith("//"):  # // /// //! doc/line comments
            continue
        if s.startswith("#!") or s.startswith("#["):  # attributes (inner/outer)
            continue
        code_lines.append(s)

    if not code_lines:
        return False

    # A definition keyword anywhere disqualifies immediately (cheap + precise).
    for line in code_lines:
        if _DEFINITION_RE.match(line):
            return False

    # Every `;`-terminated statement must be a use / pub use.
    joined = " ".join(code_lines)
    stmts = [x.strip() for x in joined.split(";") if x.strip()]
    if not stmts:
        return False

    has_pub_use = False
    for st in stmts:
        if st.startswith("pub use"):
            has_pub_use = True
        elif st.startswith("use "):
            continue  # a private `use` supporting the re-export
        else:
            return False
    return has_pub_use


def discover_shims() -> list[str]:
    """Repo-relative POSIX paths of every re-export shim under src/, sorted."""
    shims = []
    for path in SRC_ROOT.rglob("*.rs"):
        if is_reexport_shim(path):
            shims.append(path.relative_to(REPO_ROOT).as_posix())
    return sorted(shims)


def _self_test() -> int:
    """Prove the detector distinguishes a shim from a real module (non-vacuity)."""
    import tempfile

    ok = True
    with tempfile.TemporaryDirectory() as d:
        shim = Path(d) / "shim.rs"
        shim.write_text("// doc\npub use other_crate::thing::*;\n")
        real = Path(d) / "real.rs"
        real.write_text("// doc\npub use x::Y;\npub fn f() -> u8 { 0 }\n")
        multiline = Path(d) / "ml.rs"
        multiline.write_text("pub use a::b::{\n    One,\n    Two,\n};\n")
        if not is_reexport_shim(shim):
            print("SELF-TEST FAIL: a pure re-export was not detected as a shim")
            ok = False
        if is_reexport_shim(real):
            print("SELF-TEST FAIL: a module with a `fn` was misdetected as a shim")
            ok = False
        if not is_reexport_shim(multiline):
            print("SELF-TEST FAIL: a multi-line `pub use {..}` was not detected")
            ok = False
    if ok:
        print("self-test OK: shim detector is non-vacuous")
        return 0
    return 1


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        rc = _self_test()
        if rc != 0:
            return rc

    discovered = discover_shims()

    if "--list" in argv:
        print(f"{len(discovered)} re-export shims discovered under src/:")
        for s in discovered:
            print(f"  {s}")
        return 0

    if not BASELINE_PATH.exists():
        print(f"ERROR: baseline missing: {BASELINE_PATH}")
        print("Run with --list and record the set in the baseline.")
        return 2

    baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    tracked = set(baseline.get("shims", {}).keys())
    max_shims = int(baseline.get("max_shims", 0))
    remove_in = baseline.get("remove_in", "?")
    discovered_set = set(discovered)

    violations: list[str] = []

    new_shims = sorted(discovered_set - tracked)
    for s in new_shims:
        violations.append(
            f"- NEW untracked re-export shim: {s}\n"
            f"    A new `pub use` shim is fresh indirection. Prefer NOT adding one;\n"
            f"    if it is a deliberate transition aid, record it in\n"
            f"    ci/reexport_shims_baseline.json with its canonical target crate."
        )

    if len(discovered) > max_shims:
        violations.append(
            f"- shim count {len(discovered)} > ceiling {max_shims} "
            f"(the ratchet only moves DOWN — shims are deprecated, remove_in={remove_in})"
        )

    # A tracked shim that vanished is a removal WIN — note it and ask to tighten.
    removed = sorted(tracked - discovered_set)
    for s in removed:
        print(
            f"NOTE: tracked shim no longer present: {s}\n"
            f"      Removal win — drop it from ci/reexport_shims_baseline.json and "
            f"lower max_shims to lock the gain."
        )

    print(
        f"\nre-export shims: {len(discovered)} present "
        f"(ceiling {max_shims}, scheduled for removal at v{remove_in})."
    )

    if violations:
        print("\n=== Re-export shim ratchet violations ===")
        for v in violations:
            print(v)
        return 1

    print("Re-export shim ratchet satisfied (no new indirection).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
