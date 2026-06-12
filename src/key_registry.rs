//! # KeyRegistry (#329) — one abstraction over four key stores
//!
//! Trust in this system is grounded in Ed25519 public keys, but those keys live in
//! **four** different stores in **three** different encodings:
//!
//! | Principal            | Table                            | Column            | Encoding |
//! |----------------------|----------------------------------|-------------------|----------|
//! | Node (attestation)   | `nodes`                          | `ak_public_pem`   | SPKI PEM |
//! | Federation controller| `trusted_federation_controllers` | `public_key_b64`  | raw b64  |
//! | Operator             | `operators`                      | `pubkey_pem`      | SPKI PEM |
//! | Fleet-grant signer   | `trusted_federation_controllers` | `public_key_b64`  | raw b64  |
//! | Audit signer         | in-memory `SigningKey` (store)   | —                 | raw bytes|
//!
//! Before this, every verification site re-implemented the load + the decode, and
//! the fleet crate verified against a **caller-supplied** `public_key_b64: &str` —
//! the caller decided which key to trust per call. [`KeyRegistry`] is an **additive
//! wrapper** over [`VerifierStore`] that unifies the lookup behind one API,
//! [`KeyRegistry::resolve_ed25519_pubkey`], which returns the **raw 32-byte** key
//! normalized from whatever encoding the table holds. Fleet verification now grounds
//! trust in a *stored registration*, not a string the caller passed in.
//!
//! Decision + rationale: `docs/adr/0008-key-registry.md` (ADR-0008).
//!
//! ## Named residuals (ADR-0008, honest limits)
//! - **The audit signing key is resolvable READ-ONLY (#329 residual, Phase A.1).**
//!   [`KeyRole::AuditSigning`] resolves the chain's verifying key from the store's
//!   in-memory signer, keyed by the key's `verifying_key_id` fingerprint (the same id
//!   the audit chain stamps in its `key_id` column) — so a chain row's `key_id` can be
//!   resolved to a key through the registry. **Rotation + persisted key history remain
//!   deferred:** the registry knows only the ONE current in-memory key, so a request
//!   for any fingerprint other than the live signer's is `None`.
//! - **Encoding migration is deferred.** This normalizes at the *read* boundary
//!   (PEM/b64 → bytes); it does NOT rewrite the stores to one on-disk format. One
//!   abstraction now; one on-disk encoding is named future work.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use rusqlite::Result;

use crate::verifier_store::VerifierStore;

/// Which principal store a key is resolved from. The role names the *intent* of the
/// lookup; `FederationController` and `FleetGrant` resolve from the SAME table (the
/// trusted-controller registry) — the registry is the single lookup, the role
/// disambiguates the caller's purpose at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    /// `ak_public_pem` from the `nodes` table (node attestation identity).
    NodeAttestation,
    /// `public_key_b64` from `trusted_federation_controllers`.
    FederationController,
    /// `pubkey_pem` from the `operators` table. A revoked operator resolves to
    /// `None` (treated as unknown).
    Operator,
    /// `public_key_b64` from `trusted_federation_controllers` — the same registry as
    /// [`KeyRole::FederationController`]; the role marks fleet-grant intent.
    FleetGrant,
    /// The store's in-memory audit signing key (read-only, #329 residual). The
    /// `principal_id` is the key's `verifying_key_id` fingerprint (as stamped in the
    /// audit chain's `key_id`); resolves ONLY the single live signer — there is no
    /// rotation/history, so any other fingerprint is `None`.
    AuditSigning,
}

/// A read-only unified view over the four key stores, wrapping a [`VerifierStore`].
/// Borrows the store immutably for its lifetime.
pub struct KeyRegistry<'a> {
    store: &'a VerifierStore,
}

impl<'a> KeyRegistry<'a> {
    #[must_use]
    pub fn new(store: &'a VerifierStore) -> KeyRegistry<'a> {
        KeyRegistry { store }
    }

    /// Resolve a principal's Ed25519 public key to its **raw 32 bytes**, normalized
    /// from whatever encoding the backing table uses. Returns `Ok(None)` for an
    /// unknown principal, a revoked operator, a malformed/wrong-length stored key, or
    /// a principal with no registered key — every "cannot resolve" is a `None`, never
    /// a trusted default. `Err` is reserved for an actual store (SQL) failure.
    pub fn resolve_ed25519_pubkey(
        &self,
        principal_id: &str,
        role: KeyRole,
    ) -> Result<Option<[u8; 32]>> {
        let bytes = match role {
            KeyRole::NodeAttestation => self
                .store
                .load_node(principal_id)?
                .and_then(|n| n.ak_public_pem)
                .and_then(|pem| pem_to_key_bytes(&pem)),
            KeyRole::FederationController | KeyRole::FleetGrant => self
                .store
                .load_trusted_federation_controller_key(principal_id)?
                .and_then(|b64| b64_to_key_bytes(&b64)),
            KeyRole::Operator => match self.store.load_operator(principal_id)? {
                // A revoked operator is treated as unknown — None, never a key.
                Some(op) if op.revoked_at_ms.is_none() => pem_to_key_bytes(&op.pubkey_pem),
                _ => None,
            },
            KeyRole::AuditSigning => self.store.audit_verifying_key().and_then(|vk| {
                // Resolve ONLY when the requested fingerprint matches the live signer
                // (no key history) — fail-closed for any other id.
                if crate::audit_chain::verifying_key_id(&vk) == principal_id {
                    Some(vk.to_bytes())
                } else {
                    None
                }
            }),
        };
        Ok(bytes)
    }

    /// Verify an Ed25519 `signature_b64` over `payload` for `principal_id` in one
    /// call. **Fail-closed:** an unknown principal, a store error, a malformed key,
    /// or a malformed/bad signature all return `false` — never an `Err` a caller
    /// might ignore into an accept. Uses `verify_strict` (the attestation path).
    #[must_use]
    pub fn verify_for(
        &self,
        principal_id: &str,
        role: KeyRole,
        payload: &[u8],
        signature_b64: &str,
    ) -> bool {
        let Ok(Some(key_bytes)) = self.resolve_ed25519_pubkey(principal_id, role) else {
            return false;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&key_bytes) else { return false };
        let Ok(sig_raw) = B64.decode(signature_b64.trim()) else { return false };
        let Ok(sig_arr) = <[u8; 64]>::try_from(sig_raw.as_slice()) else { return false };
        vk.verify_strict(payload, &Signature::from_bytes(&sig_arr)).is_ok()
    }
}

/// SPKI PEM → raw 32-byte Ed25519 key, via the existing, tested SPKI parser
/// ([`crate::attestation::parse_ed25519_public_pem`]) — NOT a hand-rolled offset.
/// `None` on any malformed PEM.
fn pem_to_key_bytes(pem: &str) -> Option<[u8; 32]> {
    crate::attestation::parse_ed25519_public_pem(pem).map(|vk| vk.to_bytes())
}

/// base64 raw key → 32 bytes. A wrong-length or undecodable key is rejected with a
/// logged warning and `None` — never a panic, never a truncated/padded key.
fn b64_to_key_bytes(b64: &str) -> Option<[u8; 32]> {
    let raw = match B64.decode(b64.trim()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "key_registry: stored base64 key failed to decode — rejected");
            return None;
        }
    };
    match <[u8; 32]>::try_from(raw.as_slice()) {
        Ok(k) => Some(k),
        Err(_) => {
            tracing::warn!(len = raw.len(), "key_registry: stored key is not 32 bytes — rejected");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::{NodeTrustState, RegisteredNode};
    use ed25519_dalek::{Signer, SigningKey};

    /// A deterministic keypair + its SPKI PEM (the in-repo RFC-8410 12-byte prefix
    /// convention) — derived from a seed, NOT a hardcoded key.
    fn keypair(seed: u8) -> (SigningKey, String, String) {
        const ED25519_SPKI_PREFIX: [u8; 12] =
            [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let raw = sk.verifying_key().to_bytes();
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(&raw);
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            B64.encode(&der)
        );
        let raw_b64 = B64.encode(raw);
        (sk, pem, raw_b64)
    }

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    fn save_node_with_pem(store: &VerifierStore, node_id: &str, pem: Option<String>) {
        store
            .save_node(&RegisteredNode {
                node_id: node_id.to_string(),
                status: NodeTrustState::Trusted,
                registered_at_ms: 1,
                last_trust_update_ms: 1,
                ak_public_pem: pem,
                expected_pcr16_digest_hex: None,
            })
            .expect("save node");
    }

    #[test]
    fn pem_node_key_resolves_to_correct_32_bytes() {
        let (sk, pem, _b64) = keypair(7);
        let store = store();
        save_node_with_pem(&store, "robot-01", Some(pem));
        let reg = KeyRegistry::new(&store);
        let got = reg.resolve_ed25519_pubkey("robot-01", KeyRole::NodeAttestation).unwrap();
        assert_eq!(got, Some(sk.verifying_key().to_bytes()), "PEM node key → the exact 32 bytes");
    }

    #[test]
    fn b64_and_pem_paths_agree_encoding_parity() {
        // The SAME key registered as a federation controller (raw b64) and as a node
        // (SPKI PEM) must resolve to the SAME 32 bytes — the normalization is parity.
        let (sk, pem, raw_b64) = keypair(9);
        let store = store();
        save_node_with_pem(&store, "node-A", Some(pem));
        store.save_trusted_federation_controller("ctrl-A", &raw_b64, 1).unwrap();
        let reg = KeyRegistry::new(&store);
        let via_pem = reg.resolve_ed25519_pubkey("node-A", KeyRole::NodeAttestation).unwrap();
        let via_b64 = reg.resolve_ed25519_pubkey("ctrl-A", KeyRole::FederationController).unwrap();
        assert_eq!(via_pem, Some(sk.verifying_key().to_bytes()));
        assert_eq!(via_pem, via_b64, "PEM and b64 paths normalize to identical bytes");
        // FleetGrant resolves from the same controller registry.
        let via_fleet = reg.resolve_ed25519_pubkey("ctrl-A", KeyRole::FleetGrant).unwrap();
        assert_eq!(via_fleet, via_b64, "FleetGrant shares the controller registry");
    }

    #[test]
    fn unknown_principal_is_none_not_error() {
        let store = store();
        let reg = KeyRegistry::new(&store);
        assert_eq!(reg.resolve_ed25519_pubkey("ghost", KeyRole::NodeAttestation).unwrap(), None);
        assert_eq!(reg.resolve_ed25519_pubkey("ghost", KeyRole::FederationController).unwrap(), None);
        assert_eq!(reg.resolve_ed25519_pubkey("ghost", KeyRole::Operator).unwrap(), None);
    }

    #[test]
    fn revoked_operator_resolves_to_none() {
        let (_sk, pem, _b64) = keypair(3);
        let mut store = store();
        store.register_operator("alice", &pem, 1).unwrap();
        let reg = KeyRegistry::new(&store);
        assert!(reg.resolve_ed25519_pubkey("alice", KeyRole::Operator).unwrap().is_some(), "active resolves");
        // Revoke, then it must read as unknown.
        store.revoke_operator("alice", 2).unwrap();
        let reg = KeyRegistry::new(&store);
        assert_eq!(
            reg.resolve_ed25519_pubkey("alice", KeyRole::Operator).unwrap(),
            None,
            "a revoked operator resolves to None (treated as unknown)"
        );
    }

    #[test]
    fn verify_for_good_bad_and_unknown() {
        let (sk, pem, _b64) = keypair(5);
        let store = store();
        save_node_with_pem(&store, "robot-02", Some(pem));
        let reg = KeyRegistry::new(&store);

        let payload = b"attestation-challenge-bytes";
        let good = B64.encode(sk.sign(payload).to_bytes());
        assert!(reg.verify_for("robot-02", KeyRole::NodeAttestation, payload, &good), "valid sig → true");

        // Tampered payload → bad signature.
        assert!(!reg.verify_for("robot-02", KeyRole::NodeAttestation, b"different", &good), "bad sig → false");

        // Unknown principal → false (fail-closed), never an ignored error.
        assert!(!reg.verify_for("ghost", KeyRole::NodeAttestation, payload, &good), "unknown → false");

        // Malformed signature base64 → false.
        assert!(!reg.verify_for("robot-02", KeyRole::NodeAttestation, payload, "!!notb64"), "malformed sig → false");
    }

    #[test]
    fn wrong_length_b64_key_is_none_no_panic() {
        let store = store();
        // 16 bytes, not 32 — a structurally invalid Ed25519 key.
        store.save_trusted_federation_controller("ctrl-bad", &B64.encode([0u8; 16]), 1).unwrap();
        let reg = KeyRegistry::new(&store);
        assert_eq!(
            reg.resolve_ed25519_pubkey("ctrl-bad", KeyRole::FederationController).unwrap(),
            None,
            "a wrong-length stored key is rejected (None), not a panic"
        );
    }

    #[test]
    fn audit_signing_key_resolves_readonly_by_fingerprint() {
        let (sk, _pem, _b64) = keypair(11);
        let mut store = store();

        // No signer installed → None for any fingerprint (fail-closed).
        {
            let reg = KeyRegistry::new(&store);
            assert_eq!(
                reg.resolve_ed25519_pubkey("anything", KeyRole::AuditSigning).unwrap(),
                None,
                "no audit signer installed → None"
            );
        }

        let vk = sk.verifying_key();
        let fp = crate::audit_chain::verifying_key_id(&vk);
        store.set_signing_key(sk.clone());
        let reg = KeyRegistry::new(&store);

        // The live audit key resolves by its verifying_key_id (the chain's key_id).
        assert_eq!(
            reg.resolve_ed25519_pubkey(&fp, KeyRole::AuditSigning).unwrap(),
            Some(vk.to_bytes()),
            "the live audit key resolves by its verifying_key_id"
        );
        // Any other fingerprint → None (no rotation / no key history — the residual).
        assert_eq!(
            reg.resolve_ed25519_pubkey("deadbeef", KeyRole::AuditSigning).unwrap(),
            None,
            "only the single live signer resolves — rotation/history still deferred"
        );

        // verify_for over the audit key: good sig true; wrong fingerprint false.
        let payload = b"a-signed-audit-event";
        let sig = B64.encode(sk.sign(payload).to_bytes());
        assert!(reg.verify_for(&fp, KeyRole::AuditSigning, payload, &sig), "audit-key signature verifies");
        assert!(!reg.verify_for("wrong-fp", KeyRole::AuditSigning, payload, &sig), "wrong fingerprint → false");
    }
}
