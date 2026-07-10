// posture_gate_real_router_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// Issue #72 — posture gate is wired on the REAL assembled router.
//
// The external `tests/posture_gate_integration.rs` builds a *representative*
// router (stub handlers at the production paths) precisely because, as an
// out-of-crate integration test, it cannot see the binary's inline assembly.
// That left a residual gap: nothing asserted the gate is mounted on the
// router `main()` actually serves. These tests close it by driving requests
// through `build_app()` — the exact production assembly — and proving the
// posture gate (and its exemptions) are in force on it.
// ---------------------------------------------------------------------------

use super::build_app;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // for `oneshot`

use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

/// Builds an Active `ServiceState` with the given seeded posture (or a
/// cold cache when `None`), mirroring the production field set.
fn build_state(initial: Option<CachedFleetPosture>) -> Arc<ServiceState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(initial));
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
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    })
}

fn state_with(posture: FleetPosture) -> Arc<ServiceState> {
    build_state(Some(CachedFleetPosture::new(posture)))
}

/// Drives one request through the REAL assembled app and returns its status.
/// A fresh app per call because `oneshot` consumes the router.
async fn status_through_real_app(svc: Arc<ServiceState>, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .expect("build request");
    build_app(svc, None)
        .oneshot(req)
        .await
        .expect("router service should not panic")
        .status()
}

// --- OTA campaign control-plane (WS-4 / Track 3 · Fleet Plane) ---------

const CAMPAIGN_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A router mounting ONLY the real campaign handlers, WITHOUT the admin-scope
/// layer — the INV-13-compliant way to exercise the authenticated lifecycle
/// end-to-end (the auth composition itself is covered by `authz::tests` +
/// `tests/authz_rbac.rs`, and the gating is proven on the real router below).
fn campaign_router(svc: Arc<ServiceState>) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/system/campaigns",
            post(super::create_campaign_handler).get(super::list_campaigns_handler),
        )
        .route(
            "/system/campaigns/summary",
            get(super::campaigns_summary_handler),
        )
        // The node adoption report (bare here for logic testing; its identity gate
        // is proven by `ws1_scope_gated_routes_fail_closed_on_real_router`).
        .route("/fleet/campaigns/report", post(super::report_node_artifact))
        .route(
            "/system/campaigns/{campaign_id}",
            get(super::get_campaign_handler),
        )
        .route(
            "/system/campaigns/{campaign_id}/arm",
            post(super::arm_campaign_handler),
        )
        .route(
            "/system/campaigns/{campaign_id}/advance",
            post(super::advance_campaign_handler),
        )
        .route(
            "/system/campaigns/{campaign_id}/halt",
            post(super::halt_campaign_handler),
        )
        .with_state(svc)
}

/// Fire one request at a fresh clone of the campaign router (oneshot consumes
/// it) and return the status + parsed JSON body.
async fn campaign_req(
    svc: Arc<ServiceState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let rb = Request::builder().method(method).uri(path);
    let req = match body {
        Some(b) => rb
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => rb.body(Body::empty()),
    }
    .expect("build request");
    let resp = campaign_router(svc)
        .oneshot(req)
        .await
        .expect("router service should not panic");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn create_body(id: &str) -> String {
    serde_json::json!({
        "campaign_id": id,
        "artifact_digest": CAMPAIGN_DIGEST,
        "artifact_version": "v1.2.3",
        "cohorts": ["canary", "fleet"],
        "stages": [10, 50, 100],
    })
    .to_string()
}

/// The campaign routes are mounted on the REAL assembled router behind the
/// admin-scope gate and fail closed WITHOUT a credential (503 when
/// `KIRRA_ADMIN_TOKEN` is unset — INV-13 forbids setting it here — or 401 if CI
/// configures one). Never 2xx unauthenticated.
#[tokio::test]
async fn campaign_routes_fail_closed_without_credential() {
    let cases = [
        ("POST", "/system/campaigns"),
        ("GET", "/system/campaigns"),
        ("GET", "/system/campaigns/summary"),
        ("GET", "/system/campaigns/c1"),
        ("POST", "/system/campaigns/c1/arm"),
        ("POST", "/system/campaigns/c1/advance"),
        ("POST", "/system/campaigns/c1/halt"),
    ];
    for (method, path) in cases {
        let status = status_through_real_app(state_with(FleetPosture::Nominal), method, path).await;
        assert!(
            status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::UNAUTHORIZED,
            "{method} {path} must fail closed (503/401) without a credential; got {status}"
        );
        assert!(
            !status.is_success(),
            "{method} {path} must never reach the handler unauthenticated; got {status}"
        );
    }
}

/// End-to-end lifecycle on the real handlers: create → arm → advance (Nominal)
/// → the fleet regresses → advance now HALTS fail-closed instead of rolling
/// further → the halted campaign is terminal (a further advance is 409). Proves
/// the posture-bound halt fires through the actual HTTP handler, not just the
/// pure engine.
#[tokio::test]
async fn campaign_lifecycle_and_fail_closed_posture_halt_end_to_end() {
    let svc = state_with(FleetPosture::Nominal);

    let (st, j) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-e2e")),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "create; body={j}");
    assert_eq!(j["state"], "draft");
    assert_eq!(j["rollout_percent"], 0);

    let (st, _) = campaign_req(svc.clone(), "POST", "/system/campaigns/camp-e2e/arm", None).await;
    assert_eq!(st, StatusCode::OK, "arm");

    // Advance under Nominal → first stage (10%).
    let (st, j) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-e2e/advance",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "advance; body={j}");
    assert_eq!(j["outcome"]["advanced"], true);
    assert_eq!(j["outcome"]["rollout_percent"], 10);
    assert_eq!(j["campaign"]["state"], "rolling");

    // The fleet regresses to LockedOut — flip the posture cache on the SAME svc
    // (store, and therefore the campaign, intact).
    *svc.posture_cache.write().expect("posture cache") =
        Some(CachedFleetPosture::new(FleetPosture::LockedOut));

    // The next advance HALTS fail-closed rather than rolling to 50%.
    let (st, j) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-e2e/advance",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "halt-advance; body={j}");
    assert_eq!(j["outcome"]["halted"], true);
    assert_eq!(j["outcome"]["halt_reason"], "posture_locked_out");
    assert_eq!(j["campaign"]["state"], "halted");
    // Rollout never advanced past the last safe stage.
    assert_eq!(j["campaign"]["rollout_percent"], 10);

    // Terminal: a further advance is a 409 conflict (the engine authors no resume).
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-e2e/advance",
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "terminal campaign must reject advance"
    );
}

/// The fleet rollout summary reflects real campaign state through the HTTP
/// handler: counts by state, active-campaign stage progress, and a halted
/// campaign surfaced with its reason.
#[tokio::test]
async fn campaign_summary_reflects_fleet_state_end_to_end() {
    let svc = state_with(FleetPosture::Nominal);

    // camp-roll: create → arm → advance → Rolling @ 10% (stage 1 of 3).
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-roll")),
    )
    .await;
    campaign_req(svc.clone(), "POST", "/system/campaigns/camp-roll/arm", None).await;
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-roll/advance",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // camp-draft: left in Draft. camp-halt: armed then operator-halted (halt is
    // only legal from an active state).
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-draft")),
    )
    .await;
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-halt")),
    )
    .await;
    campaign_req(svc.clone(), "POST", "/system/campaigns/camp-halt/arm", None).await;
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-halt/halt",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "operator halt of an armed campaign");

    let (st, j) = campaign_req(svc.clone(), "GET", "/system/campaigns/summary", None).await;
    assert_eq!(st, StatusCode::OK, "summary; body={j}");
    assert_eq!(j["total"], 3);
    assert_eq!(j["draft"], 1);
    assert_eq!(j["rolling"], 1);
    assert_eq!(j["halted"], 1);

    // The active list holds exactly the rolling campaign, with its stage progress
    // (camp-draft is Draft, camp-halt is Halted — neither is active).
    assert_eq!(j["active"].as_array().map(|a| a.len()), Some(1));
    let roll = &j["active"][0];
    assert_eq!(roll["campaign_id"], "camp-roll");
    assert_eq!(roll["state"], "rolling");
    assert_eq!(roll["rollout_percent"], 10);
    assert_eq!(roll["stage"], 1);
    assert_eq!(roll["stage_count"], 3);

    // The halted campaign is surfaced WITH its reason.
    assert_eq!(j["halted_campaigns"].as_array().map(|a| a.len()), Some(1));
    assert_eq!(j["halted_campaigns"][0]["campaign_id"], "camp-halt");
    assert_eq!(j["halted_campaigns"][0]["halt_reason"], "operator_halt");
}

/// Node adoption reporting closes the observability loop: a node reports the
/// digest it is running, and the summary's active-campaign `applied_nodes`
/// reflects it. Also proves validation (bad digest → 400, no row written) and
/// upsert semantics (a node's re-report replaces, never double-counts).
#[tokio::test]
async fn node_adoption_report_reflects_in_summary_end_to_end() {
    let svc = state_with(FleetPosture::Nominal);

    // A Rolling campaign (create → arm → advance) whose digest nodes will adopt.
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-adopt")),
    )
    .await;
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-adopt/arm",
        None,
    )
    .await;
    campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-adopt/advance",
        None,
    )
    .await;

    // A bad digest is rejected (400) and records nothing.
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/fleet/campaigns/report",
        Some(&serde_json::json!({ "node_id": "robot-x", "applied_digest": "nothex" }).to_string()),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "non-hex digest must 400");

    // Two nodes report the campaign's digest; robot-1 reports TWICE (upsert).
    let report = |node: &str| {
        serde_json::json!({
            "node_id": node,
            "applied_digest": CAMPAIGN_DIGEST,
            "campaign_id": "camp-adopt",
            "artifact_version": "v1.2.3",
        })
        .to_string()
    };
    for body in [report("robot-1"), report("robot-2"), report("robot-1")] {
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/fleet/campaigns/report", Some(&body)).await;
        assert_eq!(st, StatusCode::OK, "valid report recorded");
    }

    let (st, j) = campaign_req(svc.clone(), "GET", "/system/campaigns/summary", None).await;
    assert_eq!(st, StatusCode::OK);
    let roll = &j["active"][0];
    assert_eq!(roll["campaign_id"], "camp-adopt");
    // TWO distinct nodes adopted the digest (robot-1's re-report did not double-count).
    assert_eq!(roll["applied_nodes"], 2);
}

/// The summary's static path segment wins the match over `{campaign_id}`: a GET
/// on `/system/campaigns/summary` returns the summary, never a campaign lookup
/// for an id "summary".
#[tokio::test]
async fn campaign_summary_route_is_not_shadowed_by_id_param() {
    let svc = state_with(FleetPosture::Nominal);
    let (st, j) = campaign_req(svc.clone(), "GET", "/system/campaigns/summary", None).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "summary must resolve, not 404 as a missing id"
    );
    // A summary body has the count fields; a campaign body would not.
    assert!(
        j.get("total").is_some(),
        "got a summary, not a campaign; body={j}"
    );
}

/// Fail-closed on posture UNAVAILABILITY: with a cold posture cache
/// (`gate_posture` resolves empty/stale → LockedOut), the very first advance
/// halts — a rollout never proceeds when fleet posture cannot be confirmed.
#[tokio::test]
async fn campaign_advance_fails_closed_when_posture_unavailable() {
    let svc = build_state(None); // cold cache

    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("camp-cold")),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = campaign_req(svc.clone(), "POST", "/system/campaigns/camp-cold/arm", None).await;
    assert_eq!(st, StatusCode::OK);

    let (st, j) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns/camp-cold/advance",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "advance; body={j}");
    assert_eq!(
        j["outcome"]["halted"], true,
        "cold posture must halt, not roll"
    );
    assert_eq!(j["campaign"]["state"], "halted");
}

/// Route-level validation + error mapping through the real handlers: bad digest
/// → 422, duplicate id → 409, unknown campaign → 404.
#[tokio::test]
async fn campaign_route_validation_and_error_mapping() {
    let svc = state_with(FleetPosture::Nominal);

    // Malformed artifact digest → 422.
    let bad = serde_json::json!({
        "campaign_id": "camp-bad", "artifact_digest": "not-hex",
        "artifact_version": "v1", "cohorts": ["a"], "stages": [100],
    })
    .to_string();
    let (st, _) = campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&bad)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // First create OK, duplicate id → 409.
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("dup")),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = campaign_req(
        svc.clone(),
        "POST",
        "/system/campaigns",
        Some(&create_body("dup")),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Advancing an unknown campaign → 404.
    let (st, _) = campaign_req(svc.clone(), "POST", "/system/campaigns/ghost/advance", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Percentage the seeded campaign is parked at — a REACHABLE mid-rollout stage.
const SEED_ROLLOUT_PERCENT: u8 = 50;

/// Seed a genuinely reachable `Rolling` campaign (arm + one real `advance` to a
/// mid-rollout `SEED_ROLLOUT_PERCENT`% stage — NOT a manufactured 100% Rolling
/// state, which the engine never produces because the final 100% stage
/// transitions to `Completed`). No admin auth needed for an in-process store
/// write. Leaves the campaign at `Rolling` / `SEED_ROLLOUT_PERCENT`% so the
/// assignment read exercises the PARTIAL-rollout membership path.
fn seed_rolling_campaign(svc: &Arc<ServiceState>, id: &str, cohort: &str) {
    use kirra_verifier::ota_campaign::{Campaign, CampaignState};
    svc.app
        .store
        .with(|store| {
            let mut c = Campaign::new(
                id,
                CAMPAIGN_DIGEST,
                "v1",
                vec![cohort.to_string()],
                vec![SEED_ROLLOUT_PERCENT, 100],
                1_000,
            )
            .unwrap();
            c.arm(1_100).unwrap();
            // One real advance under Nominal → Rolling at SEED_ROLLOUT_PERCENT%.
            c.advance(kirra_verifier::verifier::FleetPosture::Nominal, 1_200)
                .unwrap();
            assert_eq!(c.state, CampaignState::Rolling);
            assert_eq!(c.rollout_percent, SEED_ROLLOUT_PERCENT);
            store.insert_campaign(&c)
        })
        .expect("seed campaign");
}

/// A `node-N` id that IS (or is NOT, when `want_rolled` is false) inside
/// `campaign_id`'s rolled bucket at `SEED_ROLLOUT_PERCENT`% — chosen
/// deterministically so the router test asserts real partial-rollout membership.
fn node_rolled_at_seed(campaign_id: &str, want_rolled: bool) -> String {
    use kirra_verifier::ota_campaign::is_node_rolled;
    (0..10_000)
        .map(|i| format!("node-{i}"))
        .find(|n| is_node_rolled(campaign_id, n, SEED_ROLLOUT_PERCENT) == want_rolled)
        .expect("a node with the desired rolled status exists")
}

/// GET the node assignment through the REAL assembled router and return the
/// status + parsed JSON body.
async fn assignment_req(
    svc: Arc<ServiceState>,
    node_id: &str,
    cohorts: &str,
) -> (StatusCode, serde_json::Value) {
    let uri = format!("/fleet/campaigns/assignment/{node_id}?cohorts={cohorts}");
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    let resp = build_app(svc, None)
        .oneshot(req)
        .await
        .expect("router service should not panic");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

/// The node-facing assignment read is mounted on the real router, is reachable
/// WITHOUT auth (public read-only), and resolves a genuinely reachable
/// PARTIAL-rollout campaign to a signed-artifact assignment — a node inside the
/// rolled bucket gets the artifact, a node in the SAME cohort but OUTSIDE the
/// rolled bucket does not, and a node in a different cohort does not.
#[tokio::test]
async fn node_assignment_resolves_on_real_router_under_nominal() {
    let svc = state_with(FleetPosture::Nominal);
    seed_rolling_campaign(&svc, "camp-assign", "canary"); // Rolling @ 50%

    // A cohort node INSIDE the 50% rolled bucket gets the artifact.
    let rolled = node_rolled_at_seed("camp-assign", true);
    let (st, j) = assignment_req(Arc::clone(&svc), &rolled, "canary").await;
    assert_eq!(
        st,
        StatusCode::OK,
        "public assignment read must be reachable; body={j}"
    );
    assert_eq!(j["rolled"], true, "{rolled} is inside the 50% bucket");
    assert_eq!(j["artifact_digest"], CAMPAIGN_DIGEST);
    assert_eq!(j["campaign_id"], "camp-assign");

    // A cohort node OUTSIDE the 50% rolled bucket gets no assignment (this is
    // the partial-rollout path the old 100% seed could not exercise).
    let unrolled = node_rolled_at_seed("camp-assign", false);
    let (st, j) = assignment_req(Arc::clone(&svc), &unrolled, "canary").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["rolled"], false, "{unrolled} is outside the 50% bucket");
    assert_eq!(j["artifact_digest"], serde_json::Value::Null);

    // A node in a DIFFERENT cohort gets no assignment even if it would be in
    // the rolled bucket.
    let (st, j) = assignment_req(Arc::clone(&svc), &rolled, "other").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["rolled"], false, "cohort mismatch → no assignment");
}

/// The assignment read is posture-gated like the other `/fleet/*` reads: under
/// LockedOut it is denied (503) — no artifact is adopted while the fleet is
/// locked out.
#[tokio::test]
async fn node_assignment_is_denied_under_lockedout() {
    let svc = state_with(FleetPosture::LockedOut);
    seed_rolling_campaign(&svc, "camp-assign", "canary");
    let (st, _) = assignment_req(svc, "node-1", "canary").await;
    assert_eq!(
        st,
        StatusCode::SERVICE_UNAVAILABLE,
        "the assignment read must be posture-gated (denied under LockedOut)"
    );
}

/// LockedOut blocks a functional READ on the production router — proving
/// the gate is mounted on the real assembly, not just the test stand-in.
#[tokio::test]
async fn lockedout_blocks_read_on_real_router() {
    let status =
        status_through_real_app(state_with(FleetPosture::LockedOut), "GET", "/fleet/posture").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the real assembled router must deny GET /fleet/posture under LockedOut; got {status}"
    );
}

/// Posture-dependence on the SAME route + real handler: under Nominal the
/// gate steps aside and the production `get_fleet_posture` handler returns
/// 200 (empty fleet). The LockedOut→503 / Nominal→200 contrast is what
/// proves it is the posture gate — not a blanket 503 — that is wired in.
#[tokio::test]
async fn nominal_passes_read_through_to_real_handler() {
    let status =
        status_through_real_app(state_with(FleetPosture::Nominal), "GET", "/fleet/posture").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the real router must let GET /fleet/posture reach the handler under Nominal; got {status}"
    );
}

/// The safety-critical actuator WRITE is denied under LockedOut on the real
/// router. The posture gate is the outermost layer, so it returns 503
/// before the admin-token / envelope layers ever run.
#[tokio::test]
async fn lockedout_blocks_actuator_write_on_real_router() {
    let status = status_through_real_app(
        state_with(FleetPosture::LockedOut),
        "POST",
        "/actuator/motion/command",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the real router must deny POST /actuator/motion/command under LockedOut; got {status}"
    );
}

/// Option A / ADR-0011 on the REAL assembled router: under **Degraded** the
/// outer posture gate now DEFERS the actuator-motion command to the inner
/// `enforce_actuator_safety_envelope` (decel-to-stop) instead of 503-ing it
/// (`should_route_command` Degraded admits `ReadTelemetry` + `ActuatorMotion`).
///
/// On the real assembly the layer after the posture gate is
/// `require_admin_token`, which 503s when `KIRRA_ADMIN_TOKEN` is unset — and
/// INV-13 forbids `set_var` in this multithreaded test — so a token-less
/// Degraded POST is 503 at the ADMIN layer, masking the deferral by status.
/// The authoritative auth-free proof therefore lives in
/// `tests/posture_gate_integration.rs::test_degraded_defers_actuator_motion_but_blocks_other_writes`.
/// Here we prove it on the REAL assembly WHEN a token is configured: an
/// authenticated Degraded POST reaches the inner envelope (its verdict is a
/// 200/clamp or 400, never the posture/admin 503 nor 401), while the
/// authenticated LockedOut control still 503s at the posture gate, before the
/// envelope. With no token the test degrades to the robust LockedOut control.
#[tokio::test]
async fn degraded_actuator_write_reaches_inner_envelope_on_real_router() {
    use axum::http::header;

    async fn post_actuator(svc: Arc<ServiceState>, bearer: Option<&str>, body: &str) -> StatusCode {
        let mut rb = Request::builder()
            .method("POST")
            .uri("/actuator/motion/command")
            .header("content-type", "application/json");
        if let Some(tok) = bearer {
            rb = rb.header(header::AUTHORIZATION, format!("Bearer {tok}"));
        }
        build_app(svc, None)
            .oneshot(
                rb.body(Body::from(body.to_string()))
                    .expect("build request"),
            )
            .await
            .expect("router service should not panic")
            .status()
    }

    let token = std::env::var("KIRRA_ADMIN_TOKEN").unwrap_or_default();
    if token.is_empty() {
        // No token: the actuator route is admin-gated, so a Degraded POST is
        // 503 at the admin layer (indistinguishable by status from a posture
        // denial). Assert only the robust LockedOut control here; the Option A
        // deferral is proven auth-free in the integration test referenced above.
        let locked = post_actuator(state_with(FleetPosture::LockedOut), None, "{}").await;
        assert_eq!(
            locked,
            StatusCode::SERVICE_UNAVAILABLE,
            "LockedOut must 503 at the posture gate on the real router; got {locked}"
        );
        return;
    }

    // Authenticated. A valid decel command (4.0 -> 3.0 m/s, within MRC 5.0)
    // reaches the inner envelope under Degraded and is admitted there — the
    // status is the ENVELOPE verdict, never the posture/admin 503 or 401.
    let degraded = post_actuator(
        state_with(FleetPosture::Degraded),
        Some(&token),
        r#"{"linear_velocity_mps":3.0,"current_velocity_mps":4.0,"delta_time_s":0.1,"steering_angle_deg":0.0,"current_steering_angle_deg":0.0}"#,
    )
    .await;
    assert!(
        degraded != StatusCode::SERVICE_UNAVAILABLE && degraded != StatusCode::UNAUTHORIZED,
        "Degraded actuator command must reach the inner envelope on the real router \
         (Option A) — not a posture/admin 503 or 401; got {degraded}"
    );

    // LockedOut control: still denied at the posture gate, before the envelope.
    let locked = post_actuator(state_with(FleetPosture::LockedOut), Some(&token), "{}").await;
    assert_eq!(
        locked,
        StatusCode::SERVICE_UNAVAILABLE,
        "LockedOut must still 503 at the posture gate even authenticated; got {locked}"
    );
}

/// WS-1 (#G7) — the new/rewired scope-gated groups are WIRED behind the authz
/// gate on the REAL assembled router and fail closed WITHOUT a credential. Under
/// Nominal the outer posture gate lets each request through to the scope layer,
/// which denies (503 when `KIRRA_ADMIN_TOKEN` is unset — INV-13 forbids setting
/// it here — or 401 if the CI env happens to configure one). Either way the
/// invariant is the same: NEVER 2xx without a valid credential. The positive
/// RBAC paths (integrator/auditor/operator reach exactly their surface) are
/// proven by `authz::tests` (pure truth table) + `tests/authz_rbac.rs`
/// (store↔authz composition), which need neither env nor router.
#[tokio::test]
async fn ws1_scope_gated_routes_fail_closed_on_real_router() {
    // (method, path) for one route in each newly scope-gated group.
    let cases = [
        // admin group — the new principal-mint route (SCOPE_ADMIN).
        ("POST", "/system/principals"),
        // carved auditor group (SCOPE_AUDIT_READ) — must NOT be accidentally open.
        ("GET", "/system/audit/verify"),
        // integration group, switched admin-token → SCOPE_INTEGRATION_EVALUATE.
        ("POST", "/action_filter/evaluate"),
        // WS-4 node adoption report — identity-gated (a node write needs a credential).
        ("POST", "/fleet/campaigns/report"),
        // EP-17 verdict retrieval — auditor tier (exposes denied-command inputs).
        ("GET", "/verdicts/0123456789abcdef0123456789abcdef"),
    ];
    for (method, path) in cases {
        let status = status_through_real_app(state_with(FleetPosture::Nominal), method, path).await;
        assert!(
            status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::UNAUTHORIZED,
            "{method} {path} must fail closed (503/401) without a credential; got {status}"
        );
        assert!(
            !status.is_success(),
            "{method} {path} must never reach the handler unauthenticated; got {status}"
        );
    }
}

/// WS-0.5 DoD — "Prometheus scrape returns fleet-safety series", proven
/// on the REAL assembled router UNDER LockedOut (the scrape must survive
/// exactly the posture it exists to observe). Asserts reachability, the
/// exposition content type, every fleet-safety family, the fail-closed
/// posture gauge value, and that a denial that just happened is visible
/// on the labeled counter — the series are live, not just present.
#[tokio::test]
async fn metrics_scrape_returns_fleet_safety_series_under_lockedout() {
    let svc = state_with(FleetPosture::LockedOut);

    // A functional read denied by the gate first, so the scrape can show
    // a non-zero locked_out denial.
    let denied = status_through_real_app(Arc::clone(&svc), "GET", "/fleet/posture").await;
    assert_eq!(
        denied,
        StatusCode::SERVICE_UNAVAILABLE,
        "precondition: LockedOut denies the functional read"
    );

    let resp = build_app(Arc::clone(&svc), None)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router service should not panic");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "/metrics must remain reachable under LockedOut (posture-exempt)"
    );
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "Prometheus text exposition content type expected; got {content_type:?}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("read body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8 exposition");

    for family in [
        "kirra_fleet_posture{",
        "kirra_posture_cache_stale{",
        "kirra_posture_generation{",
        "kirra_mode_active{",
        "kirra_posture_transitions_total{",
        "kirra_gate_denials_total{",
        "kirra_ha_promotions_total{",
        "kirra_audit_write_drops_total{",
        "kirra_capture_drops_total{",
        "kirra_post_incident_write_failures_total{",
        "kirra_incident_durability_failures_total{",
        "kirra_command_source_write_failures_total{",
        // WS-4 OTA rollout series — always emitted (counts by state), so the
        // fleet's update posture is observable even under LockedOut.
        "kirra_ota_campaigns_total{",
    ] {
        assert!(
            text.contains(family),
            "the scrape must contain the {family} series; got:\n{text}"
        );
    }

    // The live LockedOut posture reads 2 on the gauge (fresh cache → not
    // the stale-synthetic flavor).
    assert!(
        text.lines()
            .any(|l| l.starts_with("kirra_fleet_posture{") && l.ends_with(" 2")),
        "the posture gauge must read 2 (LockedOut); got:\n{text}"
    );
    // The denied read above is visible on the labeled denial counter.
    assert!(
        text.lines()
            .any(|l| l.starts_with("kirra_gate_denials_total{")
                && l.contains("reason=\"locked_out\"")
                && l.ends_with(" 1")),
        "the LockedOut denial must be counted on the labeled series; got:\n{text}"
    );
}

/// Exemption wiring on the real assembly: `/health` stays reachable under
/// LockedOut (liveness is allowlisted by `is_posture_exempt`).
#[tokio::test]
async fn health_exempt_under_lockedout_on_real_router() {
    let status =
        status_through_real_app(state_with(FleetPosture::LockedOut), "GET", "/health").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/health must remain reachable under LockedOut on the real router (exempt); got {status}"
    );
}

/// Part 3 (#891 narration): the last-verdict sidecar is mounted in the
/// auditor tier on the REAL assembled router — an unauthenticated GET is
/// refused (the route exists but the scope gate holds), proving the
/// narration surface is read-only telemetry behind RBAC, not an open (or
/// admin-token) endpoint.
#[tokio::test]
async fn last_verdict_sidecar_is_auditor_gated_on_the_real_router() {
    let svc = state_with(FleetPosture::Nominal);
    let status = status_through_real_app(svc, "GET", "/system/verdicts/last").await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::SERVICE_UNAVAILABLE,
        "unauthenticated access to the narration sidecar must be refused (got {status})"
    );
}
