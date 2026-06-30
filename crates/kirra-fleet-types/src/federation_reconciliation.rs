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
//
// CR1 / #692 DECISION
// ===================
// Generation-ordering is KEPT authoritative even when it lets a higher-generation
// `Nominal` win over a lagging peer's `LockedOut`. That is intentional: a STALE
// `LockedOut` from a dead/lagging controller must not mask a fresh `Nominal`
// forever, and severity-wins-among-all would let exactly that happen. To avoid
// blinding an operator to a genuine lockout, the advisory view ALSO carries a
// dissent overlay (`dissenting_restriction`) that surfaces the most restrictive
// posture among still-FRESH reports above the authoritative value. The overlay
// is additive and fail-safe (only ever adds caution) and advisory-only — it does
// NOT feed the actuator-gating posture engine.

use serde::{Deserialize, Serialize};
use kirra_core::FleetPosture;
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
            posture: self.posture,
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

    Some(current.posture)
}

/// CR1 (#692): the dissent overlay for the advisory federated view.
///
/// `authoritative_posture` is generation-ordered, so a controller merely AHEAD
/// in recalc count can present `Nominal` over a lagging peer's genuine
/// `LockedOut`. That ordering is intentional — a STALE `LockedOut` from a
/// dead/lagging peer must not mask a fresh `Nominal` forever — but an operator
/// must never be BLIND to a live lockout. This returns the MOST restrictive
/// posture among reports that are still FRESH (not expired at `now_ms`) **and**
/// strictly more restrictive than `authoritative`; `None` when nothing fresh
/// dissents upward.
///
/// Purely additive and fail-safe: it only ever surfaces MORE caution, never
/// relaxes the authoritative value, and is bounded to the advisory read path
/// (it does not feed the actuator-gating posture engine). Freshness uses the
/// report's own signed `expires_at_ms`, so an expired dissent self-clears.
pub fn dissenting_restriction<'a>(
    reports: impl IntoIterator<Item = &'a FederatedTrustReportV2>,
    authoritative: FleetPosture,
    now_ms: u64,
) -> Option<FleetPosture> {
    reports
        .into_iter()
        .filter(|r| now_ms < r.expires_at_ms) // fresh only — expired dissent self-clears
        .map(|r| r.posture)
        .filter(|p| posture_severity(p) > posture_severity(&authoritative))
        .max_by_key(posture_severity)
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
    use kirra_core::FleetPosture;

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

    /// Item 20 — BYTE-STABILITY pin. The canonical payload is the exact byte string
    /// the Ed25519 signature is computed over; any reordering, key rename, or
    /// whitespace change silently breaks cross-controller verification (a signed
    /// report from peer B would no longer verify here). These assertions pin the
    /// serialized bytes so such a regression fails CI instead of the field.
    #[test]
    fn test_canonical_payload_v2_byte_stability_with_generation() {
        let r = degraded("ctrl-a", 1000, Some(412));
        let payload = canonical_federation_payload_v2(&r);
        // serde_json serializes a Map with sorted keys (no preserve_order feature),
        // so the field order is deterministic and alphabetical.
        assert_eq!(
            payload,
            r#"{"asset_id":"lidar_front","expires_at_ms":31000,"issued_at_ms":1000,"nonce_hex":"ctrl-a_1000","posture":"Degraded","source_controller_id":"ctrl-a","source_generation":412}"#
        );
    }

    /// Item 20 — BYTE-STABILITY pin for the v1-compat (no-generation) payload. It
    /// must be byte-identical to the v1 canonical payload (no `source_generation`
    /// key at all), or a v1 controller's signature would fail to verify on the v2
    /// path. This is the wire-compat contract, asserted on exact bytes.
    #[test]
    fn test_canonical_payload_v2_byte_stability_without_generation() {
        let r = nominal("ctrl-a", 1000, None);
        let payload = canonical_federation_payload_v2(&r);
        assert_eq!(
            payload,
            r#"{"asset_id":"lidar_front","expires_at_ms":31000,"issued_at_ms":1000,"nonce_hex":"ctrl-a_1000","posture":"Nominal","source_controller_id":"ctrl-a"}"#
        );
        // And it equals the v1 canonical payload byte-for-byte (the compat contract).
        assert_eq!(payload, crate::federation::canonical_federation_payload(&r.as_v1()));
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

    // -----------------------------------------------------------------------
    // CR1 (#692): dissent overlay — surfaces a fresh, more-restrictive posture
    // that the generation-ordered authoritative value masks.
    // -----------------------------------------------------------------------

    #[test]
    fn test_dissent_surfaces_fresh_lockout_masked_by_higher_gen_nominal() {
        // The exact CR1 case: gen-500 Nominal is authoritative, but a gen-100
        // peer is LockedOut and still fresh — the overlay must surface it.
        let reports = vec![
            nominal("ctrl-a", 1000, Some(500)),
            locked("ctrl-b", 1000, Some(100)),
        ];
        let auth = authoritative_posture(&reports).unwrap();
        assert_eq!(auth, FleetPosture::Nominal, "gen-ordered authoritative is unchanged");
        // Reports expire at 31_000 (issued + 30s); now=1_500 is within window.
        assert_eq!(
            dissenting_restriction(&reports, auth, 1_500),
            Some(FleetPosture::LockedOut),
        );
    }

    #[test]
    fn test_dissent_ignores_expired_lockout() {
        // Same shape, but evaluated AFTER the LockedOut report has expired — the
        // overlay self-clears (a dead peer's stale lockout must not linger).
        let reports = vec![
            nominal("ctrl-a", 1000, Some(500)),
            locked("ctrl-b", 1000, Some(100)),
        ];
        let auth = authoritative_posture(&reports).unwrap();
        assert_eq!(dissenting_restriction(&reports, auth, 31_001), None);
    }

    #[test]
    fn test_dissent_none_when_authoritative_already_most_restrictive() {
        // Authoritative is LockedOut; nothing can dissent ABOVE it.
        let reports = vec![
            locked("ctrl-a", 1000, Some(500)),
            nominal("ctrl-b", 1000, Some(100)),
        ];
        let auth = authoritative_posture(&reports).unwrap();
        assert_eq!(auth, FleetPosture::LockedOut);
        assert_eq!(dissenting_restriction(&reports, auth, 1_500), None);
    }

    #[test]
    fn test_dissent_picks_most_restrictive_among_several() {
        // Authoritative Nominal; fresh peers at Degraded and LockedOut → surface
        // the most restrictive (LockedOut), not merely the first dissent.
        let reports = vec![
            nominal("ctrl-a", 1000, Some(500)),
            degraded("ctrl-b", 1000, Some(300)),
            locked("ctrl-c", 1000, Some(100)),
        ];
        let auth = authoritative_posture(&reports).unwrap();
        assert_eq!(auth, FleetPosture::Nominal);
        assert_eq!(
            dissenting_restriction(&reports, auth, 1_500),
            Some(FleetPosture::LockedOut),
        );
    }

    #[test]
    fn test_dissent_surfaces_degraded_when_no_fresh_lockout() {
        // Authoritative Nominal; the only fresh dissent is Degraded (the
        // LockedOut peer has expired) → surface Degraded.
        let reports = vec![
            nominal("ctrl-a", 5000, Some(500)),
            degraded("ctrl-b", 5000, Some(300)),
            locked("ctrl-c", 1000, Some(100)), // expires at 31_000
        ];
        let auth = authoritative_posture(&reports).unwrap();
        assert_eq!(
            dissenting_restriction(&reports, auth, 31_500),
            Some(FleetPosture::Degraded),
        );
    }
}
