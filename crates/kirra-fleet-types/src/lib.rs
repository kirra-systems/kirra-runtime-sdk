// crates/kirra-fleet-types/src/lib.rs
//
// R2 — the lean fleet-lane trust/wire types, extracted from `kirra-verifier`.
//
// The QM fleet transport (`kirra-fleet-transport`) depends on this crate for the
// shared federated-trust payload + verification contract and the narrow
// `FleetTrustStore` persistence seam, WITHOUT pulling the verifier service tree.
// `kirra-verifier` depends on this crate and re-exports `federation` /
// `federation_reconciliation` so every `kirra_verifier::federation*::*` path
// resolves unchanged.

/// v1 federated trust report + canonical payload, Ed25519 verification, and
/// freshness/replay evaluation. Pure (serde + kirra-core `FleetPosture` + crypto).
pub mod federation;

/// v2 generation-ordered federated trust report + verify/evaluate +
/// reconciliation. Pure (delegates the v1 freshness checks to `federation`).
pub mod federation_reconciliation;

/// The `FleetTrustStore` trait — the narrow durable seam (nonce burn, clearance
/// grant persist, fleet key lookup) the transport needs, kept abstract so the
/// SQLite + audit-chain implementation stays in the verifier.
pub mod store;
