#!/usr/bin/env bash
# test-check-commit-signing.sh — prove the signing preflight tells the truth.
#
# The bug this guards against is subtle and cost real SHA churn: git reports
# `%G?` = 'N' for an SSH-signed commit when `gpg.ssh.allowedSignersFile` is
# unset, which is the SAME character it uses for "no signature at all". A hook
# that reads %G? therefore calls signed commits unsigned, prescribes an amend,
# and the amended commit reports N as well — forever.
#
# Everything runs in `mktemp -d` repositories. Nothing touches the developer's
# ~/.ssh, the real index, the working tree, or any branch.
#
# Run: bash scripts/test-check-commit-signing.sh   (exit 0 = the preflight is honest)
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/check-commit-signing.sh"
PASS=0; FAIL=0
ok()  { printf '  ok   - %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL - %s\n' "$1"; FAIL=$((FAIL + 1)); }
chk() { if [ "$1" -eq 0 ]; then ok "$2"; else bad "$2"; fi; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A repository with signing DISABLED — the "nothing to prove" baseline.
mk_repo() {
    local d="$1"; git init --quiet "$d"
    git -C "$d" config user.email t@example.invalid
    git -C "$d" config user.name  Tester
    git -C "$d" config commit.gpgsign false
    git -C "$d" -c commit.gpgsign=false commit --quiet --allow-empty -m seed
    printf '%s' "$d"
}

echo "== P1-P3: configuration reporting =="

R1="$(mk_repo "$WORK/r1")"
OUT="$(cd "$R1" && bash "$SCRIPT" 2>&1)"; RC=$?
[ "$RC" -eq 2 ]; chk $? "P1. signing not required → exit 2 (nothing to prove), not a failure"
grep -q "not required" <<<"$OUT"; chk $? "P2. …and it says so rather than implying a defect"

R2="$(mk_repo "$WORK/r2")"
git -C "$R2" config commit.gpgsign true
git -C "$R2" config gpg.format ssh
git -C "$R2" config user.signingkey "$WORK/zero.pub"
: > "$WORK/zero.pub"          # exists, ZERO BYTES — the real-world case
OUT="$(cd "$R2" && bash "$SCRIPT" 2>&1)"; RC=$?
grep -q "key file empty *YES (zero bytes)" <<<"$OUT"; chk $? \
    "P3. a zero-byte key file is reported as such, as evidence"

echo "== P4-P6: the verdict comes from SIGNING, not from configuration =="

# The decisive property: whatever the circumstantial evidence says, the verdict
# must follow the disposable signing probe.
grep -q "commit-tree -S" "$SCRIPT"; chk $? \
    "P4. the probe signs a throwaway object with commit-tree -S"
grep -q "gpgsig" "$SCRIPT"; chk $? \
    "P5. success is judged by a gpgsig header, never by an exit code"
if grep -qE 'RESULT: signing is (usable|CONFIGURED)' "$SCRIPT"; then ok \
    "P6. the script emits an explicit RESULT verdict line"; else bad "P6"; fi

echo "== P7-P9: %G? is NOT used as a presence test =="

# The core regression. If the range listing ever goes back to `%G? == N`, a
# signed-but-unverifiable commit is called unsigned again.
RANGE_BLOCK="$(sed -n '/signature PRESENCE in range/,$p' "$SCRIPT")"
grep -q "cat-file commit" <<<"$RANGE_BLOCK"; chk $? \
    "P7. presence is read from the commit object's gpgsig header"
! grep -qE "awk .\\\$2 == \"N\"" <<<"$RANGE_BLOCK"; chk $? \
    "P8. the range listing does NOT classify by %G? == N"
grep -q "allowedSignersFile" "$SCRIPT"; chk $? \
    "P9. the N-means-two-things trap is documented in the output itself"

echo "== P10-P12: the preflight mutates nothing =="

R3="$(mk_repo "$WORK/r3")"
git -C "$R3" config commit.gpgsign true
git -C "$R3" config gpg.format ssh
BEFORE_HEAD="$(git -C "$R3" rev-parse HEAD)"
BEFORE_STATUS="$(git -C "$R3" status --porcelain=v1)"
BEFORE_REFS="$(git -C "$R3" for-each-ref --format='%(refname) %(objectname)')"
(cd "$R3" && bash "$SCRIPT" >/dev/null 2>&1)
[ "$(git -C "$R3" rev-parse HEAD)" = "$BEFORE_HEAD" ]; chk $? \
    "P10. HEAD is unchanged by the preflight"
[ "$(git -C "$R3" status --porcelain=v1)" = "$BEFORE_STATUS" ]; chk $? \
    "P11. index and working tree are unchanged"
[ "$(git -C "$R3" for-each-ref --format='%(refname) %(objectname)')" = "$BEFORE_REFS" ]; chk $? \
    "P12. no ref moved — the preflight cannot churn a SHA"

echo "== P13-P15: no false success, and no amend is ever performed =="

# Match INVOCATIONS, not prose. The words "unpushed"/"amend" appear throughout
# the script's own explanations; a substring scan would flag its documentation.
body="$(grep -v '^[[:space:]]*#' "$SCRIPT")"
names=("git commit --amend" "git rebase" "git push" "git reset")
pats=('git +(-C +[^ ]+ +)?commit +--amend' 'git +(-C +[^ ]+ +)?rebase' \
      'git +(-C +[^ ]+ +)?push' 'git +(-C +[^ ]+ +)?reset')
for i in "${!pats[@]}"; do
    if grep -qE -- "${pats[$i]}" <<<"$body"; then
        bad "P13. the preflight must never invoke: ${names[$i]}"
    else
        ok "P13. the preflight never invokes: ${names[$i]}"
    fi
done
grep -qi "No amend was attempted" "$SCRIPT"; chk $? \
    "P14. the unusable-signing path states that no amend was attempted"
! grep -qiE '(^|[^n])say "(fixed|verified)"' "$SCRIPT"; chk $? \
    "P15. it never prints a bare 'fixed'/'verified' success claim"

echo "== P16: the live environment, reported honestly =="

OUT="$(cd "$REPO_ROOT" && bash "$SCRIPT" 2>&1)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/     /' | grep -E "RESULT|signed|UNSIGNED" | head -5
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    ok "P16. the preflight reaches a definite verdict in this environment (rc=$RC)"
else
    bad "P16. unexpected rc=$RC"
fi

echo
if [ "$FAIL" -ne 0 ]; then
    echo "== signing preflight self-test: $FAIL FAILED, $PASS passed =="
    exit 1
fi
echo "== signing preflight self-test: all $PASS checks passed =="
