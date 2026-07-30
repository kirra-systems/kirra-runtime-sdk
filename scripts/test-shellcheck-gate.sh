#!/usr/bin/env bash
# test-shellcheck-gate.sh — prove the CI ShellCheck step can actually go RED.
#
# A lint gate that cannot fail is worse than no gate: it reports green forever
# and everyone stops looking. This repository shipped exactly that — the step ran
# `… | tee shellcheck-report.txt` with no `set -o pipefail`, so the pipeline
# returned TEE's status and shellcheck's non-zero exit was discarded. Measured on
# the pre-fix form: exit 0 on a tree that had a real error-level SC2148 finding.
# The same defect was fixed for `cargo audit` in #687 but never applied here.
#
# So this asserts the PROPERTY, not the wording: run the gate's pipeline shape
# over a deliberately-bad throwaway script and require a non-zero status.
#
# Everything is generated under `mktemp -d`. Nothing tracked is written, and no
# bad script is committed — a committed bad script would break the real gate,
# which is the thing under test.
#
# Run: bash scripts/test-shellcheck-gate.sh   (exit 0 = the gate is real)
set -euo pipefail

# NOTE: the tool emits UTF-8; under a C/POSIX locale it cannot write non-ASCII
# source lines to a pipe and TRUNCATES its report (exiting 2). The CI step pins
# this for the same reason — see the comment on the ShellCheck step in ci.yml.
# (This comment deliberately does not begin with the tool's name: a comment
# starting `# shellcheck …` is parsed as a DIRECTIVE, which is SC1072/SC1073 at
# error level. The fixed gate caught exactly that here.)
export LC_ALL=C.UTF-8

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PASS=0
FAIL=0

ok() { printf '  ok   - %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL - %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [ "$1" -eq 0 ]; then ok "$2"; else bad "$2"; fi; }

command -v shellcheck >/dev/null 2>&1 || {
    echo "test-shellcheck-gate: shellcheck not installed — cannot verify the gate" >&2
    echo "(install it, or run this on the CI runner where it is preinstalled)" >&2
    exit 2
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ── the gate's pipeline shape, isolated ──────────────────────────────────────
# Deliberately mirrors the CI step: pipefail + one invocation + tee. If the CI
# step's shape changes, P7 below catches the divergence.
run_gate() {
    local report="$1"
    shift
    (
        set -o pipefail
        shellcheck -S error "$@" 2>&1 | tee "$report" >/dev/null
    )
}

echo "== P1-P2: a real finding must produce a real failure =="

# An error-level finding with no ambiguity: a sourced fragment with no shebang
# and no shell directive is SC2148 (error) — the exact class this repo tripped.
cat >"$WORK/bad_fragment.sh" <<'FRAGMENT'
# no shebang, no shell directive: SC2148 at error level
SOME_VALUE=1
FRAGMENT

set +e
run_gate "$WORK/bad.txt" "$WORK/bad_fragment.sh"
BAD_RC=$?
set -e
[ "$BAD_RC" -ne 0 ]
check $? "P1. a script with an error-level finding FAILS the gate (exit $BAD_RC)"

grep -q 'SC2148' "$WORK/bad.txt"
check $? "P2. the finding still reaches the report through tee"

echo "== P3: the historical bug is really a bug =="
# The pre-fix pipeline shape, run over the SAME bad script. If this ever starts
# failing, the masking has gone away on its own and this test's premise needs
# revisiting — but until then it documents WHY pipefail is load-bearing.
set +e
bash -c 'shellcheck -S error "$1" 2>&1 | tee "$2" >/dev/null' _ \
    "$WORK/bad_fragment.sh" "$WORK/masked.txt"
MASKED_RC=$?
set -e
[ "$MASKED_RC" -eq 0 ]
check $? "P3. WITHOUT pipefail the same finding exits 0 — the masking is real (got $MASKED_RC)"

echo "== P4-P5: a clean script must still pass, and emptiness must not =="

cat >"$WORK/clean.sh" <<'CLEAN'
#!/usr/bin/env bash
set -euo pipefail
echo "nothing to complain about"
CLEAN

set +e
run_gate "$WORK/clean.txt" "$WORK/clean.sh"
CLEAN_RC=$?
set -e
[ "$CLEAN_RC" -eq 0 ]
check $? "P4. a clean script PASSES the gate (exit $CLEAN_RC)"

# An empty file list must be a hard failure. `xargs -r` silently skips, which
# would turn a broken glob or a bad checkout into a green run.
set +e
(
    set -o pipefail
    mapfile -d '' -t files < <(printf '')
    if [ "${#files[@]}" -eq 0 ]; then exit 1; fi
    shellcheck -S error "${files[@]}"
)
EMPTY_RC=$?
set -e
[ "$EMPTY_RC" -ne 0 ]
check $? "P5. an EMPTY file list fails rather than passing vacuously (exit $EMPTY_RC)"

echo "== P6: the sourced fragment is checked AS a fragment =="

FRAG="$REPO_ROOT/tools/qnx-rtm-harness/judge_build_flags.sh"
set +e
run_gate "$WORK/frag.txt" "$FRAG"
FRAG_RC=$?
set -e
[ "$FRAG_RC" -eq 0 ]
check $? "P6a. judge_build_flags.sh passes the gate (exit $FRAG_RC)"

grep -q '^# shellcheck shell=bash' "$FRAG"
check $? "P6b. …because it DECLARES its dialect with a shell directive"

! head -n 1 "$FRAG" | grep -q '^#!'
check $? "P6c. …and NOT by gaining a shebang it does not deserve"

[ ! -x "$FRAG" ]
check $? "P6d. …and it is still not executable (it is sourced, never run)"

# The fix must not have been achieved by muting the check everywhere. This file
# is excluded because it necessarily contains the string it searches for — a
# scanner that matches its own needle only ever finds itself.
SELF="$(basename "${BASH_SOURCE[0]}")"
if grep -rl 'disable=SC2148' "$REPO_ROOT/scripts" "$REPO_ROOT/tools" 2>/dev/null \
        | grep -qv "/$SELF\$"; then
    bad "P6e. SC2148 is blanket-disabled somewhere (that would hide real cases)"
else
    ok "P6e. SC2148 is not blanket-disabled anywhere"
fi

echo "== P7: the CI step still has the shape this test verifies =="

CI="$REPO_ROOT/.github/workflows/ci.yml"
# Comment lines are stripped before matching: the step's own comments explain WHY
# it does not use xargs, and prose about a command must never be mistaken for the
# command. Only the executable lines are inspected.
CI_STEP="$(awk '/name: ShellCheck \(shellcheck is preinstalled/,/name: Install Helm/' "$CI" \
    | grep -v '^[[:space:]]*#')"
for needle in 'set -o pipefail' 'LC_ALL=C.UTF-8' 'mapfile -d' \
              'tee shellcheck-report.txt' 'refusing to pass vacuously'; do
    if printf '%s\n' "$CI_STEP" | grep -q -- "$needle"; then
        ok "P7. the CI ShellCheck step still runs: $needle"
    else
        bad "P7. the CI ShellCheck step no longer runs: $needle"
    fi
done
if printf '%s\n' "$CI_STEP" | grep -q 'xargs'; then
    bad "P7. the CI step reverted to xargs (status would be xargs's 123, not shellcheck's)"
else
    ok "P7. the CI step does not route through xargs"
fi

echo "== P8: the whole tracked tree is error-clean under the fixed gate =="

cd "$REPO_ROOT"
mapfile -d '' -t tracked < <(git ls-files -z -- '*.sh')
[ "${#tracked[@]}" -gt 0 ]
check $? "P8a. the tracked script set is non-empty (${#tracked[@]} scripts)"

set +e
run_gate "$WORK/tree.txt" "${tracked[@]}"
TREE_RC=$?
set -e
[ "$TREE_RC" -eq 0 ]
check $? "P8b. every tracked script passes -S error (exit $TREE_RC)"
[ "$TREE_RC" -eq 0 ] || sed -n '1,40p' "$WORK/tree.txt"

echo
if [ "$FAIL" -ne 0 ]; then
    echo "== shellcheck gate self-test: $FAIL FAILED, $PASS passed =="
    exit 1
fi
echo "== shellcheck gate self-test: all $PASS checks passed =="
