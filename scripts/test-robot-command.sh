#!/usr/bin/env bash
#
# test-robot-command.sh — self-test for the Robot Command Language
# (scripts/robot-command.sh). PURE SHELL + LOCAL GIT: every case builds a
# throwaway repository with a local BARE remote in a temp dir, so there is no
# network access, no GitHub, and no dependency on this checkout's own state.
#
# It asserts BEHAVIOUR — exit status AND resulting repository state — for both
# phrases, their refusals, and the dispatcher. It also asserts the textual
# ABSENCE of every prohibited destructive Git operation, so a later edit that
# introduces `git reset --hard`, a stash, or a force-push reds this suite.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CMD="${SCRIPT_DIR}/robot-command.sh"

pass=0; fail=0
ok()  { echo "  ok   - $1"; pass=$((pass+1)); }
bad() { echo "  FAIL - $1"; fail=$((fail+1)); }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

OUT="${WORK}/out.txt"

# Run the command under test inside a repo; echo its exit status.
run_in() {
    local repo="$1"; shift
    ( cd "$repo" && bash "$CMD" "$@" ) >"$OUT" 2>&1
    echo $?
}
out() { cat "$OUT"; }

# Deterministic identity + settings, so no case depends on the host's gitconfig.
git_q() { git -c user.name=Test -c user.email=test@example.com -c commit.gpgsign=false "$@"; }

# Build: a bare remote with a `main` holding one commit, plus a clone.
# Echoes "<remote> <clone>".
new_fixture() {
    local name="$1"
    local remote="${WORK}/${name}.git"
    local clone="${WORK}/${name}"
    git init --quiet --bare --initial-branch=main "$remote"
    git init --quiet --initial-branch=main "${clone}.seed"
    (
        cd "${clone}.seed" || exit 1
        echo "base" > base.txt
        git_q add base.txt
        git_q commit --quiet -m "base"
        git_q remote add origin "$remote"
        git_q push --quiet -u origin main
    ) >/dev/null 2>&1
    git clone --quiet "$remote" "$clone" >/dev/null 2>&1
    git -C "$clone" config user.name Test
    git -C "$clone" config user.email test@example.com
    git -C "$clone" config commit.gpgsign false
    echo "$remote $clone"
}

# Add a commit on a new branch in a clone; echoes the new HEAD sha.
commit_on_branch() {
    local repo="$1" branch="$2" file="$3"
    git -C "$repo" switch --quiet --create "$branch" >/dev/null 2>&1
    echo "content" > "${repo}/${file}"
    git -C "$repo" add "$file" >/dev/null 2>&1
    git -C "$repo" commit --quiet -m "add ${file}" >/dev/null 2>&1
    git -C "$repo" rev-parse HEAD
}

sha()   { git -C "$1" rev-parse "${2:-HEAD}"; }
# Bare `git rev-parse <missing-ref>` ECHOES the ref name and exits non-zero, so
# `|| echo none` yields two lines. `--verify --quiet` is the clean plumbing query:
# a sha or nothing.
remote_ref() { git -C "$1" rev-parse --verify --quiet "$2" || echo none; }
branch_of() { git -C "$1" symbolic-ref --quiet --short HEAD 2>/dev/null || echo "<detached>"; }
clean_p() { [ -z "$(git -C "$1" status --porcelain=v1 --untracked-files=all)" ]; }

echo "== robot-command.sh self-test =="

# ---------------------------------------------------------------------------
echo "-- Sync to main --"

# S1. Clean feature branch switches and synchronizes to main.
#     Also covers: no merge commit created; final HEAD == origin/main.
read -r REMOTE REPO <<<"$(new_fixture s1)"
commit_on_branch "$REPO" feature/x f.txt >/dev/null
# Advance the remote main so the sync has real work to do.
( cd "${WORK}/s1.seed" && echo more > more.txt && git_q add more.txt \
    && git_q commit --quiet -m "remote advance" && git_q push --quiet origin main ) >/dev/null 2>&1
remote_main_sha="$(git -C "$REMOTE" rev-parse refs/heads/main)"
parents_before=$(git -C "$REPO" rev-list --count --all)
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "0" ] && [ "$(branch_of "$REPO")" = "main" ] \
   && [ "$(sha "$REPO")" = "$remote_main_sha" ]; then
    ok "S1 clean feature branch switches to main and synchronizes"
else
    bad "S1 expected rc=0 on main at $remote_main_sha (rc=$rc branch=$(branch_of "$REPO") head=$(sha "$REPO"))"
    out
fi
# S8/S9. No merge commit: HEAD is exactly origin/main and has a single parent.
nparents=$(git -C "$REPO" rev-list --parents -n 1 HEAD | wc -w)
if [ "$(sha "$REPO")" = "$(sha "$REPO" refs/remotes/origin/main)" ] && [ "$nparents" -le 2 ]; then
    ok "S8/S9 no merge commit; HEAD equals origin/main"
else
    bad "S8/S9 merge commit or mismatch (parents_words=$nparents before=$parents_before)"
fi

# S2. Already synchronized main succeeds (idempotence).
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "0" ] && out | grep -q "already at origin/main"; then
    ok "S2 already-synchronized main succeeds and reports that state"
else
    bad "S2 expected idempotent success (rc=$rc)"; out
fi

# S3. Dirty TRACKED file refuses; the change survives and the branch is unchanged.
read -r REMOTE REPO <<<"$(new_fixture s3)"
commit_on_branch "$REPO" feature/y f.txt >/dev/null
echo "dirty" >> "${REPO}/base.txt"
before_branch="$(branch_of "$REPO")"
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "5" ] && [ "$(branch_of "$REPO")" = "$before_branch" ] \
   && grep -q dirty "${REPO}/base.txt" && out | grep -q "base.txt"; then
    ok "S3 dirty tracked file refuses (rc=5), work intact, branch unchanged"
else
    bad "S3 expected rc=5 with the file preserved (rc=$rc branch=$(branch_of "$REPO"))"; out
fi

# S4. UNTRACKED file refuses and is not removed.
read -r REMOTE REPO <<<"$(new_fixture s4)"
commit_on_branch "$REPO" feature/z f.txt >/dev/null
echo "scratch" > "${REPO}/untracked.txt"
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "5" ] && [ -f "${REPO}/untracked.txt" ] && out | grep -q "untracked.txt"; then
    ok "S4 untracked file refuses (rc=5) and is not deleted"
else
    bad "S4 expected rc=5 with untracked.txt preserved (rc=$rc)"; out
fi

# S5. Missing origin refuses.
read -r REMOTE REPO <<<"$(new_fixture s5)"
git -C "$REPO" remote remove origin
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "4" ] && out | grep -qi "no 'origin' remote"; then
    ok "S5 missing origin refuses (rc=4)"
else
    bad "S5 expected rc=4 (rc=$rc)"; out
fi

# S6. Missing origin/main refuses. Remote exists but has no `main`.
read -r REMOTE REPO <<<"$(new_fixture s6)"
git -C "$REMOTE" branch -m main other >/dev/null 2>&1
git -C "$REPO" update-ref -d refs/remotes/origin/main >/dev/null 2>&1
git -C "$REPO" branch -D main >/dev/null 2>&1 || true
git -C "$REPO" switch --quiet --create feature/w >/dev/null 2>&1
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "6" ] && out | grep -q "does not exist after fetching"; then
    ok "S6 missing origin/main refuses (rc=6)"
else
    bad "S6 expected rc=6 (rc=$rc)"; out
fi

# S7. Diverged local main refuses because fast-forward is impossible; the local
#     commit survives and the operator is left where they started.
read -r REMOTE REPO <<<"$(new_fixture s7)"
# Local main gains a commit the remote will never have...
echo local > "${REPO}/local_only.txt"
git -C "$REPO" add local_only.txt >/dev/null 2>&1
git -C "$REPO" commit --quiet -m "local only" >/dev/null 2>&1
diverged_sha="$(sha "$REPO")"
# ...and the remote main advances independently.
( cd "${WORK}/s7.seed" && echo r > r.txt && git_q add r.txt \
    && git_q commit --quiet -m "remote only" && git_q push --quiet origin main ) >/dev/null 2>&1
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "7" ] && [ "$(sha "$REPO")" = "$diverged_sha" ] \
   && [ -f "${REPO}/local_only.txt" ] && out | grep -q "fast-forward is impossible"; then
    ok "S7 diverged local main refuses (rc=7); local commit preserved"
else
    bad "S7 expected rc=7 with HEAD still $diverged_sha (rc=$rc head=$(sha "$REPO"))"; out
fi

# ---------------------------------------------------------------------------
echo "-- Publish my work --"

# P1. New feature branch is pushed and the upstream is created.
read -r REMOTE REPO <<<"$(new_fixture p1)"
head_sha="$(commit_on_branch "$REPO" feature/new n.txt)"
rc=$(run_in "$REPO" "Publish my work")
remote_sha="$(remote_ref "$REMOTE" refs/heads/feature/new)"
upstream="$(git -C "$REPO" for-each-ref --format='%(upstream:short)' refs/heads/feature/new)"
if [ "$rc" = "0" ] && [ "$remote_sha" = "$head_sha" ] \
   && [ "$upstream" = "origin/feature/new" ] && out | grep -q "upstream created *yes"; then
    ok "P1 new branch pushed, upstream created, remote matches HEAD"
else
    bad "P1 expected rc=0 remote=$head_sha upstream=origin/feature/new (rc=$rc remote=$remote_sha up=$upstream)"; out
fi

# P2. Existing upstream pushes normally (a second commit on the same branch).
echo again > "${REPO}/n2.txt"
git -C "$REPO" add n2.txt >/dev/null 2>&1
git -C "$REPO" commit --quiet -m "second" >/dev/null 2>&1
head2="$(sha "$REPO")"
rc=$(run_in "$REPO" "Publish my work")
remote_sha="$(git -C "$REMOTE" rev-parse refs/heads/feature/new)"
if [ "$rc" = "0" ] && [ "$remote_sha" = "$head2" ] && out | grep -q "upstream created *no"; then
    ok "P2 existing upstream pushes normally without recreating it"
else
    bad "P2 expected rc=0 remote=$head2 (rc=$rc remote=$remote_sha)"; out
fi

# P9. Final local HEAD equals its remote-tracking branch.
if [ "$(sha "$REPO")" = "$(sha "$REPO" refs/remotes/origin/feature/new)" ]; then
    ok "P9 local HEAD equals its remote-tracking branch"
else
    bad "P9 HEAD != refs/remotes/origin/feature/new"
fi

# P3. Dirty TRACKED file refuses; no push happens.
read -r REMOTE REPO <<<"$(new_fixture p3)"
commit_on_branch "$REPO" feature/dirty d.txt >/dev/null
echo dirty >> "${REPO}/base.txt"
rc=$(run_in "$REPO" "Publish my work")
pushed="$(remote_ref "$REMOTE" refs/heads/feature/dirty)"
if [ "$rc" = "5" ] && [ "$pushed" = "none" ] && grep -q dirty "${REPO}/base.txt"; then
    ok "P3 dirty tracked file refuses (rc=5); nothing pushed, nothing committed"
else
    bad "P3 expected rc=5 and no remote branch (rc=$rc pushed=$pushed)"; out
fi

# P4. UNTRACKED file refuses; the file survives and nothing is pushed.
read -r REMOTE REPO <<<"$(new_fixture p4)"
commit_on_branch "$REPO" feature/untracked u.txt >/dev/null
echo scratch > "${REPO}/extra.txt"
rc=$(run_in "$REPO" "Publish my work")
pushed="$(remote_ref "$REMOTE" refs/heads/feature/untracked)"
if [ "$rc" = "5" ] && [ "$pushed" = "none" ] && [ -f "${REPO}/extra.txt" ]; then
    ok "P4 untracked file refuses (rc=5); file preserved, nothing pushed"
else
    bad "P4 expected rc=5 and no remote branch (rc=$rc pushed=$pushed)"; out
fi

# P5. Running from main refuses.
read -r REMOTE REPO <<<"$(new_fixture p5)"
rc=$(run_in "$REPO" "Publish my work")
if [ "$rc" = "9" ] && out | grep -q "refusing to publish directly from 'main'"; then
    ok "P5 publishing from main refuses (rc=9)"
else
    bad "P5 expected rc=9 (rc=$rc)"; out
fi

# P6. Detached HEAD refuses.
read -r REMOTE REPO <<<"$(new_fixture p6)"
commit_on_branch "$REPO" feature/det x.txt >/dev/null
git -C "$REPO" switch --quiet --detach HEAD >/dev/null 2>&1
rc=$(run_in "$REPO" "Publish my work")
if [ "$rc" = "8" ] && out | grep -q "HEAD is detached"; then
    ok "P6 detached HEAD refuses (rc=8)"
else
    bad "P6 expected rc=8 (rc=$rc branch=$(branch_of "$REPO"))"; out
fi

# P7. Missing origin refuses.
read -r REMOTE REPO <<<"$(new_fixture p7)"
commit_on_branch "$REPO" feature/noremote y.txt >/dev/null
git -C "$REPO" remote remove origin
rc=$(run_in "$REPO" "Publish my work")
if [ "$rc" = "4" ]; then
    ok "P7 missing origin refuses (rc=4)"
else
    bad "P7 expected rc=4 (rc=$rc)"; out
fi

# P8. No force-push: a diverged remote branch must make the push FAIL and leave
#     remote history untouched. This is the behavioural half of the guarantee.
read -r REMOTE REPO <<<"$(new_fixture p8)"
commit_on_branch "$REPO" feature/race f1.txt >/dev/null
rc=$(run_in "$REPO" "Publish my work")   # establish the upstream
if [ "$rc" != "0" ]; then bad "P8 setup push should succeed (rc=$rc)"; fi
# A second clone advances the same remote branch behind our back.
git clone --quiet "$REMOTE" "${WORK}/p8-other" >/dev/null 2>&1
git -C "${WORK}/p8-other" config user.name Test
git -C "${WORK}/p8-other" config user.email test@example.com
git -C "${WORK}/p8-other" switch --quiet feature/race >/dev/null 2>&1
echo other > "${WORK}/p8-other/other.txt"
git -C "${WORK}/p8-other" add other.txt >/dev/null 2>&1
git -C "${WORK}/p8-other" commit --quiet -m "other side" >/dev/null 2>&1
git -C "${WORK}/p8-other" push --quiet origin feature/race >/dev/null 2>&1
remote_before="$(git -C "$REMOTE" rev-parse refs/heads/feature/race)"
# Now our side commits and tries to publish — a non-fast-forward.
echo mine > "${REPO}/mine.txt"
git -C "$REPO" add mine.txt >/dev/null 2>&1
git -C "$REPO" commit --quiet -m "my side" >/dev/null 2>&1
rc=$(run_in "$REPO" "Publish my work")
remote_after="$(git -C "$REMOTE" rev-parse refs/heads/feature/race)"
if [ "$rc" = "11" ] && [ "$remote_after" = "$remote_before" ]; then
    ok "P8 non-fast-forward push fails (rc=11); remote history NOT overwritten"
else
    bad "P8 expected rc=11 and remote unchanged at $remote_before (rc=$rc after=$remote_after)"; out
fi

# P8b. Static half: no prohibited destructive operation appears in the source.
forbidden_found=""
# Strip full-line comments: the script's own header names these operations in
# order to promise it does not perform them, which must not count as a hit.
CMD_CODE="${WORK}/cmd-code.sh"
grep -vE '^[[:space:]]*#' "$CMD" > "$CMD_CODE"
while IFS= read -r pattern; do
    if grep -Eq -- "$pattern" "$CMD_CODE"; then
        forbidden_found="${forbidden_found} [${pattern}]"
    fi
done <<'PATTERNS'
reset[[:space:]]+--hard
git[[:space:]]+clean
git[[:space:]]+stash
--force
-f[[:space:]]+origin
commit[[:space:]]+-m
PATTERNS
if [ -z "$forbidden_found" ]; then
    ok "P8b no prohibited destructive git operation appears in robot-command.sh"
else
    bad "P8b prohibited operation(s) present:${forbidden_found}"
fi

# ---------------------------------------------------------------------------
echo "-- Dispatcher --"

read -r REMOTE REPO <<<"$(new_fixture d1)"
commit_on_branch "$REPO" feature/disp q.txt >/dev/null
git -C "$REPO" switch --quiet main >/dev/null 2>&1

# D1/D2/D3. Exact, case-insensitive, and whitespace-padded phrases all dispatch.
#           "Sync to main" on an already-synced main is the safe probe.
rc=$(run_in "$REPO" "Sync to main")
if [ "$rc" = "0" ]; then ok "D1 exact phrase dispatches"; else bad "D1 rc=$rc"; out; fi

rc=$(run_in "$REPO" "sync to main")
if [ "$rc" = "0" ]; then ok "D2 lowercase phrase dispatches"; else bad "D2 rc=$rc"; out; fi

rc=$(run_in "$REPO" "  SYNC   TO   MAIN  ")
if [ "$rc" = "0" ]; then ok "D2b mixed case + padded/internal whitespace dispatches"; else bad "D2b rc=$rc"; out; fi

rc=$(run_in "$REPO" "  Publish my work  ")
if [ "$rc" = "9" ]; then
    ok "D3 padded 'Publish my work' dispatches (refuses on main, as designed)"
else
    bad "D3 expected the publish path to run and refuse with rc=9 (rc=$rc)"; out
fi

# D4. Unknown phrase fails without mutating the repository.
head_before="$(sha "$REPO")"; branch_before="$(branch_of "$REPO")"
refs_before="$(git -C "$REPO" for-each-ref | sha1sum)"
rc=$(run_in "$REPO" "delete everything")
refs_after="$(git -C "$REPO" for-each-ref | sha1sum)"
if [ "$rc" = "2" ] && [ "$(sha "$REPO")" = "$head_before" ] \
   && [ "$(branch_of "$REPO")" = "$branch_before" ] && [ "$refs_before" = "$refs_after" ] \
   && out | grep -q "Sync to main"; then
    ok "D4 unknown phrase exits 2, mutates nothing, prints supported phrases"
else
    bad "D4 expected rc=2 with no mutation (rc=$rc)"; out
fi

# D5. Wrong argument count is a usage error, not a partial run.
rc=$(run_in "$REPO")
if [ "$rc" = "2" ]; then ok "D5 missing argument is a usage error (rc=2)"; else bad "D5 rc=$rc"; out; fi

# D6. Every invocation left the probe repo clean (no command dirtied it).
if clean_p "$REPO"; then
    ok "D6 dispatcher probes left the working tree clean"
else
    bad "D6 working tree was dirtied by a dispatcher probe"
fi

# ---------------------------------------------------------------------------
echo ""
echo "== $pass passed, $fail failed =="
if [ "$fail" -ne 0 ]; then exit 1; fi
