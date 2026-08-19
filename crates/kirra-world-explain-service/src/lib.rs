//! **The World-side explanation producer — the PROCESS BOUNDARY.**
//!
//! One job, and it is deliberately a boring one: serve bounded,
//! presentation-safe [`ExplanationArtifact`] responses over a process boundary,
//! on exactly one capability-specific route.
//!
//! ```text
//! kirra-world-store
//!       ↓
//! kirra-world-service          the explanation SEMANTICS
//!       ↓                      (labels, bounds, absent-vs-deleted)
//! THIS CRATE                   the PROCESS BOUNDARY
//!       ↓  ExplainCurrentSubject / ExplainOutcome over HTTP
//! kirra-explain-types          the neutral wire contract
//!       ↓
//! Mick                         the language
//! ```
//!
//! # What this crate does NOT own
//!
//! The explanation SEMANTICS. Labels, bounds, the lineage-then-provenance path
//! and the absent-versus-deleted distinction all live behind the typed World
//! boundary in [`kirra_world_service::explain_subject`], because
//! `check_query_boundedness` rule 5 refuses a consumer that reaches past the
//! query engine — and this crate is a consumer by that gate's definition.
//!
//! That refusal improved the split rather than obstructing it: all World-aware
//! interpretation stays behind the World service, and what is left here is
//! transport — a socket, a route table, and a codec.
//!
//! # The store dependency is the constructor only
//!
//! This crate names `kirra-world-store` to OPEN a store and pass a reference
//! along. It calls no domain read: `open` and `projection_generation` are
//! classified `operational` in `ci/store_boundedness_baseline.json`, and every
//! read that returns domain content happens behind
//! [`explain_current_subject`]. Rule 5 is what holds that line, and it holds it
//! whether or not anyone remembers this paragraph.
//!
//! # It never folds
//!
//! The producer opens the store and reads. Folding is a WRITE, and an operation
//! whose whole claim is *"this describes what Kirra World currently holds"*
//! must not change what it is describing in the course of describing it. A
//! deployment where nothing else folds will get honest refusals rather than
//! explanations, which is the correct failure.
//!
//! [`ExplanationArtifact`]: kirra_explain_types::ExplanationArtifact

pub mod http;
pub mod service;

pub use kirra_world_service::explain_subject::{explain_current_subject, ExplainError};
