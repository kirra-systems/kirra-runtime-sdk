#!/usr/bin/env python3
"""EP-07 — gate a checker crate's DECISION coverage on a floor.

Reads a `cargo llvm-cov --json --summary-only` export and fails (exit 1) when
the strongest decision metric the toolchain produced is below the floor:

  - if the export carries an MC/DC summary (`totals.mcdc`), gate on it — this
    is the auto-upgrade path for the day rustc restores `-Zcoverage-options=mcdc`
    (see issue #65: current nightly accepts only block|branch|condition);
  - otherwise gate on BRANCH coverage (`totals.branches`) — decision coverage,
    the strongest gate the toolchain can express today.

Usage: check_decision_floor.py <summary.json> <floor_percent> <label>
"""

import json
import sys


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    path, floor_s, label = sys.argv[1], sys.argv[2], sys.argv[3]
    floor = float(floor_s)

    with open(path, encoding="utf-8") as f:
        export = json.load(f)
    totals = export["data"][0]["totals"]

    if "mcdc" in totals and totals["mcdc"].get("count", 0) > 0:
        metric, name = totals["mcdc"], "MC/DC"
    elif "branches" in totals and totals["branches"].get("count", 0) > 0:
        metric, name = totals["branches"], "branch"
    else:
        print(
            f"FAIL [{label}]: the coverage export carries neither an MC/DC nor a "
            "branch summary — the lane is not measuring decision coverage at all "
            "(instrumentation flag regression?)",
            file=sys.stderr,
        )
        return 1

    pct = float(metric["percent"])
    covered = metric.get("covered", "?")
    count = metric.get("count", "?")
    print(
        f"[{label}] {name} coverage: {pct:.2f}% ({covered}/{count}) — floor {floor:.2f}%"
    )
    if pct < floor:
        print(
            f"FAIL [{label}]: {name} coverage {pct:.2f}% is below the gated floor "
            f"{floor:.2f}%. Either cover the new decisions or (with reviewer sign-off) "
            "adjust the floor in ci.yml.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
