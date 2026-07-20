#!/usr/bin/env bash
# crossdistro_hash_check.sh — the Humble↔Jazzy wire-compatibility gate for the
# curated Autoware interface (ADR-0036; extends KIRRA-OCCY-MSGSYNC-001 to the
# distro pair).
#
# The "only Autoware on Humble" split (ADR-0036) runs the Autoware *doer* on
# 22.04/Humble and the KIRRA checker+adapter on 24.04/Jazzy, meeting ONLY on the
# 5 curated boundary topics over DDS. That direct cross-distro DDS is wire-safe
# iff the curated `.msg` closures are BYTE-IDENTICAL on both distros: identical
# closure + identical base messages ⇒ identical RIHS type hash ⇒ genuine
# messages cross the boundary. This script proves (or refutes) exactly that:
#
#   1. curated == Humble reference       (reuses verify_hashes.sh)
#   2. curated == Jazzy  reference       (reuses verify_hashes.sh)
#   3. Humble reference == Jazzy reference, over the FULL RECURSIVE CLOSURE of
#      each curated seed — base packages included (closure_diff.py)
#
# M3 (#1042): step 3 previously diffed only the curated top-level autoware_*_msgs
# `.msg`. A differing NESTED base message (e.g. builtin_interfaces/Time,
# std_msgs/Header, a geometry_msgs sub-type) leaves every curated leaf
# byte-identical while the RIHS type hash diverges — undetected. Step 3 now walks
# the transitive closure from the seeds and byte-compares EVERY message in it
# across the two references, so a nested drift can no longer slip through.
#
# A DRIFT in step 3 names the exact interface (leaf OR base) that must go through
# kirra_bridge_cpp / domain_bridge instead of direct DDS.
#
# Bench tool — the full run needs BOTH a Humble and a Jazzy Autoware msg
# reference available (sourced installs or mounted `share/` trees), so it is not
# itself a per-PR CI gate. But the closure comparator's LOGIC is gated:
# `closure_diff_selftest.sh` runs `closure_diff.py` over committed fixture trees
# (no ROS) in CI, proving it detects a nested drift the leaf-only check misses.
# Promoting the FULL comparison to a merge-gating lane needs pinned dual-distro
# msg shares in a container (the remainder tracked on #1042).
#
# Usage:
#   bash scripts/curated_interface/crossdistro_hash_check.sh \
#        [REF_HUMBLE=/opt/ros/humble/share] [REF_JAZZY=/opt/ros/jazzy/share]
set -euo pipefail

REF_HUMBLE="${1:-/opt/ros/humble/share}"
REF_JAZZY="${2:-/opt/ros/jazzy/share}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
WS_SRC="$REPO_ROOT/ros2_ws/src"

echo "== cross-distro curated-interface wire-compat check (ADR-0036) =="
echo "  Humble ref: $REF_HUMBLE"
echo "  Jazzy  ref: $REF_JAZZY"
echo

missing=0

# --- 1 + 2: curated is byte-identical to EACH reference (the existing gate) ---
for ref in "$REF_HUMBLE" "$REF_JAZZY"; do
  if [ ! -d "$ref" ]; then
    echo "SKIP: reference share not found: $ref (source that distro, or mount its share/ tree)"
    missing=1
    continue
  fi
  echo "-- curated vs ${ref} --"
  bash "$HERE/verify_hashes.sh" "$ref"
  echo
done

# --- 3: the cross-distro RECURSIVE-CLOSURE diff (are the upstream .msg identical
#        all the way down — base packages included?) ---
echo "== Humble↔Jazzy recursive-closure diff (closure_diff.py) =="
# Seeds must match extract_closures.sh (the types the adapter r2r-binds).
if python3 "$HERE/closure_diff.py" \
    --ref-a "$REF_HUMBLE" --ref-b "$REF_JAZZY" \
    --seed autoware_perception_msgs/PredictedObjects \
    --seed autoware_planning_msgs/Trajectory; then
  drift=0
else
  drift=1
fi

echo
if [ "$missing" -ne 0 ]; then
  echo "RESULT: INCOMPLETE — a reference share was missing (steps 1/2)."
  echo "        Re-run with BOTH distros' Autoware msg shares available."
  exit 2
fi
if [ "$drift" -ne 0 ]; then
  echo "RESULT: DRIFT — some interface in the curated closure (leaf OR nested base"
  echo "        message) differs across Humble/Jazzy. Those need kirra_bridge_cpp /"
  echo "        domain_bridge translation, NOT direct cross-distro DDS. Record the"
  echo "        drift in the MSGSYNC SRAC."
  exit 1
fi
echo "RESULT: PASS — the whole curated closure (leaves + nested base messages) is"
echo "        byte-identical Humble == Jazzy. Direct cross-distro DDS is wire-safe."
