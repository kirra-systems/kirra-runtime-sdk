// src/federation.rs

use serde::{Deserialize, Serialize};
use kirra_core::FleetPosture;

/// Maximum age of a federated report's issued_at_ms relative to the local clock.
/// Reports older than this window are rejected even if not yet expired, preventing
/// delayed-delivery replay attacks.
pub const FEDERATION_REPLAY_WINDOW_MS: u64 = 5_000;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FederatedTrustReport {
    pub source_controller_id: String,
    pub asset_id: String,
    pub posture: FleetPosture,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    /// Hex-encoded unique nonce; consumed on first acceptance to prevent replay.
    pub nonce_hex: String,
    /// Base64-encoded Ed25519 signature over the canonical payload.
    pub signature_b64: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegisterFederationControllerRequest {
    pub controller_id: String,
    pub public_key_b64: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportEvaluation {
    pub accepted: bool,
    pub reason: String,
}

/// Builds a strictly ordered, deterministic JSON payload for signature verification.
/// Only the fields that were signed are included; signature_b64 is intentionally excluded.
pub fn canonical_federation_payload(report: &FederatedTrustReport) -> String {
    serde_json::json!({
        "source_controller_id": report.source_controller_id,
        "asset_id": report.asset_id,
        "posture": report.posture,
        "issued_at_ms": report.issued_at_ms,
        "expires_at_ms": report.expires_at_ms,
        "nonce_hex": report.nonce_hex,
    })
    .to_string()
}

/// Decodes Base64 parameters and verifies an Ed25519 signature over the canonical payload.
/// Returns false on any parse, length, or cryptographic failure (fail-closed).
pub fn verify_federated_report_signature(
    report: &FederatedTrustReport,
    public_key_b64: &str,
) -> bool {
    use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
    use ed25519_dalek::{Signature, VerifyingKey};

    let Ok(pk_bytes) = b64.decode(public_key_b64) else { return false; };
    let Ok(sig_bytes) = b64.decode(&report.signature_b64) else { return false; };

    let Ok(pk_array) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else { return false; };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else { return false; };

    let Ok(key) = VerifyingKey::from_bytes(&pk_array) else { return false; };
    let sig = Signature::from_bytes(&sig_array);

    // verify_strict (not verify) rejects signature malleability and non-canonical
    // / small-order edge cases, matching attestation.rs, tpm_quote.rs, and
    // key_registry.rs. Fail-closed on any cryptographic failure.
    key.verify_strict(canonical_federation_payload(report).as_bytes(), &sig).is_ok()
}

/// Evaluates structural completeness, chronological freshness, and replay window.
/// Cryptographic signature verification and nonce uniqueness are enforced by the caller.
pub fn evaluate_federated_report(report: &FederatedTrustReport, current_time_ms: u64) -> ReportEvaluation {
    if report.source_controller_id.is_empty() {
        return ReportEvaluation { accepted: false, reason: "MISSING_SOURCE_CONTROLLER".to_string() };
    }
    if report.asset_id.is_empty() {
        return ReportEvaluation { accepted: false, reason: "MISSING_ASSET_ID".to_string() };
    }
    if report.nonce_hex.is_empty() {
        return ReportEvaluation { accepted: false, reason: "MISSING_NONCE".to_string() };
    }
    if report.issued_at_ms > current_time_ms {
        return ReportEvaluation { accepted: false, reason: "REPORT_TIMELINE_FUTURE_INVALID".to_string() };
    }
    // Enforce the local replay window: reject reports issued more than FEDERATION_REPLAY_WINDOW_MS ago.
    if current_time_ms.saturating_sub(report.issued_at_ms) > FEDERATION_REPLAY_WINDOW_MS {
        return ReportEvaluation { accepted: false, reason: "REPORT_ISSUED_OUTSIDE_REPLAY_WINDOW".to_string() };
    }
    if current_time_ms >= report.expires_at_ms {
        return ReportEvaluation { accepted: false, reason: "REPORT_STALE_EXPIRED".to_string() };
    }

    ReportEvaluation { accepted: true, reason: "FEDERATED_OBSERVATION_RECORDED".to_string() }
}
