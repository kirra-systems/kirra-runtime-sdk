#!/usr/bin/env python3
"""File/update/close the scheduled-advisory-scan tracking issue (#797 F4).

Runs as the last step of .github/workflows/advisories-cron.yml, after the two
cargo-deny advisory scans (root + parko, continue-on-error). Behavior:

* any scan FAILED  -> ensure ONE open tracking issue exists: create it if
  absent, else append a comment (so repeated nightly findings thread into one
  issue instead of spamming new ones).
* both scans PASSED -> if an open tracking issue exists, comment and CLOSE it
  (the advisory was bumped or triaged; the loop self-resolves).

Identity: the issue is found by its exact TITLE marker, not labels — label
creation needs repo-admin steps this script must not depend on.

Pure stdlib (urllib): no third-party action, nothing new to SHA-pin (M-6).
Exit is ALWAYS 0 on a completed filing decision — the red signal is the
ISSUE, not this workflow — but an API failure exits 1 loudly (a scan that
cannot report is not a scan).
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request

TITLE = "[advisories] scheduled RUSTSEC scan findings"

TRIAGE = (
    "Triage (same policy as the gating PR lane): bump the affected "
    "dependency, or add a scoped, dated entry to `deny.toml` "
    "`[advisories].ignore` with a written justification + expiry. "
    "This issue is updated by `.github/workflows/advisories-cron.yml` and "
    "closes itself on the first clean scan."
)


def api(path: str, method: str = "GET", body: dict | None = None):
    repo = os.environ["GITHUB_REPOSITORY"]
    req = urllib.request.Request(
        f"https://api.github.com/repos/{repo}{path}",
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"Bearer {os.environ['GITHUB_TOKEN']}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


def find_open_tracking_issue() -> int | None:
    # Newest 100 open issues is ample: there is at most one tracking issue.
    for issue in api("/issues?state=open&per_page=100"):
        if issue.get("title") == TITLE and "pull_request" not in issue:
            return issue["number"]
    return None


def main() -> int:
    outcomes = {
        "root workspace": os.environ["OUTCOME_ROOT"],
        "parko workspace": os.environ["OUTCOME_PARKO"],
    }
    run_url = os.environ["RUN_URL"]
    failed = [ws for ws, oc in outcomes.items() if oc != "success"]
    existing = find_open_tracking_issue()

    if failed:
        summary = ", ".join(failed)
        body = (
            f"Scheduled RUSTSEC advisory scan found advisories in: **{summary}**.\n\n"
            f"Full cargo-deny output: {run_url}\n\n{TRIAGE}"
        )
        if existing is None:
            created = api("/issues", "POST", {"title": TITLE, "body": body})
            print(f"filed tracking issue #{created['number']} ({summary})")
        else:
            api(f"/issues/{existing}/comments", "POST", {"body": body})
            print(f"updated tracking issue #{existing} ({summary})")
    else:
        if existing is not None:
            api(
                f"/issues/{existing}/comments",
                "POST",
                {"body": f"Scheduled scan is clean again ({run_url}) — closing."},
            )
            api(f"/issues/{existing}", "PATCH", {"state": "closed"})
            print(f"closed tracking issue #{existing} (scan clean)")
        else:
            print("scan clean; no tracking issue open — nothing to do")
    return 0


if __name__ == "__main__":
    sys.exit(main())
