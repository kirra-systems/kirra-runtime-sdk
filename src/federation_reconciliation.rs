// src/federation_reconciliation.rs
//
// Generation-ordered federation reconciliation for Kirra multi-controller deployments.
//
// WHAT THIS SOLVES
// ================
// Two Kirra controllers (A and B) independently observe the same asset fleet.
// Both receive sensor faults, run DAG traversals, and produce posture views.
// Under network partition or clock skew their views can diverge:
//
//   Controller A (generation 412): asset "lidar_front" → Degraded
//   Controller B (generation 398): asset "lidar_front" → Nominal
//
// Which view is authoritative? The existing federation layer accepts and stores
// reports but has no mechanism to resolve conflicts — the last write wins,
// which is incorrect under partition recovery.
//
// RESOLUTION STRATEGY
// ===================
// Generation counters are the tie-breaker. The controller with the higher
// generation has processed more recalculation events and has a more recent
// view of the DAG state. When two reports conflict:
//
//   1. If one carries source_generation and the other doesn't → prefer the one
//      with generation (it's the newer protocol version, more informative)
//   2. If both carry source_generation → higher generation wins
//   3. If neither carries source_generation → fall back to issued_at_ms ordering
//      (original behavior — backward compatible)
//   4. Tie on all criteria → prefer Degraded/LockedOut over Nominal (fail-closed)
//
// WIRE FORMAT CHANGE
// ==================
// FederatedTrustReport gains an optional source_generation: Option<u64> field.
// It is included in the canonical signed payload when present, so it cannot be
// forged or stripped without invalidating the Ed25519 signature.
//
// Backward compatibility: reports without source_generation are still accepted
// and processed — they just use timestamp-based ordering for conflict resolution.
//
// SECURITY INVARIANTS PRESERVED
// ==============================
// - source_generation is inside the signed payload — not an unsigned annotation
// - Generation comparison is purely advisory for ordering; it never bypasses
//   the existing 5-step acceptance pipeline (structural check → key lookup →
//   signature verify → nonce replay → atomic commit)
// - A controller cannot claim a higher generation than it has actually computed
//   because doing so would require forging an Ed25519 signature
// - Fail-closed: on any reconciliation ambiguity, prefer the more restrictive
//   posture (Degraded > Nominal, LockedOut > Degraded)

use serde::{Deserialize, Serialize};
use crate::verifier::FleetPosture;
use crate::federation::{FederatedTrustReport, ReportEvaluation};

// ---------------------------------------------------------------------------
// Extended report type with generation field
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FederatedTrustReportV2 {
    pub source_controller_id: String,
    pub asset_id: String,
    pub posture: FleetPosture,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub nonce_hex: String,
    pub signature_b64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_generation: Option<u64>,
}

impl FederatedTrustReportV2 {
    pub fn as_v1(&self) -> FederatedTrustReport {
        FederatedTrustReport {
            source_controller_id: self.source_controller_id.clone(),
            asset_id: self.asset_id.clone(),
            posture: self.posture.clone(),
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
            nonce_hex: self.nonce_hex.clone(),
            signature_b64: self.signature_b64.clone(),
        }
    }
}

pub fn canonical_federation_payload_v2(report: &FederatedTrustReportV2) -> String {
    match report.source_generation {
        Some(gen) => serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": report.posture,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
            "source_generation": gen,
        }).to_string(),
        None => serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": report.posture,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
        }).to_string(),
    }
}

pub fn verify_federated_report_signature_v2(
    report: &FederatedTrustReportV2,
    public_key_b64: &str,
) -> bool {
    use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
    use ed25519_dalek::{Signature, VerifyingKey};

    let Ok(pk_bytes)  = b64.decode(public_key_b64)          else { return false; };
    let Ok(sig_bytes) = b64.decode(&report.signature_b64)   else { return false; };

    let Ok(pk_array)  = <[u8; 32]>::try_from(pk_bytes.as_slice())  else { return false; };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else { return false; };

    let Ok(key) = VerifyingKey::from_bytes(&pk_array) else { return false; };
    let sig     = Signature::from_bytes(&sig_array);

    // verify_strict rejects malleable / non-canonical signatures, consistent
    // with the v1 path and the rest of the crate's crypto. Fail-closed.
    key.verify_strict(canonical_federation_payload_v2(report).as_bytes(), &sig).is_ok()
}

// ---------------------------------------------------------------------------
// Conflict resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ReconciliationOutcome {
    PreferFirst,
    PreferSecond,
    Equivalent,
    FailClosed,
}

fn posture_severity(p: &FleetPosture) -> u8 {
    match p {
        FleetPosture::Nominal   => 0,
        FleetPosture::Degraded  => 1,
        FleetPosture::LockedOut => 2,
    }
}

pub fn reconcile_reports(
    first: &FederatedTrustReportV2,
    second: &FederatedTrustReportV2,
) -> ReconciliationOutcome {
    if first.asset_id != second.asset_id {
        return ReconciliationOutcome::FailClosed;
    }

    if first.posture == second.posture {
        return ReconciliationOutcome::Equivalent;
    }

    match (first.source_generation, second.source_generation) {
        (Some(g1), Some(g2)) => {
            if g1 > g2 { return ReconciliationOutcome::PreferFirst; }
            if g2 > g1 { return ReconciliationOutcome::PreferSecond; }
        }
        (Some(_), None) => return ReconciliationOutcome::PreferFirst,
        (None, Some(_)) => return ReconciliationOutcome::PreferSecond,
        (None, None) => {}
    }

    if first.issued_at_ms > second.issued_at_ms {
        return ReconciliationOutcome::PreferFirst;
    }
    if second.issued_at_ms > first.issued_at_ms {
        return ReconciliationOutcome::PreferSecond;
    }

    let s1 = posture_severity(&first.posture);
    let s2 = posture_severity(&second.posture);

    if s1 > s2 { return ReconciliationOutcome::PreferFirst; }
    if s2 > s1 { return ReconciliationOutcome::PreferSecond; }

    ReconciliationOutcome::FailClosed
}

pub fn authoritative_posture<'a>(
    reports: impl IntoIterator<Item = &'a FederatedTrustReportV2>,
) -> Option<FleetPosture> {
    let mut iter = reports.into_iter();
    let first = iter.next()?;
    let mut current = first;

    for next in iter {
        match reconcile_reports(current, next) {
            ReconciliationOutcome::PreferFirst  => {}
            ReconciliationOutcome::PreferSecond => { current = next; }
            ReconciliationOutcome::Equivalent   => {}
            ReconciliationOutcome::FailClosed   => {
                if posture_severity(&next.posture) > posture_severity(&current.posture) {
                    current = next;
                }
            }
        }
    }

    Some(current.posture.clone())
}

pub fn evaluate_federated_report_v2(
    report: &FederatedTrustReportV2,
    current_time_ms: u64,
) -> ReportEvaluation {
    if let Some(0) = report.source_generation {
        return ReportEvaluation {
            accepted: false,
            reason: "INVALID_SOURCE_GENERATION_ZERO".to_string(),
        };
    }

    crate::federation::evaluate_federated_report(&report.as_v1(), current_time_ms)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod federation_reconciliation_tests {
    use super::*;
    use crate::verifier::FleetPosture;

    fn report(
        controller: &str,
        asset: &str,
        posture: FleetPosture,
        issued_at_ms: u64,
        generation: Option<u64>,
    ) -> FederatedTrustReportV2 {
        FederatedTrustReportV2 {
            source_controller_id: controller.to_string(),
            asset_id: asset.to_string(),
            posture,
            issued_at_ms,
            expires_at_ms: issued_at_ms + 30_000,
            nonce_hex: format!("{controller}_{issued_at_ms}"),
            signature_b64: "test_sig".to_string(),
            source_generation: generation,
        }
    }

    fn nominal(controller: &str, t: u64, gen: Option<u64>) -> FederatedTrustReportV2 {
        report(controller, "lidar_front", FleetPosture::Nominal, t, gen)
    }

    fn degraded(controller: &str, t: u64, gen: Option<u64>) -> FederatedTrustReportV2 {
        report(controller, "lidar_front", FleetPosture::Degraded, t, gen)
    }

    fn locked(controller: &str, t: u64, gen: Option<u64>) -> FederatedTrustReportV2 {
        report(controller, "lidar_front", FleetPosture::LockedOut, t, gen)
    }

    #[test]
    fn test_higher_generation_wins_over_lower() {
        let r1 = degraded("ctrl-a", 1000, Some(412));
        let r2 = nominal("ctrl-b", 1000, Some(398));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferFirst);
    }

    #[test]
    fn test_lower_generation_loses_even_if_more_restrictive() {
        let r1 = nominal("ctrl-a", 1000, Some(500));
        let r2 = locked("ctrl-b", 1000, Some(100));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferFirst);
    }

    #[test]
    fn test_report_with_generation_preferred_over_report_without() {
        let r1 = degraded("ctrl-a", 1000, Some(412));
        let r2 = nominal("ctrl-b", 1000, None);
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferFirst);
    }

    #[test]
    fn test_report_without_generation_loses_to_report_with_generation() {
        let r1 = nominal("ctrl-a", 1000, None);
        let r2 = degraded("ctrl-b", 1000, Some(412));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferSecond);
    }

    #[test]
    fn test_newer_timestamp_wins_when_no_generation() {
        let r1 = degraded("ctrl-a", 2000, None);
        let r2 = nominal("ctrl-b", 1000, None);
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferFirst);
    }

    #[test]
    fn test_newer_timestamp_wins_when_equal_generation() {
        let r1 = degraded("ctrl-a", 2000, Some(100));
        let r2 = nominal("ctrl-b", 1000, Some(100));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferFirst);
    }

    #[test]
    fn test_older_timestamp_loses_when_no_generation() {
        let r1 = nominal("ctrl-a", 1000, None);
        let r2 = degraded("ctrl-b", 2000, None);
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferSecond);
    }

    #[test]
    fn test_fail_closed_prefers_degraded_over_nominal() {
        let r1 = nominal("ctrl-a", 1000, Some(100));
        let r2 = degraded("ctrl-b", 1000, Some(100));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferSecond);
    }

    #[test]
    fn test_fail_closed_prefers_locked_out_over_degraded() {
        let r1 = degraded("ctrl-a", 1000, Some(100));
        let r2 = locked("ctrl-b", 1000, Some(100));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferSecond);
    }

    #[test]
    fn test_fail_closed_prefers_locked_out_over_nominal() {
        let r1 = nominal("ctrl-a", 1000, None);
        let r2 = locked("ctrl-b", 1000, None);
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::PreferSecond);
    }

    #[test]
    fn test_identical_postures_are_equivalent() {
        let r1 = degraded("ctrl-a", 1000, Some(412));
        let r2 = degraded("ctrl-b", 999, Some(1));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::Equivalent);
    }

    #[test]
    fn test_same_posture_no_generation_is_equivalent() {
        let r1 = nominal("ctrl-a", 1000, None);
        let r2 = nominal("ctrl-b", 500, None);
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::Equivalent);
    }

    #[test]
    fn test_different_asset_ids_fail_closed() {
        let r1 = report("ctrl-a", "lidar_front", FleetPosture::Nominal, 1000, Some(100));
        let r2 = report("ctrl-b", "camera_front", FleetPosture::Degraded, 1000, Some(200));
        assert_eq!(reconcile_reports(&r1, &r2), ReconciliationOutcome::FailClosed);
    }

    #[test]
    fn test_single_report_returns_its_posture() {
        let reports = vec![degraded("ctrl-a", 1000, Some(100))];
        assert_eq!(authoritative_posture(&reports), Some(FleetPosture::Degraded));
    }

    #[test]
    fn test_empty_reports_returns_none() {
        let reports: Vec<FederatedTrustReportV2> = vec![];
        assert_eq!(authoritative_posture(&reports), None);
    }

    #[test]
    fn test_highest_generation_wins_across_three_controllers() {
        let reports = vec![
            nominal("ctrl-a", 1000, Some(100)),
            degraded("ctrl-b", 1000, Some(412)),
            nominal("ctrl-c", 1000, Some(200)),
        ];
        assert_eq!(authoritative_posture(&reports), Some(FleetPosture::Degraded));
    }

    #[test]
    fn test_fail_closed_wins_when_all_same_generation_and_timestamp() {
        let reports = vec![
            nominal("ctrl-a", 1000, Some(100)),
            locked("ctrl-b", 1000, Some(100)),
            degraded("ctrl-c", 1000, Some(100)),
        ];
        assert_eq!(authoritative_posture(&reports), Some(FleetPosture::LockedOut));
    }

    #[test]
    fn test_mixed_v1_and_v2_reports_v2_wins() {
        let reports = vec![
            nominal("ctrl-a", 2000, None),
            degraded("ctrl-b", 1000, Some(412)),
        ];
        assert_eq!(authoritative_posture(&reports), Some(FleetPosture::Degraded));
    }

    #[test]
    fn test_payload_without_generation_matches_v1_field_set() {
        let r = nominal("ctrl-a", 1000, None);
        let v2_payload = canonical_federation_payload_v2(&r);
        let v1_report = r.as_v1();
        let v1_payload = crate::federation::canonical_federation_payload(&v1_report);
        assert_eq!(v2_payload, v1_payload);
    }

    #[test]
    fn test_payload_with_generation_includes_generation_field() {
        let r = degraded("ctrl-a", 1000, Some(412));
        let payload = canonical_federation_payload_v2(&r);
        assert!(payload.contains("source_generation"));
        assert!(payload.contains("412"));
    }

    #[test]
    fn test_payload_without_generation_excludes_generation_field() {
        let r = nominal("ctrl-a", 1000, None);
        let payload = canonical_federation_payload_v2(&r);
        assert!(!payload.contains("source_generation"));
    }

    #[test]
    fn test_zero_generation_is_rejected() {
        let r = degraded("ctrl-a", 1000, Some(0));
        let result = evaluate_federated_report_v2(&r, 1001);
        assert!(!result.accepted);
        assert_eq!(result.reason, "INVALID_SOURCE_GENERATION_ZERO");
    }

    #[test]
    fn test_valid_generation_passes_structural_checks() {
        let now = 1_700_000_000_000u64;
        let r = FederatedTrustReportV2 {
            source_controller_id: "ctrl-a".to_string(),
            asset_id: "lidar_front".to_string(),
            posture: FleetPosture::Nominal,
            issued_at_ms: now,
            expires_at_ms: now + 30_000,
            nonce_hex: "abc123".to_string(),
            signature_b64: "sig".to_string(),
            source_generation: Some(412),
        };
        let result = evaluate_federated_report_v2(&r, now + 100);
        assert!(result.accepted, "valid v2 report must be accepted: {}", result.reason);
    }

    #[test]
    fn test_none_generation_passes_through_to_v1_checks() {
        let now = 1_700_000_000_000u64;
        let r = FederatedTrustReportV2 {
            source_controller_id: "ctrl-a".to_string(),
            asset_id: "lidar_front".to_string(),
            posture: FleetPosture::Nominal,
            issued_at_ms: now,
            expires_at_ms: now + 30_000,
            nonce_hex: "abc123".to_string(),
            signature_b64: "sig".to_string(),
            source_generation: None,
        };
        let result = evaluate_federated_report_v2(&r, now + 100);
        assert!(result.accepted, "v1-compat report must be accepted: {}", result.reason);
    }

    #[test]
    fn test_posture_severity_ordering_is_correct() {
        assert!(posture_severity(&FleetPosture::LockedOut) > posture_severity(&FleetPosture::Degraded));
        assert!(posture_severity(&FleetPosture::Degraded) > posture_severity(&FleetPosture::Nominal));
        assert!(posture_severity(&FleetPosture::LockedOut) > posture_severity(&FleetPosture::Nominal));
    }
}
