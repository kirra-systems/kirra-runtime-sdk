#!/usr/bin/env python3
"""Tests for the proposal-context symbolic-seam gate.

**This file IS Tier 2.5's control 3.** The gate's whole claim is that a checker
bound cannot cross the World→proposal seam. A gate that has never been shown to
refuse anything is indistinguishable from a no-op, and it would be worse than
nothing: it would let the Tier 2.5 acceptance proof cite a capability limit that
does not exist.

So the negative half is the point. Each `*_rejects_*` case feeds the real scanner
synthetic source containing a quantity a planner could act on, and fails if the
gate stays quiet. The `*_accepts_*` cases guard the other failure mode — a gate so
eager that the intended symbolic design cannot be written, which would simply get
it switched off.

Three holes in the scanner have been found by writing these cases rather than by
reading the scanner: a single-line struct variant that matched none of the field
regexes; a `#[cfg(test)]` exit condition that could never fire, silencing every
line after the first test module; and a declaration line carrying a const generic.
Each is now a case below. The pattern is consistent enough to name: enumerating
the syntactic shapes a magnitude can take is only ever as good as the list.

Run:  python3 ci/test_proposal_context_symbolic.py
"""

from __future__ import annotations

import sys
import textwrap
from pathlib import Path

CI = Path(__file__).resolve().parent
sys.path.insert(0, str(CI))

from check_proposal_context_symbolic import scan_source  # noqa: E402

RESULTS: list[tuple[bool, str, str]] = []


def record(ok: bool, name: str, detail: str = "") -> None:
    RESULTS.append((ok, name, detail))


def scan(src: str) -> list[str]:
    return scan_source(textwrap.dedent(src), "fixture.rs")


def expect_rejected(name: str, src: str, because: str) -> None:
    found = scan(src)
    record(bool(found), name, "" if found else f"gate stayed quiet on {because}")


def expect_accepted(name: str, src: str) -> None:
    found = scan(src)
    record(not found, name, "" if not found else "gate fired on: " + "; ".join(found))


# ---------------------------------------------------------------------------
# The negative half — the gate must REFUSE. Control 3.
# ---------------------------------------------------------------------------


def t01_rejects_a_float_speed_field() -> None:
    """The obvious one: a speed cap on a public struct field."""
    expect_rejected(
        "t01_rejects_a_float_speed_field",
        """
        pub struct ProposalContext {
            pub preferred: String,
            pub speed_mps: f64,
        }
        """,
        "`speed_mps: f64`",
    )


def t02_rejects_an_integer_quantity_in_disguise() -> None:
    """THE ONE A FLOAT-ONLY BAN WOULD MISS.

    `speed_mm_s: u32` is a bound expressed in integer millimetres. It is the
    more likely accident than a bare f64, because someone reaching for integer
    units is usually trying to be careful — and a float-only gate would wave it
    straight through with a clean bill of health.
    """
    expect_rejected(
        "t02_rejects_an_integer_quantity_in_disguise",
        """
        pub struct ProposalContext {
            pub speed_mm_s: u32,
        }
        """,
        "`speed_mm_s: u32` — the integer disguise",
    )


def t03_rejects_a_magnitude_inside_a_container() -> None:
    """`Vec<f64>` carries magnitudes just as plainly as a bare `f64`."""
    expect_rejected(
        "t03_rejects_a_magnitude_inside_a_container",
        """
        pub struct ProposalContext {
            pub corridor_widths_m: Vec<f64>,
        }
        """,
        "`Vec<f64>`",
    )


def t04_rejects_a_magnitude_inside_an_option() -> None:
    expect_rejected(
        "t04_rejects_a_magnitude_inside_an_option",
        """
        pub struct ProposalContext {
            pub stopping_distance_m: Option<f64>,
        }
        """,
        "`Option<f64>`",
    )


def t05_rejects_a_tuple_struct_payload() -> None:
    """A newtype is still a seam value."""
    expect_rejected(
        "t05_rejects_a_tuple_struct_payload",
        """
        pub struct MaxSpeed(f64);
        """,
        "a tuple-struct `f64` payload",
    )


def t06_rejects_a_tuple_enum_variant_payload() -> None:
    """Enum payloads are public with the enum, so they cross the seam too."""
    expect_rejected(
        "t06_rejects_a_tuple_enum_variant_payload",
        """
        pub enum ContextHint {
            PreferDestination(String),
            MaxSpeedMps(f64),
        }
        """,
        "an enum variant carrying `f64`",
    )


def t07_rejects_a_named_enum_variant_field() -> None:
    expect_rejected(
        "t07_rejects_a_named_enum_variant_field",
        """
        pub enum ContextHint {
            Envelope { lateral_accel_mps2: f64 },
        }
        """,
        "a struct-variant field carrying `f64`",
    )


def t08_rejects_a_private_looking_field_on_a_public_type() -> None:
    """Privacy is not the property.

    A private field still reaches a planner through an accessor, and accessors
    are how this crate exposes everything. The gate checks what the TYPE can
    hold, not what the field's visibility keyword says.
    """
    expect_rejected(
        "t08_rejects_a_private_looking_field_on_a_public_type",
        """
        pub struct ProposalContext {
            speed_cap_mps: f64,
        }
        """,
        "a private `f64` field on a public type",
    )


def t09_rejects_usize_and_isize() -> None:
    expect_rejected(
        "t09_rejects_usize_and_isize",
        """
        pub struct ProposalContext {
            pub wheelbase_mm: usize,
        }
        """,
        "`usize`",
    )


def t16_rejects_a_bound_declared_after_a_test_module() -> None:
    """The `#[cfg(test)]` exit condition must actually fire.

    The first scanner reset on `depth < test_mod_depth`, which can never be true:
    `test_mod_depth` is the depth AT the attribute, the module body raises depth
    by one, and closing it returns depth to exactly that value — never below. So
    one `#[cfg(test)]` anywhere in a file silenced the scanner for the rest of it,
    and a public type declared afterwards was never examined.

    Today's crate happens to put its test module last, so nothing slipped. That
    is luck, not a property, and this case removes the luck.
    """
    expect_rejected(
        "t16_rejects_a_bound_declared_after_a_test_module",
        """
        #[cfg(test)]
        mod tests {
            pub struct Fixture {
                pub whatever: f64,
            }
        }

        pub struct ProposalContext {
            pub speed_mps: f64,
        }
        """,
        "a bound declared AFTER a `#[cfg(test)]` module",
    )


def t17_rejects_a_const_generic_on_the_declaration_line() -> None:
    """A magnitude can ride the declaration itself, not only a field.

    `pub struct Ctx<const N: usize>` carries a numeric across the seam without a
    single numeric token appearing on any field line. Same lesson as the
    struct-variant hole: scan the text, do not enumerate the shapes.
    """
    expect_rejected(
        "t17_rejects_a_const_generic_on_the_declaration_line",
        """
        pub struct ProposalContext<const N: usize> {
            hints: [ContextHint; 4],
        }
        """,
        "`<const N: usize>` on the declaration line",
    )


def t18_rejects_a_defaulted_numeric_type_parameter() -> None:
    expect_rejected(
        "t18_rejects_a_defaulted_numeric_type_parameter",
        """
        pub struct ProposalContext<T = u32> {
            value: T,
        }
        """,
        "a defaulted numeric type parameter",
    )


# ---------------------------------------------------------------------------
# The positive half — the intended design must remain writable.
# ---------------------------------------------------------------------------


def t10_accepts_the_symbolic_shape() -> None:
    """Identities, relations, ordering. The design the rule permits."""
    expect_accepted(
        "t10_accepts_the_symbolic_shape",
        """
        pub struct ContextId(String);

        pub enum ContextHint {
            PreferDestination(ContextId),
            AvoidTask(ContextId),
            CandidatePriority(Vec<ContextId>),
            MissionFact { subject: ContextId, relation: ContextId, object: ContextId },
        }

        pub struct ProposalContext {
            hints: Vec<ContextHint>,
        }
        """,
    )


def t11_accepts_prose_that_merely_names_the_forbidden_types() -> None:
    """LOAD-BEARING, not politeness.

    The guarded crate's own module docs discuss speed caps, corridor widths and
    `f64` at length — that is where the rule is explained. A gate that counted
    its own rationale as a violation would fire on the very file arguing for it.
    """
    expect_accepted(
        "t11_accepts_prose_that_merely_names_the_forbidden_types",
        """
        // A checker bound is an f64 with units: speed_mps, corridor_width_m,
        // stopping_distance_m. None of them may cross this seam.
        pub struct ProposalContext {
            hints: Vec<ContextHint>, // never Vec<f64>
        }
        """,
    )


def t12_accepts_numerics_inside_a_cfg_test_module() -> None:
    """Test code is not the seam. A fixture may compute with numbers freely."""
    expect_accepted(
        "t12_accepts_numerics_inside_a_cfg_test_module",
        """
        pub struct ProposalContext {
            hints: Vec<ContextHint>,
        }

        #[cfg(test)]
        mod tests {
            pub struct Fixture {
                pub speed_mps: f64,
            }
        }
        """,
    )


def t13_accepts_bool_and_char_categorical_state() -> None:
    """Categorical state is explicitly permitted — neither can say HOW MUCH."""
    expect_accepted(
        "t13_accepts_bool_and_char_categorical_state",
        """
        pub struct ProposalContext {
            pub is_urgent: bool,
        }
        """,
    )


# ---------------------------------------------------------------------------
# The gate's own non-vacuity, one level up.
# ---------------------------------------------------------------------------


def t14_the_real_crate_passes_its_own_gate() -> None:
    """The shipped seam is symbolic. If this ever fails, the rule was broken."""
    root = CI.parent
    src = root / "crates" / "kirra-proposal-context" / "src"
    found: list[str] = []
    for path in sorted(src.rglob("*.rs")):
        found.extend(scan_source(path.read_text(encoding="utf-8"), str(path.name)))
    record(
        not found,
        "t14_the_real_crate_passes_its_own_gate",
        "" if not found else "; ".join(found),
    )


def t15_a_planted_bound_in_the_real_crate_would_be_caught() -> None:
    """Mutation control: take the REAL source, plant one bound, expect a catch.

    t14 alone cannot distinguish "the crate is symbolic" from "the scanner does
    not understand this crate's syntax and found nothing anywhere". This does:
    the same scanner, the same file, one field added.
    """
    root = CI.parent
    real = (root / "crates" / "kirra-proposal-context" / "src" / "lib.rs").read_text(
        encoding="utf-8"
    )
    mutated = real.replace(
        "pub struct ProposalContext {\n    hints: Vec<ContextHint>,\n}",
        "pub struct ProposalContext {\n    hints: Vec<ContextHint>,\n    speed_cap_mps: f64,\n}",
    )
    changed = mutated != real
    found = scan_source(mutated, "lib.rs")
    record(
        changed and bool(found),
        "t15_a_planted_bound_in_the_real_crate_would_be_caught",
        ""
        if changed and found
        else (
            "the mutation did not apply — the anchor text moved"
            if not changed
            else "the scanner missed a planted f64 in the real source"
        ),
    )


ALL = [v for k, v in sorted(globals().items()) if k.startswith("t") and k[1:3].isdigit()]

# A case that is not COLLECTED is the most literal form of a test that cannot
# fail. Three were written as `t0a`/`t0b`/`t0c`, which the `t` + two-digit filter
# above silently skipped -- they sat in the file looking like coverage and ran
# never. This makes that impossible: every callable named `t<something>` must end
# up in ALL, or the suite refuses to run.
_UNCOLLECTED = [
    k
    for k, v in globals().items()
    if k.startswith("t") and callable(v) and getattr(v, "__module__", None) == __name__ and v not in ALL
]


def main() -> int:
    if _UNCOLLECTED:
        print(f"FAIL: these cases are never run: {sorted(_UNCOLLECTED)}")
        print("      names must be `t` + two digits to be collected")
        return 1
    for fn in ALL:
        try:
            fn()
        except Exception as exc:  # a crashing test is a failing test
            record(False, fn.__name__, f"raised {type(exc).__name__}: {exc}")

    failed = [r for r in RESULTS if not r[0]]
    for ok, name, detail in RESULTS:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
        if not ok and detail:
            print(textwrap.indent(detail, "        "))
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
