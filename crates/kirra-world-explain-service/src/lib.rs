//! **The World-side explanation producer — the PROCESS BOUNDARY.**
//!
//! One job, and it is deliberately a boring one: expose bounded,
//! presentation-safe [`ExplanationArtifact`] responses over a process boundary.
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
//! transport. Box 3b fills it in.
//!
//! [`ExplanationArtifact`]: kirra_explain_types::ExplanationArtifact

pub use kirra_world_service::explain_subject::{explain_current_subject, ExplainError};
