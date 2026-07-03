// src/post_incident.rs
//
// #104 — post-incident sequence audit instrumentation.
//
// OBSERVABILITY ONLY. This module emits a hash-chained, signed forensic
// sequence into the EXISTING verifier audit log (via
// `VerifierStore::save_posture_event_chained` → `audit_chain::append_audit_event_tx`,
// the same signed ledger the posture-transition and #247 divergence events use —
// no new sink). It MUST NOT perturb or block the safety verdict / control path:
//
//   * It runs only AFTER the posture transition is already committed to the
//     chain (called from `posture_engine::recalculate_and_broadcast` past the
//     `audit_committed` fail-closed gate), so it never changes whether a posture
//     change is enforced.
//   * A failed audit write is surfaced via the operator-observable
//     `post_incident_write_failures` counter (the #245/#247 pattern) and logged
//     loudly — never blocks, never propagates.
//
// Lifecycle (one correlation id per incident):
//   OPEN  — a transition INTO the fail-closed safe posture (`FleetPosture::LockedOut`)
//           opens a sequence and assigns a correlation id.
//   EVENT — each subsequent posture transition while the incident is open
//           (re-fault, partial recovery to Degraded, re-entry to LockedOut).
//   CLOSE — resolution: return to `Nominal` (recovered to Active). A LockedOut
//           that never resolves leaves the sequence open by design — the absence
//           of a CLOSE event is itself the forensic signal that the incident did
//           not resolve. (A distinct human-reset / terminal-lockout acknowledgment
//           close is a tracked follow-up.)
//
// Every event carries the correlation id, so the chain reconstructs
// "incident X → a, b, c → resolved".

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::verifier::{AppState, FleetPosture};

/// Audit event types (SCREAMING_SNAKE, matching the existing vocabulary; no
/// collision with the posture-engine / adapter / federation event strings).
pub const POST_INCIDENT_SEQUENCE_OPENED: &str = "POST_INCIDENT_SEQUENCE_OPENED";
pub const POST_INCIDENT_EVENT: &str = "POST_INCIDENT_EVENT";
pub const POST_INCIDENT_SEQUENCE_CLOSED: &str = "POST_INCIDENT_SEQUENCE_CLOSED";

/// `node_id` column value for post-incident rows (mirrors `"posture_engine"`,
/// `"governor_comparator"`, etc.).
const POST_INCIDENT_NODE_ID: &str = "post_incident";

/// Volatile open-incident state held in [`AppState`]. The durable forensic
/// continuity lives in the chain (via the correlation id), not here; this is
/// only the in-memory pointer to the currently-open sequence.
#[derive(Debug, Clone)]
pub struct IncidentState {
    /// The correlation id stamped on every event of this sequence.
    pub correlation_id: String,
    /// When the incident opened (ms).
    pub opened_at_ms: u64,
    /// The posture generation at open.
    pub opened_generation: u64,
    /// Ordinal of the NEXT event in the sequence (OPENED is 0).
    pub seq: u64,
}

/// Stable rendering of `FleetPosture` for the audit payload (matches the
/// `Debug`/`fleet_posture_str` rendering used elsewhere).
fn posture_str(p: &FleetPosture) -> &'static str {
    match p {
        FleetPosture::Nominal => "Nominal",
        FleetPosture::Degraded => "Degraded",
        FleetPosture::LockedOut => "LockedOut",
    }
}

/// A 128-bit hex correlation id from the OS CSPRNG (the same entropy source as
/// the attestation nonce, #147). Fail-closed: on the (extremely unlikely)
/// entropy failure, fall back to a generation/timestamp-tagged id so an incident
/// is NEVER left without a correlation id.
fn new_correlation_id(ts: u64, generation: u64) -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_ok() {
        let mut s = String::with_capacity(4 + 32);
        s.push_str("inc-");
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    } else {
        format!("inc-fallback-{generation}-{ts}")
    }
}

fn note_failure(app: &AppState, event_type: &str, why: &str) {
    app.post_incident_write_failures.fetch_add(1, Ordering::SeqCst);
    tracing::error!(
        event_type,
        error = %why,
        "POST-INCIDENT audit write failed — forensic event missing from the chain \
         (observability only; the safety verdict path is unaffected)"
    );
}

/// Append one post-incident event to the signed, hash-chained audit log.
/// Best-effort: a failure bumps the counter and logs; it never propagates.
fn emit(app: &AppState, event_type: &str, body: &serde_json::Value, ts: u64) {
    // WS-0.3 / #772 F2+F4: every post-incident sequence event is incident-class
    // by definition, so it is written DIRECTLY on the FULL (synchronous=FULL)
    // connection — the commit itself fsyncs the WAL, making the forensic row
    // hard-power-loss durable at write time, ATOMICALLY. This replaces the prior
    // "NORMAL commit then separate `fsync_wal_durable` marker" two-step, which
    // (a) left a crash window between the two commits and (b) routed a fsync
    // failure into `note_failure` — conflating "row MISSING from the chain" with
    // "row committed, durability degraded". Now a single `Err` means exactly the
    // former (the row did not commit), so the counter's documented meaning holds.
    // Same best-effort contract: a failure is counted, never propagated.
    let outcome = app.store.with(|store| {
        store.save_posture_event_chained_durable(
            POST_INCIDENT_NODE_ID,
            event_type,
            &body.to_string(),
            Some("post-incident forensic sequence (#104)"),
            ts,
        )
    });
    if let Err(e) = outcome {
        note_failure(app, event_type, &e.to_string());
    }
}

/// Record a COMMITTED posture transition into the post-incident forensic
/// sequence. Call from `recalculate_and_broadcast` AFTER the posture audit
/// commit succeeds.
///
/// OBSERVABILITY ONLY — the return is `()`; failures bump
/// `post_incident_write_failures`, never block the caller.
pub fn record_posture_transition(
    app: &Arc<AppState>,
    previous: Option<&FleetPosture>,
    new_posture: &FleetPosture,
    is_transition: bool,
    generation: u64,
    ts: u64,
) {
    // Only posture CHANGES form the forensic timeline; a plain cache refresh
    // (no posture change) is not a sequence event.
    if !is_transition {
        return;
    }

    // Lock order is always current_incident → store (only here); the store lock
    // is already released by the caller before this runs, so no deadlock.
    let mut guard = match app.current_incident.lock() {
        Ok(g) => g,
        Err(_) => {
            note_failure(app, "current_incident", "incident mutex poisoned");
            return;
        }
    };

    let prev_str = previous.map(posture_str);
    let new_str = posture_str(new_posture);

    match guard.as_mut() {
        // No open incident → a transition INTO the fail-closed safe posture
        // (LockedOut) OPENS one. Any other transition (e.g. Nominal→Degraded) is
        // not a post-incident onset and is left for the existing posture events.
        None => {
            if *new_posture == FleetPosture::LockedOut {
                let cid = new_correlation_id(ts, generation);
                let body = serde_json::json!({
                    "correlation_id":   cid,
                    "phase":            "OPENED",
                    "seq":              0,
                    "incident_posture": new_str,
                    "previous_posture": prev_str,
                    "generation":       generation,
                    "opened_at_ms":     ts,
                });
                emit(app, POST_INCIDENT_SEQUENCE_OPENED, &body, ts);
                *guard = Some(IncidentState {
                    correlation_id: cid,
                    opened_at_ms: ts,
                    opened_generation: generation,
                    seq: 1,
                });
            }
        }
        // Open incident → record this transition; CLOSE on resolution to Nominal.
        Some(state) => {
            let seq = state.seq;
            state.seq = state.seq.saturating_add(1);

            if *new_posture == FleetPosture::Nominal {
                let body = serde_json::json!({
                    "correlation_id":   state.correlation_id,
                    "phase":            "CLOSED",
                    "resolution":       "RESOLVED_TO_NOMINAL",
                    "seq":              seq,
                    "new_posture":      new_str,
                    "previous_posture": prev_str,
                    "generation":       generation,
                    "opened_at_ms":     state.opened_at_ms,
                    "opened_generation": state.opened_generation,
                    "duration_ms":      ts.saturating_sub(state.opened_at_ms),
                    "closed_at_ms":     ts,
                });
                emit(app, POST_INCIDENT_SEQUENCE_CLOSED, &body, ts);
                *guard = None;
            } else {
                let body = serde_json::json!({
                    "correlation_id":   state.correlation_id,
                    "phase":            "EVENT",
                    "seq":              seq,
                    "new_posture":      new_str,
                    "previous_posture": prev_str,
                    "generation":       generation,
                    "opened_at_ms":     state.opened_at_ms,
                    "emitted_at_ms":    ts,
                });
                emit(app, POST_INCIDENT_EVENT, &body, ts);
            }
        }
    }
}

/// Operator-observable count of post-incident audit writes that were DETECTED
/// but could NOT be durably recorded (#245/#247 convention). MUST be `0` in a
/// healthy deployment; a non-zero value means that many forensic events are
/// MISSING from the tamper-evident log.
pub fn write_failures(app: &AppState) -> u64 {
    app.post_incident_write_failures.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use crate::verifier_store::VerifierStore;
    use ed25519_dalek::SigningKey;

    fn app_with_key() -> (Arc<AppState>, ed25519_dalek::VerifyingKey) {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let vk = key.verifying_key();
        let mut store = VerifierStore::new(":memory:").expect("store");
        store.set_signing_key(key);
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        (app, vk)
    }

    fn body_of<'a>(events: &'a [serde_json::Value], event_type: &str) -> &'a serde_json::Value {
        &events
            .iter()
            .find(|e| e["event_type"] == event_type)
            .unwrap_or_else(|| panic!("{event_type} must be present"))["posture"]
    }

    /// The core forensic property: OPEN → EVENT → CLOSE all link to ONE
    /// correlation id, and the events are signed + hash-chained.
    #[test]
    fn incident_sequence_links_to_id_and_is_signed_and_chained() {
        let (app, vk) = app_with_key();

        // OPEN: Nominal → LockedOut (entry to the fail-closed safe posture).
        record_posture_transition(
            &app, Some(&FleetPosture::Nominal), &FleetPosture::LockedOut, true, 10, 1_000);
        // EVENT: LockedOut → Degraded (recovery progress, same incident).
        record_posture_transition(
            &app, Some(&FleetPosture::LockedOut), &FleetPosture::Degraded, true, 11, 2_000);
        // CLOSE: Degraded → Nominal (resolved to Active).
        record_posture_transition(
            &app, Some(&FleetPosture::Degraded), &FleetPosture::Nominal, true, 12, 3_000);

        assert_eq!(write_failures(&app), 0, "all three writes must land");
        // The incident is closed → no open state remains.
        assert!(app.current_incident.lock().unwrap().is_none());

        let (v, events) = app.store.with(|store| {
            // Signed + hash-linked under the signing key.
            let v = store.verify_audit_chain_full(Some(&vk)).expect("verify");
            let events = store.load_all_posture_events().expect("load");
            (v, events)
        });
        assert!(v.chain_intact, "post-incident events must be hash-chained");
        assert!(v.signature_valid, "post-incident events must verify under the signing key");
        assert!(v.signed_entries >= 3, "all three sequence events must be signed, got {}", v.signed_entries);

        // All three events carry the SAME correlation id.
        let opened = body_of(&events, POST_INCIDENT_SEQUENCE_OPENED);
        let event = body_of(&events, POST_INCIDENT_EVENT);
        let closed = body_of(&events, POST_INCIDENT_SEQUENCE_CLOSED);

        let cid = opened["correlation_id"].as_str().expect("correlation id");
        assert!(cid.starts_with("inc-"), "correlation id format, got {cid}");
        assert_eq!(event["correlation_id"].as_str(), Some(cid), "EVENT links to the incident id");
        assert_eq!(closed["correlation_id"].as_str(), Some(cid), "CLOSED links to the incident id");

        assert_eq!(opened["phase"], "OPENED");
        assert_eq!(opened["incident_posture"], "LockedOut");
        assert_eq!(closed["resolution"], "RESOLVED_TO_NOMINAL");
        assert_eq!(closed["duration_ms"], 2_000); // 3_000 − 1_000
    }

    /// A transition that does NOT enter the fail-closed safe posture must not
    /// open an incident (no false sequences).
    #[test]
    fn non_lockout_transition_opens_no_incident() {
        let (app, _vk) = app_with_key();
        record_posture_transition(
            &app, Some(&FleetPosture::Nominal), &FleetPosture::Degraded, true, 1, 100);
        assert!(app.current_incident.lock().unwrap().is_none());
        let events = app.store.with(|store| store.load_all_posture_events().expect("load"));
        assert!(
            !events.iter().any(|e| e["event_type"] == POST_INCIDENT_SEQUENCE_OPENED),
            "a non-lockout transition must not open a post-incident sequence"
        );
    }

    /// A non-transition (cache refresh) emits nothing.
    #[test]
    fn non_transition_emits_nothing() {
        let (app, _vk) = app_with_key();
        record_posture_transition(
            &app, Some(&FleetPosture::LockedOut), &FleetPosture::LockedOut, false, 1, 100);
        let is_empty = app
            .store
            .with(|store| store.load_all_posture_events().expect("load").is_empty());
        assert!(is_empty, "is_transition=false must emit nothing");
    }

    /// Each distinct incident gets a fresh correlation id.
    #[test]
    fn each_incident_gets_a_distinct_correlation_id() {
        let (app, _vk) = app_with_key();
        // First incident: open then resolve.
        record_posture_transition(
            &app, Some(&FleetPosture::Nominal), &FleetPosture::LockedOut, true, 1, 100);
        record_posture_transition(
            &app, Some(&FleetPosture::LockedOut), &FleetPosture::Nominal, true, 2, 200);
        // Second incident.
        record_posture_transition(
            &app, Some(&FleetPosture::Nominal), &FleetPosture::LockedOut, true, 3, 300);

        let events = app.store.with(|store| store.load_all_posture_events().expect("load"));
        let opens: Vec<String> = events
            .iter()
            .filter(|e| e["event_type"] == POST_INCIDENT_SEQUENCE_OPENED)
            .map(|e| e["posture"]["correlation_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(opens.len(), 2, "two incidents opened");
        assert_ne!(opens[0], opens[1], "each incident must get a distinct correlation id");
    }
}
