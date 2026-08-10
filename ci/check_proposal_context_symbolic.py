#!/usr/bin/env python3
"""Tier 2.5 — the capability gate on the World→proposal seam.

THE RULE
--------

    World-derived proposal context is SYMBOLIC ONLY. Its public API may carry
    identities, relations, ordering, categorical state, and opaque references;
    it may not carry numeric quantities that could encode checker bounds.

WHY THIS AND NOT "the crate does not depend on kirra-core"
----------------------------------------------------------

That weaker property is true today and one `use` away from false, and its
violation is invisible in review. This one is structural: every checker bound in
this codebase is a magnitude with physical units -- corridor width, max speed,
stopping distance, lateral acceleration, wheelbase -- and a type with nowhere to
put a magnitude cannot carry one however hard a future caller tries.

WHY INTEGERS TOO, not just floats
---------------------------------

`speed_mm_s: u32` is a bound wearing a disguise, and it is the MORE likely
accident: someone reaching for integer millimetres is usually trying to be
careful about precision. A float-only ban would wave it through. So the gate
prohibits every primitive numeric type and requires an explicit allowlist entry,
with a written non-physical justification, for the handful of legitimate cases
(a schema version, an index). That inverts the burden the right way -- a
quantity with units cannot write that justification.

WHAT IS AND IS NOT CHECKED
--------------------------

CHECKED: the types that CROSS the seam -- fields of public structs, and the
payloads of public enum variants, in the guarded crate.

NOT CHECKED: function parameters and return scalars. `now_ms` is a bitemporal
query instant; the store cannot be read without one, and it is never carried ON
the context. The rule bites on values that cross the seam, because those are the
values a planner can act on. A value used to read the store and then discarded
is not one of them. This is a real limit and it is stated rather than papered
over: a producer could in principle take a bound as an argument and encode it
into an id string. Nothing here would catch that, and nothing cheap would.

Exit 0 when the seam is symbolic; exit 1 with a per-violation report otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# The crate this gate guards. A list, so a second sanctioned consumer inherits
# the rule by being added here rather than by remembering to.
GUARDED_CRATES = ("crates/kirra-proposal-context",)

# Every Rust primitive that can hold a magnitude. `bool` and `char` are absent
# deliberately: neither can express "how much", which is the whole question.
NUMERIC_PRIMITIVES = {
    "f32", "f64",
    "i8", "i16", "i32", "i64", "i128", "isize",
    "u8", "u16", "u32", "u64", "u128", "usize",
    "NonZeroU8", "NonZeroU16", "NonZeroU32", "NonZeroU64", "NonZeroUsize",
    "NonZeroI8", "NonZeroI16", "NonZeroI32", "NonZeroI64", "NonZeroIsize",
}

# (type, field) -> justification. EMPTY BY DESIGN: the seam currently carries no
# numbers at all, and the strongest version of this gate is one with nothing on
# its allowlist. An entry here must explain why the quantity is non-physical --
# "a schema version", "an index into a caller-supplied list". If a proposed
# justification mentions a unit, it is a bound and the answer is no.
ALLOWLIST: dict[tuple[str, str], str] = {}

# `pub struct Foo {` / `pub enum Foo {` — captures the kind and the name.
TYPE_DECL = re.compile(r"^\s*pub\s+(struct|enum)\s+([A-Za-z_][A-Za-z0-9_]*)")
# A field line: `pub name: Type,` inside a struct, or `name: Type,` inside an
# enum variant's braces (enum payloads are public with the enum).
FIELD = re.compile(r"^\s*(?:pub\s+(?:\([^)]*\)\s+)?)?([a-z_][A-Za-z0-9_]*)\s*:\s*(.+?),?\s*$")
# A tuple-struct / tuple-variant payload: `pub struct Foo(String);` or `Bar(u32)`.
TUPLE_PAYLOAD = re.compile(r"^\s*(?:pub\s+)?(?:struct\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\((.+?)\)")


def strip_comments(text: str) -> str:
    """Remove line comments so prose about `f64` is not read as a field.

    Load-bearing: this crate's module docs discuss speed caps and corridor
    widths at length, precisely to explain why they may not cross. Counting
    those as violations would make the gate fire on its own rationale.
    """
    out = []
    for line in text.splitlines():
        idx = line.find("//")
        out.append(line if idx == -1 else line[:idx])
    return "\n".join(out)


def numeric_types_in(type_expr: str) -> list[str]:
    """Primitive numeric types mentioned anywhere in a type expression.

    Looks INSIDE generics and containers: `Vec<f64>`, `Option<u32>` and
    `(String, f64)` all carry a magnitude just as plainly as a bare `f64`.
    """
    tokens = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", type_expr)
    return [t for t in tokens if t in NUMERIC_PRIMITIVES]


def scan_source(text: str, path: str) -> list[str]:
    """Violations in one Rust source. Returns human-readable lines."""
    violations: list[str] = []
    text = strip_comments(text)
    current_type: str | None = None
    depth_at_decl: int | None = None
    depth = 0
    in_test_mod = False
    test_mod_depth: int | None = None

    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw

        # `#[cfg(test)]` modules are not part of the public seam.
        if re.match(r"^\s*#\[cfg\(test\)\]", line):
            in_test_mod = True
            test_mod_depth = depth
        if in_test_mod and test_mod_depth is not None and depth < test_mod_depth:
            in_test_mod = False
            test_mod_depth = None

        decl = TYPE_DECL.match(line)
        if decl and not in_test_mod:
            current_type = decl.group(2)
            depth_at_decl = depth
            # A tuple struct declares its payload on the same line.
            tup = TUPLE_PAYLOAD.search(line)
            if tup and "{" not in line:
                for num in numeric_types_in(tup.group(2)):
                    if (current_type, "0") not in ALLOWLIST:
                        violations.append(
                            f"{path}:{lineno}: `{current_type}` tuple payload is `{num}` "
                            f"-- a magnitude cannot cross the seam"
                        )
        elif current_type and not in_test_mod and depth_at_decl is not None and depth > depth_at_decl:
            # Scan the WHOLE LINE, not a field pattern.
            #
            # This started as three regexes -- named field, tuple payload, tuple
            # variant -- and control 3 (`t07`) caught what that missed: a
            # single-line struct variant, `Envelope { lateral_accel_mps2: f64 },`.
            # It matches none of them, because the leading token is a variant name
            # rather than a field name and the payload is braced rather than
            # parenthesised. Enumerating declaration shapes means the gate is only
            # as good as the list, and the list was already wrong once.
            #
            # Inside a public type's body every line is part of what that type can
            # HOLD, so any numeric primitive token on it is a magnitude crossing
            # the seam regardless of the syntax carrying it. Comments are already
            # stripped and attributes are skipped, so the remaining lines are
            # declarations.
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                field = FIELD.match(line)
                name = field.group(1) if field else None
                for num in numeric_types_in(line):
                    if name is not None and (current_type, name) in ALLOWLIST:
                        continue
                    where = f"{current_type}.{name}" if name else current_type
                    violations.append(
                        f"{path}:{lineno}: `{where}` carries `{num}` in "
                        f"`{stripped}` -- a magnitude cannot cross the seam"
                    )

        depth += line.count("{") - line.count("}")
        if current_type and depth_at_decl is not None and depth <= depth_at_decl:
            current_type = None
            depth_at_decl = None

    return violations


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    violations: list[str] = []
    scanned = 0

    for crate in GUARDED_CRATES:
        src = root / crate / "src"
        if not src.is_dir():
            print(f"FAIL: guarded crate {crate} has no src/ -- gate cannot run")
            return 1
        for path in sorted(src.rglob("*.rs")):
            scanned += 1
            rel = str(path.relative_to(root))
            violations.extend(scan_source(path.read_text(encoding="utf-8"), rel))

    if scanned == 0:
        print("FAIL: no sources scanned -- a gate that inspects nothing passes everything")
        return 1

    print(f"Proposal-context symbolic-seam gate: scanned {scanned} source(s)")
    if violations:
        print("\n=== The seam is carrying a magnitude ===")
        for v in violations:
            print(f"  - {v}")
        print(
            "\nWorld-derived proposal context is symbolic only: identities, relations,\n"
            "ordering, categorical state, opaque references. A numeric field could encode\n"
            "a checker bound (speed, distance, acceleration, width), and the point of this\n"
            "seam is that it CANNOT. If the quantity is genuinely non-physical -- a schema\n"
            "version, an index -- add it to ALLOWLIST with that justification. If the\n"
            "justification needs a unit, it is a bound and the answer is no."
        )
        return 1

    print("  seam is symbolic: no magnitude can cross it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
