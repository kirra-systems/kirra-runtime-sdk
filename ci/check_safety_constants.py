#!/usr/bin/env python3
"""EP-09 — RSS / checker safety-constant provenance & sign-off gate.

Verifies every constant declared in `ci/safety_constants_manifest.json`:

  1. the `const NAME: ... = VALUE;` declaration still exists in the manifest's
     file — a moved/renamed constant fails until the manifest is updated
     (the provenance record must follow the code);
  2. the declared value matches the manifest value — a silent retune of a
     safety constant is impossible: any change reds CI until the manifest is
     re-signed alongside it (a visible, reviewable diff);
  3. a `validated` entry carries non-empty provenance and owner — a sign-off
     without recorded evidence is not a sign-off;
  4. under `KIRRA_RELEASE_GATE=1` (the release path), any `pending` entry
     FAILS the build — no VALIDATION-PENDING placeholder can reach a release.

Numeric values compare numerically (so `1_000` == `1000`); symbolic values
(e.g. an inherited `STOP_EPSILON_MPS`) compare as exact identifiers.

Usage: check_safety_constants.py [manifest.json]   (default: the file beside
this script). Exit 0 = green, 1 = gate failure, 2 = usage/manifest error.
"""

import json
import os
import re
import sys

VALID_STATUSES = {"validated", "pending"}


def parse_declared_value(repo_root: str, rel_path: str, name: str):
    """Return the right-hand-side expression of `const NAME: T = <expr>;`."""
    path = os.path.join(repo_root, rel_path)
    try:
        with open(path, encoding="utf-8") as f:
            src = f.read()
    except OSError as e:
        return None, f"cannot read {rel_path}: {e}"
    pattern = re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+"
        + re.escape(name)
        + r"\s*:\s*[^=;]+=\s*(.+?)\s*;",
        re.MULTILINE,
    )
    matches = pattern.findall(src)
    if not matches:
        return None, f"`const {name}` not found in {rel_path}"
    if len(matches) > 1:
        return None, f"`const {name}` declared {len(matches)} times in {rel_path}"
    return matches[0], None


def values_match(manifest_value: str, declared_value: str) -> bool:
    m, d = manifest_value.strip(), declared_value.strip()
    try:
        return float(m.replace("_", "")) == float(d.replace("_", ""))
    except ValueError:
        return m == d


def main() -> int:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    manifest_path = (
        sys.argv[1]
        if len(sys.argv) > 1
        else os.path.join(script_dir, "safety_constants_manifest.json")
    )
    if len(sys.argv) > 2:
        print(__doc__, file=sys.stderr)
        return 2
    repo_root = os.path.dirname(script_dir)

    try:
        with open(manifest_path, encoding="utf-8") as f:
            manifest = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        print(f"FAIL: cannot load manifest {manifest_path}: {e}", file=sys.stderr)
        return 2

    entries = manifest.get("constants")
    if not isinstance(entries, list) or not entries:
        print("FAIL: manifest carries no `constants` entries", file=sys.stderr)
        return 2

    release_gate = os.environ.get("KIRRA_RELEASE_GATE", "") == "1"
    errors: list[str] = []
    pending: list[str] = []
    seen: set[tuple[str, str]] = set()

    for entry in entries:
        name = entry.get("name", "<unnamed>")
        rel_path = entry.get("file", "")
        label = f"{rel_path}::{name}"

        key = (rel_path, name)
        if key in seen:
            errors.append(f"{label}: duplicate manifest entry")
            continue
        seen.add(key)

        status = entry.get("status", "")
        if status not in VALID_STATUSES:
            errors.append(
                f"{label}: status {status!r} is not one of {sorted(VALID_STATUSES)}"
            )
            continue
        if status == "validated" and not (
            str(entry.get("provenance", "")).strip()
            and str(entry.get("owner", "")).strip()
        ):
            errors.append(
                f"{label}: `validated` without recorded provenance/owner — a "
                "sign-off must carry its evidence reference and signer"
            )
            continue

        declared, err = parse_declared_value(repo_root, rel_path, name)
        if err:
            errors.append(f"{label}: {err} (update the manifest alongside the code)")
            continue
        if not values_match(str(entry.get("value", "")), declared):
            errors.append(
                f"{label}: declared value `{declared}` != manifest value "
                f"`{entry.get('value')}` — a safety constant changed without "
                "re-recording its provenance; update BOTH together"
            )
            continue

        if status == "pending":
            pending.append(label)
        print(f"OK   [{status:>9}] {label} = {declared}")

    if pending:
        print(f"\n{len(pending)}/{len(entries)} constants are VALIDATION-PENDING:")
        for label in pending:
            print(f"  pending  {label}")

    if errors:
        print("", file=sys.stderr)
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1

    if release_gate and pending:
        print(
            f"\nFAIL: KIRRA_RELEASE_GATE=1 and {len(pending)} safety constant(s) "
            "are still `pending` — a release is blocked until a safety engineer "
            "signs each off (status → `validated` with provenance) in "
            "ci/safety_constants_manifest.json",
            file=sys.stderr,
        )
        return 1

    print(
        f"\nsafety-constants gate green ({len(entries)} constants verified"
        + (", RELEASE mode)" if release_gate else ")")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
