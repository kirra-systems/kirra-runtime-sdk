//! # kirra-fleet-transport — the FLEET-LANE (QM) Zenoh transport spike (#296)
//!
//! Vehicle ↔ ops/cloud transport for cellular / distributed fleets. The decision
//! and its three clauses live in `docs/adr/0007-fleet-transport-zenoh.md`; this
//! crate is the spike. Restated:
//!
//! 1. **Untrusted carrier (the trust rule).** Zenoh is an UNTRUSTED CARRIER.
//!    Trust derives from **Ed25519 payload signatures** — federation reports via
//!    [`kirra_verifier::federation_reconciliation::verify_federated_report_signature_v2`],
//!    grants via [`verify_clearance_grant`] — **never** from transport identity, a
//!    topic name, or Zenoh's own auth. Every ingest **verifies before use**;
//!    unsigned / bad-signature / malformed payloads are rejected and **counted**
//!    ([`RejectionCounter`]).
//! 2. **Strictly QM (the domain rule).** This crate is a LEAF consumer that
//!    depends on the SDK. **Nothing under `src/gateway/` or any safety path may
//!    depend on it** (ADR-0006 Clause 2's boundary asymmetry is the parent rule).
//! 3. **Grants terminate at the store (the grant rule).** The remote grant lane
//!    writes a *verified* grant through the **existing** Phase-A store path
//!    ([`VerifierStore::save_clearance_grant_chained`]) as a `PENDING` row;
//!    Phase-B's one-shot pickup + two-checkpoint delivery proceed UNCHANGED. No
//!    new store schema, no second release path.
//!
//! This module ([`lib`](self)) is the **transport-free trust + codec core** — the
//! load-bearing safety logic, unit-tested without any Zenoh session. The Zenoh
//! pub/sub edge is in [`transport`].

use std::sync::atomic::{AtomicU64, Ordering};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

// R2: the QM fleet transport depends only on the lean fleet-lane types + the
// `FleetTrustStore` seam (+ kirra-core for FleetPosture) — NOT the heavy
// `kirra-verifier` service crate. The verifier implements `FleetTrustStore` for
// its `VerifierStore`; the ingest functions below are generic over that trait.
use kirra_core::FleetPosture;
use kirra_fleet_types::federation_reconciliation::{
    evaluate_federated_report_v2, verify_federated_report_signature_v2, FederatedTrustReportV2,
};
use kirra_fleet_types::store::{FleetKeyRole, FleetTrustStore};

pub mod transport;

// ---------------------------------------------------------------------------
// Namespace — versioned key expressions
// ---------------------------------------------------------------------------
//
// Up (vehicle → ops/cloud): trust reports + posture summaries.
// Down (ops/cloud → vehicle): clearance grants.
// The `v1` segment is the wire-contract version; a breaking change bumps it.

/// Key expression for a node's signed trust report (vehicle → fleet).
#[must_use]
pub fn key_trust_report(node_id: &str) -> String {
    format!("kirra/v1/fleet/{node_id}/trust-report")
}

/// Key expression for a node's posture summary (vehicle → fleet).
#[must_use]
pub fn key_posture(node_id: &str) -> String {
    format!("kirra/v1/fleet/{node_id}/posture")
}

/// Key expression for a node's clearance grant (ops/cloud → vehicle).
#[must_use]
pub fn key_clearance_grant(node_id: &str) -> String {
    format!("kirra/v1/ops/{node_id}/clearance-grant")
}

// ---------------------------------------------------------------------------
// Rejection accounting (surfaced for ops)
// ---------------------------------------------------------------------------

/// Why a received payload was rejected BEFORE use. Surfaced via [`RejectionCounter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// The payload carried no signature (empty `signature_b64`).
    Unsigned,
    /// The Ed25519 signature did not verify against the expected key.
    BadSignature,
    /// The payload could not be decoded from the wire bytes.
    Decode(String),
    /// The payload decoded but is structurally invalid (e.g. empty node/operator).
    Malformed(String),
    /// The signature verified but the grant's freshness window has closed
    /// (`now_ms >= expires_at_ms`). A stale-but-authentic grant replayed off the
    /// carrier outside its TTL — rejected fail-closed.
    Expired,
    /// The signature verified and the grant is fresh, but its nonce was already
    /// burned — i.e. this exact grant has been ingested before. A replay of a
    /// captured, still-in-window grant — rejected fail-closed (verify-AND-consume).
    Replayed,
    /// A verified grant could not be written to the store (fail-closed).
    StoreError(String),
}

impl RejectReason {
    /// A short, stable code for ops dashboards / logs.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            RejectReason::Unsigned => "unsigned",
            RejectReason::BadSignature => "bad_signature",
            RejectReason::Decode(_) => "decode_error",
            RejectReason::Malformed(_) => "malformed",
            RejectReason::Expired => "expired",
            RejectReason::Replayed => "replayed",
            RejectReason::StoreError(_) => "store_error",
        }
    }
}

/// Operator-observable rejected-payload counters. An untrusted carrier WILL deliver
/// junk and tampered payloads; this makes the rejections visible rather than silent.
#[derive(Debug, Default)]
pub struct RejectionCounter {
    unsigned: AtomicU64,
    bad_signature: AtomicU64,
    decode_error: AtomicU64,
    malformed: AtomicU64,
    expired: AtomicU64,
    replayed: AtomicU64,
    store_error: AtomicU64,
    accepted: AtomicU64,
}

impl RejectionCounter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rejection by reason.
    pub fn record(&self, reason: &RejectReason) {
        let slot = match reason {
            RejectReason::Unsigned => &self.unsigned,
            RejectReason::BadSignature => &self.bad_signature,
            RejectReason::Decode(_) => &self.decode_error,
            RejectReason::Malformed(_) => &self.malformed,
            RejectReason::Expired => &self.expired,
            RejectReason::Replayed => &self.replayed,
            RejectReason::StoreError(_) => &self.store_error,
        };
        slot.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an accepted (verified) payload.
    pub fn record_accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> RejectionSnapshot {
        RejectionSnapshot {
            unsigned: self.unsigned.load(Ordering::Relaxed),
            bad_signature: self.bad_signature.load(Ordering::Relaxed),
            decode_error: self.decode_error.load(Ordering::Relaxed),
            malformed: self.malformed.load(Ordering::Relaxed),
            expired: self.expired.load(Ordering::Relaxed),
            replayed: self.replayed.load(Ordering::Relaxed),
            store_error: self.store_error.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
        }
    }

    /// Total rejected across all reasons.
    #[must_use]
    pub fn total_rejected(&self) -> u64 {
        let s = self.snapshot();
        s.unsigned + s.bad_signature + s.decode_error + s.malformed + s.expired + s.replayed
            + s.store_error
    }
}

/// A point-in-time copy of [`RejectionCounter`] for ops surfacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RejectionSnapshot {
    pub unsigned: u64,
    pub bad_signature: u64,
    pub decode_error: u64,
    pub malformed: u64,
    pub expired: u64,
    pub replayed: u64,
    pub store_error: u64,
    pub accepted: u64,
}

// ---------------------------------------------------------------------------
// Fleet posture summary (a small vehicle → fleet status payload)
// ---------------------------------------------------------------------------

/// A compact, *unsigned* posture summary for the fleet dashboard. Posture is
/// advisory telemetry, not an authority signal — the authoritative, signed path is
/// the [`FederatedTrustReportV2`]. (A future hardening may sign these too; for the
/// spike they are debug/telemetry only and never drive a safety decision.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostureSummary {
    pub node_id: String,
    pub posture: FleetPosture,
    pub generated_at_ms: u64,
}

// ---------------------------------------------------------------------------
// Report codec (JSON — fleet-lane debuggability; trust is in the signature)
// ---------------------------------------------------------------------------

/// Encode a signed trust report for the fleet wire (JSON).
pub fn encode_report(report: &FederatedTrustReportV2) -> Result<Vec<u8>, RejectReason> {
    serde_json::to_vec(report).map_err(|e| RejectReason::Decode(e.to_string()))
}

/// Decode + **verify the signature** of a trust report against `public_key_b64`.
/// **Verification happens BEFORE the report is surfaced to any caller** — an
/// unsigned / bad-signature / malformed payload is rejected and the `counter` is
/// incremented; only a signature-verified report is returned.
///
/// **Signature-only — NOT replay-safe (#322).** This is the pure-verify primitive
/// (the report-lane analogue of [`verify_clearance_grant`]); it does **no**
/// freshness or nonce check, so a captured validly-signed report replays forever
/// through it. The **replay-safe, store-backed deployment path is
/// [`ingest_report`]** (and [`ingest_report_from_registry`]), which adds the
/// freshness window + atomic nonce-burn. Use those whenever a `VerifierStore` is
/// available; `accept_report` is for spike/test callers verifying a signature in
/// isolation.
pub fn accept_report(
    bytes: &[u8],
    public_key_b64: &str,
    counter: &RejectionCounter,
) -> Result<FederatedTrustReportV2, RejectReason> {
    let report: FederatedTrustReportV2 = match serde_json::from_slice(bytes) {
        Ok(r) => r,
        Err(e) => {
            let reason = RejectReason::Decode(e.to_string());
            counter.record(&reason);
            return Err(reason);
        }
    };
    if report.signature_b64.trim().is_empty() {
        counter.record(&RejectReason::Unsigned);
        return Err(RejectReason::Unsigned);
    }
    // THE TRUST RULE — verify the Ed25519 signature over the canonical payload
    // BEFORE the report is used. Trust is the signature, never the carrier.
    if !verify_federated_report_signature_v2(&report, public_key_b64) {
        counter.record(&RejectReason::BadSignature);
        return Err(RejectReason::BadSignature);
    }
    counter.record_accepted();
    Ok(report)
}

/// Registry-backed [`accept_report`] (#329) — the **fleet-deployment** path. Resolves
/// the controller's key from the unified [`KeyRegistry`] (grounded in a STORED
/// registration, not a caller-supplied `public_key_b64` string), then delegates to
/// [`accept_report`]. The `&str` variant remains for test/spike callers without a
/// store. Fail-closed: an unresolvable principal (unknown controller / malformed
/// stored key) is a counted [`RejectReason::BadSignature`] reject — the report is
/// untrusted, never accepted by default.
pub fn accept_report_from_registry<S: FleetTrustStore>(
    bytes: &[u8],
    principal_id: &str,
    store: &S,
    counter: &RejectionCounter,
) -> Result<FederatedTrustReportV2, RejectReason> {
    let key_b64 = match store.resolve_fleet_pubkey(principal_id, FleetKeyRole::FederationController) {
        Ok(Some(k)) => B64.encode(k),
        _ => {
            counter.record(&RejectReason::BadSignature);
            return Err(RejectReason::BadSignature);
        }
    };
    accept_report(bytes, &key_b64, counter)
}

/// Replay-safe trust-report ingest (#322) — the report-lane analogue of
/// [`ingest_clearance_grant`]. Decode → verify the Ed25519 signature → enforce
/// FRESHNESS (the report's `issued_at_ms`/`expires_at_ms` window, via
/// [`evaluate_federated_report_v2`]) → claim the `nonce_hex` in one atomic
/// verify-AND-consume step ([`VerifierStore::burn_federation_nonce`]). The nonce is
/// burned **only after** the signature and freshness pass, so a stale report never
/// burns a slot; a nonce already on record means the same report was ingested
/// before ([`RejectReason::Replayed`]). On the explicitly-untrusted carrier this is
/// what stops a captured validly-signed report from replaying forever — trust is
/// the signature + freshness + single-use nonce, never the carrier.
pub fn ingest_report<S: FleetTrustStore>(
    store: &S,
    bytes: &[u8],
    public_key_b64: &str,
    counter: &RejectionCounter,
    now_ms: u64,
) -> Result<FederatedTrustReportV2, RejectReason> {
    let report: FederatedTrustReportV2 = match serde_json::from_slice(bytes) {
        Ok(r) => r,
        Err(e) => {
            let r = RejectReason::Decode(e.to_string());
            counter.record(&r);
            return Err(r);
        }
    };
    if report.signature_b64.trim().is_empty() {
        let r = RejectReason::Unsigned;
        counter.record(&r);
        return Err(r);
    }
    if report.nonce_hex.trim().is_empty() {
        let r = RejectReason::Malformed("empty nonce_hex".into());
        counter.record(&r);
        return Err(r);
    }
    // THE TRUST RULE — verify the signature over the canonical payload first.
    if !verify_federated_report_signature_v2(&report, public_key_b64) {
        let r = RejectReason::BadSignature;
        counter.record(&r);
        return Err(r);
    }
    // Authentic — enforce freshness BEFORE consuming the nonce, so a stale report
    // never burns a nonce slot (mirrors the grant lane's ordering). The
    // `evaluate_federated_report_v2` gate covers the issued/expiry window + the
    // structural generation check; any non-accept is a fail-closed freshness reject.
    if !evaluate_federated_report_v2(&report, now_ms).accepted {
        let r = RejectReason::Expired;
        counter.record(&r);
        return Err(r);
    }
    // Verify-AND-consume: atomically claim the nonce. `Ok(false)` = already burned =
    // a replay of a still-fresh captured report.
    match store.burn_federation_nonce(&report.nonce_hex) {
        Ok(true) => {}
        Ok(false) => {
            let r = RejectReason::Replayed;
            counter.record(&r);
            return Err(r);
        }
        Err(e) => {
            let r = RejectReason::StoreError(e.to_string());
            counter.record(&r);
            return Err(r);
        }
    }
    counter.record_accepted();
    Ok(report)
}

/// Registry-backed [`ingest_report`] (#322/#329) — the **replay-safe fleet-deployment**
/// report path. Resolves the controller's key from the unified [`KeyRegistry`]
/// (grounded in a STORED registration), then delegates to [`ingest_report`].
/// Fail-closed: an unresolvable principal is a counted [`RejectReason::BadSignature`].
pub fn ingest_report_from_registry<S: FleetTrustStore>(
    store: &S,
    bytes: &[u8],
    principal_id: &str,
    counter: &RejectionCounter,
    now_ms: u64,
) -> Result<FederatedTrustReportV2, RejectReason> {
    let key_b64 = match store.resolve_fleet_pubkey(principal_id, FleetKeyRole::FederationController) {
        Ok(Some(k)) => B64.encode(k),
        _ => {
            let r = RejectReason::BadSignature;
            counter.record(&r);
            return Err(r);
        }
    };
    ingest_report(store, bytes, &key_b64, counter, now_ms)
}

// ---------------------------------------------------------------------------
// Signed clearance grant (the down lane) — reuses the federation Ed25519 pattern
// ---------------------------------------------------------------------------

/// Freshness window for a signed clearance grant (ms). A grant is valid from
/// `granted_at_ms` until `granted_at_ms + FLEET_GRANT_TTL_MS`; after that an
/// otherwise-authentic grant replayed off the carrier is rejected as `Expired`.
/// Two minutes accommodates a cellular ops→vehicle hop while bounding the window in
/// which a captured grant could be replayed before its nonce burn (the nonce burn is
/// the hard, single-use defense; the TTL bounds the worst case and lets the vehicle
/// reject obviously-stale traffic without a store round-trip).
pub const FLEET_GRANT_TTL_MS: u64 = 120_000;

/// An operator clearance grant signed by the issuing controller for transport over
/// the untrusted fleet carrier. The vehicle verifies the signature against the
/// controller's registered public key BEFORE writing it to the store.
///
/// The signature is over a canonical payload of the *semantic* fields
/// ([`canonical_grant_payload`]), NOT the wire bytes — so the codec is independent
/// of the trust root (mirrors `canonical_federation_payload_v2`).
///
/// **Replay defense (#322).** Both the `nonce_hex` (a per-grant unique token) and
/// `expires_at_ms` (the freshness deadline) are inside the signed payload, so an
/// attacker on the untrusted carrier cannot mint a fresh nonce or extend the TTL
/// without invalidating the signature. The vehicle enforces both at ingest:
/// verify-AND-consume burns the nonce exactly once, and the TTL bounds the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedClearanceGrant {
    pub node_id: String,
    pub operator_id: String,
    pub granted_at_ms: u64,
    /// Freshness deadline (ms, same clock domain as `granted_at_ms`). Signed.
    pub expires_at_ms: u64,
    /// Per-grant unique nonce (hex). Burned exactly once at ingest. Signed.
    pub nonce_hex: String,
    pub signature_b64: String,
}

/// The canonical bytes an Ed25519 signature covers for a clearance grant. Includes
/// the replay-defense fields (`expires_at_ms`, `nonce_hex`) so they are tamper-evident.
#[must_use]
pub fn canonical_grant_payload(
    node_id: &str,
    operator_id: &str,
    granted_at_ms: u64,
    expires_at_ms: u64,
    nonce_hex: &str,
) -> String {
    serde_json::json!({
        "node_id": node_id,
        "operator_id": operator_id,
        "granted_at_ms": granted_at_ms,
        "expires_at_ms": expires_at_ms,
        "nonce_hex": nonce_hex,
    })
    .to_string()
}

/// Sign a clearance grant with the issuing controller's key (ops/cloud side). Mints a
/// fresh random `nonce_hex` and sets `expires_at_ms = granted_at_ms + FLEET_GRANT_TTL_MS`,
/// then signs over both — so the grant is single-use and time-bounded on the wire.
#[must_use]
pub fn sign_clearance_grant(
    signing_key: &SigningKey,
    node_id: &str,
    operator_id: &str,
    granted_at_ms: u64,
) -> SignedClearanceGrant {
    let mut nonce = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut nonce);
    let nonce_hex = hex_encode(&nonce);
    let expires_at_ms = granted_at_ms.saturating_add(FLEET_GRANT_TTL_MS);
    let payload =
        canonical_grant_payload(node_id, operator_id, granted_at_ms, expires_at_ms, &nonce_hex);
    let sig: Signature = signing_key.sign(payload.as_bytes());
    SignedClearanceGrant {
        node_id: node_id.to_string(),
        operator_id: operator_id.to_string(),
        granted_at_ms,
        expires_at_ms,
        nonce_hex,
        signature_b64: B64.encode(sig.to_bytes()),
    }
}

/// Lowercase-hex encode (small, dependency-free — avoids pulling the `hex` crate
/// into this leaf for a 16-byte nonce).
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Verify a clearance grant's Ed25519 signature against `public_key_b64`. Fail-closed
/// on any malformed key / signature.
#[must_use]
pub fn verify_clearance_grant(grant: &SignedClearanceGrant, public_key_b64: &str) -> bool {
    let Ok(pk_bytes) = B64.decode(public_key_b64) else { return false };
    let Ok(sig_bytes) = B64.decode(grant.signature_b64.as_bytes()) else { return false };
    let Ok(pk_array) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else { return false };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&pk_array) else { return false };
    let sig = Signature::from_bytes(&sig_array);
    let payload = canonical_grant_payload(
        &grant.node_id,
        &grant.operator_id,
        grant.granted_at_ms,
        grant.expires_at_ms,
        &grant.nonce_hex,
    );
    // L-1: verify_strict rejects malleable / non-canonical signatures, matching the
    // crate-wide crypto discipline (federation uses verify_strict). Inherent method
    // on VerifyingKey — no Verifier trait needed.
    key.verify_strict(payload.as_bytes(), &sig).is_ok()
}

/// **THE GRANT RULE.** Verify a received grant, then write it through the EXISTING
/// Phase-A store path as a `PENDING-NODE-TRANSPORT` row — the same row Phase-B's
/// `take_pending_clearance_grant` consumes. No new schema, no second release path.
///
/// Verify-FIRST, fail-closed: an unsigned / bad-signature / structurally-malformed /
/// expired / replayed grant is rejected + counted and NEVER reaches the store.
/// Returns the store rowid on success.
///
/// **Replay defense (#322).** After the signature verifies, the grant must be both
/// FRESH (`now_ms < expires_at_ms`, else [`RejectReason::Expired`]) and UNSEEN — the
/// nonce is burned via [`VerifierStore::burn_federation_nonce`] in one atomic
/// verify-AND-consume step; a nonce already on record means the same grant was
/// ingested before ([`RejectReason::Replayed`]). The burn happens BEFORE the store
/// write so a replay can never land a duplicate PENDING row.
pub fn ingest_clearance_grant<S: FleetTrustStore>(
    store: &mut S,
    grant: &SignedClearanceGrant,
    public_key_b64: &str,
    counter: &RejectionCounter,
    now_ms: u64,
) -> Result<i64, RejectReason> {
    if grant.signature_b64.trim().is_empty() {
        let r = RejectReason::Unsigned;
        counter.record(&r);
        return Err(r);
    }
    if grant.node_id.trim().is_empty() || grant.operator_id.trim().is_empty() {
        let r = RejectReason::Malformed("empty node_id or operator_id".into());
        counter.record(&r);
        return Err(r);
    }
    if grant.nonce_hex.trim().is_empty() {
        let r = RejectReason::Malformed("empty nonce_hex".into());
        counter.record(&r);
        return Err(r);
    }
    if !verify_clearance_grant(grant, public_key_b64) {
        let r = RejectReason::BadSignature;
        counter.record(&r);
        return Err(r);
    }
    // Authentic — now enforce freshness BEFORE consuming the nonce, so a stale grant
    // never burns a nonce slot.
    if now_ms >= grant.expires_at_ms {
        let r = RejectReason::Expired;
        counter.record(&r);
        return Err(r);
    }
    // Verify-AND-consume: atomically claim the nonce. `Ok(false)` = already burned =
    // a replay of a still-fresh captured grant. Burn BEFORE the store write so a
    // replay can never produce a duplicate PENDING row.
    match store.burn_federation_nonce(&grant.nonce_hex) {
        Ok(true) => {} // first use — proceed
        Ok(false) => {
            let r = RejectReason::Replayed;
            counter.record(&r);
            return Err(r);
        }
        Err(e) => {
            let r = RejectReason::StoreError(e.to_string());
            counter.record(&r);
            return Err(r);
        }
    }
    // Verified, fresh, first-use → the existing Phase-A path (writes the PENDING row +
    // signed audit event). Phase-B picks it up unchanged.
    match store.save_clearance_grant_chained(&grant.node_id, &grant.operator_id, grant.granted_at_ms) {
        Ok(rowid) => {
            counter.record_accepted();
            Ok(rowid)
        }
        Err(e) => {
            let r = RejectReason::StoreError(e.to_string());
            counter.record(&r);
            Err(r)
        }
    }
}

/// Registry-backed [`ingest_clearance_grant`] (#329) — the **fleet-deployment** path.
/// Resolves the grant signer's key from the unified [`KeyRegistry`] (the
/// [`KeyRole::FleetGrant`] registry), then delegates to [`ingest_clearance_grant`].
///
/// Takes `principal_id` rather than a `&KeyRegistry` ON PURPOSE: the registry borrows
/// the store IMMUTABLY but `ingest_clearance_grant` needs it MUTABLY (it burns the
/// nonce + writes the row), so the two borrows cannot coexist. The key is resolved in
/// a scoped immutable borrow that ends BEFORE the mutating store path. The `&str`
/// variant remains the test/spike path. Fail-closed: an unresolvable signer is a
/// counted [`RejectReason::BadSignature`] reject.
pub fn ingest_clearance_grant_from_registry<S: FleetTrustStore>(
    store: &mut S,
    grant: &SignedClearanceGrant,
    principal_id: &str,
    counter: &RejectionCounter,
    now_ms: u64,
) -> Result<i64, RejectReason> {
    let key_b64 = {
        // Scoped immutable borrow for key lookup; it ends before the mutating
        // `ingest_clearance_grant` (which burns the nonce + writes the row).
        match store.resolve_fleet_pubkey(principal_id, FleetKeyRole::FleetGrant) {
            Ok(Some(k)) => B64.encode(k),
            _ => {
                counter.record(&RejectReason::BadSignature);
                return Err(RejectReason::BadSignature);
            }
        }
    };
    ingest_clearance_grant(store, grant, &key_b64, counter, now_ms)
}

#[cfg(test)]
mod core_tests {
    use super::*;
    // R2: the reference `FleetTrustStore` impl used to drive the generic ingest
    // functions in tests (a DEV-dependency; not in the production build graph).
    use kirra_verifier::verifier_store::VerifierStore;
    use ed25519_dalek::SigningKey;

    fn keypair() -> (SigningKey, String) {
        let mut seed = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut seed);
        let sk = SigningKey::from_bytes(&seed);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        (sk, pk_b64)
    }

    /// A genuinely signed report, built the way the verifier signs (over the
    /// canonical v2 payload).
    fn signed_report(sk: &SigningKey, asset: &str, gen: Option<u64>) -> FederatedTrustReportV2 {
        use kirra_fleet_types::federation_reconciliation::canonical_federation_payload_v2;
        let mut report = FederatedTrustReportV2 {
            source_controller_id: "controller-A".into(),
            asset_id: asset.into(),
            posture: FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 6_000,
            nonce_hex: "deadbeef".into(),
            signature_b64: String::new(),
            source_generation: gen,
        };
        let sig = sk.sign(canonical_federation_payload_v2(&report).as_bytes());
        report.signature_b64 = B64.encode(sig.to_bytes());
        report
    }

    #[test]
    fn report_round_trips_and_verifies() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let report = signed_report(&sk, "robot-01", Some(7));
        let bytes = encode_report(&report).unwrap();
        let got = accept_report(&bytes, &pk, &counter).unwrap();
        assert_eq!(got, report);
        assert_eq!(counter.snapshot().accepted, 1);
        assert_eq!(counter.total_rejected(), 0);
    }

    #[test]
    fn tampered_report_is_rejected_and_counted() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let report = signed_report(&sk, "robot-01", Some(7));
        let mut bytes = encode_report(&report).unwrap();
        // Flip a byte in the asset_id region — the signature no longer matches the
        // canonical payload. (Find the 'r' of "robot-01" and bump it.)
        let pos = bytes.windows(8).position(|w| w == b"robot-01").expect("asset in json");
        bytes[pos] ^= 0x01;
        let err = accept_report(&bytes, &pk, &counter).unwrap_err();
        assert_eq!(err, RejectReason::BadSignature, "tamper must be a bad-signature reject");
        assert_eq!(counter.snapshot().bad_signature, 1);
        assert_eq!(counter.snapshot().accepted, 0);
    }

    #[test]
    fn unsigned_report_is_rejected_and_counted() {
        let (_sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let report = FederatedTrustReportV2 {
            source_controller_id: "c".into(),
            asset_id: "robot-01".into(),
            posture: FleetPosture::Nominal,
            issued_at_ms: 1,
            expires_at_ms: 2,
            nonce_hex: "00".into(),
            signature_b64: String::new(), // UNSIGNED
            source_generation: None,
        };
        let bytes = encode_report(&report).unwrap();
        let err = accept_report(&bytes, &pk, &counter).unwrap_err();
        assert_eq!(err, RejectReason::Unsigned);
        assert_eq!(counter.snapshot().unsigned, 1);
    }

    #[test]
    fn report_signed_by_wrong_key_is_bad_signature() {
        let (sk, _pk) = keypair();
        let (_other, attacker_pk) = keypair();
        let counter = RejectionCounter::new();
        let report = signed_report(&sk, "robot-01", None);
        let bytes = encode_report(&report).unwrap();
        // Verify against an UNRELATED key → bad signature, never accepted.
        let err = accept_report(&bytes, &attacker_pk, &counter).unwrap_err();
        assert_eq!(err, RejectReason::BadSignature);
    }

    // --- #322: replay-safe report ingest (ingest_report) ---

    #[test]
    fn report_ingest_accepts_once_and_burns_nonce() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        let report = signed_report(&sk, "robot-01", Some(7)); // issued 1_000, expires 6_000
        let bytes = encode_report(&report).unwrap();

        let got = ingest_report(&mut store, &bytes, &pk, &counter, 1_001).unwrap();
        assert_eq!(got, report);
        assert_eq!(counter.snapshot().accepted, 1);
        // The accepted report burned its nonce — a manual re-burn now returns false.
        assert!(
            !store.burn_federation_nonce(&report.nonce_hex).unwrap(),
            "an accepted report must have burned its nonce"
        );
    }

    #[test]
    fn replayed_report_is_rejected_and_not_re_accepted() {
        // #322 — THE REPORT REPLAY PROOF: the SAME signed report, captured off the
        // untrusted carrier and re-delivered while still fresh, is rejected on the
        // second ingest (its nonce is already burned). accept_report (verify-only)
        // would have replayed it forever; ingest_report does not.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        let report = signed_report(&sk, "robot-07", None);
        let bytes = encode_report(&report).unwrap();

        ingest_report(&mut store, &bytes, &pk, &counter, 1_001).unwrap();
        assert_eq!(counter.snapshot().accepted, 1);

        let err = ingest_report(&mut store, &bytes, &pk, &counter, 1_002).unwrap_err();
        assert_eq!(err, RejectReason::Replayed);
        assert_eq!(counter.snapshot().replayed, 1);
        assert_eq!(counter.snapshot().accepted, 1, "the replay must NOT count as accepted");
    }

    #[test]
    fn expired_report_is_rejected_and_burns_no_nonce() {
        // An authentic-but-stale report replayed past its freshness window is
        // rejected and — crucially — does NOT burn its nonce (freshness gates first).
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        let report = signed_report(&sk, "robot-08", None); // expires_at_ms = 6_000
        let bytes = encode_report(&report).unwrap();

        let err = ingest_report(&mut store, &bytes, &pk, &counter, 6_001).unwrap_err();
        assert_eq!(err, RejectReason::Expired);
        assert_eq!(counter.snapshot().expired, 1);
        assert!(
            store.burn_federation_nonce(&report.nonce_hex).unwrap(),
            "freshness gates before the burn → the nonce must still be free"
        );
    }

    #[test]
    fn report_with_empty_nonce_is_malformed() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        let mut report = signed_report(&sk, "robot-09", None);
        report.nonce_hex = String::new(); // strip the replay nonce
        let bytes = encode_report(&report).unwrap();

        let err = ingest_report(&mut store, &bytes, &pk, &counter, 1_001).unwrap_err();
        assert_eq!(err, RejectReason::Malformed("empty nonce_hex".into()));
        assert_eq!(counter.snapshot().malformed, 1);
    }

    #[test]
    fn report_for_controller_a_rejected_as_b() {
        // A3 — CROSS-CONTROLLER BINDING: a report legitimately signed by
        // controller-A cannot be re-presented as another controller's. The
        // source_controller_id is inside the signed canonical payload, so
        // substituting it breaks the signature → BadSignature, never a nonce burn.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        let mut report = signed_report(&sk, "robot-A", None);
        report.source_controller_id = "controller-B".into(); // re-target after signing
        let bytes = encode_report(&report).unwrap();

        let err = ingest_report(&mut store, &bytes, &pk, &counter, 1_001).unwrap_err();
        assert_eq!(err, RejectReason::BadSignature, "controller-id substitution must break the sig");
        assert_eq!(counter.snapshot().bad_signature, 1);
        assert!(
            store.burn_federation_nonce(&report.nonce_hex).unwrap(),
            "a rejected report must not have burned its nonce"
        );
    }

    #[test]
    fn grant_relay_lands_a_pending_row_that_phase_b_consumes() {
        // THE COMPOSITION PROOF: a verified grant goes through the EXISTING
        // Phase-A store path and is then consumed by the EXISTING Phase-B
        // one-shot pickup — no new schema, no new release path.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let grant = sign_clearance_grant(&sk, "robot-01", "alice", 1_000);
        let rowid = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_001).unwrap();
        assert!(rowid > 0);
        assert_eq!(counter.snapshot().accepted, 1);

        // Phase-B's EXISTING one-shot pickup finds exactly that PENDING row.
        let picked = store.take_pending_clearance_grant("robot-01", 1_500).unwrap();
        let picked = picked.expect("the relayed grant is a pending row Phase-B consumes");
        assert_eq!(picked.node_id, "robot-01");
        assert_eq!(picked.operator_id, "alice");
        assert_eq!(picked.granted_at_ms, 1_000);
        // Exactly once — a second pickup finds nothing.
        assert!(store.take_pending_clearance_grant("robot-01", 1_600).unwrap().is_none());
    }

    #[test]
    fn tampered_grant_never_reaches_the_store() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let mut grant = sign_clearance_grant(&sk, "robot-01", "alice", 1_000);
        // Tamper: change the operator AFTER signing → signature no longer matches.
        grant.operator_id = "mallory".into();
        let err = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_001).unwrap_err();
        assert_eq!(err, RejectReason::BadSignature);
        assert_eq!(counter.snapshot().bad_signature, 1);
        // Nothing was written — Phase-B finds no pending row.
        assert!(store.take_pending_clearance_grant("robot-01", 2_000).unwrap().is_none());
    }

    #[test]
    fn replayed_grant_is_rejected_and_lands_no_second_row() {
        // #322 — THE REPLAY PROOF: the SAME signed grant, captured off the
        // untrusted carrier and re-delivered while still fresh, is rejected on the
        // second ingest (its nonce is already burned) and writes NO second row.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let grant = sign_clearance_grant(&sk, "robot-07", "alice", 1_000);

        // First delivery — accepted, lands the PENDING row.
        let rowid = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_001).unwrap();
        assert!(rowid > 0);
        assert_eq!(counter.snapshot().accepted, 1);

        // Verbatim replay (same bytes, still inside the TTL) — rejected as Replayed.
        let err = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_002).unwrap_err();
        assert_eq!(err, RejectReason::Replayed);
        assert_eq!(counter.snapshot().replayed, 1);
        assert_eq!(counter.snapshot().accepted, 1, "the replay must NOT count as accepted");

        // Exactly one row reached Phase-B — the replay produced no duplicate.
        assert!(store.take_pending_clearance_grant("robot-07", 1_500).unwrap().is_some());
        assert!(store.take_pending_clearance_grant("robot-07", 1_600).unwrap().is_none());
    }

    #[test]
    fn expired_grant_is_rejected_and_burns_no_nonce() {
        // An authentic-but-stale grant replayed past its TTL is rejected as Expired
        // and — crucially — does NOT burn its nonce, so a legitimate re-issue of the
        // same logical grant is unaffected.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let grant = sign_clearance_grant(&sk, "robot-08", "alice", 1_000);
        // now_ms past expires_at_ms (1_000 + FLEET_GRANT_TTL_MS).
        let stale_now = 1_000 + FLEET_GRANT_TTL_MS + 1;
        let err = ingest_clearance_grant(&mut store, &grant, &pk, &counter, stale_now).unwrap_err();
        assert_eq!(err, RejectReason::Expired);
        assert_eq!(counter.snapshot().expired, 1);

        // Nothing written, and the nonce was never burned (freshness gates before burn).
        assert!(store.take_pending_clearance_grant("robot-08", stale_now).unwrap().is_none());
        assert!(
            store.burn_federation_nonce(&grant.nonce_hex).unwrap(),
            "nonce must still be free → first burn returns true"
        );
    }

    #[test]
    fn grant_with_empty_nonce_is_malformed() {
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let mut grant = sign_clearance_grant(&sk, "robot-09", "alice", 1_000);
        grant.nonce_hex = String::new(); // strip the nonce
        let err = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_001).unwrap_err();
        assert_eq!(err, RejectReason::Malformed("empty nonce_hex".into()));
        assert_eq!(counter.snapshot().malformed, 1);
    }

    #[test]
    fn grant_for_node_a_rejected_as_node_b() {
        // A3 — CROSS-NODE BINDING: a grant legitimately signed for node-A cannot be
        // re-presented as node-B's. `node_id` is inside the signed canonical payload,
        // so substituting it breaks the signature → BadSignature, never a store write.
        let (sk, pk) = keypair();
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();

        let mut grant = sign_clearance_grant(&sk, "robot-A", "alice", 1_000);
        // Re-target the (authentically node-A) grant at node-B AFTER signing.
        grant.node_id = "robot-B".into();
        let err = ingest_clearance_grant(&mut store, &grant, &pk, &counter, 1_001).unwrap_err();
        assert_eq!(err, RejectReason::BadSignature, "node_id substitution must break the sig");
        assert_eq!(counter.snapshot().bad_signature, 1);
        // Neither node sees a pending row — the cross-node grant reached no store.
        assert!(store.take_pending_clearance_grant("robot-A", 2_000).unwrap().is_none());
        assert!(store.take_pending_clearance_grant("robot-B", 2_000).unwrap().is_none());
    }

    /// #329 — THE REGISTRY COMPOSITION PROOF: the fleet-deployment ingest resolves the
    /// grant signer's key from the unified `KeyRegistry` (a STORED federation-controller
    /// registration), not a caller-supplied string. A registered signer's grant lands
    /// the PENDING row Phase-B consumes; an unregistered signer is fail-closed.
    #[test]
    fn e2e_fleet_ingest_via_registry() {
        let (sk, pk_b64) = keypair(); // pk_b64 = raw-32 verifying key (the controller registry format)
        let counter = RejectionCounter::new();
        let mut store = VerifierStore::new(":memory:").unwrap();
        // Register the fleet-grant signer in the trusted-controller registry.
        store.save_trusted_federation_controller("ops-controller", &pk_b64, 1).unwrap();

        // Sign + ingest THROUGH THE REGISTRY (no caller-supplied key string).
        let grant = sign_clearance_grant(&sk, "robot-77", "alice", 1_000);
        let rowid =
            ingest_clearance_grant_from_registry(&mut store, &grant, "ops-controller", &counter, 1_001)
                .unwrap();
        assert!(rowid > 0);
        assert_eq!(counter.snapshot().accepted, 1);

        // The registry-resolved verification produced the EXISTING Phase-B pending row.
        let picked = store
            .take_pending_clearance_grant("robot-77", 1_500)
            .unwrap()
            .expect("registry-ingested grant is a pending row Phase-B consumes");
        assert_eq!(picked.operator_id, "alice");

        // An UNREGISTERED signer principal is fail-closed (BadSignature) — nothing written.
        let grant2 = sign_clearance_grant(&sk, "robot-78", "bob", 2_000);
        let err =
            ingest_clearance_grant_from_registry(&mut store, &grant2, "no-such-controller", &counter, 2_001)
                .unwrap_err();
        assert_eq!(err, RejectReason::BadSignature, "an unresolvable signer is fail-closed");
        assert!(store.take_pending_clearance_grant("robot-78", 2_500).unwrap().is_none());
    }
}
