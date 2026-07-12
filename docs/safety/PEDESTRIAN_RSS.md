# Pedestrian / VRU RSS — the omnidirectional reachable-set bound

**Document ID:** KIRRA-VRU-RSS-001
**Status:** DRAFT — pending formal safety-engineer review (the reasoning and
implementation are real and tested; treating the parameter values as a
validated safety claim requires sign-off — same discipline as
`ANGULAR_VELOCITY_SOTIF.md`).
**Implements:** WS-2 "Pedestrian RSS primitive … wired into
`validate_trajectory_slow` — the courier/sidewalk persona is P1's flagship."
**Code:** `crates/kirra-trajectory/src/vru.rs` (primitive) +
`validation.rs` section D2 (wiring). Traceability tag:
`REQ: vru-pedestrian-reachable-set-bound`.

---

## 1. Why the vehicle RSS is unsound for pedestrians

The checker's vehicle-object RSS (validation.rs section C) implements the
IEEE 2846 / Shalev-Shwartz §5 model for road users **with a defined
direction of travel**: a longitudinal safe-distance bound gated by a
**lateral-alignment filter** (an object laterally outside
`rss_lateral_alignment_tolerance_m` is "another lane's problem" — handled
by containment, skipped by RSS).

Both halves break for a VRU:

1. **The lateral filter is a hole, not an optimization.** A pedestrian
   standing on the kerb 2.5 m laterally "clear" can step into the corridor
   within the ego's stopping time. For vehicles, lane discipline justifies
   the filter; a pedestrian has no lane discipline.
2. **Directional closing-speed bounds assume inertia.** RSS's
   longitudinal/lateral decomposition leans on vehicles being unable to
   change velocity direction instantly. Relative to vehicle dynamics, a
   pedestrian *can*: walking → turning → stepping sideways happens inside
   one reaction time.

## 2. The model: omnidirectional reachable set

At trajectory time `t`, assume the pedestrian may be anywhere in a disc
around their perceived position:

```text
reach(t_eval) = r_ped + v_ped_max · t_eval
```

The ego is safe with respect to that pedestrian iff, from every
time-matched trajectory pose with speed `v > ε`, it could come to a
**complete stop** (reaction ρ, then assured braking `a_brake`) without its
stopping envelope meeting the disc as it grows over the whole stopping
interval:

```text
v_eff    = max(|v_declared|, displacement / dt)   — effective ego speed (F4, #789): the
                                                    declared scalar is NOT trusted alone
v_after  = v_eff + a_max · ρ                       — worst-case speed AFTER the response phase (F2)
t_stop   = ρ + v_after / a_brake                  — time to full stop
d_stop   = v_eff·ρ + ½·a_max·ρ² + v_after² / (2·a_brake)   — RSS stopping distance
required = d_stop + r_ped + v_ped_max · (age + t + t_stop) + clearance + ego_reach
UNSAFE   ⟺ dist(pose, ped) < required
```

where:
- **`v_eff`** (F4, #789) is `max(|v_declared|, displacement/dt)` over the adjacent
  pose pair — the checker does not trust a doer-declared `velocity_mps` in isolation,
  so a planner emitting translating poses that DECLARE `v = 0` cannot skip the bound
  (or understate `d_stop`). A nonzero displacement over a non-positive/non-finite `dt`
  (teleport / time reversal) fails closed.
- **`age`** (F8, #789) is the pedestrian measurement age (`PerceivedPedestrian.age_s`):
  the reachable disc has already been growing since the observation, so the growth window
  is `age + t + t_stop`. A negative or non-finite age is a perception fault → breach.
- **`a_max`** is the ego's max acceleration: during the reaction time ρ the
  plan/actuator may still be executing acceleration, so the ego brakes from
  `v_after`, not `v` (Shalev-Shwartz Def. 1 / Lemma 2; IEEE 2846). This is the
  same response-phase term the vehicle RSS carries (#779 F2).
- **`ego_reach`** = `max(wheelbase+overhang_front, overhang_rear).hypot(half_width)`
  — the max distance from the pose (the **rear axle**) to any point of the ego
  footprint. The ego is a BODY, not a point: without this term the distance was
  rear-axle-to-pedestrian and the robotaxi's ~3.8 m nose swept past the pedestrian
  before the disc growth counted (#779 F1). Direction-independent, matching the
  omnidirectional model.
- **`a_brake`** is the **posture-composed** brake the validator passes
  (`kinematics.max_brake_mps2`) — the Nominal service brake under Nominal, the
  weaker MRC brake under Degraded (#779 F3), so a faulted-posture ego is held to
  its actual stopping power.

Distance is **euclidean** — deliberately no lateral filter and no
behind-ego filter (§1; a VRU beside or behind the path can enter it — the
disc's time growth keeps genuinely distant pedestrians from binding).
Verdict on breach: `MRCFallback`, exactly like a containment or RSS breach.

### 2.1 The crossing model is subsumed

WS-2 names "longitudinal/lateral bounds + crossing model". A directed
crossing model (pedestrian heading toward the path at up to `v_cross`)
traces a cone of positions; for any crossing speed ≤ `v_ped_max` that cone
is a **subset of the omnidirectional disc**. v0 therefore ships the disc
and gets the crossing model for free, at an availability (not safety)
cost. The directed refinement — using tracked heading/velocity to shrink
the disc toward a cone — is a tracked follow-up (§6) and is only ever a
RELAXATION of this bound: it requires validated tracking evidence and its
own review, and it can never be introduced as a silent default.

### 2.2 Worked reference point

`v = 2 m/s`, pose `t = 0`, robotaxi (`default_urban`) defaults (ρ = 0.5 s,
a_brake = 4.5 m/s², a_max = 2.5 m/s², v_ped_max = 2.0, r_ped = 0.3,
clearance = 0.5, ego_reach = `3.7.hypot(0.925)` ≈ 3.814 m):

```text
v_after  = 2 + 2.5·0.5                 = 3.25 m/s
t_stop   = 0.5 + 3.25/4.5              = 1.2222 s
d_stop   = 2·0.5 + ½·2.5·0.25 + 3.25²/9 = 2.4861 m
reach    = 0.3 + 2.0 · 1.2222          = 2.7444 m
required = 2.4861 + 2.7444 + 0.5 + 3.814 = 9.5444 m
```

(pinned by `vru::tests::worked_reference_point_matches_the_doc`).

## 3. Responsibility semantics & the stop-proposal invariant

A pose with `|v| ≤ stop_epsilon_mps` imposes **no requirement**:

- **RSS responsibility:** a stationary ego strikes nothing; a pedestrian
  contacting a stopped vehicle is not an ego-caused collision.
- **Deadlock freedom (load-bearing):** the architecture requires
  `PlanOutput::safe_stop` to always exist and be admissible ("a planner
  with no stop output deadlocks the loop"). A VRU bound that refused
  stopped trajectories near pedestrians would make the MRC itself
  inadmissible — the gate must converge on stopping, never forbid it.
  Pinned end-to-end by `vru_safe_stop_next_to_pedestrian_admits`.

The consequence is intentionally asymmetric: near a pedestrian the checker
admits *stopping and staying stopped* and refuses *moving through* — which
is exactly the sidewalk-courier posture.

## 4. Fail-closed rules

| Condition | Treatment |
|---|---|
| Non-finite pedestrian field | Breach → MRC (unlocalizable perception fault; mirrors the vehicle-object rule) |
| Non-finite pose speed/time | Breach → MRC |
| Non-positive / non-finite `a_brake`, or non-finite/negative `a_max` / `ego_reach` | `required = ∞` → any moving pose breaches (an unbrakeable ego, or corrupt ego geometry, cannot prove VRU safety) |
| Non-finite speed/time input or ANY corrupt `VruRssParams` field (non-finite or negative) | `required = ∞` → breach (a NaN would otherwise poison `dist < required` into failing OPEN) |
| Non-finite trajectory pose field | Breach → MRC (self-contained — not dependent on containment rejecting it first) |
| Negative / non-finite pedestrian measurement `age_s` (F8, #789) | Breach → MRC (a stale-age fault must not silently reduce the disc growth) |
| Malformed segment — nonzero displacement over non-positive/non-finite `dt` (F4, #789) | Breach → MRC (teleport / time reversal: no finite implied speed is definable) |
| More than `MAX_PEDESTRIANS` (64) in a scene (F9, #789) | Breach → MRC (over-bound perception input; bounds the per-tick WCET) |
| Absent VRU channel (`None` scene) OR empty pedestrian list | **No-op** — byte-identical path (the derate-only invariant: absent input never relaxes, never fabricates) |

## 5. Parameters (`VruRssParams`, plus the ego brake from `VehicleConfig`) — all VALIDATION-PENDING

| Param | Default | Rationale / tuning obligation |
|---|---|---|
| `v_ped_max_mps` | 2.0 | Brisk walk (~1.4 typical walking; 2.0 covers hurried). **ODDs with expected runners, children darting, or cyclists-as-VRUs must raise it** — a raise only tightens the bound. **Floored at 2.0 (F5, #789): a caller cannot supply a smaller value** — `sanitized()` enforces `max(v_ped_max, V_PED_MAX_FLOOR_MPS)`. |
| `ped_radius_m` | 0.3 | Body half-width; not a point target. |
| `clearance_m` | 0.5 | Comfort/robustness margin beyond the geometric envelopes. |
| `reaction_time_s` | 0.5 | Matches the vehicle-RSS `RSS_REACTION_TIME_S` — one reaction model per checker. |
| `stop_epsilon_mps` | 0.05 | Matches `STOP_EPSILON_MPS` (the Degraded stop-and-hold epsilon). **Clamped ≤ the kernel `STOP_EPSILON_MPS` (F5, #789): a caller cannot loosen it** — a larger value would let a still-rolling ego skip the bound. `sanitized()` enforces `min(stop_epsilon, VRU_STOP_EPSILON_CEILING_MPS)`. |

**Params authority (F5, #789).** `VruRssParams` are caller-supplied per call. The
`params_valid` finiteness/non-negativity check is not sufficient on its own — a
`stop_epsilon = 5.0` or `v_ped_max = 0.0` is finite and non-negative yet WEAKENS the
bound. `VruRssParams::sanitized()`, applied once at the single choke point
(`pedestrian_breach`), floors `v_ped_max` at `V_PED_MAX_FLOOR_MPS` (2.0) and clamps
`stop_epsilon` to `VRU_STOP_EPSILON_CEILING_MPS` (the kernel `STOP_EPSILON_MPS`); both
clamps are monotone-tightening, and non-finite OR negative fields are left untouched for
`params_valid` to fail closed rather than clamped/laundered into an admitting value (a
negative `v_ped_max` is not floored to 2.0 — Copilot #799). A full per-ODD derivation of
`v_ped_max` (mirroring `KIRRA_VEHICLE_CLASS` / `contract_for`) remains a deployment
tuning obligation ON TOP of this floor.
| `a_brake_mps2` (not a `VruRssParams` field — the validator passes the **posture-composed** `kinematics.max_brake_mps2`, #779 F3) | per-class contract | The ego's assured braking: the Nominal service brake under Nominal, the weaker MRC brake under Degraded (so a faulted-posture ego is held to its actual stopping power). Derating it further (e.g. wet-surface factor) is a tracked refinement. |
| `a_max_mps2` (not a `VruRssParams` field — the validator passes `VehicleConfig::max_accel_mps2`, #779 F2) | per-class contract | The ego's max acceleration, for the RSS response-phase term (`v_after = v + a_max·ρ`). |
| `ego_reach_m` (derived from the footprint by the validator, #779 F1) | per-class geometry | `max(wheelbase+overhang_front, overhang_rear).hypot(half_width)` — the ego body extent from the rear-axle pose. |

Availability envelope at the robotaxi (`default_urban`) defaults (euclidean
`required`, pose `t = 0`, with the F1 ego-body + F2 response-phase terms):
ego 1 m/s → 8.0 m; 2 m/s → 9.5 m; 4 m/s → 13.3 m; 6 m/s → 18.0 m. These are
substantially larger than the pre-fix point-ego numbers — the sound bound is
also a MORE conservative one. The courier persona (smaller footprint, lower
a_max/speed) keeps a tighter bubble; a robotaxi at speed relies on pedestrians
being genuinely distant. The over-conservatism at the urban ODD cap — road-
structure / right-of-way semantics to shrink the disc to the corridor — is
tracked as a §6 refinement (never a silent relaxation).

## 6. Integration status & tracked follow-ups

**Live now:** the primitive (`vru.rs`) and its wiring as section D2 of
`validate_trajectory_slow_capped` behind an optional `PedestrianScene`
argument (absent → no-op). The WCET-critical per-pose
`validate_vehicle_command` path is untouched; the Nominal path without a
VRU channel is byte-identical.

**Go-live soundness hardening (#789), landed — the prerequisites for feeding a
live producer:**
- **F4 — velocity-declaration trust.** The stop-epsilon skip and `d_stop` use
  `v_eff = max(|v_declared|, displacement/dt)`; a translating pose declaring `v = 0`
  can no longer bypass the bound. Malformed segments fail closed.
- **F5 — params authority.** `VruRssParams::sanitized()` floors `v_ped_max` and
  clamps `stop_epsilon` at the single choke point (§5).
- **F8 — measurement age.** `PerceivedPedestrian.age_s` grows the disc by
  `v_ped_max · age`; the wire shape is frozen now, before a producer exists.
- **F9 — WCET.** `MAX_PEDESTRIANS` fail-closed input bound; per-pose `required`
  hoisted out of the pedestrian loop; `O(T·P)` with one `required` per pose (module
  doc). Cross-checked by the `hoisted_breach_matches_naive_reference` proptest.

**Follow-ups (in dependency order):**
0. **Road-structure / right-of-way (F6, #789, availability).** The pure disc makes
   the robotaxi ODD over-conservative (§5) and can bind a pedestrian behind a receding
   ego. Corridor-clipped disc growth (a distance-to-path term) and/or per-zone
   `v_ped_max`, keeping the pure disc for the sidewalk-courier ODD. **Availability,
   strictly after soundness** — a relaxation, never a silent default; own review.
   **Design proposal (review-gated, no code yet): [`PEDESTRIAN_RSS_F6_DESIGN.md`](PEDESTRIAN_RSS_F6_DESIGN.md)**
   — the corridor-geodesic subset argument (`TRUE ⊆ GEODESIC ⊆ EUCLIDEAN`), the
   impassability evidence gate (the load-bearing input), fail-closed→pure-disc
   fallbacks, and the open ODD/evidence decisions for sign-off before any bound
   change.
1. **Node VRU channel** — a `~/input/pedestrians` subscription on the ros2
   adapter (staleness-budgeted like the object channels: an ARMED but
   silent/stale channel fails closed to `Some(empty…)`→cap, never silently
   disarms), feeding the live scene into the D2 argument.
2. **Classification ingest** — today nothing produces
   `PerceivedPedestrian`s; Taj Phase-B semantic classes (the detector seam)
   or an integrator-supplied VRU topic must classify. Until then the gate
   is armed-but-unfed by construction, and *that is stated here rather than
   papered over*.
3. **KPI corpus rows** — pedestrian scenarios in the WS-3.1 gate corpus
   (admissibility of stop-near-VRU proposals; refusal of drive-through
   proposals) once the seam has a producer.
4. **Directed refinement** (§2.1) — cone-shrunk reachable sets from
   validated tracking; relaxation-only, review-gated.
5. **Brake derating** (§5) — surface-condition factor on `a_brake`.

## 7. Test traceability

| Property | Test |
|---|---|
| Formula reference point | `vru::tests::worked_reference_point_matches_the_doc` |
| Monotonicity in speed & time | `vru::tests::requirement_is_monotone_in_speed_and_time` |
| In-path pedestrian → MRC (end-to-end) | `vru_pedestrian_in_path_mrcs` |
| Distant pedestrian admits | `vru_far_pedestrian_admits` |
| Stop-proposal invariant | `vru_safe_stop_next_to_pedestrian_admits` (+ unit `safe_stop_next_to_pedestrian_is_admitted`) |
| Omnidirectionality vs the lateral band | `vru_kerbside_pedestrian_binds_despite_lateral_clearance` (+ unit) |
| Absent-channel byte-identity | `vru_absent_channel_is_byte_identical` |
| Fail-closed non-finite / unbrakeable / bad geometry | `vru_non_finite_pedestrian_mrcs`, unit `non_positive_brake_and_bad_geometry_fail_closed`, `non_finite_pedestrian_breaches` |
| Ego-body term (F1) / response-phase term (F2) / posture brake (F3) | unit `ego_footprint_term_binds_the_body_not_the_axle`, `response_phase_accel_term_raises_the_requirement`, `weaker_degraded_brake_demands_more_clearance` |
| Velocity-declaration trust (F4, #789) | unit `declared_zero_velocity_but_translating_pose_still_binds`, `malformed_segment_fails_closed` |
| Params authority — loose params cannot weaken (F5, #789) | unit `loose_params_cannot_weaken_the_bound` |
| Measurement-age disc growth (F8, #789) | unit `measurement_age_grows_the_reachable_disc` |
| WCET input bound + hoist equivalence (F9, #789) | unit `too_many_pedestrians_fails_closed`, `hoisted_breach_matches_naive_reference` (proptest) |


---

## WP-10 status update (2026-07-07)

The PRODUCER now exists: `kirra_taj::TajTracker::classify_pedestrians`
(WP-10) classifies tracked lidar clusters into `PerceivedPedestrian`s —
small footprint (cluster-extent, threaded through Phase-A before the lean
contract drops it) within the pedestrian speed envelope, with
classification UNCERTAINTY promoted TOWARD pedestrian (a first-sighting
track with unknown velocity classifies as one until tracking rules it
out); classified pedestrians are ADDITIVE (they remain vehicle-RSS
objects). The contract type moved to `kirra_core::trajectory::
PerceivedPedestrian` (re-exported from `kirra_trajectory::vru` unchanged)
so producer and checker share it through the lean contract crate, like
`PerceivedObject`. End-to-end proof: a Taj-classified kerbside pedestrian
makes the checker refuse an 8 m/s pass that is admitted without the VRU
channel (`classified_pedestrian_feeds_the_checker_reachable_set_bound`).
The adapter's `AdaptorState` carries the channel
(`update_pedestrians`/`snapshot_pedestrians`) with enabled-but-silent/
stale/poisoned → fail-closed semantics (the channel-B precedent).

**Remaining before the bound is LIVE on the Autoware node** (ros2-gated
`node.rs`, buildable only in the CI ros2 lane): the `~/input/pedestrians`
subscription (message-type mapping), the `KIRRA_VRU_RSS_ENABLED` env gate
(default off — byte-identical), the `snapshot_pedestrians` → MRC-on-None
consumption at the `validate_trajectory_slow_capped` call, replacing the
`None` at the WS-2 comment. All classifier params are VALIDATION-PENDING
(`kirra_taj::VruClassifierConfig`).
