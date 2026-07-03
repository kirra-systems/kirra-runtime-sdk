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
t_stop   = ρ + v / a_brake                       — time to full stop
d_stop   = v·ρ + v² / (2·a_brake)                — RSS stopping distance
required = d_stop + r_ped + v_ped_max · (t + t_stop) + clearance
UNSAFE   ⟺ dist(pose, ped) < required
```

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

`v = 2 m/s`, pose `t = 0`, defaults (ρ = 0.5 s, a_brake = 4.5 m/s²,
v_ped_max = 2.0, r_ped = 0.3, clearance = 0.5):

```text
t_stop   = 0.5 + 2/4.5            = 0.944 s
d_stop   = 2·0.5 + 4/(2·4.5)      = 1.444 m
reach    = 0.3 + 2.0 · 0.944      = 2.189 m
required = 1.444 + 2.189 + 0.5    = 4.133 m
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
| Non-positive / non-finite `a_brake` | `required = ∞` → any moving pose breaches (an unbrakeable ego cannot prove VRU safety) |
| Absent VRU channel (`None` scene) | **No-op** — byte-identical path (the derate-only invariant: absent input never relaxes, never fabricates) |

## 5. Parameters (`VruRssParams`) — all VALIDATION-PENDING

| Param | Default | Rationale / tuning obligation |
|---|---|---|
| `v_ped_max_mps` | 2.0 | Brisk walk (~1.4 typical walking; 2.0 covers hurried). **ODDs with expected runners, children darting, or cyclists-as-VRUs must raise it** — a raise only tightens the bound. Lowering below 2.0 needs ODD evidence + review. |
| `ped_radius_m` | 0.3 | Body half-width; not a point target. |
| `clearance_m` | 0.5 | Comfort/robustness margin beyond the geometric envelopes. |
| `reaction_time_s` | 0.5 | Matches the vehicle-RSS `RSS_REACTION_TIME_S` — one reaction model per checker. |
| `stop_epsilon_mps` | 0.05 | Matches `STOP_EPSILON_MPS` (the Degraded stop-and-hold epsilon). |
| `a_brake` | per-class `VehicleConfig::max_decel_mps2` | The ego's assured service braking from the per-class contract (`KIRRA_VEHICLE_CLASS`). Using full service braking is the *least* conservative element here; derating it (e.g. wet-surface factor) is a tracked refinement. |

Availability envelope at the defaults (euclidean `required`, pose `t = 0`):
ego 1 m/s → 3.0 m; 2 m/s → 4.1 m; 4 m/s → 7.1 m; 6 m/s → 10.7 m. The
courier persona (≤ ~2 m/s on sidewalks) keeps a ~4 m bubble around
pedestrians — conservative but operable; a robotaxi at speed must rely on
pedestrians being genuinely distant, which is correct.

## 6. Integration status & tracked follow-ups

**Live now:** the primitive (`vru.rs`) and its wiring as section D2 of
`validate_trajectory_slow_capped` behind an optional `PedestrianScene`
argument (absent → no-op). The WCET-critical per-pose
`validate_vehicle_command` path is untouched; the Nominal path without a
VRU channel is byte-identical.

**Follow-ups (in dependency order):**
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
| Fail-closed non-finite / unbrakeable | `vru_non_finite_pedestrian_mrcs`, unit `non_positive_brake_fails_closed`, `non_finite_pedestrian_breaches` |
