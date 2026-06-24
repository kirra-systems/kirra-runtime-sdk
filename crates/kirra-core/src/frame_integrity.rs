//! **kirra-core frame-integrity gate** (Stage S-FI1a) — the fail-closed runtime
//! check behind `AOU-LOCALIZATION-001`.
//!
//! # Why this module exists
//!
//! `containment::validate_trajectory_containment` (SG2) reasons about the ego
//! footprint inside a map-anchored corridor. Every such map-anchored judgement
//! is interpreted *through the ego pose* — so a wrong pose silently mislocates
//! the corridor (and every commit-zone / water veto) and the checker validates
//! against the wrong world **without any single check observing the fault**
//! (`ASSUMPTIONS_OF_USE.md` AOU-LOCALIZATION-001). Localization is the one input
//! the governor structurally cannot de-risk, because it is the governor's *own*
//! coordinate-frame input.
//!
//! This module turns that static assumption into a runtime check: it consumes
//! the integrator's per-tick frame-integrity report and resolves a **graduated**
//! verdict that selects the containment margin and drives the posture engine.
//!
//! # Design commitments (Stage S-FI1, see `docs/safety/STAGE_S-FI1_FRAME_INTEGRITY_GATE.md`)
//!
//! * **Graduated, not binary.** The verdict carries three regimes — Trusted
//!   (0.40 m primary margin), Degraded (0.75 m fallback margin), Untrusted
//!   (refuse to validate) — selected from the *reported error value*, not a bool.
//! * **Frame-integrity, not localization-only.** [`FrameIntegrity::Reported`]
//!   carries a [`LocalizationChannel`] today; calibration / time-sync are
//!   RESERVED sibling channels (Stage S-FI2) so the enum shape never changes to
//!   add them.
//! * **Fail-closed-immediately.** `Unknown` (absent ≠ healthy), non-finite, or
//!   stale → `Untrusted` on the FIRST tick. Hysteresis (Stage S-FI1d) governs
//!   only the Degraded→LockedOut escalation, never a grace period on the initial
//!   fail-closed response.
//! * **Self-report, honestly.** This narrows AOU-LOCALIZATION-001 to a *checked
//!   self-report*; independent KIRRA-computed integrity is the named follow-on
//!   (Stage S-FI3), not this stage.
//!
//! # WCET
//!
//! [`resolve_frame_trust`] and [`containment_margin_m`] are O(1): scalar `f64`
//! comparisons and `match`, no heap, no recursion. They ride the `wcet_gate`
//! structural-boundedness argument unchanged.
//!
//! # Scope of S-FI1a
//!
//! This sub-stage is the PURE module only — types + resolver + margin selection
//! + tests. It does NOT touch `containment.rs`, the posture engine, parko, or any
//! call site; those are S-FI1b–e. The new `DenyCode::FrameIntegrityUntrusted`
//! and the containment parameter are introduced in S-FI1b.

/// One channel of the ego coordinate-frame's integrity. Localization is the
/// first and dominant channel; calibration / time-sync are RESERVED siblings
/// (Stage S-FI2) — they manifest *as* pose error in the fused frame
/// (`AOU-TIMESYNC-001`), so they belong to the same frame-defining class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalizationChannel {
    /// 95th-percentile lateral (cross-track) position error, metres.
    /// Non-finite → fail closed (an unverifiable pose is no pose).
    pub lateral_error_95_m: f64,
    /// Age (ms) of this snapshot vs now. Above the cfg bound → stale → fail
    /// closed (a stale pose is unobserved travel; see the staleness × speed
    /// coupling in the stage spec §3).
    pub age_ms: u64,
}

/// What the integrator's frame-integrity channel reports THIS tick.
///
/// Mirrors the established ABSENT-vs-KNOWN discipline (cf. [`crate::containment::Corridor`],
/// parko `LocalizationIntegrity`): an ABSENT report is NOT a healthy frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameIntegrity {
    /// No report this tick → NOT trusted (absent ≠ healthy; the #238 trap — a
    /// missing pose-quality signal is not "the pose is fine").
    Unknown,
    /// The integrator reported a frame-quality estimate.
    Reported {
        /// The localization channel (dominant, present today).
        localization: LocalizationChannel,
        // RESERVED (Stage S-FI2):
        //   calibration: Option<CalibrationChannel>,
        //   time_sync:   Option<TimeSyncChannel>,
    },
}

/// Graduated frame-trust verdict. Maps 1:1 to a containment margin
/// ([`containment_margin_m`]) AND a posture class (Stage S-FI1d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTrust {
    /// ε ≤ primary bound, fresh, finite → PRIMARY margin (0.40 m), posture
    /// Nominal.
    Trusted,
    /// primary < ε ≤ fallback bound, fresh, finite → FALLBACK margin (0.75 m),
    /// posture Degraded. "Wrong but bounded": widen the margin to absorb the
    /// larger error and keep moving conservatively.
    Degraded,
    /// absent / stale / non-finite / ε > fallback → containment refuses to
    /// validate; the MRC controlled-stop (decel along current heading) is the
    /// frame-trust-minimal maneuver. Posture: immediate Degraded, escalating to
    /// LockedOut if sustained / flapping (Stage S-FI1d).
    Untrusted,
}

/// Configuration for the frame-integrity gate.
///
/// The bounds are **documented, sourced, VALIDATION-PENDING** placeholders — NOT
/// manufactured FTTI numbers. `primary` couples to the 0.40 m SG2 margin
/// derivation (KIRRA-OCCY-SG2-MARGIN-001); `fallback` couples to the 0.75 m
/// conservative margin (urban-canyon worst case, `OCCY_SG2_MARGIN.md` §2/§3).
/// `max_age_ms` is a liveness placeholder — see the staleness × speed coupling
/// note in the stage spec: it is NOT a standalone safety bound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameIntegrityCfg {
    /// ε ≤ this → [`FrameTrust::Trusted`]. Default `0.10` m (G2 AoU bound).
    pub primary_max_lateral_error_95_m: f64,
    /// primary < ε ≤ this → [`FrameTrust::Degraded`]. Default `0.30` m
    /// (urban-canyon worst case the 0.75 m fallback margin is derived against).
    pub fallback_max_lateral_error_95_m: f64,
    /// Maximum acceptable staleness (ms). Default `500` — VALIDATION-PENDING;
    /// tie to the per-cycle localization-path FTTI on allocation. Matches the
    /// parko `LocalizationCfg` default.
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

/// The 0.75 m conservative-fallback lateral containment margin (metres),
/// promoted from `OCCY_SG2_MARGIN.md` §3 to code. Selected by
/// [`containment_margin_m`] under [`FrameTrust::Degraded`]. The PRIMARY 0.40 m
/// margin stays its existing constant ([`crate::containment::CONTAINMENT_LATERAL_MARGIN_M`])
/// for audit / derivation stability.
pub const CONTAINMENT_LATERAL_MARGIN_FALLBACK_M: f64 = 0.75;

/// Resolve the graduated frame-trust verdict for this tick. Fail-closed on
/// every uncertain input — `Unknown`, non-finite ε, stale, or ε beyond the
/// fallback bound all yield [`FrameTrust::Untrusted`].
///
/// O(1), scalar-only, no allocation (WCET-clean).
// SAFETY: SG2 | REQ: frame-integrity-gate | TEST: unknown_is_untrusted,nonfinite_is_untrusted,stale_is_untrusted,primary_boundary_is_trusted,just_over_primary_is_degraded,fallback_boundary_is_degraded,just_over_fallback_is_untrusted
#[must_use]
pub fn resolve_frame_trust(integrity: &FrameIntegrity, cfg: &FrameIntegrityCfg) -> FrameTrust {
    let LocalizationChannel {
        lateral_error_95_m: e,
        age_ms,
    } = match *integrity {
        FrameIntegrity::Unknown => return FrameTrust::Untrusted,
        FrameIntegrity::Reported { localization } => localization,
    };

    // Unverifiable or stale → fail closed. (A non-finite error is no pose; a
    // stale pose is unobserved travel.)
    if !e.is_finite() || age_ms > cfg.max_age_ms {
        return FrameTrust::Untrusted;
    }

    if e <= cfg.primary_max_lateral_error_95_m {
        FrameTrust::Trusted
    } else if e <= cfg.fallback_max_lateral_error_95_m {
        FrameTrust::Degraded
    } else {
        // Beyond the conservative-fallback bound → fail closed.
        FrameTrust::Untrusted
    }
}

/// The lateral containment margin (metres) for a frame-trust verdict, or `None`
/// when the frame is [`FrameTrust::Untrusted`] and containment must refuse to
/// validate.
///
/// Note the geometry direction: a *larger* margin is *stricter* (more inward),
/// so `Degraded` (0.75 m) rejects MORE than `Trusted` (0.40 m). Graduation
/// tightens safety under worse localization — it never loosens it.
///
/// O(1), no allocation (WCET-clean).
// SAFETY: SG2 | REQ: frame-integrity-gate | TEST: margin_trusted_is_primary,margin_degraded_is_fallback,margin_untrusted_is_none,degraded_margin_is_stricter_than_trusted
#[must_use]
pub fn containment_margin_m(trust: FrameTrust) -> Option<f64> {
    match trust {
        FrameTrust::Trusted => Some(crate::containment::CONTAINMENT_LATERAL_MARGIN_M),
        FrameTrust::Degraded => Some(CONTAINMENT_LATERAL_MARGIN_FALLBACK_M),
        FrameTrust::Untrusted => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(lateral_error_95_m: f64, age_ms: u64) -> FrameIntegrity {
        FrameIntegrity::Reported {
            localization: LocalizationChannel {
                lateral_error_95_m,
                age_ms,
            },
        }
    }

    // --- resolve_frame_trust -------------------------------------------------

    #[test]
    fn unknown_is_untrusted() {
        // Absent report ≠ healthy pose.
        assert_eq!(
            resolve_frame_trust(&FrameIntegrity::Unknown, &FrameIntegrityCfg::default()),
            FrameTrust::Untrusted
        );
    }

    #[test]
    fn nonfinite_is_untrusted() {
        let cfg = FrameIntegrityCfg::default();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                resolve_frame_trust(&reported(bad, 0), &cfg),
                FrameTrust::Untrusted,
                "non-finite error must fail closed ({bad})"
            );
        }
    }

    #[test]
    fn stale_is_untrusted() {
        let cfg = FrameIntegrityCfg::default();
        // Even a perfect ε is untrusted if the snapshot is stale.
        assert_eq!(
            resolve_frame_trust(&reported(0.0, cfg.max_age_ms + 1), &cfg),
            FrameTrust::Untrusted
        );
    }

    #[test]
    fn primary_boundary_is_trusted() {
        let cfg = FrameIntegrityCfg::default();
        // ε exactly at the primary bound is Trusted (≤, inclusive).
        assert_eq!(
            resolve_frame_trust(&reported(cfg.primary_max_lateral_error_95_m, 0), &cfg),
            FrameTrust::Trusted
        );
    }

    #[test]
    fn just_over_primary_is_degraded() {
        let cfg = FrameIntegrityCfg::default();
        assert_eq!(
            resolve_frame_trust(&reported(cfg.primary_max_lateral_error_95_m + 1e-9, 0), &cfg),
            FrameTrust::Degraded
        );
    }

    #[test]
    fn fallback_boundary_is_degraded() {
        let cfg = FrameIntegrityCfg::default();
        // ε exactly at the fallback bound is still Degraded (≤, inclusive).
        assert_eq!(
            resolve_frame_trust(&reported(cfg.fallback_max_lateral_error_95_m, 0), &cfg),
            FrameTrust::Degraded
        );
    }

    #[test]
    fn just_over_fallback_is_untrusted() {
        let cfg = FrameIntegrityCfg::default();
        assert_eq!(
            resolve_frame_trust(&reported(cfg.fallback_max_lateral_error_95_m + 1e-9, 0), &cfg),
            FrameTrust::Untrusted
        );
    }

    #[test]
    fn fresh_at_max_age_is_not_stale() {
        let cfg = FrameIntegrityCfg::default();
        // age_ms == max_age_ms is fresh (the gate is `>`, not `>=`).
        assert_eq!(
            resolve_frame_trust(&reported(0.05, cfg.max_age_ms), &cfg),
            FrameTrust::Trusted
        );
    }

    // --- containment_margin_m ------------------------------------------------

    #[test]
    fn margin_trusted_is_primary() {
        assert_eq!(
            containment_margin_m(FrameTrust::Trusted),
            Some(crate::containment::CONTAINMENT_LATERAL_MARGIN_M)
        );
    }

    #[test]
    fn margin_degraded_is_fallback() {
        assert_eq!(
            containment_margin_m(FrameTrust::Degraded),
            Some(CONTAINMENT_LATERAL_MARGIN_FALLBACK_M)
        );
    }

    #[test]
    fn margin_untrusted_is_none() {
        // Untrusted ⇒ containment must refuse to validate.
        assert_eq!(containment_margin_m(FrameTrust::Untrusted), None);
    }

    #[test]
    fn degraded_margin_is_stricter_than_trusted() {
        // A larger margin is stricter (more inward). Graduation must TIGHTEN
        // under worse localization, never loosen.
        let trusted = containment_margin_m(FrameTrust::Trusted).unwrap();
        let degraded = containment_margin_m(FrameTrust::Degraded).unwrap();
        assert!(
            degraded > trusted,
            "fallback margin ({degraded}) must exceed primary ({trusted})"
        );
    }

    #[test]
    fn fallback_bound_above_primary_bound() {
        // Config sanity: the bands must be ordered, else Degraded is unreachable.
        let cfg = FrameIntegrityCfg::default();
        assert!(cfg.fallback_max_lateral_error_95_m > cfg.primary_max_lateral_error_95_m);
    }
}
