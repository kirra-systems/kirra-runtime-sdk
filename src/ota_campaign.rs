// src/ota_campaign.rs
//
// ADR-0035 Stage 2.5 step 2 (C2 domain-type relocation): the OTA governor-artifact
// campaign engine — the PURE `Campaign` / `CampaignState` state machine, the
// fail-closed halt-on-regression rule, `NodeArtifactStatus`, node-assignment
// resolution, and the rollout metrics — was relocated to the lean
// `kirra-ota-campaign` crate so the persistence layer
// (`verifier_store::ota_campaigns`) depends on the shared campaign contract
// WITHOUT the state machine living inside the verifier service tree (the last C2
// coupling blocking a mechanical `kirra-persistence` extraction). Re-exported here
// so every existing `crate::ota_campaign::*` / `kirra_verifier::ota_campaign::*`
// path (store, service binary, campaign monitor, metrics, tests) resolves
// unchanged.
pub use kirra_ota_campaign::*;
