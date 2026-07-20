//! **Occlusion / assured-clear-distance channel orchestration (S2, #1025) — pure, no ROS.**
//!
//! The RSS Rule 4 limited-visibility bound ([`crate::validation`]'s
//! `outruns_assured_clear_distance`) is sound and already plumbed into
//! `validate_trajectory_slow_*` via the `visibility_range_m` parameter — but the
//! live ROS 2 node fed it a hardcoded `None` ("perception does not yet supply a
//! visibility range"), so the gate was **dormant every tick**. The only occlusion
//! protection deployed was the untrusted planner's own `OccludedApproach` cap —
//! i.e. the DOER guarding a hazard the CHECKER is meant to bound (finding S2).
//!
//! This module is the **one safety-critical decision that stands between a
//! perception-supplied sight distance and the checker** — the exact sibling of
//! [`crate::vru_channel`].
//!
//! ## Why an explicit resolver (the overloaded-`None` hazard)
//!
//! The checker's `visibility_range_m` argument is `Option<f64>` where `None` means
//! "**no occlusion channel → skip the gate (no-op)**" (byte-identical). A
//! freshness snapshot ALSO returns `None` to mean "**silent / stale → fail
//! closed**". Those two `None`s mean OPPOSITE things: passing a silent snapshot
//! straight through would turn a lost sight-distance sensor into a silent no-op —
//! the ego driving at full ODD speed into unobserved space, the exact failure this
//! channel exists to prevent.
//!
//! So the node makes a THREE-way decision, pure and testable here:
//!
//! 1. **DISARMED** (`KIRRA_OCCLUSION_CHANNEL_ENABLED` unset/false) → feed the
//!    checker `None` (skip the occlusion gate), no cap. Byte-identical to the
//!    pre-wiring behaviour.
//! 2. **LIVE** (armed, snapshot fresh AND a finite, non-negative range) → feed the
//!    checker `Some(range)`. A trajectory that outruns what the ego can see now
//!    breaches → MRC.
//! 3. **FAIL-CLOSED** (armed, snapshot `None` = silent/stale, OR a non-finite /
//!    negative range = garbage) → do NOT feed the occlusion gate an admitting
//!    `None`; instead compose an MRC-floor cap (`Some(0.0)`) into the SAME Track-C
//!    derate ([`crate::perception_redundancy::more_restrictive_cap`] →
//!    `apply_perception_cap`), bringing the ego to a controlled stop.
//!
//! **AOU-OCCLUSION-RATE-001 (assumption of use).** An armed channel MUST publish
//! the assured-clear distance at a bounded rate — including when visibility is
//! wide open (a large range), not silence. Silence is indistinguishable from a
//! dead sensor and is treated as a fault (state 3, MRC stop), exactly as the
//! object / secondary / VRU channels already require.
//!
//! The fail-closed enforcement deliberately rides the perception cap (option a,
//! mirroring `vru_channel`) rather than feeding `visibility_range_m = Some(0.0)`
//! into the occlusion gate: the cap path is posture-composed and does not depend
//! on the occlusion gate's `max_decel_mps2` brake parameter (finding S4 / #1038),
//! so a lost sensor always stops via the Track-C derate.

/// Environment gate for the occlusion / assured-clear-distance channel (mirrors
/// [`crate::vru_channel::VRU_CHANNEL_ENABLED_ENV`]). Truthy = `1`/`true`/`yes`
/// (case-insensitive); unset or anything else = disarmed (byte-identical no-op).
/// The `std::env` READ lives in the adapter node (integration glue), not here —
/// this pure checker module keeps only the tested [`resolve_occlusion_channel`]
/// decision, so the mutation gate covers only safety logic, not env I/O.
pub const OCCLUSION_CHANNEL_ENABLED_ENV: &str = "KIRRA_OCCLUSION_CHANNEL_ENABLED";

/// The per-tick decision for the occlusion channel — the three-way distinction the
/// checker's `Option<f64>` cannot express on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OcclusionResolution {
    /// Channel disarmed → feed the checker `None` (skip the gate), no cap.
    Disarmed,
    /// Armed + fresh + valid range (metres) → feed this to the occlusion gate.
    Live(f64),
    /// Armed + silent/stale/garbage → feed the checker `None` **and** an MRC-floor cap.
    FailClosedStale,
}

impl OcclusionResolution {
    /// The assured-clear distance to hand the checker's `visibility_range_m`:
    /// `Some` only in [`Live`], so a [`FailClosedStale`] can NEVER reach the
    /// occlusion gate as an admitting no-op — its enforcement rides the
    /// [`perception_cap`](Self::perception_cap) instead.
    ///
    /// [`Live`]: OcclusionResolution::Live
    /// [`FailClosedStale`]: OcclusionResolution::FailClosedStale
    #[must_use]
    pub fn visibility_range(&self) -> Option<f64> {
        match self {
            OcclusionResolution::Live(range) => Some(*range),
            OcclusionResolution::Disarmed | OcclusionResolution::FailClosedStale => None,
        }
    }

    /// The perception cap this resolution contributes, to compose via
    /// [`crate::perception_redundancy::more_restrictive_cap`] into the Track-C
    /// derate: an MRC-floor `Some(0.0)` on a silent/stale/garbage channel, else
    /// `None`.
    #[must_use]
    pub fn perception_cap(&self) -> Option<f64> {
        match self {
            OcclusionResolution::FailClosedStale => Some(0.0),
            OcclusionResolution::Disarmed | OcclusionResolution::Live(_) => None,
        }
    }
}

/// Resolve the occlusion channel for one slow-loop tick from the enable gate and
/// the already-fail-closed `snapshot` the state layer produced.
///
/// * `enabled` — the adapter's occlusion enable gate
///   (`KIRRA_OCCLUSION_CHANNEL_ENABLED`, read in the node). The caller MUST only
///   evaluate `snapshot` when `enabled` (short-circuit), so a disarmed deployment
///   never touches the visibility lock or logs — the byte-identical no-op.
/// * `snapshot` — the assured-clear distance in metres: `Some(range)` when the
///   channel is fresh, `None` when silent / stale (already failed closed by the
///   freshness layer).
///
/// The rules that make this safe:
/// * an `enabled` channel whose snapshot is `None` becomes [`FailClosedStale`]
///   (→ MRC cap), NEVER a bare `None` handed to the checker (a silent no-op);
/// * an `enabled` channel whose range is non-finite or negative (a garbage
///   reading) ALSO becomes [`FailClosedStale`] — the occlusion gate must never be
///   armed with a value that could compute a nonsensical assured-clear speed.
///   Only a `DISARMED` channel yields the no-op `None`.
///
/// [`FailClosedStale`]: OcclusionResolution::FailClosedStale
#[must_use]
pub fn resolve_occlusion_channel(enabled: bool, snapshot: Option<f64>) -> OcclusionResolution {
    if !enabled {
        return OcclusionResolution::Disarmed; // state 1 — the checker's byte-identical no-op
    }
    match snapshot {
        // state 2 — fresh AND a sane range (finite, non-negative metres)
        Some(range) if range.is_finite() && range >= 0.0 => OcclusionResolution::Live(range),
        // state 3 — armed but lost (None) OR garbage (NaN/Inf/negative) → MRC cap
        _ => OcclusionResolution::FailClosedStale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DISARMED (enabled=false) is the checker's no-op regardless of the snapshot
    /// — `visibility_range() == None`, `perception_cap() == None`. Byte-identical.
    #[test]
    fn disarmed_is_the_checker_noop() {
        let r = resolve_occlusion_channel(false, Some(25.0));
        assert_eq!(r, OcclusionResolution::Disarmed);
        assert_eq!(r.visibility_range(), None);
        assert_eq!(r.perception_cap(), None);
        // …and with no snapshot.
        assert_eq!(
            resolve_occlusion_channel(false, None),
            OcclusionResolution::Disarmed
        );
    }

    /// Armed + fresh + valid range → LIVE: the range reaches the occlusion gate,
    /// no extra cap.
    #[test]
    fn armed_fresh_valid_range_is_live() {
        let r = resolve_occlusion_channel(true, Some(18.5));
        assert_eq!(r, OcclusionResolution::Live(18.5));
        assert_eq!(r.visibility_range(), Some(18.5));
        assert_eq!(r.perception_cap(), None);
    }

    /// A fresh range of EXACTLY zero (the ego can see nothing ahead) is a valid,
    /// legitimate reading — not garbage. It reaches the gate as `Some(0.0)`, which
    /// refuses any forward speed. This is distinct from a silent channel (which
    /// also stops, but via the cap), and pins the `>= 0.0` boundary as inclusive.
    #[test]
    fn armed_zero_range_is_live_not_garbage() {
        let r = resolve_occlusion_channel(true, Some(0.0));
        assert_eq!(r, OcclusionResolution::Live(0.0));
        assert_eq!(r.visibility_range(), Some(0.0));
        assert_eq!(r.perception_cap(), None);
    }

    /// THE safety-critical case: armed + `None` snapshot (silent/stale, already
    /// failed closed by the freshness layer) → FailClosedStale. The range handed
    /// to the checker is `None` (NOT an admitting no-op) and the MRC-floor cap
    /// carries the enforcement — the ego stops rather than driving blind.
    #[test]
    fn armed_silent_snapshot_fails_closed_not_noop() {
        let r = resolve_occlusion_channel(true, None);
        assert_eq!(r, OcclusionResolution::FailClosedStale);
        assert_eq!(r.visibility_range(), None); // must NOT reach the gate as a no-op
        assert_eq!(r.perception_cap(), Some(0.0)); // …enforcement is the MRC-floor cap
    }

    /// Armed + a GARBAGE range (NaN / +Inf / negative) is a fault, NOT a live
    /// bound: feeding it to the occlusion gate could compute a nonsensical
    /// assured-clear speed. Each fails closed to the MRC-floor cap.
    #[test]
    fn armed_garbage_range_fails_closed() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.001] {
            let r = resolve_occlusion_channel(true, Some(bad));
            assert_eq!(
                r,
                OcclusionResolution::FailClosedStale,
                "range {bad} must fail closed"
            );
            assert_eq!(r.visibility_range(), None);
            assert_eq!(r.perception_cap(), Some(0.0));
        }
    }

    /// The disarmed `None` and the fail-closed `None` differ ONLY by the gate, and
    /// they produce OPPOSITE caps — the whole point of the resolver.
    #[test]
    fn the_two_nones_are_disambiguated_by_the_gate() {
        let disarmed = resolve_occlusion_channel(false, None);
        let armed_silent = resolve_occlusion_channel(true, None);
        assert_eq!(disarmed.visibility_range(), armed_silent.visibility_range()); // both None
        assert_eq!(disarmed.perception_cap(), None); // …but disarmed relaxes
        assert_eq!(armed_silent.perception_cap(), Some(0.0)); // …and armed-silent stops
    }
}
