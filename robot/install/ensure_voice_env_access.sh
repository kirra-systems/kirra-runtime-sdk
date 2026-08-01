#!/usr/bin/env bash
# ensure_voice_env_access.sh — grant the USER-unit voice service exactly the
# filesystem access its EnvironmentFile= needs, and nothing else.
#
# WHY: rabbit-voice.service is a USER unit — its EnvironmentFile= is opened by
# the operator's own `systemd --user` manager, which (unlike PID 1 opening the
# same path for the system units) is subject to ordinary permission checks.
# /etc/kirra is legitimately tight: deploy/systemd/install.sh keeps it
# 0750 kirra:kirra because kirra.env in the same directory holds the governor
# admin secrets. Result on the live R2: the operator account could not
# TRAVERSE /etc/kirra, the user manager could not open robot.env, and the unit
# died before ExecStart with Result=resources / pid=0 and no service log.
#
# THE FIX is two narrow POSIX ACL entries — least privilege, not a mode change:
#     setfacl -m u:<user>:--x /etc/kirra            # traverse ONLY (no listing)
#     setfacl -m u:<user>:r-- /etc/kirra/robot.env  # read ONLY this file
# The user still cannot list /etc/kirra and still cannot read kirra.env (0600).
# Deliberately NOT chmod 0755 on the directory (would expose every future
# file's name and weaken the governor-secret posture), NOT a kirra-group
# membership (group read = listing + every future group-readable file), and
# NOT a default ACL (would grant access to every future file in the dir).
#
# ACL durability: chmod/chown from either installer re-running does NOT remove
# these entries (a chmod only rewrites the ACL mask, and the group bits both
# installers apply keep traverse effective). What DOES drop the FILE entry is
# inode replacement — lint_robot_env.sh --fix, sed -i, tee-to-temp-then-rename.
# lint_robot_env.sh --fix now restores ACLs itself; for anything else, re-run
# this script (idempotent — setfacl -m updates in place, never duplicates).
#
#   sudo robot/install/ensure_voice_env_access.sh                # apply + verify
#   sudo robot/install/ensure_voice_env_access.sh --user jetson  # explicit user
#   robot/install/ensure_voice_env_access.sh --check             # read-only diagnosis
#   sudo robot/install/ensure_voice_env_access.sh --remove       # rollback
#
# The service user resolves, in order: --user, $KIRRA_ROBOT_USER, $SUDO_USER,
# $USER — the same "invoking user" convention install_robot_units.sh renders
# into the units. --dir/--file exist for the host tests (tmpdir fixtures).
set -euo pipefail

DIR=/etc/kirra
FILE=/etc/kirra/robot.env
TARGET_USER="${KIRRA_ROBOT_USER:-${SUDO_USER:-${USER:-}}}"
MODE=apply

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user) TARGET_USER="${2:?--user needs a value}"; shift 2 ;;
    --dir)  DIR="${2:?--dir needs a value}"; shift 2 ;;
    --file) FILE="${2:?--file needs a value}"; shift 2 ;;
    --check)  MODE=check; shift ;;
    --remove) MODE=remove; shift ;;
    *) echo "usage: $0 [--user U] [--dir D] [--file F] [--check|--remove]"; exit 2 ;;
  esac
done

fail() { echo "❌ $*" >&2; exit 1; }

[[ -n "$TARGET_USER" ]] || fail "cannot resolve the service user (pass --user, or set KIRRA_ROBOT_USER)"
id -u "$TARGET_USER" >/dev/null 2>&1 \
  || fail "service user '$TARGET_USER' does not exist — refusing to grant access to a phantom account"

# Guardrails on the grant TARGET (this runs under sudo — a typo'd --file must
# not quietly grant read on a secret like kirra.env): the file must be named
# robot.env, and it must live directly inside --dir. Pure bash expansions
# (no basename/dirname) so the missing-acl-tools error path stays reachable
# even on a minimal PATH.
_file_name="${FILE##*/}"
_file_parent="${FILE%/*}"; [[ "$_file_parent" == "$FILE" ]] && _file_parent="."
[[ "$_file_name" == "robot.env" ]] \
  || fail "refusing --file '$FILE': this helper grants access to robot.env ONLY (never another file — kirra.env holds governor secrets)"
if [[ -d "$DIR" && -d "$_file_parent" ]]; then
  _dir_real="$(cd "$DIR" && pwd)"
  _file_dir_real="$(cd "$_file_parent" && pwd)"
  [[ "$_file_dir_real" == "$_dir_real" ]] \
    || fail "refusing: --file '$FILE' is not directly inside --dir '$DIR' (traverse grant and read grant must target the same directory)"
fi

# Diagnosis helper: can TARGET_USER actually read FILE? Runs the check AS the
# target user when we are root (the honest test); as ourselves when we ARE the
# target; otherwise reports that it cannot prove it either way.
as_user() {  # as_user <test-flag> <path>
  if [[ "$(id -u)" -eq 0 ]]; then
    runuser -u "$TARGET_USER" -- test "$1" "$2"
  elif [[ "$(id -un)" == "$TARGET_USER" ]]; then
    test "$1" "$2"
  else
    return 3   # cannot verify from this account
  fi
}

diagnose() {
  # Precise, in causal order: parent traverse → file existence → file read.
  local rc=0
  if as_user -x "$DIR"; then
    echo "  ✔ $TARGET_USER can traverse $DIR"
  else
    rc=$?
    if [[ $rc -eq 3 ]]; then
      echo "  ⚠ cannot verify as '$TARGET_USER' from this account (run as root or as the user)"
      return 3
    fi
    echo "  ❌ $TARGET_USER cannot traverse $DIR (this is the live-incident failure)"
    echo "       ↳ fix: sudo $0 --user $TARGET_USER"
    return 1
  fi
  if as_user -e "$FILE"; then
    if as_user -r "$FILE"; then
      echo "  ✔ $TARGET_USER can read $FILE"
      return 0
    fi
    echo "  ❌ $TARGET_USER cannot READ $FILE (traverse ok, file mode/ACL blocks it)"
    echo "       ↳ fix: sudo $0 --user $TARGET_USER"
    return 1
  fi
  echo "  ❌ $FILE does not exist (for $TARGET_USER)"
  echo "       ↳ fix: run robot/install/install_kirra.sh (renders robot.env if absent), then sudo $0 --user $TARGET_USER"
  return 1
}

if [[ "$MODE" == check ]]; then
  echo "== voice env access check — user: $TARGET_USER, file: $FILE =="
  diagnose
  exit $?
fi

# apply / remove need the ACL tools and the real paths.
command -v setfacl >/dev/null 2>&1 && command -v getfacl >/dev/null 2>&1 \
  || fail "setfacl/getfacl not found — install the 'acl' package (sudo apt-get install acl)"
[[ -d "$DIR" ]]  || fail "$DIR does not exist — run the installer that creates it first (install_kirra.sh or deploy/systemd/install.sh)"

if [[ "$MODE" == remove ]]; then
  echo "== removing voice env ACLs — user: $TARGET_USER =="
  [[ -e "$FILE" ]] && setfacl -x "u:$TARGET_USER" "$FILE" 2>/dev/null || true
  setfacl -x "u:$TARGET_USER" "$DIR" 2>/dev/null || true
  echo "  ✔ removed u:$TARGET_USER entries from $DIR and $FILE (where present)"
  exit 0
fi

[[ -f "$FILE" ]] || fail "$FILE does not exist — render it first (install_kirra.sh; never created by this script)"

# Apply the two narrow entries. setfacl -m is idempotent: re-running updates
# the one named entry in place — repeated installer runs cannot accumulate
# duplicates or touch any other entry, file, or the base mode bits.
setfacl -m "u:$TARGET_USER:--x" "$DIR"
setfacl -m "u:$TARGET_USER:r--" "$FILE"
echo "  ✔ granted u:$TARGET_USER traverse-only on $DIR, read-only on $FILE"

# Prove it with the real account, not our own privileges.
rc=0
diagnose || rc=$?
if [[ $rc -eq 0 ]]; then
  exit 0
elif [[ $rc -eq 3 ]]; then
  echo "  ⚠ ACLs applied, but verification needs root (or the target account): sudo $0 --check --user $TARGET_USER"
  exit 0
fi
fail "ACLs applied but '$TARGET_USER' still cannot read $FILE — inspect: getfacl $DIR $FILE ; namei -l $FILE"
