// src/bin/kirra_verifier_service/campaigns.rs
// OTA governor-artifact campaign handlers (WS-4 · Track 3 · Fleet Plane).
//
// `use super::*` pulls the binary root's helpers, DTOs and imports (`ServiceState`,
// `now_ms`, `gate_posture`, `valid_identifier`, the axum extractors, `json!`).
// These routes are ADMIN-scoped at the router layer (`require_admin_token` /
// SCOPE_ADMIN) — only an admin authors, arms, advances or halts a rollout.
//
// The control-plane state machine + the fail-closed halt-on-regression rule live
// in `kirra_verifier::ota_campaign`; persistence + the R156 audit append live in
// `verifier_store::ota_campaigns`. Each mutating handler does the whole
// read-modify-write INSIDE one `store.call` closure, so the writer mutex is held
// across load→transition→persist and two concurrent advances cannot race.
//
// The advance path reads the fleet posture via `gate_posture` (fail-closed: a
// stale/empty/poisoned cache resolves to `LockedOut`), so a rollout can only
// proceed while the fleet is genuinely `Nominal`; anything else HALTS it.

use super::*;

use kirra_verifier::ota_campaign::{AdvanceOutcome, Campaign, HaltReason};

#[derive(Deserialize)]
pub(crate) struct CreateCampaignRequest {
    campaign_id: String,
    /// 64-char lowercase hex SHA-256 of the cosign-signed governor artifact.
    artifact_digest: String,
    artifact_version: String,
    /// Target cohort labels; must be non-empty.
    cohorts: Vec<String>,
    /// Strictly-increasing rollout percentages within `1..=100`, ending at `100`.
    stages: Vec<u8>,
    /// WP-12 — the governor artifact-release signature over `artifact_digest`
    /// (base64 Ed25519, `kirra_release_token::artifact_release` domain).
    /// Optional at create time (legacy campaigns); a key-provisioned node
    /// refuses to stage an assignment without it.
    #[serde(default)]
    artifact_signature_b64: Option<String>,
}

/// The lifecycle-operation result surfaced from a `store.call` closure back to the
/// handler, which maps each variant to an HTTP status. Keeps the closure free of
/// axum types (it runs on the blocking pool).
enum CampaignOpError {
    /// No campaign with that id.
    NotFound,
    /// An illegal transition for the current state (e.g. advancing a terminal
    /// campaign) — maps to 409.
    InvalidTransition(String),
    /// A persistence failure — maps to 500.
    Store(String),
}

/// POST /system/campaigns — author a new campaign in `Draft`. ADMIN-scoped.
/// Validates the artifact digest, cohorts and stage schedule fail-closed before
/// persisting. A duplicate `campaign_id` is a 409.
pub(crate) async fn create_campaign_handler(
    State(svc): State<Arc<ServiceState>>,
    Json(req): Json<CreateCampaignRequest>,
) -> impl IntoResponse {
    let campaign_id = req.campaign_id.trim().to_string();
    // Same identifier hygiene as the principal/operator registries (#326): the id
    // is echoed into audit payloads, so no `|` / control characters.
    if !valid_identifier(&campaign_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "campaign_id must be non-empty and free of '|' or control characters"
            })),
        )
            .into_response();
    }
    let now = now_ms();
    let campaign = match Campaign::new(
        campaign_id,
        req.artifact_digest.trim(),
        req.artifact_version.trim(),
        req.cohorts,
        req.stages,
        now,
    ) {
        Ok(c) => match req.artifact_signature_b64.as_deref().map(str::trim) {
            Some(sig) if !sig.is_empty() => c.with_artifact_signature(sig),
            _ => c,
        },
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let persisted = svc
        .app
        .store
        .call(move |store| match store.insert_campaign(&campaign) {
            Ok(()) => Ok(campaign),
            Err(e) if is_unique_violation(&e) => Err(CampaignOpError::InvalidTransition(
                "campaign_id already exists".into(),
            )),
            Err(e) => Err(CampaignOpError::Store(e.to_string())),
        })
        .await;

    match persisted {
        Ok(Ok(c)) => (StatusCode::CREATED, Json(campaign_json(&c))).into_response(),
        Ok(Err(CampaignOpError::InvalidTransition(msg))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Err(CampaignOpError::Store(detail))) => {
            tracing::error!(error = %detail, "OTA campaign create — persist failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "persist failed" })),
            )
                .into_response()
        }
        // NotFound is impossible on create; fold into 500 defensively.
        Ok(Err(CampaignOpError::NotFound)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "persist failed" })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store task failed" })),
        )
            .into_response(),
    }
}

/// GET /system/campaigns — list every campaign, newest first. ADMIN-scoped.
pub(crate) async fn list_campaigns_handler(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    match svc
        .app
        .store
        .call_read(|store| store.load_campaigns())
        .await
    {
        Ok(Ok(list)) => {
            let out: Vec<_> = list.iter().map(campaign_json).collect();
            (StatusCode::OK, Json(json!({ "campaigns": out }))).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "query failed" })),
        )
            .into_response(),
    }
}

/// GET /system/campaigns/summary — fleet-wide rollout observability. ADMIN-scoped.
/// An at-a-glance view: campaign counts by state, active-campaign stage progress, and
/// halted campaigns WITH their reason (so an operator sees a fail-closed auto-halt).
/// Read-only over the campaign records the verifier authoritatively owns — off the
/// read replica, so it never contends the writer. Registered BEFORE the
/// `{campaign_id}` route so the static segment wins the match.
pub(crate) async fn campaigns_summary_handler(
    State(svc): State<Arc<ServiceState>>,
) -> impl IntoResponse {
    match svc
        .app
        .store
        .call_read(|store| {
            // One replica read for both the campaigns and the node adoption reports;
            // the pure `summarize_campaigns` joins them (adoption numerator per digest).
            let campaigns = store.load_campaigns()?;
            let statuses = store.load_node_artifact_statuses()?;
            Ok::<_, rusqlite::Error>((campaigns, statuses))
        })
        .await
    {
        Ok(Ok((campaigns, statuses))) => {
            let summary =
                kirra_verifier::ota_campaign::summarize_campaigns(&campaigns, &statuses);
            (StatusCode::OK, Json(summary)).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "query failed" })),
        )
            .into_response(),
    }
}

/// GET /system/campaigns/{campaign_id} — fetch one campaign. ADMIN-scoped.
pub(crate) async fn get_campaign_handler(
    State(svc): State<Arc<ServiceState>>,
    Path(campaign_id): Path<String>,
) -> impl IntoResponse {
    match svc
        .app
        .store
        .call_read(move |store| store.load_campaign(&campaign_id))
        .await
    {
        Ok(Ok(Some(c))) => (StatusCode::OK, Json(campaign_json(&c))).into_response(),
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such campaign" })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "query failed" })),
        )
            .into_response(),
    }
}

/// POST /system/campaigns/{campaign_id}/arm — `Draft → Staged`. ADMIN-scoped.
pub(crate) async fn arm_campaign_handler(
    State(svc): State<Arc<ServiceState>>,
    Path(campaign_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    let op = svc
        .app
        .store
        .call(move |store| {
            let mut c = load_for_mutation(store, &campaign_id)?;
            c.arm(now)
                .map_err(|e| CampaignOpError::InvalidTransition(e.to_string()))?;
            store
                .update_campaign(&c, "OtaCampaignArmed")
                .map_err(|e| CampaignOpError::Store(e.to_string()))?;
            Ok(c)
        })
        .await;
    respond_op(op, |c| json!({ "campaign": campaign_json(&c) }))
}

/// POST /system/campaigns/{campaign_id}/advance — advance one rollout stage,
/// fail-closed on posture. ADMIN-scoped. The observed fleet posture is resolved
/// via `gate_posture` (stale/empty/poisoned → `LockedOut`); a non-`Nominal`
/// posture HALTS the campaign instead of advancing it.
pub(crate) async fn advance_campaign_handler(
    State(svc): State<Arc<ServiceState>>,
    Path(campaign_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    // Fail-closed posture read (LockedOut on any staleness/poison). Copy into the
    // blocking closure.
    let (posture, _reason) = gate_posture(&svc);

    let op = svc
        .app
        .store
        .call(move |store| {
            let mut c = load_for_mutation(store, &campaign_id)?;
            let outcome = c
                .advance(posture, now)
                .map_err(|e| CampaignOpError::InvalidTransition(e.to_string()))?;
            let event_type = match outcome {
                AdvanceOutcome::Advanced { .. } => "OtaCampaignAdvanced",
                AdvanceOutcome::Completed => "OtaCampaignCompleted",
                AdvanceOutcome::Halted { .. } => "OtaCampaignHalted",
            };
            store
                .update_campaign(&c, event_type)
                .map_err(|e| CampaignOpError::Store(e.to_string()))?;
            Ok((c, outcome))
        })
        .await;

    match op {
        Ok(Ok((c, outcome))) => {
            let outcome_str = match outcome {
                AdvanceOutcome::Advanced { rollout_percent } => {
                    json!({ "advanced": true, "rollout_percent": rollout_percent })
                }
                AdvanceOutcome::Completed => json!({ "completed": true }),
                AdvanceOutcome::Halted { reason } => {
                    json!({ "halted": true, "halt_reason": reason.as_str() })
                }
            };
            (
                StatusCode::OK,
                Json(json!({
                    "outcome": outcome_str,
                    "campaign": campaign_json(&c),
                })),
            )
                .into_response()
        }
        other => respond_op(
            other.map(|r| r.map(|(c, _)| c)),
            |c| json!({ "campaign": campaign_json(&c) }),
        ),
    }
}

/// POST /system/campaigns/{campaign_id}/halt — operator-commanded halt. ADMIN-scoped.
/// Terminal; the engine authors no resume.
pub(crate) async fn halt_campaign_handler(
    State(svc): State<Arc<ServiceState>>,
    Path(campaign_id): Path<String>,
) -> impl IntoResponse {
    let now = now_ms();
    let op = svc
        .app
        .store
        .call(move |store| {
            let mut c = load_for_mutation(store, &campaign_id)?;
            c.halt(HaltReason::OperatorHalt, now)
                .map_err(|e| CampaignOpError::InvalidTransition(e.to_string()))?;
            store
                .update_campaign(&c, "OtaCampaignHalted")
                .map_err(|e| CampaignOpError::Store(e.to_string()))?;
            Ok(c)
        })
        .await;
    respond_op(op, |c| json!({ "campaign": campaign_json(&c) }))
}

/// Load a campaign for an in-closure mutation, mapping absence to `NotFound` and a
/// store error to `Store`.
fn load_for_mutation(
    store: &kirra_verifier::verifier_store::VerifierStore,
    campaign_id: &str,
) -> Result<Campaign, CampaignOpError> {
    match store.load_campaign(campaign_id) {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(CampaignOpError::NotFound),
        Err(e) => Err(CampaignOpError::Store(e.to_string())),
    }
}

/// Map a `store.call` lifecycle result to an HTTP response. `ok` renders the
/// success body from the resulting campaign.
fn respond_op(
    op: Result<Result<Campaign, CampaignOpError>, kirra_verifier::store_handle::StoreError>,
    ok: impl FnOnce(Campaign) -> serde_json::Value,
) -> axum::response::Response {
    match op {
        Ok(Ok(c)) => (StatusCode::OK, Json(ok(c))).into_response(),
        Ok(Err(CampaignOpError::NotFound)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such campaign" })),
        )
            .into_response(),
        Ok(Err(CampaignOpError::InvalidTransition(msg))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Err(CampaignOpError::Store(detail))) => {
            tracing::error!(error = %detail, "OTA campaign mutation — persist failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "persist failed" })),
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store task failed" })),
        )
            .into_response(),
    }
}

/// The public JSON view of a campaign (no secrets — the artifact digest is public).
fn campaign_json(c: &Campaign) -> serde_json::Value {
    json!({
        "campaign_id": c.campaign_id,
        "artifact_digest": c.artifact_digest,
        "artifact_signature_b64": c.artifact_signature_b64,
        "artifact_version": c.artifact_version,
        "cohorts": c.cohorts,
        "stages": c.stages,
        "stage_index": c.stage_index,
        "rollout_percent": c.rollout_percent,
        "state": c.state.as_str(),
        "halt_reason": c.halt_reason.map(|r| r.as_str()),
        "created_at_ms": c.created_at_ms,
        "updated_at_ms": c.updated_at_ms,
    })
}

/// Constraint-violation classifier (duplicate `campaign_id` PRIMARY KEY) → 409.
/// Mirrors the `principals` module helper (kept module-local — the sibling's is
/// private).
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}
