// src/attestation.rs
//
// ADR-0035 Stage 3 (slice 3b): the node-attestation proof-verification module
// (issue #73 — the per-node Ed25519 challenge-response that is INVARIANT #3's real
// crypto, plus the PCR16 measured-boot binding) moved VERBATIM to the lean
// `kirra-safety-authority` crate (attestation is part of the safety-authority
// boundary the Stage-3 target enumerates). It is pure crypto — no `AppState` /
// `VerifierStore` coupling — so the move touches no state.
//
// Re-exported here so every existing `crate::attestation::…` /
// `kirra_verifier::attestation::…` path (the bins, `tpm_quote`, the fleet/operator
// handlers, the ota-installer enroll path, tests) resolves unchanged.
pub use kirra_safety_authority::attestation::*;
