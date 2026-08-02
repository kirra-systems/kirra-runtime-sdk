#!/usr/bin/env python3
"""location_registry.py — READ-ONLY operator CLI for the typed-destination
registries (named places + saved routes).

The registries are the TRUSTED coordinate sources behind "drive to the
kitchen" / "run route A": operator-calibrated JSON files consumed fail-closed
by the Rust resolver (`kirra-sidecars/src/destination.rs`, loaded by
`planner_service` via KIRRA_DEST_PLACES_PATH / KIRRA_DEST_ROUTES_PATH). This
CLI lets the operator LIST what is registered, VALIDATE a file against the
same rules the resolver enforces, and LOOK UP what a spoken query would
resolve to — without starting any service.

🔴 READ-ONLY by design: this tool never writes, edits, or creates a registry
file. Calibrated coordinates are authored deliberately (measure the pose,
edit the JSON, re-run `validate`) — there is no "add place" convenience that
could install an invented coordinate. It holds no safety authority either
way: the Rust resolver re-validates everything it loads.

Validation mirrors the resolver's rules (the resolver remains authoritative):
  * version == 1;  map_id present and non-empty;
  * every id / name / alias non-empty after normalization and UNIQUE across
    the whole file (one shared key space, so lookups are never ambiguous);
  * every coordinate finite; every route has >= 1 waypoint.

Usage:
  location_registry.py list     [--places P] [--routes R]
  location_registry.py validate [--places P] [--routes R]     (exit 1 on defect)
  location_registry.py lookup QUERY [--places P] [--routes R]

Runs standalone with the stdlib only; pure helpers are host-tested in
location_registry_test.py.
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

REGISTRY_VERSION = 1


def normalize(text: str) -> str:
    """ASCII-lowercase, non-alphanumerics collapse to single spaces, trimmed.
    Mirrors the Rust resolver's `normalize_query` (and voice_route.normalize)
    so this CLI answers lookups the way the resolver would."""
    out: list[str] = []
    pending = False
    for c in text:
        if c.isascii() and c.isalnum():
            if pending and out:
                out.append(" ")
            pending = False
            out.append(c.lower())
        else:
            pending = True
    return "".join(out)


def _is_finite_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def _check_header(doc: dict, what: str, errors: list[str]) -> None:
    if doc.get("version") != REGISTRY_VERSION:
        errors.append(f"{what}: version must be {REGISTRY_VERSION}, got {doc.get('version')!r}")
    map_id = doc.get("map_id")
    if not isinstance(map_id, str) or not normalize(map_id):
        errors.append(f"{what}: map_id is required and must be non-empty")


def _check_keys(entry: dict, idx: int, keys: dict[str, tuple[int, str]],
                what: str, errors: list[str]) -> None:
    """Index one entry's id/name/aliases into the shared key space.

    Uniqueness is tracked by ENTRY POSITION (`idx`), not by id string — two
    distinct entries that share an id must collide, exactly as in the Rust
    resolver's `index_entry_keys` (review: Copilot on #1293). Within one
    entry, repeating its own key (name == id) is fine.
    """
    entry_id = entry.get("id", "?")
    aliases = entry.get("aliases", [])
    if not isinstance(aliases, list) or not all(isinstance(a, str) for a in aliases):
        errors.append(f"{what} {entry_id!r}: aliases must be a list of strings")
        aliases = []
    for raw in [entry.get("id"), entry.get("name"), *aliases]:
        if not isinstance(raw, str):
            errors.append(f"{what} {entry_id!r}: id/name must be strings")
            continue
        key = normalize(raw)
        if not key:
            errors.append(f"{what} {entry_id!r}: key {raw!r} is empty after normalization")
        elif key in keys and keys[key][0] != idx:
            errors.append(
                f"{what} {entry_id!r}: key {raw!r} collides with entry {keys[key][1]!r} — "
                "ids, names and aliases must be unique across the registry"
            )
        else:
            keys[key] = (idx, str(entry_id))


def validate_places(doc: dict) -> list[str]:
    """All defects in a place-registry document (empty list = valid)."""
    errors: list[str] = []
    _check_header(doc, "place registry", errors)
    places = doc.get("places")
    if not isinstance(places, list):
        errors.append("place registry: places must be a list")
        return errors
    keys: dict[str, tuple[int, str]] = {}
    for idx, e in enumerate(places):
        if not isinstance(e, dict):
            errors.append("place registry: every place must be an object")
            continue
        _check_keys(e, idx, keys, "place", errors)
        for field in ("x_m", "y_m"):
            if not _is_finite_number(e.get(field)):
                errors.append(f"place {e.get('id', '?')!r}: {field} must be a finite number")
        if "heading_rad" in e and not _is_finite_number(e["heading_rad"]):
            errors.append(f"place {e.get('id', '?')!r}: heading_rad must be a finite number")
    return errors


def validate_routes(doc: dict) -> list[str]:
    """All defects in a route-registry document (empty list = valid)."""
    errors: list[str] = []
    _check_header(doc, "route registry", errors)
    routes = doc.get("routes")
    if not isinstance(routes, list):
        errors.append("route registry: routes must be a list")
        return errors
    keys: dict[str, tuple[int, str]] = {}
    for idx, e in enumerate(routes):
        if not isinstance(e, dict):
            errors.append("route registry: every route must be an object")
            continue
        _check_keys(e, idx, keys, "route", errors)
        waypoints = e.get("waypoints")
        if not isinstance(waypoints, list) or not waypoints:
            errors.append(f"route {e.get('id', '?')!r}: waypoints must be a non-empty list")
            continue
        for i, w in enumerate(waypoints):
            if not isinstance(w, dict):
                errors.append(f"route {e.get('id', '?')!r}: waypoint {i} must be an object")
                continue
            for field in ("x_m", "y_m"):
                if not _is_finite_number(w.get(field)):
                    errors.append(
                        f"route {e.get('id', '?')!r}: waypoint {i} {field} must be a finite number"
                    )
            if "heading_rad" in w and not _is_finite_number(w["heading_rad"]):
                errors.append(
                    f"route {e.get('id', '?')!r}: waypoint {i} heading_rad must be finite"
                )
    return errors


def lookup(query: str, places_doc: dict | None, routes_doc: dict | None):
    """What the normalized query resolves to: ("place"|"route", entry) or None.
    Exact key match only, like the resolver — no fuzzy matching."""
    key = normalize(query)
    if not key:
        return None
    for kind, doc, field in (("place", places_doc, "places"), ("route", routes_doc, "routes")):
        if not doc:
            continue
        for e in doc.get(field, []):
            candidates = [e.get("id", ""), e.get("name", ""), *e.get("aliases", [])]
            if any(normalize(c) == key for c in candidates if isinstance(c, str)):
                return kind, e
    return None


def _load(path: str | None, what: str) -> dict | None:
    if not path:
        return None
    p = Path(path)
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: {what} {path}: {e}", file=sys.stderr)
        sys.exit(1)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("command", choices=("list", "validate", "lookup"))
    ap.add_argument("query", nargs="?", default=None)
    ap.add_argument("--places", default=None, help="place registry JSON path")
    ap.add_argument("--routes", default=None, help="route registry JSON path")
    args = ap.parse_args(argv)

    places = _load(args.places, "places registry")
    routes = _load(args.routes, "routes registry")
    if places is None and routes is None:
        print("error: pass --places and/or --routes", file=sys.stderr)
        return 2

    if args.command == "validate":
        errors: list[str] = []
        if places is not None:
            errors += validate_places(places)
        if routes is not None:
            errors += validate_routes(routes)
        for e in errors:
            print(f"DEFECT: {e}")
        print("INVALID" if errors else "OK")
        return 1 if errors else 0

    if args.command == "list":
        if places is not None:
            print(f"places (map_id={places.get('map_id')!r}):")
            for e in places.get("places", []):
                aliases = ", ".join(e.get("aliases", []))
                print(f"  {e.get('id')}: {e.get('name')!r}"
                      f" ({e.get('x_m')}, {e.get('y_m')})"
                      + (f"  aka: {aliases}" if aliases else ""))
        if routes is not None:
            print(f"routes (map_id={routes.get('map_id')!r}):")
            for e in routes.get("routes", []):
                print(f"  {e.get('id')}: {e.get('name')!r}"
                      f"  {len(e.get('waypoints', []))} waypoint(s)")
        return 0

    # lookup
    if not args.query:
        print("error: lookup needs a QUERY", file=sys.stderr)
        return 2
    hit = lookup(args.query, places, routes)
    if hit is None:
        print(f"NOT FOUND: {args.query!r} (normalized: {normalize(args.query)!r})")
        return 1
    kind, e = hit
    print(f"{kind}: {e.get('id')} ({e.get('name')!r})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
