// src/fleet_trust_store.rs
//
// R2: implement the lean `kirra_fleet_types::store::FleetTrustStore` seam for the
// heavy `VerifierStore`, so the QM fleet transport can drive the durable trust
// operations (nonce burn, clearance-grant persist, fleet key lookup) through the
// trait WITHOUT depending on this verifier crate. The SQLite + hash-chained audit
// persistence stays here, behind the seam.

use kirra_fleet_types::store::{FleetKeyRole, FleetTrustStore};

use crate::key_registry::{KeyRegistry, KeyRole};
use crate::verifier_store::VerifierStore;

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
        // Reuse the existing KeyRegistry resolution (FederationController / FleetGrant
        // both resolve from the trusted-federation-controller registry). Return the
        // raw 32-byte key; the transport base64-encodes it for the verify call.
        let key_role = match role {
            FleetKeyRole::FederationController => KeyRole::FederationController,
            FleetKeyRole::FleetGrant => KeyRole::FleetGrant,
        };
        KeyRegistry::new(self)
            .resolve_ed25519_pubkey(principal_id, key_role)
            .map(|opt| opt.map(|bytes| bytes.to_vec()))
            .map_err(|e| e.to_string())
    }
}
