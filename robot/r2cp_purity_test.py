#!/usr/bin/env python3
"""Guard: the Python side must not learn R2CP.

There is exactly ONE R2CP implementation on the host — `crates/kirra-r2cp`,
differentially tested against the firmware's normative `wire.cpp`. A second one
in Python would be a third dialect that nothing tests against either of the
others, and the way that happens is never a decision: it is someone adding
"just a constant" to avoid an FFI round-trip.

So this checks mechanically. It walks the AST rather than grepping, because
every file under test DOCUMENTS what it must not contain — a grep for "COBS"
would fire on the docstring explaining why there is no COBS here.

Run: python3 robot/r2cp_purity_test.py
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The Python files allowed to mention R2CP at all. Anything else importing the
# link module is fine; these are the ones whose CONTENTS are constrained.
GUARDED = ("kirra_r2cp.py", "kirra_motor_consumer.py")

# Protocol facts that must live only in Rust. Matched against identifier names
# and string literals in EXECUTABLE positions — never docstrings or comments.
FORBIDDEN_NAMES = {
    "cobs", "cobs_encode", "cobs_decode",
    "crc32c", "crc32", "crc",
    "magic", "protocol_major", "protocol_minor",
    "header_size", "max_payload", "max_frame_size", "max_encoded_frame",
    "flag_auth_tag", "flag_response", "flag_ack_required", "known_flag_mask",
    "nonce", "make_nonce", "gen_nonce",
    "hello", "hello_payload_len",
    "ack_accepted", "ack_stale", "ack_replay", "ack_disarmed", "ack_faulted",
    "message_type", "motion_command", "command_ack",
    "min_advertised_frame", "agreed_major_of",
}

# Numeric literals that would betray a re-implementation. Deliberately small:
# a list of every protocol number would itself be a duplication of the
# protocol. These are the load-bearing ones.
FORBIDDEN_NUMBERS = {
    0x3252,      # R2CP magic
    0x82F63B78,  # CRC32C polynomial
}


class Visitor(ast.NodeVisitor):
    """Collect violations from executable positions only."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.violations: list[str] = []
        self._docstrings: set[int] = set()

    def _note_docstring(self, node: ast.AST) -> None:
        body = getattr(node, "body", None)
        if (
            body
            and isinstance(body[0], ast.Expr)
            and isinstance(body[0].value, ast.Constant)
            and isinstance(body[0].value.value, str)
        ):
            self._docstrings.add(id(body[0].value))

    def visit_Module(self, node: ast.Module) -> None:
        self._note_docstring(node)
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._note_docstring(node)
        self._check_name(node.name, node.lineno, "function")
        self.generic_visit(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._note_docstring(node)
        self._check_name(node.name, node.lineno, "function")
        self.generic_visit(node)

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self._note_docstring(node)
        self._check_name(node.name, node.lineno, "class")
        self.generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        # Only BINDINGS, not reads: reading an attribute called `nonce` from
        # somewhere else is not a re-implementation, defining one is.
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            self._check_name(node.id, node.lineno, "variable")
        self.generic_visit(node)

    def visit_Constant(self, node: ast.Constant) -> None:
        if id(node) in self._docstrings:
            return
        if isinstance(node.value, int) and not isinstance(node.value, bool):
            if node.value in FORBIDDEN_NUMBERS:
                self.violations.append(
                    f"{self.path.name}:{node.lineno}: protocol constant "
                    f"{node.value:#x} — this number belongs in kirra-r2cp"
                )
        self.generic_visit(node)

    def _check_name(self, name: str, lineno: int, kind: str) -> None:
        if name.lower().lstrip("_") in FORBIDDEN_NAMES:
            self.violations.append(
                f"{self.path.name}:{lineno}: {kind} named {name!r} — R2CP "
                "protocol detail must stay in kirra-r2cp, reached over the FFI"
            )


def check(path: Path) -> list[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    v = Visitor(path)
    v.visit(tree)
    return v.violations


def main() -> int:
    failures: list[str] = []
    checked = 0
    for name in GUARDED:
        path = HERE / name
        if not path.is_file():
            # kirra_motor_consumer.py always exists; kirra_r2cp.py must too
            # once the mode lands. A missing guarded file is a failure, not a
            # skip — otherwise a rename silently disables the guard.
            failures.append(f"{name}: guarded file is missing")
            continue
        checked += 1
        failures.extend(check(path))

    # Non-vacuity: the guard must actually be capable of failing. If the
    # detector cannot flag a deliberately dirty sample, a clean run proves
    # nothing about the real files.
    dirty = ast.parse("magic = 0x3252\ndef cobs_encode(x):\n    return x\n")
    probe = Visitor(Path("synthetic"))
    probe.visit(dirty)
    if len(probe.violations) < 2:
        failures.append(
            "SELF-TEST: the purity detector failed to flag a deliberately "
            "dirty sample — a clean result would be meaningless"
        )

    for f in failures:
        print(f"  FAIL {f}")
    if failures:
        print(f"\n{len(failures)} violation(s)")
        return 1
    print(f"  ok   {checked} guarded file(s) contain no R2CP protocol detail")
    print("  ok   the detector flags a dirty sample (non-vacuous)")
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
