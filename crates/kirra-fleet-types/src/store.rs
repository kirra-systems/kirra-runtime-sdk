// crates/kirra-fleet-types/src/store.rs
//
// R2 — the `FleetTrustStore` trait: the NARROW durable persistence seam the QM
// fleet transport needs, abstracted so the transport crate does not depend on the
// heavy `kirra-verifier` `VerifierStore` (SQLite + audit chain). The verifier
// implements this trait for `VerifierStore`; the transport's ingest functions are
// generic over `S: FleetTrustStore`.
//
// Error type is `String` (not `rusqlite::Error`) on purpose: it keeps this crate
// free of the DB dependency, and the transport already surfaces store failures as
// `RejectReason::StoreError(String)`.

/// Which fleet-lane public key to resolve. Both roles resolve from the trusted
/// federation-controller registry in the reference (`VerifierStore`) implementation;
/// the distinction marks intent (a controller's report-signing key vs a fleet
/// clearance-grant signing key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetKeyRole {
    /// The key that signs `FederatedTrustReportV2` payloads.
    FederationController,
    /// The key that signs `SignedClearanceGrant` payloads.
    FleetGrant,
}

/// The durable trust operations the fleet transport requires. Implemented by the
/// verifier's `VerifierStore` so the SQLite + hash-chained audit persistence stays
/// in the verifier crate, while the transport depends only on this seam.
pub trait FleetTrustStore {
    /// Verify-AND-consume a federation nonce (replay defense). Returns `Ok(true)`
    /// on first use (the nonce is now burned), `Ok(false)` if already burned (a
    /// replay), `Err` on a store failure.
    fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool, String>;

    /// Persist a PENDING clearance grant (Phase-A) and append its signed audit
    /// event. Returns the new row id. Implementations MUST keep the audit-chain
    /// write atomic with the row write.
    fn save_clearance_grant_chained(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
    ) -> Result<i64, String>;

    /// Resolve the raw 32-byte Ed25519 public key for `principal_id` in `role`,
    /// or `Ok(None)` if unknown. Fail-closed: a store error is `Err`, never a key.
    fn resolve_fleet_pubkey(
        &self,
        principal_id: &str,
        role: FleetKeyRole,
    ) -> Result<Option<Vec<u8>>, String>;
}
