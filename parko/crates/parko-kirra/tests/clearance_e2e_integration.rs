//! End-to-end composition test (#330) — the seam no single unit test covers.
//!
//! Drives the FULL post-collision clearance chain across three crates, off-target
//! (no ROS, no Zenoh — runs on the parko-safety CI lane):
//!
//! ```text
//!   SG6 impact latch (parko-core ClearanceLoop)
//!     → operator proves identity + the grant is recorded (kirra-runtime-sdk,
//!       the real #314 verify-THEN-consume path + the Phase-A store path)
//!     → Phase-B delivery (parko-kirra ClearanceDelivery — the EXACT call the
//!       parko-ros2 node tick makes)
//!     → the loop clears → motion resumes
//!     → the whole thing is provable from the signed audit chain.
//! ```
//!
//! This is exactly where the composed defects in the #319 review register live:
//! #321 (gravity-proxy × per-class threshold) and #322 (console vs fleet replay
//! divergence) are cross-component bugs no single unit test would catch. The second
//! test pins the #321 per-class contract boundary mechanically.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

use kirra_verifier::attestation::{
    operator_grant_signing_payload, operator_key_fingerprint, verify_ed25519_pem_signature,
};
use kirra_verifier::store_handle::StoreHandle;
use kirra_verifier::verifier::{AppState, VerifierOperationMode};
use kirra_verifier::verifier_store::{AuditExportPage, VerifierStore};

use parko_core::{
    impact_cfg_for_class, is_impact, ClearanceLoop, ClearanceState, ImpactCfg, ImpactEvidence,
    VehicleClass, DEFAULT_MAX_GRANT_AGE_MS,
};
use parko_kirra::clearance_delivery::{ClearanceDelivery, DeliveryOutcome};

/// ISO 80000-3 standard gravity — the datum the #321 deviation convention subtracts.
const G_MPS2: f64 = 9.806_65;

// --------------------------------------------------------------------------
// Helpers — mirror the verifier service's operator-keypair + signed-store setup.
// --------------------------------------------------------------------------

/// A deterministic operator keypair + its SPKI PEM (the same 12-byte Ed25519 DER
/// prefix the verifier service constructs, so `verify_ed25519_pem_signature` and
/// `operator_key_fingerprint` accept it).
fn operator_keypair(seed: u8) -> (SigningKey, String) {
    const ED25519_SPKI_PREFIX: [u8; 12] =
        [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(sk.verifying_key().as_bytes());
    let pem = format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        B64.encode(&der)
    );
    (sk, pem)
}

/// A real `AppState` over a SIGNED in-memory store (audit chain signed under
/// `key_seed`). `app.store` is the single co-located store the verifier and the
/// node share — production's "one vehicle, one store" (clearance_delivery.rs).
fn signed_appstate(key_seed: u8) -> (AppState, VerifyingKey) {
    let signing = SigningKey::from_bytes(&[key_seed; 32]);
    let vk = signing.verifying_key();
    let mut store = VerifierStore::new(":memory:").expect("in-memory store");
    store.set_signing_key(signing);
    (AppState::new(store, VerifierOperationMode::Active), vk)
}

/// Drive the REAL SG6 state machine into `EscalationRaised` (immobilized) via a
/// person-under-vehicle (`vanished_object`) trigger — the seeded KIRRA-DEMO-03
/// incident, through `observe()`, never by poking internals.
fn escalated_loop() -> ClearanceLoop {
    let mut l = ClearanceLoop::new();
    let ev = ImpactEvidence {
        imu_accel_spike_mps2: 0.0,
        contact_sensor: false,
        vanished_object: true,
    };
    let cfg = ImpactCfg::default();
    l.observe(&ev, &cfg, 0); // Normal -> Latched
    l.observe(&ev, &cfg, 0); // Latched -> EscalationRaised
    assert!(l.is_immobilized() && l.escalation_pending(), "SG6 must be immobilized");
    l
}

fn audit_chain(store: &StoreHandle, vk: &VerifyingKey) -> AuditExportPage {
    store.with(|s| s.load_audit_chain_page(500, 0, Some(vk))).unwrap()
}

// --------------------------------------------------------------------------
// The headline composition.
// --------------------------------------------------------------------------

#[test]
fn sg6_latch_to_operator_signed_grant_to_phase_b_clears_motion() {
    let node = "KIRRA-DEMO-03";
    let operator = "op-alice";
    let (app, vk) = signed_appstate(7);
    let store = app.store.clone();

    // 1. SG6 — the node is immobilized after a post-collision latch.
    let mut sg6 = escalated_loop();

    // 2. The operator PROVES identity (#314 verify-THEN-consume), then the grant is
    //    recorded through the real Phase-A store path.
    let (op_sk, op_pem) = operator_keypair(42);
    store.with(|s| s.register_operator(operator, &op_pem, 1)).unwrap();
    assert!(
        store.with(|s| s.load_operator(operator)).unwrap().is_some(),
        "operator is registered"
    );

    let now = 1_000_000u64;
    let nonce = "a1b2c3d4e5f6";
    let key = format!("{operator}|{node}");
    app.issue_clearance_challenge(&key, nonce.to_string(), now);

    // The operator signs the domain-separated (operator, node, nonce) payload with
    // their PRIVATE key; the verifier reconstructs the same bytes and checks the
    // signature against the registered PUBLIC key — trust is the signature.
    let payload = operator_grant_signing_payload(operator, node, nonce);
    let sig = op_sk.sign(&payload).to_bytes();
    assert!(
        verify_ed25519_pem_signature(&op_pem, &payload, &sig),
        "operator signature must verify against the registered key"
    );

    // Verify-THEN-consume: the nonce is single-use. A replay of the SAME nonce is
    // rejected — the #314/#322 replay defense, here at the console lane.
    assert!(app.consume_clearance_challenge(&key, nonce, now), "first consume succeeds");
    assert!(
        !app.consume_clearance_challenge(&key, nonce, now),
        "the nonce is single-use — a replay is rejected"
    );

    // Record the operator-signed grant: a PENDING-NODE-TRANSPORT row + a SIGNED
    // audit event naming WHICH operator key cleared it (non-repudiation).
    let fingerprint = operator_key_fingerprint(&op_pem);
    let rowid = store
        .with(|s| s.save_clearance_grant_chained_with_auth(node, operator, now, "operator-signed", fingerprint.as_deref()))
        .unwrap();
    assert!(rowid > 0, "Phase-A recorded the grant");

    // 3. Phase-B delivery (the EXACT call the node tick makes) clears the loop →
    //    motion resumes. Same co-located store; within the grant-age window.
    let delivery = ClearanceDelivery::new(store.clone(), node);
    match delivery.poll_and_deliver(&mut sg6, now + 500) {
        DeliveryOutcome::Cleared { operator_id, grant_rowid } => {
            assert_eq!(operator_id, operator, "the clearing operator is carried through");
            assert_eq!(grant_rowid, rowid, "the delivered grant is the recorded row");
        }
        other => panic!("expected Cleared, got {other:?}"),
    }
    assert_eq!(sg6.state(), ClearanceState::Normal, "the loop cleared to Normal");
    assert!(!sg6.is_immobilized(), "motion resumes");

    // 4. One-shot — the grant is consumed; a second delivery finds nothing (no
    //    replay machine).
    assert_eq!(
        delivery.poll_and_deliver(&mut sg6, now + 600),
        DeliveryOutcome::NoGrant,
        "the grant is consumed exactly once"
    );

    // 5. SIGNED-CHAIN EVIDENCE end-to-end: the hash chain verifies under the store
    //    key and carries both the operator-signed grant AND the delivery, each with a
    //    VALID signature.
    let page = audit_chain(&store, &vk);
    assert!(page.chain_intact, "the audit hash-chain is intact under the signing key");
    let find = |et: &str| page.entries.iter().find(|e| e.event_type == et);
    let issued = find("OperatorClearanceGrantIssued").expect("grant issuance is audited");
    let delivered = find("ClearanceDelivered").expect("delivery is audited");
    assert_eq!(issued.signature_status, "valid", "grant event is signed + valid");
    assert_eq!(delivered.signature_status, "valid", "delivery event is signed + valid");
    assert!(issued.payload.contains(operator), "the grant event names the operator");
}

// --------------------------------------------------------------------------
// Two-checkpoint property across the operator-signed path.
// --------------------------------------------------------------------------

#[test]
fn operator_signed_grant_stale_at_delivery_is_rejected_two_checkpoint() {
    // The verifier accepts a well-formed grant at RECORD time, but Phase-B
    // re-validates at DELIVERY time: a grant that aged past DEFAULT_MAX_GRANT_AGE_MS
    // is REJECTED at the loop even though it was authentically operator-signed and
    // the verifier accepted it. It is consumed (no retry) — the operator re-issues.
    let node = "KIRRA-DEMO-03";
    let operator = "op-bob";
    let (app, _vk) = signed_appstate(9);
    let store = app.store.clone();
    let mut sg6 = escalated_loop();

    let (_op_sk, op_pem) = operator_keypair(11);
    store.with(|s| s.register_operator(operator, &op_pem, 1)).unwrap();
    let granted = 1_000_000u64;
    let fingerprint = operator_key_fingerprint(&op_pem);
    store
        .with(|s| s.save_clearance_grant_chained_with_auth(node, operator, granted, "operator-signed", fingerprint.as_deref()))
        .unwrap();

    let delivery = ClearanceDelivery::new(store.clone(), node);
    let stale_now = granted + DEFAULT_MAX_GRANT_AGE_MS + 1;
    let out = delivery.poll_and_deliver(&mut sg6, stale_now);
    assert!(
        matches!(out, DeliveryOutcome::Rejected { reason: "malformed_grant", .. }),
        "a verifier-accepted grant is still rejected at delivery if stale; got {out:?}"
    );
    assert!(sg6.is_immobilized(), "the loop stays immobilized after a stale grant");
    assert_eq!(
        delivery.poll_and_deliver(&mut sg6, stale_now + 1),
        DeliveryOutcome::NoGrant,
        "the stale grant is consumed, not retried"
    );
}

// --------------------------------------------------------------------------
// The #321 per-class contract boundary — pinned mechanically.
// --------------------------------------------------------------------------

#[test]
fn per_class_impact_profile_pins_321_deviation_contract() {
    // #321 / ADL-013: the IMU term is a gravity-DEVIATION threshold (`|‖a‖ − G|`),
    // not a raw norm — so a STATIC vehicle reads ≈ 0 and never false-latches — and
    // each vehicle class carries its own profile (the parko mirror of the normative
    // docs/CONTRACT_PROFILES.md table). This is the seam #330 guards mechanically.
    let courier = impact_cfg_for_class(VehicleClass::Courier);
    let delivery = impact_cfg_for_class(VehicleClass::DeliveryAv);
    let robotaxi = impact_cfg_for_class(VehicleClass::Robotaxi);

    // Per-class ordering: pedestrian-space is the most sensitive, full-speed least.
    // A swapped/duplicated profile breaks exactly this.
    assert!(
        courier.spike_threshold_mps2 < delivery.spike_threshold_mps2,
        "courier < delivery-av"
    );
    assert!(
        delivery.spike_threshold_mps2 < robotaxi.spike_threshold_mps2,
        "delivery-av < robotaxi"
    );

    // The deviation convention is load-bearing: the courier threshold sits BELOW
    // standard gravity. That is only safe because the field is a deviation (`|‖a‖−G|`)
    // — under the OLD raw-norm convention a sub-gravity threshold false-latched a
    // parked vehicle on gravity alone (the #321 bug). Still above the 1.0 noise floor.
    assert!(courier.spike_threshold_mps2 < G_MPS2, "courier threshold is sub-gravity (deviation units)");
    assert!(courier.spike_threshold_mps2 > 1.0, "courier threshold is above the noise floor");

    // A STATIC vehicle (deviation ≈ 0) never fuses an impact for ANY class.
    let at_rest = ImpactEvidence {
        imu_accel_spike_mps2: 0.0,
        contact_sensor: false,
        vanished_object: false,
    };
    assert!(!is_impact(&at_rest, &courier), "a parked courier never latches at rest");
    assert!(!is_impact(&at_rest, &delivery), "a parked delivery-av never latches at rest");
    assert!(!is_impact(&at_rest, &robotaxi), "a parked robotaxi never latches at rest");

    // SAME physical deviation, DIFFERENT verdict by class: a 3.0 m/s² deviation is a
    // sidewalk-collision-grade event for a courier but well within a robotaxi's /
    // delivery-av's envelope. A mis-wired class→profile map would break exactly this.
    let small = ImpactEvidence {
        imu_accel_spike_mps2: 3.0,
        contact_sensor: false,
        vanished_object: false,
    };
    assert!(is_impact(&small, &courier), "3.0 deviation latches a courier (> 2.5)");
    assert!(!is_impact(&small, &delivery), "3.0 deviation is within a delivery-av (< 8.0)");
    assert!(!is_impact(&small, &robotaxi), "3.0 deviation is within a robotaxi (< 22.0)");
}
