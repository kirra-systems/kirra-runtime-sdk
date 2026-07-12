#!/usr/bin/env bash
# capture_from_robot.sh — harvest the robot-local configuration that is NOT in
# git, so a reflash is non-destructive (REFLASH.md). Run ON the robot while it
# is in a known-working state; commit the resulting directory.
#
# Captures:
#   - the WORKING ydlidar params (the hard-won TG30 @ 512000 config)
#   - the vendor udev rules that create /dev/myserial and /dev/ydlidar
#   - the current device-symlink truth (which ttyUSB each name points at)
#   - version stamps (L4T, ROS, Rosmaster_Lib, kernel) for provenance
#
# Usage:  robot/install/capture_from_robot.sh [output-dir]
# Default output: robot/install/captured/<hostname>/
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-${HERE}/captured/$(hostname)}"
mkdir -p "${OUT}"
echo "capturing robot-local config -> ${OUT}"

# ---- ydlidar params (the validated TG30 config) -----------------------------
# Search the installed driver share dir and the common vendor workspaces.
found_yaml=0
{
  if command -v ros2 >/dev/null 2>&1; then
    ros2 pkg prefix ydlidar_ros2_driver 2>/dev/null || true
  fi
} | while read -r prefix; do
  [[ -n "${prefix}" ]] || continue
  find "${prefix}/share/ydlidar_ros2_driver" -name '*.yaml' -print 2>/dev/null || true
done > "${OUT}/.yaml_candidates" || true
for base in /home/*/ydlidar_ws /home/*/*_ws /root/ydlidar_ws; do
  [[ -d "${base}" ]] || continue
  find "${base}" -name 'ydlidar*.yaml' -print 2>/dev/null || true
done >> "${OUT}/.yaml_candidates"
mkdir -p "${OUT}/ydlidar"
while read -r y; do
  [[ -f "${y}" ]] || continue
  cp -v "${y}" "${OUT}/ydlidar/" && found_yaml=1
done < "${OUT}/.yaml_candidates"
rm -f "${OUT}/.yaml_candidates"
if [[ ${found_yaml} -eq 0 ]]; then
  echo "⚠  no ydlidar yaml found automatically — locate the WORKING params" >&2
  echo "   (port /dev/ydlidar, baud 512000) and copy them into ${OUT}/ydlidar/ manually." >&2
fi

# ---- udev rules (the /dev/myserial + /dev/ydlidar symlinks) ------------------
mkdir -p "${OUT}/udev"
grep -l -e myserial -e ydlidar -e rplidar /etc/udev/rules.d/*.rules 2>/dev/null \
  | while read -r r; do cp -v "${r}" "${OUT}/udev/"; done || true
ls "${OUT}/udev" 2>/dev/null | grep -q . \
  || echo "⚠  no matching udev rules found under /etc/udev/rules.d — capture them manually." >&2

# ---- device-symlink truth ----------------------------------------------------
{
  echo "# captured device state ($(hostname))"
  for dev in /dev/myserial /dev/ydlidar /dev/rplidar; do
    if [[ -e "${dev}" ]]; then
      echo "${dev} -> $(readlink -f "${dev}")"
    else
      echo "${dev} ABSENT"
    fi
  done
  ls -l /dev/ttyUSB* 2>/dev/null || echo "no ttyUSB devices present"
} > "${OUT}/devices.txt"

# ---- version stamps -----------------------------------------------------------
{
  echo "# provenance stamps ($(hostname))"
  uname -a
  cat /etc/nv_tegra_release 2>/dev/null || echo "no /etc/nv_tegra_release (not L4T?)"
  if [[ -f /opt/ros/humble/setup.bash ]]; then echo "ROS 2 humble present"; fi
  python3 - <<'PY' 2>/dev/null || echo "Rosmaster_Lib not importable"
import Rosmaster_Lib, inspect, os
print("Rosmaster_Lib:", os.path.dirname(inspect.getfile(Rosmaster_Lib)))
PY
} > "${OUT}/versions.txt"

echo
echo "done. Review ${OUT}/ and COMMIT it:"
echo "  git add ${OUT} && git commit -m 'capture: robot-local config ($(hostname))'"
