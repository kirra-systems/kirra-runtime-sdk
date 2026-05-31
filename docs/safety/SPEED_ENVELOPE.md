# Occy / KIRRA — Speed-Envelope Analysis (ODD speed cap rationale)

**Feeds:** S4 (#116) ODD speed cap; FTTI absolutes in OCCY_SAFETY_GOALS.md
(#113); S8 (#120) validation of the detection-range assumption.
**Doc ID:** KIRRA-OCCY-SPEED-001.
**Status:** Working analysis for review. The detection-range figure is the
dominant assumption and must be validated empirically with the actual sensor
suite (S8); the cap follows from it.

---

## 1. The governing relationship

The safety envelope closes where the **required stopping sight distance (SSD)**
— the distance needed to reliably detect a hazard and stop short of it — exceeds
the **range at which perception reliably detects and classifies the worst-case
object** (low-contrast, low-lying: debris, a dark-clad person, a water
boundary).

    SSD(v) = v · t_react  +  v² / (2a)
    Breaking point:  SSD(v) = R_reliable

Design to the *worst-case object class* and *degraded conditions*, not the easy
case (a stopped truck in clear daylight is seen much farther than a tire in
rain).

## 2. Assumptions (flex these)

- t_react = 0.5 s — full chain: confirm detection (multi-frame, to avoid
  false-positive hard stops) + plan + Governor verdict + actuation onset.
- a_comfortable = 3.0 m/s² — routine, passenger-friendly, achievable on wet
  roads. **The correct design basis.**
- a_firm = 5.0 m/s² — emergency, still plausible on damp pavement, NOT max. Held
  in reserve, not a routine basis.
- R_reliable ≈ 130 m — best-in-class confident detection of the worst-case
  object, dry. **Degrades in rain/fog/spray — i.e., exactly flood conditions.**

## 3. SSD table

| Speed | React | MRC stop (firm) | SSD comfortable | SSD firm | vs ~130 m |
|---|---|---|---|---|---|
| 45 mph (20.1 m/s) | 10 m | 41 m | 78 m | 51 m | comfortable, big margin |
| 50 mph (22.4 m/s) | 11 m | 50 m | 94 m | 61 m | comfortable, ~28% margin |
| 55 mph (24.6 m/s) | 12 m | 61 m | 113 m | 73 m | comfortable but thin (~13%) |
| 60 mph (26.8 m/s) | 13 m | 72 m | 133 m | 85 m | **comfortable basis breaks** |
| 65 mph (29.1 m/s) | 15 m | 85 m | 155 m | 99 m | firm only, no margin |
| 70 mph (31.3 m/s) | 16 m | 98 m | 179 m | 114 m | firm only, thin |
| 75 mph (33.5 m/s) | 17 m | 112 m | 204 m | 129 m | **firm basis breaks** |
| 80 mph (35.8 m/s) | 18 m | 128 m | 231 m | 146 m | past the wall on both |

## 4. Breaking points

- **Comfortable-decel basis: ~60 mph.** The defensible breaking point — it
  doesn't depend on emergency braking being available (it often isn't, on wet
  roads) and doesn't make hard stops routine.
- **Firm-decel basis: ~75 mph.** Only valid with zero margin, dry pavement, and
  the assumption that R stays at 130 m in all conditions (it won't). Not a
  safety basis.
- **80 mph is ~100 m past the wall** — it needs ~230 m of reliable look-ahead
  that current perception does not provide for the worst-case object class.

## 5. Recommended cap: 50 mph (80 km/h)

Do **not** set the cap "just under" the breaking point: the breaking point is
computed on nominal/dry/full-range/nominal-latency assumptions, all of which
degrade in the field (wet roads cut μ and braking; rain/fog/spray cut R; sensors
age; low-contrast objects are seen later; latency spikes). Each lowers the real
breaking point below the table.

Set the cap where there is comfortable-stop capability **plus** degradation
margin:

- **50 mph / 80 km/h:** comfortable SSD 94 m inside 130 m (~28% headroom); firm
  braking entirely in reserve; room for wet roads and reduced visibility. Also
  sits inside the urban/surface ODD where SG4/SG5/SG6 actually occur.
- 55 mph is the bare "just under 60" point at ~13% margin — erodes to nothing on
  the first rainy night. Not recommended.

(Note: the safe cap is **80 km/h, not 80 mph** — a factor the original question
inverted.)

## 6. The cap is a function of demonstrated range — not permanent

Because the breaking point is `SSD(v) = R_reliable`, the cap rises as the inputs
improve, **with measured evidence**, never by guessing:

- **Lever 1 — detection range R.** Better long-range lidar, 4D imaging radar,
  sensor fusion. Pushing R from 130 m to 180 m moves the comfortable breaking
  point from ~60 to ~70 mph.
- **Lever 2 — reaction time t_react.** Tighter confident-detection + verdict
  chain. Each 0.1 s saved at 50 mph is ~2.2 m of SSD.

**Rule:** the ODD speed cap is set below the comfortable-decel breaking point
for the *measured* worst-case detection range in *worst-case* conditions (S8),
re-evaluated whenever the sensor suite or pipeline latency is revalidated. The
Governor enforces the current cap; raising it requires new S8 evidence, not a
config change.

## 7. Feedback to the safety goals

- Sets the look-ahead FTTI for SG4/SG5: at 50 mph the Governor must reject (and
  perception/map must flag the hazard) by ~94 m ahead (comfortable basis).
- Bounds the per-cycle WCET (S3): the 0.5 s reaction budget is the chain dead
  time; the Governor verdict is a slice of it (target ≪ 0.5 s so detection +
  actuation fit).
- Confirms the MRC stopping distance (the standing-MRC controlled stop) per
  speed (column "MRC stop").

Cross-refs: OCCY_SOTIF.md (#116), OCCY_SAFETY_GOALS.md (#113), S3 WCET (#115),
S8 validation (#120). Recorded in ADR-0001.
