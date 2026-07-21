#!/usr/bin/env python3
"""ghcr tag-ordering guard (#797 F6 / the #1049 A4 registry half).

The version line went backwards across the rebrand (v1.5.0 "Aegis" →
v1.1.2 "Kirra"), so the ghcr registry still carries `1.5.0`/`1.5` image tags
that SEMVER-SORT ABOVE the current code — any semver image-update policy
(Renovate, Flux/Argo semver ranges, `~1`) would "upgrade" a fleet onto the
old Aegis image. The in-repo half of A4 is ci/check_version_ordering.py;
THIS guard covers the registry: no published ghcr tag may semver-exceed the
version being built, except entries in the reviewed legacy allowlist
(ci/ghcr_legacy_tags.json) awaiting owner deletion/re-tag.

Runs in .github/workflows/docker.yml BEFORE the image build/push, with the
job's GITHUB_TOKEN (packages scope). Fail-closed: an unreadable registry is
a red, not a silent green — publishing next to an unverifiable tag set is
the exact hazard.

Exit 0 = ordering sane. Exit 1 = an offending tag (delete/re-tag it, or —
only for pre-existing legacy tags — allowlist it with a justification) or a
registry/API failure.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO_DIR = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_DIR / "ci" / "ghcr_legacy_tags.json"

SEMVER_TAG_RE = re.compile(r"^v?(\d+)\.(\d+)(?:\.(\d+))?$")


def current_version() -> tuple[int, int, int]:
    text = (REPO_DIR / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', text, re.M)
    if not m:
        sys.exit("FAIL cannot read [package] version from Cargo.toml")
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)))


def tag_version(tag: str) -> tuple[int, int, int] | None:
    """Parse a semver-ish tag (1.5.0, 1.5, v2.0.0). Partial tags compare by
    their floor (1.5 == 1.5.0) — that is exactly how a floating-tag consumer
    resolves them. Non-semver tags (main, latest, sha-*, aegis-*) are not
    version-ordered and are ignored."""
    m = SEMVER_TAG_RE.match(tag)
    if not m:
        return None
    return (int(m.group(1)), int(m.group(2)), int(m.group(3) or 0))


def fetch_all_tags(owner: str, package: str, token: str) -> list[str]:
    """Every tag on the container package, via the org endpoint with the
    user endpoint as fallback (the repo may live under either)."""
    headers = {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    last_err: Exception | None = None
    for base in (
        f"https://api.github.com/orgs/{owner}/packages/container/{package}/versions",
        f"https://api.github.com/users/{owner}/packages/container/{package}/versions",
    ):
        tags: list[str] = []
        page = 1
        try:
            while True:
                req = urllib.request.Request(
                    f"{base}?per_page=100&page={page}", headers=headers
                )
                for attempt in range(3):
                    try:
                        with urllib.request.urlopen(req) as resp:
                            batch = json.load(resp)
                        break
                    except urllib.error.URLError as e:
                        if attempt == 2:
                            raise
                        time.sleep(2 * (attempt + 1))
                        last_err = e
                if not batch:
                    return tags
                for v in batch:
                    tags.extend(
                        v.get("metadata", {}).get("container", {}).get("tags", [])
                    )
                page += 1
        except urllib.error.HTTPError as e:
            if e.code == 404:
                last_err = e
                continue  # wrong owner kind — try the other endpoint
            raise
    sys.exit(f"FAIL ghcr package not found under org or user endpoint: {last_err}")


def main() -> int:
    token = os.environ.get("GITHUB_TOKEN", "")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    if not token or "/" not in repository:
        sys.exit("FAIL GITHUB_TOKEN / GITHUB_REPOSITORY not set")
    owner, repo_name = repository.split("/", 1)
    # The container package name defaults to the repo (the verifier image);
    # the dashboard job overrides via GHCR_PACKAGE (its image lives under
    # ghcr.io/<owner>/kirra-dashboard).
    package = os.environ.get("GHCR_PACKAGE", repo_name)

    allowlist = json.loads(ALLOWLIST_PATH.read_text(encoding="utf-8"))
    allowed = {entry["tag"]: entry["reason"] for entry in allowlist}

    cur = current_version()
    tags = fetch_all_tags(owner, package, token)
    offenders = []
    for tag in sorted(set(tags)):
        ver = tag_version(tag)
        if ver is None or ver <= cur:
            continue
        if tag in allowed:
            print(f"warn {tag} semver-exceeds {'.'.join(map(str, cur))} — "
                  f"ALLOWLISTED: {allowed[tag]}")
            continue
        offenders.append(tag)

    # A stale allowlist entry (tag gone, or no longer exceeding) should be
    # removed — decreases never need review.
    for tag in allowed:
        ver = tag_version(tag)
        if tag not in tags or (ver is not None and ver <= cur):
            print(f"warn allowlist entry `{tag}` is stale — remove it from "
                  f"{ALLOWLIST_PATH.name}")

    if offenders:
        print(
            f"\nFAIL these ghcr tags semver-exceed the version being built "
            f"({'.'.join(map(str, cur))}): {', '.join(offenders)}\n"
            "A semver image-update policy would 'upgrade' onto them. Delete or "
            "re-tag them (e.g. aegis-<ver>); only a pre-existing legacy tag may "
            "be allowlisted in ci/ghcr_legacy_tags.json with a justification."
        )
        return 1
    print(f"ghcr tag ordering ok ({len(tags)} tags, none unexpectedly above "
          f"{'.'.join(map(str, cur))})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
