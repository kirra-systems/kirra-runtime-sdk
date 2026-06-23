// crates/kirra-core/src/posture_tracker.rs (de-monolith Stage 5: relocated verbatim from the SDK kernel)
//
// M1b — fail-closed fleet-posture state machine for the Occy ros2-adapter.
//
// The tracker is a pure, deterministic state machine: no I/O, no async, no
// clock reads. The caller supplies `now_ms` on every observe / read, so the
// behaviour is unit-testable on stable without ROS or a verifier service.
//
// Two modes:
//
//   - `nominal_default_no_source`  — no posture source configured. The
//     adapter behaves exactly as in M1 (no behaviour change from the M1
//     era): `current_posture` always returns `Nominal`. This is the path
//     for verifier-less deployments and unit tests that don't drive a
//     source.
//
//   - `with_source`                — a source IS configured. Three
//     fail-closed properties hold:
//
//       1. Pre-first-event seed = `Degraded`, NOT `Nominal`.
//          A new / unverified fleet must not be commandable at full
//          envelope before the verifier confirms posture. Mirrors the
//          verifier's own "new asset → Degraded" seed.
//
//       2. Staleness derate. If wall-clock now exceeds `last_event_ms
//          + POSTURE_STALENESS_TIMEOUT_MS`, the current posture derates
//          to `Degraded` — we do NOT hold the last-known `Nominal`.
//          This is the posture analog of the SG9 telemetry watchdog.
//
//       3. `LockedOut` is sticky-toward-safe. Once a `LockedOut`
//          event lands, the staleness derate does not RELAX the
//          ceiling — we stay at `LockedOut` (already the most
//          restrictive). The only way out is an explicit non-LockedOut
//          observation from the source (the "recovery event").
//
// The mode is fixed at construction. Production deployments call
// `with_source` from the binary's startup when the
// `KIRRA_POSTURE_STREAM_URL` env var is set; the existing M1 default
// (`nominal_default_no_source`) is preserved for callers that pass no
// posture source.

use crate::FleetPosture;

/// Maximum wall-clock age a posture observation may have before the
/// tracker derates to `Degraded`.
///
/// Rationale: the verifier's SSE stream uses `tokio_stream`'s default
/// `KeepAlive` (15s heartbeat) on top of posture-engine broadcasts that
/// fire on transitions and on every recalculation
/// (`POSTURE_CACHE_TTL_MS` = 5_000 ms in the verifier). At 5s expected
/// inter-event spacing, 6_000 ms is a single-cycle slack — tight enough
/// that a one-cycle drop derates, loose enough that ordinary jitter on
/// the network won't false-positive. **TODO: revisit this value after
/// the first integration deployment measures real SSE-event arrival
/// jitter; if the keepalive cadence dominates over the recalc events,
/// bump to 3× keepalive (45s).**
pub const POSTURE_STALENESS_TIMEOUT_MS: u64 = 6_000;

/// Fail-closed fleet-posture state machine.
///
/// See module-level doc for the three fail-closed properties; see
/// `current_posture` for the resolution logic.
///
// SAFETY: SG8 SG9 | REQ: posture-source-fail-closed | TEST: tracker_no_source_stays_nominal,tracker_source_pre_first_event_is_degraded,tracker_nominal_event_then_stale_derates_to_degraded,tracker_locked_out_sticky_through_staleness,tracker_locked_out_recovers_on_explicit_nominal_event,tracker_in_window_nominal_observation_is_reflected,tracker_in_window_degraded_observation_is_reflected,tracker_staleness_boundary_inclusive_to_one_ms,tracker_no_source_ignores_observe
#[derive(Debug, Clone)]
pub struct PostureTracker {
    /// `false` for the M1 default (no source configured); `true` when a
    /// source is configured at construction time. The mode is fixed
    /// for the tracker's lifetime — an integrator changes mode by
    /// recreating `AdaptorState`.
    source_configured: bool,
    /// Last observation from the source. `None` until the first
    /// `observe` call.
    last_observation: Option<FleetPosture>,
    /// Wall-clock-ms timestamp of the last `observe` call.
    /// `None` until the first event.
    last_event_ms: Option<u64>,
    /// Sticky-LockedOut flag — set whenever an `observe` of
    /// `LockedOut` lands, cleared when an `observe` of a non-LockedOut
    /// posture lands. Lets the staleness path return `LockedOut`
    /// instead of dropping to `Degraded`.
    sticky_locked_out: bool,
}

impl PostureTracker {
    /// Construct a tracker for the no-source configuration. M1 behaviour
    /// is preserved: `current_posture` always returns `Nominal`,
    /// `observe` is a no-op.
    #[must_use]
    pub fn nominal_default_no_source() -> Self {
        Self {
            source_configured: false,
            last_observation: None,
            last_event_ms: None,
            sticky_locked_out: false,
        }
    }

    /// Construct a tracker for a source-configured deployment. The
    /// pre-first-event seed is `Degraded` (fail-closed): a new
    /// fleet must not be commandable at full envelope before the
    /// verifier confirms posture.
    #[must_use]
    pub fn with_source() -> Self {
        Self {
            source_configured: true,
            last_observation: None,
            last_event_ms: None,
            sticky_locked_out: false,
        }
    }

    /// `true` if this tracker is configured with a live posture source.
    #[must_use]
    pub fn source_configured(&self) -> bool {
        self.source_configured
    }

    /// Record a posture observation from the configured source.
    ///
    /// No-op when no source is configured (the M1 path) — callers must
    /// not be able to bypass the fail-closed semantics by injecting
    /// observations into a no-source tracker. Tests that need to drive
    /// posture directly should construct a source-configured tracker.
    pub fn observe(&mut self, now_ms: u64, posture: FleetPosture) {
        if !self.source_configured {
            return;
        }
        self.last_event_ms = Some(now_ms);
        // Sticky-LockedOut: latch on LockedOut; release on any
        // non-LockedOut observation. The release is the only way out
        // of sticky LockedOut — a staleness timeout cannot release
        // it (the timeout path would relax the ceiling, which is the
        // opposite of fail-closed).
        self.sticky_locked_out = posture == FleetPosture::LockedOut;
        self.last_observation = Some(posture);
    }

    /// Resolve the effective posture at wall-clock `now_ms`.
    ///
    /// No-source path: always `Nominal` (preserves M1 behaviour).
    ///
    /// Source-configured path, in priority order:
    ///   1. Sticky-LockedOut → return `LockedOut` (most restrictive
    ///      regardless of staleness).
    ///   2. No observation yet → return `Degraded` (pre-first-event
    ///      seed).
    ///   3. `now_ms - last_event_ms > POSTURE_STALENESS_TIMEOUT_MS`
    ///      → return `Degraded` (staleness derate; the last-known
    ///      `Nominal` is NOT held).
    ///   4. Otherwise → return the most recent observation.
    #[must_use]
    pub fn current_posture(&self, now_ms: u64) -> FleetPosture {
        if !self.source_configured {
            return FleetPosture::Nominal;
        }
        if self.sticky_locked_out {
            return FleetPosture::LockedOut;
        }
        match (self.last_event_ms, self.last_observation.clone()) {
            (None, _) => FleetPosture::Degraded,
            (Some(last), Some(observed)) => {
                if now_ms.saturating_sub(last) > POSTURE_STALENESS_TIMEOUT_MS {
                    FleetPosture::Degraded
                } else {
                    observed
                }
            }
            // last_event_ms set but observation missing — defensive
            // (the two are updated together inside `observe`, so this
            // is unreachable in practice). Fail closed.
            (Some(_), None) => FleetPosture::Degraded,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (STEP 4 — pure state machine, on stable)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tracker_tests {
    use super::*;

    #[test]
    fn tracker_no_source_stays_nominal() {
        // M1 behaviour unchanged: no source configured → Nominal forever,
        // regardless of clock advancement or stray observe calls.
        let mut t = PostureTracker::nominal_default_no_source();
        assert_eq!(t.current_posture(0),         FleetPosture::Nominal);
        assert_eq!(t.current_posture(1_000_000), FleetPosture::Nominal);
        // observe is a no-op in this mode — verify directly.
        t.observe(1_000, FleetPosture::Degraded);
        t.observe(2_000, FleetPosture::LockedOut);
        assert_eq!(t.current_posture(3_000),     FleetPosture::Nominal,
            "observe must not affect a no-source tracker");
    }

    #[test]
    fn tracker_no_source_ignores_observe() {
        // The previous test exercised observe + read; this test pins the
        // invariant by name so a future refactor doesn't accidentally
        // let observe leak through in no-source mode.
        let mut t = PostureTracker::nominal_default_no_source();
        assert!(!t.source_configured());
        t.observe(1_000, FleetPosture::LockedOut);
        assert!(!t.sticky_locked_out,
            "no-source tracker must not latch sticky-LockedOut from observe");
    }

    #[test]
    fn tracker_source_pre_first_event_is_degraded() {
        // A new source-configured tracker has not yet received a posture
        // event. The fail-closed seed is Degraded — NOT Nominal —
        // because the fleet posture has not yet been confirmed.
        let t = PostureTracker::with_source();
        assert!(t.source_configured());
        assert_eq!(t.current_posture(0),     FleetPosture::Degraded);
        assert_eq!(t.current_posture(5_000), FleetPosture::Degraded);
        assert_eq!(t.current_posture(60_000), FleetPosture::Degraded,
            "pre-first-event state must remain Degraded indefinitely");
    }

    #[test]
    fn tracker_in_window_nominal_observation_is_reflected() {
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::Nominal);
        // Read inside the staleness window — the observed posture applies.
        assert_eq!(t.current_posture(1_000), FleetPosture::Nominal);
        assert_eq!(t.current_posture(1_000 + POSTURE_STALENESS_TIMEOUT_MS),
                   FleetPosture::Nominal,
            "boundary read AT the staleness timeout must still be in-window");
    }

    #[test]
    fn tracker_in_window_degraded_observation_is_reflected() {
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::Degraded);
        assert_eq!(t.current_posture(2_000), FleetPosture::Degraded);
    }

    #[test]
    fn tracker_nominal_event_then_stale_derates_to_degraded() {
        // Source sent Nominal, then went silent. Once the staleness
        // timeout elapses, the tracker MUST derate to Degraded — it
        // must NOT hold the last-known Nominal. This is the SG9-style
        // posture watchdog.
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::Nominal);
        // 1ms past the timeout boundary → stale.
        let stale_at = 1_000 + POSTURE_STALENESS_TIMEOUT_MS + 1;
        assert_eq!(t.current_posture(stale_at), FleetPosture::Degraded,
            "staleness must derate the last-known Nominal to Degraded");
    }

    #[test]
    fn tracker_staleness_boundary_inclusive_to_one_ms() {
        // Pin the boundary: read AT exactly `last + TIMEOUT` is in-window;
        // read at `last + TIMEOUT + 1` is stale. This guards a future
        // off-by-one change to the comparison operator.
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::Nominal);
        assert_eq!(
            t.current_posture(1_000 + POSTURE_STALENESS_TIMEOUT_MS),
            FleetPosture::Nominal,
            "boundary read at exactly the timeout must still be in-window"
        );
        assert_eq!(
            t.current_posture(1_000 + POSTURE_STALENESS_TIMEOUT_MS + 1),
            FleetPosture::Degraded,
            "boundary read 1ms past the timeout must be stale"
        );
    }

    #[test]
    fn tracker_locked_out_sticky_through_staleness() {
        // LockedOut event then timeout: the staleness path must NOT
        // relax to Degraded — LockedOut is the most-restrictive
        // posture, and we stay there until an explicit recovery event.
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::LockedOut);
        let stale_at = 1_000 + POSTURE_STALENESS_TIMEOUT_MS + 100_000;
        assert_eq!(t.current_posture(stale_at), FleetPosture::LockedOut,
            "LockedOut must stick through a staleness timeout (sticky-toward-safe)");
    }

    #[test]
    fn tracker_locked_out_recovers_on_explicit_nominal_event() {
        // The only release from sticky-LockedOut is an explicit
        // non-LockedOut observation from the source.
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::LockedOut);
        assert_eq!(t.current_posture(2_000), FleetPosture::LockedOut);
        t.observe(3_000, FleetPosture::Nominal);
        assert_eq!(t.current_posture(3_000), FleetPosture::Nominal,
            "explicit Nominal observation must release sticky-LockedOut");

        // Now a follow-on staleness should derate Nominal → Degraded,
        // not stay at LockedOut (the sticky flag must have cleared).
        let stale_at = 3_000 + POSTURE_STALENESS_TIMEOUT_MS + 1;
        assert_eq!(t.current_posture(stale_at), FleetPosture::Degraded,
            "post-release staleness derates Nominal → Degraded, not LockedOut");
    }

    #[test]
    fn tracker_locked_out_release_via_degraded_event_also_clears_sticky() {
        // Recovery to Degraded (rather than Nominal) also clears
        // sticky-LockedOut — the flag tracks "last observation was
        // LockedOut", not "ever saw LockedOut".
        let mut t = PostureTracker::with_source();
        t.observe(1_000, FleetPosture::LockedOut);
        t.observe(2_000, FleetPosture::Degraded);
        assert_eq!(t.current_posture(2_000), FleetPosture::Degraded);
    }
}
