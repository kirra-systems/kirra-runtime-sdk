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
#   3. Humble reference == Jazzy reference, per curated .msg  (the cross-distro diff)
#
# A DRIFT in step 3 names the exact interface that must go through
# kirra_bridge_cpp / domain_bridge instead of direct DDS.
#
# Bench tool — needs BOTH a Humble and a Jazzy Autoware msg reference available
# (sourced installs or mounted `share/` trees). Not a CI gate.
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

# --- 3: the cross-distro per-interface diff (are the upstream .msg identical?) ---
echo "== per-interface Humble↔Jazzy diff =="
mapfile -t CURATED_PKGS < <(
  find "$WS_SRC" -maxdepth 1 -type d -name 'autoware_*_msgs' -printf '%f\n' | sort
)
drift=0
checked=0
for pkg in "${CURATED_PKGS[@]}"; do
  shopt -s nullglob
  msgs=("$WS_SRC/$pkg/msg"/*.msg)
  shopt -u nullglob
  for m in "${msgs[@]}"; do
    name="$(basename "$m")"
    h="$REF_HUMBLE/$pkg/msg/$name"
    j="$REF_JAZZY/$pkg/msg/$name"
    if [ ! -f "$h" ] || [ ! -f "$j" ]; then
      echo "SKIP  $pkg/$name (missing on a reference)"
      missing=1
      continue
    fi
    checked=$((checked + 1))
    if cmp -s "$h" "$j"; then
      echo "MATCH $pkg/$name  (Humble == Jazzy → direct DDS ok)"
    else
      echo "DRIFT $pkg/$name  (Humble != Jazzy → bridge THIS interface):"
      diff -u "$h" "$j" | sed 's/^/      /' || true
      drift=1
    fi
  done
done

echo
if [ "$missing" -ne 0 ]; then
  echo "RESULT: INCOMPLETE — a reference (or a per-interface file) was missing."
  echo "        Re-run with BOTH distros' Autoware msg shares available."
  exit 2
fi
if [ "$checked" -eq 0 ]; then
  echo "ERROR: no curated .msg to compare — run extract_closures.sh first." >&2
  exit 4
fi
if [ "$drift" -ne 0 ]; then
  echo "RESULT: DRIFT — some curated interfaces differ across Humble/Jazzy."
  echo "        Those interfaces need kirra_bridge_cpp / domain_bridge translation,"
  echo "        NOT direct cross-distro DDS. Record the drift in the MSGSYNC SRAC."
  exit 1
fi
echo "RESULT: PASS — all $checked curated interfaces are byte-identical Humble == Jazzy."
echo "        Direct cross-distro DDS is wire-safe for the whole boundary; no bridge needed."
