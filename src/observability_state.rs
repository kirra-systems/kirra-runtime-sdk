//! ADR-0035 Stage 3 (slice 3j) — the fleet-observability registries, lifted off
//! the `AppState` god-object into a cohesive field façade (the 3c/3e/3f/3g/3h/3i
//! pattern), byte-identical.
//!
//! Like the writer-handle slice (3g), these two are pure off-verdict-path plumbing
//! — lock-free counters the binary's `GET /metrics` scrape reads, NEVER a
//! safety-decision surface the posture engine / supervisor / fence gates on:
//!
//! - `fleet_metrics` — WS-0.5 fleet-safety Prometheus counters (posture
//!   transitions, gate denials, HA promotions) + the WP-05 (G-10) request /
//!   actuator-envelope latency histograms. Incremented on the observed paths,
//!   never gating them. `AtomicU64`/histogram interior mutability shared via the
//!   outer `Arc<AppState>`.
//! - `deadline_registry` — WP-20 (G-11) per-task deadline-miss counters for the
//!   supervised loops that declare a `deadline_ms` budget in
//!   `execution_manager::TASK_MANIFEST` (today: the telemetry watchdog). The task
//!   loop records each cycle; `GET /metrics` exports `kirra_task_deadline_*`.
//!   Observability only.
//!
//! They are therefore grouped in a ROOT-crate leaf (not `kirra-safety-authority`,
//! whose deliberately std-only-plus-crypto character keeps it Kani/loom/MSRV-clean).
//! The move adds NO dependency edge: `FleetSafetyMetrics` (`crate::metrics`) and
//! `DeadlineRegistry` (`crate::execution_manager`) already live in the root tree.
//! Embedded on `AppState` as `app.observability`; every field is interior-mutable
//! (`AtomicU64` / `Arc<DeadlineRegistry>`), so the move is pure relocation — no
//! `&mut self`, no ordering change, no behaviour change.

use std::sync::Arc;

use crate::execution_manager::{DeadlineRegistry, TASK_MANIFEST};
use crate::metrics::FleetSafetyMetrics;

/// The fleet-observability registries (ADR-0035 slice 3j). Neither ever gates the
/// verdict path; both are read only by the `GET /metrics` scrape and incremented
/// on the observed paths.
#[derive(Debug)]
pub struct ObservabilityState {
    /// WS-0.5 — fleet-safety Prometheus counters (posture transitions, gate
    /// denials, HA promotions) + WP-05 (G-10) request/actuator-envelope latency
    /// histograms, exported by `GET /metrics`. Lock-free; incremented on the
    /// observed paths, never gating them. Lives on `AppState` (not `ServiceState`)
    /// so the posture engine, the routing gate, and the HA promotion path can all
    /// reach it. Field semantics UNCHANGED from the prior `app.fleet_metrics`.
    pub fleet_metrics: FleetSafetyMetrics,
    /// WP-20 (G-11) per-task deadline-miss counters for the supervised loops that
    /// declare a `deadline_ms` budget in `execution_manager::TASK_MANIFEST` (today:
    /// the telemetry watchdog). The task loop records each cycle; `GET /metrics`
    /// exports `kirra_task_deadline_*`. Lock-free; observability only. Field
    /// semantics UNCHANGED from the prior `app.deadline_registry`.
    pub deadline_registry: Arc<DeadlineRegistry>,
}

impl ObservabilityState {
    /// Construct the observability registries — byte-identical to the prior two
    /// inline field initializers in `AppState::new` (`FleetSafetyMetrics::new()` +
    /// `Arc::new(DeadlineRegistry::from_manifest(TASK_MANIFEST))`).
    pub fn new() -> Self {
        Self {
            fleet_metrics: FleetSafetyMetrics::new(),
            deadline_registry: Arc::new(DeadlineRegistry::from_manifest(TASK_MANIFEST)),
        }
    }
}

impl Default for ObservabilityState {
    fn default() -> Self {
        Self::new()
    }
}
