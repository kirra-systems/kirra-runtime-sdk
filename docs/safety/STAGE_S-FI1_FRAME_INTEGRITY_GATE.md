# Stage S-FI1 — Unified Frame-Integrity Gate (kirra-core)

**Status:** PROPOSED — design-for-review. No code written. Awaiting owner sign-off before implementation, per de-monolith stage discipline.
**Date:** 2026-06-24
**Owner:** Kirra Systems, LLC
**Scope:** Promote localization-integrity from a parko-only self-report gate into a first-class, fail-closed **frame-integrity input** in `kirra-core`, wired into the SG2 containment check and the posture engine. Closes the gap behind **AOU-LOCALIZATION-001** by turning a static margin *assumption* into a runtime *check*.
**Future decision of record:** ADR-0016 (to be filed on acceptance).

---

## 1. Problem (grounded)

`validate_trajectory_containment` (`crates/kirra-core/src/containment.rs:194`) takes a `&[Pose]`, a `&Corridor`, and a `&VehicleFootprint`. It checks `pose_is_finite` (NaN/Inf only — `:234`) and asserts every footprint corner is inside the corridor polygon by at least `CONTAINMENT_LATERAL_MARGIN_M = 0.40 m` (`:66`). **It has no localization-integrity input.** The 0.40 m margin is a *static* discharge of a *runtime* property: it is sized assuming the integrator holds ≤ 0.10 m 95th-pct lateral error (`AOU-LOCALIZATION-001`).

This is the **one fault class the governor structurally cannot catch**, because localization is the governor's *own* coordinate-frame input: a pose error silently mislocates the corridor and *every* map-anchored veto, and the checker validates against the wrong world without any single check observing the fault. The safety case already states this (`ASSUMPTIONS_OF_USE.md:660`).

Integrity logic *does* exist — but only in parko (`parko/crates/parko-core/src/localization.rs`), as a **binary self-report gate** (`localization_trusted() -> bool`, `:79`) feeding only parko's discrete map-anchored scene gates (`gate_commit_zone_scene`, `gate_water_scene`). The main verifier never sees it.

## 2. Design principle

Apply Kirra's own thesis to localization: **you don't need great localization to be safe; you need to detect untrustworthy localization and fail closed — then grow the doer (EKF → SLAM) behind that gate.** Build the checker first; the localization *capability* becomes a doer bounded by an integrity checker, exactly like the planner.

Four design commitments (the deltas from a naive "lift parko's bool up"):

1. **Graduated, not binary.** kirra-core already encodes three regimes (0.40 m primary / 0.75 m fallback / fail-closed). The unified gate selects the margin from the *reported error value*; it does not flatten to a bool.
2. **Frame-integrity, not localization-only.** Calibration error and time-sync skew manifest *as* pose error in the fused frame (`AOU-TIMESYNC-001`). The input is named and shaped for the whole frame-defining class, with localization as the first (dominant) channel.
3. **Honest about self-report.** This *narrows* AOU-LOCALIZATION-001 to a *checked self-report*; it does not eliminate it. Independent KIRRA-computed integrity (map-matching residual / multi-source divergence) is the named follow-on, not this stage.
4. **Two consumers, two thresholds, one input.** Containment may run in the fallback band (0.75 m). The discrete map-anchored vetoes (commit-zone, water earn-back) may **not** — they require strict `Trusted`. The unified type makes that asymmetry explicit and intentional.

---

## 3. New types (`crates/kirra-core/src/frame_integrity.rs`, new module)

```rust
/// One channel of the ego coordinate-frame's integrity. Localization is the
/// first and dominant channel; calibration / time-sync are reserved siblings
/// (Stage S-FI2) so the enum shape never has to change to add them.
#[derive(Debug, Clone, Copy)]
pub struct LocalizationChannel {
    /// 95th-percentile lateral (cross-track) position error, metres.
    /// Non-finite → fail closed (an unverifiable pose is no pose).
    pub lateral_error_95_m: f64,
    /// Age (ms) of this snapshot vs now. Above cfg bound → stale → fail closed.
    pub age_ms: u64,
}

/// What the integrator's frame-integrity channel reports THIS tick.
/// Mirrors the established ABSENT-vs-KNOWN discipline (cf. `Corridor`,
/// parko `LocalizationIntegrity`): an ABSENT report is NOT a healthy frame.
#[derive(Debug, Clone, Copy)]
pub enum FrameIntegrity {
    /// No report this tick → NOT trusted (absent ≠ healthy; the #238 trap).
    Unknown,
    Reported {
        localization: LocalizationChannel,
        // RESERVED (Stage S-FI2): calibration: Option<CalibrationChannel>,
        //                         time_sync:   Option<TimeSyncChannel>,
    },
}

/// Graduated verdict. Maps 1:1 to a containment margin AND a posture class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTrust {
    /// ε ≤ primary bound, fresh, finite → PRIMARY margin (0.40 m), Nominal.
    Trusted,
    /// primary < ε ≤ fallback bound, fresh, finite → FALLBACK margin (0.75 m),
    /// posture Degraded. "Wrong but bounded": we widen the margin to absorb the
    /// larger error and keep moving conservatively.
    Degraded,
    /// absent / stale / non-finite / ε > fallback → containment refuses to
    /// validate; MRC controlled-stop. (Posture mapping: §6.)
    Untrusted,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameIntegrityCfg {
    /// ε ≤ this → Trusted. Default 0.10 m — the G2 AoU bound the 0.40 m primary
    /// containment margin is derived against (KIRRA-OCCY-SG2-MARGIN-001).
    pub primary_max_lateral_error_95_m: f64,
    /// primary < ε ≤ this → Degraded. Default 0.30 m — the urban-canyon
    /// worst-case the 0.75 m fallback margin is derived against
    /// (OCCY_SG2_MARGIN.md §2/§3).
    pub fallback_max_lateral_error_95_m: f64,
    /// Staleness bound (ms). Default 500 — VALIDATION-PENDING; tie to per-cycle
    /// FTTI on integration (matches parko `LocalizationCfg` default).
    pub max_age_ms: u64,
}

impl Default for FrameIntegrityCfg {
    fn default() -> Self {
        Self {
            primary_max_lateral_error_95_m: 0.10,
            fallback_max_lateral_error_95_m: 0.30,
            max_age_ms: 500,
        }
    }
}
```

### Resolver + margin selection (both O(1), no alloc — fits `wcet_gate`)

```rust
// SAFETY: SG2 | REQ: frame-integrity-gate | TEST: see §7
#[must_use]
pub fn resolve_frame_trust(integrity: &FrameIntegrity, cfg: &FrameIntegrityCfg) -> FrameTrust {
    let LocalizationChannel { lateral_error_95_m: e, age_ms } = match *integrity {
        FrameIntegrity::Unknown => return FrameTrust::Untrusted,
        FrameIntegrity::Reported { localization } => localization,
    };
    if !e.is_finite() || age_ms > cfg.max_age_ms {
        return FrameTrust::Untrusted;           // unverifiable / stale → fail closed
    }
    if e <= cfg.primary_max_lateral_error_95_m  { FrameTrust::Trusted }
    else if e <= cfg.fallback_max_lateral_error_95_m { FrameTrust::Degraded }
    else { FrameTrust::Untrusted }              // beyond fallback → fail closed
}

/// PRIMARY stays the existing constant (audit/derivation stability). FALLBACK
/// is the documented conservative margin, promoted from doc to code.
pub const CONTAINMENT_LATERAL_MARGIN_M: f64 = 0.40;          // unchanged
pub const CONTAINMENT_LATERAL_MARGIN_FALLBACK_M: f64 = 0.75; // new

/// `None` ⇒ do not validate; fail closed.
#[must_use]
pub fn containment_margin_m(trust: FrameTrust) -> Option<f64> {
    match trust {
        FrameTrust::Trusted   => Some(CONTAINMENT_LATERAL_MARGIN_M),
        FrameTrust::Degraded  => Some(CONTAINMENT_LATERAL_MARGIN_FALLBACK_M),
        FrameTrust::Untrusted => None,
    }
}
```

---

## 4. Containment change (`crates/kirra-core/src/containment.rs`)

New `DenyCode` variant (`kinematics_contract.rs:221`; existing variants/strings untouched → audit-chain hash stability preserved):

```rust
/// Safety: SG-002 ≅ SG2. Frame/localization integrity is untrusted this tick
/// (absent / stale / non-finite / lateral error beyond the fallback bound), so
/// the map-anchored corridor cannot be trusted to be correctly placed relative
/// to the ego. Containment refuses to validate. Issued by
/// `validate_trajectory_containment` when `FrameTrust::Untrusted`.
FrameIntegrityUntrusted,        // reason(): "FRAME_INTEGRITY_UNTRUSTED"
```

Signature gains **one parameter** (`frame_trust`); the margin becomes a local instead of a const:

```rust
pub fn validate_trajectory_containment(
    trajectory: &[Pose],
    corridor: &Corridor,
    footprint: &VehicleFootprint,
    frame_trust: FrameTrust,        // NEW
) -> EnforceAction {
    let margin = match containment_margin_m(frame_trust) {
        Some(m) => m,
        None => return EnforceAction::DenyBreach(DenyCode::FrameIntegrityUntrusted),
    };
    // ... existing corridor.is_healthy() / horizon / footprint_is_finite gates UNCHANGED ...
    let margin_sq = margin * margin;            // was: CONTAINMENT_LATERAL_MARGIN_M²
    // ... existing per-pose corner loop UNCHANGED ...
}
```

Note the geometry direction: a *larger* margin is *stricter* (more inward), so `Degraded` (0.75 m) **rejects more** than `Trusted` (0.40 m). Graduation tightens safety under worse localization — it never loosens it.

---

## 5. parko re-point (`parko/crates/parko-core/src/localization.rs`)

parko's binary scene gates (`gate_commit_zone_scene`, `gate_water_scene`) keep their `bool` signature. The canonical type/cfg move to kirra-core; parko consumes a **strict** bool view:

```rust
pub use kirra_core::frame_integrity::{FrameIntegrity, FrameIntegrityCfg, FrameTrust, resolve_frame_trust};

/// Map-anchored discrete vetoes require STRICT trust — `Degraded` (the 0.75 m
/// band) is good enough for *continuous* containment but NOT for trusting a
/// mapped commit-zone / ford LOCATION. Hence: Trusted only.
pub fn localization_trusted(integrity: &FrameIntegrity, cfg: &FrameIntegrityCfg) -> bool {
    matches!(resolve_frame_trust(integrity, cfg), FrameTrust::Trusted)
}
```

parko's existing `test_loc_*` tests become adapter tests over the unified resolver (same boundary cases; `Degraded` band now asserts `localization_trusted == false`, which is the existing 0.10 m semantics preserved). `gate_commit_zone_scene` / `gate_water_scene` and their tests are **unchanged**.

---

## 6. Posture wiring (`src/posture_engine_v2.rs`)

New reason + trigger (existing variants untouched):

```rust
// LockoutReason (:21)
FrameIntegrityUntrusted,    // "FRAME_INTEGRITY_UNTRUSTED"

// PostureRecalcTrigger (:165)
FrameIntegrityChanged { trust: FrameTrust },
```

**Posture mapping — the one decision flagged for review:**

| `FrameTrust` | Containment | Posture | Recovery |
|---|---|---|---|
| `Trusted` | 0.40 m | Nominal | — |
| `Degraded` | 0.75 m | **Degraded** (decel-to-stop-HOLD MRC) | automatic on return to `Trusted` |
| `Untrusted` (transient) | refuse → MRC stop | **Degraded** (MRC) | automatic on reacquire |
| `Untrusted` (sustained / flapping) | refuse → MRC stop | **LockedOut** | human reset |

Rationale: localization loss (GNSS dropout, NDT divergence) is a classic *transient* — forcing a human reset on every dropout is operationally untenable and not safety-required (the vehicle is already safely stopped). So transient `Untrusted` → Degraded-MRC (auto-recover); **sustained/flapping** `Untrusted` escalates to LockedOut by **reusing the existing AV recovery-hysteresis machinery** (`recovery_hysteresis.rs`: streak/window, `AV_RECOVERY_*`). This keeps the decel-to-stop-and-HOLD semantics self-consistent: Degraded posture + containment-refuses = exactly the decel-to-stop MRC the governor already authors, and the governor never authors re-acceleration.

**Open question for you:** confirm transient `Untrusted` → Degraded (not straight to LockedOut). I believe Degraded-with-escalation is correct and matches SS-002, but it's the load-bearing choice in this stage.

---

## 7. Call sites — the four enforcement points

Each must source a `FrameIntegrity` for the tick and pass `resolve_frame_trust(...)` into containment. **Sourcing the integrity report at each point is where the integrator contract surfaces** and is the bulk of the integration work:

1. **gateway** `enforce_actuator_safety_envelope` — frame-integrity from the perception input contract alongside the corridor.
2. **fabric** `AssetGovernor::evaluate_command` — per-asset integrity in the command envelope.
3. **ros2-adapter** `validate_trajectory_slow` — integrity from the ROS2 localization-quality topic.
4. **parko-kirra** `KirraGovernor::apply_mrc_profile` — already has `LocalizationIntegrity`; re-point to the unified type.

A temporary `validate_trajectory_containment_assuming_trusted(...)` shim (delegates with `FrameTrust::Trusted`, `#[doc(hidden)]`, deny-listed in CI) may be used to keep tests/benches compiling during the call-site migration, removed at stage close. No production path keeps it.

---

## 8. Test matrix

**`resolve_frame_trust`:** Unknown→Untrusted; non-finite ε→Untrusted; `age > max_age`→Untrusted; ε=0.10 (boundary)→Trusted; ε=0.10+δ→Degraded; ε=0.30 (boundary)→Degraded; ε=0.30+δ→Untrusted.
**`containment_margin_m`:** Trusted→0.40; Degraded→0.75; Untrusted→None.
**`validate_trajectory_containment`:** Untrusted→`DenyBreach(FrameIntegrityUntrusted)`; near-edge pose that **passes at 0.40 but fails at 0.75** (proves graduation tightens); Trusted pose centred→Allow; existing containment tests get `FrameTrust::Trusted` and must stay green (no regression).
**parko asymmetry (key):** ε in (0.10, 0.30] → containment Allows (Degraded/0.75) **but** `gate_commit_zone_scene → Unknown` (strict view false). This single test captures the two-thresholds-one-input design.
**Posture:** Degraded trust → Degraded posture; transient Untrusted → Degraded; sustained Untrusted via hysteresis → LockedOut.

## 9. Safety-case updates

- **AOU-LOCALIZATION-001** (`ASSUMPTIONS_OF_USE.md:650`): status → *narrowed* — "pose is correct" becomes "the frame-integrity self-report is honest and correctly characterized"; graduated primary/fallback regimes are now runtime-selected, not statically assumed; sustained-untrusted → LockedOut. Remains **AoU-GAP** (self-report).
- **New AOU-FRAME-INTEGRITY-SELFREPORT-001** (OPEN): the integrity channel is integrator self-reported; independent KIRRA-computed integrity (map-matching residual, multi-source divergence, RAIM-style cross-check) is the de-risking endgame — **Stage S-FI3**, not this stage.
- **AOU-TIMESYNC-001**: cross-reference — the reserved `time_sync`/`calibration` channels (Stage S-FI2) are the runtime home for its frame-affecting component.
- New `wcet_gate` rows for `resolve_frame_trust` / `containment_margin_m` (O(1), branch-only, no alloc).

## 10. What this stage explicitly does NOT do

- No EKF / GNSS-IMU fusion / SLAM (the *doer* — built behind this gate later).
- No independent integrity computation (Stage S-FI3).
- No calibration/time-sync channels (reserved shape only; Stage S-FI2).
- No change to the Nominal WCET-critical `validate_vehicle_command` path.

## 11. Sequencing

S-FI1a: `frame_integrity` module + types + resolver + tests (kirra-core, no call-site change; shim in place).
S-FI1b: containment signature + `DenyCode` + tests.
S-FI1c: parko re-point.
S-FI1d: posture wiring + hysteresis escalation.
S-FI1e: four call sites + integrator-contract surfacing; remove shim.
S-FI1f: safety-case/AoU updates; ADR-0016 to Accepted.
