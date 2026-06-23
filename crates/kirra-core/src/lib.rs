//! **kirra-core** — the lean safety/contract foundation of the Kirra stack.
//!
//! This crate carries the dependency-light types and (in later de-monolith stages) the
//! governor / kinematics-contract talisman, extracted from `kirra-runtime-sdk` so that
//! the certified-governor surface does not pull the verifier service's heavy tree
//! (tokio / axum / rusqlite / reqwest) or any ML backend. Consumers that only need the
//! foundation (the ROS2 adapter, the planner, the lane map, parko) depend on this crate
//! directly; `kirra-runtime-sdk` re-exports everything here so existing paths are
//! unchanged.
//!
//! Stage 1: the fleet posture / node trust types, previously defined in the heavy
//! `kirra_verifier::verifier` module and imported across the whole stack.

// Mirror the parent crate's doc-lint posture (`kirra_verifier` lib root) so the
// verbatim-relocated modules (the kinematics-contract talisman, the SG2 containment
// checker) keep their byte-identical doc comments — the safety-derivation tables use
// aligned arithmetic continuations that these two pedantic doc lints would otherwise
// reject. No logic, no behavior — purely the same lint allowance traveling with the code.
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

use serde::{Deserialize, Serialize};

/// The FROZEN kinematics-contract talisman — the deterministic vehicle flight-envelope
/// safety contract (`EnforceAction` / `DenyCode` / `VehicleKinematicsContract` /
/// `validate_vehicle_command`). Relocated here verbatim (de-monolith Stage 3); re-exported
/// by `kirra_verifier::gateway::kinematics_contract` so every existing path holds.
pub mod kinematics_contract;

/// The SG2 drivable-space containment checker — the per-trajectory corridor-containment
/// sibling of `validate_vehicle_command` (`VehicleFootprint` / `Corridor` / `Pose` /
/// `validate_trajectory_containment` / `MAX_TRAJECTORY_HORIZON`). Relocated here verbatim
/// (de-monolith Stage 4); re-exported by `kirra_verifier::gateway::containment` so every
/// existing path (the ROS2 adapter, the planner) holds.
pub mod containment;

/// The shared non-finite governor-input guard (`all_finite`, the #410 convergence point).
/// A dependency-free predicate every governor entry calls. Relocated here verbatim
/// (de-monolith Stage 5); re-exported by `kirra_verifier::governor_guard` so the
/// SDK's internal callers (the scalar kernel, the C FFI) and parko's diverse governor hold.
pub mod governor_guard;

/// The fail-closed fleet-posture state machine (`PostureTracker` /
/// `POSTURE_STALENESS_TIMEOUT_MS`) — a pure, deterministic, clock-injected machine whose
/// only input is `FleetPosture`. Relocated here verbatim (de-monolith Stage 5); re-exported
/// by `kirra_verifier::posture_tracker` so the ROS2 adapter and the parko-ros2 node hold.
pub mod posture_tracker;

/// The lean drivable-corridor seam (`Point` / `CorridorSource` / `MockCorridorSource`).
/// Relocated verbatim from the kirra-ros2-adapter (de-monolith Stage 6a) so the shared
/// lane map (kirra-map) and the planner can build on it without the heavy adapter; the
/// adapter re-exports it (and the feature-gated Lanelet2 cxx source impls the trait).
pub mod corridor;

/// The lean trajectory/perception data types (`Pose` / `TrajectoryPoint` /
/// `TrajectoryVerdict` / `PerceivedObject`) — pure plain-old-data the planner and the
/// shared lane map consume. Relocated verbatim from the kirra-ros2-adapter `state` module
/// (de-monolith Stage 6a); the adapter's heavy `AdaptorState` re-exports them.
pub mod trajectory;

/// The Track-C perception monitor (KIRRA-OCCY-PMON-001) — the deterministic
/// perception-degradation speed-cap derate (`KinematicPlausibilityContract` /
/// `apply_perception_cap` / `resolve_perception_cap` / `SharedPerceptionCap`). Pure +
/// dependency-light (`std::sync` + the kinematics-contract talisman + `PerceptionDerateEvent`),
/// so it rides on the lean foundation. Relocated verbatim from the SDK's
/// `gateway::perception_monitor` (de-monolith Stage 7); re-exported by
/// `kirra_verifier::gateway::perception_monitor` so every existing path (the verifier
/// service, the ROS2 adapter) holds.
pub mod perception_monitor;

/// The kinematic forward simulator (`VehicleState` / `apply_enforcement` /
/// `apply_enforce_action` / `run_simulation`) — the deterministic bicycle-model integrator
/// the scenario harness uses to assert physical outcomes. Pure physics over the
/// kinematics-contract talisman; no service/runtime deps. Relocated verbatim from the SDK
/// (de-monolith Stage 7); re-exported by `kirra_verifier::kinematics_sim`.
pub mod kinematics_sim;

/// The learning-loop capture channel — the record BUILDERS (`record_from_verdict` /
/// `record_from_trajectory_verdict`) plus the bounded-mpsc JSONL writer
/// (`spawn_capture_writer`). Touches governor types (now lean, here) so it could not live
/// in the governor-free `kirra-capture-schema`. Behind the off-by-default `capture` feature
/// because the writer pulls `tokio` / `serde_json` / `tracing` — crates that only need the
/// foundation (the planner, the lane map, parko, the governor-service) keep core serde-only.
/// Relocated verbatim from the SDK (de-monolith Stage 7); re-exported by
/// `kirra_verifier::capture`.
#[cfg(feature = "capture")]
pub mod capture;

/// A registered node's trust state, as decided by attestation and the recovery
/// hysteresis. `Untrusted` carries a human-readable reason tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeTrustState {
    Trusted,
    Untrusted(String),
    Unknown,
}

/// The fleet's safety posture — the spine the whole governor hangs on. `Nominal` →
/// full operation; `Degraded` → controlled decel-to-stop-and-hold envelope; `LockedOut`
/// → MRC, human reset required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FleetPosture {
    Nominal,
    Degraded,
    LockedOut,
}

/// Event payload written to the audit chain when the Track-C perception
/// monitor (KIRRA-OCCY-PMON-001) applies a derate. `reason` is the byte-stable
/// `DerateCode` token (SCREAMING_SNAKE_CASE) and is used as the chain
/// `event_type`; `cap_mps` is the resulting permitted-speed cap (`0.0` =
/// controlled stop). All fields are included in the SHA-256 hash.
///
/// Stage 2: relocated here (a lean, dependency-light event payload) so the gateway's
/// `perception_monitor` does not import the heavy `audit_chain` (rusqlite) to name it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerceptionDerateEvent {
    pub reason: String,
    pub cap_mps: f64,
    pub timestamp_ms: u64,
}
