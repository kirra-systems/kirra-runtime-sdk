// crates/kirra-persistence/src/fleet_trust_store.rs
//
// R2 / ADR-0035 slice 4: implement the lean `kirra_fleet_types::store::FleetTrustStore`
// seam for `VerifierStore`, so the QM fleet transport can drive the durable trust
// operations (nonce burn, clearance-grant persist, fleet key lookup) through the
// trait WITHOUT depending on the verifier service tree. This impl MUST live in the
// crate that owns `VerifierStore` (the orphan rule — the trait is external); it was
// relocated here from the root crate at the persistence extraction. The
// federation-key resolution is inlined (a base64 decode of the stored controller
// key) so this impl needs neither the root `KeyRegistry` wrapper nor `crate::attestation`.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use kirra_fleet_types::store::{FleetKeyRole, FleetTrustStore};

use crate::VerifierStore;

/// Decode a base64 controller key into the raw 32-byte Ed25519 public key.
/// `None` on undecodable input or a wrong length — fail-closed (an unresolvable
/// key can never be treated as valid).
fn b64_to_key_bytes(b64: &str) -> Option<[u8; 32]> {
    let raw = B64.decode(b64.as_bytes()).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

impl FleetTrustStore for VerifierStore {
    fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool, String> {
        // Inherent method (priority over the trait method in method-call syntax);
        // call via the type path to be unambiguous.
        VerifierStore::burn_federation_nonce(self, nonce_hex).map_err(|e| e.to_string())
    }

    fn save_clearance_grant_chained(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
    ) -> Result<i64, String> {
        VerifierStore::save_clearance_grant_chained(self, node_id, operator_id, granted_at_ms)
            .map_err(|e| e.to_string())
    }

    fn resolve_fleet_pubkey(
        &self,
        principal_id: &str,
        role: FleetKeyRole,
    ) -> Result<Option<Vec<u8>>, String> {
        // Both fleet roles resolve from the trusted-federation-controller registry
        // (mirrors the root `KeyRegistry` FederationController / FleetGrant arms).
        // Return the raw 32-byte key; the transport base64-encodes it for verify.
        match role {
            FleetKeyRole::FederationController | FleetKeyRole::FleetGrant => Ok(self
                .load_trusted_federation_controller_key(principal_id)
                .map_err(|e| e.to_string())?
                .and_then(|b64| b64_to_key_bytes(&b64))
                .map(|bytes| bytes.to_vec())),
        }
    }
}
