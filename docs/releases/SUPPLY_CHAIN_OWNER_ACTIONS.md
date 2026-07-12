# Release supply-chain — owner actions (repo settings, not code)

Issue #798 (F7) requires two protections that live in GitHub **settings**, not
in this repository's files. The workflow half is already merged (the `release`
job runs in the `release` Environment — `.github/workflows/release.yml`); the
two settings below are what make it bite. Until they are configured, the
threat F7 names remains open: **any user with push access can push a `v9.9.9`
tag and mint signatures under the release identity** (the exact-identity
verify instructions merged with #798 stop such a signature from verifying *as
a different release*, but not from being minted).

## 1. Protect `v*` tags (ruleset)

Settings → Rules → Rulesets → **New tag ruleset**:

- Name: `release-tags`; Enforcement: **Active**
- Target tags → Add target → Include by pattern: `v*`
- Rules: enable **Restrict creations**, **Restrict updates**, **Restrict
  deletions** (updates/deletions matter doubly here: keyless signing means a
  deleted-and-re-pushed tag leaves conflicting Rekor entries for one identity
  — the workflow comments already forbid it as policy; the ruleset makes it
  mechanical)
- Bypass list: only the release manager(s)

Equivalent API call (`gh api`):

```sh
gh api repos/kirra-systems/kirra-runtime-sdk/rulesets -X POST --input - <<'JSON'
{
  "name": "release-tags",
  "target": "tag",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
  "rules": [ { "type": "creation" }, { "type": "update" }, { "type": "deletion" } ]
}
JSON
```

(Then add your bypass actors in the UI — bypass lists take actor IDs.)

## 2. Required reviewers on the `release` Environment

Settings → Environments → `release` (auto-created by the first post-#798
release run; create it manually if it does not exist yet):

- **Required reviewers**: add the release manager(s). A pushed tag then
  builds everything, but the `release` job — the ONLY job holding
  `id-token: write` — waits for human approval before signing or publishing.
- Optionally **Deployment branches and tags** → restrict to `v*` tags only
  (defense in depth with the ruleset above).

## 3. Verification quick reference (post-#798)

- Release artifacts: the release notes carry the **exact** identity for that
  version — `…/release.yml@refs/tags/vX.Y.Z`. No `.*` regexps.
- Container images: exact identity `…/docker.yml@refs/tags/vX.Y.Z` (releases)
  or `…@refs/heads/main` (main pushes); if a regexp is unavoidable, anchor it
  end-to-end (see the comment in `.github/workflows/docker.yml`).
- `install.sh` verifies `SHA256SUMS` authenticity with cosign automatically
  when cosign is installed; `KIRRA_REQUIRE_SIGNED=1` makes it mandatory
  (fail-closed).

## 4. Roadmap remainder (tracked, not yet merged)

- **SLSA L2→L3 provenance for the tarballs**: adopt
  `actions/attest-build-provenance` (or `slsa-github-generator`) in the
  release workflow — L3 additionally isolates signing from `npm install` /
  `build.rs` structurally. Deferred from the #798 PR only because the repo's
  SHA-pinning rule (#775 F2) requires pinning the new action by commit SHA,
  and resolving that SHA needs repo access outside this session's scope —
  a one-line follow-up with the SHA in hand. (Container images already get
  SLSA provenance via BuildKit `provenance: mode=max`, merged with #798.)
- QNX judge tarball SBOM: tracked in #790.
