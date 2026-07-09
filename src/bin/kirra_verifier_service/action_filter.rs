// src/bin/kirra_verifier_service/action_filter.rs
// action_filter route handlers — split from kirra_verifier_service.rs (pure move).
//
// `use super::*` pulls the binary root's DTOs, helpers and `use` imports
// (visible to this descendant module); handlers are `pub(crate)` so the
// root re-export (`use action_filter::*`) lets build_app/tests name them unqualified.

use super::*;

pub(crate) async fn evaluate_action_filter(
    State(svc): State<Arc<ServiceState>>,
    body: Result<Json<ActionClaim>, JsonRejection>,
) -> impl IntoResponse {
    let claim = match body {
        Ok(Json(c)) => c,
        Err(rejection) => {
            // P1: durable audit write off the worker pool. Materialize the payload
            // first (rejection is reused for the response detail below).
            let now = now_ms();
            let payload = json!({ "error": rejection.body_text() }).to_string();
            let _ = svc
                .app
                .store
                .call(move |store| {
                    let _ = store.save_posture_event_chained(
                        "action_filter",
                        "ACTION_FILTER_MALFORMED_REQUEST",
                        &payload,
                        Some("malformed request body"),
                        now,
                    );
                })
                .await;
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "MALFORMED_REQUEST",
                    "detail": rejection.body_text(),
                    "allowed": false,
                })),
            )
                .into_response();
        }
    };

    let request_id = now_ms().to_string();

    let (posture, lockout_reason) = gate_posture(&svc);

    let posture_str = format!("{:?}", posture);
    let decision = evaluate_action_claim(claim.clone(), posture);

    let audit_event_type = if !decision.allowed {
        if decision.reason == "UNKNOWN_ACTION_TYPE" {
            "ACTION_FILTER_UNKNOWN_TYPE"
        } else {
            "ACTION_FILTER_DENIED"
        }
    } else {
        "ACTION_FILTER_ALLOWED"
    };

    let event = json!({
        "request_id": request_id,
        "target_node": claim.target_node,
        "action_type": claim.action_type,
        "risk_class": claim.risk_class,
        "allowed": decision.allowed,
        "reason": decision.reason,
        "posture": posture_str,
        "lockout_reason": lockout_reason.as_ref().map(|r| r.to_string()),
    });
    // P1: durable audit write off the worker pool. `audit_event_type` is
    // `&'static str` and the event string is owned, so both move into the closure.
    let now = now_ms();
    let event_str = event.to_string();
    let _ = svc
        .app
        .store
        .call(move |store| {
            let _ = store.save_posture_event_chained(
                "action_filter",
                audit_event_type,
                &event_str,
                None,
                now,
            );
        })
        .await;

    tracing::info!(
        action_type = %claim.action_type,
        target_node = %claim.target_node,
        allowed = decision.allowed,
        posture = %posture_str,
        reason = %decision.reason,
        request_id = %request_id,
        "action_filter evaluated"
    );

    Json(json!({
        "allowed": decision.allowed,
        "reason": decision.reason,
        "posture_at_evaluation": posture_str,
        "request_id": request_id,
    }))
    .into_response()
}
