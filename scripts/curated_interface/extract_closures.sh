#!/usr/bin/env bash
# extract_closures.sh — populate the curated Autoware interface packages with
# the VERBATIM, fully-transitive message closures the Kirra governor consumes.
# (PMON-004 §8 / AOU-MSG-TOOLCHAIN-001 — retire the laptop trim properly.)
#
# Runs on a host WITH a reference Autoware install (the bench laptop). For each
# seed message it walks the .msg field types, recursively copying every
# Autoware .msg in the closure BYTE-IDENTICALLY into the matching curated
# package under ros2_ws/src/, then regenerates each CMakeLists.txt's
# rosidl_generate_interfaces file list. Base-package types (std_msgs,
# geometry_msgs, builtin_interfaces, unique_identifier_msgs, …) are NOT copied —
# they come from ros-base on the target.
#
# Usage:
#   bash scripts/curated_interface/extract_closures.sh [REF_SHARE_DIR]
#   REF_SHARE_DIR default: /opt/ros/jazzy/share
#
# NEVER hand-edit a copied .msg — verify_hashes.sh is the byte-identical gate.
set -euo pipefail

REF_SHARE="${1:-/opt/ros/jazzy/share}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WS_SRC="$REPO_ROOT/ros2_ws/src"

# Seeds: "package Message" (the types the adapter r2r-binds; verify against
# crates/kirra-ros2-adapter/src/node.rs).
SEEDS=(
  "autoware_perception_msgs PredictedObjects"
  "autoware_planning_msgs Trajectory"
)

# The set of packages we are allowed to curate (must already be scaffolded).
mapfile -t CURATED_PKGS < <(
  find "$WS_SRC" -maxdepth 1 -type d -name 'autoware_*_msgs' -printf '%f\n' | sort
)
is_curated_pkg() { local p; for p in "${CURATED_PKGS[@]}"; do [ "$p" = "$1" ] && return 0; done; return 1; }

# ROS primitive types (no closure follow).
is_primitive() {
  case "$1" in
    bool|byte|char|float32|float64|int8|int16|int32|int64|uint8|uint16|uint32|uint64|string|wstring) return 0 ;;
    *) return 1 ;;
  esac
}

echo "=== extract_closures.sh ==="
echo "reference share : $REF_SHARE"
echo "curated packages: ${CURATED_PKGS[*]:-<none found>}"
[ "${#CURATED_PKGS[@]}" -gt 0 ] || { echo "ERROR: no curated autoware_*_msgs packages under $WS_SRC" >&2; exit 1; }

# Clear any previously-extracted .msg (keep the README placeholders).
for pkg in "${CURATED_PKGS[@]}"; do
  find "$WS_SRC/$pkg/msg" -maxdepth 1 -name '*.msg' -delete 2>/dev/null || true
done

declare -A VISITED=()
QUEUE=("${SEEDS[@]}")

while [ "${#QUEUE[@]}" -gt 0 ]; do
  item="${QUEUE[0]}"; QUEUE=("${QUEUE[@]:1}")
  pkg="${item%% *}"; msg="${item#* }"
  key="$pkg/$msg"
  [ -n "${VISITED[$key]:-}" ] && continue
  VISITED[$key]=1

  if ! is_curated_pkg "$pkg"; then
    echo "ERROR: closure references autoware package '$pkg' (type $msg) that is NOT scaffolded under" >&2
    echo "       $WS_SRC. The closure is larger than the two scaffolded packages — scaffold the" >&2
    echo "       missing package (and update the SRAC) before re-running. (This is a real finding,)" >&2
    echo "       not a bug: the curated interface surface must cover the full closure.)" >&2
    exit 2
  fi

  ref_msg="$REF_SHARE/$pkg/msg/$msg.msg"
  [ -f "$ref_msg" ] || { echo "ERROR: reference message not found: $ref_msg" >&2; exit 3; }

  cp -p "$ref_msg" "$WS_SRC/$pkg/msg/$msg.msg"   # VERBATIM copy
  echo "copied  $pkg/msg/$msg.msg"

  # Parse field types. Strip comments, blank lines, and constants (NAME=val).
  # First token on a definition line is the type (with optional array/bounds).
  while IFS= read -r line; do
    line="${line%%#*}"                       # strip comment
    line="$(echo "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    [ -z "$line" ] && continue
    type_tok="$(echo "$line" | awk '{print $1}')"
    name_tok="$(echo "$line" | awk '{print $2}')"
    # Skip constants: "Type NAME=value" (name contains '=' , or second field has '=').
    case "$name_tok" in *=*) continue ;; esac
    # Strip array '[...]' and bounded-string '<=N' / '<N'.
    base="${type_tok%%[*}"      # drop [..]
    base="${base%%<*}"          # drop <=N / <N (bounded string/array)

    if [[ "$base" == */* ]]; then
      dep_pkg="${base%%/*}"; dep_name="${base##*/}"
      if [[ "$dep_pkg" == autoware_*_msgs ]]; then
        QUEUE+=("$dep_pkg $dep_name")
      fi
      # else: base package (std_msgs/geometry_msgs/…) — not curated.
    else
      # Bare type. Header/Time/Duration map to base packages.
      case "$base" in
        Header|Time|Duration) : ;;            # std_msgs / builtin_interfaces
        *)
          if is_primitive "$base"; then
            :
          elif [[ "$base" =~ ^[A-Z] ]]; then
            # Same-package nested type.
            QUEUE+=("$pkg $base")
          fi
          ;;
      esac
    fi
  done < "$ref_msg"
done

# Regenerate each curated CMakeLists.txt rosidl_generate_interfaces file list
# between the CURATED_MSG_LIST sentinels.
regen_cmake() {
  local pkg="$1" cml="$WS_SRC/$1/CMakeLists.txt"
  local listing="" f
  while IFS= read -r f; do
    listing+="  \"msg/$(basename "$f")\""$'\n'
  done < <(find "$WS_SRC/$pkg/msg" -maxdepth 1 -name '*.msg' | sort)
  [ -n "$listing" ] || listing="  # (no messages extracted — check the reference)"$'\n'
  awk -v ins="$listing" '
    /CURATED_MSG_LIST .* >>>/ { print; printf "%s", ins; skip=1; next }
    /<<< CURATED_MSG_LIST <<</ { print; skip=0; next }
    skip==1 { next }
    { print }
  ' "$cml" > "$cml.tmp" && mv "$cml.tmp" "$cml"
  echo "regenerated $pkg/CMakeLists.txt msg list"
}

echo
echo "=== resulting closure ==="
for pkg in "${CURATED_PKGS[@]}"; do
  echo "$pkg:"
  find "$WS_SRC/$pkg/msg" -maxdepth 1 -name '*.msg' -printf '  %f\n' | sort || true
  regen_cmake "$pkg"
done

echo
echo "=== DONE. Next: byte-verify the closure, then build. ==="
echo "    bash scripts/curated_interface/verify_hashes.sh \"$REF_SHARE\""
echo "Sanity-check the closure above against the expected sets in each msg/README.md."
