#!/usr/bin/env bash
# pin-actions.sh — pin GitHub Actions to full commit SHAs (M-6).
#
# A `uses: owner/repo@v4` reference trusts a MUTABLE tag: a compromised action
# maintainer (or a hijacked account) can re-point that tag at malicious code,
# which then runs in CI with this repo's GITHUB_TOKEN and secrets. Pinning to a
# full 40-hex commit SHA makes the reference immutable; Dependabot
# (.github/dependabot.yml) then maintains the SHA as new versions ship.
#
# This script resolves each tag/branch ref to its commit SHA via `git ls-remote`
# (no GitHub auth, no extra tooling) and rewrites the workflow in place as
#   uses: owner/repo@<40-hex-sha> # <original-ref>
# It is IDEMPOTENT: an already-SHA-pinned ref is left untouched, and local
# (`./...`) / docker (`docker://...`) action refs are skipped.
#
# Usage:
#   scripts/pin-actions.sh            # resolve + rewrite (needs network egress to github.com)
#   scripts/pin-actions.sh --check    # CI gate: exit 1 if any mutable-tag ref remains; no writes
#
# NOTE: this repo's build sandbox blocks egress to github.com (policy 403), so
# the apply mode must run in a networked environment (CI or a maintainer host).
set -euo pipefail

WORKFLOW_DIR=".github/workflows"
CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

# Extract "owner/repo@ref" from a `uses:` line (ignores local/docker refs).
# A ref is "pinned" iff it is exactly 40 lowercase hex chars.
is_sha() { [[ "$1" =~ ^[0-9a-f]{40}$ ]]; }

resolve_sha() {
  # $1 = owner/repo, $2 = ref (tag or branch). Prints the commit SHA or fails.
  local repo="$1" ref="$2" out
  # Try tag (incl. annotated-tag dereference ^{}), then branch head.
  out=$(git ls-remote "https://github.com/${repo}.git" \
          "refs/tags/${ref}^{}" "refs/tags/${ref}" "refs/heads/${ref}" 2>/dev/null \
        | awk '{print $1}' | tail -1)
  [ -n "$out" ] && { printf '%s\n' "$out"; return 0; }
  return 1
}

unpinned=0
changed=0

while IFS= read -r file; do
  while IFS= read -r line; do
    # Match: optional leading "- ", then uses: owner/repo@ref  (capture repo+ref)
    if [[ "$line" =~ uses:[[:space:]]*([A-Za-z0-9._-]+/[A-Za-z0-9._-]+)@([^[:space:]#]+) ]]; then
      repo="${BASH_REMATCH[1]}"; ref="${BASH_REMATCH[2]}"
      is_sha "$ref" && continue   # already pinned
      unpinned=$((unpinned + 1))
      if [ "$CHECK_ONLY" -eq 1 ]; then
        echo "UNPINNED: ${file}: ${repo}@${ref}"
        continue
      fi
      if sha=$(resolve_sha "$repo" "$ref"); then
        # Rewrite this exact ref → sha, append the original ref as a comment.
        # Escape regex-special chars in BOTH repo and ref before using them in the
        # sed PATTERN (a ref like `v0.7` has a `.` that would otherwise match any
        # char). The replacement side keeps the literal ref in the comment.
        esc_repo=$(printf '%s' "$repo" | sed -E 's/[.[\*^$/]/\\&/g')
        esc_ref=$(printf '%s' "$ref" | sed -E 's/[.[\*^$/]/\\&/g')
        sed -i -E "s|uses:([[:space:]]*)${esc_repo}@${esc_ref}([[:space:]]*)\$|uses:\1${repo}@${sha} # ${ref}\2|" "$file"
        echo "PINNED:   ${file}: ${repo}@${ref} -> ${sha}"
        changed=$((changed + 1))
      else
        echo "ERROR: could not resolve ${repo}@${ref} (network egress to github.com required)" >&2
        exit 2
      fi
    fi
  done < "$file"
done < <(find "$WORKFLOW_DIR" -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)

if [ "$CHECK_ONLY" -eq 1 ]; then
  if [ "$unpinned" -gt 0 ]; then
    echo "FAIL: ${unpinned} unpinned action reference(s) — run scripts/pin-actions.sh" >&2
    exit 1
  fi
  echo "OK: all action references are SHA-pinned"
  exit 0
fi

echo "Done: ${changed} reference(s) pinned."
