#!/usr/bin/env python3
"""MEASUREMENT ONLY — the orphan gate's consumer-attribution blind spot.

    NOT A GATE. Nothing invokes this from CI, and it changes nothing about what
    CI rejects. It exists so the fallout of a STRONGER consumer test can be read
    off the tree before that test is written, per the tier-order rule: measure
    and disposition first, tighten second, so `main` never meets a red wall
    nobody has classified.

    Run: python3 ci/audit_orphan_gate.py
    Report: docs/design/ORPHAN_GATE_FALLOUT.md


Changes NOTHING about what CI rejects. It replicates `check_orphan_cores.is_consumed`
exactly, but RECORDS the evidence instead of short-circuiting on the first hit, then
asks one question the gate never asks:

    could the crediting file possibly name this module at all?

A reference in crate B can only consume crate A's module if B depends on A (or B IS A).
Where no such dependency edge exists, the credit is PROVABLY FALSE — not a judgement
call, a fact about the Cargo graph.
"""
from __future__ import annotations

import json
import os
import re
import sys
import tomllib
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
CI = Path(__file__).resolve().parent
sys.path.insert(0, str(CI))
import check_orphan_cores as G  # noqa: E402

REPO = G.REPO


# ---------------------------------------------------------------------------
# The Cargo graph: which crates can name which
# ---------------------------------------------------------------------------
def crate_manifests() -> dict[str, Path]:
    """package-ident -> crate directory, for EVERY crate, lib or bin-only.

    Keying off `lib.rs` was wrong and the audit caught itself doing it:
    `kirra-wcet-bench` is bin-only, so its files fell through longest-prefix
    matching to the repo-root crate and its perfectly ordinary
    `kirra_timing::evt::estimate_pwcet(..)` call was reported as unattributable.
    A measurement instrument that mis-attributes is worth less than no
    instrument, so ownership is read from manifests, not from lib targets.
    """
    out: dict[str, Path] = {}
    for man in list(REPO.glob("Cargo.toml")) + list(REPO.glob("*/Cargo.toml")) \
            + list(REPO.glob("crates/*/Cargo.toml")) + list(REPO.glob("parko/crates/*/Cargo.toml")) \
            + list(REPO.glob("parko/Cargo.toml")):
        try:
            data = tomllib.loads(man.read_text(encoding="utf-8"))
        except Exception:
            continue
        name = data.get("package", {}).get("name")
        if name:
            out[name.replace("-", "_")] = man.parent
    return out


def dep_edges(crate_dir: Path) -> set[str]:
    """Direct dependency lib-idents declared by this crate (all tables)."""
    man = crate_dir / "Cargo.toml"
    try:
        data = tomllib.loads(man.read_text(encoding="utf-8"))
    except Exception:
        return set()
    names: set[str] = set()
    tables = ["dependencies", "dev-dependencies", "build-dependencies"]
    for t in tables:
        names |= set(data.get(t, {}).keys())
    for target in data.get("target", {}).values():
        for t in tables:
            names |= set(target.get(t, {}).keys())
    return {n.replace("-", "_") for n in names}


def reachable(crates: dict[str, Path]) -> dict[str, set[str]]:
    """crate -> every crate it can name (transitive closure, incl. itself)."""
    direct = {c: dep_edges(d) & set(crates) for c, d in crates.items()}
    out: dict[str, set[str]] = {}
    for c in crates:
        seen, stack = {c}, [c]
        while stack:
            cur = stack.pop()
            for nxt in direct.get(cur, set()):
                if nxt not in seen:
                    seen.add(nxt)
                    stack.append(nxt)
        out[c] = seen
    return out


def owning_crate(path: Path, crates: dict[str, Path]) -> str | None:
    """Which crate a consumer file belongs to (longest matching dir)."""
    best, best_len = None, -1
    for name, d in crates.items():
        s = str(d.resolve())
        if str(path).startswith(s + "/") and len(s) > best_len:
            best, best_len = name, len(s)
    return best


# ---------------------------------------------------------------------------
# Evidence collection — the gate's own rules, recorded rather than short-circuited
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Multi-line `pub use` blocks — the continuation lines the gate's skip misses
# ---------------------------------------------------------------------------
_USE_START = re.compile(r"^\s*(pub\s+)?use\b")


def statement_spans(lines: list[str]) -> dict[int, str]:
    """1-indexed line -> "pub-use" | "use", for lines INSIDE a use statement.

    `REEXPORT_RE` is line-anchored, so it skips `pub use backend_selector::{`
    and then credits the item names rustfmt wrapped onto the next two lines.
    A re-export is a re-export however it is formatted, so the whole statement
    has to be attributed, not just its first line.

    A plain `use` continuation is kept distinct: importing IS consumption, and
    only the `pub` form is the shelf.
    """
    spans: dict[int, str] = {}
    i, n = 0, len(lines)
    while i < n:
        m = _USE_START.match(lines[i])
        if not m:
            i += 1
            continue
        kind = "pub-use" if m.group(1) else "use"
        depth = 0
        j = i
        while j < n:
            depth += lines[j].count("{") - lines[j].count("}")
            spans[j + 1] = kind
            if ";" in lines[j] and depth <= 0:
                break
            j += 1
        i = j + 1
    return spans


def evidence_for(crate, mod_name, lib_rs, files, ambiguous):
    own = {p.resolve() for p in G.module_own_paths(lib_rs, mod_name)}
    use_re = re.compile(rf"^\s*(?:pub\s+)?use\s+[\w:{{}}, ]*\b{mod_name}\b")
    path_re = re.compile(rf"\b{mod_name}::")
    qualified_re = re.compile(rf"\b{crate}::{mod_name}\b")
    items = G.module_pub_items(lib_rs, mod_name) - (ambiguous or set())
    item_re = G.code_reference_re(items)
    hits = []
    for f in files:
        if f in own:
            continue
        flines = G.stripped_lines(f)
        spans = statement_spans(flines)
        for n, line in enumerate(flines, start=1):
            if G.REEXPORT_RE.match(line):
                continue
            rule = None
            if qualified_re.search(line):
                rule = "qualified"          # crate::mod — unambiguous
            elif path_re.search(line):
                rule = "path"              # bare mod:: — NOT crate-scoped
            elif use_re.match(line):
                rule = "use"
            elif item_re:
                for m in item_re.finditer(line):
                    if G.is_code_shaped(line, m.start(1), m.end(1)):
                        rule = "item"
                        break
            if rule:
                hits.append({"file": str(f.relative_to(REPO)), "line": n,
                             "rule": rule, "text": line.strip()[:160],
                             "in_stmt": spans.get(n, "")})
    return hits


def main() -> int:
    mods = G.declared_pub_mods()
    files = G.consumer_files()
    ambiguous = G.ambiguous_item_names(mods)
    crates = crate_manifests()
    reach = reachable(crates)

    report = []
    for crate, mod_name, lib_rs in mods:
        hits = evidence_for(crate, mod_name, lib_rs, files, ambiguous)
        for h in hits:
            consumer = owning_crate((REPO / h["file"]).resolve(), crates)
            h["consumer_crate"] = consumer
            if consumer == crate:
                h["attribution"] = "same-crate"
            elif consumer is None:
                h["attribution"] = "unknown-crate"
            elif crate in reach.get(consumer, set()):
                h["attribution"] = "cross-crate-with-dep"
            else:
                h["attribution"] = "PROVABLY-FALSE"
        attribs = {h["attribution"] for h in hits}
        # Computed over the FULL hit list, never the capped sample written to
        # JSON. A first pass recomputed these from `evidence[:12]` and reported
        # 9 exposures where there are 3 — a module with 200 hits whose first 12
        # happen to be refuted looks exposed while hits 13+ are fine. Recorded
        # because it is the same defect class this audit exists to find: a
        # measurement that reads a truncated sample as the whole population.
        real = [h for h in hits
                if h["in_stmt"] != "pub-use" and h["attribution"] != "PROVABLY-FALSE"]
        real_a = [h for h in hits if h["attribution"] != "PROVABLY-FALSE"]
        real_b = [h for h in hits if h["in_stmt"] != "pub-use"]
        report.append({
            "module": f"{crate}::{mod_name}",
            "crate": crate,
            "consumed_by_gate": bool(hits),
            "n_evidence": len(hits),
            "attributions": sorted(attribs),
            "only_false": bool(hits) and attribs == {"PROVABLY-FALSE"},
            "n_real": len(real),
            "hidden_orphan": bool(hits) and not real,
            "exposed_by_rule_a_only": bool(hits) and not real_a,
            "exposed_by_rule_b_only": bool(hits) and not real_b,
            "evidence": hits[:12],
        })
    out = Path(os.environ.get("ORPHAN_AUDIT_OUT", "/tmp/orphan_audit.json"))
    out.write_text(json.dumps(report, indent=1), encoding="utf-8")

    total = len(report)
    consumed = [r for r in report if r["consumed_by_gate"]]
    orphans = [r for r in report if not r["consumed_by_gate"]]
    only_false = [r for r in consumed if r["only_false"]]
    print(f"modules scanned            {total}")
    print(f"  credited as consumed     {len(consumed)}")
    print(f"  reported orphan today    {len(orphans)}")
    hidden = [r for r in consumed if r["hidden_orphan"]]
    print(f"  credited ONLY by evidence the Cargo graph refutes: {len(only_false)}")
    for r in only_false:
        print(f"    - {r['module']}  ({r['n_evidence']} hits)")
    print(f"\n  HIDDEN ORPHANS — no surviving evidence once refuted credit and")
    print(f"  `pub use` continuation lines are removed:            {len(hidden)}")
    for r in hidden:
        why = sorted({h["in_stmt"] or h["attribution"] for h in r["evidence"]})
        print(f"    - {r['module']:48} {r['n_evidence']:3} hits  {why}")
    by_rule = defaultdict(int)
    by_attr = defaultdict(int)
    for r in consumed:
        for h in r["evidence"]:
            by_rule[h["rule"]] += 1
            by_attr[h["attribution"]] += 1
    a_only = [r["module"] for r in consumed if r["exposed_by_rule_a_only"]]
    b_only = [r["module"] for r in consumed if r["exposed_by_rule_b_only"]]
    print(f"\n  rule A alone (crate attribution) exposes {len(a_only)}: {a_only}")
    print(f"  rule B alone (whole `pub use` stmt) exposes {len(b_only)}: {b_only}")
    print("\nevidence by rule (first 12/module):", dict(by_rule))
    print("evidence by attribution:          ", dict(by_attr))
    print(f"\nfull report: {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
