// crates/kirra-industrial/src/lib.rs
//
// ADR-0035 Stage 1: the industrial-protocol adapter layer, moved VERBATIM from
// the root crate's `src/protocol_adapter.rs` + `src/adapters/*` (pure move —
// behaviour-identical; the only edits are the three cross-crate import rewrites
// `crate::gateway::policy::OperationalCommand` → `kirra_policy_types::OperationalCommand`,
// `crate::action_filter::{..}` → `kirra_policy_types::action_claim::{..}`, and
// `crate::verifier::FleetPosture` → `kirra_core::FleetPosture`).
//
// A clean leaf: depends only on `kirra-policy-types` + `kirra-core` + serde.
// `kirra-verifier` re-exports both modules via shims so existing paths hold.

pub mod adapters;
pub mod protocol_adapter;
