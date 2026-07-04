// src/gateway/kinematics_contract.rs
//
// The FROZEN kinematics-contract talisman moved VERBATIM to the lean `kirra-core` crate
// (de-monolith Stage 3) so the certified governor surface need not pull the verifier
// service's heavy tree. Re-exported here so every existing
// `crate::gateway::kinematics_contract::*` (and `kirra_verifier::gateway::
// kinematics_contract::*`) path keeps the SAME type — zero churn, zero logic change.
//
// The contract and its tests now live in `kirra_core::kinematics_contract`. The talisman
// rule stands: the contract is amended ONLY under explicit review + a re-pin. It received
// ONE such reviewed amendment (stop-gate review H1/M1 — `EnforceAction::ClampBoth` and
// direction-aware accel/brake selection); re-pinned to logic blob
// ed00f4da30afe8f3f83ff10a0d31103737526622 (see docs/CAPTURE_PIPELINE_SPEC.md §0). This
// shim stays a pure re-export.
pub use kirra_core::kinematics_contract::*;
