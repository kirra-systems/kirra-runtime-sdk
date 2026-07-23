#!/usr/bin/env bash
# rabbit_update.sh — pull the latest Rabbit code onto this robot and bring the
# voice back up, in one command. **Run this ON the robot** (it operates on the
# repo checkout; there is no remote self-update — the OTA path updates only the
# governor artifact, never these scripts).
#
#   robot/rabbit_update.sh                 # pull main, re-stage, restart, talk-test
#   robot/rabbit_update.sh --ref <branch>  # pull a branch other than main
#   robot/rabbit_update.sh --restart-stack # also restart kirra-ros-stack (the doer)
#   robot/rabbit_update.sh --no-restart    # update + re-stage only (no unit restart)
#   robot/rabbit_update.sh --no-talk       # skip the spoken confirmation
#
# What it does (idempotent, fail-closed on a real error):
#   1. fast-forwards the repo checkout to origin/<ref> (refuses on a dirty tree
#      so it never clobbers local edits),
#   2. re-stages the Rabbit scripts to /opt/kirra/robot via install_robot_units.sh,
#   3. restarts ONLY the Rabbit units that are already ACTIVE — it never newly
#      starts a disabled unit (the "validate wheels-up before enabling" rule,
#      docs/hardware/R2_AUTOSTART_CHECKLIST.md),
#   4. speaks a confirmation line so you can hear it's talking (and tells you
#      plainly if KIRRA_TTS_CMD is unset, i.e. print-only, no audio).
#
# 🔴 CHANNEL A / operations only. This updates code and restarts services; it has
#    no path to motion. The KIRRA checker bounds every command regardless.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/.." && pwd)"

REF="main"
DO_RESTART=1
RESTART_STACK=0
DO_TALK=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref) REF="${2:?--ref needs a branch name}"; shift 2 ;;
    --no-restart) DO_RESTART=0; shift ;;
    --restart-stack) RESTART_STACK=1; shift ;;
    --no-talk) DO_TALK=0; shift ;;
    -h|--help) sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown arg: $1 (see --help)" >&2; exit 2 ;;
  esac
done

say() { printf '\n== %s ==\n' "$*"; }

# ---- 1. update the checkout -------------------------------------------------
say "1. update ${REPO} → origin/${REF}"
# Only MODIFICATIONS TO TRACKED FILES block the update — those risk being
# clobbered by the merge. Untracked files (build artifacts, caches, lockfiles a
# real robot accumulates) are ignored here; git's own ff-merge below still fails
# safely if a specific untracked file would be overwritten, and we surface that.
if [[ -n "$(git -C "${REPO}" status --porcelain --untracked-files=no)" ]]; then
  echo "REFUSING: the checkout has uncommitted changes to TRACKED files — commit/stash them first" >&2
  git -C "${REPO}" status --short --untracked-files=no >&2
  exit 1
fi
git -C "${REPO}" fetch origin "${REF}"
git -C "${REPO}" checkout "${REF}"
# Fast-forward only: a non-ff (someone committed locally) OR an untracked file
# that would be overwritten stops here, loudly, with git's REAL message — rather
# than a canned guess or a surprise merge on the robot.
if ! merge_out="$(git -C "${REPO}" merge --ff-only "origin/${REF}" 2>&1)"; then
  echo "REFUSING: could not fast-forward to origin/${REF}:" >&2
  echo "${merge_out}" >&2
  echo "(if it names untracked files that would be overwritten, move/remove them and re-run;" >&2
  echo " if the branch has diverged, reconcile it first)" >&2
  exit 1
fi
echo "${merge_out}"
NEW_HEAD="$(git -C "${REPO}" log --oneline -1)"
echo "now at: ${NEW_HEAD}"

# ---- 2. re-stage the scripts ------------------------------------------------
say "2. re-stage Rabbit scripts → /opt/kirra/robot"
"${REPO}/robot/install/install_robot_units.sh"

# ---- 3. restart the ACTIVE Rabbit units -------------------------------------
if [[ "${DO_RESTART}" -eq 1 ]]; then
  say "3. restart running Rabbit units (active ones only)"
  UNITS=(kirra-rabbit-watch kirra-rabbit-voice kirra-rabbit-greet)
  [[ "${RESTART_STACK}" -eq 1 ]] && UNITS=(kirra-ros-stack "${UNITS[@]}")
  restarted=0
  for u in "${UNITS[@]}"; do
    if systemctl is-active --quiet "${u}"; then
      sudo systemctl restart "${u}" && echo "  restarted ${u}" && restarted=1
    else
      echo "  skipped ${u} (not active — enable it deliberately after validation)"
    fi
  done
  [[ "${restarted}" -eq 0 ]] && echo "  (no active Rabbit units — start them per RABBIT_BRINGUP_RUNBOOK.md)"
else
  say "3. restart skipped (--no-restart)"
fi

# ---- 4. talk-test -----------------------------------------------------------
if [[ "${DO_TALK}" -eq 1 ]]; then
  say "4. talk-test"
  # Load the robot's audio env (KIRRA_TTS_CMD lives here) exactly as the units do.
  [[ -f /etc/kirra/robot.env ]] && { set -a; . /etc/kirra/robot.env; set +a; }
  if [[ -z "${KIRRA_TTS_CMD:-}" ]]; then
    echo "  NOTE: KIRRA_TTS_CMD is unset → Rabbit will PRINT the line but not speak it."
    echo "        Configure TTS (piper) per robot/install/rabbit.env.example +"
    echo "        docs/hardware/RABBIT_AUDIO_STACK.md, then re-run to hear it aloud."
  fi
  MSG="Rabbit updated to the latest and back online. Robotic Agent, Bounded by Independent Trust — at your disposal."
  PYTHONPATH="${REPO}/robot" python3 -c 'import sys; from rabbit_persona import speak; speak(sys.argv[1])' "${MSG}" \
    || echo "  (speak failed — see robot/kirra_voice_doctor.sh for an audio check)"
fi

say "done — Rabbit is on ${NEW_HEAD}"
