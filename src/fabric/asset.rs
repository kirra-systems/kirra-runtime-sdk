// src/fabric/asset.rs
//
// ADR-0035 Stage 2.5 C2 slice 2: the fabric-plane asset domain types
// (`FabricAsset`, `AssetType`, `KinematicProfileType`, `AssetPosture`,
// `FabricState`) were relocated to the lean `kirra-fabric-types` crate so
// `verifier_store::fabric` can name them without the fabric service tree.
// Re-exported here so every existing `crate::fabric::asset::*` path (store,
// router, telemetry, governor, service binary, tests) resolves unchanged.
pub use kirra_fabric_types::asset::*;
