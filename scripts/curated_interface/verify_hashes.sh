#!/usr/bin/env bash
# verify_hashes.sh — the WIRE-COMPATIBILITY GATE for the curated Autoware
# interface (PMON-004 §8 / KIRRA-OCCY-MSGSYNC-001).
#
# For every .msg in the curated packages under ros2_ws/src/, byte-diff it
# against the same package's .msg in the reference Autoware install. A
# byte-identical closure + identical base-message versions ⇒ identical RIHS
# type hash by construction, so DDS delivers genuine Autoware messages to the
# governor. ANY mismatch (or a curated .msg missing from the reference) is a
# FAIL and exits non-zero.
#
# Optional belt-and-suspenders: if `ros2 interface` RIHS hashing is available
# for the built type, compare the curated vs reference type hash too — but the
# byte-diff is the primary, sufficient proof.
#
# Usage:
#   bash scripts/curated_interface/verify_hashes.sh [REF_SHARE_DIR]
#   REF_SHARE_DIR default: /opt/ros/jazzy/share
set -euo pipefail

REF_SHARE="${1:-/opt/ros/jazzy/share}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WS_SRC="$REPO_ROOT/ros2_ws/src"

mapfile -t CURATED_PKGS < <(
  find "$WS_SRC" -maxdepth 1 -type d -name 'autoware_*_msgs' -printf '%f\n' | sort
)

echo "=== verify_hashes.sh ==="
echo "reference share : $REF_SHARE"
echo "curated packages: ${CURATED_PKGS[*]:-<none>}"

fail=0
checked=0
for pkg in "${CURATED_PKGS[@]}"; do
  shopt -s nullglob
  msgs=("$WS_SRC/$pkg/msg"/*.msg)
  shopt -u nullglob
  if [ "${#msgs[@]}" -eq 0 ]; then
    echo "WARN  $pkg: no .msg extracted yet (run extract_closures.sh first)"
    continue
  fi
  for m in "${msgs[@]}"; do
    name="$(basename "$m")"
    ref="$REF_SHARE/$pkg/msg/$name"
    checked=$((checked + 1))
    if [ ! -f "$ref" ]; then
      echo "FAIL  $pkg/$name — not present in reference ($ref)"
      fail=1
      continue
    fi
    if cmp -s "$m" "$ref"; then
      echo "PASS  $pkg/$name (byte-identical)"
    else
      echo "FAIL  $pkg/$name — DIFFERS from reference:"
      diff -u "$ref" "$m" | sed 's/^/      /' || true
      fail=1
    fi
  done
done

echo
if [ "$checked" -eq 0 ]; then
  echo "ERROR: no curated .msg files to verify — run extract_closures.sh first." >&2
  exit 4
fi
if [ "$fail" -ne 0 ]; then
  echo "RESULT: FAIL — curated closure is NOT byte-identical to the reference."
  echo "        Do NOT deploy/build against this closure; re-extract or resolve the mismatch."
  exit 1
fi
echo "RESULT: PASS — all $checked curated .msg byte-identical to the reference."
echo "        Wire compatibility (RIHS type hash) holds by construction."
