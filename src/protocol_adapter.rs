// src/protocol_adapter.rs
//
// ADR-0035 Stage 1: the industrial-protocol adapter layer (`protocol_adapter` +
// `adapters/*`) was moved VERBATIM into the lean `kirra-industrial` leaf crate.
// This module is now a re-export shim so every existing
// `kirra_verifier::protocol_adapter::*` path (the service bin, its industrial
// handler + tests) resolves unchanged.
pub use kirra_industrial::protocol_adapter::*;
