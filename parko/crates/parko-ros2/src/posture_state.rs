// parko/crates/parko-ros2/src/posture_state.rs
//
// M2b — wrapper around the SHARED `kirra_runtime_sdk::posture_tracker::PostureTracker`
// for the Parko ROS 2 node.
//
// The fail-closed state machine lives in the kernel
// (`kirra_runtime_sdk::posture_tracker::PostureTracker`) so the
// adapter (`kirra-ros2-adapter`) and this node share ONE
// implementation — duplicating safety logic is the one thing M2b
// exists to prevent.
//
// This module provides a parko-side wrapper that:
//   - Owns the tracker behind an `Arc<RwLock<…>>` (the trait needs
//     `Send + Sync` and `observe` mutates).
//   - Bridges `FleetPosture` (kernel enum) → `SafetyPosture`
//     (parko-core enum) at the read boundary, because the parko
//     governor's `evaluate` takes `SafetyPosture`. The two enums
//     have identical variant sets — the bridge is a pure 3-arm
//     match, not a semantic conversion.
//   - Provides `observe(FleetPosture)` so the SSE task (or any
//     other source — bridged ROS 2 topic, manual test fixture)
//     can drive the tracker.
//
// Fail-closed properties are inherited verbatim from the kernel:
//   - pre-first-event seed = Degraded (source-configured constructor)
//   - staleness derate Nominal → Degraded after `POSTURE_STALENESS_TIMEOUT_MS`
//   - LockedOut is sticky-toward-safe
//   - no-source mode → Nominal (preserves the M2 default behaviour)

use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use kirra_runtime_sdk::posture_tracker::PostureTracker;
use kirra_runtime_sdk::verifier::FleetPosture;
use parko_core::safety::SafetyPosture;

/// Owning wrapper around the kernel's `PostureTracker` for parko-ros2.
///
/// Cloning is cheap — the inner state is `Arc<RwLock<…>>`. Hand
/// `Arc<ParkoPostureState>` to the drain tasks so each can read /
/// observe without contention; the kernel tracker's wall-clock
/// resolution is `u64 ms` so there's no time-source coupling.
pub struct ParkoPostureState {
    tracker: Arc<RwLock<PostureTracker>>,
}

impl ParkoPostureState {
    /// M2 default: NO posture source configured. `current_*` always
    /// returns `Nominal`; `observe` is a no-op. Matches the pre-M2b
    /// behaviour exactly so any caller that constructs the node
    /// without a posture source sees zero behaviour change.
    #[must_use]
    pub fn no_source() -> Self {
        Self {
            tracker: Arc::new(RwLock::new(PostureTracker::nominal_default_no_source())),
        }
    }

    /// M2b: posture source configured. Pre-first-event seed =
    /// `Degraded` (fail-closed); `observe(p)` then updates per the
    /// kernel tracker's state machine. Used by the binary when
    /// `KIRRA_POSTURE_STREAM_URL` is set (the operator's intent to
    /// govern is explicit).
    #[must_use]
    pub fn with_source() -> Self {
        Self {
            tracker: Arc::new(RwLock::new(PostureTracker::with_source())),
        }
    }

    /// Record an observation from the configured source. No-op for
    /// no-source trackers (the kernel tracker enforces this so the
    /// fail-closed seed can never be bypassed via `observe`).
    pub fn observe(&self, posture: FleetPosture) {
        let now = current_time_ms();
        match self.tracker.write() {
            Ok(mut guard) => guard.observe(now, posture),
            Err(poisoned) => poisoned.into_inner().observe(now, posture),
        }
    }

    /// Read the effective `FleetPosture` from the tracker at the
    /// current wall-clock instant. Fail-closed: a poisoned lock
    /// returns `Degraded` rather than `Nominal` so a panic in the
    /// writer can never widen the envelope. Mirrors the adapter's
    /// `AdaptorState::current_posture` discipline.
    #[must_use]
    pub fn current_fleet_posture(&self) -> FleetPosture {
        let now = current_time_ms();
        match self.tracker.read() {
            Ok(guard) => guard.current_posture(now),
            Err(_)    => FleetPosture::Degraded,
        }
    }

    /// Read the effective posture as `parko_core::safety::SafetyPosture`.
    /// Convenience wrapper for the tick pipeline — parko's governor
    /// takes the parko-core enum, the kernel tracker emits the kernel
    /// enum, and the bridge is a 3-arm pure match.
    #[must_use]
    pub fn current_safety_posture(&self) -> SafetyPosture {
        fleet_to_safety(self.current_fleet_posture())
    }
}

/// Bridge `FleetPosture` (kernel) → `SafetyPosture` (parko-core).
/// The two enums have identical variant sets; this match is a pure
/// projection, not a semantic conversion. Kept as a free function
/// so it's directly unit-testable.
#[must_use]
pub fn fleet_to_safety(p: FleetPosture) -> SafetyPosture {
    match p {
        FleetPosture::Nominal   => SafetyPosture::Nominal,
        FleetPosture::Degraded  => SafetyPosture::Degraded,
        FleetPosture::LockedOut => SafetyPosture::LockedOut,
    }
}

#[inline]
fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests — fail-closed behaviour through the parko wrapper. All run on
// stable; the kernel tracker's own 10 tests cover the state machine
// itself, so these only verify the wiring + the FleetPosture →
// SafetyPosture bridge.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_to_safety_is_pure_projection() {
        assert!(matches!(fleet_to_safety(FleetPosture::Nominal),   SafetyPosture::Nominal));
        assert!(matches!(fleet_to_safety(FleetPosture::Degraded),  SafetyPosture::Degraded));
        assert!(matches!(fleet_to_safety(FleetPosture::LockedOut), SafetyPosture::LockedOut));
    }

    #[test]
    fn no_source_default_is_nominal_across_consecutive_reads() {
        let state = ParkoPostureState::no_source();
        // Multiple reads at wall-clock time advances must still
        // return Nominal — the no-source mode is time-independent.
        assert!(matches!(state.current_fleet_posture(),  FleetPosture::Nominal));
        assert!(matches!(state.current_safety_posture(), SafetyPosture::Nominal));
        // observe is a no-op in this mode — verify the tracker
        // refuses to latch a LockedOut even when one is offered.
        state.observe(FleetPosture::LockedOut);
        assert!(matches!(state.current_fleet_posture(), FleetPosture::Nominal),
            "no_source must ignore observe and hold Nominal");
    }

    #[test]
    fn source_pre_first_event_seeds_degraded_not_nominal() {
        // The whole point of M2b — a source-configured Parko node
        // that hasn't yet received an event must NOT command at the
        // full envelope. The kernel tracker's pre-first-event seed
        // gives us this for free.
        let state = ParkoPostureState::with_source();
        assert!(matches!(state.current_fleet_posture(),  FleetPosture::Degraded));
        assert!(matches!(state.current_safety_posture(), SafetyPosture::Degraded));
    }

    #[test]
    fn observe_nominal_then_read_is_nominal() {
        let state = ParkoPostureState::with_source();
        state.observe(FleetPosture::Nominal);
        assert!(matches!(state.current_safety_posture(), SafetyPosture::Nominal));
    }

    #[test]
    fn observe_locked_out_is_sticky() {
        let state = ParkoPostureState::with_source();
        state.observe(FleetPosture::LockedOut);
        // A subsequent read should still be LockedOut — the kernel
        // tracker latches sticky-toward-safe.
        assert!(matches!(state.current_safety_posture(), SafetyPosture::LockedOut));
        // Recovery requires an EXPLICIT non-LockedOut event.
        state.observe(FleetPosture::Nominal);
        assert!(matches!(state.current_safety_posture(), SafetyPosture::Nominal));
    }
}
