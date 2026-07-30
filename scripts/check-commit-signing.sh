#!/usr/bin/env bash
# check-commit-signing.sh — prove signing WORKS before anyone rewrites history.
#
# WHY THIS EXISTS
#
# A stop hook reported the branch tip as Unverified and prescribed:
#
#     git config user.email … && git config user.name …
#     git commit --amend --no-edit --reset-author
#
# That was run. It returned 0. The commit was still unsigned. The author had
# been correct all along, so `--reset-author` corrected nothing, and the
# signature never appeared because `user.signingkey` pointed at a ZERO-BYTE
# public key with no private key anywhere. The only effect was a new SHA.
#
# Two distinct defects produced that outcome, and this script addresses both:
#
#   1. NO PREFLIGHT. "Signing is configured" was treated as "signing works".
#      `commit.gpgsign=true` says what is REQUIRED, never what is POSSIBLE.
#   2. NO POST-CHECK. `git commit` exiting 0 was treated as proof of a
#      signature. It is not: git can be configured to sign, fail to sign, and
#      still exit 0 (observed here — see `docs/COMMIT_SIGNING.md`).
#
# So: never recommend an amend to add a signature until a DISPOSABLE signing
# operation has actually produced a verifiable signature.
#
# WHAT IT DOES NOT DO
#
# It never amends, commits, stages, checks out, or writes to the working tree
# or the index. The preflight signs a throwaway object in a temporary
# repository under `mktemp -d`. Running this can never churn a SHA — which is
# the entire point, given that churning a SHA is the damage being prevented.
#
# Usage:
#   scripts/check-commit-signing.sh            # preflight + report the range
#   scripts/check-commit-signing.sh --preflight-only
#   scripts/check-commit-signing.sh --range <rev-range>
#
# Exit: 0 signing proven usable · 1 configured but UNUSABLE · 2 not configured
#       (nothing to prove) · 3 usage error.
set -uo pipefail

PREFLIGHT_ONLY=0
RANGE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --preflight-only) PREFLIGHT_ONLY=1; shift ;;
        --range) RANGE="${2-}"; [ -n "$RANGE" ] || { echo "--range needs a value" >&2; exit 3; }; shift 2 ;;
        -h|--help) sed -n '1,40p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 3 ;;
    esac
done

say() { printf '%s\n' "$*"; }
field() { printf '  %-34s %s\n' "$1" "$2"; }

git rev-parse --git-dir >/dev/null 2>&1 || { echo "not a git repository" >&2; exit 3; }

# ── 1. what is CONFIGURED (requirement, not capability) ──────────────────────
GPGSIGN="$(git config --type=bool commit.gpgsign 2>/dev/null || echo false)"
FORMAT="$(git config gpg.format 2>/dev/null || echo openpgp)"
SIGNKEY="$(git config user.signingkey 2>/dev/null || echo '')"

say "commit-signing preflight"
field "commit.gpgsign" "$GPGSIGN"
field "gpg.format" "$FORMAT"
field "user.signingkey" "${SIGNKEY:-(unset)}"

if [ "$GPGSIGN" != "true" ]; then
    say ""
    say "Signing is not required here (commit.gpgsign is not true). Nothing to prove."
    exit 2
fi

# ── 2. does the configured key REFERENCE resolve to anything usable? ─────────
#
# `user.signingkey` for gpg.format=ssh may legitimately be either a path to a
# public key OR a literal key string ("ssh-ed25519 AAAA…"). Both are supported
# by git, so both are accepted here. The private half may live beside the .pub
# OR in an agent — neither is assumed, because assuming the wrong provisioning
# model is how a preflight produces a confident wrong answer.
KEY_EXISTS="n/a"; KEY_EMPTY="n/a"; KEY_KIND="unset"
if [ -n "$SIGNKEY" ]; then
    case "$SIGNKEY" in
        ssh-*|ecdsa-*|sk-*) KEY_KIND="literal-key-string" ;;
        *)
            KEY_KIND="path"
            if [ -e "$SIGNKEY" ]; then
                KEY_EXISTS="yes"
                if [ -s "$SIGNKEY" ]; then KEY_EMPTY="no"; else KEY_EMPTY="YES (zero bytes)"; fi
            else
                KEY_EXISTS="NO"
            fi
            ;;
    esac
fi
field "signingkey kind" "$KEY_KIND"
[ "$KEY_KIND" = "path" ] && { field "key file exists" "$KEY_EXISTS"; field "key file empty" "$KEY_EMPTY"; }

PRIVATE="unknown"
if [ "$KEY_KIND" = "path" ] && [ "$KEY_EXISTS" = "yes" ]; then
    # Convention only — NOT required. Reported as evidence, never as the verdict.
    if [ -s "${SIGNKEY%.pub}" ]; then PRIVATE="found beside .pub"; else PRIVATE="not beside .pub"; fi
fi
if [ -n "${SSH_AUTH_SOCK:-}" ]; then
    PRIVATE="$PRIVATE; ssh-agent socket present"
fi
field "private key evidence" "$PRIVATE"

# ── 3. THE ONLY VERDICT THAT COUNTS: sign something disposable ───────────────
#
# Everything above is circumstantial. A key file can exist, be non-empty, have a
# private half beside it, and still fail to sign. So the verdict comes from
# actually signing — in a throwaway repository, with `commit-tree -S`, touching
# nothing real.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SIGN_OK="no"; SIGN_ERR=""; OBJ=""
if git init --quiet "$TMP/probe" 2>/dev/null; then
    (
        cd "$TMP/probe" || exit 1
        git config user.email "$(git -C "$OLDPWD" config user.email 2>/dev/null || echo probe@example.invalid)"
        git config user.name  "$(git -C "$OLDPWD" config user.name  2>/dev/null || echo probe)"
        git config commit.gpgsign true
        git config gpg.format "$FORMAT"
        [ -n "$SIGNKEY" ] && git config user.signingkey "$SIGNKEY"
        EMPTY_TREE="$(git hash-object -t tree /dev/null)"
        git commit-tree -S -m "signing preflight probe" "$EMPTY_TREE"
    ) >"$TMP/obj" 2>"$TMP/err"
    OBJ="$(cat "$TMP/obj" 2>/dev/null | tr -d '[:space:]')"
    SIGN_ERR="$(head -3 "$TMP/err" 2>/dev/null)"
fi

SIGNATURE_PRESENT="no"
if [ -n "$OBJ" ]; then
    # Success is not the exit code — it is a gpgsig header on the resulting object.
    if git -C "$TMP/probe" cat-file commit "$OBJ" 2>/dev/null | grep -q '^gpgsig'; then
        SIGNATURE_PRESENT="yes"; SIGN_OK="yes"
    fi
fi
field "disposable signing produced object" "${OBJ:-no}"
field "object carries a signature" "$SIGNATURE_PRESENT"

# Verification is reported SEPARATELY from presence: an unverifiable signature
# is a different defect from an absent one, and conflating them is how "N" got
# read as "needs another amend".
VERIFY="not attempted"
if [ "$SIGNATURE_PRESENT" = "yes" ]; then
    if git -C "$TMP/probe" verify-commit "$OBJ" >/dev/null 2>&1; then
        VERIFY="verified"
    else
        VERIFY="present but NOT verifiable locally (no allowedSignersFile is normal)"
    fi
fi
field "signature verification" "$VERIFY"

if [ "$SIGN_OK" != "yes" ]; then
    say ""
    say "RESULT: commit signing is CONFIGURED BUT UNAVAILABLE in this environment."
    [ -n "$SIGN_ERR" ] && say "  git said: $SIGN_ERR"
    say ""
    say "  No amend was attempted, and none should be: amending cannot add a"
    say "  signature that the environment cannot produce. It would only replace"
    say "  one unsigned commit with a different unsigned commit under a new SHA."
    say ""
    say "  Provision a usable ${FORMAT} signing key, re-run this preflight, and"
    say "  only then consider rewriting UNPUSHED history. See docs/COMMIT_SIGNING.md."
    exit 1
fi

say ""
say "RESULT: signing is usable. An amend of UNPUSHED commits can add signatures."
[ "$PREFLIGHT_ONLY" = "1" ] && exit 0

# ── 4. which commits are actually at issue ──────────────────────────────────
#
# Default range is UNPUSHED commits only. That is deliberate and it is the one
# thing the stop hook already got right: commits that exist on the remote must
# not be silently rewritten, and evidence-linked commits must not be rewritten
# at all. Widen with --range only when you have decided to rewrite history.
if [ -z "$RANGE" ]; then
    BRANCH="$(git branch --show-current)"
    if [ -n "$BRANCH" ] && git rev-parse "origin/$BRANCH" >/dev/null 2>&1; then
        RANGE="origin/$BRANCH..HEAD"
    else
        RANGE="origin/HEAD..HEAD"
    fi
fi
say ""
say "signature PRESENCE in range ${RANGE} (unpushed unless --range widened):"
say ""
say "  NOTE: presence is read from the commit object's gpgsig header, NOT from"
say "  %G?. For SSH signatures with no gpg.ssh.allowedSignersFile configured,"
say "  git cannot VERIFY and reports %G? = 'N' — the same character it uses for"
say "  'no signature at all'. Reading N as unsigned is what sent this repository"
say "  into an amend loop against commits that were signed the whole time."
say ""
UNSIGNED=0
while read -r sha subject; do
    [ -n "$sha" ] || continue
    if git cat-file commit "$sha" 2>/dev/null | grep -q '^gpgsig'; then
        state="signed"
    else
        state="UNSIGNED"; UNSIGNED=$((UNSIGNED + 1))
    fi
    printf '  %-10s %-9s %%G?=%s  %s\n' "$sha" "$state" \
        "$(git log --format='%G?' -1 "$sha" 2>/dev/null)" "$subject"
done < <(git log --format='%h %s' "$RANGE" 2>/dev/null)

say ""
if [ "$UNSIGNED" -eq 0 ]; then
    say "  No unsigned commits in range. If a verification hook still complains,"
    say "  it is reading %G? and is wrong — fix the hook, do not amend."
else
    say "  $UNSIGNED unsigned commit(s). Amending is only appropriate for commits"
    say "  that are UNPUSHED and not referenced by a retained evidence artifact."
fi
exit 0
