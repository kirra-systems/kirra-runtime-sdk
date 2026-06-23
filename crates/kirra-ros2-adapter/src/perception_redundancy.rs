//! **Perception redundancy cross-check** — a fail-closed assurance monitor, the
//! True-Redundancy analog (gap #2b / P1).
//!
//! Mobileye's *True Redundancy* runs two INDEPENDENT world models (camera-only vs.
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
        Self { position_tol_m: 2.0, velocity_tol_mps: 1.5 }
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
    SpeedMismatch { id_a: u64, id_b: u64, delta_mps: f64 },
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

/// Cross-check two INDEPENDENT perception channels. Fail-closed: returns the FIRST
/// divergence found (an unmatched object in either direction, or a speed mismatch on a
/// matched pair). Matching is nearest-position within `cfg.position_tol_m`.
///
/// Determinism: objects are matched in input order, nearest-first; ties broken by the
/// other channel's index. Empty-vs-empty is `Consistent`; an object on only one side
/// is always a divergence (a sensor that sees a hazard the other misses must not be
/// silently trusted *or* silently ignored).
#[must_use]
pub fn cross_check(
    channel_a: &[PerceivedObject],
    channel_b: &[PerceivedObject],
    cfg: RedundancyConfig,
) -> RedundancyVerdict {
    // Every A must have a B counterpart (and agree on speed).
    if let Some(reason) = unmatched_or_mismatched(channel_a, channel_b, cfg, Channel::A) {
        return RedundancyVerdict::Diverged(reason);
    }
    // And every B must have an A counterpart (catches B-only phantoms / A misses).
    // Speed is already checked above; here we only need the existence direction.
    for b in channel_b {
        if nearest_within(b, channel_a, cfg.position_tol_m).is_none() {
            return RedundancyVerdict::Diverged(DivergenceReason::Unmatched {
                id: b.id,
                channel: Channel::B,
            });
        }
    }
    RedundancyVerdict::Consistent
}

/// For each object in `from`, require a position-match in `other` and speed agreement.
fn unmatched_or_mismatched(
    from: &[PerceivedObject],
    other: &[PerceivedObject],
    cfg: RedundancyConfig,
    from_channel: Channel,
) -> Option<DivergenceReason> {
    for o in from {
        match nearest_within(o, other, cfg.position_tol_m) {
            None => {
                return Some(DivergenceReason::Unmatched { id: o.id, channel: from_channel });
            }
            Some(m) => {
                let delta = (o.velocity_mps - m.velocity_mps).abs();
                if delta > cfg.velocity_tol_mps {
                    // Order the ids A-then-B regardless of which side `from` is.
                    let (id_a, id_b) = match from_channel {
                        Channel::A => (o.id, m.id),
                        Channel::B => (m.id, o.id),
                    };
                    return Some(DivergenceReason::SpeedMismatch { id_a, id_b, delta_mps: delta });
                }
            }
        }
    }
    None
}

/// The nearest object in `candidates` within `tol_m` of `target` (by Euclidean
/// position), if any.
fn nearest_within<'a>(
    target: &PerceivedObject,
    candidates: &'a [PerceivedObject],
    tol_m: f64,
) -> Option<&'a PerceivedObject> {
    candidates
        .iter()
        .map(|c| (c, (c.pos.x_m - target.pos.x_m).hypot(c.pos.y_m - target.pos.y_m)))
        .filter(|(_, d)| *d <= tol_m)
        .min_by(|(_, d1), (_, d2)| d1.total_cmp(d2))
        .map(|(c, _)| c)
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
        assert_eq!(cross_check(&[], &[], RedundancyConfig::default()), RedundancyVerdict::Consistent);
    }

    #[test]
    fn a_phantom_in_one_channel_diverges_and_caps_to_mrc() {
        // Channel A sees a stopped car at x=20 that B does not → fail closed.
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b: [PerceivedObject; 0] = [];
        let v = cross_check(&a, &b, RedundancyConfig::default());
        assert!(v.is_diverged());
        assert_eq!(v, RedundancyVerdict::Diverged(DivergenceReason::Unmatched { id: 1, channel: Channel::A }));
        assert_eq!(v.to_perception_cap(), Some(0.0), "divergence → MRC-floor cap");
    }

    #[test]
    fn an_object_only_in_channel_b_also_diverges() {
        let a: [PerceivedObject; 0] = [];
        let b = [obj(4, 18.0, 0.5, 2.0)];
        assert_eq!(
            cross_check(&a, &b, RedundancyConfig::default()),
            RedundancyVerdict::Diverged(DivergenceReason::Unmatched { id: 4, channel: Channel::B })
        );
    }

    #[test]
    fn a_matched_pair_disagreeing_on_speed_diverges() {
        // Same position, wildly different speed (one channel says stopped, one says fast).
        let a = [obj(1, 20.0, 0.0, 0.0)];
        let b = [obj(2, 20.3, 0.0, 8.0)];
        match cross_check(&a, &b, RedundancyConfig::default()) {
            RedundancyVerdict::Diverged(DivergenceReason::SpeedMismatch { id_a, id_b, delta_mps }) => {
                assert_eq!((id_a, id_b), (1, 2));
                assert!((delta_mps - 8.0).abs() < 1e-9);
            }
            other => panic!("expected a speed mismatch, got {other:?}"),
        }
    }
}
