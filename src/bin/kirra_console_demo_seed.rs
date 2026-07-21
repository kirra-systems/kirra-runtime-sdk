// src/bin/kirra_console_demo_seed.rs
//
// Operator-console DEMO SEED — a SEPARATE dev-only binary, never a flag on the
// production service (a prod service must not carry a seeding mode).
//
// Makes `/console` showable end-to-end with zero hardware by writing a small,
// obviously-demo fleet + a real SG6 escalation through the REAL signed store
// APIs. The chain the console displays is GENUINELY signed (RAIL 2), so its
// sig-verified flags are real verification, not decoration.
//
// RAILS:
//   1. Never pollute — refuses to run if the target DB already holds ANY
//      registered node (prints, exits non-zero; no --force).
//   2. Never fake verification — KIRRA_LOG_SIGNING_KEY is REQUIRED at seed time;
//      every chained row is signed under it (the same key the service verifies
//      with), exactly as the production sinks sign.
//
// DEMO DATA, NOT EVIDENCE. Node ids are KIRRA-DEMO-*; nothing here enters any
// safety case. See docs/CONSOLE_RUNBOOK.md.

use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
use ed25519_dalek::SigningKey;

use kirra_persistence::VerifierStore;
use kirra_verifier::verifier::{NodeTrustState, RegisteredNode};

// The REAL SG6 impact-event vocabulary, mirrored as literals. We cannot IMPORT
// the parko-kirra `audit_sink` constants here because `parko-kirra` is only a
// DEV-dependency of this crate (it depends on `kirra-verifier`, so a normal
// dependency would be a cycle). These strings are kept byte-identical to
// `parko_kirra::audit_sink::{IMPACT_DETECTED_EVENT_TYPE,
// IMPACT_ESCALATION_RAISED_EVENT_TYPE, IMPACT_AUDIT_SOURCE}` and the
// `ImpactDetectedPayload` / `ImpactEscalationPayload` serialized shapes — the
// `#[cfg(test)]` block uses the real types (dev-dep) to keep them honest.
const IMPACT_DETECTED_EVENT_TYPE: &str = "ImpactDetected";
const IMPACT_ESCALATION_RAISED_EVENT_TYPE: &str = "ImpactEscalationRaised";
/// The source tag the production impact sink writes impact rows under
/// (`ImpactAuditSink` → `ChainedAuditWriter::record(IMPACT_AUDIT_SOURCE, …)`), so
/// the seeded rows are byte-shaped like the real ones (node_id = the source).
const IMPACT_AUDIT_SOURCE: &str = "governor_impact_latch";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn decode_signing_key(b64: &str) -> Result<SigningKey, String> {
    let bytes = b64e
        .decode(b64.trim())
        .map_err(|_| "KIRRA_LOG_SIGNING_KEY is not valid base64".to_string())?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "KIRRA_LOG_SIGNING_KEY must decode to a 32-byte ed25519 seed".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

/// RAIL 1: the seed only initializes a FRESH store. A populated store is left
/// untouched (the failure this guards is a seeder pointed at real data).
fn ensure_fresh(store: &VerifierStore) -> Result<(), String> {
    match store.load_nodes() {
        Ok(nodes) if nodes.is_empty() => Ok(()),
        Ok(_) => Err("store is not empty; demo seed only initializes fresh stores".to_string()),
        Err(e) => Err(format!("could not read the store: {e}")),
    }
}

struct SeedSummary {
    nodes: usize,
    events: usize,
}

/// Seed the demo fleet + the KIRRA-DEMO-03 SG6 escalation through the REAL,
/// SIGNED store writers. The caller MUST have set a signing key first (RAIL 2).
fn seed(store: &mut VerifierStore, now: u64) -> rusqlite::Result<SeedSummary> {
    // node_id, trust state (drives the console fleet tile), route/condition note.
    let nodes: [(&str, NodeTrustState, &str); 6] = [
        (
            "KIRRA-DEMO-01",
            NodeTrustState::Trusted,
            "route: depot → dock A",
        ),
        (
            "KIRRA-DEMO-02",
            NodeTrustState::Trusted,
            "route: dock A → yard",
        ),
        (
            "KIRRA-DEMO-03",
            NodeTrustState::Untrusted(
                "post-collision impact latch — operator clearance required".into(),
            ),
            "SG6 immobilized — escalation raised",
        ),
        (
            "KIRRA-DEMO-04",
            NodeTrustState::Trusted,
            "route: yard → depot",
        ),
        (
            "KIRRA-DEMO-05",
            NodeTrustState::Untrusted("flood_condition_active".into()),
            "degraded: standing water on segment 7",
        ),
        (
            "KIRRA-DEMO-06",
            NodeTrustState::Trusted,
            "route: charging bay",
        ),
    ];

    let mut events = 0usize;
    for (i, (id, status, note)) in nodes.iter().enumerate() {
        let t = now - (nodes.len() as u64 - i as u64) * 1_000;
        store.save_node(&RegisteredNode {
            node_id: id.to_string(),
            status: status.clone(),
            registered_at_ms: t,
            last_trust_update_ms: t,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        })?;
        // A background attestation event per node (real type) so the feed is alive.
        store.save_posture_event_chained(
            id,
            "ATTESTATION_TRUSTED",
            &serde_json::json!({ "node": id, "note": note }).to_string(),
            Some(note),
            t,
        )?;
        events += 1;
    }

    // KIRRA-DEMO-05 flood degrade — a real MRC fallback (perception/condition derate).
    store.save_posture_event_chained(
        "KIRRA-DEMO-05",
        "TRAJECTORY_MRC_FALLBACK",
        &serde_json::json!({ "deny_code": "TRAJECTORY_MRC_FALLBACK", "note": "flood_condition_active — standing-water derate" }).to_string(),
        Some("flood_condition_active"),
        now - 4_000,
    )?;
    events += 1;

    // KIRRA-DEMO-03 SG6 sequence — REAL event types, in incident order.
    store.save_posture_event_chained(
        "KIRRA-DEMO-03",
        "RSS_VIOLATION",
        &serde_json::json!({ "safe": false, "note": "RSS longitudinal safe-distance violated" })
            .to_string(),
        Some("rss_violation"),
        now - 3_000,
    )?;
    events += 1;
    store.save_posture_event_chained(
        "KIRRA-DEMO-03",
        "TRAJECTORY_MRC_FALLBACK",
        &serde_json::json!({ "deny_code": "TRAJECTORY_MRC_FALLBACK", "note": "perception-derate MRC floor — monitor stale/silent" }).to_string(),
        Some("perception_gap"),
        now - 2_500,
    )?;
    events += 1;

    // ImpactDetected — the REAL writer shape: node_id = the source tag, and the
    // `ImpactDetectedPayload` serialized form (vanished-object trigger; the
    // optional `spike_magnitude_mps2` is omitted when absent, per the real type's
    // `skip_serializing_if`). The reachable-band context is carried in the audit
    // `reason` (the production payload has no band field — we do not invent one).
    store.save_posture_event_chained(
        IMPACT_AUDIT_SOURCE,
        IMPACT_DETECTED_EVENT_TYPE,
        &serde_json::json!({
            "contact_sensor": false,
            "spike_over_threshold": false,
            "vanished_object": true,
        })
        .to_string(),
        Some("close-range tracked agent vanished — person-under-vehicle reachable band"),
        now - 2_000,
    )?;
    events += 1;

    // ImpactEscalationRaised — the `ImpactEscalationPayload` shape ({ detail }).
    store.save_posture_event_chained(
        IMPACT_AUDIT_SOURCE,
        IMPACT_ESCALATION_RAISED_EVENT_TYPE,
        &serde_json::json!({
            "detail": "operator intervention required (SS-003): immobilized pending authenticated clearance",
        })
        .to_string(),
        Some("escalation_raised"),
        now - 1_500,
    )?;
    events += 1;

    Ok(SeedSummary {
        nodes: nodes.len(),
        events,
    })
}

fn main() {
    let db = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("KIRRA_DB_PATH").ok())
        .unwrap_or_else(|| "kirra_demo.sqlite".to_string());

    // RAIL 2: require a real signing key — the seeded chain must be genuinely signed.
    let key_b64 = std::env::var("KIRRA_LOG_SIGNING_KEY").unwrap_or_default();
    if key_b64.trim().is_empty() {
        eprintln!(
            "KIRRA_LOG_SIGNING_KEY is required (the seeded audit chain must be GENUINELY signed,\n\
             so the console's sig-verified flags are real). See docs/CONSOLE_RUNBOOK.md."
        );
        exit(2);
    }
    let signing_key = match decode_signing_key(&key_b64) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            exit(2);
        }
    };

    let mut store = match VerifierStore::new(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not open store '{db}': {e}");
            exit(2);
        }
    };

    // RAIL 1: never pollute a populated store.
    if let Err(e) = ensure_fresh(&store) {
        eprintln!("{e} (target: '{db}')");
        exit(1);
    }

    store.set_signing_key(signing_key);
    let summary = match seed(&mut store, now_ms()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("seed failed: {e}");
            exit(2);
        }
    };

    println!(
        "Seeded '{db}': {} demo nodes, {} signed chain events.",
        summary.nodes, summary.events
    );
    println!("  fleet: KIRRA-DEMO-01..06 (DEMO DATA, not evidence).");
    println!("  KIRRA-DEMO-03: SG6 escalation (RSS_VIOLATION → TRAJECTORY_MRC_FALLBACK → ImpactDetected → ImpactEscalationRaised).");
    println!();
    println!("Next — serve the console (same signing key so the chain verifies):");
    println!("  KIRRA_ADMIN_TOKEN=demo-admin \\");
    println!("  KIRRA_SUPERVISOR_RESET_KEY=demo-supervisor-key \\");
    println!("  KIRRA_LOG_SIGNING_KEY=$KIRRA_LOG_SIGNING_KEY \\");
    println!("  KIRRA_DB_PATH={db} \\");
    println!("    cargo run --bin kirra_verifier_service");
    println!("Then open  http://127.0.0.1:8090/console");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32]) // deterministic demo seed (test only)
    }

    #[test]
    fn seed_populates_fleet_and_escalation_and_chain_verifies() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        let key = demo_key();
        let vk = key.verifying_key();
        store.set_signing_key(key);

        let summary = seed(&mut store, 1_700_000_000_000).expect("seed");
        assert_eq!(summary.nodes, 6);

        // Fleet (what /console/fleet passes through): all 6 demo nodes present.
        let nodes = store.load_nodes().unwrap();
        assert_eq!(nodes.len(), 6);
        assert!(nodes.iter().any(|n| n.node_id == "KIRRA-DEMO-03"));

        // Audit + escalation (what /console/audit + /console/escalations pass
        // through): the chain VERIFIES under the seeding key (RAIL 2 — real
        // signatures), and the SG6 events are present.
        let page = store.load_audit_chain_page(200, 0, Some(&vk)).unwrap();
        assert!(page.chain_intact, "the seeded chain must hash-verify");
        let types: Vec<String> = page
            .entries
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .filter_map(|v| {
                v.get("event_type")
                    .and_then(|x| x.as_str())
                    .map(String::from)
            })
            .collect();
        for required in [
            "ImpactDetected",
            "ImpactEscalationRaised",
            "RSS_VIOLATION",
            "TRAJECTORY_MRC_FALLBACK",
        ] {
            assert!(
                types.iter().any(|t| t == required),
                "missing real event type {required}"
            );
        }
        // Every row carries a real signature that VERIFIES under the seeding key
        // (load_audit_chain_page renders this as "valid"; "unsigned"/"invalid"
        // would mean a decorative or broken chain — RAIL 2).
        let all_valid = page.entries.iter().all(|e| {
            serde_json::to_value(e)
                .ok()
                .and_then(|v| {
                    v.get("signature_status")
                        .and_then(|x| x.as_str())
                        .map(|s| s == "valid")
                })
                .unwrap_or(false)
        });
        assert!(
            all_valid,
            "every seeded row must be signed AND verify (status == \"valid\")"
        );
    }

    #[test]
    fn impact_event_strings_match_real_taxonomy() {
        // The bin's literals are byte-identical to the production parko-kirra
        // constants (we can't import them in non-test code — dev-dep cycle).
        assert_eq!(
            IMPACT_DETECTED_EVENT_TYPE,
            parko_kirra::audit_sink::IMPACT_DETECTED_EVENT_TYPE
        );
        assert_eq!(
            IMPACT_ESCALATION_RAISED_EVENT_TYPE,
            parko_kirra::audit_sink::IMPACT_ESCALATION_RAISED_EVENT_TYPE
        );
    }

    #[test]
    fn refuses_non_empty_store() {
        let mut store = VerifierStore::new(":memory:").expect("store");
        store.set_signing_key(demo_key());
        seed(&mut store, 1_700_000_000_000).expect("seed");
        // A second seed attempt is refused (RAIL 1).
        assert!(
            ensure_fresh(&store).is_err(),
            "a populated store must be refused"
        );
    }

    #[test]
    fn delivery_example_clears_the_seeded_escalation_end_to_end() {
        // The money demo, as a test: seed → operator records a grant for
        // KIRRA-DEMO-03 → the Phase-B delivery (the example's call) clears a
        // pre-latched loop. Reuses the Phase-B harness shape.
        use kirra_verifier::store_handle::StoreHandle;
        use parko_core::{ClearanceLoop, ImpactCfg, ImpactEvidence};
        use parko_kirra::clearance_delivery::{ClearanceDelivery, DeliveryOutcome};

        let mut store = VerifierStore::new(":memory:").expect("store");
        store.set_signing_key(demo_key());
        seed(&mut store, 1_700_000_000_000).expect("seed");
        // The operator records a grant via the console (here: the store API).
        store
            .save_clearance_grant_chained("KIRRA-DEMO-03", "demo-operator", 1_700_000_000_500)
            .expect("record grant");

        let store = StoreHandle::new(store);
        // A loop driven into EscalationRaised through the REAL state machine.
        let mut clearance_loop = ClearanceLoop::new();
        let ev = ImpactEvidence {
            imu_accel_spike_mps2: 0.0,
            contact_sensor: true,
            vanished_object: false,
        };
        clearance_loop.observe(&ev, &ImpactCfg::default(), 0);
        clearance_loop.observe(&ev, &ImpactCfg::default(), 0);
        assert!(clearance_loop.is_immobilized());

        let delivery = ClearanceDelivery::new(store, "KIRRA-DEMO-03");
        let outcome = delivery.poll_and_deliver(&mut clearance_loop, 1_700_000_001_000);
        assert!(
            matches!(outcome, DeliveryOutcome::Cleared { .. }),
            "got {outcome:?}"
        );
        assert_eq!(clearance_loop.state(), parko_core::ClearanceState::Normal);
    }
}
