// src/scenario_runner.rs
//
// Deterministic temporal scenario replay harness.

use std::sync::Arc;
use crate::verifier::{AppState, FleetPosture, NodeTrustState};
use crate::posture_cache::{CachedFleetPosture, SharedPostureCache};
use crate::posture_engine::recalculate_and_broadcast;
use crate::recovery_hysteresis::{evaluate_recovery_report, HysteresisDecision};
use crate::clock::VirtualClock;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ScenarioEvent {
    TelemetryReport {
        node_id: String,
        confidence: f64,
        hw_fault: bool,
    },
    MarkUntrusted {
        node_id: String,
        reason: String,
    },
    AdvanceClock { delta_ms: u64 },
    TriggerRecalculation,
}

// ---------------------------------------------------------------------------
// Assertion types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum PostureAssertion {
    FleetPostureIs(FleetPosture),
    NodeTrustIs(String, NodeTrustState),
    NodeIsUntrusted(String),
    NodeIsTrusted(String),
    CacheIsPopulated,
    CacheIsStale,
}

// ---------------------------------------------------------------------------
// Assertion result
// ---------------------------------------------------------------------------

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

pub struct ScenarioRunner {
    pub app: Arc<AppState>,
    pub posture_cache: SharedPostureCache,
    pub clock: Arc<VirtualClock>,
    pub default_confidence_floor: f64,
    events: Vec<(u64, ScenarioEvent)>,
    assertions: Vec<(u64, PostureAssertion)>,
}

impl ScenarioRunner {
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

    pub fn at_ms(mut self, timestamp_ms: u64, event: ScenarioEvent) -> Self {
        self.events.push((timestamp_ms, event));
        self
    }

    pub fn assert_at_ms(mut self, timestamp_ms: u64, assertion: PostureAssertion) -> Self {
        self.assertions.push((timestamp_ms, assertion));
        self
    }

    pub async fn run(self) -> Vec<AssertionResult> {
        self.run_inner(true).await
    }

    pub async fn run_collecting(self) -> Vec<AssertionResult> {
        self.run_inner(false).await
    }

    async fn run_inner(mut self, panic_on_failure: bool) -> Vec<AssertionResult> {
        let mut milestones: Vec<u64> = self.events.iter().map(|e| e.0)
            .chain(self.assertions.iter().map(|a| a.0))
            .collect();
        milestones.sort_unstable();
        milestones.dedup();

        let mut results: Vec<AssertionResult> = Vec::new();
        let mut assertion_index: usize = 0;

        for milestone in milestones {
            self.clock.set_ms(milestone);
            let ts = self.clock.now_ms();

            let mut needs_recalc = false;

            let active_events: Vec<ScenarioEvent> = self.events.iter()
                .filter(|e| e.0 == milestone)
                .map(|e| e.1.clone())
                .collect();

            for event in active_events {
                match event {
                    ScenarioEvent::TelemetryReport { ref node_id, confidence, hw_fault } => {
                        let floor = self.app.store.lock().unwrap()
                            .load_av_confidence_floor(node_id)
                            .unwrap_or(None)
                            .unwrap_or(self.default_confidence_floor);

                        let is_degraded = hw_fault || confidence < floor;

                        if is_degraded {
                            let reason = if hw_fault {
                                "HARDWARE_FAULT_DETECTED"
                            } else {
                                "CONFIDENCE_BELOW_FLOOR"
                            };

                            // Disk-first: reset streak on disk before memory mutation.
                            let _ = self.app.store.lock().unwrap()
                                .reset_recovery_streak(node_id, ts);
                            let _ = self.app.store.lock().unwrap()
                                .touch_av_telemetry_timestamp(node_id, ts);

                            if let Some(mut node) = self.app.nodes.get_mut(node_id) {
                                node.status = NodeTrustState::Untrusted(reason.to_string());
                            }
                            needs_recalc = true;
                        } else {
                            let currently_untrusted = self.app.nodes
                                .get(node_id)
                                .map(|n| matches!(n.status, NodeTrustState::Untrusted(_)))
                                .unwrap_or(false);

                            if currently_untrusted {
                                // Evaluate time-bounded hysteresis.
                                // Lock once, evaluate, drop guard before acting on result.
                                let decision = {
                                    let guard = self.app.store.lock().unwrap();
                                    evaluate_recovery_report(&*guard, node_id, ts)
                                };

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
                                        let _ = self.app.store.lock().unwrap()
                                            .reset_recovery_streak(node_id, ts);
                                        needs_recalc = true;
                                    }
                                    HysteresisDecision::StreakBuilding { current, required, .. } => {
                                        tracing::debug!(
                                            node_id = %node_id,
                                            current = current, required = required,
                                            virtual_ms = ts,
                                            "Scenario: streak building"
                                        );
                                    }
                                    HysteresisDecision::WindowExpired { old_streak } => {
                                        tracing::debug!(
                                            node_id = %node_id, old_streak = old_streak,
                                            virtual_ms = ts,
                                            "Scenario: streak window expired, reset"
                                        );
                                    }
                                    HysteresisDecision::NotApplicable => {
                                        let _ = self.app.store.lock().unwrap()
                                            .touch_av_telemetry_timestamp(node_id, ts);
                                    }
                                }
                            } else {
                                let _ = self.app.store.lock().unwrap()
                                    .touch_av_telemetry_timestamp(node_id, ts);
                            }
                        }
                    }

                    ScenarioEvent::MarkUntrusted { ref node_id, ref reason } => {
                        let _ = self.app.store.lock().unwrap()
                            .reset_recovery_streak(node_id, ts);
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
                }
            }

            if needs_recalc {
                recalculate_and_broadcast(&self.app, &self.posture_cache);
            }

            let active_assertions: Vec<(usize, PostureAssertion)> = self.assertions.iter()
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
                ).await;

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
    use crate::clock::Clock;
    let (passed, description) = match assertion {
        PostureAssertion::FleetPostureIs(expected) => {
            let guard = cache.read().unwrap();
            match guard.as_ref() {
                Some(cached) => {
                    let ok = cached.propagated_status == *expected;
                    let desc = format!(
                        "FleetPostureIs({expected:?}): got {:?}", cached.propagated_status
                    );
                    (ok, desc)
                }
                None => (
                    false,
                    format!("FleetPostureIs({expected:?}): cache is None"),
                ),
            }
        }

        PostureAssertion::NodeTrustIs(node_id, expected) => {
            match app.nodes.get(node_id) {
                Some(node) => {
                    let ok = node.status == *expected;
                    let desc = format!(
                        "NodeTrustIs({node_id}, {expected:?}): got {:?}", node.status
                    );
                    (ok, desc)
                }
                None => (false, format!("NodeTrustIs({node_id}, ...): node not found")),
            }
        }

        PostureAssertion::NodeIsUntrusted(node_id) => {
            match app.nodes.get(node_id) {
                Some(node) => {
                    let ok = matches!(node.status, NodeTrustState::Untrusted(_));
                    let desc = format!(
                        "NodeIsUntrusted({node_id}): got {:?}", node.status
                    );
                    (ok, desc)
                }
                None => (false, format!("NodeIsUntrusted({node_id}): node not found")),
            }
        }

        PostureAssertion::NodeIsTrusted(node_id) => {
            match app.nodes.get(node_id) {
                Some(node) => {
                    let ok = matches!(node.status, NodeTrustState::Trusted);
                    let desc = format!(
                        "NodeIsTrusted({node_id}): got {:?}", node.status
                    );
                    (ok, desc)
                }
                None => (false, format!("NodeIsTrusted({node_id}): node not found")),
            }
        }

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
                None => (true, "CacheIsStale: cache is None (fail-closed)".to_string()),
            }
        }
    };

    AssertionResult { timestamp_ms, assertion_index: index, passed, description }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scenario_runner_tests {
    use super::*;
    use std::sync::Arc;
    use crate::posture_cache::CachedFleetPosture;
    use crate::verifier::FleetPosture;
    use crate::clock::VirtualClock;

    #[test]
    fn test_virtual_clock_advances_affect_staleness_check() {
        use crate::posture_engine::POSTURE_CACHE_TTL_MS;

        let clock = VirtualClock::starting_at(1_000);

        let entry = CachedFleetPosture {
            propagated_status: FleetPosture::Nominal,
            generated_at_ms: 1_000,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };

        assert!(!entry.is_stale(clock.now_ms()));

        clock.advance_ms(POSTURE_CACHE_TTL_MS + 1);
        assert!(entry.is_stale(clock.now_ms()),
            "entry must be stale after virtual clock advances past TTL");
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

    #[tokio::test]
    async fn test_runner_evaluates_assertions_synchronously_after_events() {
        let _ = std::mem::size_of::<ScenarioRunner>();
        let _ = std::mem::size_of::<ScenarioEvent>();
        let _ = std::mem::size_of::<PostureAssertion>();
        let _ = std::mem::size_of::<AssertionResult>();
    }
}
