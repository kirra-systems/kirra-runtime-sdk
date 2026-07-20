#!/usr/bin/env bash
# closure_diff_selftest.sh — CI-gated proof that closure_diff.py catches a
# NESTED base-message drift the leaf-only step-3 misses (M3, #1042). Pure
# python3 over committed fixture trees — no ROS, so it gates on every PR.
#
# The fixtures under testdata/{humble,jazzy}/ have byte-identical curated leaves
# but a geometry_msgs/Point that differs (jazzy drops `z`). Assertions:
#   1. leaf-only  humble vs jazzy  -> PASS (models the OLD gate's blind spot)
#   2. closure    humble vs jazzy  -> FAIL (the fix: nested drift detected)
#   3. closure    humble vs humble -> PASS (positive control: no false alarm)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TD="$HERE/testdata"
DIFF="$HERE/closure_diff.py"
SEED="autoware_demo_msgs/Leaf"
rc=0

echo "== 1. leaf-only humble vs jazzy (must PASS — the OLD gate is blind here) =="
if python3 "$DIFF" --ref-a "$TD/humble" --ref-b "$TD/jazzy" --seed "$SEED" --leaf-only; then
  echo "OK: leaf-only passed (nested drift invisible, as expected)"
else
  echo "SELFTEST FAIL: leaf-only should have passed" >&2; rc=1
fi

echo; echo "== 2. closure humble vs jazzy (must FAIL — the fix detects nested drift) =="
if python3 "$DIFF" --ref-a "$TD/humble" --ref-b "$TD/jazzy" --seed "$SEED"; then
  echo "SELFTEST FAIL: closure should have detected the geometry_msgs/Point drift" >&2; rc=1
else
  echo "OK: closure correctly flagged the nested drift"
fi

echo; echo "== 3. closure humble vs humble (must PASS — no false positive) =="
if python3 "$DIFF" --ref-a "$TD/humble" --ref-b "$TD/humble" --seed "$SEED"; then
  echo "OK: identical closure passes"
else
  echo "SELFTEST FAIL: identical closure should pass" >&2; rc=1
fi

echo
if [ "$rc" -eq 0 ]; then
  echo "SELFTEST PASS — closure_diff.py detects nested drift the leaf check misses."
else
  echo "SELFTEST FAILED." >&2
fi
exit "$rc"
