//! **Fast-loop trajectory tracker** — the System-1 side of the dual-rate loop.
//!
//! The slow loop (System-2 → Occy → KIRRA) authors and *admits* a whole trajectory now and
//! then; the fast loop must turn that one committed trajectory into a command every tick, in
//! between the slow loop's (infrequent) re-plans. The correct way — and what the production
//! fast loop does (`kirra_trajectory::check_command_conforms`) — is to sample the committed
//! trajectory at the **elapsed time since it was promoted** (`now − promoted_at`), advancing a
//! monotonic cursor across many fast ticks. The ego then follows the trajectory's own velocity
//! profile, so it accelerates from a standstill and tracks a curve instead of teleporting along
//! the first 0.1 s of a freshly re-planned trajectory each tick (which can neither re-accelerate
//! from a dead stop nor hold a turn).
//!
//! This is a pure, owned helper over a [`PlanOutput`]; it holds no clock and no actuator.

use crate::{PlanOutput, Pose};

/// Tracks one admitted trajectory and samples the per-tick command from it by elapsed time.
#[derive(Default)]
pub struct FastLoopTracker {
    active: Option<Tracked>,
}

struct Tracked {
    plan: PlanOutput,
    promoted_at_ms: u64,
}

/// The fast-loop command for a tick: the tracked pose to be at, and the speed to hold there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackedCommand {
    pub pose: Pose,
    pub velocity_mps: f64,
}

impl FastLoopTracker {
    #[must_use]
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Promote a freshly **admitted** trajectory: the cursor restarts from `now_ms`, so the
    /// fast loop begins tracking this trajectory from its first pose. Call this only when the
    /// slow loop produces a new accepted trajectory — NOT every fast tick (that would pin the
    /// cursor at ~0 and defeat the point).
    pub fn promote(&mut self, plan: PlanOutput, now_ms: u64) {
        self.active = Some(Tracked {
            plan,
            promoted_at_ms: now_ms,
        });
    }

    /// The command to apply at `now_ms`: the active trajectory sampled at the elapsed time
    /// since promotion (nearest forward pose, matching the production conformance check).
    /// `None` if nothing is promoted or the trajectory is exhausted — the caller should MRC
    /// (and ask the slow loop for a fresh trajectory).
    #[must_use]
    pub fn track(&self, now_ms: u64) -> Option<TrackedCommand> {
        let t = self.active.as_ref()?;
        let elapsed_s = now_ms.saturating_sub(t.promoted_at_ms) as f64 / 1000.0;
        t.plan
            .trajectory
            .iter()
            .find(|p| p.time_from_start_s >= elapsed_s)
            .map(|p| TrackedCommand {
                pose: p.pose,
                velocity_mps: p.velocity_mps,
            })
    }

    /// Whether the committed trajectory is spent at `now_ms` (the cursor is past its last pose,
    /// or nothing is promoted) — the fast loop should ask the slow loop to re-plan.
    #[must_use]
    pub fn is_exhausted(&self, now_ms: u64) -> bool {
        match &self.active {
            None => true,
            Some(t) => {
                let elapsed_s = now_ms.saturating_sub(t.promoted_at_ms) as f64 / 1000.0;
                t.plan
                    .trajectory
                    .last()
                    .is_none_or(|p| p.time_from_start_s < elapsed_s)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanOutput, Pose, ProposalKind};
    use kirra_core::trajectory::TrajectoryPoint;

    /// A straight trajectory that ACCELERATES from rest: pose advances with the velocity ramp,
    /// one pose every 0.1 s. The tracking-from-a-stop case the toy conformance could not do.
    fn accelerating_plan() -> PlanOutput {
        let mut trajectory = Vec::new();
        let (mut x, mut v) = (0.0_f64, 0.0_f64);
        for i in 0..=20 {
            let t = i as f64 * 0.1;
            trajectory.push(TrajectoryPoint {
                pose: Pose {
                    x_m: x,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: v,
                time_from_start_s: t,
            });
            v += 0.5; // +0.5 m/s each 0.1 s step
            x += v * 0.1;
        }
        PlanOutput {
            trajectory,
            kind: ProposalKind::Motion,
        }
    }

    #[test]
    fn tracks_the_velocity_ramp_across_ticks_instead_of_pinning_at_the_start() {
        let mut tr = FastLoopTracker::new();
        tr.promote(accelerating_plan(), 1_000);

        // Sampling at successive elapsed times follows the ramp — velocity and position climb,
        // exactly what lets the ego accelerate from a standstill (the toy conformance, pinned at
        // ~0.1 s of an ever-restarting plan from rest, stays ~0).
        let a = tr.track(1_100).unwrap(); // elapsed 0.1 s
        let b = tr.track(1_500).unwrap(); // elapsed 0.5 s
        let c = tr.track(2_000).unwrap(); // elapsed 1.0 s
        assert!(
            a.velocity_mps < b.velocity_mps && b.velocity_mps < c.velocity_mps,
            "velocity climbs along the committed plan"
        );
        assert!(
            a.pose.x_m < b.pose.x_m && b.pose.x_m < c.pose.x_m,
            "position advances along the committed plan"
        );
    }

    #[test]
    fn reports_exhaustion_past_the_end_and_emptiness_when_unpromoted() {
        let tr0 = FastLoopTracker::new();
        assert!(tr0.is_exhausted(0), "nothing promoted → exhausted");
        assert!(tr0.track(0).is_none());

        let mut tr = FastLoopTracker::new();
        tr.promote(accelerating_plan(), 0); // spans 0..=2.0 s
        assert!(!tr.is_exhausted(1_000), "mid-trajectory is not exhausted");
        assert!(tr.track(1_000).is_some());
        assert!(
            tr.is_exhausted(2_100),
            "past the last pose (2.0 s) → exhausted"
        );
        assert!(
            tr.track(2_100).is_none(),
            "exhausted → no command (caller MRCs)"
        );
    }
}
