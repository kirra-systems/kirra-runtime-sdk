#!/usr/bin/env python3
"""Tests for the orphan-pure-core gate.

The gate had none, which is part of why the hole below survived: a guard that
is never run against a fixture is only ever exercised by the tree it happens to
be pointed at, and a false NEGATIVE there looks exactly like success.

Every case builds a throwaway crate on disk and runs the real functions against
it. The negative half carries the weight — a gate that cannot report an orphan
proves nothing.

Four properties:

  * an unwired module IS an orphan;
  * a module referenced by non-test source is NOT;
  * a module reachable only through a `pub use` IS still an orphan — a shelf
    with a label is still a shelf;
  * **a module whose only apparent consumer is a COLLIDING item name is still
    an orphan.** That is the regression this file exists for.
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def load_gate():
    """Import the gate as a module, which does not run `main`.

    The gate guards its entry point with `if __name__ == "__main__":`, and an
    imported module's `__name__` is its module name — so a plain import cannot
    reach `main`. Nothing needs to be rewritten to prevent it.
    """
    path = REPO / "ci" / "check_orphan_cores.py"
    spec = importlib.util.spec_from_file_location("orphan_gate", path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["orphan_gate"] = mod
    spec.loader.exec_module(mod)
    return mod


G = load_gate()


def build(root: Path, crates: dict[str, dict[str, str]]) -> None:
    """`{crate: {"lib.rs": src, "mod_name.rs": src, ...}}`."""
    for crate, files in crates.items():
        d = root / "crates" / crate / "src"
        d.mkdir(parents=True, exist_ok=True)
        # The gate reads the lib name from the manifest, so a fixture needs one.
        (d.parent / "Cargo.toml").write_text(
            f'[package]\nname = "{crate}"\n', encoding="utf-8"
        )
        for name, src in files.items():
            (d / name).write_text(src, encoding="utf-8")


def orphans(root: Path) -> set[str]:
    """Run the real gate over a fixture tree.

    Calls the gate's own composition rather than reassembling it here, so a
    wiring mistake inside `find_orphans` — dropping the ambiguity set, say —
    fails these tests instead of slipping past a private copy of the loop.
    """
    # Save/restore: the fixtures point the gate at a throwaway tree, and a temp
    # dir that outlives the `with` block is a deleted one. Leaking it made the
    # real-tree regression below scan nothing and report zero orphans — a
    # green that meant "the harness lost the repo", not "nothing is orphaned".
    real_repo = G.REPO
    try:
        G.REPO = root
        return set(G.find_orphans())
    finally:
        G.REPO = real_repo


def case(name: str, crates: dict, expected: set[str]) -> bool:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build(root, crates)
        got = orphans(root)
    ok = got == expected
    print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    if not ok:
        print(f"        expected {sorted(expected)}")
        print(f"        got      {sorted(got)}")
    return ok


#: The module this gate was blind to, against the REAL tree.
#:
#: Tier 2 box 2a landed `kirra_world::same_as_candidate` deliberately unwired —
#: 2b (adjudication/promotion) is its first consumer. Before the code-reference
#: fix the gate called it CONSUMED, which is precisely the false negative it
#: exists to prevent, so it makes an honest regression fixture: a real module,
#: in the real repo, in the state the gate must be able to see.
REAL_ORPHAN_FIXTURE = "kirra_world::same_as_candidate"


def real_tree_regression() -> bool:
    """The fixture runs against the real repo, not a synthetic crate.

    **This test is expected to flip when 2b wires the module, and that is the
    point.** A failure here means one of two things, and the message says which
    to check first:

    * 2b (or anything else) now consumes `same_as_candidate` — then this fixture
      has done its job and should be repointed at whatever is currently unwired,
      or retired with the synthetic cases above kept;
    * the code-reference matching regressed and bare identifiers in prose are
      being credited again — then the gate is back to hiding orphans.
    """
    orphans = set(G.find_orphans())
    ok = REAL_ORPHAN_FIXTURE in orphans
    print(f"  {'PASS' if ok else 'FAIL'}  the real unwired module is detected as an orphan")
    if not ok:
        print(f"        expected {REAL_ORPHAN_FIXTURE} among the reported orphans")
        print("        if 2b now consumes it: repoint or retire this fixture (it worked)")
        print("        if not: the code-reference matching has regressed")
    return ok


def main() -> int:
    results = []

    results.append(
        case(
            "an unwired module is an orphan",
            {"alpha": {"lib.rs": "pub mod widget;\n", "widget.rs": "pub fn make_widget() {}\n"}},
            {"alpha::widget"},
        )
    )

    results.append(
        case(
            "a module called from sibling source is not an orphan",
            {
                "alpha": {
                    "lib.rs": "pub mod widget;\npub fn go() { widget::make_widget(); }\n",
                    "widget.rs": "pub fn make_widget() {}\n",
                }
            },
            set(),
        )
    )

    results.append(
        case(
            "a module reachable only through a `pub use` is still an orphan",
            {
                "alpha": {
                    "lib.rs": "pub mod widget;\npub use widget::make_widget;\n",
                    "widget.rs": "pub fn make_widget() {}\n",
                }
            },
            {"alpha::widget"},
        )
    )

    # THE REGRESSION. Two modules export `fold_all`; only `beta` is consumed.
    # Before 2026-08-08 the item scan matched `fold_all` in beta.rs and
    # pronounced `gamma` consumed while it was entirely unwired -- which is the
    # exact condition this gate exists to catch, and is how
    # `kirra_world_store::entity_projection` passed while unwired.
    results.append(
        case(
            "a colliding item name does not count as consumption",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub mod gamma;\npub fn go() { beta::fold_all(); }\n",
                    "beta.rs": "pub fn fold_all() {}\n",
                    "gamma.rs": "pub fn fold_all() {}\n",
                }
            },
            {"alpha::gamma"},
        )
    )

    # ...and the exclusion must not over-reach: a module with a colliding name
    # AND a unique consumed one is still consumed. Without this, the fix would
    # trade a false negative for a false positive.
    results.append(
        case(
            "a unique item name still counts when a sibling name collides",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub mod gamma;\npub fn go() { let _ = GammaThing; }\n",
                    "beta.rs": "pub fn fold_all() {}\n",
                    "gamma.rs": "pub fn fold_all() {}\npub struct GammaThing;\n",
                }
            },
            {"alpha::beta"},
        )
    )

    # -- code references, not bare identifiers in text ----------------------
    #
    # The second hole. `ambiguous_item_names` handled a name owned by two
    # modules; it said nothing about a name owned by one module that is also an
    # ordinary English word or a common local. `kirra_world::same_as_candidate`
    # was pronounced consumed, while entirely unwired, by `"… high-water
    # {high_water}"` in an unrelated error message.

    results.append(
        case(
            "a name appearing only in a comment is not consumption",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub fn go() { let _ = 1; }\n// WidgetThing is discussed here\n",
                    "beta.rs": "pub struct WidgetThing;\n",
                }
            },
            {"alpha::beta"},
        )
    )

    # Commented-out code is the case that gives comment-stripping its own teeth:
    # it is CODE-SHAPED, so the code-reference rule alone would credit it.
    results.append(
        case(
            "a commented-out call is not consumption",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub fn go() {\n    // WidgetThing::make();\n}\n",
                    "beta.rs": "pub struct WidgetThing;\n",
                }
            },
            {"alpha::beta"},
        )
    )

    results.append(
        case(
            "a name appearing only in a string literal is not consumption",
            {
                "alpha": {
                    "lib.rs": 'pub mod beta;\npub fn go() { let _ = "WidgetThing failed"; }\n',
                    "beta.rs": "pub struct WidgetThing;\n",
                }
            },
            {"alpha::beta"},
        )
    )

    # ...and the tightening must not over-reach: a genuine code reference in
    # any of the ordinary shapes still counts.
    results.append(
        case(
            "a genuine code reference still counts as consumption",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub fn go() { WidgetThing::make(); }\n",
                    "beta.rs": "pub struct WidgetThing;\n",
                }
            },
            set(),
        )
    )

    # A method name is NOT an importable item. Treating `pub fn version` as one
    # is what made an accessor collide with every unrelated local called
    # `version` in the tree.
    results.append(
        case(
            "an impl method name is not evidence the module is consumed",
            {
                "alpha": {
                    "lib.rs": "pub mod beta;\npub fn go(x: u32) { let version = x; let _ = version; }\n",
                    "beta.rs": "pub struct Widget;\nimpl Widget {\n    pub fn version(&self) -> u32 { 0 }\n}\n",
                }
            },
            {"alpha::beta"},
        )
    )

    # The stripper must not eat real code. Blanking `//` before strings
    # destroys a string containing a URL and leaves its opening quote
    # unmatched, after which a DOTALL string match swallows following lines —
    # which briefly turned a module with a real caller into an orphan.
    results.append(
        case(
            "a string containing // does not swallow the code after it",
            {
                "alpha": {
                    "lib.rs": 'pub mod beta;\npub fn go() {\n    let _ = "https://example.com/x";\n    WidgetThing::make();\n}\n',
                    "beta.rs": "pub struct WidgetThing;\n",
                }
            },
            set(),
        )
    )

    results.append(real_tree_regression())

    print()
    if all(results):
        print(f"orphan-core gate tests: all {len(results)} passed")
        return 0
    print(f"orphan-core gate tests: {results.count(False)} of {len(results)} FAILED")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
