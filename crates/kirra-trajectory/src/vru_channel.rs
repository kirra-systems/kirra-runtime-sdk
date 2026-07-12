//! **VRU perception-channel orchestration (WS-2, #789 follow-up 1) — pure, no ROS.**
//!
//! The pedestrian bound ([`crate::vru::pedestrian_breach`]) is sound and already
//! plumbed into `validate_trajectory_slow_*`, but the live ROS 2 node fed it
//! `None` — "no VRU perception channel is wired yet." The freshness/fail-closed
//! *storage* is already built (WP-10: the adapter's `AdaptorState::update_pedestrians`
//! stamps arrival and `snapshot_pedestrians` returns `Some(peds)` when fresh,
//! `None` when silent/stale/poisoned). What was missing — and what this module
//! is — is the **one safety-critical decision that stands between that snapshot
//! and the checker.**
//!
//! ## Why an explicit resolver (the overloaded-`None` hazard)
//!
//! `snapshot_pedestrians` returns `Option<Vec<PerceivedPedestrian>>` where `None`
//! means "**fail closed** (silent/stale)". But the checker's `pedestrians`
//! argument is ALSO `Option`, where `None` means "**no VRU channel → no-op**"
//! (byte-identical). Those two `None`s mean OPPOSITE things. Passing the snapshot
//! straight through would turn an armed-but-silent channel (a lost pedestrian
//! sensor) into a silent no-op — the ego driving blind to pedestrians, the exact
//! failure this channel exists to prevent.
//!
//! So the node must make a THREE-way decision, and that decision is pure and
//! testable here:
//!
//! 1. **DISARMED** (`KIRRA_VRU_CHANNEL_ENABLED` unset/false) → feed the checker
//!    `None` (no-op), no cap. Byte-identical to the pre-wiring behaviour.
//! 2. **LIVE** (armed, snapshot fresh — possibly an empty "road clear" list) →
//!    feed the checker `Some(scene)`. A pedestrian inside the omnidirectional
//!    stopping bound now breaches → MRC.
//! 3. **FAIL-CLOSED** (armed, snapshot `None` = silent/stale/poisoned) → DO NOT
//!    feed an admitting no-op; instead compose an MRC-floor cap (`Some(0.0)`)
//!    into the SAME Track-C derate ([`crate::perception_redundancy::more_restrictive_cap`]
//!    → `apply_perception_cap`), bringing the ego to a controlled stop.
//!
//! **AOU-VRU-RATE-001 (assumption of use).** An armed channel MUST publish at a
//! bounded rate — an EMPTY message when the road is clear, not silence. Silence
//! is indistinguishable from a dead sensor and is treated as a fault (state 3,
//! MRC stop). A detector that only publishes on a positive detection must be
//! adapted to emit a "clear" heartbeat, exactly as the object / secondary
//! channels already require.
//!
//! This keeps the heavy per-pose checker untouched; only the `Option<&scene>`
//! and one composed cap are decided here.

use crate::vru::PerceivedPedestrian;

/// Environment gate for the VRU perception channel (mirrors
/// `PERCEPTION_REDUNDANCY_ENABLED_ENV`). Truthy = `1`/`true`/`yes`
/// (case-insensitive); unset or anything else = disarmed (byte-identical no-op).
pub const VRU_CHANNEL_ENABLED_ENV: &str = "KIRRA_VRU_CHANNEL_ENABLED";

/// Read the VRU-channel enable gate from the environment. Disarmed unless
/// explicitly truthy — a typo never silently arms an enforcement path, and (the
/// conservative default) a disarmed channel is the byte-identical pre-wiring
/// state.
#[must_use]
pub fn vru_channel_enabled() -> bool {
    std::env::var(VRU_CHANNEL_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// The per-tick decision for the VRU channel — the three-way distinction the
/// checker's `Option` cannot express on its own.
#[derive(Debug, Clone, PartialEq)]
pub enum VruResolution {
    /// Channel disarmed → feed the checker `None` (no-op), no cap.
    Disarmed,
    /// Armed + fresh → feed this scene to the checker (may be empty = clear).
    Live(Vec<PerceivedPedestrian>),
    /// Armed + silent/stale → feed the checker `None` **and** an MRC-floor cap.
    FailClosedStale,
}

impl VruResolution {
    /// The pedestrian scene to hand the checker: `Some` only in [`Live`], so a
    /// `FailClosedStale` can NEVER reach the checker as an admitting no-op — its
    /// enforcement rides the [`perception_cap`](Self::perception_cap) instead.
    ///
    /// [`Live`]: VruResolution::Live
    #[must_use]
    pub fn scene(&self) -> Option<&[PerceivedPedestrian]> {
        match self {
            VruResolution::Live(peds) => Some(peds),
            VruResolution::Disarmed | VruResolution::FailClosedStale => None,
        }
    }

    /// The perception cap this resolution contributes, to compose via
    /// [`crate::perception_redundancy::more_restrictive_cap`] into the Track-C
    /// derate: an MRC-floor `Some(0.0)` on a silent/stale channel, else `None`.
    #[must_use]
    pub fn perception_cap(&self) -> Option<f64> {
        match self {
            VruResolution::FailClosedStale => Some(0.0),
            VruResolution::Disarmed | VruResolution::Live(_) => None,
        }
    }
}

/// Resolve the VRU channel for one slow-loop tick from the enable gate and the
/// already-fail-closed [`snapshot`] the state layer produced.
///
/// * `enabled` — [`vru_channel_enabled`] (the DISARMED gate).
/// * `snapshot` — `AdaptorState::snapshot_pedestrians(now, budget)`: `Some(peds)`
///   when the channel is fresh (possibly empty), `None` when silent / stale /
///   poisoned (already failed closed by the freshness layer).
///
/// The single rule that makes this safe: an `enabled` channel whose snapshot is
/// `None` becomes [`FailClosedStale`] (→ MRC cap), NEVER a bare `None` handed to
/// the checker (which the checker would read as "no VRU channel", a no-op). Only
/// a `DISARMED` channel yields the no-op `None`.
///
/// [`snapshot`]: VruResolution
/// [`FailClosedStale`]: VruResolution::FailClosedStale
#[must_use]
pub fn resolve_vru_channel(
    enabled: bool,
    snapshot: Option<Vec<PerceivedPedestrian>>,
) -> VruResolution {
    if !enabled {
        return VruResolution::Disarmed; // state 1 — the checker's byte-identical no-op
    }
    match snapshot {
        Some(peds) => VruResolution::Live(peds), // state 2 — fresh (maybe empty)
        None => VruResolution::FailClosedStale,  // state 3 — armed but lost → MRC cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::corridor::Point;

    fn ped(id: u64, x: f64) -> PerceivedPedestrian {
        PerceivedPedestrian {
            id,
            pos: Point { x_m: x, y_m: 0.0 },
            vel: Point { x_m: 0.0, y_m: 0.0 },
            age_s: 0.0,
        }
    }

    /// DISARMED (enabled=false) is the checker's no-op regardless of the snapshot
    /// — `scene() == None`, `perception_cap() == None`. Byte-identical.
    #[test]
    fn disarmed_is_the_checker_noop() {
        // Even if a snapshot exists, a disarmed gate stays a no-op.
        let r = resolve_vru_channel(false, Some(vec![ped(1, 5.0)]));
        assert_eq!(r, VruResolution::Disarmed);
        assert_eq!(r.scene(), None);
        assert_eq!(r.perception_cap(), None);
        // And with no snapshot.
        assert_eq!(resolve_vru_channel(false, None), VruResolution::Disarmed);
    }

    /// Armed + fresh snapshot → LIVE: the scene reaches the checker, no extra cap.
    #[test]
    fn armed_fresh_snapshot_is_live_scene() {
        let r = resolve_vru_channel(true, Some(vec![ped(7, 5.0), ped(8, -3.0)]));
        assert_eq!(r.perception_cap(), None);
        let scene = r.scene().expect("Live → Some scene");
        assert_eq!(scene.len(), 2);
        assert_eq!(scene[0].id, 7);
    }

    /// Armed + a fresh but EMPTY snapshot is a legitimate "road clear" signal →
    /// LIVE([]) (scene reaches the checker as empty → no breach, no cap). This is
    /// the case that MUST NOT be confused with silence.
    #[test]
    fn armed_fresh_empty_snapshot_is_live_clear() {
        let r = resolve_vru_channel(true, Some(vec![]));
        assert_eq!(r, VruResolution::Live(vec![]));
        assert_eq!(r.scene(), Some(&[][..]));
        assert_eq!(r.perception_cap(), None);
    }

    /// THE safety-critical case: armed + `None` snapshot (silent/stale/poisoned,
    /// already failed closed by the freshness layer) → FailClosedStale. The scene
    /// handed to the checker is `None` (NOT an admitting no-op) and the MRC-floor
    /// cap carries the enforcement — the ego stops rather than driving blind.
    #[test]
    fn armed_silent_snapshot_fails_closed_not_noop() {
        let r = resolve_vru_channel(true, None);
        assert_eq!(r, VruResolution::FailClosedStale);
        // Must NOT reach the checker as a scene (that would be a silent no-op)…
        assert_eq!(r.scene(), None);
        // …the enforcement is the MRC-floor cap instead.
        assert_eq!(r.perception_cap(), Some(0.0));
    }

    /// The disarmed `None` and the fail-closed `None` differ ONLY by the gate,
    /// and they produce OPPOSITE caps — the whole point of the resolver.
    #[test]
    fn the_two_nones_are_disambiguated_by_the_gate() {
        let disarmed = resolve_vru_channel(false, None);
        let armed_silent = resolve_vru_channel(true, None);
        assert_eq!(disarmed.scene(), armed_silent.scene()); // both None to the checker
        assert_eq!(disarmed.perception_cap(), None); // …but disarmed relaxes
        assert_eq!(armed_silent.perception_cap(), Some(0.0)); // …and armed-silent stops
    }
}
