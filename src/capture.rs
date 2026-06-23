// src/capture.rs
//
// The learning-loop capture channel (builders + JSONL writer) moved VERBATIM to the lean
// `kirra-core` crate behind its off-by-default `capture` feature (de-monolith Stage 7) so
// the ROS2 adapter's slow-loop emit point can build records without pulling the verifier
// service's heavy tree. Re-exported here so every existing `crate::capture::*` (and
// `kirra_runtime_sdk::capture::*`) path keeps the SAME type — zero churn, zero logic change.
//
// The builders + writer now live in `kirra_core::capture` (which itself re-exports the
// governor-free `kirra_capture_schema` wire types). The SDK enables `kirra-core/capture`
// in its manifest so this re-export resolves; the verifier command-gateway emit point is
// unchanged.
pub use kirra_core::capture::*;
