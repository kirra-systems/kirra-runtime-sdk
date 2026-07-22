# Parko scene-veto gate semantics (WS-0.1 / #795)

Status: **living doc.** Records the intended behaviour and the open safety-owner
decisions for the parko-ros2 publication-seam scene-veto gates (occlusion / water
/ commit-zone). Cited from `parko/crates/parko-ros2/src/scene_vetoes.rs` and
`parko/crates/parko-ros2/src/config.rs`.

## The gates

Each gate composes ONTO a `TickOutcome` after the tick, bounding the exact twist
about to be published (same seam as `taj_objects::apply_object_rss_gate`). A gate
runs only when **armed** (configured); an armed gate whose scene slot is
missing/stale fails **closed** to its worst-case scene variant (the
enabled-but-silent → fail-closed house rule). Not armed → the gate is never called
→ byte-identical prior behaviour.

| Gate | Armed by | Missing/stale (armed) | Bounds |
|------|----------|-----------------------|--------|
| Occlusion (RSS rule iv) | `occlusion_gate_enabled` **and** a `platform_profile` (RSS params) | `OcclusionScene::Absent` → cap 0.0 | **linear only** (see below) |
| Water (SG4) | `water_gate_enabled` | `WaterScene::Unknown` → veto | zeroes both channels |
| Commit-zone (SG5) | `commit_zone_gate_enabled` | `CommitZoneScene::Unknown` → veto | zeroes both channels |

## F6 — armed without a producer = permanent immobilization (fail-loud)

This build ships **no producers** for the occlusion / water slots (commit-zone
now has one — see the #1124 section below). An armed gate with a slot that is
never fed fails closed to a STOP **every tick, forever** — a permanent
immobilization. That behaviour is fail-*safe* (a stop), but silently coming up
immobilized with no diagnostic is a foot-gun.

`ParkoNodeConfig::scene_gate_startup_check` (pure, unit-tested) makes it loud: if
any gate that would *actually* immobilize is armed (see
`producerless_armed_scene_gates`) and the operator has **not** set
`PARKO_ALLOW_SCENE_GATE_WITHOUT_PRODUCER=1`, the node **refuses to start** and
names the offending gates. The operator then either configures a producer,
disarms the gate, or deliberately acknowledges the immobilization (e.g. a
bring-up / immobilizer test). Occlusion counts only when it truly arms (flag +
`platform_profile`); a flag with no profile is a dark flag, not an immobilizer.

The veto **configs** (`WaterVetoConfig`, `CommitZoneCfg`) are now carried on
`ParkoNodeConfig` (`water_veto_config` / `commit_zone_config`) rather than a
per-tick hardcoded `Default()`, so their (VALIDATION-PENDING) bounds are a
deployment knob. Defaults are unchanged → byte-identical.

**Remaining (tracked, #1124):** a producer for the occlusion slot
(sightline-from-lidar — perception work requiring hardware/design validation)
and for the water slot. The commit-zone slot's producer landed (#1124, below).

## #1124 — the map-anchored commit-zone producer (shipped)

`parko/crates/parko-ros2/src/commit_zone_producer.rs` — the first real scene
producer, map-anchored with **no new perception**:

- **Zones** are a site-authored, versioned JSON artifact
  (`PARKO_COMMIT_ZONE_MAP_PATH`): polygons in the map frame (rail crossings,
  box junctions, narrow bridges). Loaded **fail-closed at startup** — an
  unreadable or invalid spec (bad version, empty, duplicate ids, <3 or
  non-finite vertices, degenerate polygon) ABORTS the node; never a partial
  zone map. Each zone's traverse length is derived as the polygon **diameter**
  (conservative: a longer assumed zone → a longer required clearance gap).
- **Anchor** is the ego pose (`PARKO_POSE_TOPIC`, `nav_msgs/Odometry`,
  arrival-stamped). A missing, stale, future-skewed, or non-finite pose yields
  `CommitZoneScene::Unknown` → the gate vetoes ("Reject fires from MAP ALONE" —
  an unanchored prior is exactly the `Unknown` case). Pose *accuracy* rides
  AOU-LOCALIZATION-001; a frame-integrity feed composes later via
  `gate_commit_zone_scene` unchanged.
- **Distance** is the Euclidean point-to-polygon distance (0 inside). Parko has
  no route model, so this LOWER-BOUNDS any along-path distance — the veto can
  only fire *earlier* than a path-aware producer's, never later. A zone near
  (but not on) the actual path can over-veto; sites author polygons accordingly.
- **Entry confirmations are DERIVED, never asserted**: `exit_verified` via
  `exit_clearance_verified`, `clearance_confirmed` via `non_yielding_clearance`.
  The node currently supplies **no evidence** (`NonYieldingScene::Absent`, no
  exit measurement), so a zone within the look-ahead **vetoes — the ego stops
  short of every mapped commit zone**. That is SG5's intended fail-closed
  behaviour until the #107/#108 evidence *ingestion* lands; the producer's
  evidence parameters make that a fill-in, not a rework (tests pin that entry
  becomes earnable the moment evidence arrives).

Arming: `commit_zone_producer_armed()` = spec **and** pose topic configured
(the same one-predicate discipline as `object_gate_armed`). With the producer
armed, the commit-zone gate drops out of `producerless_armed_scene_gates` and
the F6 startup guard passes without the immobilizer acknowledgment; a
half-configured producer (spec without pose topic, or vice versa) still counts
as producer-less and refuses startup.

## F8 — occlusion angular-channel semantics (pending decision)

The occlusion cap is an assured-clear-**distance** bound, so it binds only the
LINEAR channel and leaves `angular_z` untouched. This is deliberate:

- A pure in-place rotation (`linear ≈ 0`, `angular ≠ 0`) under an `Absent` /
  `Limited` scene passes the gate unchanged. Turning in place does not advance the
  ego into the unobserved region — it is the *creep-and-peek* primitive that lets
  the ego rotate to improve its own sightline before committing to forward motion.
  Zeroing it would deadlock that maneuver.
- Any rotation with a nonzero turn radius carries a nonzero `linear_x` and is
  therefore still bound by the clamp (its along-track speed is capped).

**Open decision (safety owner):** a *swept* in-place rotation of a long /
rectangular footprint can sweep the vehicle's extremities into an occluded
conflict zone even at zero `linear_x`. Whether to additionally bound that
swept-rotation case here — versus the stricter water / commit-zone gates, which
zero both channels — is a footprint-geometry decision. Until decided, the
linear-only binding stands (documented here so the choice is explicit, not
implicit).

## F9 — occlusion clamp-to-cap (#794, shipped)

Over a positive assured-clear-distance cap the ego is clamped to ±cap (creep,
direction preserved), not bang-bang stopped; full-stop only when no motion is
admissible (`cap == 0` / non-finite). See #794.
