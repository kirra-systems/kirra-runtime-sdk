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
# This script FLAGS both hazards (inline comments on value lines, duplicate keys)
# and, with --fix, rewrites the file to one bare `KEY=VALUE` per key (last value
# wins, inline comments stripped), backing up the original first. Full-line
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
  echo "  ✔ clean — no inline-comment value lines, no duplicate keys"
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
    if key not in last:
        order.append(key)
    last[key] = val
print("# /etc/kirra/robot.env — normalized by lint_robot_env.sh --fix.")
print("# One bare KEY=VALUE per key (systemd-safe: no inline comments, no dups).")
print("# Provenance for each var lives in robot/install/env.template.")
for k in order:
    print(f"{k}={last[k]}")
PY
# Preserve mode/owner of the original, then swap in.
chmod --reference="$FILE" "${FILE}.tmp" 2>/dev/null || true
chown --reference="$FILE" "${FILE}.tmp" 2>/dev/null || true
mv "${FILE}.tmp" "$FILE"
echo "  ✔ normalized $FILE ($(grep -cE '^[A-Za-z_]' "$FILE") keys). Original at $BACKUP."
echo "  restart the consumer to pick it up: sudo systemctl restart kirra-consumer"
