#!/usr/bin/env bash
#
# Vercel "Ignored Build Step" for the Kirra console.
#
#   EXIT 0  =>  SKIP the build.
#   EXIT 1  =>  RUN the build.
#
# That is Vercel's convention, and it reads backwards from every other exit
# code in this repository. Inverting it does not fail loudly — it either never
# deploys the console again, or never saves a single build. If you change this
# script, re-read those two lines first.
#
# ---------------------------------------------------------------------------
# Why this exists
#
# This repository is overwhelmingly Rust. A typical pull request touches
# crates/ and nothing else, yet every push rebuilds the Next.js console —
# once per connected Vercel project. That is how the account reached its
# build-rate limit while the changes under review contained no console code
# at all.
#
# Vercel builds this project from the console/ Root Directory, so console/ is
# the complete set of inputs. If nothing under it changed, the previous
# deployment is already an accurate preview of the console at this commit and
# rebuilding produces a byte-identical result at the cost of a build slot.
#
# ---------------------------------------------------------------------------
# The fail-safe direction is BUILD, not SKIP
#
# Every uncertain case below exits 1. The two errors are not symmetric:
#
#   * Building unnecessarily costs a build slot. Visible, cheap, self-correcting.
#   * Skipping a build that should have run leaves the previous deployment in
#     place while the preview URL still resolves. A reviewer opens it, sees a
#     page that looks current, and reviews code that was never deployed. The
#     failure is silent and it looks like success.
#
# So this script only ever skips when it can positively demonstrate that
# nothing under console/ changed. "I could not tell" means build.
# ---------------------------------------------------------------------------

set -u

log() { echo "[vercel-ignore] $*"; }

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
    log "not a git checkout — cannot prove nothing changed, so building"
    exit 1
fi

# Vercel clones shallowly. Without a parent commit there is no diff to take.
if ! git -C "$repo_root" rev-parse --verify --quiet 'HEAD^' >/dev/null 2>&1; then
    log "no parent commit in this checkout (shallow clone or root commit) — building"
    exit 1
fi

# Resolved from the repo root rather than the current directory, so the answer
# does not depend on which directory Vercel happens to invoke this from.
if git -C "$repo_root" diff --quiet 'HEAD^' 'HEAD' -- console; then
    log "no changes under console/ between HEAD^ and HEAD — skipping build"
    exit 0
fi

log "console/ changed between HEAD^ and HEAD — building"
exit 1

# ---------------------------------------------------------------------------
# A note on the obvious objection
#
# This compares HEAD against its parent, not against the base branch. On a
# branch whose first commit changed console/ and whose later commits did not,
# the later pushes skip — and that is correct rather than a gap. Vercel built
# the earlier commit, and since console/ has not changed since, that existing
# deployment IS the current console for this branch. The preview URL stays
# accurate.
#
# The one case it does not cover: if that earlier build failed or was itself
# skipped, later pushes will keep skipping until something under console/
# changes again. Re-run the deployment from the Vercel dashboard if you land
# there. Widening this to diff against the base branch would fix it, but the
# base ref is not reliably available to an ignore step, and guessing it wrong
# fails in the silent direction this script is written to avoid.
# ---------------------------------------------------------------------------
