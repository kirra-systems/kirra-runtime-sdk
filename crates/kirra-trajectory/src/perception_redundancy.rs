//! **Perception redundancy cross-check** — a fail-closed assurance monitor, the
//! True-Redundancy analog (gap #2b / P1).
//!
//! The *True Redundancy* pattern runs two INDEPENDENT world models (camera-only vs.
//! radar+lidar) as mutual backups. KIRRA promotes that idea from a perception design
//! into an **assurance check**: given two independent perception channels, verify they
//! AGREE — and **fail closed** when they don't. A divergence means at least one channel
//! is wrong and neither can be trusted: a *phantom* object (present in one channel,
//! absent in the other) or a *mismatched* object (matched by position but disagreeing
//! on speed) is exactly the single-channel fault redundancy exists to catch.
//!
//! The verdict composes with the existing Track-C perception derate: a divergence maps
//! to an MRC-floor speed cap (`to_perception_cap` → `Some(0.0)`), so KIRRA brings the
//! vehicle to a controlled stop via the *same* `apply_perception_cap` path — no change
//! to the WCET-critical checker.

use crate::state::PerceivedObject;
use kirra_core::FleetPosture;

/// Tolerances for declaring two channels' objects "the same".
#[derive(Debug, Clone, Copy)]
pub struct RedundancyConfig {
    /// Max position disagreement (m) for two objects to be considered a match.
    pub position_tol_m: f64,
    /// Max speed-magnitude disagreement (m/s) a matched pair may have.
    pub velocity_tol_mps: f64,
}

impl Default for RedundancyConfig {
    fn default() -> Self {
        // Conservative defaults: ~1 vehicle-width of position slack, a brisk-walk of
        // speed slack. Tighter than this risks flapping on sensor noise; looser risks
        // missing a real single-channel fault.
        Self {
            position_tol_m: 2.0,
            velocity_tol_mps: 1.5,
        }
    }
}

/// Which channel an unmatched object came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    A,
    B,
}

/// Why two perception channels diverged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DivergenceReason {
    /// An object in one channel has no counterpart within `position_tol_m` in the
    /// other — a phantom (false positive) or a miss (false negative). The dangerous case.
    Unmatched { id: u64, channel: Channel },
    /// A position-matched pair disagrees on speed beyond `velocity_tol_mps`.
    SpeedMismatch {
        id_a: u64,
        id_b: u64,
        delta_mps: f64,
    },
}

/// The cross-check verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RedundancyVerdict {
    /// Both channels agree within tolerance → perception is trusted.
    Consistent,
    /// The channels disagree → at least one is wrong. Fail closed.
    Diverged(DivergenceReason),
}

impl RedundancyVerdict {
    /// Compose into the Track-C perception derate: a divergence is an MRC-floor cap
    /// (`Some(0.0)` → controlled stop via `apply_perception_cap`); consistency adds no
    /// cap (`None`). This is how the monitor reaches the actuator without touching the
    /// per-pose checker.
    #[must_use]
    pub fn to_perception_cap(self) -> Option<f64> {
        match self {
            RedundancyVerdict::Consistent => None,
            RedundancyVerdict::Diverged(_) => Some(0.0),
        }
    }

    /// Whether perception diverged (and the system must fail closed).
    #[must_use]
    pub fn is_diverged(self) -> bool {
        matches!(self, RedundancyVerdict::Diverged(_))
    }
}

/// Env gate enabling the perception-divergence monitor — a SECOND perception channel is
/// configured and should be cross-checked. Truthy = `1`/`true`/`yes` (case-insensitive); unset
/// or anything else = disabled (no redundant channel → the monitor is a no-op).
pub const PERCEPTION_REDUNDANCY_ENABLED_ENV: &str = "KIRRA_PERCEPTION_REDUNDANCY_ENABLED";

/// Read the redundancy-monitor enable gate from the environment (mirrors
/// `perception_derate_enabled`). Disabled unless explicitly truthy.
#[must_use]
pub fn perception_redundancy_enabled() -> bool {
    std::env::var(PERCEPTION_REDUNDANCY_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Resolve the perception-divergence MRC cap for one slow-loop tick — the orchestration that
/// makes the [`cross_check`] monitor LIVE and composes its verdict into the same Track-C
/// perception derate (`apply_perception_cap`), so a divergence reaches the actuator as a
/// controlled stop without touching the WCET-critical per-pose checker. Four states, mirroring
/// `resolve_perception_cap`:
///
/// 1. **DISABLED** (no redundant channel configured) → `None`. Byte-identical prior behaviour.
/// 2. enabled, secondary **FRESH** + channels **consistent** → `None` (no cap).
/// 3. enabled, secondary **FRESH** + channels **diverged** → `Some(0.0)` (fail closed: at least
///    one channel is wrong and neither can be trusted).
/// 4. enabled, secondary **STALE / silent** → `Some(0.0)` (the redundant channel dropped out, so
///    the primary can no longer be cross-checked — redundancy LOST → fail closed, the
///    True-Redundancy doctrine).
// SAFETY: SG9 | REQ: perception-divergence-fails-closed | TEST: a_phantom_in_one_channel_diverges_and_caps_to_mrc,an_object_only_in_channel_b_also_diverges,a_matched_pair_disagreeing_on_speed_diverges,enabled_but_silent_secondary_fails_closed,disabled_monitor_is_inert_even_when_channels_diverge
#[must_use]
pub fn resolve_redundancy_cap(
    enabled: bool,
    primary: &[PerceivedObject],
    secondary: &[PerceivedObject],
    secondary_fresh: bool,
    cfg: RedundancyConfig,
) -> Option<f64> {
    if !enabled {
        return None; // no redundant channel → monitor inert (state 1)
    }
    if !secondary_fresh {
        return Some(0.0); // lost the redundant channel → cannot cross-check → fail closed (state 4)
    }
    cross_check(primary, secondary, cfg).to_perception_cap() // states 2 & 3
}

/// Compose two optional perception caps into the MORE RESTRICTIVE one: `None` = no cap, `Some` =
/// a speed ceiling, and the lower ceiling wins (an MRC-floor `Some(0.0)` always binds). Lets the
/// divergence cap fold into the existing Track-C derate cap without either masking the other.
#[must_use]
pub fn more_restrictive_cap(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Continuous-divergence duration (ms) after which the monitor escalates fleet posture to
/// `Degraded` — a divergence that PERSISTS this long is no longer a transient sensor blip the
/// per-tick MRC cap absorbs, but a perception-integrity concern.
pub const DIVERGENCE_DEGRADE_MS: u64 = 1_000;
/// Continuous-divergence duration (ms) after which the monitor escalates to `LockedOut` — the
/// redundant world model has been untrustworthy long enough that a controlled stop + human reset
/// is warranted, not just a speed cap. Mirrors the verifier's LockedOut "human-reset" semantics.
pub const DIVERGENCE_LOCKOUT_MS: u64 = 5_000;

/// **Sustained-divergence posture escalator.** The per-tick [`resolve_redundancy_cap`] already
/// brings the vehicle to a controlled stop on ANY divergence (the MRC-floor cap); this adds the
/// orthogonal, stickier signal the cap cannot express — a divergence that PERSISTS is a
/// perception-INTEGRITY fault that should escalate FLEET POSTURE, so the whole stack degrades
/// (and ultimately locks out for a human reset), not just this tick's speed. Pure and
/// deterministic (the caller supplies `now_ms`); parallels the frame-integrity S-FI1d hysteresis
/// and the verifier's recovery-streak pattern. A consistent observation clears the streak.
#[derive(Debug, Clone, Default)]
pub struct DivergenceEscalator {
    /// Wall-clock ms when the CURRENT continuous divergence began; `None` while consistent.
    diverged_since_ms: Option<u64>,
}

impl DivergenceEscalator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record this tick's cross-check outcome. `diverged` = the monitor returned a divergence (a
    /// phantom/miss/speed-mismatch, OR a lost redundant channel). A consistent tick clears the
    /// streak — the escalation recovers once perception AGREES again (the per-tick cap and the
    /// verifier's own posture remain the backstops).
    pub fn observe(&mut self, diverged: bool, now_ms: u64) {
        if diverged {
            self.diverged_since_ms.get_or_insert(now_ms);
        } else {
            self.diverged_since_ms = None;
        }
    }

    /// The posture this monitor RECOMMENDS at `now_ms`, to be escalated INTO the effective fleet
    /// posture (`base.escalate(recommended)`): `Nominal` while consistent or only MOMENTARILY
    /// diverged (the MRC cap handles that); `Degraded` once divergence has persisted
    /// [`DIVERGENCE_DEGRADE_MS`]; `LockedOut` at [`DIVERGENCE_LOCKOUT_MS`]. Escalation-only — it
    /// can only make the posture stricter, never relax it.
    // SAFETY: SG8 SG9 | REQ: sustained-divergence-escalates-posture | TEST: a_sustained_divergence_escalates_degraded_then_locked_out,a_momentary_divergence_does_not_escalate_posture,divergence_clearing_resets_the_streak,a_consistent_stream_never_escalates
    #[must_use]
    pub fn recommended_posture(&self, now_ms: u64) -> FleetPosture {
        match self.diverged_since_ms {
            None => FleetPosture::Nominal,
            Some(since) => {
                let elapsed = now_ms.saturating_sub(since);
                if elapsed >= DIVERGENCE_LOCKOUT_MS {
                    FleetPosture::LockedOut
                } else if elapsed >= DIVERGENCE_DEGRADE_MS {
                    FleetPosture::Degraded
                } else {
                    FleetPosture::Nominal // momentary → cap-only, no posture change
                }
            }
        }
    }
}

/// Cross-check two INDEPENDENT perception channels. Fail-closed: returns the FIRST
/// divergence found — an object with no EXCLUSIVE counterpart in the other channel
/// (in either direction), or a speed mismatch on a matched pair.
///
/// S5 (#1039): the match is **one-to-one**. Each channel-A object claims a DISTINCT
/// channel-B object — its nearest still-UNCLAIMED counterpart within
/// `cfg.position_tol_m` — and a B already claimed by an earlier A cannot be reused.
/// Any A that cannot claim a B, and any B left unclaimed after all A are matched, is
/// a divergence. The prior greedy match had each object independently take its
/// nearest counterpart with NO exclusivity, so two distinct A objects could both
/// "match" a single B (and that one B match a single A): a genuine second object seen
/// by only one channel — a phantom or a miss in a ~2 m cluster (two pedestrians /
/// vehicles a car-width apart) — passed as `Consistent`, defeating True-Redundancy
/// for exactly the dangerous case it exists to catch. Exclusivity can only ADD
/// divergences (fail-closed: an over-flag caps speed to the MRC floor, it never hides
/// a real object).
///
/// Determinism: A is matched in input order; each A takes its nearest unclaimed B
/// (ties broken by the lower B index). Empty-vs-empty is `Consistent`; an object on
/// only one side is always a divergence (a sensor that sees a hazard the other misses
/// must not be silently trusted *or* silently ignored).
#[must_use]
pub fn cross_check(
    channel_a: &[PerceivedObject],
    channel_b: &[PerceivedObject],
    cfg: RedundancyConfig,
) -> RedundancyVerdict {
    // One claim slot per B object; an A can only match a still-unclaimed B, so the
    // assignment is one-to-one and a second real object cannot be masked.
    let mut b_claimed = vec![false; channel_b.len()];
    for a in channel_a {
        match nearest_unclaimed(a, channel_b, &b_claimed, cfg.position_tol_m) {
            None => {
                return RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                    id: a.id,
                    channel: Channel::A,
                });
            }
            Some(idx) => {
                let m = &channel_b[idx];
                let delta = (a.velocity_mps - m.velocity_mps).abs();
                if delta > cfg.velocity_tol_mps {
                    return RedundancyVerdict::Diverged(DivergenceReason::SpeedMismatch {
                        id_a: a.id,
                        id_b: m.id,
                        delta_mps: delta,
                    });
                }
                b_claimed[idx] = true;
            }
        }
    }
    // Any B not claimed by some A is a B-only object (a B phantom / an A miss).
    for (i, b) in channel_b.iter().enumerate() {
        if !b_claimed[i] {
            return RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: b.id,
                channel: Channel::B,
            });
        }
    }
    RedundancyVerdict::Consistent
}

/// The INDEX of the nearest object in `candidates` that is (a) not yet `claimed` and
/// (b) within `tol_m` of `target` (by Euclidean position), if any. Ties → the lower
/// index (`min_by` returns the first equal-minimum). Returns an index rather than a
/// reference so the caller can mark the claim slot.
fn nearest_unclaimed(
    target: &PerceivedObject,
    candidates: &[PerceivedObject],
    claimed: &[bool],
    tol_m: f64,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| !claimed[*i])
        .map(|(i, c)| {
            (
                i,
                (c.pos.x_m - target.pos.x_m).hypot(c.pos.y_m - target.pos.y_m),
            )
        })
        .filter(|(_, d)| *d <= tol_m)
        .min_by(|(_, d1), (_, d2)| d1.total_cmp(d2))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corridor::Point;

    fn obj(id: u64, x: f64, y: f64, v: f64) -> PerceivedObject {
        PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: v,
            heading_rad: 0.0,
            vel: Point { x_m: v, y_m: 0.0 },
        }
    }

    #[test]
    fn agreeing_channels_are_consistent() {
        let a = [obj(1, 20.0, 0.0, 5.0), obj(2, 35.0, -3.0, 0.0)];
        // Same world, slightly noisy ids/positions/speeds within tolerance.
        let b = [obj(7, 20.4, 0.1, 5.6), obj(9, 34.8, -3.2, 0.0)];
        let v = cross_check(&a, &b, RedundancyConfig::default());
        assert_eq!(v, RedundancyVerdict::Consistent);
        assert_eq!(v.to_perception_cap(), None);
    }

    #[test]
    fn empty_channels_are_consistent() {
        assert_eq!(
            cross_check(&[], &[], RedundancyConfig::default()),
            RedundancyVerdict::Consistent
        );
    }

    #[test]
    fn a_phantom_in_one_channel_diverges_and_caps_to_mrc() {
        // Channel A sees a stopped car at x=20 that B does not → fail closed.
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b: [PerceivedObject; 0] = [];
        let v = cross_check(&a, &b, RedundancyConfig::default());
        assert!(v.is_diverged());
        assert_eq!(
            v,
            RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: 1,
                channel: Channel::A
            })
        );
        assert_eq!(
            v.to_perception_cap(),
            Some(0.0),
            "divergence → MRC-floor cap"
        );
    }

    #[test]
    fn an_object_only_in_channel_b_also_diverges() {
        let a: [PerceivedObject; 0] = [];
        let b = [obj(4, 18.0, 0.5, 2.0)];
        assert_eq!(
            cross_check(&a, &b, RedundancyConfig::default()),
            RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: 4,
                channel: Channel::B
            })
        );
    }

    #[test]
    fn a_matched_pair_disagreeing_on_speed_diverges() {
        // Same position, wildly different speed (one channel says stopped, one says fast).
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b = [obj(2, 20.3, 0.0, 8.0)];
        match cross_check(&a, &b, RedundancyConfig::default()) {
            RedundancyVerdict::Diverged(DivergenceReason::SpeedMismatch {
                id_a,
                id_b,
                delta_mps,
            }) => {
                assert_eq!((id_a, id_b), (1, 2));
                assert!((delta_mps - 8.0).abs() < 1e-9);
            }
            other => panic!("expected a speed mismatch, got {other:?}"),
        }
    }

    /// S5 (#1039) THE masking case: two distinct A objects clustered within
    /// `position_tol_m` of a SINGLE B object must diverge — the second A has no
    /// exclusive counterpart, so channel A genuinely sees an object B does not. The
    /// prior greedy (non-exclusive) match let both A objects "match" the one B and
    /// passed this as `Consistent`, hiding the extra object.
    #[test]
    fn two_clustered_a_objects_sharing_one_b_diverge() {
        // a1 and a2 are ~1 m apart, both within 2 m of the single b7.
        let a = [obj(1, 20.0, 0.0, 0.0), obj(2, 21.0, 0.0, 0.0)];
        let b = [obj(7, 20.5, 0.0, 0.0)];
        let v = cross_check(&a, &b, RedundancyConfig::default());
        assert!(
            v.is_diverged(),
            "a second A with no exclusive B must diverge, got {v:?}"
        );
        assert_eq!(
            v,
            RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: 2,
                channel: Channel::A
            }),
            "the un-paired second A object is the divergence"
        );
        assert_eq!(
            v.to_perception_cap(),
            Some(0.0),
            "divergence → MRC-floor cap"
        );
    }

    /// Symmetric: two B objects clustered near a single A → the un-paired B (claimed
    /// by no A) is the divergence.
    #[test]
    fn two_clustered_b_objects_sharing_one_a_diverge() {
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b = [obj(7, 20.3, 0.0, 0.0), obj(9, 20.6, 0.0, 0.0)];
        assert_eq!(
            cross_check(&a, &b, RedundancyConfig::default()),
            RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: 9,
                channel: Channel::B
            })
        );
    }

    /// One-to-one must NOT over-flag a legitimate distinct 2↔2 pairing: each A has its
    /// own nearby, exclusive B, so all claims resolve → `Consistent`.
    #[test]
    fn distinct_pairs_match_one_to_one_and_stay_consistent() {
        let a = [obj(1, 20.0, 0.0, 3.0), obj(2, 40.0, 0.0, 3.0)];
        let b = [obj(7, 20.2, 0.0, 3.0), obj(9, 40.1, 0.0, 3.0)];
        assert_eq!(
            cross_check(&a, &b, RedundancyConfig::default()),
            RedundancyVerdict::Consistent
        );
    }

    /// The speed-mismatch boundary is STRICT (`>`): a position-matched pair whose
    /// speed delta is EXACTLY `velocity_tol_mps` is still within tolerance →
    /// `Consistent`. Pins the `>` operator against the `>=` mutant (which would
    /// wrongly diverge a pair sitting exactly on the tolerance). `5.0 - 3.5 == 1.5`
    /// is bit-exact and equals the default `velocity_tol_mps`.
    #[test]
    fn speed_delta_exactly_at_tolerance_is_consistent() {
        let a = [obj(1, 20.0, 0.0, 5.0)];
        let b = [obj(7, 20.2, 0.0, 3.5)]; // |5.0 - 3.5| == 1.5 == velocity_tol_mps
        assert_eq!(
            cross_check(&a, &b, RedundancyConfig::default()),
            RedundancyVerdict::Consistent,
            "delta EXACTLY at the tolerance is within tolerance (strict >), not a mismatch"
        );
    }

    // ----- the live-monitor resolution (4-state machine) -----

    #[test]
    fn disabled_monitor_is_inert_even_when_channels_diverge() {
        // State 1: a stark divergence, but the monitor is off → no cap (byte-identical prior).
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b: [PerceivedObject; 0] = [];
        assert_eq!(
            resolve_redundancy_cap(false, &a, &b, true, RedundancyConfig::default()),
            None
        );
    }

    #[test]
    fn enabled_consistent_channels_add_no_cap() {
        // State 2: enabled, fresh secondary, channels agree → None.
        let a = [obj(1, 20.0, 0.0, 5.0)];
        let b = [obj(7, 20.3, 0.1, 5.4)];
        assert_eq!(
            resolve_redundancy_cap(true, &a, &b, true, RedundancyConfig::default()),
            None
        );
    }

    #[test]
    fn enabled_diverged_channels_cap_to_mrc() {
        // State 3: enabled, fresh, a phantom in A that B misses → MRC-floor cap.
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b: [PerceivedObject; 0] = [];
        assert_eq!(
            resolve_redundancy_cap(true, &a, &b, true, RedundancyConfig::default()),
            Some(0.0)
        );
    }

    #[test]
    fn enabled_but_silent_secondary_fails_closed() {
        // State 4: the redundant channel went stale → redundancy lost → fail closed, EVEN IF the
        // last secondary snapshot happens to still agree (it is no longer assured-fresh).
        let a = [obj(1, 20.0, 0.0, 5.0)];
        let b = [obj(7, 20.3, 0.1, 5.4)];
        assert_eq!(
            resolve_redundancy_cap(true, &a, &b, false, RedundancyConfig::default()),
            Some(0.0)
        );
    }

    #[test]
    fn more_restrictive_cap_takes_the_lower_ceiling() {
        use super::more_restrictive_cap;
        assert_eq!(more_restrictive_cap(None, None), None);
        assert_eq!(more_restrictive_cap(Some(3.0), None), Some(3.0));
        assert_eq!(more_restrictive_cap(None, Some(2.0)), Some(2.0));
        assert_eq!(
            more_restrictive_cap(Some(3.0), Some(0.0)),
            Some(0.0),
            "an MRC floor binds"
        );
        assert_eq!(more_restrictive_cap(Some(5.0), Some(2.0)), Some(2.0));
    }

    // ----- sustained-divergence posture escalation -----

    #[test]
    fn a_consistent_stream_never_escalates() {
        let mut e = DivergenceEscalator::new();
        e.observe(false, 1_000);
        e.observe(false, 9_999);
        assert_eq!(e.recommended_posture(9_999), FleetPosture::Nominal);
    }

    #[test]
    fn a_momentary_divergence_does_not_escalate_posture() {
        // The per-tick MRC cap handles a blip; posture stays Nominal under DIVERGENCE_DEGRADE_MS.
        let mut e = DivergenceEscalator::new();
        e.observe(true, 1_000);
        assert_eq!(
            e.recommended_posture(1_000 + DIVERGENCE_DEGRADE_MS - 1),
            FleetPosture::Nominal
        );
    }

    #[test]
    fn a_sustained_divergence_escalates_degraded_then_locked_out() {
        let mut e = DivergenceEscalator::new();
        e.observe(true, 1_000); // divergence onset; stays diverged across ticks
        e.observe(true, 1_500);
        assert_eq!(
            e.recommended_posture(1_000 + DIVERGENCE_DEGRADE_MS),
            FleetPosture::Degraded,
            "≥ degrade window → Degraded"
        );
        assert_eq!(
            e.recommended_posture(1_000 + DIVERGENCE_LOCKOUT_MS),
            FleetPosture::LockedOut,
            "≥ lockout window → LockedOut"
        );
    }

    #[test]
    fn divergence_clearing_resets_the_streak() {
        let mut e = DivergenceEscalator::new();
        e.observe(true, 1_000);
        // It had been diverging long enough to escalate...
        assert_eq!(
            e.recommended_posture(1_000 + DIVERGENCE_LOCKOUT_MS),
            FleetPosture::LockedOut
        );
        // ...but a consistent tick clears it → recovers to Nominal (cap + verifier posture remain).
        e.observe(false, 1_000 + DIVERGENCE_LOCKOUT_MS + 10);
        assert_eq!(
            e.recommended_posture(1_000 + DIVERGENCE_LOCKOUT_MS + 10),
            FleetPosture::Nominal
        );
        // A NEW divergence restarts the streak from its own onset (not the old one).
        e.observe(true, 100_000);
        assert_eq!(
            e.recommended_posture(100_000 + 10),
            FleetPosture::Nominal,
            "fresh streak → momentary again"
        );
    }
}
