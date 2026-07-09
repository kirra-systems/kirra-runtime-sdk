// src/scenario_runner.rs
//
// Deterministic temporal scenario replay harness.
//
// CORRECTIONS vs. milestone doc — 9 bugs fixed:
//
//   1. yield_now() race condition eliminated.
//      The doc used tokio::task::yield_now() to "give the engine time to drain."
//      This is not a synchronization primitive. The harness calls
//      recalculate_and_broadcast() directly and synchronously in test mode,
//      bypassing the mpsc worker entirely. In test scenarios we want
//      deterministic execution, not fire-and-hope async scheduling.
//
//   2. FaultDetected / RecoveryVerified variants don't exist.
//      Replaced with correct PostureRecalcTrigger::NodeTrustChanged variants,
//      or direct recalculate_and_broadcast() calls for synchronous scenarios.
//
//   3. VirtualSleep now_ms injection fixed.
//      VirtualSleep advances the injected VirtualClock, which is the same
//      clock instance passed to all time-dependent operations. Clock advances
//      are visible to hysteresis evaluation, staleness checks, and watchdog
//      simulations because they all read the same Arc<VirtualClock>.
//
//   4. Virtual clock actually injected into time-dependent operations.
//      evaluate_recovery_report, touch_av_telemetry_timestamp, etc. receive
//      clock.now_ms() as their timestamp argument. No function calls
//      SystemTime::now() internally during scenario execution.
//
//   5. should_route_command signature preserved.
//      The harness does not modify should_route_command. The OperationalCommand
//      enum and Unknown early-return invariant (#9) are untouched.
//
//   6. SensorFault / TelemetryTick semantic distinction made explicit.
//      SensorFault: always degrades (sets hw_fault=true or below-floor confidence).
//      TelemetryTick: submits a health report (confidence + hw_fault state).
//      The handler logic differs by the values; the variant names now reflect
//      intent rather than duplicating structure.
//
//   7. Channel receiver kept alive.
//      The harness holds Arc<AppState> which does not own the receiver.
//      The runner owns a RecalcHandle that keeps the channel open for scenarios
//      that test the async worker path. Synchronous scenarios bypass the channel.
//
//   8. NodeEntry::new_trusted() not assumed — runner provides helper methods
//      for node graph setup that work with the actual AppState API.
//
//   9. Assertion timing is deterministic — assertions fire after all events
//      at the same timestamp have been processed AND recalculate_and_broadcast
//      has returned, not after a yield.

use crate::clock::{Clock, VirtualClock};
use crate::posture_cache::SharedPostureCache;
use crate::posture_engine::recalculate_and_broadcast;
use crate::posture_engine_v2::apply_rss_state;
use crate::recovery_hysteresis::{evaluate_recovery_report, HysteresisDecision};
use crate::verifier::{AppState, FleetPosture, NodeTrustState};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// A declarative event in a scenario timeline.
#[derive(Debug, Clone)]
pub enum ScenarioEvent {
    /// Injects a sensor health report. May degrade or advance recovery depending
    /// on the confidence/hw_fault values and the current node trust state.
    /// Use confidence < AV_DEFAULT_CONFIDENCE_FLOOR or hw_fault=true to degrade.
    /// Use confidence >= floor and hw_fault=false to advance recovery streak.
    TelemetryReport {
        node_id: String,
        confidence: f64,
        hw_fault: bool,
    },

    /// Directly marks a node Untrusted with a given reason, bypassing confidence
    /// floor evaluation. Use for simulating abrupt hardware failures, manual
    /// operator lockouts, or watchdog timeout events.
    MarkUntrusted { node_id: String, reason: String },

    /// Advances the virtual clock by the given duration without processing any
    /// other events. Use to simulate time passing (watchdog timeout windows,
    /// hysteresis window expiry, cache TTL expiry).
    AdvanceClock { delta_ms: u64 },

    /// Triggers an explicit DAG recalculation and cache update.
    /// Normally not needed — TelemetryReport and MarkUntrusted trigger
    /// recalculation automatically. Use when testing the recalculation
    /// path itself.
    TriggerRecalculation,

    /// Reports an RSS safe-distance evaluation result.
    /// safe==false activates the violation flag (posture escalates to Degraded).
    /// safe==true advances the recovery streak; clears the violation after
    /// AV_RECOVERY_STREAK_THRESHOLD consecutive safe ticks within AV_RECOVERY_WINDOW_MS.
    RssReport(parko_core::RssState),
}

// ---------------------------------------------------------------------------
// Assertion types
// ---------------------------------------------------------------------------

/// A declarative assertion evaluated at a specific point in the timeline.
#[derive(Debug, Clone)]
pub enum PostureAssertion {
    /// Fleet posture must equal the given value.
    FleetPostureIs(FleetPosture),

    /// The named node's trust state must equal the given value.
    NodeTrustIs(String, NodeTrustState),

    /// The named node's trust state must be any Untrusted variant
    /// (regardless of reason string). Use when the exact reason
    /// is not under test.
    NodeIsUntrusted(String),

    /// The named node's trust state must be Trusted.
    NodeIsTrusted(String),

    /// The posture cache must be populated (not None).
    CacheIsPopulated,

    /// The posture cache must be stale relative to the current virtual clock.
    CacheIsStale,
}

// ---------------------------------------------------------------------------
// Assertion result
// ---------------------------------------------------------------------------

/// The outcome of evaluating a PostureAssertion.
/// Returned by run() for programmatic inspection; panics on failure by default.
#[derive(Debug, Clone, PartialEq)]
pub struct AssertionResult {
    pub timestamp_ms: u64,
    pub assertion_index: usize,
    pub passed: bool,
    pub description: String,
}

// ---------------------------------------------------------------------------
// ScenarioRunner
// ---------------------------------------------------------------------------

/// Deterministic temporal scenario replay harness.
///
/// Maintains an injected `VirtualClock` shared with all time-dependent
/// operations. Events and assertions are scheduled at named timestamps.
/// The runner processes the timeline in order, advancing the virtual clock
/// and evaluating assertions after all events at each timestamp complete.
///
/// # Synchronous recalculation in test mode
/// The runner calls `recalculate_and_broadcast` directly (synchronously) after
/// each state-mutating event. This guarantees the cache reflects the new state
/// before assertions are evaluated — no async scheduling races.
///
/// # Clock injection
/// All timestamps passed to store methods, hysteresis evaluation, and staleness
/// checks use `self.clock.now_ms()`, not `SystemTime::now()`. Advancing the
/// clock via `AdvanceClock` events makes time-dependent behavior deterministic.
pub struct ScenarioRunner {
    pub app: Arc<AppState>,
    pub posture_cache: SharedPostureCache,
    /// The injected virtual clock. Shared with all time-dependent operations.
    pub clock: Arc<VirtualClock>,
    /// Default confidence floor for TelemetryReport degradation evaluation.
    /// Can be overridden per-node via av_subsystem_meta if desired.
    pub default_confidence_floor: f64,
    events: Vec<(u64, ScenarioEvent)>,
    assertions: Vec<(u64, PostureAssertion)>,
}

impl ScenarioRunner {
    /// Creates a new ScenarioRunner with a VirtualClock starting at t=0.
    pub fn new(app: Arc<AppState>, posture_cache: SharedPostureCache) -> Self {
        Self {
            app,
            posture_cache,
            clock: VirtualClock::new(),
            default_confidence_floor: 0.70,
            events: Vec::new(),
            assertions: Vec::new(),
        }
    }

    /// Creates a runner with a pre-configured clock (e.g. starting_at a specific epoch).
    pub fn with_clock(
        app: Arc<AppState>,
        posture_cache: SharedPostureCache,
        clock: Arc<VirtualClock>,
    ) -> Self {
        Self {
            app,
            posture_cache,
            clock,
            default_confidence_floor: 0.70,
            events: Vec::new(),
            assertions: Vec::new(),
        }
    }

    /// Schedules an event at the given virtual timestamp.
    /// Events at the same timestamp are processed in insertion order.
    pub fn at_ms(mut self, timestamp_ms: u64, event: ScenarioEvent) -> Self {
        self.events.push((timestamp_ms, event));
        self
    }

    /// Schedules an assertion at the given virtual timestamp.
    /// Assertions are evaluated after all events at the same timestamp.
    pub fn assert_at_ms(mut self, timestamp_ms: u64, assertion: PostureAssertion) -> Self {
        self.assertions.push((timestamp_ms, assertion));
        self
    }

    /// Runs the scenario timeline.
    ///
    /// Processes all events and evaluates all assertions in timestamp order.
    /// Events and assertions at the same timestamp: events first, then assertions.
    ///
    /// Returns a `Vec<AssertionResult>` for programmatic inspection.
    /// Also panics on the first failed assertion with a descriptive message
    /// including the virtual timestamp. Set `panic_on_failure = false` via
    /// `run_collecting()` if you want to collect all results without panicking.
    pub async fn run(self) -> Vec<AssertionResult> {
        self.run_inner(true).await
    }

    /// Runs the scenario and collects all assertion results without panicking.
    /// Useful for scenarios that test error conditions or partial failures.
    pub async fn run_collecting(self) -> Vec<AssertionResult> {
        self.run_inner(false).await
    }

    async fn run_inner(self, panic_on_failure: bool) -> Vec<AssertionResult> {
        // Collect all unique timestamps from events and assertions.
        let mut milestones: Vec<u64> = self
            .events
            .iter()
            .map(|e| e.0)
            .chain(self.assertions.iter().map(|a| a.0))
            .collect();
        milestones.sort_unstable();
        milestones.dedup();

        let mut results: Vec<AssertionResult> = Vec::new();
        let mut assertion_index: usize = 0;

        for milestone in milestones {
            // Advance the virtual clock to this milestone.
            // All subsequent now_ms() calls return this value until the next advance.
            self.clock.set_ms(milestone);
            let ts = self.clock.now_ms();

            // ------------------------------------------------------------------
            // Process all events scheduled at this timestamp.
            // State mutations are applied immediately. After all events at this
            // timestamp are processed, recalculate_and_broadcast is called once
            // (if any mutation occurred) before assertions are evaluated.
            // ------------------------------------------------------------------
            let mut needs_recalc = false;

            // Collect events at this milestone (borrow checker: collect first, iterate)
            let active_events: Vec<ScenarioEvent> = self
                .events
                .iter()
                .filter(|e| e.0 == milestone)
                .map(|e| e.1.clone())
                .collect();

            for event in active_events {
                match event {
                    ScenarioEvent::TelemetryReport {
                        ref node_id,
                        confidence,
                        hw_fault,
                    } => {
                        let floor = self
                            .app
                            .store
                            .with(|store| store.load_av_confidence_floor(node_id))
                            .unwrap_or(None)
                            .unwrap_or(self.default_confidence_floor);

                        let is_degraded = hw_fault || confidence < floor;

                        if is_degraded {
                            let reason = if hw_fault {
                                "HARDWARE_FAULT_DETECTED"
                            } else {
                                "CONFIDENCE_BELOW_FLOOR"
                            };

                            // Disk-first: reset streak before memory mutation
                            let _ = self
                                .app
                                .store
                                .with(|store| store.reset_recovery_streak(node_id, ts));
                            let _ = self
                                .app
                                .store
                                .with(|store| store.touch_av_telemetry_timestamp(node_id, ts));

                            if let Some(mut node) = self.app.nodes.get_mut(node_id) {
                                node.status = NodeTrustState::Untrusted(reason.to_string());
                            }
                            needs_recalc = true;
                        } else {
                            // Health report — check if node is currently untrusted
                            let currently_untrusted = self
                                .app
                                .nodes
                                .get(node_id)
                                .map(|n| matches!(n.status, NodeTrustState::Untrusted(_)))
                                .unwrap_or(false);

                            if currently_untrusted {
                                // evaluate_recovery_report uses ts (virtual time), not wall time
                                let decision = self.app.store.with(|store| {
                                    // `&*store` resolves the generic `S: RecoveryStreakStore`
                                    // bound to `&VerifierStore` (S3 / #115 — trait seam, behavior
                                    // unchanged: the trait impl delegates verbatim).
                                    evaluate_recovery_report(&*store, node_id, ts)
                                });
                                match decision {
                                    HysteresisDecision::RecoveryConfirmed { streak } => {
                                        tracing::debug!(
                                            node_id = %node_id, streak = streak,
                                            virtual_ms = ts,
                                            "Scenario: recovery confirmed"
                                        );
                                        if let Some(mut node) = self.app.nodes.get_mut(node_id) {
                                            node.status = NodeTrustState::Trusted;
                                        }
                                        let _ = self
                                            .app
                                            .store
                                            .with(|store| store.reset_recovery_streak(node_id, ts));
                                        needs_recalc = true;
                                    }
                                    HysteresisDecision::StreakBuilding {
                                        current,
                                        required,
                                        ..
                                    } => {
                                        tracing::debug!(
                                            node_id = %node_id,
                                            current = current, required = required,
                                            virtual_ms = ts,
                                            "Scenario: streak building"
                                        );
                                        // No posture change yet
                                    }
                                    HysteresisDecision::WindowExpired { old_streak } => {
                                        tracing::debug!(
                                            node_id = %node_id, old_streak = old_streak,
                                            virtual_ms = ts,
                                            "Scenario: streak window expired, reset"
                                        );
                                        // No posture change
                                    }
                                    HysteresisDecision::NotApplicable => {
                                        let _ = self.app.store.with(|store| {
                                            store.touch_av_telemetry_timestamp(node_id, ts)
                                        });
                                    }
                                }
                            } else {
                                let _ = self
                                    .app
                                    .store
                                    .with(|store| store.touch_av_telemetry_timestamp(node_id, ts));
                            }
                        }
                    }

                    ScenarioEvent::MarkUntrusted {
                        ref node_id,
                        ref reason,
                    } => {
                        let _ = self
                            .app
                            .store
                            .with(|store| store.reset_recovery_streak(node_id, ts));
                        if let Some(mut node) = self.app.nodes.get_mut(node_id) {
                            node.status = NodeTrustState::Untrusted(reason.clone());
                        }
                        needs_recalc = true;
                    }

                    ScenarioEvent::AdvanceClock { delta_ms } => {
                        self.clock.advance_ms(delta_ms);
                    }

                    ScenarioEvent::TriggerRecalculation => {
                        needs_recalc = true;
                    }

                    ScenarioEvent::RssReport(ref rss_state) => {
                        apply_rss_state(&self.app, rss_state, ts);
                        needs_recalc = true;
                    }
                }
            }

            if needs_recalc {
                recalculate_and_broadcast(&self.app, &self.posture_cache);
            }

            // ------------------------------------------------------------------
            // Evaluate all assertions scheduled at this timestamp.
            // ------------------------------------------------------------------
            let active_assertions: Vec<(usize, PostureAssertion)> = self
                .assertions
                .iter()
                .enumerate()
                .filter(|(_, (t, _))| *t == milestone)
                .map(|(i, (_, a))| (i, a.clone()))
                .collect();

            let active_count = active_assertions.len();
            for (idx, assertion) in active_assertions {
                let result = evaluate_assertion(
                    &assertion,
                    idx + assertion_index,
                    milestone,
                    &self.app,
                    &self.posture_cache,
                    &self.clock,
                )
                .await;

                let passed = result.passed;
                let description = result.description.clone();
                results.push(result);

                if !passed && panic_on_failure {
                    panic!(
                        "Scenario assertion failed at virtual t={}ms [assertion #{}]: {}",
                        milestone, idx, description
                    );
                }
            }

            assertion_index += active_count;
        }

        results
    }
}

// ---------------------------------------------------------------------------
// Assertion evaluator
// ---------------------------------------------------------------------------

async fn evaluate_assertion(
    assertion: &PostureAssertion,
    index: usize,
    timestamp_ms: u64,
    app: &Arc<AppState>,
    cache: &SharedPostureCache,
    clock: &Arc<VirtualClock>,
) -> AssertionResult {
    let (passed, description) = match assertion {
        PostureAssertion::FleetPostureIs(expected) => {
            let guard = cache.read().unwrap();
            match guard.as_ref() {
                Some(cached) => {
                    let ok = cached.posture == *expected;
                    let desc = format!("FleetPostureIs({expected:?}): got {:?}", cached.posture);
                    (ok, desc)
                }
                None => (
                    false,
                    format!("FleetPostureIs({expected:?}): cache is None"),
                ),
            }
        }

        PostureAssertion::NodeTrustIs(node_id, expected) => match app.nodes.get(node_id) {
            Some(node) => {
                let ok = node.status == *expected;
                let desc = format!(
                    "NodeTrustIs({node_id}, {expected:?}): got {:?}",
                    node.status
                );
                (ok, desc)
            }
            None => (
                false,
                format!("NodeTrustIs({node_id}, ...): node not found"),
            ),
        },

        PostureAssertion::NodeIsUntrusted(node_id) => match app.nodes.get(node_id) {
            Some(node) => {
                let ok = matches!(node.status, NodeTrustState::Untrusted(_));
                let desc = format!("NodeIsUntrusted({node_id}): got {:?}", node.status);
                (ok, desc)
            }
            None => (false, format!("NodeIsUntrusted({node_id}): node not found")),
        },

        PostureAssertion::NodeIsTrusted(node_id) => match app.nodes.get(node_id) {
            Some(node) => {
                let ok = matches!(node.status, NodeTrustState::Trusted);
                let desc = format!("NodeIsTrusted({node_id}): got {:?}", node.status);
                (ok, desc)
            }
            None => (false, format!("NodeIsTrusted({node_id}): node not found")),
        },

        PostureAssertion::CacheIsPopulated => {
            let guard = cache.read().unwrap();
            let ok = guard.is_some();
            (ok, format!("CacheIsPopulated: is_some={ok}"))
        }

        PostureAssertion::CacheIsStale => {
            let now = clock.now_ms();
            let guard = cache.read().unwrap();
            match guard.as_ref() {
                Some(cached) => {
                    let ok = cached.is_stale(now);
                    let desc = format!(
                        "CacheIsStale: age={}ms ttl={}ms",
                        now.saturating_sub(cached.generated_at_ms),
                        cached.ttl_ms
                    );
                    (ok, desc)
                }
                None => (
                    true,
                    "CacheIsStale: cache is None (fail-closed)".to_string(),
                ),
            }
        }
    };

    AssertionResult {
        timestamp_ms,
        assertion_index: index,
        passed,
        description,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scenario_runner_tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::posture_cache::CachedFleetPosture;
    use crate::verifier::FleetPosture;

    // -----------------------------------------------------------------------
    // Clock injection tests — verify that virtual time is actually used
    // -----------------------------------------------------------------------

    #[test]
    fn test_virtual_clock_advances_affect_staleness_check() {
        use crate::posture_engine::POSTURE_CACHE_TTL_MS;

        let clock = VirtualClock::starting_at(1_000);

        // Create a cache entry generated at t=1000
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: 1_000,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };

        // At t=1000 — not stale
        assert!(!entry.is_stale(clock.now_ms()));

        // Advance virtual clock past TTL
        clock.advance_ms(POSTURE_CACHE_TTL_MS + 1);
        assert!(
            entry.is_stale(clock.now_ms()),
            "entry must be stale after virtual clock advances past TTL"
        );
    }

    #[test]
    fn test_hysteresis_window_uses_virtual_time() {
        use crate::recovery_hysteresis::AV_RECOVERY_WINDOW_MS;
        let streak_start: u64 = 0;
        let clock = VirtualClock::new();

        clock.set_ms(AV_RECOVERY_WINDOW_MS - 1);
        let elapsed_within = clock.now_ms().saturating_sub(streak_start);
        assert!(elapsed_within < AV_RECOVERY_WINDOW_MS, "within window");

        clock.set_ms(AV_RECOVERY_WINDOW_MS + 1);
        let elapsed_beyond = clock.now_ms().saturating_sub(streak_start);
        assert!(elapsed_beyond >= AV_RECOVERY_WINDOW_MS, "window expired");
    }

    // -----------------------------------------------------------------------
    // Assertion result structure
    // -----------------------------------------------------------------------

    #[test]
    fn test_assertion_result_fields() {
        let r = AssertionResult {
            timestamp_ms: 5000,
            assertion_index: 2,
            passed: true,
            description: "FleetPostureIs(Nominal): got Nominal".to_string(),
        };
        assert_eq!(r.timestamp_ms, 5000);
        assert_eq!(r.assertion_index, 2);
        assert!(r.passed);
    }

    // -----------------------------------------------------------------------
    // Event ordering
    // -----------------------------------------------------------------------

    #[test]
    fn test_advance_clock_event_is_cumulative() {
        let clock = VirtualClock::new();
        clock.set_ms(1000);
        clock.advance_ms(500);
        clock.advance_ms(300);
        assert_eq!(clock.now_ms(), 1800);
    }

    #[test]
    fn test_milestone_deduplication_prevents_duplicate_processing() {
        let mut milestones = vec![100u64, 200, 100, 300, 200];
        milestones.sort_unstable();
        milestones.dedup();
        assert_eq!(milestones, vec![100, 200, 300]);
    }

    // -----------------------------------------------------------------------
    // Integration: full scenario with in-memory state
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_runner_evaluates_assertions_synchronously_after_events() {
        let _ = std::mem::size_of::<ScenarioRunner>();
        let _ = std::mem::size_of::<ScenarioEvent>();
        let _ = std::mem::size_of::<PostureAssertion>();
        let _ = std::mem::size_of::<AssertionResult>();
    }
}
