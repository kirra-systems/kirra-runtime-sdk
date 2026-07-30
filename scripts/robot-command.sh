#!/usr/bin/env bash
# scripts/robot-command.sh — the Robot Command Language: spoken workflow phrases
# mapped to safe, auditable Git state transitions.
#
# Usage:
#   ./scripts/robot-command.sh "Sync to main"
#   ./scripts/robot-command.sh "Publish my work"
#
# Matching is case-insensitive and tolerates surrounding whitespace.
#
# WHY THIS EXISTS
# A phrase like "sync to main" names a DESTINATION STATE, not a script. The
# operator means "put me on a clean main that matches the remote". The unsafe
# ways to get there — stash, reset --hard, checkout -B, a merge commit — all
# reach a state that *looks* right while local work is gone or history is
# rewritten. This layer refuses instead: every command validates its
# preconditions first, stops on the first unmet one, and never destroys,
# hides, or rewrites local work.
#
# 🔴 PROHIBITED, BY CONSTRUCTION — this script contains no call to
#    `git reset --hard`, `git clean`, `git stash`, any force-push, any
#    automatic commit, or any automatic conflict resolution. The
#    `scripts/test-robot-command.sh` suite asserts their textual absence, so a
#    later edit that introduces one reds the test.
#
# Documentation: docs/ROBOT_COMMAND_LANGUAGE.md

set -euo pipefail

# --- exit statuses (distinct, so a caller can branch on the refusal) ---------
readonly EX_OK=0
readonly EX_USAGE=2           # unknown phrase / bad invocation
readonly EX_NOT_A_REPO=3      # not inside a Git worktree
readonly EX_NO_ORIGIN=4       # no `origin` remote
readonly EX_DIRTY=5           # working tree has changes or untracked files
readonly EX_NO_REMOTE_MAIN=6  # origin/main missing
readonly EX_NOT_FF=7          # fast-forward impossible (diverged)
readonly EX_DETACHED=8        # detached HEAD
readonly EX_ON_MAIN=9         # publish refused from main
readonly EX_NO_COMMITS=10     # branch has no commits to publish
readonly EX_PUSH_FAILED=11    # the push itself failed
readonly EX_VERIFY_FAILED=12  # post-condition verification failed

readonly MAIN_BRANCH="main"
readonly REMOTE="origin"

# --- reporting ---------------------------------------------------------------
say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
field() { printf '  %-22s %s\n' "$1" "$2"; }

# Refusals go to stderr with an actionable next step, so a caller that only
# captures stdout still fails loudly rather than parsing a success-shaped log.
refuse() {
    local code="$1"; shift
    printf 'REFUSED: %s\n' "$1" >&2
    shift || true
    while [ "$#" -gt 0 ]; do printf '  %s\n' "$1" >&2; shift; done
    exit "$code"
}

# --- validation helpers (plumbing only — never parse human-oriented output) --

require_worktree() {
    # `--is-inside-work-tree` prints true/false and fails outside a repo.
    if [ "$(git rev-parse --is-inside-work-tree 2>/dev/null || echo false)" != "true" ]; then
        refuse "$EX_NOT_A_REPO" \
            "not inside a Git worktree." \
            "Run this from within the repository checkout."
    fi
}

require_origin() {
    if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
        refuse "$EX_NO_ORIGIN" \
            "no '$REMOTE' remote is configured." \
            "Add one:  git remote add $REMOTE <url>"
    fi
}

# The clean check. `--porcelain=v1` is a documented STABLE machine format and
# `--untracked-files=all` counts untracked files as dirty on purpose: an
# untracked file is unpublished work, and a command that switched branches
# around it could silently leave it stranded or collide with an incoming file.
working_tree_status() {
    git status --porcelain=v1 --untracked-files=all
}

require_clean_worktree() {
    local dirty
    dirty="$(working_tree_status)"
    if [ -n "$dirty" ]; then
        # Report the files rather than acting on them. Deliberately no stash,
        # no reset, no checkout: the operator decides what happens to their work.
        printf 'REFUSED: %s\n' "the working tree is not clean." >&2
        printf '  %s\n' "Changed or untracked paths:" >&2
        printf '%s\n' "$dirty" | sed 's/^/    /' >&2
        printf '  %s\n' "Commit them, or move them aside yourself, then re-run." >&2
        printf '  %s\n' "This command will not stash, reset, or discard anything." >&2
        exit "$EX_DIRTY"
    fi
}

# The current branch, or empty on detached HEAD. `symbolic-ref` is the plumbing
# form: it exits non-zero when HEAD is not a symbolic ref, which IS the
# detached-HEAD test — no output parsing.
current_branch() {
    git symbolic-ref --quiet --short HEAD 2>/dev/null || true
}

ref_exists() {
    git rev-parse --verify --quiet "$1" >/dev/null 2>&1
}

# --- «Sync to main» ---------------------------------------------------------
#
# Destination state: on a local `main` whose HEAD is exactly origin/main, with a
# clean working tree. Idempotent — running it when already there succeeds.
cmd_sync_to_main() {
    say "== Sync to main =="

    require_worktree
    require_origin
    # Checked BEFORE any network or branch operation, so a dirty tree costs
    # nothing and changes nothing.
    require_clean_worktree

    info "fetching $REMOTE (with prune)..."
    if ! git fetch --prune "$REMOTE"; then
        refuse "$EX_NO_ORIGIN" \
            "could not fetch from '$REMOTE'." \
            "Check network access and the remote URL."
    fi

    local remote_main="refs/remotes/$REMOTE/$MAIN_BRANCH"
    if ! ref_exists "$remote_main"; then
        refuse "$EX_NO_REMOTE_MAIN" \
            "'$REMOTE/$MAIN_BRANCH' does not exist after fetching." \
            "This repository has no '$MAIN_BRANCH' branch on '$REMOTE'."
    fi

    local target
    target="$(git rev-parse "$remote_main")"

    # Fast-forwardability is checked BEFORE switching branches. `merge --ff-only`
    # would refuse on its own, but only after the switch — this way a diverged
    # local main leaves the operator exactly where they started.
    if ref_exists "refs/heads/$MAIN_BRANCH"; then
        local local_main
        local_main="$(git rev-parse "refs/heads/$MAIN_BRANCH")"
        if ! git merge-base --is-ancestor "$local_main" "$target"; then
            refuse "$EX_NOT_FF" \
                "local '$MAIN_BRANCH' has diverged from '$REMOTE/$MAIN_BRANCH' — fast-forward is impossible." \
                "local  $MAIN_BRANCH:  $local_main" \
                "remote $REMOTE/$MAIN_BRANCH: $target" \
                "Local '$MAIN_BRANCH' holds commits the remote does not." \
                "Resolve it yourself (rebase or merge deliberately, or move them to a branch)." \
                "This command will not rebase, merge-commit, or reset."
        fi
        if [ "$(current_branch)" != "$MAIN_BRANCH" ]; then
            info "switching to '$MAIN_BRANCH'..."
            git switch "$MAIN_BRANCH"
        fi
    else
        info "local '$MAIN_BRANCH' does not exist — creating it to track $REMOTE/$MAIN_BRANCH..."
        git switch --create "$MAIN_BRANCH" --track "$REMOTE/$MAIN_BRANCH"
    fi

    # `--ff-only` is the load-bearing primitive: it advances the branch pointer
    # or fails. It cannot create a merge commit and cannot rebase.
    if [ "$(git rev-parse HEAD)" != "$target" ]; then
        info "fast-forwarding to $REMOTE/$MAIN_BRANCH..."
        if ! git merge --ff-only "$remote_main"; then
            refuse "$EX_NOT_FF" \
                "fast-forward to '$REMOTE/$MAIN_BRANCH' failed." \
                "No merge commit was created and nothing was reset."
        fi
    else
        info "already at $REMOTE/$MAIN_BRANCH — nothing to fast-forward."
    fi

    # --- post-condition verification ---
    local head branch
    head="$(git rev-parse HEAD)"
    branch="$(current_branch)"
    if [ "$head" != "$target" ] || [ "$branch" != "$MAIN_BRANCH" ]; then
        refuse "$EX_VERIFY_FAILED" \
            "post-condition check failed after sync." \
            "branch: ${branch:-<detached>} (expected $MAIN_BRANCH)" \
            "HEAD:   $head (expected $target)"
    fi
    if [ -n "$(working_tree_status)" ]; then
        refuse "$EX_VERIFY_FAILED" \
            "the working tree is unexpectedly dirty after sync."
    fi

    say ""
    say "State: synchronized."
    field "branch" "$branch"
    field "commit" "$head"
    field "commit (short)" "$(git rev-parse --short HEAD)"
    field "upstream" "$REMOTE/$MAIN_BRANCH"
    field "working tree" "clean (no changes, no untracked files)"
    field "ready for new work" "yes"
    return "$EX_OK"
}

# --- «Publish my work» ------------------------------------------------------
#
# Destination state: the current feature branch exists on origin at exactly the
# local HEAD. Creates no commits, opens no pull request, changes no branch.
cmd_publish_my_work() {
    say "== Publish my work =="

    require_worktree
    require_origin

    local branch
    branch="$(current_branch)"
    if [ -z "$branch" ]; then
        refuse "$EX_DETACHED" \
            "HEAD is detached — there is no branch to publish." \
            "Switch to a branch first:  git switch <branch>"
    fi
    if [ "$branch" = "$MAIN_BRANCH" ]; then
        refuse "$EX_ON_MAIN" \
            "refusing to publish directly from '$MAIN_BRANCH'." \
            "Move your work to a feature branch:  git switch --create <branch>"
    fi

    require_clean_worktree

    if ! ref_exists HEAD; then
        refuse "$EX_NO_COMMITS" \
            "branch '$branch' has no commits yet — nothing to publish." \
            "Commit your work first; this command never creates commits."
    fi

    local head
    head="$(git rev-parse HEAD)"

    # Upstream presence via for-each-ref: plumbing, and empty when unset (unlike
    # `@{upstream}`, which errors).
    local upstream
    upstream="$(git for-each-ref --format='%(upstream:short)' "refs/heads/$branch")"

    local created_upstream="no"
    if [ -z "$upstream" ]; then
        created_upstream="yes"
        info "no upstream configured — creating $REMOTE/$branch..."
        # The safe equivalent of the documented contract. No force flag, so a
        # non-fast-forward push fails instead of overwriting remote history.
        if ! git push --set-upstream "$REMOTE" HEAD; then
            refuse "$EX_PUSH_FAILED" \
                "push failed while creating the upstream for '$branch'." \
                "Nothing local was changed. This command never force-pushes."
        fi
        upstream="$(git for-each-ref --format='%(upstream:short)' "refs/heads/$branch")"
    else
        info "upstream is '$upstream' — pushing..."
        # Plain `git push` honours the configured upstream under push.default=simple
        # and refuses a non-fast-forward. No refspec guessing, no force.
        if ! git push; then
            refuse "$EX_PUSH_FAILED" \
                "push to '$upstream' failed (it is not a fast-forward, or the remote rejected it)." \
                "Nothing local was changed and remote history was not rewritten." \
                "This command never force-pushes; reconcile the branch deliberately."
        fi
    fi

    if [ -z "$upstream" ]; then
        refuse "$EX_VERIFY_FAILED" \
            "the push reported success but no upstream is configured for '$branch'."
    fi

    # --- post-condition verification: the remote-tracking ref must equal HEAD ---
    local tracking_sha
    if ! tracking_sha="$(git rev-parse --verify --quiet "refs/remotes/$upstream")"; then
        refuse "$EX_VERIFY_FAILED" \
            "remote-tracking ref 'refs/remotes/$upstream' does not exist after the push."
    fi
    if [ "$tracking_sha" != "$head" ]; then
        refuse "$EX_VERIFY_FAILED" \
            "remote-tracking branch does not match local HEAD after the push." \
            "local HEAD:      $head" \
            "$upstream: $tracking_sha"
    fi

    say ""
    say "State: published."
    field "branch" "$branch"
    field "commit" "$head"
    field "commit (short)" "$(git rev-parse --short HEAD)"
    field "remote tracking" "$upstream"
    field "upstream created" "$created_upstream"
    field "fully published" "yes (remote matches local HEAD)"
    return "$EX_OK"
}

# --- dispatcher -------------------------------------------------------------

usage() {
    say "Robot Command Language — supported phrases:"
    say ""
    say '  "Sync to main"       Clean local main, exactly synchronized with origin/main.'
    say '  "Publish my work"    Push the current feature branch to origin and verify it.'
    say ""
    say "Usage: $0 \"<phrase>\""
    say "Matching is case-insensitive; surrounding whitespace is ignored."
    say "Documentation: docs/ROBOT_COMMAND_LANGUAGE.md"
}

main() {
    if [ "$#" -ne 1 ]; then
        usage >&2
        exit "$EX_USAGE"
    fi

    local raw="$1"
    # Normalize: strip leading/trailing whitespace, collapse internal runs, lowercase.
    # No `eval` anywhere — the phrase selects a function, it is never executed.
    local phrase
    phrase="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -s '[:space:]' ' ')"
    phrase="${phrase# }"
    phrase="${phrase% }"

    case "$phrase" in
        "sync to main")    cmd_sync_to_main ;;
        "publish my work") cmd_publish_my_work ;;
        *)
            printf 'REFUSED: unknown phrase: %s\n' "$raw" >&2
            printf '  %s\n' "No repository changes were made." >&2
            printf '\n' >&2
            usage >&2
            exit "$EX_USAGE"
            ;;
    esac
}

main "$@"
