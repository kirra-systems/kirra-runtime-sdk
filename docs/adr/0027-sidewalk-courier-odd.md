# ADR-0027: Sidewalk courier ODD — a pedestrian-space class, not a shrunk car

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG1** (KIRRA bounds the doer — here a *creep + assured-clear-distance* envelope, not RSS car-following); **SG6** (impact energy — the courier's defining bound, `impact_cfg_for_class(Courier)`); **SG2** (drivable-surface containment — stay on the walkable strip) |
| Cross-refs | `docs/CONTRACT_PROFILES.md` (the Courier column + the new `*.rss_lateral_band` row); `docs/MARKET_AUTONOMOUS_SERVICES.md` §"Sidewalk couriers" (Serve / Coco / Starship); ADR-0014 (Orin NX single-box deployment); #312 (per-class profiles), #313 (VRU-dense courier); `VehicleClass::Courier` (`src/gateway/contract_profiles.rs`, `parko-core/src/impact.rs`); `VehicleConfig::courier()` (`crates/kirra-ros2-adapter/src/config.rs`); the Occy doer bridge (`docs/testing/OCCY_DOER_BRIDGE.md`) |

## Context

A sidewalk delivery robot (Rosmaster-class on a Jetson Orin NX; the commercial analogues are
Serve Robotics, Coco, Starship) is **not a small car**. Its operational design domain, its
dominant hazard, and therefore its safety model are different in kind, not degree:

- the dominant hazard is **striking a pedestrian / VRU**, not a vehicle collision at speed;
- it operates in **pedestrian space** (sidewalks, plazas, campus paths, crosswalks), not road lanes;
- it moves at **walking-to-jog pace** and must *creep and yield*, not lane-keep and car-follow.

Treating it as a scaled-down robotaxi imports the wrong frame: RSS car-following gaps, lane
containment, head-on RSS, and junction right-of-way are mostly irrelevant on a sidewalk, while the
bounds that actually matter — **creep speed, assured-clear-distance, ultra-low impact energy,
stay-on-the-walkable-surface, yield-to-people** — are under-weighted. The earlier per-class RSS
band work (#1) made the *mechanism* per-class but framed the win as "drive **past** a side object a
car stops for" — which is car-think in reverse: a courier near a pedestrian should **creep,
yield-ready**, not blow past *or* full-stop-and-give-up.

The repo already names this class (`VehicleClass::Courier` = "sidewalk courier — pedestrian-space,
low-speed", #313), with a fast-loop envelope (`courier_nominal` / `courier_mrc`) and a
pedestrian-energy SG6 threshold (`impact_cfg_for_class(Courier)`, spike 2.5 vs robotaxi 22). This
ADR pins the **ODD, the safety model, and the behavior/intent model** so the Courier class rests on
a written domain rather than ad-hoc numbers.

## Decision

Adopt the **Sidewalk Courier** as a first-class ODD with its own safety and behavior model,
realized as the existing `VehicleClass::Courier` profile family (sibling to Robotaxi / Delivery-AV;
the **Robotaxi numbers stay frozen** — ADR/§ CONTRACT_PROFILES sibling rule).

### 1. ODD (where it is allowed to operate)

- **Space:** sidewalks, shared paths, plazas, campus walkways; road only at **marked crosswalks**.
- **Speed:** creep regime — ODD cap **2.5 m/s** (`courier` `odd_speed_cap_mps`), typically well below
  it; the *assured-clear-distance* cap (Taj) routinely holds it lower in company.
- **Agents:** pedestrian-dense (VRU-dense, #313) — people, strollers, dogs, cyclists at low speed.
- **Conditions:** daylight + fair-weather first; geofenced; a human teleop fallback for edge cases.
- **Out of ODD → fail to a safe state:** leaving the geofence, losing localization, or a perception
  fault yields a **controlled stop / hold**, not a degraded crawl into traffic.

### 2. Safety model (the checker bounds, in priority order)

The courier's safety case rests on **low energy + creep + see-before-you-go**, not gap-keeping:

1. **Impact energy (SG6, defining).** Kinetic energy kept so low a contact is non-injurious —
   `impact_cfg_for_class(Courier)` spike threshold (2.5, VALIDATION-PENDING). This is the bound a
   sidewalk robot is *certified on*, the way a robotaxi is certified on RSS gaps.
2. **Creep speed cap.** The Courier ODD speed cap (2.5 m/s) is the absolute ceiling; the doer cruises
   below it.
3. **Assured-clear-distance / visibility (SG1, primary derate).** The Taj perception cap — speed
   bounded by the clear distance ahead (RSS Rule 4) — is the *primary* moment-to-moment bound: the
   robot **creeps to what it can see** and slows near anything. Already built (the cmd_vel
   perception cap; `docs/testing/OCCY_DOER_BRIDGE.md`).
4. **Drivable-surface containment (SG2).** Stay on the walkable strip (Taj corridor); leaving it is
   refused exactly as a road ego leaving its lane is.
5. **RSS car-following — DE-EMPHASIZED to a backstop.** The lateral-alignment band is tightened to
   the courier's ~1 m "lane" (`rss_lateral_band` = 0.6 m), so RSS rarely fires; on a sidewalk the
   creep cap + energy bound + ACD carry the safety case, and RSS is retained only as a
   defense-in-depth backstop, never the primary tool. (This corrects the #1 framing.)

The checker **logic** is identical to the robotaxi's — only these numbers differ, and the robotaxi
numbers are unchanged and frozen-by-test, so the courier profile **cannot regress the AV path**.

### 3. Behavior / intent model (the doer — Mick / Occy)

Sidewalk intents replace road maneuvers. Mick authors, Occy grounds, KIRRA bounds:

| Sidewalk intent | Meaning | (Road analogue it REPLACES) |
|---|---|---|
| `FollowPath` | track the walkable corridor at creep | lane-keeping |
| `Yield` | slow/stop and give way to a person | (none — couriers always yield) |
| `CrossWhenClear` | wait for a clear gap / signal, then cross a road at a crosswalk | junction right-of-way |
| `Hold` | stop and wait (blocked path, out-of-ODD, operator) | MRC hold |

What is **NOT** in the courier doer: lane-change, overtake, merge, asserting into junction
right-of-way. A courier does not negotiate for position; it yields and creeps. (`Yield` /
`CrossWhenClear` are the doer-side follow-up that ADR-0014's Mick seam already accommodates; this
ADR pins the set, the wiring is tracked separately.)

### 4. Class mapping (one selector, two loops, cited copies)

The Courier profile already spans the stack; the per-class numbers are **cited copies** across the
two dependency-separated workspaces (the CONTRACT_PROFILES.md single-source-of-truth rule):

| Surface | Courier realization |
|---|---|
| Fast-loop kinematic contract | `contract_for(VehicleClass::Courier)` → `courier_nominal` / `courier_mrc` |
| Impact / SG6 | `impact_cfg_for_class(VehicleClass::Courier)` (spike 2.5) |
| Slow-loop checker | `VehicleConfig::courier()` (footprint 0.6×0.9, wheelbase 0.5, 3.0 m/s, ODD 2.5, **rss_band 0.6**) |
| Doer (Occy) | the `class:"courier"` profile on the planner endpoint + the Occy doer bridge |
| Selector | the **class string** (`courier` / `robotaxi` / `delivery-av`) parsed on both sides (`VehicleClass::from_str`, `VehicleConfig::for_class`) — no shared import across the workspace boundary |

### 5. Deployment

Single-box on the Orin NX (ADR-0014), the "**Courier end**" of the platform strategy — a
QNX-native single-SoC variant is the certifiable target (`docs/MARKET_AUTONOMOUS_SERVICES.md`,
`parko/QNX_BACKEND_SELECTION.md`). The doer (Occy/Mick + perception) is the untrusted partition;
KIRRA is the creep+energy+ACD boundary.

## Consequences

- The Rosmaster validates the **Courier class**, not a degraded robotaxi — and in doing so it
  hardens the per-class profile machinery the whole family (incl. the robotaxi) depends on.
- The AV / robotaxi effort is **untouched and frozen** (guaranteed by
  `default_urban_rss_band_is_the_frozen_robotaxi_value`); the two ODDs share *logic*, never *numbers*.
- Follow-ups (tracked, not in this ADR): the doer-side sidewalk intents (`Yield` / `CrossWhenClear`
  / creep-through-crowd) in Mick/Occy; certified Courier impact + creep numbers (today
  VALIDATION-PENDING); pedestrian detection via Parko feeding the same objects seam.

## Status

**Proposed — for owner sign-off** (merge ratifies, as with ADR-0011/0012/0013/0014). Records the
sidewalk-courier ODD + safety/behavior model before further courier code, so the build rests on a
written domain. Implementation of the class selector + slow-loop sibling family follows (step b).
