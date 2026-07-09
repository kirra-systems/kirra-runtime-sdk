// src/bin/kirra_verifier_service/verdicts.rs — EP-17 explainable safety verdicts.
//
// `GET /verdicts/{verdict_id}` — the operator view of one DENIED actuator
// command, rendered from the hash-chained (and, with a signing key installed,
// Ed25519-signed) audit record the deny arm wrote. Response layers:
//
//   * the MACHINE code (`DenyCode` wire token) → the reviewed HUMAN
//     explanation (`kirra_verifier::verdicts::explain_deny_token`);
//   * the denied inputs as recorded, plus `inputs_digest_sha256` — the SHA-256
//     of the EXACT chained `event_json` bytes (recompute it over `audit.
//     event_json` to confirm the rendering matches the ledger);
//   * the chain fields (sequence, record/prev hash, hash version, signature,
//     key id) that make the artifact INDEPENDENTLY verifiable off-box — the
//     same shape `verify_shipped_chain` re-verifies.
//
// AUDITOR-scoped at the router layer (SCOPE_AUDIT_READ): a verdict exposes the
// denied command's raw inputs, which is audit-read material, not public
// telemetry. Read-only — mounted in `auditor_routes`.
//
// `use super::*` pulls the binary root's helpers (`ServiceState`, the axum
// extractors, `json!`), same as the sibling handler modules.

use super::*;

use sha2::{Digest as VerdictShaDigest, Sha256 as VerdictSha256};

/// GET /verdicts/{verdict_id} — retrieve one denial as a signed, explained
/// artifact. 400 malformed id (pre-LIKE validation, fail-closed), 404 unknown,
/// 500 store failure.
pub(crate) async fn get_verdict_handler(
    State(svc): State<Arc<ServiceState>>,
    axum::extract::Path(verdict_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !kirra_verifier::verdicts::is_valid_verdict_id(&verdict_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "malformed verdict id (expect 32 lowercase hex chars)"})),
        )
            .into_response();
    }

    // Read-only lookup → the read-replica pool, never the writer mutex (an
    // auditor retrieving verdicts must not contend the audit writer).
    let id_for_query = verdict_id.clone();
    let loaded = svc
        .app
        .store
        .call_read(move |store| store.load_audit_record_by_verdict_id(&id_for_query))
        .await;

    let record = match loaded {
        Ok(Ok(Some(rec))) => rec,
        Ok(Ok(None)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no verdict with that id"})),
            )
                .into_response();
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "verdict lookup failed (store)");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "verdict lookup failed"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "verdict lookup failed (offload)");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "verdict lookup failed"})),
            )
                .into_response();
        }
    };

    // The payload the chain hash covers, parsed for rendering. A parse failure
    // on a chained record is corruption — surface it rather than 500-ing away
    // the evidence (the raw bytes + chain fields still render).
    let payload: serde_json::Value =
        serde_json::from_str(&record.event_json).unwrap_or(serde_json::Value::Null);
    let code = payload
        .get("violation")
        .and_then(|v| v.as_str())
        .unwrap_or("UNPARSEABLE_RECORD");

    // The inputs digest: SHA-256 over the EXACT chained event_json bytes.
    let inputs_digest = {
        let mut h = VerdictSha256::new();
        h.update(record.event_json.as_bytes());
        hex::encode(h.finalize())
    };

    (
        StatusCode::OK,
        Json(json!({
            "verdict_id": verdict_id,
            "denied": true,
            "code": code,
            "explanation": kirra_verifier::verdicts::explain_deny_token(code),
            "denied_at_ms": record.created_at_ms,
            "posture_at_rejection": payload.get("posture_at_rejection"),
            "inputs": payload.get("proposed_command"),
            "inputs_digest_sha256": inputs_digest,
            "audit": {
                "sequence": record.sequence,
                "event_type": record.event_type,
                "event_json": record.event_json,
                "previous_hash_hex": record.previous_hash_hex,
                "record_hash_hex": record.record_hash_hex,
                "hash_version": record.hash_version,
                "signature_b64": record.signature_b64,
                "key_id": record.key_id,
            },
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests — the EP-17 DoD end-to-end: a denied actuator command is retrievable
// as a signed, human-readable artifact.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod verdict_handler_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use kirra_verifier::posture_cache::SharedPostureCache;
    use tower::util::ServiceExt;

    /// A minimal router: the REAL actuator envelope middleware in front of a
    /// stub actuator handler, plus the REAL verdict retrieval route — no auth
    /// layers (the WS-1 gate is proven by the root's scope-gated enumeration
    /// test; INV-13 forbids `set_var` here).
    fn verdict_router(svc: Arc<ServiceState>) -> Router {
        // The envelope middleware guards ONLY the actuator route (production
        // shape) — `route_layer` scopes it before the merge, so the verdict
        // GET is not asked to parse a command body.
        let actuator = Router::new()
            .route("/actuator/motion/command", post(|| async { "forwarded" }))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&svc),
                kirra_verifier::gateway::policy_layer::enforce_actuator_safety_envelope,
            ));
        Router::new()
            .route("/verdicts/{verdict_id}", get(get_verdict_handler))
            .merge(actuator)
            .with_state(svc)
    }

    /// An Active in-memory service with a FRESH Nominal posture cache (the
    /// actuator gate fail-closes on a stale/empty cache, so the deny under
    /// test must come from the KINEMATIC check, not the posture gate).
    fn svc_with_nominal_posture() -> Arc<ServiceState> {
        let app = Arc::new(AppState::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
            VerifierOperationMode::Active,
        ));
        let cached = kirra_verifier::posture_cache::CachedFleetPosture::new(
            kirra_verifier::verifier::FleetPosture::Nominal,
        );
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(cached)));
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

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// EP-17 DoD: deny → 400 body carries the id + explanation → GET
    /// /verdicts/{id} returns the chained artifact whose digest matches the
    /// exact chained bytes.
    #[tokio::test]
    async fn denied_command_is_retrievable_as_explained_artifact() {
        // No vehicle-class init in tests: `global_vehicle_class()` falls back
        // to the frozen Robotaxi reference instance (documented baseline).
        let svc = svc_with_nominal_posture();
        let app = verdict_router(Arc::clone(&svc));

        // A command with a non-physical dt — DenyBreach(InvalidTimeDelta).
        let deny = app
            .clone()
            .oneshot(
                Request::post("/actuator/motion/command")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "linear_velocity_mps": 5.0,
                            "current_velocity_mps": 5.0,
                            "delta_time_s": 0.0,
                            "steering_angle_deg": 0.0,
                            "current_steering_angle_deg": 0.0,
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(deny.status(), StatusCode::BAD_REQUEST);
        let deny_body = body_json(deny).await;
        assert_eq!(deny_body["denied"], serde_json::json!(true));
        assert_eq!(deny_body["code"], "INVALID_TIME_DELTA");
        let explanation = deny_body["explanation"].as_str().expect("explanation");
        assert!(explanation.contains("zero or negative"), "{explanation}");
        let verdict_id = deny_body["verdict_id"].as_str().expect("verdict id").to_string();
        assert!(kirra_verifier::verdicts::is_valid_verdict_id(&verdict_id));

        // The audit write is fire-and-forget on the deny path (fallback direct
        // write in tests — no writer task installed): it lands via the store
        // actor; one store round-trip later it is durably chained.
        let retrieved = app
            .clone()
            .oneshot(
                Request::get(format!("/verdicts/{verdict_id}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(retrieved.status(), StatusCode::OK);
        let v = body_json(retrieved).await;
        assert_eq!(v["verdict_id"], serde_json::json!(verdict_id));
        assert_eq!(v["code"], "INVALID_TIME_DELTA");
        assert_eq!(v["explanation"].as_str(), Some(explanation));
        assert_eq!(v["posture_at_rejection"], "Nominal");
        assert_eq!(v["inputs"]["delta_time_s"], serde_json::json!(0.0));

        // The rendered digest matches the EXACT chained bytes.
        let event_json = v["audit"]["event_json"].as_str().expect("chained bytes");
        let recomputed = {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(event_json.as_bytes()))
        };
        assert_eq!(v["inputs_digest_sha256"].as_str(), Some(recomputed.as_str()));
        assert!(v["audit"]["record_hash_hex"].as_str().is_some_and(|h| h.len() == 64));
    }

    #[tokio::test]
    async fn malformed_and_unknown_ids_fail_closed() {
        let svc = svc_with_nominal_posture();
        let app = verdict_router(svc);

        // LIKE metacharacters / wrong shape → 400 before any query.
        for bad in ["zz", "….", "abc%", "0123456789ABCDEF0123456789ABCDEF"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::get(format!("/verdicts/{bad}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "id={bad}");
        }

        // Well-formed but unknown → 404.
        let resp = app
            .oneshot(
                Request::get("/verdicts/0123456789abcdef0123456789abcdef")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
