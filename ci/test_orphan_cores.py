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


#: The module the gate was blind to, and the change that fixed it.
#:
#: `kirra_world::same_as_candidate` (Tier 2 box 2a) landed deliberately unwired.
#: Before the code-reference fix the gate called it CONSUMED — the exact false
#: negative it exists to prevent — because its accessors (`version`, `high`,
#: `pair`, `support`) matched unrelated locals and error strings elsewhere.
#:
#: Tier 2 box 2b (`same_as_adjudication`) is its first real consumer: it imports
#: `CandidatePair` and `SameAsCandidate` and takes them in fn signatures, which
#: is structural connectivity rather than textual coincidence.
WIRED_MODULE = "kirra_world::same_as_candidate"
WIRING_EVENT = "Tier 2 box 2b (kirra_world::same_as_adjudication)"

#: The shape that fooled the gate, as a throwaway crate. Its module exports only
#: ubiquitous ACCESSOR names, and the sibling file mentions them exactly the
#: three ways that used to count as consumption: inside a string, as an impl
#: accessor, and in commented-out code.
BLIND_SPOT_SHAPE = {
    "alpha": {
        "lib.rs": (
            "pub mod widget;\n"
            "pub struct Other;\n"
            "impl Other {\n"
            "    pub fn version(&self) -> u32 { 0 }\n"
            "}\n"
            "pub fn go() {\n"
            '    let _ = "high-water version support";\n'
            "    // WidgetThing::make();\n"
            "}\n"
        ),
        "widget.rs": (
            "pub struct WidgetThing;\n"
            "impl WidgetThing {\n"
            "    pub fn version(&self) -> u32 { 0 }\n"
            "    pub fn support(&self) -> u32 { 0 }\n"
            "    pub fn high(&self) -> u32 { 0 }\n"
            "}\n"
        ),
    }
}


def historical_non_vacuity() -> bool:
    """Both halves of the gate fix, asserted together and permanently.

    The reviewer's framing, and it is better than retiring the fixture once the
    real module got wired: a repaired gate should keep PROVING it catches the
    class it was blind to, not just that today's tree happens to be clean.

    So this asserts two things at once:

    * **the class is still caught** — a synthetic module whose only apparent
      consumers are its accessor names in a string, an impl accessor and a
      commented-out call is STILL reported as an orphan. If this half stops
      failing, the gate has regressed to textual matching, whatever the real
      tree looks like.
    * **the real module is now consumed** — `same_as_candidate` is no longer an
      orphan, because 2b imports its types and takes them in signatures. If this
      half breaks, something unwired 2b or the matching became too strict.

    Keeping both is what turns a one-time repair into durable evidence: the
    first half cannot be satisfied by an accident of the tree, and the second
    records WHY the status changed rather than leaving a deleted fixture behind.
    """
    ok = True

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build(root, BLIND_SPOT_SHAPE)
        got = orphans(root)
    caught = got == {"alpha::widget"}
    print(f"  {'PASS' if caught else 'FAIL'}  the gate still catches the blind-spot shape")
    if not caught:
        print(f"        expected {{'alpha::widget'}}, got {sorted(got)}")
        print("        the gate has regressed to crediting names in text")
    ok &= caught

    live = set(G.find_orphans())
    wired = WIRED_MODULE not in live
    print(f"  {'PASS' if wired else 'FAIL'}  {WIRED_MODULE} is consumed, not orphaned")
    if not wired:
        print(f"        {WIRED_MODULE} is reported as an orphan again")
        print(f"        it was wired by {WIRING_EVENT}; check that consumer still imports it")
    ok &= wired

    return ok


def strip_preserves_line_structure() -> bool:
    """`strip_noncode` must keep one output line per input line.

    Asserted DIRECTLY rather than through a crate fixture, and that distinction
    is the point. The escaped-character branch used to blank a backslash and the
    character after it as two spaces, which swallows the newline of a Rust line
    continuation (`"abc \\` newline `def"`) and merges the next source line onto
    this one.

    An end-to-end fixture for it is VACUOUS: the only line-anchored rule is
    `use_re`, and the un-anchored `path_re` matches the same reference on the
    merged line, so the verdict never changes. A test that passes with and
    without the fix proves nothing, so it is not written that way. What is real
    is that the function documents "newlines are preserved" and did not do it —
    a latent trap for the next line-anchored rule added without a backstop.
    """
    src = 'let s = "abc \\\n        def";\nuse crate::beta::WidgetThing;\n'
    got = len(G.strip_noncode(src).splitlines())
    want = len(src.splitlines())
    ok = got == want
    print(f"  {'PASS' if ok else 'FAIL'}  strip_noncode preserves line structure")
    if not ok:
        print(f"        expected {want} lines, got {got} — a continuation newline was swallowed")
    return ok


def strip_keep_strings_is_opt_in_and_faithful() -> bool:
    """`keep_strings=True` blanks comments only; the default is unchanged.

    Both halves are asserted because both are load-bearing. The DEFAULT must
    stay byte-identical or the orphan gate's own reasoning — a name inside a
    string is prose, not consumption — quietly reverses. The FLAG must emit the
    literal verbatim, which is why `check_world_semantics.py` can pin a reducer
    whose behaviour lives in a returned token.
    """
    src = 'let s = "kirra_world::same_as_candidate"; // kirra_world::other\n'

    default = G.strip_noncode(src)
    kept = G.strip_noncode(src, keep_strings=True)

    checks = [
        ("default still blanks the literal", "same_as_candidate" not in default),
        ("default still blanks the comment", "other" not in default),
        ("keep_strings emits the literal", "same_as_candidate" in kept),
        ("keep_strings still blanks the comment", "other" not in kept),
        (
            "line structure preserved either way",
            len(default.splitlines()) == len(kept.splitlines()) == len(src.splitlines()),
        ),
    ]
    ok = all(passed for _, passed in checks)
    print(f"  {'PASS' if ok else 'FAIL'}  strip_noncode keep_strings is opt-in and faithful")
    for label, passed in checks:
        if not passed:
            print(f"        {label}")
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

    results.append(strip_preserves_line_structure())

    results.append(strip_keep_strings_is_opt_in_and_faithful())

    results.append(historical_non_vacuity())

    print()
    if all(results):
        print(f"orphan-core gate tests: all {len(results)} passed")
        return 0
    print(f"orphan-core gate tests: {results.count(False)} of {len(results)} FAILED")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
