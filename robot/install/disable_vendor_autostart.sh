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

# Vendor signatures — what a Yahboom base autostart looks like. Deliberately does
# NOT match 'kirra' (our own units must never be caught).
PAT='rosmaster_main|Rosmaster_Lib|yahboom|Yahboom|ros_robot_controller|handsfree|bringup.*rosmaster|start_ros\.sh'
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
  if grep -qiE "$PAT" <<<"$frag"; then
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
  if grep -qiE "$PAT" "$f" 2>/dev/null; then
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
echo "== 4. running now? =="
if pgrep -af "$PAT" 2>/dev/null | grep -viE 'disable_vendor_autostart|grep'; then
  echo "  ⚠ a vendor process is running NOW."
  [[ $DISABLE -eq 1 ]] && { sudo pkill -f "$PAT" 2>/dev/null && echo "  ✔ pkill sent" || true; }
  echo "  (a running process is separate from the boot autostart above — disable both.)"
else
  echo "  (no vendor process running)"
fi

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
