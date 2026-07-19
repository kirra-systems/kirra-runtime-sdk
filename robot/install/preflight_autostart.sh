#!/usr/bin/env bash
# preflight_autostart.sh — is the R2 READY to enable autostart? (read-only gate)
#
# The green light BEFORE the one-service-at-a-time enable dance in
# docs/hardware/R2_AUTOSTART_CHECKLIST.md. Checks every prerequisite for a clean
# governed boot and names the fix for each gap. Changes NOTHING — it only reads
# state (safe to run anytime). Companion to:
#   - disable_vendor_autostart.sh (fix a vendor-autostart FAIL here)
#   - lint_robot_env.sh          (fix a robot.env FAIL here)
#   - cold_boot_drill.sh         (the POST-boot acceptance, after enabling)
#
#   robot/install/preflight_autostart.sh
#
# Exit 0 iff every checked prerequisite passes (ready to enable).
set -uo pipefail

PASS=0; FAIL=0; WARN=0
ok()   { echo "  ✔ $*"; PASS=$((PASS + 1)); }
bad()  { echo "  ❌ $*"; FAIL=$((FAIL + 1)); }
warn() { echo "  ⚠ $*"; WARN=$((WARN + 1)); }
fix()  { echo "       ↳ fix: $*"; }

ROBOT_USER="${SUDO_USER:-${USER:-$(id -un)}}"
KENV=/etc/kirra/kirra.env
RENV=/etc/kirra/robot.env

# sudo-read helper (kirra.env is 600 root/kirra). Returns "" if unreadable.
sread() { sudo cat "$1" 2>/dev/null || true; }

echo "== R2 autostart preflight — user: ${ROBOT_USER} =="

# ---- 1. unit files installed ----------------------------------------------
echo "-- unit files (/etc/systemd/system) --"
for u in kirra.target kirra-verifier.service kirra-planner.service kirra-taj.service \
         kirra-mick.service kirra-consumer.service kirra-ros-stack.service; do
  if systemctl cat "$u" >/dev/null 2>&1; then ok "$u installed"; else
    bad "$u NOT installed"
    case "$u" in
      kirra-*consumer*) fix "sudo robot/install/install_kirra.sh" ;;
      kirra-ros-stack*) fix "sudo robot/install/install_robot_units.sh" ;;
      *)                fix "sudo deploy/systemd/install.sh" ;;
    esac
  fi
done
# rabbit-watch is optional (interactive narration) — warn, don't fail.
systemctl cat kirra-rabbit-watch.service >/dev/null 2>&1 \
  && ok "kirra-rabbit-watch installed (optional)" \
  || warn "kirra-rabbit-watch not installed (optional — narration only)"

# ---- 2. binaries + consumer scripts staged --------------------------------
echo "-- /opt/kirra artifacts --"
for f in /opt/kirra/kirra_verifier_service /opt/kirra/planner_service \
         /opt/kirra/taj_service /opt/kirra/mick_service; do
  [[ -x "$f" ]] && ok "$(basename "$f")" || { bad "missing $f"; fix "sudo deploy/systemd/install.sh"; }
done
for f in /opt/kirra/robot/kirra_motor_consumer.py /opt/kirra/robot/r2_drive.py \
         /opt/kirra/robot/kirra_ffi.py /opt/kirra/lib/libkirra_consumer_ffi.so; do
  [[ -e "$f" ]] && ok "$(basename "$f")" || { bad "missing $f"; fix "sudo robot/install/install_kirra.sh"; }
done

# ---- 3. governor-stack secrets (kirra.env) --------------------------------
echo "-- /etc/kirra/kirra.env (governor stack) --"
kenv="$(sread "$KENV")"
if [[ -z "$kenv" ]]; then
  bad "$KENV unreadable/missing"; fix "sudo deploy/systemd/install.sh (generates secrets)"
else
  grep -qE '^KIRRA_ADMIN_TOKEN=.+' <<<"$kenv" && ok "KIRRA_ADMIN_TOKEN set" || { bad "KIRRA_ADMIN_TOKEN empty (verifier 503s)"; fix "re-run deploy/systemd/install.sh"; }
  grep -qE '^KIRRA_SUPERVISOR_RESET_KEY=.+' <<<"$kenv" && ok "KIRRA_SUPERVISOR_RESET_KEY set" || bad "KIRRA_SUPERVISOR_RESET_KEY empty"
  if grep -qE '^KIRRA_VEHICLE_CLASS=(courier|delivery-av|robotaxi|r2)$' <<<"$kenv"; then
    ok "KIRRA_VEHICLE_CLASS = $(grep -oE '^KIRRA_VEHICLE_CLASS=.*' <<<"$kenv" | cut -d= -f2)"
  else
    bad "KIRRA_VEHICLE_CLASS unset/invalid (verifier + parko fail-closed abort)"
    fix "set KIRRA_VEHICLE_CLASS=courier (interim) or r2 in $KENV"
  fi
  if grep -qE '^KIRRA_GOVERNOR_SIGNING_KEY_SOURCE=.+' <<<"$kenv"; then
    ok "KIRRA_GOVERNOR_SIGNING_KEY_SOURCE set"
  else
    warn "KIRRA_GOVERNOR_SIGNING_KEY_SOURCE unset — verifier won't mint release tokens"
    fix "file:/etc/kirra/gov_2a.seed (bench) — robot/install/make_gov_seed.sh"
  fi
fi

# ---- 4. consumer env (robot.env) ------------------------------------------
echo "-- /etc/kirra/robot.env (consumer) --"
renv="$(sread "$RENV")"
if [[ -z "$renv" ]]; then
  bad "$RENV unreadable/missing"; fix "sudo robot/install/install_kirra.sh --dev-key"
else
  if grep -qE '^KIRRA_GOVERNOR_VK_HEX=[0-9a-fA-F]{64}$' <<<"$renv"; then ok "governor VK pinned (64 hex)"; else
    bad "KIRRA_GOVERNOR_VK_HEX not pinned (placeholder) — consumer refuses to start"
    fix "install_kirra.sh --dev-key (bench) or enroll a real key"
  fi
  grep -qE '^KIRRA_MOTOR_PORT=' <<<"$renv" && ok "KIRRA_MOTOR_PORT set" || bad "KIRRA_MOTOR_PORT unset"
  dm="$(grep -oE '^KIRRA_DRIVE_MODE=.*' <<<"$renv" | cut -d= -f2)"
  [[ "$dm" == "r2_ackermann" ]] && ok "KIRRA_DRIVE_MODE=r2_ackermann" || warn "KIRRA_DRIVE_MODE='${dm:-unset}' (R2 wants r2_ackermann)"
  # env hygiene (the systemd EnvironmentFile traps) — delegate to the linter.
  if robot/install/lint_robot_env.sh "$RENV" >/dev/null 2>&1; then
    ok "robot.env clean (no inline-comment/dup traps)"
  else
    bad "robot.env has inline-comment or duplicate-key traps"
    fix "sudo robot/install/lint_robot_env.sh --fix"
  fi
fi

# ---- 5. user groups (serial + audio) --------------------------------------
echo "-- user groups --"
groups_u="$(id -nG "$ROBOT_USER" 2>/dev/null || true)"
grep -qw dialout <<<"$groups_u" && ok "$ROBOT_USER in dialout (serial)" || { bad "$ROBOT_USER NOT in dialout — consumer can't open /dev/myserial"; fix "sudo usermod -aG dialout $ROBOT_USER (re-login)"; }
grep -qw audio <<<"$groups_u" && ok "$ROBOT_USER in audio (rabbit narration)" || warn "$ROBOT_USER not in audio (only needed for rabbit-watch)"

# ---- 6. device symlinks ---------------------------------------------------
echo "-- devices --"
for d in /dev/myserial /dev/ydlidar; do
  [[ -e "$d" ]] && ok "$d -> $(readlink -f "$d")" || { bad "$d ABSENT"; fix "restore vendor udev rules (capture_from_robot.sh / REFLASH.md)"; }
done

# ---- 7. vendor autostart cleared ------------------------------------------
echo "-- vendor autostart --"
VPAT='rosmaster_main|Rosmaster_Lib|yahboom'
vhit=0
while IFS= read -r u; do
  frag="$(systemctl cat "$u" 2>/dev/null || true)"
  grep -qiE "$VPAT" <<<"$frag" && { [[ "$(systemctl is-enabled "$u" 2>/dev/null)" == "enabled" ]] && { bad "vendor unit enabled: $u"; vhit=1; }; }
done < <(systemctl list-unit-files --type=service --no-legend 2>/dev/null | awk '{print $1}' | grep -viE '^kirra')
[[ $vhit -eq 0 ]] && ok "no enabled vendor base unit" || fix "robot/install/disable_vendor_autostart.sh --disable"

# ---- 8. mick's LLM (ollama) + ROS ws --------------------------------------
echo "-- dependencies --"
if command -v ollama >/dev/null 2>&1 || systemctl is-active ollama >/dev/null 2>&1; then
  ok "ollama present (mick /intent)"
else
  warn "ollama not found — mick /intent 422s fail-closed (typed goals still work)"
fi
WS="$(grep -oE '^KIRRA_ROS_WS_SETUP=.*' <<<"$renv" 2>/dev/null | cut -d= -f2)"
WS="${WS:-/home/${ROBOT_USER}/kirra-runtime-sdk/ros2_ws/install/setup.bash}"
[[ -f "$WS" ]] && ok "ROS ws overlay present ($WS)" || { bad "ROS ws overlay missing: $WS"; fix "colcon build in ros2_ws, or set KIRRA_ROS_WS_SETUP"; }

# ---- verdict --------------------------------------------------------------
echo
echo "preflight: ${PASS} ok, ${WARN} warn, ${FAIL} fail"
if [[ $FAIL -eq 0 ]]; then
  echo "✔ READY to enable autostart. Proceed with the one-service-at-a-time enable"
  echo "  in docs/hardware/R2_AUTOSTART_CHECKLIST.md (validate wheels-up, then enable)."
  exit 0
else
  echo "❌ ${FAIL} blocker(s) above — fix them before enabling autostart."
  exit 1
fi
