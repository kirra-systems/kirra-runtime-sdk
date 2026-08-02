#!/usr/bin/env python3
"""Host tests for location_registry — the read-only registry CLI helpers.

Pins: normalization mirrors the Rust resolver, validation catches every
defect class the resolver refuses (version, map_id, duplicate keys across
ids/names/aliases, non-finite coordinates, empty waypoint lists), lookup is
exact-key-only, and the example registry files shipped in testdata/ are
valid.

Runs standalone (`python3 robot/location_registry_test.py`, exit 1 on
failure); importable under pytest.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from location_registry import (  # noqa: E402
    lookup, normalize, validate_places, validate_routes,
)

TESTDATA = Path(__file__).resolve().parent / "testdata"

_F: list[str] = []


def check(cond, msg):
    if not cond:
        _F.append(msg)
        print(f"  FAIL: {msg}", file=sys.stderr)


def places_doc():
    return {
        "version": 1,
        "map_id": "r2-lab-01",
        "places": [
            {"id": "kitchen", "name": "Kitchen", "aliases": ["the kitchen"],
             "x_m": 3.2, "y_m": -1.4, "heading_rad": 0.0},
            {"id": "charging-dock", "name": "Charging Dock",
             "aliases": ["the dock", "charger"], "x_m": 0.5, "y_m": 2.0},
        ],
    }


def routes_doc():
    return {
        "version": 1,
        "map_id": "r2-lab-01",
        "routes": [
            {"id": "route-a", "name": "Route A", "aliases": ["route alpha"],
             "waypoints": [{"x_m": 1.0, "y_m": 0.0, "heading_rad": 0.0},
                           {"x_m": 2.0, "y_m": 1.0}]},
        ],
    }


def test_normalize_mirrors_the_resolver():
    check(normalize("The Kitchen!") == "the kitchen", "basic normalize")
    check(normalize("  ROUTE--A  ") == "route a", "punctuation collapses")
    check(normalize("???") == "", "no alphanumerics -> empty")


def test_valid_documents_have_no_defects():
    check(validate_places(places_doc()) == [], "places doc valid")
    check(validate_routes(routes_doc()) == [], "routes doc valid")


def test_validation_catches_each_defect_class():
    d = places_doc()
    d["version"] = 2
    check(any("version" in e for e in validate_places(d)), "wrong version")

    d = places_doc()
    d["map_id"] = "  "
    check(any("map_id" in e for e in validate_places(d)), "empty map_id")

    d = places_doc()
    d["places"][1]["aliases"] = ["kitchen"]  # collides with entry 0's id
    check(any("collides" in e for e in validate_places(d)), "duplicate key")

    d = places_doc()
    d["places"][0]["x_m"] = float("nan")
    check(any("finite" in e for e in validate_places(d)), "NaN coordinate")

    d = places_doc()
    d["places"][0]["x_m"] = "3.2"
    check(any("finite" in e for e in validate_places(d)), "string coordinate")

    d = places_doc()
    d["places"][0]["x_m"] = True  # bool is not a coordinate
    check(any("finite" in e for e in validate_places(d)), "bool coordinate")

    r = routes_doc()
    r["routes"][0]["waypoints"] = []
    check(any("waypoints" in e for e in validate_routes(r)), "empty waypoints")

    r = routes_doc()
    r["routes"][0]["waypoints"][1]["y_m"] = float("inf")
    check(any("finite" in e for e in validate_routes(r)), "inf waypoint")


def test_lookup_is_exact_key_only():
    hit = lookup("the kitchen", places_doc(), routes_doc())
    check(hit is not None and hit[0] == "place" and hit[1]["id"] == "kitchen",
          "alias lookup hits the place")
    hit = lookup("Route Alpha!", places_doc(), routes_doc())
    check(hit is not None and hit[0] == "route" and hit[1]["id"] == "route-a",
          "normalized alias lookup hits the route")
    check(lookup("kitch", places_doc(), routes_doc()) is None,
          "no prefix/fuzzy matching — exact keys only")
    check(lookup("   ", places_doc(), routes_doc()) is None, "empty query misses")


def test_shipped_example_registries_are_valid():
    places = json.loads((TESTDATA / "places.example.json").read_text())
    routes = json.loads((TESTDATA / "routes.example.json").read_text())
    check(validate_places(places) == [], "places.example.json is valid")
    check(validate_routes(routes) == [], "routes.example.json is valid")
    # The examples are templates, not calibration: their map_id says so, so
    # copying one into service unedited is loud in every list/echo/log line.
    check("example" in places["map_id"] and "example" in routes["map_id"],
          "example registries self-identify as uncalibrated")


# --- standalone runner (house pattern) ----------------------------------------

def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"  ok  {name}")
    if _F:
        print(f"location_registry_test: {len(_F)} FAILURE(S)")
        return 1
    print("location_registry_test: ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
