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
    "/tests.rs",  # a `mod tests` in its own file is still a test tree
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


_IMPL_START = re.compile(r"^\s*(?:unsafe\s+)?impl\b")


def free_pub_items(text: str) -> set[str]:
    """The `pub` items a consumer could **import**, excluding `impl` members.

    Associated items are reached through a value of their type, never imported
    on their own, so a method name is not evidence that a module is consumed —
    but it *is* very often an ordinary local-variable name. Treating
    `pub fn version`, `pub fn high`, `pub fn pair`, `pub fn support` as
    importable items is what let `kirra_world::same_as_candidate` read as
    consumed while nothing referenced it: a `json!({ "v": version })` elsewhere
    in the tree matched an accessor it has no relationship to.

    Keeping only free items means the scan looks for `SameAsCandidate` and
    `proposed_relations` — names a caller would actually have to write.
    """
    items: set[str] = set()
    depth = 0
    # [depth_at_impl, entered_body]. The `entered` flag is load-bearing: an
    # `impl<B, S> ControlLoop<B, S>` with a `where` clause opens its brace three
    # lines later, so a naive "pop when depth <= depth_at_impl" pops the impl
    # before its body starts and every method inside is then read as a free
    # item. That is how `state`, `tick` and `with_clock` stayed in the item set
    # and kept `parko_core::control_loop` looking consumed.
    impl_stack: list[list] = []
    for line in text.splitlines():
        if _IMPL_START.match(line):
            impl_stack.append([depth, False])
        if not impl_stack:
            m = PUB_ITEM_RE.match(line)
            if m:
                items.add(m.group(1))
        depth += line.count("{") - line.count("}")
        while impl_stack:
            at, entered = impl_stack[-1]
            if not entered:
                if depth > at:
                    impl_stack[-1][1] = True
                break
            if depth <= at:
                impl_stack.pop()
            else:
                break
    return items


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
            items.update(free_pub_items(strip_noncode(f.read_text(encoding="utf-8"))))
    return items


def ambiguous_item_names(mods: list[tuple[str, str, Path]]) -> set[str]:
    """Item names exported by MORE THAN ONE scanned module.

    The length floor above stops *ubiquitous* names faking consumption; it does
    nothing about *colliding* ones, and that was a real hole rather than a
    theoretical one. `kirra_world_store::entity_projection` exports `fold_all`;
    so does its sibling `projection`. The item scan then matched `fold_all` in
    `projection.rs` and pronounced `entity_projection` consumed — while it was
    still entirely unwired, which is the exact condition this gate exists to
    catch. It had correctly flagged `adjudication_record` an hour earlier in the
    identical state, so the guard was not weak in general, only blind to a name
    it could not attribute.

    A name owned by two modules carries no information about WHICH of them a
    reference consumes, so it is evidence for neither. Excluding it leaves the
    heuristic doing its job for every unambiguous name and falls back to
    path-based detection where it cannot tell — which reports an orphan rather
    than hiding one, the correct direction for a fail-closed guard.
    """
    seen: dict[str, str] = {}
    ambiguous: set[str] = set()
    for crate, name, lib_rs in mods:
        for item in module_pub_items(lib_rs, name):
            owner = f"{crate}::{name}"
            if seen.setdefault(item, owner) != owner:
                ambiguous.add(item)
    return ambiguous


def strip_noncode(text: str) -> str:
    """Blank out comments and string literals, preserving line structure.

    A name inside a comment or a string is **prose, not consumption**, and the
    difference is not cosmetic: scanning raw text credited
    `kirra_world::same_as_candidate` as consumed on the strength of
    `"… high-water {high_water}"` — an accessor called `high` matching inside an
    error message about something unrelated.

    # Why this is one left-to-right pass and not a few regex substitutions

    Because the obvious version is wrong, and wrong in the direction that eats
    real code. Blanking `//` comments before string literals destroys the inside
    of any string that contains `//` — a URL — and leaves its opening quote
    unmatched. A subsequent `DOTALL` string pattern then pairs that stray quote
    with one many lines later and blanks everything between, including genuine
    references. That silently turned `kirra_verifier::audit_shipper` — a module
    with a real caller — into an orphan.

    Comments and strings can only be recognised by scanning in order, because
    each can contain the other's opening token. Newlines are preserved so
    line-oriented rules (`use` at line start) still work, and removed spans
    become spaces so nothing on either side is joined into a new token.
    """
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i : i + 2]
        if two == "//":
            while i < n and text[i] != "\n":
                out.append(" ")
                i += 1
        elif two == "/*":
            depth = 1  # Rust block comments nest
            out.append("  ")
            i += 2
            while i < n and depth:
                if text[i : i + 2] == "/*":
                    depth += 1
                    out.append("  ")
                    i += 2
                elif text[i : i + 2] == "*/":
                    depth -= 1
                    out.append("  ")
                    i += 2
                else:
                    out.append("\n" if text[i] == "\n" else " ")
                    i += 1
        elif c == "r" and (m := re.match(r'r(#*)"', text[i:])):
            close = '"' + m.group(1)
            end = text.find(close, i + m.end())
            end = n if end == -1 else end + len(close)
            out.extend("\n" if ch == "\n" else " " for ch in text[i:end])
            i = end
        elif c == '"':
            out.append(" ")
            i += 1
            while i < n:
                if text[i] == "\\":
                    out.append("  ")
                    i += 2
                    continue
                if text[i] == '"':
                    out.append(" ")
                    i += 1
                    break
                out.append("\n" if text[i] == "\n" else " ")
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def code_reference_re(items: set[str]) -> re.Pattern[str] | None:
    """Match an item name only where it is used **as code**.

    The bare-identifier scan this replaces could not tell `Corroboration` the
    type from "corroboration" the English word, so any sufficiently ordinary
    accessor name — `version`, `producer`, `support`, `high` — was satisfied by
    unrelated prose somewhere in a 150-file tree.

    A reference counts when it wears a syntactic marker that prose does not:
    a path (`Item::`), a call or tuple-struct (`Item(`), a struct literal
    (`Item {`), a method call (`.item(`), or a type position (`: Item`,
    `-> Item`, `<Item`, `&Item`, `, Item`).

    Anything else is treated as NOT consumption. That direction is deliberate:
    a missed real reference reports an orphan that is not one, which is visible
    and arguable, while a false match hides an unwired core — the exact defect
    this gate exists to catch, and the one it shipped with twice.
    """
    if not items:
        return None
    alt = "|".join(re.escape(i) for i in sorted(items))
    return re.compile(
        rf"(?:\b(?:{alt})\s*(?:::|\(|\{{|<))"  # Item::  Item(  Item {{  Item<
        rf"|(?:\.\s*(?:{alt})\s*\()"  # .item(
        rf"|(?:(?:->|:|&|<|,|=)\s*(?:{alt})\b)"  # type / binding position
    )


def is_consumed(
    crate: str,
    mod_name: str,
    lib_rs: Path,
    files: list[Path],
    ambiguous: set[str] | None = None,
) -> bool:
    # The module's own files never count. The declaring lib.rs DOES count —
    # sibling code there (a gate runner calling into the module) is real
    # consumption; only its re-export/comment lines are skipped below.
    own = {p.resolve() for p in module_own_paths(lib_rs, mod_name)}
    # `use crate::NAME` / `use some_crate::…NAME` / `NAME::item`
    use_re = re.compile(rf"^\s*(?:pub\s+)?use\s+[\w:{{}}, ]*\b{mod_name}\b")
    path_re = re.compile(rf"\b{mod_name}::")
    qualified_re = re.compile(rf"\b{crate}::{mod_name}\b")
    # A name two modules both export cannot say which one is consumed.
    items = module_pub_items(lib_rs, mod_name) - (ambiguous or set())
    item_re = code_reference_re(items)
    for f in files:
        if f in own:
            continue
        try:
            text = f.read_text(encoding="utf-8")
        except OSError:
            continue
        # Comments and string literals are blanked BEFORE any matching: a name
        # in prose or an error message is not a caller.
        for line in strip_noncode(text).splitlines():
            if REEXPORT_RE.match(line):
                continue  # re-exports label the shelf
            if qualified_re.search(line) or path_re.search(line):
                return True
            if use_re.match(line) and not REEXPORT_RE.match(line):
                return True
            if item_re and item_re.search(line):
                return True
    return False


def find_orphans() -> list[str]:
    """Every declared pure core with no non-test consumer, as `crate::module`.

    The whole gate, composed. `main` adds only the baseline comparison and the
    reporting, so this is the seam the tests drive — a test that reassembled
    these calls itself would pass even if this composition dropped one of them,
    which is the shape of the hole the gate had in the first place.
    """
    files = consumer_files()
    mods = declared_pub_mods()
    ambiguous = ambiguous_item_names(mods)
    return [
        f"{crate}::{name}"
        for crate, name, lib_rs in mods
        if not is_consumed(crate, name, lib_rs, files, ambiguous)
    ]


def main() -> int:
    baseline = {}
    if BASELINE_PATH.exists():
        baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    allow = baseline.get("allowed_orphans", {})

    orphans = find_orphans()

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
