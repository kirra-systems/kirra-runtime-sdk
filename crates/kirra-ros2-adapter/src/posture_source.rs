// crates/kirra-ros2-adapter/src/posture_source.rs
//
// M1b — SSE transport: subscribes to the verifier's
// `/system/posture/stream` endpoint, parses each event into a
// `FleetPosture` value, and pumps it into the `AdaptorState`'s
// `PostureTracker`.
//
// The transport is feature-gated on `ros2` because the adapter binary
// is the only consumer and the binary itself is `ros2`-gated. The
// state-machine logic that this transport drives
// (`crate::posture_tracker`) is feature-independent and unit-tested on
// stable.
//
// **SSE event semantics** (mirrors `src/bin/kirra_verifier_service.rs`
// + `src/posture_engine.rs`):
//
//   The verifier emits two kinds of `PostureStreamEvent`s:
//
//   1. Per-node updates with `node_id: Some(...)` and
//      `posture: Some(FleetNodePosture)`, carrying the per-node
//      propagated status.
//   2. Fleet-level transitions with `node_id: None`, `posture: None`,
//      and `event_type` ∈ {SYSTEM_POSTURE_TRANSITION,
//      POSTURE_CACHE_REFRESHED}. These announce a recomputation but do
//      NOT carry the new posture value directly.
//
// To get the fleet-wide `FleetPosture` reliably, the adapter:
//   - Uses the SSE stream as a *wake-up signal* for either event kind.
//   - On every event, issues an HTTP GET to `/fleet/posture` and reads
//     the aggregate. The endpoint returns `{"fleet": [FleetNodePosture, …]}`;
//     the adapter folds the per-node `propagated_status` field with
//     **worst-of** (LockedOut > Degraded > Nominal) to get the fleet
//     posture.
//
// Reconnect with exponential backoff (1s → 2s → 4s → 8s, capped at 30s).
// The `PostureTracker`'s staleness-derate path handles the disconnect
// window automatically: while the transport is reconnecting, no new
// `observe` lands, and `current_posture` returns `Degraded` after the
// staleness window expires — fail-closed.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use kirra_core::FleetPosture;
use serde_json::Value;

use crate::state::AdaptorState;

/// Initial reconnect delay (ms). Each successive failure doubles up to
/// `RECONNECT_MAX_BACKOFF_MS`.
const RECONNECT_BASE_BACKOFF_MS: u64 = 1_000;
const RECONNECT_MAX_BACKOFF_MS: u64 = 30_000;

/// HTTP timeout for the fleet-posture GET. Tight enough that a slow
/// verifier doesn't stall the SSE reader for long; loose enough to
/// absorb routine latency.
const FLEET_POSTURE_GET_TIMEOUT_MS: u64 = 1_000;

/// SSE keep-alive expectation. The verifier uses
/// `axum::response::sse::KeepAlive::default()` (15 s); we send a
/// reasonable client-side read timeout so a fully dead connection is
/// detected within a posture-cycle window.
const SSE_READ_TIMEOUT_MS: u64 = 30_000;

/// Configuration for the SSE posture-source subscriber. Built from the
/// adapter's environment / CLI; passed to `spawn_posture_source`.
#[derive(Debug, Clone)]
pub struct PostureSourceConfig {
    /// Verifier base URL (e.g. `http://kirra-verifier:8090`).
    pub verifier_base_url: String,
    /// Admin bearer token for the SSE + GET endpoints. Sourced from
    /// `KIRRA_ADMIN_TOKEN` per the verifier's auth contract; never
    /// hardcoded.
    pub admin_token: String,
    /// Client identity for the `x-kirra-client-id` header (identity-
    /// gated routes per the verifier's `TransportIdentityConfig`).
    pub client_id: String,
}

/// Spawn the SSE subscriber task. Returns the `tokio::task::JoinHandle`
/// so the binary can abort it on shutdown.
///
// SAFETY: SG8 SG9 | REQ: posture-source-fail-closed-transport | TEST: docs/safety/OCCY_131_OPTIONB_DESIGN.md§8b (integration; transport runs only with ros2 feature + a live verifier)
pub fn spawn_posture_source(
    state: Arc<AdaptorState>,
    config: PostureSourceConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff_ms = RECONNECT_BASE_BACKOFF_MS;

        let client = match reqwest::Client::builder()
            .read_timeout(Duration::from_millis(SSE_READ_TIMEOUT_MS))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e,
                    "posture-source: failed to build reqwest client; \
                     the staleness watchdog will derate to Degraded");
                return;
            }
        };

        loop {
            match connect_and_pump(&client, &state, &config).await {
                Ok(()) => {
                    tracing::warn!("posture-source: SSE stream closed cleanly; reconnecting");
                    backoff_ms = RECONNECT_BASE_BACKOFF_MS;
                }
                Err(e) => {
                    tracing::error!(error = %e, backoff_ms,
                        "posture-source: SSE stream errored; \
                         tracker staleness will derate to Degraded");
                }
            }
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_BACKOFF_MS);
        }
    })
}

/// One connect → pump cycle. Returns `Ok(())` on a clean stream close,
/// `Err(...)` on a transport / HTTP failure. The caller (re)connects
/// either way; the only difference is the log level.
async fn connect_and_pump(
    client: &reqwest::Client,
    state: &Arc<AdaptorState>,
    config: &PostureSourceConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "{}/system/posture/stream",
        config.verifier_base_url.trim_end_matches('/')
    );
    let response = client
        .get(&url)
        .bearer_auth(&config.admin_token)
        .header("x-kirra-client-id", &config.client_id)
        .header("Accept", "text/event-stream")
        .send()
        .await?
        .error_for_status()?;

    tracing::info!(url = %url, "posture-source: SSE stream connected");

    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(4 * 1024);
    while let Some(chunk) = stream.next().await {
        let chunk: Bytes = chunk?;
        buf.extend_from_slice(&chunk);
        // SSE framing: events terminate on blank line. We don't need to
        // parse the event body — any event is a wake-up trigger; we
        // re-fetch the aggregate fleet posture via HTTP GET.
        while let Some(pos) = find_event_terminator(&buf) {
            buf.drain(..pos);
            // Re-fetch the aggregate fleet posture. If the GET fails,
            // log + continue — the staleness watchdog will derate.
            match fetch_fleet_posture(client, config).await {
                Ok(posture) => {
                    state.update_posture(posture.clone());
                    tracing::debug!(?posture, "posture-source: fleet posture refreshed");
                }
                Err(e) => {
                    tracing::warn!(error = %e,
                        "posture-source: GET /fleet/posture failed after SSE event");
                }
            }
        }
        // Cap the buffer growth between event boundaries to bound memory.
        if buf.len() > 1024 * 1024 {
            buf.clear();
        }
    }
    Ok(())
}

/// Find the index *just past* the first SSE event terminator
/// (`\n\n` or `\r\n\r\n`) in `buf`. Returns `None` if no full event
/// has arrived yet.
fn find_event_terminator(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
    }
    None
}

/// HTTP GET `/fleet/posture` and fold the per-node `propagated_status`
/// values with **worst-of** semantics (LockedOut > Degraded > Nominal).
async fn fetch_fleet_posture(
    client: &reqwest::Client,
    config: &PostureSourceConfig,
) -> Result<FleetPosture, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "{}/fleet/posture",
        config.verifier_base_url.trim_end_matches('/')
    );
    let response = tokio::time::timeout(
        Duration::from_millis(FLEET_POSTURE_GET_TIMEOUT_MS),
        client.get(&url).send(),
    )
    .await
    .map_err(|_| "GET /fleet/posture timed out")??
    .error_for_status()?;

    let body: Value = response.json().await?;
    Ok(fold_fleet_posture(&body))
}

/// Pure folding logic: take the verifier's `/fleet/posture` JSON
/// response and return the worst-of `propagated_status` across nodes.
/// Empty fleet → `Nominal` (the verifier's own seed behaviour).
///
/// Exposed (`pub(crate)`) for direct unit testing.
pub(crate) fn fold_fleet_posture(body: &Value) -> FleetPosture {
    let Some(fleet) = body.get("fleet").and_then(|v| v.as_array()) else {
        // Unexpected response shape → fail-closed.
        return FleetPosture::Degraded;
    };
    if fleet.is_empty() {
        return FleetPosture::Nominal;
    }
    let mut worst = FleetPosture::Nominal;
    for node in fleet {
        let status = node
            .get("propagated_status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let p = match status {
            "LockedOut" => FleetPosture::LockedOut,
            "Degraded" => FleetPosture::Degraded,
            "Nominal" => FleetPosture::Nominal,
            // Unknown variant string → fail-closed.
            _ => FleetPosture::Degraded,
        };
        worst = worst_of(worst, p);
    }
    worst
}

fn worst_of(a: FleetPosture, b: FleetPosture) -> FleetPosture {
    let rank = |p: &FleetPosture| match p {
        FleetPosture::Nominal => 0,
        FleetPosture::Degraded => 1,
        FleetPosture::LockedOut => 2,
    };
    if rank(&a) >= rank(&b) {
        a
    } else {
        b
    }
}

// ---------------------------------------------------------------------------
// Pure-logic tests (transport-independent — these run on stable)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fold_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_fleet_folds_to_nominal() {
        let body = json!({"fleet": []});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::Nominal);
    }

    #[test]
    fn missing_fleet_field_fails_closed_to_degraded() {
        let body = json!({"unexpected": "shape"});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::Degraded);
    }

    #[test]
    fn worst_of_locked_out_beats_others() {
        let body = json!({"fleet": [
            {"node_id": "a", "propagated_status": "Nominal"},
            {"node_id": "b", "propagated_status": "Degraded"},
            {"node_id": "c", "propagated_status": "LockedOut"},
        ]});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::LockedOut);
    }

    #[test]
    fn worst_of_degraded_beats_nominal() {
        let body = json!({"fleet": [
            {"node_id": "a", "propagated_status": "Nominal"},
            {"node_id": "b", "propagated_status": "Degraded"},
        ]});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::Degraded);
    }

    #[test]
    fn all_nominal_folds_to_nominal() {
        let body = json!({"fleet": [
            {"node_id": "a", "propagated_status": "Nominal"},
            {"node_id": "b", "propagated_status": "Nominal"},
        ]});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::Nominal);
    }

    #[test]
    fn unknown_status_string_fails_closed_to_degraded() {
        let body = json!({"fleet": [
            {"node_id": "a", "propagated_status": "UnknownVariant"},
        ]});
        assert_eq!(fold_fleet_posture(&body), FleetPosture::Degraded);
    }

    #[test]
    fn sse_terminator_lf_lf_detected() {
        let buf = b"data: foo\n\nremaining";
        assert_eq!(find_event_terminator(buf), Some(11));
    }

    #[test]
    fn sse_terminator_crlf_crlf_detected() {
        let buf = b"data: foo\r\n\r\nremaining";
        assert_eq!(find_event_terminator(buf), Some(13));
    }

    #[test]
    fn sse_terminator_partial_event_returns_none() {
        let buf = b"data: foo\n";
        assert!(find_event_terminator(buf).is_none());
    }
}
