# R2 Typed Destination Resolution

**The rule this system enforces: language chooses the destination TYPE; a
trusted resolver chooses the actual coordinates. The LLM never invents map
coordinates, object poses, addresses, or routes.**

Before this system, a spoken "drive to the kitchen" reached Mick's LLM, which
could only answer with a `go_to`/`route_to` intent — i.e. with coordinates it
made up. The typed destination layer replaces that with a typed, fail-closed
pipeline:

```
voice/text ──▶ typed DestinationRef            (LANGUAGE: kind + the operator's words)
                  {"kind":"named_place","query":"kitchen"}
              ──▶ DestinationResolver          (TRUST: registries / live tracks)
              ──▶ GroundedDestination          (goal pose + provenance)   ─┐
                                                                           ▼
              ──▶ ordinary MickIntent::GoTo ──▶ Occy plans ──▶ KIRRA checker bounds
                                                (the EXISTING governed path, unchanged)
```

Everything lives in `crates/kirra-sidecars/src/destination.rs` — deliberately
inside the crate fenced by `ci/check_mick_actuation_fence.py`, so "the
resolver has no dependency route to the release-token mint, the serial seam,
or any ROS/DDS transport" is a standing CI invariant.

## The four destination kinds

| kind             | example utterance          | trusted source                          | outcome                    |
|------------------|----------------------------|-----------------------------------------|----------------------------|
| `named_place`    | "drive to the kitchen"     | place registry JSON (operator-calibrated) | registry pose            |
| `saved_route`    | "run route A"              | route registry JSON (operator-calibrated) | first waypoint + full list |
| `tracked_object` | "drive to the red cup"     | live camera targets (`kirra_taj::object_goal`) | standoff pose        |
| `address`        | "drive to 125 Main Street" | **none** — no geocoder exists           | `Unsupported` (refusal)    |

`address` is a *typed seam*, present so an address request fails CLOSED with
an honest reason (`DEST_UNSUPPORTED_ADDRESS`, "I can't navigate to street
addresses yet") instead of being mis-bucketed into another kind or answered
with a fabricated lat/long. Wiring a real geocoding source behind it is
future work and a deliberate trust decision.

## The explicit outcome enum

`DestinationResolver::resolve` returns a `ResolveOutcome` — never an
`Option`:

* `Resolved(GroundedDestination)` — destination id, `map_id` (None for a live
  object sighting), goal pose, route waypoints, source, `resolved_at_ms`.
* `Ambiguous { candidates }` — more than one equally good match; refuse and
  ask, never silently pick. (`DEST_AMBIGUOUS`)
* `NotFound { code }` — with the specific reason: `DEST_NOT_FOUND`,
  `DEST_NO_REGISTRY`, `DEST_NOT_SEEN`, `DEST_LOW_CONFIDENCE`,
  `DEST_BEHIND_EGO`, `DEST_NON_FINITE`.
* `Stale` — the object evidence is too old (or absent) to drive on.
* `Unsupported { code }` — no trusted source for this kind.

Every non-Resolved arm carries a stable machine code plus an operator
sentence (`ResolveOutcome::sentence`) so the conversational layer SPEAKS the
refusal instead of silently holding.

## How coordinates are kept out of the language channel

1. **The parser** — `DestinationRef::parse_json` is the ONE fail-closed
   parser for destination JSON (`{"kind":..., "query":...}`, strict fields,
   bounded query). Any coordinate-shaped key (`x_m`, `y`, `lat`,
   `longitude`, `pose`, `waypoints`, …) is refused with the distinct
   `DEST_COORDINATES_FORBIDDEN` code.
2. **The LLM schema** — `destination_schema()` (the constrained-decode form
   for an Ollama-style backend) has exactly two STRING properties and
   `additionalProperties: false`: the model structurally cannot emit a
   number at all.
3. **The prompt** — `build_destination_prompt` renders known place/route
   NAMES only; a registry coordinate never enters a prompt (pinned by test).
4. **The seam** — the planner request's `destination` channel is mutually
   exclusive with `intent` and `object_goal` (`PLAN_AMBIGUOUS_GOAL_SOURCE`),
   and a grounded goal still passes the in-map bound (`INTENT_GOAL_OUT_OF_MAP`)
   like any other goal.

A grounded destination asserts **nothing about drivability**: it grounds as
an ordinary `MickIntent::GoTo`, Occy plans inside the lidar corridor, and the
KIRRA checker bounds the trajectory. A registered place behind an obstacle
produces a robot that stops short — never one that drives at the registry
entry. The governed enforcement chain (Mick → Occy → checker → release token
→ verifying consumer) is untouched.

## Registry files

One registry file = one map frame (`map_id`). Loads are **fail-closed**: any
defect refuses the whole file (and aborts `planner_service` startup) — wrong
`version`, empty/duplicate keys, non-finite coordinates, unknown fields,
empty waypoint lists. Ids, names, and aliases share ONE normalized key space
per file, so a lookup can never be ambiguous by construction.

`robot/testdata/places.example.json`:

```json
{
  "version": 1,
  "map_id": "example-uncalibrated",
  "places": [
    { "id": "kitchen", "name": "Kitchen", "aliases": ["the kitchen"],
      "x_m": 0.0, "y_m": 0.0, "heading_rad": 0.0 }
  ]
}
```

`robot/testdata/routes.example.json` is the same header plus
`"routes": [{ "id", "name", "aliases", "waypoints": [{x_m, y_m, heading_rad}, …] }]`
(1..1024 waypoints, all finite).

**Calibration is deliberate.** The shipped examples carry
`map_id: "example-uncalibrated"` and all-zero poses; they are templates to
copy, not defaults to install. Nothing in the repo's installers writes a
registry file — creating one means measuring the real pose (e.g. from the
consumer's `/odom` at the spot), editing the JSON by hand, and re-running
`validate`. The CLI below is read-only on purpose.

## Tracked-object policy

Object resolution delegates to the proven `kirra_taj::object_goal` resolver
(every-token label match, nearest-wins, tie → ambiguous, behind-ego refusal),
under the destination policy:

| knob | env (planner_service) | default |
|------|------------------------|---------|
| freshness budget | `KIRRA_DEST_OBJECT_MAX_AGE_MS` | `2000` ms |
| min confidence | `KIRRA_DEST_OBJECT_MIN_CONFIDENCE` | `0.70` |
| standoff | `KIRRA_DEST_OBJECT_STANDOFF_M` | `0.75` m |

The grounded goal stops `standoff_m` SHORT of the object along the
ego→object ray, facing it — the robot approaches a thing, it does not park
on top of it. A sighting older than the budget is `Stale` ("my view is
stale, so I won't drive on it"); a stale position is never driven to.
Malformed knob values ABORT startup (`ObjectPolicy::validated`), never
silently default.

## planner_service wiring

`POST /plan` accepts an opt-in `destination` field (the `DestinationRef`
object, or that object as a JSON string). Configuration (boot-validated,
fail-closed):

```
KIRRA_DEST_PLACES_PATH=/etc/kirra/places.json     # absent → named_place refuses DEST_NO_REGISTRY
KIRRA_DEST_ROUTES_PATH=/etc/kirra/routes.json     # absent → saved_route refuses DEST_NO_REGISTRY
KIRRA_DEST_OBJECT_MAX_AGE_MS=2000
KIRRA_DEST_OBJECT_MIN_CONFIDENCE=0.70
KIRRA_DEST_OBJECT_STANDOFF_M=0.75
```

A resolved plan echoes provenance in the response
(`destination: { destination_id, map_id, source, resolved_at_ms,
route_waypoints }`); a refusal is a 422 seam rejection carrying the `DEST_*`
code + operator sentence and an EMPTY trajectory (NO MOTION). A saved route
grounds to its FIRST waypoint; the full list rides the echo — sequencing the
rest is the mission layer's job.

**Frames**: registry poses are in the registry's `map_id` frame. The plan
request must be expressed in that same frame (i.e. the caller is localized
against that map). Live object sightings are ego-frame targets lifted through
the request's ego pose; their `map_id` is `null`.

## Operator CLI

```
python3 robot/location_registry.py list     --places places.json --routes routes.json
python3 robot/location_registry.py validate --places places.json --routes routes.json   # exit 1 on defect
python3 robot/location_registry.py lookup "the dock" --places places.json
```

Read-only; validation mirrors the resolver's rules (the Rust resolver
remains authoritative — it re-validates at load).

## Deferred (documented, not wired in this slice)

* **Live voice-loop rewiring** — the R2 voice router still sends
  destination-shaped motion text to `POST /intent` (where the model answers
  with a typed intent). Routing "go to X" through `destination_schema()` +
  `DestinationRef` into the planner's `destination` channel needs the
  mick-service endpoint + occy_doer bridge changes, done as its own slice.
* **Waypoint sequencing** for saved routes (mission layer receding-horizon
  advance past the first waypoint).
* **Map identity plumbing** — the plan request carries no map id today; the
  grounded `map_id` is echoed so a localized caller can check it.
* **Address resolution** — requires a trusted geocoding source + map
  anchoring; the typed seam already refuses honestly.
