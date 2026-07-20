#!/usr/bin/env python3
"""A4 (#1049) — version-ordering guard.

The Kirra version line restarted at **1.1.2** after the first public release
shipped as **1.5.0** under the retired "Aegis" brand, so a naive semver sort
orders the current line *before* the retired one
(`docs/VERSIONING_POLICY.md` §2.1). That historic inversion is a documentation
fact the CHANGELOG timeline resolves for humans — but nothing stopped it from
recurring or going silent. This guard makes it machine-checked:

  1. **Forward ratchet.** The root crate version MUST be >= `ci/version_floor.txt`.
     Raise the floor in lock-step with every version bump; a *backward* bump
     (the exact defect that produced 1.5.0 -> 1.1.2) reds here.

  2. **Inversion never silent.** While the version is still below the
     retired-brand high-water (1.5.0), `docs/VERSIONING_POLICY.md` MUST carry the
     §2.1 acknowledgment of the renumbering, so the ambiguity is documented, not
     latent. The day the version reaches >= 2.0.0 the ambiguity is permanently
     cleared (every active version is then > 1.5) and this arm simply asserts
     that — belt-and-suspenders that the disambiguating MAJOR cut did its job.

Exit non-zero on any violation. Pure filesystem + string checks (no network,
no cargo); safe to run in any lane.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CARGO_TOML = REPO / "Cargo.toml"
FLOOR_FILE = REPO / "ci" / "version_floor.txt"
POLICY = REPO / "docs" / "VERSIONING_POLICY.md"

# The retired-brand high-water the current line restarted below (§2.1).
AEGIS_HIGH_WATER = (1, 5, 0)
# The MAJOR that permanently clears the inversion (every active version > 1.5).
DISAMBIGUATING_MAJOR = (2, 0, 0)


def parse_semver(text: str) -> tuple[int, int, int]:
    """Parse MAJOR.MINOR.PATCH, ignoring any -pre / +build suffix."""
    core = text.strip().split("+", 1)[0].split("-", 1)[0]
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", core)
    if not m:
        raise ValueError(f"not a MAJOR.MINOR.PATCH version: {text!r}")
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)))


def root_version() -> tuple[int, int, int]:
    # The FIRST `version = "..."` under the root `[package]` table.
    in_package = False
    for line in CARGO_TOML.read_text().splitlines():
        s = line.strip()
        if s.startswith("[") and s.endswith("]"):
            in_package = s == "[package]"
            continue
        if in_package:
            m = re.match(r'version\s*=\s*"([^"]+)"', s)
            if m:
                return parse_semver(m.group(1))
    raise SystemExit("check_version_ordering: no [package].version in root Cargo.toml")


def main() -> int:
    errors: list[str] = []

    version = root_version()
    floor = parse_semver(FLOOR_FILE.read_text())
    vstr = ".".join(map(str, version))
    fstr = ".".join(map(str, floor))

    # 1. Forward ratchet — never regress below the recorded floor.
    if version < floor:
        errors.append(
            f"root crate version {vstr} is BELOW the recorded floor {fstr} "
            f"(ci/version_floor.txt) — a backward version bump. This is exactly the "
            f"1.5.0 -> 1.1.2 defect A4 (#1049) exists to prevent. Raise the version, "
            f"or (only for a deliberate correction) raise the floor in lock-step."
        )

    # 2. The known cross-brand inversion must stay documented until the MAJOR clears it.
    policy_text = POLICY.read_text() if POLICY.exists() else ""
    acknowledged = ("1.5.0" in policy_text) and ("1.1.2" in policy_text)

    if version < AEGIS_HIGH_WATER:
        if not acknowledged:
            errors.append(
                "the version line still sorts below the retired 1.5.0 'Aegis' "
                "high-water, but docs/VERSIONING_POLICY.md no longer acknowledges the "
                "1.5.0 -> 1.1.2 renumbering (§2.1). The inversion must never be silent — "
                "restore the acknowledgment or cut the disambiguating MAJOR (>= 2.0.0)."
            )
        else:
            print(
                f"OK: version {vstr} >= floor {fstr}; the documented 1.5.0->1.1.2 "
                f"inversion (§2.1) is still in effect — cut >= 2.0.0 to clear it permanently."
            )
    elif version >= DISAMBIGUATING_MAJOR:
        print(
            f"OK: version {vstr} >= floor {fstr}; the disambiguating MAJOR has cleared "
            f"the 1.5.0 inversion permanently (every active version is now > 1.5)."
        )
    else:
        # 1.5.0 <= version < 2.0.0 — unreachable from the 1.1.2 line, but monotone.
        print(f"OK: version {vstr} >= floor {fstr}; at/above the 1.5.0 high-water.")

    if errors:
        for e in errors:
            print(f"::error::version-ordering gate: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
