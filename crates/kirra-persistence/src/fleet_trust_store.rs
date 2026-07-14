// crates/kirra-persistence/src/fleet_trust_store.rs
//
// R2 / ADR-0035 slice 4: implement the lean `kirra_fleet_types::store::FleetTrustStore`
// seam for `VerifierStore`, so the QM fleet transport can drive the durable trust
// operations (nonce burn, clearance-grant persist, fleet key lookup) through the
// trait WITHOUT depending on the verifier service tree. This impl MUST live in the
// crate that owns `VerifierStore` (the orphan rule — the trait is external).
//
// slice-4 follow-up: `resolve_fleet_pubkey` now delegates to the relocated,
// unified `KeyRegistry` (ADR-0008 / #329) instead of an inlined base64 decode —
// one vetted key-resolution path (whitespace trim, wrong-length rejection) shared
// with every other principal role, and it gives `KeyRegistry` a real consumer.

use kirra_fleet_types::store::{FleetKeyRole, FleetTrustStore};

use crate::key_registry::{KeyRegistry, KeyRole};
use crate::VerifierStore;

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
        // (ADR-0008); map to the unified `KeyRegistry` role and delegate. The
        // registry returns the raw 32-byte key; the transport base64-encodes it for
        // verify.
        let kr_role = match role {
            FleetKeyRole::FederationController | FleetKeyRole::FleetGrant => {
                KeyRole::FederationController
            }
        };
        KeyRegistry::new(self)
            .resolve_ed25519_pubkey(principal_id, kr_role)
            .map_err(|e| e.to_string())
            .map(|opt| opt.map(|bytes| bytes.to_vec()))
    }
}
