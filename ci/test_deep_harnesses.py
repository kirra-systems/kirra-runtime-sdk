#!/usr/bin/env python3
"""Tests for `ci/deep_harnesses.py` — the weekly deep lane's matrix source.

This script decides WHICH PROPERTIES GET PROVED. If it silently returns too few
names, the lane goes green having verified less than it claims — the failure
mode #1260 is about. Two behaviours are therefore pinned here rather than left
to inspection, because both were live hazards during that investigation:

  1. BOTH gate spellings are recognised. K7 uses `cfg(all(kani, feature = ...))`
     while its siblings use `cfg(feature = ...)`. A grep for the second spelling
     alone drops K7 — the harness that ate an entire six-hour run.
  2. `deep-proofs`-gated HELPERS are excluded. `proofs_rss.rs` gates
     `grid_speed` and `grid_param` behind the same feature; neither is a
     `#[kani::proof]`, and passing either to `cargo kani --harness` fails.

Run: python3 ci/test_deep_harnesses.py
"""
from __future__ import annotations

import pathlib
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from deep_harnesses import harnesses_in  # noqa: E402

CI = pathlib.Path(__file__).resolve().parent
REPO = CI.parent

_FAILURES: list[str] = []


def check(cond: bool, label: str) -> None:
    print(f"  {'ok  ' if cond else 'FAIL'} - {label}")
    if not cond:
        _FAILURES.append(label)


print("== 1. both gate spellings are recognised ==")
plain = """
    #[cfg(feature = "deep-proofs")]
    #[kani::proof]
    fn k3_plain_gate() {}
"""
nested = """
    #[cfg(all(kani, feature = "deep-proofs"))]
    #[kani::proof]
    #[kani::stub(f64::tan, stub_tan)]
    fn k7_nested_gate() {}
"""
check(harnesses_in(plain) == ["k3_plain_gate"], "cfg(feature = ...) is found")
check(harnesses_in(nested) == ["k7_nested_gate"],
      "cfg(all(kani, feature = ...)) is found — the spelling a naive grep drops")

print("== 2. stub attributes between the gate and the fn do not break parsing ==")
interleaved = """
    #[cfg(all(kani, feature = "deep-proofs"))]
    #[kani::proof]
    #[kani::stub(f64::powi, stub_powi)]
    #[kani::stub(f64::tan, stub_tan)]
    #[kani::stub(f64::atan, stub_atan)]
    fn many_stubs() {}
"""
check(harnesses_in(interleaved) == ["many_stubs"], "attribute block is walked to the fn")

print("== 3. deep-gated NON-proofs are excluded ==")
helper = """
    #[cfg(all(kani, feature = "deep-proofs"))]
    fn grid_speed(i: u8) -> f64 { 0.0 }
"""
check(harnesses_in(helper) == [], "a deep-gated helper without #[kani::proof] is not a harness")

print("== 4. non-deep proofs are excluded ==")
shallow = """
    #[cfg(kani)]
    #[kani::proof]
    fn k6_per_pr_harness() {}
"""
check(harnesses_in(shallow) == [], "a per-PR harness is not pulled into the deep matrix")

print("== 5. against the REAL source tree ==")
out = subprocess.run([sys.executable, str(CI / "deep_harnesses.py")],
                     capture_output=True, text=True, cwd=REPO)
check(out.returncode == 0, f"script exits 0 (got {out.returncode}: {out.stderr.strip()[:120]})")
names = out.stdout.strip()
for expected in ("k3_speed_ceiling_clamp_exact",
                 "r2_longitudinal_monotone_in_closing_speed_on_grid"):
    check(expected in names, f"{expected} is in the matrix")
for helper_name in ("grid_speed", "grid_param"):
    check(helper_name not in names, f"helper {helper_name} is NOT in the matrix")

# K7 was DEMOTED OUT OF THE PROOF TIER (#1268) and lives behind
# `outside-proof-envelope`, which no lane enables. Pinned by name rather than
# left to the gate spelling, because re-adding it would silently reintroduce a
# harness that cannot finish — it stalls in propositional conversion in
# multi-property mode AND is solver-hard when isolated to its two assertions.
# Its per-PR enforcement is the 306,180-point grid, which is unaffected.
check("k7_executable_return_respects_the_rack_limit" not in names,
      "K7 is NOT in the weekly matrix — demoted out of the proof tier (#1268)")
check("k8_executable_return_respects_the_rate_bound" not in names,
      "K8 is NOT in the weekly matrix — demoted out of the proof tier (#1260)")
# R2 must STAY. It is the one harness that fails purely solver-side (it reaches
# the solver rather than stalling in conversion) on a formula 4x smaller, so it
# is a budget/solver question with a live answer — not a modelling one. Pinned
# positively so a future sweep cannot demote it alongside its neighbours.
check("r2_longitudinal_monotone_in_closing_speed_on_grid" in names,
      "R2 REMAINS in the weekly matrix — solver-bound, not diagnosed intractable")
check("outside-proof-envelope" in (CI.parent / "verification" / "kani"
                                   / "Cargo.toml").read_text(encoding="utf-8"),
      "the outside-proof-envelope feature still exists to hold K7's source")

print("== 6. fail-closed on an empty deep tier ==")
# A vacuous lane passes while proving nothing. Point the script at a tree with
# no deep harnesses and require a non-zero exit rather than `[]`.
import tempfile  # noqa: E402
import textwrap  # noqa: E402

with tempfile.TemporaryDirectory() as td:
    fake = pathlib.Path(td) / "verification" / "kani" / "src"
    fake.mkdir(parents=True)
    (fake / "proofs.rs").write_text(textwrap.dedent("""
        #[cfg(kani)]
        #[kani::proof]
        fn only_a_per_pr_harness() {}
    """), encoding="utf-8")
    shim = pathlib.Path(td) / "deep_harnesses.py"
    shim.write_text((CI / "deep_harnesses.py").read_text(encoding="utf-8"), encoding="utf-8")
    # The script resolves KANI_SRC relative to its own location's parent.
    (pathlib.Path(td) / "ci").mkdir()
    (pathlib.Path(td) / "ci" / "deep_harnesses.py").write_text(
        (CI / "deep_harnesses.py").read_text(encoding="utf-8"), encoding="utf-8")
    empty = subprocess.run(
        [sys.executable, str(pathlib.Path(td) / "ci" / "deep_harnesses.py")],
        capture_output=True, text=True)
    check(empty.returncode != 0, "an empty deep tier is an ERROR, not an empty matrix")
    check("Refusing to emit an empty matrix" in empty.stderr,
          "the refusal explains itself")

if _FAILURES:
    print(f"\n== deep_harnesses: {len(_FAILURES)} FAILED ==")
    for f in _FAILURES:
        print(f"   - {f}")
    raise SystemExit(1)
print("\n== deep_harnesses: all checks passed ==")
