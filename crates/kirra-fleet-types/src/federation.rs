// src/federation.rs

use kirra_core::FleetPosture;
use serde::{Deserialize, Serialize};

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

    let Ok(pk_bytes) = b64.decode(public_key_b64) else {
        return false;
    };
    let Ok(sig_bytes) = b64.decode(&report.signature_b64) else {
        return false;
    };

    let Ok(pk_array) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };

    let Ok(key) = VerifyingKey::from_bytes(&pk_array) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig_array);

    // verify_strict (not verify) rejects signature malleability and non-canonical
    // / small-order edge cases, matching attestation.rs, tpm_quote.rs, and
    // key_registry.rs. Fail-closed on any cryptographic failure.
    key.verify_strict(canonical_federation_payload(report).as_bytes(), &sig)
        .is_ok()
}

/// True iff `public_key_b64` decodes (Base64 STANDARD) to a valid 32-byte Ed25519
/// verifying key — the EXACT format `verify_federated_report_signature` expects
/// (base64 → `[u8; 32]` → `VerifyingKey::from_bytes`).
///
/// A trusted-controller registration should FAIL FAST on a malformed key rather
/// than storing a string that can never verify and only surfacing the error at the
/// first report (a confusing, deferred failure). This mirrors the parse-at-boundary
/// discipline the attestation/operator key-registration paths already apply. Pure /
/// side-effect-free; the caller maps `false` to a 422.
pub fn federation_public_key_is_valid(public_key_b64: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
    use ed25519_dalek::VerifyingKey;

    let Ok(pk_bytes) = b64.decode(public_key_b64) else {
        return false;
    };
    let Ok(pk_array) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
        return false;
    };
    VerifyingKey::from_bytes(&pk_array).is_ok()
}

/// Evaluates structural completeness, chronological freshness, and replay window.
/// Cryptographic signature verification and nonce uniqueness are enforced by the caller.
pub fn evaluate_federated_report(
    report: &FederatedTrustReport,
    current_time_ms: u64,
) -> ReportEvaluation {
    if report.source_controller_id.is_empty() {
        return ReportEvaluation {
            accepted: false,
            reason: "MISSING_SOURCE_CONTROLLER".to_string(),
        };
    }
    if report.asset_id.is_empty() {
        return ReportEvaluation {
            accepted: false,
            reason: "MISSING_ASSET_ID".to_string(),
        };
    }
    if report.nonce_hex.is_empty() {
        return ReportEvaluation {
            accepted: false,
            reason: "MISSING_NONCE".to_string(),
        };
    }
    if report.issued_at_ms > current_time_ms {
        return ReportEvaluation {
            accepted: false,
            reason: "REPORT_TIMELINE_FUTURE_INVALID".to_string(),
        };
    }
    // Enforce the local replay window: reject reports issued more than FEDERATION_REPLAY_WINDOW_MS ago.
    if current_time_ms.saturating_sub(report.issued_at_ms) > FEDERATION_REPLAY_WINDOW_MS {
        return ReportEvaluation {
            accepted: false,
            reason: "REPORT_ISSUED_OUTSIDE_REPLAY_WINDOW".to_string(),
        };
    }
    if current_time_ms >= report.expires_at_ms {
        return ReportEvaluation {
            accepted: false,
            reason: "REPORT_STALE_EXPIRED".to_string(),
        };
    }

    ReportEvaluation {
        accepted: true,
        reason: "FEDERATED_OBSERVATION_RECORDED".to_string(),
    }
}

#[cfg(test)]
mod key_validation_tests {
    use super::federation_public_key_is_valid;
    use base64::{engine::general_purpose::STANDARD as b64, Engine as _};

    #[test]
    fn accepts_a_real_ed25519_key_and_rejects_malformed() {
        // A real verifying key: base64 of its 32 raw bytes (the format the verify
        // path decodes). from_bytes must accept it.
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let good = b64.encode(sk.verifying_key().to_bytes());
        assert!(
            federation_public_key_is_valid(&good),
            "a real key is accepted"
        );

        // Not base64.
        assert!(!federation_public_key_is_valid("!!! not base64 !!!"));
        // Valid base64 but the wrong length (16 bytes, not 32).
        assert!(!federation_public_key_is_valid(&b64.encode([0u8; 16])));
        // Empty string.
        assert!(!federation_public_key_is_valid(""));
    }
}
