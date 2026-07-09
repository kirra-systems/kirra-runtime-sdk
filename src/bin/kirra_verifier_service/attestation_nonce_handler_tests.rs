// attestation_nonce_handler_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// #147 — attestation nonce lifecycle: VERIFY-THEN-CONSUME at the handler.
//
// The crypto (verify_attestation_proof) and the store invariants (single-use,
// TTL, node-binding, CSPRNG) are tested in attestation.rs / verifier.rs. This
// proves the remaining handler-level invariant: a FAILED proof must NOT burn
// the pending nonce, so an attacker cannot force nonce exhaustion — the
// legitimate node can still attest with the same outstanding nonce.
// ---------------------------------------------------------------------------

use super::{register_node, verify_attestation, RegisterNodeRequest, VerifyAttestationRequest};

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

use kirra_verifier::attestation::attestation_signing_payload;
use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, NodeTrustState, RegisteredNode, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

const NODE: &str = "edge-node-1";

/// Test-only Ed25519 SubjectPublicKeyInfo PEM (RFC 8410 prefix; public key only).
fn public_key_to_pem(vk: &VerifyingKey) -> String {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(vk.as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        B64.encode(&der)
    )
}

fn sign_proof(sk: &SigningKey, node_id: &str, nonce: u64) -> String {
    hex::encode(
        sk.sign(&attestation_signing_payload(node_id, nonce))
            .to_bytes(),
    )
}

fn svc_with_registered_node(ak_pem: String) -> Arc<ServiceState> {
    let app = Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    ));
    app.persist_and_insert_node(RegisteredNode {
        node_id: NODE.to_string(),
        status: NodeTrustState::Unknown,
        registered_at_ms: 1,
        last_trust_update_ms: 0,
        ak_public_pem: Some(ak_pem),
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    })
    .expect("register node");

    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(
            kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
        ),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    })
}

async fn verify(svc: Arc<ServiceState>, nonce: u64, proof_hex: String) -> StatusCode {
    let req: VerifyAttestationRequest = serde_json::from_value(serde_json::json!({
        "node_id": NODE, "nonce": nonce, "proof_hex": proof_hex,
    }))
    .expect("build request");
    verify_attestation(State(svc), Json(req))
        .await
        .into_response()
        .status()
}

/// WS-4: an attestation-SIGNED adoption report is verified against the node's
/// registered AK — a valid signature marks the stored report `attested`, a
/// tampered one is rejected fail-closed (401), and an unsigned report is accepted
/// but unattested.
#[tokio::test]
async fn signed_adoption_report_is_attested_and_forgery_rejected() {
    use super::{report_node_artifact, NodeArtifactReport};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let svc = svc_with_registered_node(public_key_to_pem(&sk.verifying_key()));
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let ts: u64 = 5_000;
    let good_sig = B64.encode(
        sk.sign(&kirra_verifier::attestation::adoption_report_signing_payload(NODE, digest, ts))
            .to_bytes(),
    );

    let report = |body: serde_json::Value| {
        let req: NodeArtifactReport = serde_json::from_value(body).expect("build report");
        report_node_artifact(State(svc.clone()), Json(req))
    };
    let attested_of = |svc: Arc<ServiceState>| async move {
        svc.app
            .store
            .call_read(|s| s.load_node_artifact_statuses())
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|r| r.node_id == NODE)
            .map(|r| r.attested)
    };

    // Valid signature → 200 and stored attested=true.
    let st = report(serde_json::json!({
        "node_id": NODE, "applied_digest": digest,
        "signature": good_sig, "reported_at_ms": ts,
    }))
    .await
    .into_response()
    .status();
    assert_eq!(st, StatusCode::OK, "valid signed report accepted");
    assert_eq!(
        attested_of(svc.clone()).await,
        Some(true),
        "stored as attested"
    );

    // Tampered signature (flip a byte) → 401, fail-closed.
    let mut bad = B64.decode(&good_sig).unwrap();
    bad[0] ^= 0x01;
    let st = report(serde_json::json!({
        "node_id": NODE, "applied_digest": digest,
        "signature": B64.encode(&bad), "reported_at_ms": ts,
    }))
    .await
    .into_response()
    .status();
    assert_eq!(st, StatusCode::UNAUTHORIZED, "forged signature rejected");

    // Unsigned report for a DIFFERENT digest → a fresh claim, so attestation
    // resets to false (attested is monotonic PER DIGEST — an unsigned report for
    // the SAME digest would instead preserve the prior attested=true; that
    // per-digest rule is covered by the store test).
    let other_digest = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let st = report(serde_json::json!({
        "node_id": NODE, "applied_digest": other_digest,
    }))
    .await
    .into_response()
    .status();
    assert_eq!(
        st,
        StatusCode::OK,
        "unsigned report still accepted (identity-gated)"
    );
    assert_eq!(
        attested_of(svc.clone()).await,
        Some(false),
        "unsigned → not attested"
    );

    // A signed report with a FAR-FUTURE timestamp is rejected (the monotonic
    // upsert would otherwise let it permanently wedge later legitimate updates).
    let future_ts = now_ms() + 3_600_000; // 1h ahead — beyond the skew allowance
    let future_sig = B64.encode(
        sk.sign(
            &kirra_verifier::attestation::adoption_report_signing_payload(NODE, digest, future_ts),
        )
        .to_bytes(),
    );
    let st = report(serde_json::json!({
        "node_id": NODE, "applied_digest": digest,
        "signature": future_sig, "reported_at_ms": future_ts,
    }))
    .await
    .into_response()
    .status();
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "far-future signed timestamp rejected"
    );
}

// ---- PCR16 measured-boot binding (attestation follow-up) --------------

/// `svc_with_registered_node`, but the node is enrolled with an expected
/// measured-boot PCR16 digest.
fn svc_with_pcr16_node(ak_pem: String, expected_pcr16: &str) -> Arc<ServiceState> {
    let svc = svc_with_registered_node(ak_pem);
    let existing = svc.app.nodes.get(NODE).map(|n| n.clone()).unwrap();
    svc.app
        .persist_and_insert_node(RegisteredNode {
            expected_pcr16_digest_hex: Some(expected_pcr16.to_string()),
            ..existing
        })
        .expect("re-register with expected PCR16");
    svc
}

fn sign_proof_with_pcr16(
    sk: &SigningKey,
    node_id: &str,
    nonce: u64,
    presented: Option<&str>,
) -> String {
    let payload = kirra_verifier::attestation::attestation_signing_payload_with_pcr16(
        node_id, nonce, presented,
    );
    hex::encode(sk.sign(&payload).to_bytes())
}

async fn verify_with_pcr16(
    svc: Arc<ServiceState>,
    nonce: u64,
    proof_hex: String,
    presented: Option<&str>,
) -> StatusCode {
    let req: VerifyAttestationRequest = serde_json::from_value(serde_json::json!({
        "node_id": NODE, "nonce": nonce, "proof_hex": proof_hex,
        "presented_pcr16_digest_hex": presented,
    }))
    .expect("build request");
    verify_attestation(State(svc), Json(req))
        .await
        .into_response()
        .status()
}

/// A node enrolled with an expected PCR16 attests ONLY with a matching digest
/// bound into the AK signature; an absent or mismatched digest is refused
/// (403) and — critically — does NOT burn the nonce (verify-then-consume), so
/// the node can retry after a corrected measured boot.
#[tokio::test]
async fn attestation_pcr16_match_succeeds_absent_and_mismatch_are_refused() {
    const X: &str = "abababababababababababababababababababababababababababababababab12";
    const Y: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd34cd";
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let svc = svc_with_pcr16_node(public_key_to_pem(&node_key.verifying_key()), X);
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    // (a) Expected PCR16 but the node presents none → 403, nonce preserved.
    let absent = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
    assert_eq!(
        absent,
        StatusCode::FORBIDDEN,
        "expected PCR16, none presented → 403"
    );
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "a PCR16 refusal must not burn the nonce"
    );

    // (b) A wrong digest Y (correctly signed) ≠ the expectation X → 403, preserved.
    let wrong = verify_with_pcr16(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(Y)),
        Some(Y),
    )
    .await;
    assert_eq!(wrong, StatusCode::FORBIDDEN, "mismatched PCR16 → 403");
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "still not burned"
    );

    // (c) The correct digest X bound into the signature → 200 OK, Trusted.
    let ok = verify_with_pcr16(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(X)),
        Some(X),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "matching bound PCR16 attests");
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Trusted
        ),
        "node becomes Trusted after a valid PCR16-bound proof"
    );
}

// ---- Hardware TPM quote enforcement (live wiring) ---------------------

/// The 32-byte PCR16 VALUE a quote node attests, in hex (exactly 64 chars — a
/// real SHA-256 PCR value). The quote carries a HASH OVER this (`SHA256(value)`);
/// the self-report proof carries the value.
const PCR16_VALUE_HEX: &str = "abababababababababababababababababababababababababababababababcd";

/// A node enrolled with an expected PCR16 AND `require_tpm_quote = true` in
/// the policy table, mirroring `svc_with_pcr16_node`.
fn svc_with_quote_node(ak_pem: String, expected_pcr16: &str) -> Arc<ServiceState> {
    let svc = svc_with_pcr16_node(ak_pem, expected_pcr16);
    svc.app
        .store
        .with(|store| store.set_node_attestation_policy(NODE, true))
        .expect("set require_tpm_quote policy");
    svc
}

/// Build `(quote_msg_hex, signature_hex)` for the canonical single-PCR16
/// quote bound to `nonce`, signed by the node's AK.
fn quote_evidence(sk: &SigningKey, nonce: u64, pcr16_value_hex: &str) -> (String, String) {
    let value = hex::decode(pcr16_value_hex).unwrap();
    let quote = kirra_verifier::tpm_quote::marshal_pcr16_quote(&nonce.to_be_bytes(), &value);
    let sig = hex::encode(sk.sign(&quote).to_bytes());
    (hex::encode(quote), sig)
}

/// Post a verify with a self-report digest AND a TPM quote.
async fn verify_with_quote(
    svc: Arc<ServiceState>,
    nonce: u64,
    proof_hex: String,
    presented: Option<&str>,
    quote: Option<(String, String)>,
) -> StatusCode {
    let tpm_quote = quote.map(|(q, s)| {
        serde_json::json!({
            "quote_msg_hex": q, "signature_hex": s,
        })
    });
    let req: VerifyAttestationRequest = serde_json::from_value(serde_json::json!({
        "node_id": NODE, "nonce": nonce, "proof_hex": proof_hex,
        "presented_pcr16_digest_hex": presented,
        "tpm_quote": tpm_quote,
    }))
    .expect("build request");
    verify_attestation(State(svc), Json(req))
        .await
        .into_response()
        .status()
}

/// A node whose policy requires a TPM quote is REFUSED when it presents only
/// a (valid) self-reported proof and no quote — fail-closed, nonce preserved.
#[tokio::test]
async fn tpm_quote_required_but_absent_is_refused() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let svc = svc_with_quote_node(
        public_key_to_pem(&node_key.verifying_key()),
        PCR16_VALUE_HEX,
    );
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    let status = verify_with_quote(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
        None, // no quote, but policy requires one
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "policy requires a quote, none presented → 403"
    );
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "a quote refusal must not burn the nonce"
    );
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Unknown
        ),
        "node is not trusted without the required quote"
    );
}

/// A valid TPM quote (correct nonce + PCR16 digest, AK-signed) attests the
/// node → 200 OK, Trusted.
#[tokio::test]
async fn tpm_quote_valid_attests_node_trusted() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let svc = svc_with_quote_node(
        public_key_to_pem(&node_key.verifying_key()),
        PCR16_VALUE_HEX,
    );
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    let status = verify_with_quote(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
        Some(quote_evidence(&node_key, nonce, PCR16_VALUE_HEX)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "valid quote attests");
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Trusted
        ),
        "node becomes Trusted after a valid hardware quote"
    );
}

/// A quote signed by the WRONG key is refused (401) and the nonce is NOT
/// burned, so the node can retry with a genuine quote.
#[tokio::test]
async fn tpm_quote_invalid_is_refused_and_nonce_preserved() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let attacker = SigningKey::from_bytes(&[9u8; 32]); // not the registered AK
    let svc = svc_with_quote_node(
        public_key_to_pem(&node_key.verifying_key()),
        PCR16_VALUE_HEX,
    );
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    // The base proof is genuine (node_key); only the QUOTE is forged.
    let status = verify_with_quote(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
        Some(quote_evidence(&attacker, nonce, PCR16_VALUE_HEX)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "quote signed by the wrong key → 401"
    );
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "an invalid quote must not burn the nonce"
    );
}

/// A node with NO quote policy is unaffected: the self-report path attests
/// without any quote (back-compat). Also proves a presented quote is still
/// rejected if it does not verify, even when the policy does not require one.
#[tokio::test]
async fn tpm_quote_policy_absent_is_back_compat() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    // svc_with_pcr16_node sets NO attestation policy → require_tpm_quote=false.
    let svc = svc_with_pcr16_node(
        public_key_to_pem(&node_key.verifying_key()),
        PCR16_VALUE_HEX,
    );
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    let status = verify_with_pcr16(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "no quote policy → self-report path still attests"
    );
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Trusted
        ),
        "node attests via the back-compat self-report path"
    );
}

// ---- WP-16 (MGA G-8): quote-required-default env gate at registration -----

/// The pure `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT` parser: `1`/`true`
/// (case-insensitive, trimmed) enable it; everything else (unset/empty/`0`/
/// `false`/garbage) is OFF — fail-safe to the byte-identical back-compat default.
#[test]
fn require_tpm_quote_fleet_default_parses_the_gate() {
    for on in ["1", "true", "TRUE", "True", "  true  ", "\t1"] {
        assert!(
            super::require_tpm_quote_fleet_default(Some(on)),
            "{on:?} must enable"
        );
    }
    for off in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("yes"),
        Some("2"),
        Some("on"),
    ] {
        assert!(
            !super::require_tpm_quote_fleet_default(off),
            "{off:?} must stay off"
        );
    }
}

/// The pure resolver: an EXPLICIT request field always wins; an OMITTED field
/// (`None`) defers to the fleet default. This is the whole WP-16 policy.
#[test]
fn resolve_require_tpm_quote_explicit_wins_else_default() {
    assert!(
        super::resolve_require_tpm_quote(Some(true), false),
        "explicit true wins over off"
    );
    assert!(
        !super::resolve_require_tpm_quote(Some(false), true),
        "explicit false opts out under an on default"
    );
    assert!(
        super::resolve_require_tpm_quote(None, true),
        "omitted defers to an on default"
    );
    assert!(
        !super::resolve_require_tpm_quote(None, false),
        "omitted under off default stays off (back-compat)"
    );
}

/// An Active service with an EMPTY node registry — for exercising register_node.
fn active_svc_no_nodes() -> Arc<ServiceState> {
    let app = Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    ));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    Arc::new(ServiceState {
        app,
        posture_cache,
        started_at_ms: now_ms(),
        audit_verifying_key: None,
        fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
        fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
        fabric_causal_log: Arc::new(
            kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None),
        ),
        posture_engine_tx: std::sync::OnceLock::new(),
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    })
}

/// Drive `register_node` for `node_id` with the given (optional) explicit flag,
/// assert 201, and return the persisted `require_tpm_quote` policy. The env gate
/// is UNSET in the test process, so an omitted flag resolves to the off default
/// (no `set_var` — INVARIANT #13; the on-default case is the pure-fn test above).
async fn register_and_policy(
    svc: &Arc<ServiceState>,
    node_id: &str,
    require: Option<bool>,
) -> bool {
    // Always carry a valid AK + 64-hex PCR16 so the quote-required guard (a node
    // that could never attest is rejected) is satisfied — this helper exercises
    // the POLICY resolution, not the missing-material guard (tested separately).
    let ak = public_key_to_pem(&SigningKey::from_bytes(&[5u8; 32]).verifying_key());
    let mut body = serde_json::json!({
        "node_id": node_id,
        "ak_public_pem": ak,
        "expected_pcr16_digest_hex": PCR16_VALUE_HEX,
    });
    if let Some(r) = require {
        body["require_tpm_quote"] = serde_json::json!(r);
    }
    let req: RegisterNodeRequest = serde_json::from_value(body).expect("build request");
    let status = register_node(State(Arc::clone(svc)), Json(req))
        .await
        .into_response()
        .status();
    assert_eq!(status, StatusCode::CREATED, "registration succeeds");
    let nid = node_id.to_string();
    svc.app
        .store
        .call_read(move |s| s.node_requires_tpm_quote(&nid))
        .await
        .expect("store task")
        .expect("policy query")
}

/// An omitted `require_tpm_quote` with the gate UNSET registers a node with NO
/// quote requirement — the back-compat default is preserved byte-for-byte.
#[tokio::test]
async fn register_omitted_field_defaults_off_when_gate_unset() {
    let svc = active_svc_no_nodes();
    assert!(
        !register_and_policy(&svc, "node-omit", None).await,
        "omitted field + unset gate → not quote-required (back-compat)"
    );
}

/// An EXPLICIT `require_tpm_quote: true` in the request enrolls the node as
/// quote-required regardless of the gate — the one-call measured-boot enrollment.
#[tokio::test]
async fn register_explicit_true_requires_quote() {
    let svc = active_svc_no_nodes();
    assert!(
        register_and_policy(&svc, "node-strict", Some(true)).await,
        "explicit require_tpm_quote:true persists the policy"
    );
}

/// An EXPLICIT `require_tpm_quote: false` opts a (TPM-less) node OUT even when a
/// fleet default would otherwise apply — the explicit field always wins.
#[tokio::test]
async fn register_explicit_false_opts_out() {
    let svc = active_svc_no_nodes();
    assert!(
        !register_and_policy(&svc, "node-nopt", Some(false)).await,
        "explicit require_tpm_quote:false is honored (a TPM-less node can still enroll)"
    );
}

/// The pure PCR16-SHA256 validator: exactly 64 hex chars, trimmed.
#[test]
fn is_valid_pcr16_sha256_hex_requires_64_hex() {
    assert!(
        super::is_valid_pcr16_sha256_hex(&"ab".repeat(32)),
        "64 hex chars ok"
    );
    assert!(
        super::is_valid_pcr16_sha256_hex(&format!("  {}  ", "cd".repeat(32))),
        "trimmed"
    );
    assert!(
        !super::is_valid_pcr16_sha256_hex(&"ab".repeat(31)),
        "62 chars rejected"
    );
    assert!(
        !super::is_valid_pcr16_sha256_hex(&"ab".repeat(33)),
        "66 chars rejected"
    );
    assert!(!super::is_valid_pcr16_sha256_hex(""), "empty rejected");
    assert!(
        !super::is_valid_pcr16_sha256_hex(&format!("xy{}", "ab".repeat(31))),
        "non-hex rejected"
    );
}

/// Copilot #861 fail-closed: a quote-required registration MISSING its AK or a
/// valid PCR16 is rejected 400 — a node that could never attest is never minted.
#[tokio::test]
async fn register_quote_required_without_material_is_400() {
    let svc = active_svc_no_nodes();
    let ak = public_key_to_pem(&SigningKey::from_bytes(&[5u8; 32]).verifying_key());

    // require=true but NO ak + NO pcr16 → 400.
    let no_material: RegisterNodeRequest = serde_json::from_value(serde_json::json!({
        "node_id": "n1", "require_tpm_quote": true,
    }))
    .unwrap();
    let s = register_node(State(Arc::clone(&svc)), Json(no_material))
        .await
        .into_response()
        .status();
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "quote-required with no AK/PCR16 → 400"
    );

    // require=true, AK present but PCR16 the wrong length → 400.
    let bad_pcr: RegisterNodeRequest = serde_json::from_value(serde_json::json!({
        "node_id": "n2", "require_tpm_quote": true,
        "ak_public_pem": ak, "expected_pcr16_digest_hex": "abab",
    }))
    .unwrap();
    let s = register_node(State(Arc::clone(&svc)), Json(bad_pcr))
        .await
        .into_response()
        .status();
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "a non-64-hex PCR16 under require_tpm_quote → 400"
    );

    // Neither node was minted.
    assert!(!svc.app.nodes.contains_key("n1") && !svc.app.nodes.contains_key("n2"));
}

/// WP-16 end-to-end: ENROLL a node through the real `register_node` handler with
/// the exact body `kirra-ota-ctl enroll` posts (AK + PCR16 + require_tpm_quote),
/// then prove the measured-boot contract holds — a self-report WITHOUT a quote is
/// now REFUSED, and only a genuine TPM quote (generated via `marshal_pcr16_quote`)
/// attests the node Trusted. This ties the provisioning path to the LIVE quote
/// enforcement: challenge → quote → verify.
#[tokio::test]
async fn enroll_via_register_handler_then_quote_attests_end_to_end() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let svc = active_svc_no_nodes();

    // 1. Enroll — the wire body `enroll` builds (require_tpm_quote explicit).
    let body = serde_json::json!({
        "node_id": NODE,
        "ak_public_pem": public_key_to_pem(&node_key.verifying_key()),
        "expected_pcr16_digest_hex": PCR16_VALUE_HEX,
        "require_tpm_quote": true,
    });
    let req: RegisterNodeRequest = serde_json::from_value(body).expect("build request");
    let status = register_node(State(Arc::clone(&svc)), Json(req))
        .await
        .into_response()
        .status();
    assert_eq!(status, StatusCode::CREATED, "enrollment registers the node");

    // 2. A valid self-report but NO quote is refused — enrollment made a quote
    //    mandatory (fail-closed, nonce preserved for the retry).
    let nonce = 0x1122_3344_5566_7788;
    svc.app.issue_challenge(NODE, nonce, now_ms());
    let no_quote = verify_with_quote(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
        None,
    )
    .await;
    assert_eq!(
        no_quote,
        StatusCode::FORBIDDEN,
        "an enrolled node must present a quote"
    );
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "the refusal preserves the nonce"
    );

    // 3. A genuine TPM quote attests → Trusted.
    let ok = verify_with_quote(
        Arc::clone(&svc),
        nonce,
        sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
        Some(PCR16_VALUE_HEX),
        Some(quote_evidence(&node_key, nonce, PCR16_VALUE_HEX)),
    )
    .await;
    assert_eq!(
        ok,
        StatusCode::OK,
        "a valid quote attests the enrolled node"
    );
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Trusted
        ),
        "the enrolled measured-boot node becomes Trusted after a valid quote"
    );
}

#[tokio::test]
async fn failed_proof_does_not_burn_the_nonce_then_valid_proof_succeeds() {
    let node_key = SigningKey::from_bytes(&[7u8; 32]);
    let attacker_key = SigningKey::from_bytes(&[9u8; 32]); // not the registered AK
    let svc = svc_with_registered_node(public_key_to_pem(&node_key.verifying_key()));

    let nonce = 0xABCD_1234_5678_9F01;
    svc.app.issue_challenge(NODE, nonce, now_ms());

    // 1) A bad proof (signed by the wrong key) is rejected 401 — and the
    //    pending nonce is NOT consumed (verify-then-consume).
    let bad = verify(
        Arc::clone(&svc),
        nonce,
        sign_proof(&attacker_key, NODE, nonce),
    )
    .await;
    assert_eq!(bad, StatusCode::UNAUTHORIZED, "a bad proof is refused");
    assert!(
        svc.app.pending_challenges.contains_key(NODE),
        "a FAILED proof must not burn the pending nonce"
    );

    // 2) The legitimate node, with the SAME outstanding nonce, now succeeds.
    let good = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
    assert_eq!(
        good,
        StatusCode::OK,
        "valid proof over the still-outstanding nonce attests"
    );
    assert!(
        matches!(
            svc.app.nodes.get(NODE).unwrap().status,
            NodeTrustState::Trusted
        ),
        "node becomes Trusted after a valid proof"
    );

    // 3) Single-use: the nonce is now consumed; a replay is a 409 conflict.
    let replay = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
    assert_eq!(
        replay,
        StatusCode::CONFLICT,
        "the consumed nonce cannot be replayed"
    );
}
