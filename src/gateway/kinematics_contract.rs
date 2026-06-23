// src/gateway/kinematics_contract.rs
//
// The FROZEN kinematics-contract talisman moved VERBATIM to the lean `kirra-core` crate
// (de-monolith Stage 3) so the certified governor surface need not pull the verifier
// service's heavy tree. Re-exported here so every existing
// `crate::gateway::kinematics_contract::*` (and `kirra_verifier::gateway::
// kinematics_contract::*`) path keeps the SAME type — zero churn, zero logic change.
//
// The contract and its tests now live in `kirra_core::kinematics_contract`. The talisman
// rule is unchanged: the contract is never modified, only relocated.
pub use kirra_core::kinematics_contract::*;
