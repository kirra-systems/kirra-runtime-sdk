#!/usr/bin/env bash
# lint_robot_env.sh — validate (and optionally normalize) /etc/kirra/robot.env.
#
# WHY: robot.env is consumed by systemd `EnvironmentFile=`, which — unlike a
# shell `source` — does NOT strip inline `# comments` and does NOT dedup repeated
# keys (it takes the LAST). A value line like
#     KIRRA_R2_WHEELBASE_M=0.229   # CHOSEN ~0.229 nominal
# is read by systemd as the value `0.229   # CHOSEN ...`, and the consumer
# fail-closes: "must be a number, got '0.229   # ...'". Bench edits that append a
# clean duplicate below the commented one work by last-wins, but leave a trap:
# reorder or delete the clean line and the commented one silently wins.
#
# AND: robot.env is ALSO `source`d by bash (rabbit_voice.sh, the doctor, the
# bring-up docs' `set -a; . robot.env`). Bash parses an UNQUOTED multi-word
# value line
#     KIRRA_RECORD_CMD=python3 /opt/kirra/robot/vad_record.py
# as `VAR=word command` — it EXECUTES vad_record.py (with no WAV argument, a
# FATAL) and leaves the variable EMPTY in the sourcing shell, so the turn
# recorder later tries to run the WAV path itself ("Permission denied"). Found
# live on the R2, 2026-08. systemd accepted the same line happily, which is
# exactly why it survived: the two consumers disagree. The ONE form both parse
# identically is the double-quoted value (systemd strips surrounding quotes):
#     KIRRA_RECORD_CMD="python3 /opt/kirra/robot/vad_record.py"
# Multi-word values MUST be quoted; escaped spaces are NOT supported (the two
# parsers disagree about those too — fail closed on any unquoted whitespace).
#
# This script FLAGS all three hazards (inline comments on value lines,
# duplicate keys, unquoted multi-word values) and, with --fix, rewrites the
# file to one `KEY=VALUE` per key (last value wins, inline comments stripped,
# multi-word values double-quoted), backing up the original first. Full-line
# comments and blanks are dropped in --fix (the rich provenance lives in
# robot/install/env.template, not the deployed copy).
#
#   robot/install/lint_robot_env.sh            # validate only (exit 1 on issues)
#   robot/install/lint_robot_env.sh --fix      # normalize in place (backs up)
#   robot/install/lint_robot_env.sh /path/to/env [--fix]
set -euo pipefail

FILE=/etc/kirra/robot.env
FIX=0
for arg in "$@"; do
  case "$arg" in
    --fix) FIX=1 ;;
    -*) echo "unknown flag: $arg (known: --fix)"; exit 2 ;;
    *)  FILE="$arg" ;;
  esac
done

[[ -r "$FILE" ]] || { echo "❌ cannot read $FILE (need sudo?)"; exit 2; }
echo "== lint $FILE =="

# A "value line" is KEY=... (KEY = leading [A-Za-z_][A-Za-z0-9_]*). Everything
# else (blank / full-line comment starting with optional ws then #) is structure.
issues=0

# 1. inline comments on value lines (systemd keeps them in the value).
while IFS= read -r line; do
  [[ "$line" =~ ^[[:space:]]*# ]] && continue
  [[ "$line" =~ ^[[:space:]]*$ ]] && continue
  if [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] && [[ "$line" =~ [[:space:]]# ]]; then
    key="${line%%=*}"
    echo "  ⚠ inline comment on value line for ${key} — systemd keeps it IN the value"
    issues=$((issues + 1))
  fi
done < "$FILE"

# 1b. UNQUOTED multi-word values (bash `source` executes the tail as a command
# and leaves the var EMPTY — see the header). Safe: a single unquoted token; a
# value fully wrapped in matching double/single quotes. Unsafe: any other value
# containing whitespace. Trailing whitespace alone is stripped, not flagged.
while IFS= read -r line; do
  [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] || continue
  key="${line%%=*}"
  val="${line#*=}"
  val="${val%"${val##*[![:space:]]}"}"   # trim trailing whitespace
  # An inline-comment line is already flagged by check 1 (and --fix strips the
  # comment before the quoting pass) — don't give misleading quote-this advice.
  [[ "$val" =~ [[:space:]]# ]] && continue
  case "$val" in
    \"*\") [[ ${#val} -ge 2 ]] && continue ;;   # fully double-quoted
    \'*\') [[ ${#val} -ge 2 ]] && continue ;;   # fully single-quoted
  esac
  if [[ "$val" == *[[:space:]]* ]]; then
    echo "  ⚠ UNQUOTED multi-word value for ${key} — systemd accepts it, but a bash \`source\` EXECUTES the tail as a command and leaves ${key} empty. Quote it: ${key}=\"${val}\""
    issues=$((issues + 1))
  fi
done < "$FILE"

# 2. duplicate keys (systemd takes the last; a trap on reorder/delete).
dups="$(grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' "$FILE" | sed 's/=$//' | sort | uniq -d || true)"
if [[ -n "$dups" ]]; then
  while IFS= read -r k; do
    n="$(grep -cE "^${k}=" "$FILE" || true)"
    echo "  ⚠ duplicate key ${k} appears ${n}× (systemd uses the LAST)"
    issues=$((issues + 1))
  done <<< "$dups"
fi

if [[ "$issues" -eq 0 ]]; then
  echo "  ✔ clean — no inline-comment value lines, no duplicate keys, no unquoted multi-word values"
  exit 0
fi

if [[ "$FIX" -eq 0 ]]; then
  echo
  echo "  $issues issue(s). Re-run with --fix to normalize (backs up first):"
  echo "    sudo robot/install/lint_robot_env.sh $FILE --fix"
  exit 1
fi

# --fix: normalize to bare KEY=VALUE, last value wins, inline comments stripped.
BACKUP="${FILE}.bak.$(od -An -N4 -tx1 /dev/urandom | tr -d ' ')"
cp -a "$FILE" "$BACKUP"
echo "  backed up → $BACKUP"

# Emit in first-appearance order, using each key's LAST value (inline comment
# stripped: cut from the first ' #' or tab-#; then trim trailing whitespace).
python3 - "$FILE" > "${FILE}.tmp" <<'PY'
import re, sys
path = sys.argv[1]
order, last = [], {}
for raw in open(path):
    line = raw.rstrip("\n")
    if re.match(r'^\s*#', line) or re.match(r'^\s*$', line):
        continue
    m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)=(.*)$', line)
    if not m:
        continue
    key, val = m.group(1), m.group(2)
    # Strip an inline comment: first ' #' or '\t#'. (KIRRA_* values are numbers/
    # paths/hex/topics and never legitimately contain '#'.)
    val = re.split(r'[ \t]#', val, 1)[0].rstrip()
    # Quote a multi-word value so bash `source` and systemd EnvironmentFile
    # parse it IDENTICALLY (unquoted, bash executes the tail as a command and
    # leaves the var empty; systemd strips the surrounding quotes we add).
    # Already-fully-quoted values pass through; a value that itself contains a
    # double quote is left for a human (cannot be quoted mechanically).
    if re.search(r'\s', val) and not (
            len(val) >= 2 and val[0] == val[-1] and val[0] in ('"', "'")):
        if '"' not in val:
            val = f'"{val}"'
    if key not in last:
        order.append(key)
    last[key] = val
print("# /etc/kirra/robot.env — normalized by lint_robot_env.sh --fix.")
print("# One KEY=VALUE per key (systemd- AND bash-source-safe: no inline")
print("# comments, no dups, multi-word values double-quoted).")
print("# Provenance for each var lives in robot/install/env.template.")
for k in order:
    print(f"{k}={last[k]}")
PY
# Preserve mode/owner AND ACLs of the original, then swap in. The mv replaces
# the inode, which silently drops POSIX ACL entries — and the user voice unit
# depends on a u:<user>:r-- entry on this file (ensure_voice_env_access.sh),
# so a --fix that lost it would kill rabbit-voice.service on its next start
# with Result=resources. getfacl|setfacl round-trips the full ACL (base bits
# included); the chmod/chown fallback covers systems without the acl tools.
chmod --reference="$FILE" "${FILE}.tmp" 2>/dev/null || true
chown --reference="$FILE" "${FILE}.tmp" 2>/dev/null || true
if command -v getfacl >/dev/null 2>&1 && command -v setfacl >/dev/null 2>&1; then
  getfacl --absolute-names -p "$FILE" 2>/dev/null | setfacl --set-file=- "${FILE}.tmp" 2>/dev/null \
    || echo "  ⚠ could not copy ACLs to the rewritten file — if the user voice unit loses access, re-run: sudo robot/install/ensure_voice_env_access.sh"
else
  echo "  ⚠ acl tools not installed — any voice-unit ACL on $FILE is NOT preserved by --fix (sudo apt-get install acl, then re-run robot/install/ensure_voice_env_access.sh)"
fi
mv "${FILE}.tmp" "$FILE"
echo "  ✔ normalized $FILE ($(grep -cE '^[A-Za-z_]' "$FILE") keys). Original at $BACKUP."
echo "  restart the consumer to pick it up: sudo systemctl restart kirra-consumer"
