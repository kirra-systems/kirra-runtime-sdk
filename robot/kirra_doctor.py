#!/usr/bin/env python3
"""kirra_doctor — the R2 diagnostics orchestrator (read-only observability).

Discovers the independent modules in robot/doctor/modules/, runs them in
parallel with per-module isolation (one module failing/hanging never stops the
others), aggregates a schema-v1 report, and renders it human- or
machine-readable. See docs/diagnostics.md for the architecture and schema.

  kirra_doctor.py                 # default module set, human report
  kirra_doctor.py --json          # full schema-v1 JSON on stdout
  kirra_doctor.py --summary       # the short spoken/logged summary line(s)
  kirra_doctor.py --verbose       # include PASS details
  kirra_doctor.py --timings       # per-module elapsed ms
  kirra_doctor.py --module voice --module devices   # just these (incl. opt-in)
  kirra_doctor.py --all           # default set + the opt-in heavy modules
  kirra_doctor.py --list          # module inventory
  kirra_doctor.py --output r.json # also write the JSON report to a file
  kirra_doctor.py --serial        # disable parallel execution

Exit codes: 0 healthy · 1 warnings/unverifiable · 2 failures · 3 internal error.

🔴 Read-only. Never a safety authority: no writes to robot state, no actuation,
no /intent — and no verdict here ever gates the planner/governor/checker/fence.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from doctor import core  # noqa: E402


def select_modules(mods, names=None, include_all=False):
    """Explicit --module names win (and may include opt-in modules); otherwise
    the DEFAULT set, plus the opt-ins under --all. Unknown names are an error
    (exit 3) — a typo'd module silently skipped would be a lie of omission."""
    if names:
        by_name = {m.NAME: m for m in mods}
        missing = [n for n in names if n not in by_name]
        if missing:
            raise KeyError(f"unknown module(s): {', '.join(missing)} "
                           f"(known: {', '.join(sorted(by_name))})")
        return [by_name[n] for n in sorted(set(names))]
    return [m for m in mods if include_all or getattr(m, "DEFAULT", True)]


def collect(module_names=None, include_all=False, parallel=True):
    """Programmatic entry (rabbit_boot / rabbit_diag import this): run and
    return the schema-v1 report dict."""
    mods = select_modules(core.discover_modules(), module_names, include_all)
    return core.run_all(mods, parallel=parallel)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="kirra_doctor", description=__doc__.splitlines()[0])
    ap.add_argument("--json", action="store_true", help="full JSON report on stdout")
    ap.add_argument("--summary", action="store_true", help="short summary only")
    ap.add_argument("--verbose", action="store_true", help="include PASS details")
    ap.add_argument("--timings", action="store_true", help="per-module elapsed ms")
    ap.add_argument("--module", action="append", metavar="NAME",
                    help="run only NAME (repeatable; may name opt-in modules)")
    ap.add_argument("--all", action="store_true", help="include opt-in heavy modules")
    ap.add_argument("--list", action="store_true", help="list modules and exit")
    ap.add_argument("--output", metavar="PATH", help="also write the JSON report to PATH")
    ap.add_argument("--serial", action="store_true", help="disable parallel execution")
    args = ap.parse_args(argv)

    try:
        mods = core.discover_modules()
        if args.list:
            for m in mods:
                tag = "default" if getattr(m, "DEFAULT", True) else "opt-in"
                tag += ", heavy" if getattr(m, "HEAVY", False) else ""
                print(f"{m.NAME:<12} [{tag}]  {getattr(m, 'DESCRIPTION', '')}")
            return 0
        selected = select_modules(mods, args.module, args.all)
        report = core.run_all(selected, parallel=not args.serial)
    except KeyError as e:
        print(f"kirra_doctor: {e}", file=sys.stderr)
        return core.EXIT_INTERNAL
    except Exception as e:  # noqa: BLE001 — orchestrator's own failure = exit 3
        print(f"kirra_doctor: internal error: {e}", file=sys.stderr)
        return core.EXIT_INTERNAL

    if args.output:
        try:
            with open(args.output, "w", encoding="utf-8") as f:
                json.dump(report, f, indent=2, sort_keys=True)
        except OSError as e:
            print(f"kirra_doctor: could not write {args.output}: {e}", file=sys.stderr)

    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    elif args.summary:
        print(core.speech_summary(report))
    else:
        print(core.human_report(report, verbose=args.verbose, timings=args.timings))
    return core.EXIT_FOR[report["status"]]


if __name__ == "__main__":
    sys.exit(main())
