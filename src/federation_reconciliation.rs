// src/federation_reconciliation.rs
//
// R2: the federation v2 report + verify/evaluate + reconciliation were relocated to
// the lean `kirra-fleet-types` crate. Re-exported here so every existing
// `crate::federation_reconciliation::*` / `kirra_verifier::federation_reconciliation::*`
// path resolves unchanged.
pub use kirra_fleet_types::federation_reconciliation::*;
