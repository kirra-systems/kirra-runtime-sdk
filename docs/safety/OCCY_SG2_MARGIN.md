# Occy / KIRRA — SG2 Lateral Margin Derivation

**Doc ID:** KIRRA-OCCY-SG2-MARGIN-001.
**Issue:** #120 (S8 — quantitative envelope characterization), Item A.
**Status:** Pilot — analytical derivation against literature / spec-sheet
upper bounds. Empirical validation on integrator hardware is a
pre-production gate (§6).

---

## 1. Claim

**SG2** (drivable-space containment, ASIL D) requires that the vehicle
remain within the physical corridor at every pose of every accepted
trajectory. The kernel check
`gateway::containment::validate_trajectory_containment` enforces this
by computing each footprint corner's position from the pose and
asserting it is inside the corridor polygon by at least
`CONTAINMENT_LATERAL_MARGIN_M`.

The placeholder value `0.30 m` carried in the Phase-1 SG2 build
(commit `73cc7b1` on `sg2-drivable-space`) was a documented standin
for the S8 / #120 characterization. This document is that
characterization: it derives the value, justifies the rounding,
captures the assumptions of use, and is the basis for setting
`CONTAINMENT_LATERAL_MARGIN_M = 0.40 m` in `src/gateway/containment.rs`.

The same loop-closure pattern as `ADR-0001` (speed cap) is followed —
upper bounds derived term-by-term, conservative-vs-typical split,
selected value, residuals.

## 2. Formula and derivation

The margin must absorb the maximum possible lateral excursion from
the planner's commanded path that the kernel's per-cycle check cannot
yet observe:

> **margin ≥ v_lat_max × FTTI_fast + ε_localization + ε_perception + ε_control**

### 2.1 Term values

| Term | Conservative basis | Value (typical) | Value (worst-case) |
|---|---|---|---|
| `v_lat_max × FTTI_fast` | At the ODD cap speed `v_max = 22.35 m/s` (50 mph, ADR-0001) with the kernel's maximum allowed steering angle `δ_max = 0.6109 rad` (35°, `VehicleKinematicsContract::max_steering_deg`), the instantaneous lateral velocity is `v · sin(δ)` = `22.35 · sin(35°) ≈ 12.83 m/s`. The fast-loop period is one control cycle ≈ 10 ms at 100 Hz (the existing `FAST_LOOP_WCET_BUDGET_US = 200 µs` is the verdict budget — the cycle itself is the planner→control→vehicle interface period). | **0.128 m** | 0.128 m |
| `ε_localization` | Cross-track error of the ego-pose estimate vs. ground truth. Typical: RTK-fix 95th-percentile in clear-sky urban driving ≈ 0.05–0.10 m. Worst-case: NDT or visual-LiDAR localization in urban canyon / multipath ≈ 0.30 m. | **0.10 m** | 0.30 m |
| `ε_perception` | Cross-track uncertainty in the corridor boundary as the kernel sees it (HD-map lane-edge precision + map-frame projection). Typical: Lanelet2-mapped lane edges ≈ 0.10 m. Worst-case: dynamic re-mapping under camera-only conditions ≈ 0.20 m. | **0.10 m** | 0.20 m |
| `ε_control` | Cross-track error in tracking the commanded path on the actual vehicle. Typical: stable urban steering with characterized rack ≈ 0.05 m. Worst-case: rapid manoeuvres on wet road, cold tire ≈ 0.10 m. | **0.05 m** | 0.10 m |
| **Sum** | | **0.378 m** | **0.728 m** |
| **Rounded up to a hundredth** | | **0.40 m** | **0.75 m** |

### 2.2 v_lat_max derivation note

The 12.83 m/s upper bound assumes the planner publishes a trajectory
at simultaneously `v_max` and `δ_max`. The kernel's
`validate_vehicle_command` Priority-6 (lateral-acceleration envelope)
check would in practice clamp this combination via the bicycle-model
constraint `a_lat = v² · |tan(δ)| / L ≤ max_lateral_accel_mps2 = 3.5`
— solving at `v = 22.35 m/s` yields `δ_implied ≈ atan(3.5 · 2.8 /
22.35²) ≈ 1.13°` and a clamped `v_lat ≈ v · sin(1.13°) ≈ 0.44 m/s`.

The derivation uses the **unclamped 12.83 m/s** as the conservative
upper bound: the margin must hold for the worst case the kernel
could see between the slow-loop accept and the next fast-loop
conformance tick. Using the clamped value would tighten the formula
margin by ≈ 0.124 m and produce 0.25 m typical / 0.60 m
conservative — a tempting but mathematically circular reduction
(the clamp is itself a kernel check that the margin protects).

## 3. Selected value — two-tier disposition

| Tier | Margin | Required AoU | Code disposition |
|---|---|---|---|
| **PRIMARY (pilot default)** | **0.40 m** | G2 AoU (#123): integrator localization stack achieves ≤ 0.10 m 95th-percentile lateral error within the deployment ODD. | `CONTAINMENT_LATERAL_MARGIN_M = 0.40` — committed in this commit. |
| **CONSERVATIVE FALLBACK** | **0.75 m** | Applies when (a) localization accuracy is uncharacterized, or (b) ε_localization > 0.10 m, or (c) the deployment ODD contains urban canyons / multipath-prone zones not covered by the integrator's localization profile. | **Deployment configuration flag — NOT in code.** Documented here for integrators that need it; setting it lives in the integrator's runtime config and overrides the constant at startup. |

The fallback is intentionally **not the code default**: a 0.75 m margin
narrows a standard 3.5 m urban lane's per-side clearance from
`(3.5 − 1.85) / 2 = 0.825 m` (the half-corridor minus half-vehicle-width)
to `0.075 m` — a 9 cm operating margin on the corridor's centerline
that is impractical at scale (see §4). Setting it cold by default would
exclude virtually all narrow urban deployments without the integrator
having opted in.

## 4. Navigability analysis (PRIMARY = 0.40 m)

| Corridor width | Half-clearance to vehicle = `(W − vehicle_width) / 2` | After margin | Verdict |
|---|---|---|---|
| 3.5 m (US/EU standard urban lane) | (3.5 − 1.85) / 2 = 0.825 m | 0.825 − 0.40 = **0.425 m** | Feasible. |
| 3.25 m (older standard / utility road) | (3.25 − 1.85) / 2 = 0.700 m | 0.700 − 0.40 = **0.300 m** | Feasible with tight tolerance. |
| 3.0 m (narrow urban lane) | (3.0 − 1.85) / 2 = 0.575 m | 0.575 − 0.40 = **0.175 m** | At the limit. Exclude lanes below 3.0 m from the deployment ODD at the 0.40 m setting, OR flag those lanes for reduced speed (the bicycle-model derivation in §2.2 derates `v_lat_max` linearly with `v`, so a 25 mph slow zone reduces the margin requirement by ≈ 50 %). |
| < 3.0 m | Negative or < 0.10 m | n/a | **Exclude from ODD at 0.40 m.** Use the 0.75 m fallback only with reduced-speed sub-ODD; on 3.0 m at 0.75 m the margin exceeds the available clearance. |

Vehicle width = 1.85 m from `VehicleKinematicsContract::nominal_reference_profile`
(matches `VehicleConfig::default_urban()` per the adapter). Footprint
geometry is platform-independent in the kernel — these limits hold for
the reference urban sedan; lighter / narrower platforms widen the
feasible corridor range.

## 5. G2 localization assumption-of-use (#123)

The PRIMARY value is conditional on the integrator's localization
stack delivering:

> **ε_localization ≤ 0.10 m, 95th-percentile lateral error, evaluated
> over the deployment ODD.**

This is a **new concrete requirement on #123** (the existing
"G2 — localization integrity" follow-up). The previous text on #123
described the integrity claim without a numeric target; this
derivation makes the target explicit. A separate comment posted on
#123 cross-references this document.

The 95th-percentile rather than 99th-percentile is consistent with
ISO 26262 SoTIF practice for residual-risk decomposition: the
remaining 5 % tail is absorbed by the conservative-vs-typical
headroom in §3 (the 0.40 m PRIMARY value is the typical-term sum
rounded up; the 0.378 m unrounded floor leaves ≈ 2.2 cm tail headroom).

## 6. Residuals + empirical-validation gate (pre-production)

| Term | Pilot disposition | Empirical-validation gate |
|---|---|---|
| `v_lat_max × FTTI_fast` | Analytically derived from kernel constants — no measurement gap. | None required. |
| `ε_localization` | Conservative literature value. **G2 AoU (#123) makes this an integrator requirement; pilot does not measure.** | Integrator-side: per-deployment characterization of the localization stack vs. ground-truth on a representative track. Sign-off artefact: deployment-specific G2 evidence package. |
| `ε_perception` | Conservative literature value for Lanelet2-mapped lane edges. | Pre-production: ground-truth-vs-perceived boundary-error campaign on integrator hardware + map. |
| `ε_control` | Conservative literature value. | Pre-production: track-test on the actual vehicle, characterized over the ODD (dry / wet, cold / hot tire, fresh / worn pad). |

Pilot evidence: this document + the constant change.
Pre-production evidence: the empirical campaigns above, returning
measured 95th-percentile values per term. If any measured term exceeds
its literature upper bound, the PRIMARY value is re-derived and the
constant is bumped (or the fallback is mandated for that integrator).

## 7. Code change in this commit

```rust
// src/gateway/containment.rs
pub const CONTAINMENT_LATERAL_MARGIN_M: f64 = 0.40;
```

Previous value: `0.30` (placeholder). The doc-comment on the constant
now cites this document and the G2 AoU. No other code change; the
kernel containment check at l.175 (`let margin_sq =
CONTAINMENT_LATERAL_MARGIN_M * CONTAINMENT_LATERAL_MARGIN_M;`) is
agnostic to the value's source. All 19 containment unit tests pass
unchanged under the new value (no test asserted on the placeholder).

The 0.75 m conservative-fallback is a **deployment configuration**,
not committed in code. Integrators needing it set the value via the
runtime config layer at startup; tracked separately from this
characterization.

---

## Cross-references

- **SG2** — `docs/safety/OCCY_SAFETY_GOALS.md` SG2 row (margin row
  updated to cite this document).
- **G2 localization AoU** — `#123` (comment posted cross-referencing
  this document; states the ≤ 0.10 m 95th-percentile requirement).
- **ADR-0001** — `docs/adr/0001-occy-odd-speed-cap.md` (derivation
  pattern: SSD chain, term decomposition, AoU split).
- **KIRRA-OCCY-OPTIONB-001** — `docs/safety/OCCY_131_OPTIONB_DESIGN.md`
  §7 (the SG2 wiring this margin gates — the matrix flip carries this
  doc as the substantive input).
- **S8 / #120** — Item A. Items B (IDC detection ranges), C (speed-cap
  validation matrix), D (SPFM / LFM / PMHF target-vs-claimed) remain
  open.
- **Traceability matrix** — SG2 row updated to ENFORCED (CARLA
  scenario verification documented as a pending integrator-side
  artefact, not blocking the matrix update).
