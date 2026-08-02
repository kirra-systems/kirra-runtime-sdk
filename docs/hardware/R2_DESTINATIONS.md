# R2 Typed Destination Resolution

**The rule this system enforces: language chooses the destination TYPE; a
trusted resolver chooses the actual coordinates. The LLM never invents map
coordinates, object poses, addresses, or routes.**

## The live voice path

Destination-shaped speech no longer goes to the ordinary `/intent` door (where
a model would have had to invent a goal point). It routes to the typed
destination door:

```
wake phrase → bounded capture → STT
  → voice_route.classify_transcript      (PURE, deterministic — no LLM)
      ├─ conversation      → POST /chat/stream
      ├─ explicit motion   → POST /intent            (unchanged)
      ├─ semantic destination
      │     → typed DestinationRef {"kind":…, "query":…}
      │     → POST /destination  (mick_service)
      │        → DestinationRef::parse_json   (the ONE fail-closed parse)
      │        → DestinationResolver          (the ONLY source of coordinates)
      │        → grounded target latched on GET /destination/last
      │        → ordinary MickIntent::GoTo → Occy → KIRRA checker → release
      │                                       → verifying consumer
      └─ ambiguous         → a fixed local clarification, NO endpoint
```

**One transcript reaches at most one endpoint.** A destination that does not
resolve produces **no planner request at all** — there is no fallback to
`/intent`, no fallback to chat, and no retry (a retry could duplicate an
accepted grounded target).

The voice router has **zero motion authority and never creates a coordinate**:
it sends a kind plus the operator's own words, and the success body it gets
back carries no pose — so there is nothing for it to speak, log, or leak. The
grounded pose lives only on the doer-facing latch.

### Spoken outcomes

| resolver outcome | spoken line |
|---|---|
| resolved | "Destination accepted for governed navigation." |
| resolved (saved route) | "Destination accepted for governed navigation, first waypoint only." |
| `DEST_NOT_FOUND` / `DEST_NO_REGISTRY` / `DEST_NOT_SEEN` | "I don't have a configured location matching that request." |
| `DEST_AMBIGUOUS` | "That destination is ambiguous. Please be more specific." |
| `DEST_STALE` / `DEST_LOW_CONFIDENCE` | "I no longer have a fresh position for that object." |
| `DEST_UNSUPPORTED_ADDRESS` | "Address routing isn't configured on this robot." |
| unreachable service | "The destination service is unavailable." |
| malformed / unknown code | "I couldn't validate that destination." |

Acceptance is **admission, not execution**: the ack never claims the robot
moved or arrived. Every line is spoken through the existing `PlaybackGuard`,
so the robot cannot hear its own acknowledgement and start another turn.

### The phrase grammar (deliberately small, pure, no LLM)

| said | typed request |
|---|---|
| "drive to the kitchen", "go to the charging dock", "take me to the workshop", "navigate to the front door", "head to the garage" | `{"kind":"named_place","query":"kitchen"}` |
| "run route A", "follow route A", "take route A", "start patrol route", "follow the perimeter route" | `{"kind":"saved_route","query":"route a"}` |
| "drive to the red cup", "go to the blue chair", "move near the person", "navigate to the box" | `{"kind":"tracked_object","class":"cup","color":"red"}` |
| "drive to 125 Main Street" | `{"kind":"address","query":"125 Main Street"}` |

Wake phrases, courtesy prefixes and articles are stripped; meaningful
destination words are not. The object vocabulary is a **closed** list of
classes and colours — free-form scene description is out of scope, and an
unrecognised thing falls through to `named_place` where the registry refuses
it. An address needs a leading house number **and** a street suffix.

These never become destinations: "what is the kitchen used for", "tell me
about Route 66", "where is the red cup", "what do you see near the chair",
"explain how address routing works", "tell me about the kitchen", "what is
route planning", "describe a red cup" (all conversation), and "go there",
"take me there", "drive to it", "run it", "follow that" (all **ambiguous** —
a pronoun names nothing, so the robot asks rather than guesses). "Drive
forward one meter", "turn left", "stop" and "pull over" remain ordinary
motion on `/intent`.

**Reclassification note.** "go to the loading dock" / "take me to the dock"
were `MOTION` before this integration and are now `DESTINATION`. That is the
point: they name a place, and a place must be grounded by the trusted
resolver rather than by a model guessing a goal point.

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

## mick_service wiring (the voice-facing door)

```
POST /destination  {"destination":{"kind":"named_place","query":"kitchen"},
                    "targets"?:[…], "targets_stamp_ms"?, "ego"?}
  → 200 {"ok":true,"seq":n,"outcome":"resolved","kind":…,"route_resolution":…}
        ADMISSION ONLY — no pose, no map id
  → 422 {"ok":false,"error":"DEST_…","detail":"<operator sentence>"}
        fail-closed: NOTHING latched, seq unchanged
  → 429 {"ok":false,"error":"DEST_RATE_LIMITED"}

GET /destination/last → {"destination":{…,"frame":"map"|"ego",…},"seq":n}
```

`mick_service` reads the **same** `KIRRA_DEST_*` registry/policy vars as
`planner_service` (`destination::resolver_from_env`) — one config path, so two
processes can never disagree about what "the kitchen" means. A malformed value
or an invalid registry aborts startup.

**The latch is frame-explicit, and deliberately its own channel.** A registry
pose is in the registry's map frame; `GET /intent/last` is consumed by
`occy_doer` as **ego-frame at receipt**. Publishing a map pose there would be
silently misread as ego-relative and aim the robot at the wrong place — so
grounded destinations ride `GET /destination/last` with an explicit `frame`
(`map` + `map_id`, or `ego`), and a consumer must understand the frame before
it can use the pose.

**Tracked objects need a perception-aware caller.** `mick_service` has no
perception (it is inside the actuation fence — no ROS, no camera). Object
resolution runs against the `targets` the *caller* supplies. The voice router
supplies none, so an object destination spoken today resolves `DEST_STALE` —
"the detector did not look" is never "it isn't there". Relaying live targets
is the deferred doer-bridge step below.

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

## Verification discipline

**Keep the planner parked for initial verification.** With
`kirra-planner.service` stopped, every step below exercises voice → typed
destination → trusted resolver → refusal-or-admission and the robot cannot
move. That is deliberate: the resolver round trip is what this integration
adds, and it is verifiable without motion.

Do **not** claim voice-to-destination navigation works on hardware until a
real calibrated destination resolves *and* the full governed chain (Occy →
checker → release token → verifying consumer) has been observed end to end.

## Deferred (documented, not wired in this slice)

* **Doer-bridge consumption of the grounded latch** — `occy_doer` polls
  `GET /intent/last` (ego-frame) and does not yet read
  `GET /destination/last`. Consuming it requires honouring the explicit
  `frame`: a `map` pose is usable only by a consumer localized against that
  `map_id`. Until then a grounded destination is resolved, latched and
  auditable, but is not yet picked up by the ROS doer.
* **Live target relay for object destinations** — the voice path supplies no
  `targets`, so spoken object destinations resolve `DEST_STALE`. A
  perception-aware caller (the doer bridge) supplying `targets` +
  `targets_stamp_ms` gets full standoff resolution today.
* **Waypoint sequencing** for saved routes (mission layer receding-horizon
  advance past the first waypoint). Phase 1 grounds the FIRST waypoint only
  and marks it `route_resolution=first_waypoint_only` on every wire read, in
  the logs, and in the spoken acknowledgement.
* **Map identity plumbing** — the plan request carries no map id today; the
  grounded `map_id` is echoed so a localized caller can check it.
* **Address resolution** — requires a trusted geocoding source + map
  anchoring; the typed seam already refuses honestly.
