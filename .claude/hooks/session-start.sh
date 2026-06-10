#!/bin/bash
# SessionStart hook — re-anchor an ephemeral remote container onto origin's
# default-branch HEAD, discarding a known container-snapshot artifact.
#
# Why this exists
# ---------------
# The remote-execution base image for this repo was snapshotted with the working
# tree checked out at a STALE commit (observed: d88d2ba, an old already-merged
# feature-branch tip) carrying uncommitted edits that are ALREADY on main. Every
# container (re)start restores that stale tree. On a talisman-guarded,
# safety-critical repo that is an active hazard: a missed re-anchor can commit a
# stale edit back onto a branch and regress merged work. This hook removes the
# manual step.
#
# Safety design (deliberately conservative — destructive ops are tightly gated)
# -----------------------------------------------------------------------------
#   * Remote-only: no-op outside Claude Code on the web (never touches a dev's
#     local clone).
#   * startup-only: runs on a genuine session/container start, NOT on
#     resume/compact/clear — those fire MID-TASK and must never discard live WIP.
#   * Surgical reset condition: re-anchor ONLY when the tree is dirty AND HEAD is
#     already fully contained in origin/<default> (an ancestor). That precisely
#     matches "sitting on stale, already-merged history with junk edits" and
#     EXCLUDES any branch that carries real unmerged commits (a branch with its
#     own work is not an ancestor of the default branch, so it is left alone).
#   * Fail-safe: any git error → leave the tree untouched and exit 0 (a hook must
#     never wedge session start).
set -uo pipefail

# --- read hook input (JSON on stdin) -----------------------------------------
input="$(cat 2>/dev/null || true)"
source_field="$(printf '%s' "$input" \
  | grep -o '"source"[[:space:]]*:[[:space:]]*"[^"]*"' \
  | head -n1 | sed 's/.*"\([^"]*\)"$/\1/')"

# --- guards ------------------------------------------------------------------
# Only meaningful in the ephemeral remote container.
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
# Only on a real session start; resume/compact/clear happen mid-task.
[ "${source_field:-startup}" = "startup" ] || exit 0

cd "${CLAUDE_PROJECT_DIR:-.}" 2>/dev/null || exit 0
git rev-parse --git-dir >/dev/null 2>&1 || exit 0

DEFAULT_BRANCH="main"

# Refresh the ref; if the network/auth is unavailable, do nothing (fail-safe).
git fetch --quiet origin "$DEFAULT_BRANCH" 2>/dev/null || exit 0

origin_sha="$(git rev-parse --verify --quiet "origin/${DEFAULT_BRANCH}")" || exit 0
head_sha="$(git rev-parse --verify --quiet HEAD)" || exit 0

# Already on origin's default-branch HEAD → nothing to do (idempotent).
[ "$head_sha" = "$origin_sha" ] && exit 0

# Re-anchor ONLY if: working tree dirty AND HEAD is an ancestor of origin/main
# (i.e. HEAD has no commits that aren't already on main — discarding loses
# nothing). A branch with genuine unmerged work fails the ancestor test and is
# preserved.
if ! git diff --quiet 2>/dev/null || ! git diff --cached --quiet 2>/dev/null; then
  if git merge-base --is-ancestor HEAD "origin/${DEFAULT_BRANCH}" 2>/dev/null; then
    echo "session-start: re-anchoring stale container snapshot (${head_sha:0:7}) → origin/${DEFAULT_BRANCH} (${origin_sha:0:7})" >&2
    # -f / --force discards tracked-file modifications (the leaked artifact edits
    # tracked files); git clean -fd then removes any untracked leftovers.
    git checkout -f -B "$DEFAULT_BRANCH" "origin/${DEFAULT_BRANCH}" --quiet 2>/dev/null || exit 0
    git reset --hard "origin/${DEFAULT_BRANCH}" --quiet 2>/dev/null || true
    git clean -fd --quiet 2>/dev/null || true
  else
    echo "session-start: tree is dirty but HEAD has unmerged commits — leaving it untouched (not the snapshot artifact)" >&2
  fi
fi

exit 0
