#!/usr/bin/env python3
"""closure_diff.py — recursive-closure cross-distro .msg wire-compat comparator.

The leaf-only step-3 of crossdistro_hash_check.sh diffs only the curated
top-level autoware_*_msgs .msg. A DIFFERING NESTED base message (e.g.
builtin_interfaces/Time or geometry_msgs/Point) leaves every curated leaf
byte-identical while the RIHS type hash diverges — undetected. This walks the
FULL transitive closure from the seeds across BOTH reference share trees and
byte-compares every message in it, base packages included.

Exit: 0 = whole closure byte-identical across both refs; 1 = drift/missing.
Pure text over two `share/` dirs — no ROS, no rosidl.
"""
import argparse
import os
import sys

PRIMITIVES = {
    "bool", "byte", "char", "float32", "float64", "int8", "int16", "int32",
    "int64", "uint8", "uint16", "uint32", "uint64", "string", "wstring",
}


def resolve(base, cur_pkg):
    """(pkg, Msg) for a field base type, or None for a primitive/unresolved."""
    if base in PRIMITIVES:
        return None
    if "/" in base:
        p, n = base.split("/", 1)
        return (p, n)
    if base == "Header":
        return ("std_msgs", "Header")
    if base in ("Time", "Duration"):
        return ("builtin_interfaces", base)
    if base[:1].isupper():
        return (cur_pkg, base)  # same-package nested type
    return None


def deps_of(msg_path, cur_pkg):
    out = []
    with open(msg_path, "r") as fh:
        for raw in fh:
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            toks = line.split()
            type_tok = toks[0]
            # constant: "Type NAME=value" (second token contains '=')
            if len(toks) >= 2 and "=" in toks[1]:
                continue
            base = type_tok.split("[", 1)[0].split("<", 1)[0]
            r = resolve(base, cur_pkg)
            if r:
                out.append(r)
    return out


def closure(ref_a, seeds):
    """BFS the closure using ref_a as the structure oracle; returns sorted set."""
    seen, queue, missing = set(), list(seeds), []
    while queue:
        pkg, msg = queue.pop(0)
        if (pkg, msg) in seen:
            continue
        seen.add((pkg, msg))
        p = os.path.join(ref_a, pkg, "msg", msg + ".msg")
        if not os.path.isfile(p):
            missing.append((pkg, msg))
            continue
        queue.extend(deps_of(p, pkg))
    return sorted(seen), missing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ref-a", required=True)
    ap.add_argument("--ref-b", required=True)
    ap.add_argument("--seed", action="append", required=True,
                    help="pkg/Msg, repeatable")
    ap.add_argument("--leaf-only", action="store_true",
                    help="compare only the seed leaves (models the OLD gate)")
    args = ap.parse_args()

    seeds = []
    for s in args.seed:
        pkg, msg = s.split("/", 1)
        seeds.append((pkg, msg))

    if args.leaf_only:
        members, missing = list(seeds), []
    else:
        members, missing = closure(args.ref_a, seeds)

    drift = 0
    for pkg, msg in members:
        rel = os.path.join(pkg, "msg", msg + ".msg")
        a = os.path.join(args.ref_a, rel)
        b = os.path.join(args.ref_b, rel)
        if not os.path.isfile(a) or not os.path.isfile(b):
            print(f"MISSING {rel} (absent on a reference)")
            drift = 1
            continue
        with open(a, "rb") as fa, open(b, "rb") as fb:
            if fa.read() == fb.read():
                print(f"MATCH   {rel}")
            else:
                print(f"DRIFT   {rel} (ref-a != ref-b)")
                drift = 1
    for pkg, msg in missing:
        print(f"UNRESOLVED {pkg}/{msg} (not found in ref-a closure walk)")
        drift = 1
    print(f"-- {len(members)} messages in "
          f"{'leaf set' if args.leaf_only else 'closure'} --")
    return 1 if drift else 0


if __name__ == "__main__":
    sys.exit(main())
