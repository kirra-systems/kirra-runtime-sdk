// src/command_source.rs
//
// #111 (CommandSource definition) + #112 (command_source handoff provenance).
//
// SG7 — READ THIS. This is the AUDIT / INGRESS layer. The Governor verdict
// (`validate_vehicle_command` / `classify_http_command`) is and stays
// SOURCE-BLIND: it takes no source parameter (SG7 "doer-agnostic verdict",
// guarded by `sg7_doer_agnostic_verdict_byte_identical_across_ingress_paths` in
// `gateway/policy_layer.rs`). Command source is recorded HERE — where it is
// already known at ingress — and is NEVER threaded into the verdict. Nothing in
// this module is reachable from the verdict path.
//
// OBSERVABILITY ONLY. A handoff event is forensic: it emits into the EXISTING
// signed, hash-chained verifier audit log via
// `VerifierStore::save_posture_event_chained` → `audit_chain::append_audit_event_tx`
// (the same ledger as posture transitions / the #247 divergence sink — no new
// sink; #165 key-trust applies). A failed write bumps the operator-observable
// `command_source_write_failures` counter (#245/#247 pattern) and is dropped —
// it never blocks or alters the control path.

use std::sync::atomic::Ordering;

use crate::verifier::AppState;

/// Audit event type for a control-authority transition (SCREAMING_SNAKE,
/// matching the existing vocabulary; no collision with existing strings).
pub const COMMAND_SOURCE_HANDOFF: &str = "COMMAND_SOURCE_HANDOFF";

/// `node_id` column value for command-source rows.
const COMMAND_SOURCE_NODE_ID: &str = "command_source";

/// #111 — who authored a command, recorded at the ingress/audit layer (NEVER in
/// the verdict). The `Unattributed` variant mirrors the existing
/// `CANOPEN_NMT_OFFLINE_UNATTRIBUTED` honesty: a handoff whose source cannot be
/// attributed is recorded AS unattributed, never dropped or guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    /// The autonomous planner / AI stack.
    AutonomousPlanner,
    /// A human teleoperator.
    Teleoperator,
    /// An HA standby controller that took over command authority.
    StandbyController,
    /// The minimum-risk-condition safe-stop authority (decel-to-stop / hold).
    MrcSafeStop,
    /// Fail-closed: the source could not be attributed. Recorded explicitly,
    /// never silently dropped (cf. `CANOPEN_NMT_OFFLINE_UNATTRIBUTED`).
    Unattributed,
}

impl CommandSource {
    /// Stable audit-payload rendering (do not change without an audit-format
    /// migration — the string is bound into the chain hash).
    pub const fn as_str(self) -> &'static str {
        match self {
            CommandSource::AutonomousPlanner => "AutonomousPlanner",
            CommandSource::Teleoperator => "Teleoperator",
            CommandSource::StandbyController => "StandbyController",
            CommandSource::MrcSafeStop => "MrcSafeStop",
            CommandSource::Unattributed => "Unattributed",
        }
    }
}

fn note_failure(app: &AppState, why: &str) {
    app.off_path_writes
        .command_source_write_failures
        .fetch_add(1, Ordering::SeqCst);
    tracing::error!(
        error = %why,
        "COMMAND_SOURCE_HANDOFF audit write failed — provenance event missing from \
         the chain (observability only; the safety verdict path is unaffected)"
    );
}

/// #112 — emit a `COMMAND_SOURCE_HANDOFF` audit event recording a
/// control-authority transition (`from_source` → `to_source`, `reason`,
/// `timestamp`) into the existing signed, hash-chained verifier log.
///
/// OBSERVABILITY ONLY — never blocks or alters the verdict; a failed write only
/// bumps `command_source_write_failures`.
///
/// COMPLEMENTS (does not replace) the specific `STANDBY_PROMOTED_TO_ACTIVE`
/// event: a standby promotion emits BOTH — the promotion record AND this
/// generalized provenance record (`AutonomousPlanner` → `StandbyController`) —
/// so the provenance series is uniform across all handoff kinds without a
/// redundant second promotion event.
pub fn record_handoff(
    app: &AppState,
    from_source: CommandSource,
    to_source: CommandSource,
    reason: &str,
    ts: u64,
) {
    let body = serde_json::json!({
        "from_source": from_source.as_str(),
        "to_source":   to_source.as_str(),
        "reason":      reason,
        "timestamp":   ts,
    });
    let outcome = app.store.with(|store| {
        store.save_posture_event_chained(
            COMMAND_SOURCE_NODE_ID,
            COMMAND_SOURCE_HANDOFF,
            &body.to_string(),
            Some(reason),
            ts,
        )
    });
    if let Err(e) = outcome {
        note_failure(app, &e.to_string());
    }
}

/// Operator-observable count of handoff audit writes that were DETECTED but
/// could NOT be durably recorded (#245/#247 convention). MUST be `0` in a
/// healthy deployment; a non-zero value means that many provenance events are
/// MISSING from the tamper-evident log.
pub fn write_failures(app: &AppState) -> u64 {
    app.off_path_writes
        .command_source_write_failures
        .load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::{AppState, VerifierOperationMode};
    use crate::verifier_store::VerifierStore;
    use ed25519_dalek::SigningKey;

    fn app_with_key() -> (AppState, ed25519_dalek::VerifyingKey) {
        let key = SigningKey::from_bytes(&[5u8; 32]);
        let vk = key.verifying_key();
        let mut store = VerifierStore::new(":memory:").expect("store");
        store.set_signing_key(key);
        (AppState::new(store, VerifierOperationMode::Active), vk)
    }

    /// The required test: a handoff emits a `COMMAND_SOURCE_HANDOFF` that is
    /// signed + hash-chained and carries BOTH sources + the reason.
    #[test]
    fn handoff_emits_signed_chained_event_with_both_sources() {
        let (app, vk) = app_with_key();

        record_handoff(
            &app,
            CommandSource::AutonomousPlanner,
            CommandSource::Teleoperator,
            "operator takeover requested",
            1_000,
        );
        assert_eq!(
            write_failures(&app),
            0,
            "the handoff must have been durably recorded"
        );

        let (v, events) = app.store.with(|store| {
            let v = store.verify_audit_chain_full(Some(&vk)).expect("verify");
            let events = store.load_all_posture_events().expect("load");
            (v, events)
        });
        assert!(v.chain_intact, "handoff event must be hash-chained");
        assert!(
            v.signature_valid,
            "handoff event must verify under the signing key"
        );
        assert!(
            v.signed_entries >= 1,
            "the handoff event must be signed, got {}",
            v.signed_entries
        );

        let h = &events
            .iter()
            .find(|e| e["event_type"] == COMMAND_SOURCE_HANDOFF)
            .expect("a COMMAND_SOURCE_HANDOFF event must exist")["posture"];
        assert_eq!(h["from_source"], "AutonomousPlanner");
        assert_eq!(h["to_source"], "Teleoperator");
        assert_eq!(h["reason"], "operator takeover requested");
        assert_eq!(h["timestamp"], 1_000);
    }

    /// An unattributed handoff is recorded AS unattributed, not dropped (mirrors
    /// the CANOPEN_NMT_OFFLINE_UNATTRIBUTED honesty).
    #[test]
    fn unattributed_handoff_is_recorded_not_dropped() {
        let (app, _vk) = app_with_key();
        record_handoff(
            &app,
            CommandSource::Unattributed,
            CommandSource::MrcSafeStop,
            "source unattributable -> MRC safe-stop",
            2_000,
        );
        let events = app
            .store
            .with(|store| store.load_all_posture_events().expect("load"));
        let h = &events
            .iter()
            .find(|e| e["event_type"] == COMMAND_SOURCE_HANDOFF)
            .expect("unattributed handoff must still be recorded")["posture"];
        assert_eq!(h["from_source"], "Unattributed");
        assert_eq!(h["to_source"], "MrcSafeStop");
    }

    /// Each variant renders a stable, distinct audit string (planner↔teleop,
    /// primary→standby, planner→MRC all representable).
    #[test]
    fn variants_render_stable_distinct_strings() {
        let all = [
            CommandSource::AutonomousPlanner,
            CommandSource::Teleoperator,
            CommandSource::StandbyController,
            CommandSource::MrcSafeStop,
            CommandSource::Unattributed,
        ];
        let rendered: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        // distinct
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                assert_ne!(rendered[i], rendered[j], "variant strings must be distinct");
            }
        }
        assert_eq!(
            CommandSource::StandbyController.as_str(),
            "StandbyController"
        );
        assert_eq!(CommandSource::MrcSafeStop.as_str(), "MrcSafeStop");
    }
}
