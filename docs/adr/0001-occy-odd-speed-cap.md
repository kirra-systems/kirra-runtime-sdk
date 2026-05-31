# ADR-0001: Occy ODD speed cap = 50 mph / 80 km/h

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-05-31 |
| Deciders | Project owner |
| Issues | S1 (#113), S4 (#116), S8 (#120), S3 (#115), #99 |
| Doc | docs/safety/SPEED_ENVELOPE.md (KIRRA-OCCY-SPEED-001) |

## Context

The Phase-1 Occy deployment ODD is full-driverless (no human fallback →
controllability C3 → core safety goals ASIL D), urban / surface streets, day +
night, all-weather, with rail crossings and water both **in scope** (not
deferred). The ODD requires an explicit speed cap because:

- Cap sets the required look-ahead (SSD) for SG4 / SG5 reject distances.
- Cap minus actuation latency sets the Governor WCET budget (S3 / #115) via the
  FTTI chain.
- Cap interacts with detection-range degradation in degraded conditions
  (rain / fog / low-light / sensor degradation).

An earlier candidate cap (50 km/h, 13.9 m/s) reflected a constrained-pilot path
that was rejected in favor of a full-feature deployment. A real cap is needed.

## Decision

**The Occy Phase-1 deployment-ODD speed cap is 50 mph (80 km/h, 22.35 m/s),
clear-conditions maximum, with dynamic derating in degraded conditions.**

Three rules govern the cap:

1. **Cap-as-function-of-detection-range.** cap = f(R), where R is the
   S8-measured worst-case detection range. 50 mph presumes worst-case (wet /
   night) R supports ≥ 94 m look-ahead. If S8 finds R below that, the cap
   derates per SSD(v) = R (see SPEED_ENVELOPE.md §5–6).
2. **Dynamic derate in degraded conditions.** In rain / fog / low-light /
   sensor-degraded operation, the cap derates from the clear-conditions maximum
   via the condition-dependent speed governor (#99 weather-posture coupling).
3. **Cap-raise gate.** Raising the cap beyond 50 mph requires new S8 evidence
   (#120) of detection range supporting the higher cap's required look-ahead.
   Not a config change.

## Consequences

**Positive:**

- Active SG4 (water, ASIL B) and SG5 (commit-zone, ASIL B) — no QM deferral.
  Real-world deployment risk profile, not a pilot toy.
- ~28% margin at the cap on dry / clear R_reliable ≈ 130 m vs required 94 m
  look-ahead — comfortable headroom for S8 uncertainty.
- Margin cap (not breaking-point cap): the ~60 mph comfortable-basis breaking
  point sits above 50 mph, so the cap absorbs reasonable S8 surprise without
  re-decision. See SPEED_ENVELOPE.md §4–5 for the breaking-point analysis.
- Cap-as-function-of-R rule means the derate logic, not a hard limit, handles
  degraded conditions — supports the all-weather scope without unsafe
  assumptions.

**Negative / risk:**

- 50 mph presumes worst-case R supports 94 m look-ahead. If S8 (#120) shows
  degraded R falls short, the cap derates and the deployable speed shrinks —
  possibly impacting commercial viability of the ODD in some markets. Known
  dependency, not a risk to ignore.
- ASIL B carried for SG4 / SG5 from day one — development effort higher than a
  Phase-1-defers-water alternative.
- SG6 (post-collision) developed to elevated rigor despite ASIL-A table rating
  (owner-imposed hard constraint).

**Alternatives considered:**

- *Smaller pilot cap (e.g. 50 km/h):* rejected — defers real safety scope,
  demos a toy, doesn't validate the doer/checker architecture against the
  hazards it was built for.
- *Breaking-point cap (~60 mph comfortable basis):* rejected — no margin for S8
  uncertainty, no headroom for the dynamic derate to engage usefully before
  hitting the wall.
- *Cap > 60 mph:* rejected without new S8 evidence (breaking-point analysis in
  SPEED_ENVELOPE.md §6 — requires R > 130 m or a faster pipeline).

## Links

- `docs/safety/SPEED_ENVELOPE.md` (KIRRA-OCCY-SPEED-001) — derivation,
  breaking-point analysis, derate table.
- `docs/safety/OCCY_SOTIF.md` (KIRRA-OCCY-ODD-001, #116) — §1.1 deployment ODD;
  §5 FTTI feedback.
- `docs/safety/OCCY_SAFETY_GOALS.md` (KIRRA-OCCY-SG-001, #113) — FTTI absolutes
  per goal.
- Issue #115 — S3 Governor WCET proof (carries the per-cycle bound).
- Issue #120 — S8 V&V / detection-range validation (validates the cap's R
  presumption).
- Issue #99 — Weather / posture coupling (implements the derate).
