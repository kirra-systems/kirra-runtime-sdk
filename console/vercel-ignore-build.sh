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
# once per connected Vercel project. Those rebuilds are pure waste: build
# minutes spent producing a byte-identical result while the changes under
# review contain no console code at all.
#
# They are **not** what exhausts the account's daily limit, and this script
# does not stop the red statuses that limit produces. Read "What this script
# does NOT fix" below before concluding otherwise.
#
# Vercel builds this project from the console/ Root Directory, so console/ is
# the complete set of inputs. If nothing under it changed, the previous
# deployment is already an accurate preview of the console at this commit and
# rebuilding produces a byte-identical result at the cost of a build slot.
#
# ---------------------------------------------------------------------------
# What this script does NOT fix — measured 2026-08-05
#
# This works: PR checks show `Canceled by Ignored Build Step` against a
# SUCCESS status wherever it runs. It saves the build minutes described
# above. It does not stop the account hitting its daily limit, because that
# limit counts something else.
#
# The failures still seen on Rust-only pull requests are:
#
#   Resource is limited - try again in 24 hours
#   (more than 100, code: "api-deployments-free-per-day")
#
# `api-DEPLOYMENTS-per-day`, not builds. It is an account-level cap on
# deployments CREATED, and it is enforced before the build runs — so this
# script never gets the chance to skip anything. On a rate-limited day the
# statuses are red no matter what is written here.
#
# The cause is not this script and cannot be fixed by it: **seven Vercel
# projects are connected to this one repository, every one of them with
# console/ as its Root Directory** — observed on 2026-08-05 as
# kirra-runtime-sdk, -1vdc, -5lt3, -f35q, -hdy4, -og6p, -tgo8. Seven
# deployments per push exhausts a 100/day cap in well under a day of ordinary
# work, and an ignored build still creates a deployment (Vercel lists the
# ignored ones with inspector and preview URLs, so they are objects, not
# no-ops) — check the account's usage page to confirm how they are counted.
#
# Six of those projects are redundant. Removing them is a DASHBOARD action;
# there is no repository-side equivalent, because all seven read THIS file and
# this vercel.json, so any switch here — `git.deploymentEnabled` included —
# turns all seven off together rather than six.
#
# Recorded here rather than in a ticket because this is the file someone
# reaches for when the statuses go red, and the honest answer is "this script
# is working; the problem is upstream of it."
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
