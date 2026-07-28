#!/usr/bin/env python3
"""Guard: `unsafe` in `kirra-r2cp` lives ONLY in the OS-binding ioctls.

The crate is `deny(unsafe_code)` with narrow `#[allow]`s, and the value of that
arrangement is entirely in how narrow it stays. "Only in link.rs, only the
ioctls" is exactly the kind of boundary that erodes one convenient exception at
a time — each individually reasonable, the sum not.

So it is checked rather than trusted:

  * `sim.rs`, `handshake.rs`, `lib.rs`, `pty.rs` must contain NO `unsafe` block.
    These are the codec, the framing, the handshake evaluator and the PTY
    binding — the parts a differential test compares against the firmware's
    normative `wire.cpp`, and the parts a reviewer reads for safety logic.
  * `link.rs` may contain unsafe only inside the three named ioctl wrappers,
    each of which must carry a SAFETY comment.

Run: python3 ci/check_r2cp_unsafe.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "crates" / "kirra-r2cp" / "src"

# Files that must be entirely free of unsafe blocks.
PURE = ("lib.rs", "sim.rs", "handshake.rs", "pty.rs")

# The only functions allowed to contain one, and what each is for.
ALLOWED_IOCTL_FNS = {
    "claim_exclusive": "TIOCEXCL",
    "read_exclusive": "TIOCGEXCL",
    "release_exclusive": "TIOCNXCL",
}

# `unsafe {` as a block opener. Deliberately not matching the word `unsafe` in
# prose: every one of these files DOCUMENTS the arrangement, and a substring
# search would fire on the explanation of why there is no unsafe there.
UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")
FN_DEF = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")


def enclosing_fn(lines: list[str], index: int) -> str | None:
    """Name of the nearest preceding `fn` definition."""
    for i in range(index, -1, -1):
        m = FN_DEF.match(lines[i])
        if m:
            return m.group(1)
    return None


def check() -> list[str]:
    problems: list[str] = []

    for name in PURE:
        path = SRC / name
        if not path.is_file():
            # A missing file is a failure, not a skip: a rename must not
            # silently remove a file from the guard's scope.
            problems.append(f"{name}: guarded file is missing")
            continue
        for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if UNSAFE_BLOCK.search(line):
                problems.append(
                    f"{name}:{n}: unsafe block outside the OS binding — the "
                    "codec, framing, handshake and PTY layers must stay safe"
                )

    link = SRC / "link.rs"
    if not link.is_file():
        problems.append("link.rs: missing")
        return problems

    lines = link.read_text(encoding="utf-8").splitlines()
    seen: set[str] = set()
    for n, line in enumerate(lines, 1):
        if not UNSAFE_BLOCK.search(line):
            continue
        fn = enclosing_fn(lines, n - 1)
        if fn not in ALLOWED_IOCTL_FNS:
            problems.append(
                f"link.rs:{n}: unsafe inside {fn!r}, which is not one of the "
                f"permitted ioctl wrappers {sorted(ALLOWED_IOCTL_FNS)}"
            )
            continue
        seen.add(fn)
        # Every exception must say why it is sound, near where it is taken.
        window = "\n".join(lines[max(0, n - 8) : n])
        if "SAFETY:" not in window:
            problems.append(
                f"link.rs:{n}: unsafe in {fn!r} has no SAFETY comment within "
                "the preceding 8 lines"
            )

    missing = set(ALLOWED_IOCTL_FNS) - seen
    if missing:
        # Non-vacuity: if the ioctls vanished, every check above would pass
        # trivially and the exclusivity guarantee would be gone with them.
        problems.append(
            f"link.rs: expected unsafe ioctl wrapper(s) {sorted(missing)} not "
            "found — exclusivity may no longer be claimed at all"
        )
    return problems


def main() -> int:
    problems = check()

    # Self-test: the detector must be able to fail. A guard that cannot
    # distinguish a violation from clean code proves nothing when it passes.
    probe_lines = ["fn something_else() {", "    let x = unsafe { foo() };", "}"]
    if enclosing_fn(probe_lines, 1) != "something_else" or not UNSAFE_BLOCK.search(
        probe_lines[1]
    ):
        problems.append("SELF-TEST: the detector cannot recognise a violation")

    for p in problems:
        print(f"  FAIL {p}")
    if problems:
        print(f"\n{len(problems)} problem(s)")
        return 1
    print(f"  ok   {len(PURE)} codec/framing file(s) contain no unsafe block")
    print(f"  ok   link.rs unsafe confined to {len(ALLOWED_IOCTL_FNS)} ioctl "
          "wrappers, each with a SAFETY comment")
    print("  ok   the detector recognises a violation (non-vacuous)")
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
