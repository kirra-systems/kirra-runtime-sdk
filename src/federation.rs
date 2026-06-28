// src/federation.rs
//
// R2: the federation v1 trust types + Ed25519 verification + freshness evaluation
// were relocated to the lean `kirra-fleet-types` crate so the QM fleet transport
// can depend on the shared contract without the verifier service tree. Re-exported
// here so every existing `crate::federation::*` / `kirra_verifier::federation::*`
// path (service binary, store, tests) resolves unchanged.
pub use kirra_fleet_types::federation::*;
