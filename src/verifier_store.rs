// src/verifier_store.rs
//
// ADR-0035 slice 4: the persistence layer (`VerifierStore` + all tables, the
// migration framework, the storage-trait seams, and the audit-chain write
// mechanics) was extracted wholesale into the lean `kirra-persistence` crate — it
// depends only on the domain/audit leaf crates, never back on this service tree.
// Re-exported here so every existing `crate::verifier_store::*` /
// `kirra_verifier::verifier_store::*` path (service binary, the kirra-verifier-pg
// backend, tests) resolves unchanged.
pub use kirra_persistence::*;
