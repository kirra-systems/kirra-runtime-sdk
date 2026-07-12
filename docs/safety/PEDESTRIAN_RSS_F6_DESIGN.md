# Pedestrian VRU bound — F6 corridor-clip design proposal (#789, availability)

> **STATUS: DESIGN PROPOSAL — review-gated. No code in `vru.rs` changes until
> this is signed off.** F6 is the sole remaining #789 item; the soundness
> prerequisites (F4/F5/F8/F9) are merged and green, and the producer
> (`kirra_taj::TajTracker::classify_pedestrians`) is live. F6 is explicitly
> *"availability, strictly after soundness"* (`PEDESTRIAN_RSS.md` §6) — it
> **relaxes a now-sound safety bound**, so the module's standing rule applies:
> a relaxation is *"allowed only to RELAX this bound with validated evidence,
> never to weaken it silently"* (`vru.rs` module doc; §2.1 of the design doc).
> This document is that evidence-and-design gate; it does not itself change the
> bound.

## 1. The problem, quantified

The pure omnidirectional disc (`PEDESTRIAN_RSS.md` §2) grows the pedestrian's
reachable set as `reach = r_ped + v_ped_max · (age + t + t_stop)` in **every**
direction, measured as the **euclidean** distance from the ego pose. Two
consequences the issue names:

1. **Robotaxi over-conservatism.** At the ODD speed cap the stopping interval
   is long, so `required` reaches tens of metres — the issue cites **~78 m
   from ANY pedestrian**. At that radius essentially every pedestrian in a
   scene binds, including one on the far side of a physical median the
   pedestrian cannot cross within the stopping time. The bound is *sound* but
   *inoperable* for a structured-road robotaxi ODD.
2. **Binds a pedestrian behind a receding ego.** Each pose is checked
   statically against the pedestrian's (age-grown) position; a pedestrian just
   behind pose *k*, with the ego moving forward and away, still binds even
   though a pedestrian moving at ≤ `v_ped_max` can never catch an ego moving
   faster along the corridor.

Both are **availability** costs. Neither is a soundness bug — the pure disc is
a conservative *over-approximation* of where a pedestrian can be.

## 2. The soundness principle F6 must preserve

The euclidean disc over-approximates the pedestrian's true reachable set. F6
tightens that over-approximation toward reality **without ever going below
reality**. Formally, for stopping-interval budget `τ = age + t + t_stop`:

```
TRUE_reachable(τ)   ⊆   GEODESIC_reachable(τ)   ⊆   EUCLIDEAN_disc(τ)
        (real pedestrian)      (F6 model)          (current bound)
```

- **`EUCLIDEAN_disc(τ)`** = `{ p : ‖p − ped‖ ≤ v_ped_max·τ }` — today's bound.
- **`GEODESIC_reachable(τ)`** = points reachable by a path of length
  ≤ `v_ped_max·τ` that **respects impassable boundaries** (a pedestrian must
  walk *around* a physical barrier, not through it).
- Because any barrier-respecting path is **≥** the straight-line distance,
  `GEODESIC ⊆ EUCLIDEAN`. So checking the ego stopping envelope against the
  geodesic set yields **fewer or equal breaches** → a relaxation.
- Because a real pedestrian *also* cannot pass through a physical barrier,
  `TRUE ⊆ GEODESIC` → the geodesic set still **contains** every real
  pedestrian position → **still sound**.

**The relaxation is sound iff the boundaries it treats as impassable are
genuinely impassable within the ODD.** This is the single load-bearing
assumption and the reason F6 needs road-structure *evidence*, not just the
corridor polyline (§3.2). Treating a **paint line** as a barrier would exclude
a pedestrian who steps over it → `TRUE ⊄ GEODESIC` → **unsound**. This is the
failure mode the review must guard.

## 3. The mechanism

### 3.1 Corridor-geodesic distance (the "distance-to-path" term)

Replace the euclidean `dist(pose, ped)` in the `UNSAFE ⟺ dist < required` test
with a **lower bound on the geodesic distance** from the pedestrian to the ego
body at that pose, where the geodesic respects impassable boundaries:

```
dist_geo(pose, ped) = length of the shortest ped→pose path that does not
                      cross an impassable boundary        ( ≥ ‖pose − ped‖ )
UNSAFE ⟺ dist_geo(pose, ped) < required
```

**Using a *lower bound* on the geodesic keeps it sound.** A lower bound
over-estimates reachability (a larger reachable set, a smaller effective
distance) → **more** conservative than the true geodesic → still ⊇ TRUE. The
trivial lower bound is the euclidean distance itself (`dist_geo = ‖·‖` → the
current pure disc). Any *tighter* lower bound relaxes, and every intermediate
choice is sound. This gives a **WCET-bounded, allocation-free** implementation
(§3.4): we need not compute the exact geodesic (a visibility-graph /
wavefront, unbounded) — only a cheap provable lower bound.

### 3.2 The impassability evidence gate (the safety-critical input)

The `CorridorSource` trait (`kirra_core::corridor`) today exposes
`left_boundary()` / `right_boundary()` polylines with a `confidence()` and
`age_ms()` — **but no boundary *type*.** A Lanelet2 boundary can be a physical
barrier (kerb, jersey barrier, wall, guardrail) **or** a painted line a
pedestrian steps over. F6 **must not** clip on a boundary whose impassability
is not evidenced. Two ways to supply that evidence (a decision for review —
§8):

- **(a) A boundary-impassability accessor.** Extend the corridor seam with a
  per-boundary-segment `impassable: bool` (or a `BoundaryKind` enum mapped to
  impassability per ODD). Production Lanelet2 carries `type`/`subtype` tags
  (`curbstone`, `fence`, `road_border`, `line_thin`/`line_thick`) — the ODD's
  reviewed map profile decides which map to impassable.
- **(b) A separate `ImpassableBarriers` input** (a list of barrier segments)
  distinct from the drivable corridor, so the clip never accidentally treats a
  drivable-corridor edge (which may be paint) as a wall.

Until such evidence exists for an ODD, **F6 is inert for that ODD** — the bound
is the pure disc, by construction (§5). This mirrors how the directed-cone
refinement (§2.1) stays inert without validated tracking.

### 3.3 The "behind a receding ego" case falls out

A pedestrian behind pose *k* whose only route into the ego's forward path lies
**along the corridor** has a geodesic distance ≥ their along-corridor arc to
the pose — which grows as the ego advances. Where the corridor is effectively
1-D (a lane between two impassable edges), the geodesic naturally encodes "a
slower pedestrian cannot catch a faster receding ego from behind," with **no
separate behind-ego filter** (which §2 deliberately refused, because a naive
half-plane filter is unsound on curved paths). The behind-ego relief is thus a
*consequence* of the sound corridor-geodesic, not a special case — and it is
only as strong as the impassability evidence (§3.2).

### 3.4 WCET-bounded lower bound (implementation sketch, non-binding)

For the slow-loop budget, a bounded lower bound on the geodesic that captures
the dominant robotaxi win (a pedestrian separated from the pose by one
impassable boundary):

- If the straight segment `ped→pose` does **not** cross any impassable
  boundary segment → `dist_geo = ‖ped − pose‖` (no relaxation; the euclidean
  bound stands).
- If it crosses impassable boundary segments, take
  `dist_geo = min over crossed segments of (‖ped − e‖ + ‖e − pose‖)` for the
  segment endpoints `e` — the shortest single-detour-around-an-endpoint path.
  This is a **provable lower bound** on the true geodesic (the true path is at
  least a detour around *some* endpoint of *some* crossed barrier), so it stays
  sound, and it is `O(B)` per (pose, pedestrian) with `B` bounded barrier
  segments → `O(T·P·B)`, allocation-free, in keeping with F9's WCET discipline.
- Fail-closed: any non-finite boundary vertex → treat as **no relaxation**
  (fall back to euclidean for that pair), never a spurious detour that shrinks
  the requirement.

(Exact geodesic / multi-barrier detours are a later refinement; the
single-detour lower bound is the minimal sound step and is enough to lift the
robotaxi ODD off the ~78 m floor.)

## 4. ODD scoping

- **Default = pure disc.** No corridor/barrier input → byte-identical current
  behaviour (the derate-only invariant). This is non-negotiable: the clip is
  opt-in, never a silent default.
- **Sidewalk-courier ODD → pure disc, always.** Unstructured space, pedestrians
  everywhere, no reliable barrier map. The asymmetric "admit stopping, refuse
  moving-through" posture (§3) is exactly right there; F6 does not apply.
- **Robotaxi / structured-road ODD → opt-in corridor-clip**, gated on a
  reviewed barrier-impassability map profile (§3.2) with map confidence/age
  thresholds (§5). This is where the ~78 m over-conservatism lives and where
  the barrier evidence is available.

## 5. Fail-closed → fall back to the pure disc

Every fault falls back to the **more conservative** pure disc, never to a
weaker clip:

| Condition | Treatment |
|---|---|
| No corridor / no barrier input | Pure disc (no-op; byte-identical) |
| Corridor `confidence()` below the ODD floor | Pure disc |
| Corridor `age_ms()` over the ODD budget | Pure disc |
| Missing/absent boundary-impassability evidence | Pure disc (F6 inert) |
| Non-finite boundary vertex | Pure disc **for that (pose, ped) pair** |
| ODD not on the reviewed clip allow-list | Pure disc |

The clip can therefore only ever be an *additive relaxation on top of a
sound floor*; a corrupt or stale map degrades to the current bound, not below
it.

## 6. Proposed interface (keeps the derate-only invariant)

Thread the corridor + barrier evidence as an **optional** field on
`PedestrianScene` (absent → no-op), so the call site stays one optional
argument and the Nominal-without-VRU path is untouched:

```rust
pub struct PedestrianScene<'a> {
    pub pedestrians: &'a [PerceivedPedestrian],
    pub params: VruRssParams,
    /// F6 (opt-in): impassable barrier segments + gating. `None` → pure disc.
    pub reachability: Option<CorridorReachability<'a>>,   // NEW, default None
}
```

`pedestrian_breach` computes `dist_geo` instead of `‖·‖` **only** when
`reachability` is `Some` and passes its ODD/confidence/age gates; otherwise it
is the current euclidean path, unchanged. `CorridorReachability` carries the
impassable segments and the ODD gate — never the raw drivable-corridor edges,
so a paint edge can never leak in (§3.2 option (b) preferred for exactly this
reason).

**No signature of an existing public fn changes meaning**; the euclidean path
stays the default and the `None` branch is byte-identical to today.

## 7. Soundness obligations & test plan (to land WITH the code, not after)

1. **Subset property (the core soundness proptest).** For random scenes and
   random barrier sets, `dist_geo ≥ ‖pose − ped‖` **always** (the clip never
   shrinks distance below euclidean) ⇒ `required` is never harder to meet than
   the pure disc, and the clipped bound **never admits a trajectory the pure
   disc refused *unless* a barrier separates the pedestrian from the pose.**
2. **Never-weaker-than-reality negative control.** A pedestrian whose only
   separating boundary is marked **passable** (paint) must bind **exactly as
   the pure disc does** — the clip must not relax across a passable edge. This
   is the unsound-if-wrong case (§2) and gets an explicit failing-if-broken
   test.
3. **Robotaxi availability win.** A pedestrian across an impassable median,
   within the euclidean disc, is **admitted** — the intended relaxation.
4. **Courier default.** With `reachability: None` the verdict is identical to
   the current bound over the whole existing proptest corpus
   (`hoisted_breach_matches_naive_reference` extended with a `None` arm).
5. **Fail-closed fallbacks (§5)** each get a unit test asserting the pure-disc
   verdict.
6. **WCET.** `O(T·P·B)` with `B ≤ MAX_BARRIERS` (a new fail-closed input bound
   mirroring `MAX_PEDESTRIANS`); allocation-free; the `wcet_gate` argument
   extended.

## 8. What this design deliberately does NOT decide (needs your sign-off)

These are safety/ODD decisions, not implementation details — they are why F6 is
review-gated and not a mechanical fix:

- **The boundary-type → impassability map.** Which Lanelet2 boundary
  types/subtypes count as impassable, **per ODD**. This is the load-bearing
  soundness input (§2). Requires the map profile owner's sign-off and,
  ideally, field evidence that the mapped barriers are physically uncrossable
  within the stopping time.
- **Evidence-supply mechanism:** extend `CorridorSource` with impassability
  (§3.2 a) vs. a separate `ImpassableBarriers` input (§3.2 b). Recommendation:
  **(b)** — a distinct barrier input can never accidentally clip on a drivable
  (possibly painted) corridor edge.
- **ODD allow-list + map confidence/age thresholds** for enabling the clip.
- **Whether the single-detour lower bound (§3.4) is sufficient** for the
  target ODD, or the exact multi-barrier geodesic is needed (a later, larger
  refinement).

## 9. Rollout

Derate-only, review-gated, staged — the same discipline as every predictive
bound in the checker:

1. This design signed off (boundary-impassability profile + evidence mechanism
   chosen).
2. `ImpassableBarriers` seam + `MAX_BARRIERS` bound + the §3.4 lower bound,
   **default-off** (`reachability: None` everywhere until an ODD opts in).
3. The soundness proptest + negative controls (§7) land with the code.
4. Per-ODD enablement is a separate, explicitly-reviewed change carrying the
   map profile — never bundled with the mechanism.

Until step 1 is signed off, `vru.rs` is unchanged and the pure disc remains the
bound for every ODD.
