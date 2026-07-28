#!/usr/bin/env bash
# disable_vendor_autostart.sh — find (and optionally disable) whatever autostarts
# the Yahboom vendor base node on boot.
#
# WHY: the vendor image auto-launches its own ROS bringup (rosmaster_main.py / a
# Rosmaster_Lib driver node) at boot, and that node OPENS /dev/myserial. The KIRRA
# consumer is the ADR-0033 sole-writer of that port and will FATAL (or fight the
# vendor node for the board) if the vendor node is also running. Before the
# cold-boot drill, the vendor autostart must be OFF so KIRRA owns the board on a
# clean power-on. Killing it by hand (pkill) does not survive a reboot — this
# finds the persistent mechanism and disables it.
#
#   robot/install/disable_vendor_autostart.sh            # REPORT only (default)
#   robot/install/disable_vendor_autostart.sh --disable  # act on confident hits
#
# Fail-safe: default is report-only. --disable acts ONLY on entries this script
# is confident are the vendor bringup (matching the patterns below); it backs up
# any file it edits and never touches a kirra-* unit.
set -uo pipefail

DISABLE=0
[[ "${1:-}" == "--disable" ]] && DISABLE=1
[[ "${1:-}" == "--report" || -z "${1:-}" ]] || { [[ $DISABLE -eq 1 ]] || { echo "usage: $0 [--disable]"; exit 2; }; }

# 🔴 MOTOR-BASE signatures. These name PROGRAMS, never a vendor, a workspace or
# a hostname. The previous pattern included a bare 'yahboom|Yahboom' and, run
# with --disable on the live R2, it:
#   * disabled ollama.service          (a Yahboom workspace was on its PATH)
#   * pkill'd the YDLIDAR driver       (its exe lived under yahboomcar_ros2_ws)
#   * disabled the OLED display unit
#   * flagged 'avahi-daemon: running [yahboom.local]' as a motor base
# None of them ever opened /dev/myserial. Ollama and the lidar had to be
# restored by hand. A vendor's NAME is evidence of nothing; owning the motor
# serial device is the invariant, so that is what this script acts on.
PAT='rosmaster_main|Rosmaster_Lib|ros_robot_controller|yahboomcar_bringup|yahboomcar_base_node'
# Never act on these, whatever else matches — each is a real thing that lives in
# or is named after the vendor workspace and is NOT a motor base.
EXCLUDE='ydlidar|lidar|avahi|ollama|oled|astra|usb_cam|camera|kirra'
found=0
acted=0

hr() { printf '%s\n' "-----------------------------------------------"; }

# ---- 1. systemd services --------------------------------------------------
echo "== 1. systemd units referencing the vendor base =="
# Scan enabled/loaded unit files for the signature, excluding our kirra-* units.
mapfile -t UNITS < <(
  systemctl list-unit-files --type=service --no-legend 2>/dev/null | awk '{print $1}' \
    | grep -viE '^kirra' || true
)
for u in "${UNITS[@]}"; do
  frag="$(systemctl cat "$u" 2>/dev/null || true)"
  [[ -z "$frag" ]] && continue
  # Match ONLY the ExecStart line — a vendor path in Environment=, PATH=,
  # WorkingDirectory= or a comment is not a motor base. This is what saved
  # ollama.service, whose unit merely carried the workspace on PATH.
  exec_lines="$(grep -iE '^\s*ExecStart' <<<"$frag" || true)"
  if grep -qiE "$EXCLUDE" <<<"$u $exec_lines"; then
    continue
  fi
  if grep -qiE "$PAT" <<<"$exec_lines"; then
    state="$(systemctl is-enabled "$u" 2>/dev/null || echo '?')"
    active="$(systemctl is-active "$u" 2>/dev/null || echo '?')"
    echo "  ⚠ ${u}  (enabled=${state} active=${active})"
    grep -iE "ExecStart|$PAT" <<<"$frag" | sed 's/^/       /' | head -3
    found=$((found + 1))
    if [[ $DISABLE -eq 1 ]]; then
      sudo systemctl disable --now "$u" 2>/dev/null \
        && { echo "       ✔ disabled --now $u"; acted=$((acted + 1)); } \
        || echo "       ❌ could not disable $u"
    fi
  fi
done
[[ $found -eq 0 ]] && echo "  (none found)"

# ---- 2. cron @reboot ------------------------------------------------------
echo "== 2. cron @reboot =="
c=0
for cronsrc in "crontab -l" "sudo crontab -l"; do
  out="$($cronsrc 2>/dev/null | grep -iE "@reboot.*($PAT)" || true)"
  [[ -n "$out" ]] && { echo "  ⚠ ($cronsrc):"; sed 's/^/       /' <<<"$out"; c=$((c + 1)); found=$((found + 1)); }
done
[[ $c -eq 0 ]] && echo "  (none found)"
[[ $c -gt 0 && $DISABLE -eq 1 ]] && echo "  → edit the crontab by hand (comment the @reboot line); not auto-edited."

# ---- 3. rc.local + profile/autostart scripts ------------------------------
echo "== 3. rc.local / autostart scripts =="
r=0
for f in /etc/rc.local "$HOME/.bashrc" "$HOME/.profile" "$HOME/.config/autostart"/*.desktop; do
  [[ -e "$f" ]] || continue
  if grep -qiE "$PAT" "$f" 2>/dev/null && ! grep -qiE "$EXCLUDE" "$f" 2>/dev/null; then
    echo "  ⚠ $f references the vendor bringup:"
    grep -niE "$PAT" "$f" | sed 's/^/       /' | head -3
    r=$((r + 1)); found=$((found + 1))
    if [[ $DISABLE -eq 1 ]]; then
      bak="${f}.bak.$(od -An -N4 -tx1 /dev/urandom | tr -d ' ')"
      sudo cp -a "$f" "$bak"
      sudo sed -i -E "s@^(.*($PAT).*)@# DISABLED by disable_vendor_autostart.sh: \1@I" "$f"
      echo "       ✔ commented matching lines in $f (backup $bak)"
      acted=$((acted + 1))
    fi
  fi
done
[[ $r -eq 0 ]] && echo "  (none found)"

# ---- 4. currently-running process (advisory) ------------------------------
echo "== 4. who actually OWNS the motor port? =="
# THE authoritative check. A process is a conflict because it holds the
# configured motor device, not because of what it is called. `pkill -f` on a
# name is gone: that is the line that killed the lidar.
AUTH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/motor_authority.py"
if [[ -f "$AUTH" ]]; then
  python3 "$AUTH" "${KIRRA_MOTOR_PORT:-/dev/myserial}" 2>&1 | sed 's/^/  /' || true
else
  echo "  (motor_authority.py not found — run from the repo checkout)"
fi
echo
echo "  Processes matching a motor-base signature (secondary evidence only —"
echo "  never acted on unless they hold the motor port above):"
if pgrep -af "$PAT" 2>/dev/null | grep -viE "disable_vendor_autostart|grep|$EXCLUDE"; then
  echo "  ⚠ review the list above against the port owner."
else
  echo "  (none)"
fi
echo "  NOTE: this script never kills a process by name. If one of the above"
echo "  owns the motor port, stop its SERVICE deliberately after reading the"
echo "  ownership evidence."


hr
if [[ $found -eq 0 ]]; then
  echo "✔ no vendor autostart found — the board is KIRRA's on boot."
elif [[ $DISABLE -eq 0 ]]; then
  echo "Found $found autostart reference(s). Re-run with --disable to act on the"
  echo "systemd/rc.local/autostart hits (cron is reported, edit by hand)."
  exit 1
else
  echo "Acted on $acted item(s). Re-run WITHOUT --disable to confirm it's clean,"
  echo "then reboot and check: pgrep -af '$PAT' should be empty and"
  echo "the consumer should log 'OWNS /dev/myserial' with no car-type FATAL."
fi
