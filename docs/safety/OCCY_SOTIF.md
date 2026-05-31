# Occy / KIRRA — ODD & SOTIF (ISO 21448)

**Issue:** S4 (#116) — ODD + triggering-condition catalog + acceptance argument.
**Doc ID (proposed):** KIRRA-OCCY-ODD-001.
**Status:** Working draft for review. The ODD parameters in §1 are **signed
off** (full-driverless decision, 2026-05-31): urban / surface, day + night,
all-weather, water / rail in scope, 50 mph cap. FTTI absolutes and the SG4 /
SG5 ASILs have been propagated to OCCY_SAFETY_GOALS.md (#113); the cap-as-
function-of-measured-detection-range rule waits on S8 (#120) and the Governor
WCET bound on S3 (#115). Not a certified analysis.

---

## 1. Operational Design Domain

Two ODDs must be distinguished — conflating them is a common error:

- **V&V / simulation ODD** — what we *test* against. Deliberately includes
  injected triggering conditions (flooded segments, rail crossings, staged
  collisions, teleop handoffs) so the Governor's responses can be proven. High
  test frequency here is not "exposure."
- **Deployment ODD** — what we *operate* in. This is what sets ISO 26262
  Exposure (E) and therefore the ASILs, and what an initial constrained
  deployment can narrow to *defer* hazards we can't yet handle. A constrained
  deployment ODD is itself a SOTIF risk-reduction measure.

### 1.1 Phase-1 deployment ODD (signed off, 2026-05-31)

| Dimension | Phase-1 deployment | Notes |
|---|---|---|
| Road types | Urban / suburban surface streets, marked lanes | Highways / ramps deferred to a future ODD |
| Speed range | ≤ 50 mph (80 km/h, 22.35 m/s) — clear-conditions maximum; dynamic derate in degraded conditions (see ADR-0001 + #99) | Sets look-ahead FTTI (see §5); cap-as-function-of-measured-detection-range rule per ADR-0001 |
| Lighting | Day + night | Full-feature lighting envelope |
| Weather | All-weather with conservative water rule + dynamic speed derating | Water / rain / fog / flooding **in scope** (NOT excluded); SG4 active (conservative untraversable + earn-back #98); cap derates in degraded perception (#99) |
| Junctions | Signalized + unsignalized intersections | |
| Commit zones | Rail crossings present on route | In-scope; SG5 active |
| Dynamic agents | Vehicles, pedestrians, cyclists | Trains at crossings (high-consequence class) |
| Teleop | Available as remote-assist | SG7 active |
| Controllability basis | Driverless (C3) | No human-in-the-loop fallback; C3 is the design basis for all ASIL assignments |
| Subject vehicle | Single platform; dynamics per S8 (#120) | Stopping model is an S8 input; cap presumes worst-case detection range R supports ≥ 94 m look-ahead |

The Phase-1 deployment ODD **includes** flood-prone and adverse-weather
conditions, handled via the conservative SG4 untraversable-default rule +
depth-evidence earn-back (#98) and the flood demo (#100). Rail crossings are
**included** (SG5 active). Highways / ramps are **deferred** to a later ODD.
This is the full-driverless deployment basis: all core safety goals carry
ASIL D under C3 controllability; SG4 and SG5 are **active at ASIL B** (not QM).

### 1.2 Target ODD

The L4 generalization expands the road envelope to include highways / ramps
and raises the speed cap (per a separate S8 validation that the worst-case
detection range supports the higher cap's required look-ahead). Weather, night,
water-prone, and rail-dense regions are already in Phase-1 scope. ODD expansion
remains the unit of safety-scope growth: each expansion is a distinct S8
evidence package + ADR + re-validation of the cap rule.

---

## 2. SOTIF framing — where the ODD boundary lives

ISO 21448 partitions behavior into four areas: (1) known-safe, (2) known-unsafe,
(3) unknown-unsafe, (4) unknown-safe. The SOTIF goal is to shrink areas 2 and 3.

- **Area 1 (known-safe):** in-ODD nominal driving.
- **Area 2 (known-unsafe):** identified triggering conditions — the catalog in
  §3. Each gets a Governor mitigation + MRC member + verification.
- **Area 3 (unknown-unsafe):** not-yet-identified triggering conditions. **This
  is where the doer/checker architecture is a structural SOTIF advantage:** the
  independent Governor's drivable-space + conservative RSS + fail-closed posture
  is *default-deny*, so an un-enumerated hazardous trajectory is still likely
  rejected without anyone having catalogued its triggering condition first. The
  conservative checker converts many Area-3 unknowns into safe rejections. This
  is a claim worth making explicitly in the UL 4600 safety case (S5).
- **ODD-exit detection:** when conditions leave the deployment ODD (flooding
  beyond threshold, perception degraded beyond bound), the system degrades
  posture and commits an MRC rather than operating out-of-ODD.

---

## 3. Triggering-condition catalog (living)

Structure per entry: ID · triggering condition · functional insufficiency
exploited · resulting hazardous behavior (→ H/SG) · mitigation (Governor +
MRC) · ODD relation · verification. **TC-01..04 are the resourced clusters;
TC-05..08 are seeded** (identified + mitigation noted; promote to a full cluster
— umbrella + issues — when dedicated work is warranted).

| TC | Triggering condition | Functional insufficiency | Hazard → SG | Mitigation | Verify |
|----|----|----|----|----|----|
| TC-01 | Standing water / flooding | Depth is unobservable; flooded road looks drivable | H4 → SG4 | WATER_UNTRAVERSABLE default + depth-evidence earn-back (#98) + stop-short MRC | flood demo #100 |
| TC-02 | Rail crossing / commit zone | Gate/lights/train may be missed; can enter or stop in zone | H5 → SG5 | Map-anchored COMMIT_ZONE_BLOCKED + exit-clearance + train prediction (#108) + stop-short MRC | commit-zone demo #109 |
| TC-03 | Post-collision, object in near-field | Pull-over logic ignores a person under/near vehicle; impact ambiguous | H6 → SG6 | Impact latch + immobilize + clearance gate (#102/#103) | post-collision demo #105 |
| TC-04 | Teleoperator handoff / remote command | Human fallback is itself error-prone; may bypass safety logic | H7 → SG7 | Doer-agnostic Governor check on all commands; handoff routed through Governor | teleop injection #112 |
| TC-05 | Occluded VRU (pedestrian from behind parked car / blind corner) | Perception can't see the occluded agent; speed unsafe for sightline | H1 → SG1 | RSS limited-visibility caution (rule iv): conservative speed bound vs. occlusion — **see gap G1** | occluded-VRU scenario (Phase 2) |
| TC-06 | Sensor degradation / adverse weather | Degraded perception with unflagged low confidence | H9 → SG9 | World-model freshness + confidence + fail-toward-stale + weather posture coupling (#99) | stale/low-confidence injection |
| TC-07 | Map / localization error near a map-anchored hazard | Map-anchored checks rely on correct localization | H5/H4 → SG5/SG4 | Localization-confidence gating; map prior as backstop not sole reliance — **see gap G2** | localization-drift injection |
| TC-08 | Stopped/stalled lead or debris in lane | Static-object detection weaker than moving-object | H1 → SG1 | Forward-collision / drivable-space treats static objects | static-obstacle scenario |

New triggering conditions are added here (not as one-offs); promotion to a
cluster happens when an entry warrants dedicated issues.

---

## 4. SOTIF acceptance argument

Absence of unreasonable risk from functional insufficiency is argued by:

1. **ODD bounds the intended conditions** (§1) — in-ODD = Area 1.
2. **Known triggering conditions (Area 2) are catalogued** (§3), each with a
   Governor mitigation, an MRC member, and a verification artifact.
3. **Unknown triggering conditions (Area 3) are reduced by the conservative
   default-deny architecture** (§2) — the independent Governor rejects unsafe
   trajectories without prior enumeration of their cause.
4. **ODD-exit triggers an MRC** rather than out-of-ODD operation.
5. **Residual risk is monitored** via the V&V coverage argument (S8/#120) and
   the SPIs sourced from the audit chain (S5/#118).

**Acceptance criterion** ties to the quantitative Driving-Policy target in S8
and the UL 4600 top goal (absence of unreasonable risk). The criterion is not
"zero catalogued hazards" but "catalogued hazards mitigated + a credible
argument that the residual (Area 3) rate is acceptable, backed by the
conservative architecture and coverage evidence."

---

## 5. Feedback into the safety goals (the unlock)

Fixing the ODD resolves the two things S1 deferred.

**FTTI absolutes.** With locked deployment-ODD cap v_max = 22.35 m/s (50 mph):
- *Per-cycle goals* (SG1/2/3/7/9): FTTI = verdict before actuation within one
  control cycle; the 0.5 s chain reaction-time budget is the dead time; the
  Governor WCET is a slice of that (target ≪ 0.5 s, exact bound proven in
  S3/#115).
- *Look-ahead goals* (SG4/SG5): stopping sight distance at the cap with
  t_react = 0.5 s and comfortable a = 3 m/s² is SSD = v·t + v²/(2a) ≈ 11.2 +
  83.3 = **≈ 94 m look-ahead**. The MRC controlled stop (kinematic only) is
  ≈ v²/(2a) ≈ **83 m** comfortable / **50 m** firm (5 m/s²). The 94 m look-
  ahead bounds both sensing/map range and Governor horizon; reliable dry
  detection range R ≈ 130 m gives ~28% margin. The cap is **a function of the
  S8-measured worst-case (wet/night) R** — if S8 shows R drops below the level
  needed to support 94 m look-ahead, the cap derates per SSD(v) = R. See
  SPEED_ENVELOPE.md (KIRRA-OCCY-SPEED-001) and ADR-0001 for the derivation,
  the breaking-point analysis (~60 mph comfortable basis), and the cap-raise
  gate. *Dynamic derating in degraded conditions ties to the weather-posture
  coupling (#99).*

**SG4/SG5 ASIL re-rating against the Phase-1 deployment ODD:**

| SG | S1 (target-ODD) ASIL | Phase-1 deployment ODD | Rationale |
|----|----|----|----|
| SG4 water | B (C if flood-prone) | **B (active)** | Water is in the deployment ODD per the full-driverless decision; conservative untraversable-default + depth-evidence earn-back (#98); flood demo #100 validates the mechanism |
| SG5 commit-zone | B (C if crossing-dense) | **B** | Rail crossings in-scope; standard urban density → E2 |

**SG6** rates ASIL A by the S/E/C table (low exposure) but is **developed to
elevated rigor** (hard constraint per owner decision) given its catastrophic
severity and the known reputation-ending failure mode it addresses.

ODD sign-off completed 2026-05-31. OCCY_SAFETY_GOALS.md (#113) has been
updated: FTTI absolutes filled per the 50 mph cap, SG4 = B (active, water in
scope), SG5 = B, SG6 elevated rigor. Both #113 (S1) and this issue (#116, S4)
close on commit. The Governor WCET bound flows to S3 (#115); the cap-as-
function-of-detection-range validation flows to S8 (#120).

---

## 6. Gaps surfaced by this analysis (file as new work)

Doing the catalog properly surfaced two issues the plan doesn't yet cover —
which is the catalog doing its job:

- **G1 — Occlusion / limited-visibility caution (RSS rule iv).** Phase-1 Governor
  RSS is single-agent longitudinal; it does not yet bound speed by occlusion /
  available sightline. TC-05 needs an explicit occlusion-aware caution check.
  *Proposed: a Phase-2 Governor issue (ws:governor), or fold into the Phase-2
  RSS epic (#94).*
- **G2 — Localization-integrity coupling.** The map-anchored checks (SG5, and
  water priors) depend on localization being correct, so localization is a
  safety-relevant input — and likely a **common-cause input to both Occy and the
  Governor**, which is exactly what the S2 (#114) DFA must examine. *Proposed:
  raise in S2's DFA scope, and/or a dedicated localization-confidence-gating
  issue.*

---

## Cross-references

OCCY_SAFETY_GOALS.md (#113) · S2 DFA (#114) · S3 WCET (#115) · S5 UL 4600/SPIs
(#118) · S8 V&V/target (#120) · clusters #97–112 · AEGIS-SG-001 (governor-level
goals). Register as KIRRA-OCCY-ODD-001 in SAFETY_CASE_INDEX.md.
