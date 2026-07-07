// src/bin/kirra_verifier_service.rs
// Kirra Verifier Service — distributed legitimacy fabric entry point.

use axum::{
    extract::{Path, Query, Request, State},
    extract::rejection::JsonRejection,
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{sse::{Event, KeepAlive, Sse}, Html, IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use tower_http::cors::{CorsLayer, Any};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use kirra_verifier::verifier::{
    request_transport_is_secure, validate_client_identity_headers, AppState, BackupExport, FlapStatus,
    FleetPosture, HealthResponse, NodeTrustState, PostureStreamEvent, RegisteredNode, VerifierOperationMode,
};
use kirra_verifier::verifier_store::{DurableWriteError, VerifierStore};
use kirra_verifier::posture_cache::{now_ms, ServiceState, POSTURE_CACHE_TTL_MS};
use kirra_verifier::posture_engine_v2::{
    resolve_posture_snapshot_silent, resolve_posture_with_reason, LockoutReason,
};
use kirra_verifier::security::{admin_token_ok, constant_time_compare};
use kirra_verifier::authz::{
    authorize_request, generate_api_token, token_fingerprint, token_sha256_hex, ApiRole,
    AuthzOutcome, ResolvedPrincipal, SCOPE_ACTUATOR_COMMAND, SCOPE_ADMIN, SCOPE_AUDIT_READ,
    SCOPE_INTEGRATION_EVALUATE,
};
use kirra_verifier::action_filter::{evaluate_action_claim, ActionClaim};
use kirra_verifier::protocol_adapter::{
    evaluate_unified_industrial_request, UnifiedIndustrialRequest,
};
use kirra_verifier::adapters::ethernet_ip::{EtherNetIpAdapter, EtherNetIpMessage};
use kirra_verifier::adapters::canopen::{CanOpenAdapter, CanOpenMessage};
use kirra_verifier::adapters::dnp3::{Dnp3Adapter, Dnp3Message};
use kirra_verifier::federation::{
    RegisterFederationControllerRequest,
    ReportEvaluation,
};
use kirra_verifier::federation_reconciliation::{
    authoritative_posture, dissenting_restriction, evaluate_federated_report_v2,
    verify_federated_report_signature_v2, FederatedTrustReportV2,
};
use kirra_verifier::standby_monitor::{
    instance_id as ha_instance_id, spawn_heartbeat_writer, spawn_promotion_monitor,
    HEARTBEAT_KEY, PROMOTION_TIMEOUT_MS,
};
use kirra_verifier::gateway::kinematics_contract::ProposedVehicleCommand;
use kirra_verifier::gateway::policy_layer::{
    enforce_actuator_safety_envelope, enforce_posture_routing, EnforcementOutcome,
};
use kirra_verifier::recovery_hysteresis::{evaluate_recovery_report, HysteresisDecision};
use kirra_verifier::fabric::asset::{AssetPosture, AssetType, FabricAsset, KinematicProfileType};
use kirra_verifier::fabric::router::FabricRouter;
use kirra_verifier::fabric::telemetry::FabricTelemetry;
use kirra_verifier::fabric::causal_log::FabricCausalLog;

// Route handlers, split by domain into sibling submodules. Each holds
// `pub(crate)` handler fns that share the binary's helpers, DTOs and `use`
// imports via `use super::*` (descendant-module visibility). Re-exported
// below so `build_app` and the in-file tests reference them unqualified.
#[path = "kirra_verifier_service/attestation.rs"]
mod attestation;
#[path = "kirra_verifier_service/fleet.rs"]
mod fleet;
#[path = "kirra_verifier_service/audit.rs"]
mod audit;
#[path = "kirra_verifier_service/action_filter.rs"]
mod action_filter;
#[path = "kirra_verifier_service/industrial.rs"]
mod industrial;
#[path = "kirra_verifier_service/federation.rs"]
mod federation;
#[path = "kirra_verifier_service/actuator.rs"]
mod actuator;
#[path = "kirra_verifier_service/fabric.rs"]
mod fabric;
#[path = "kirra_verifier_service/console.rs"]
mod console;
#[path = "kirra_verifier_service/operators.rs"]
mod operators;
#[path = "kirra_verifier_service/principals.rs"]
mod principals;
// WS-4 / Track 3 (Fleet Plane) — OTA governor-artifact campaign handlers
// (create / list / get / arm / advance / halt). ADMIN-scoped at the router layer.
#[path = "kirra_verifier_service/campaigns.rs"]
mod campaigns;
// SG-008 (ASIL D) startup sentinel — pure invariant predicate + its CERT-003
// RTM coverage tests; extracted from this file to keep the entry point lean.
#[path = "kirra_verifier_service/startup.rs"]
mod startup;
// Auth middleware (admin token + RBAC, transport identity/security, admin-action
// attribution) — the CRITICAL auth path, extracted to keep the entry point lean.
#[path = "kirra_verifier_service/auth.rs"]
mod auth;
// Opt-in in-process TLS termination (WS-1 Track 1.2). Default OFF → plaintext
// serve path unchanged; fail-closed on partial config; ring provider only.
#[path = "kirra_verifier_service/tls.rs"]
mod tls;
// WP-03 (MGA G-10) — control-plane backpressure (load-shed + shared
// concurrency pools + body cap); wiring in `build_app`, semantics + tests in
// the module.
#[path = "kirra_verifier_service/backpressure.rs"]
mod backpressure;
use backpressure::{env_limit_or, with_backpressure};
// WP-05 (MGA G-10) — request observability: correlation id + tracing span +
// end-to-end latency histogram. Mounted outermost in `build_app`; makes no
// admission decisions.
#[path = "kirra_verifier_service/observability.rs"]
mod observability;
use observability::request_observability;
use attestation::*;
use fleet::*;
use audit::*;
use action_filter::*;
use industrial::*;
use federation::*;
use actuator::*;
use fabric::*;
use console::*;
use operators::*;
use principals::*;
use campaigns::*;
use startup::*;
use auth::*;


// --- Auth middleware ---------------------------------------------------------
//
// The auth middleware — admin-token auth + RBAC (`require_admin_token`),
// admin-action attribution (`record_admin_action_audit`), transport identity
// (`require_client_identity`) and transport security (`require_secure_transport`)
// — is the CRITICAL auth path; it lives in the `auth` submodule (re-exported
// above via `use auth::*`), extracted to keep this entry point lean. CRITICAL
// INVARIANTS #1/#2/#6/#13 are enforced there.
//
// SG-008 (ASIL D) process fail-closed startup sentinel — `StartupContext`,
// `StartupInvariant`, and `check_startup_invariants` — lives in the `startup`
// submodule (re-exported above). `main` builds a `StartupContext` from the real
// boot facts and aborts BEFORE `TcpListener::bind` on any `Err`.

// --- Real-time posture stream -----------------------------------------------

/// Sends an event-driven posture recalc trigger to the worker if the
/// `posture_engine_tx` is initialized (Active path). On PassiveStandby the
/// OnceLock is unset and this is a no-op — correct, since a standby does
/// not maintain a posture cache. A `try_send` failure (channel full or
/// worker gone) is logged; the periodic-refresh loop will fail-close the
/// cache and gate on its own if the worker has truly died.
fn enqueue_recalc(svc: &ServiceState, trigger: kirra_verifier::posture_engine_v2::PostureRecalcTrigger) {
    if let Some(tx) = svc.posture_engine_tx.get() {
        if let Err(e) = tx.try_send(trigger) {
            tracing::warn!(error = %e,
                "posture recalc trigger: try_send failed (channel full or worker gone)");
        }
    }
}

/// Fail-closed posture read for action/actuator gating sites.
///
/// Delegates to `resolve_posture_with_reason` so the cache-staleness check
/// (age >= POSTURE_CACHE_TTL_MS), empty-cache check, and poisoned-lock check
/// all collapse into the same `(FleetPosture::LockedOut, Some(LockoutReason))`
/// answer — never serving a stale entry as if current. The returned
/// `LockoutReason` is threaded into the denial-audit payload so operators
/// can distinguish a DAG-derived LockedOut from a posture-cache-derived one.
fn gate_posture(svc: &ServiceState) -> (FleetPosture, Option<LockoutReason>) {
    resolve_posture_with_reason(&svc.posture_cache, POSTURE_CACHE_TTL_MS)
}

/// Verifier→fabric posture feed (#88, single-local-asset model).
///
/// Mirrors THIS controller's aggregate `FleetPosture` into the fabric
/// posture of the one locally governed asset named by `KIRRA_FABRIC_ASSET_ID`,
/// so fabric command enforcement for that asset reflects real verified trust
/// rather than the interim `Degraded` registration seed.
///
/// Seam: `recalculate_and_broadcast` lives in the lib and cannot see the
/// `FabricRouter` (it is on `ServiceState`, here in the binary). The posture
/// broadcast (`app.posture_tx`) fires on every fleet-posture transition,
/// including those produced by the lib-side posture-engine worker, so a
/// broadcast subscriber catches all transitions from one place.
///
/// Inert (logs once, no task spawned) when `KIRRA_FABRIC_ASSET_ID` is
/// unset/empty: the asset then keeps its registration seed. This is the
/// single-asset model — other registered assets are intentionally NOT fed
/// here, which is why the registration seed stays `Degraded` rather than
/// `LockedOut` (an unfed asset must not be bricked).
fn spawn_local_asset_posture_feed(svc: Arc<ServiceState>) {
    let asset_id = match std::env::var("KIRRA_FABRIC_ASSET_ID") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            tracing::info!(
                "verifier→fabric posture feed: KIRRA_FABRIC_ASSET_ID unset — \
                 feed inert (local fabric asset keeps its registration seed)"
            );
            return;
        }
    };

    tokio::spawn(async move {
        // Subscribe BEFORE the initial sync so a transition occurring in the
        // window between the initial cache read and entering recv() is
        // buffered by the broadcast channel rather than lost.
        let mut rx = svc.app.posture_tx.subscribe();
        tracing::info!(
            asset_id = %asset_id,
            "verifier→fabric posture feed: started (single-local-asset model)"
        );

        // Initial sync: the synchronous startup recalc already populated the
        // cache before this task subscribed, so reflect it once now.
        sync_local_asset_posture(&svc, &asset_id);

        loop {
            match rx.recv().await {
                Ok(_event) => sync_local_asset_posture(&svc, &asset_id),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // A lag only means we may have missed a transition; the
                    // cache is authoritative, so re-sync from it.
                    tracing::warn!(
                        skipped = n,
                        "verifier→fabric posture feed lagged; re-syncing from cache"
                    );
                    sync_local_asset_posture(&svc, &asset_id);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::warn!(
                        "verifier→fabric posture feed: broadcast channel closed; feed stopping"
                    );
                    break;
                }
            }
        }
    });
}

/// Wire the Active-mode posture-freshness background tasks onto `svc`: the
/// serialized posture-engine worker, the telemetry watchdog (SG9 sensor-
/// liveness), the periodic recompute-and-restamp loop, and the verifier→fabric
/// local-asset posture feed (#88).
///
/// Shared by TWO entry points (review H2): the Active startup path AND the
/// standby→Active promotion path. The bug this closes: a node promoted from
/// standby at runtime used to (re)start only the heartbeat writer, never these
/// four tasks — so `POSTURE_CACHE_TTL_MS` after promotion the cache went stale
/// and every gated route fail-closed (503) until process restart, negating the
/// HA availability guarantee. Calling this from the promotion path keeps the
/// freshly-promoted primary serving.
///
/// Does NOT perform the initial `recalculate_and_broadcast` — that is the
/// caller's job (startup runs it synchronously before `axum::serve`;
/// `perform_promotion` runs it as part of the promotion sequence, so the cache
/// is already populated before this wiring starts the ongoing-freshness tasks).
///
/// Must be called inside a tokio runtime context (it spawns tasks). Fail-closed:
/// a double-set of `posture_engine_tx` is an invariant breach — the cell must be
/// empty on both the pre-serve Active path and a never-Active promoted standby —
/// and aborts the process (a half-wired node is safer dead: another standby /
/// systemd restart re-promotes cleanly).
fn wire_active_posture_freshness(svc: &Arc<ServiceState>) {
    let posture_tx = kirra_verifier::posture_engine_v2::start_posture_engine_worker(
        Arc::clone(&svc.app),
        Arc::clone(&svc.posture_cache),
    );
    if svc.posture_engine_tx.set(posture_tx.clone()).is_err() {
        tracing::error!(
            "posture freshness wiring failed: posture_engine_tx already initialized (fail-closed)"
        );
        std::process::exit(1);
    }
    tracing::info!("posture: serialized worker started");

    // SAFETY: SG9 | REQ: sensor-liveness-watchdog | TEST: test_watchdog_dead_mans_switch_fires_after_telemetry_timeout
    // A node going silent past AV_TELEMETRY_TIMEOUT_MS produces a WatchdogTimeout
    // trigger, which the posture engine worker consumes and recomputes the
    // posture (typically collapsing the affected node to LockedOut, which fails
    // the actuator gate closed).
    kirra_verifier::telemetry_watchdog::spawn_telemetry_watchdog(
        Arc::clone(&svc.app),
        posture_tx.clone(),
        Arc::clone(&svc.posture_cache),
    );
    tracing::info!(
        timeout_ms = kirra_verifier::telemetry_watchdog::AV_TELEMETRY_TIMEOUT_MS,
        "telemetry watchdog spawned (SG9 sensor-liveness)"
    );

    // Periodic recompute-and-restamp loop at POSTURE_REFRESH_INTERVAL_MS (= TTL/2)
    // — load-bearing: without it the cache goes stale one TTL after the last
    // event and the gate fails closed fleet-wide. It is also the engine-liveness
    // signal (if the loop stops, the cache stales and the gate fail-closes).
    let refresh_tx = posture_tx;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(
            kirra_verifier::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
        ));
        // Coalesce missed refresh windows instead of bursting catch-up recalcs
        // after runtime starvation (the trigger only re-stamps the cache; bursts
        // add no freshness and the posture worker already coalesces). Delay
        // re-paces from the actual wake time.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; skip it (the caller's initial recalc
        // already covered the populated-cache precondition).
        tick.tick().await;
        loop {
            tick.tick().await;
            if refresh_tx
                .try_send(kirra_verifier::posture_engine_v2::PostureRecalcTrigger::PeriodicRefresh)
                .is_err()
            {
                tracing::error!(
                    "posture periodic refresh: worker channel unavailable — \
                     cache will go stale (gate will fail-close fleet-wide)"
                );
            }
        }
    });
    tracing::info!(
        interval_ms = kirra_verifier::posture_cache::POSTURE_REFRESH_INTERVAL_MS,
        ttl_ms = kirra_verifier::posture_cache::POSTURE_CACHE_TTL_MS,
        "posture: periodic refresh loop started"
    );

    // #88: verifier→fabric posture feed (single-local-asset model). Mirrors this
    // controller's aggregate FleetPosture into the fabric posture for the one
    // locally governed asset, so fabric command enforcement reflects real
    // verified trust instead of the interim registration seed.
    spawn_local_asset_posture_feed(Arc::clone(svc));

    // WS-4 / Track 3 — OTA campaign posture-sweep monitor. Between explicit
    // `advance` calls, auto-halts any active campaign the moment a fleet
    // regression is CONFIRMED (a fresh non-Nominal posture; an unavailable/stale
    // posture is NOT a regression and is skipped). Wired here in the Active block
    // so a promoted standby (H2) also runs it; supervised non-critical (a wedged
    // monitor cannot make anything unsafe — the actuator gate is the safety spine).
    kirra_verifier::campaign_monitor::spawn_campaign_monitor(
        Arc::clone(&svc.app),
        Arc::clone(&svc.posture_cache),
    );
    tracing::info!(
        sweep_ms = kirra_verifier::campaign_monitor::CAMPAIGN_SWEEP_MS,
        "OTA campaign posture-sweep monitor spawned (WS-4 halt-on-regression)"
    );

    // WP-15 (MGA G-19) — mTLS cert-principal expiry monitor. Periodically censuses
    // the cert_principals registry and WARNs (log + audit) when a pinned client cert
    // has lapsed or is about to, so an operator renews before it silently stops
    // authorizing. Observability only (the auth path already fail-closes an expired
    // cert); supervised non-critical. Active-only (this instance owns audit writes).
    kirra_verifier::cert_expiry_monitor::spawn_cert_expiry_monitor(Arc::clone(&svc.app));
    tracing::info!(
        sweep_ms = kirra_verifier::cert_expiry_monitor::CERT_EXPIRY_SWEEP_MS,
        warn_window_ms = kirra_verifier::cert_expiry_monitor::CERT_EXPIRY_WARN_WINDOW_MS,
        "mTLS cert-principal expiry monitor spawned (WP-15 cert lifecycle)"
    );

    // WS-4 / Track 3 — WORM off-box audit shipper. Opt-in: runs only when
    // KIRRA_AUDIT_SHIP_PATH names an append-only sink file, appending new audit
    // records off-box each cycle so the tamper-evidence log survives loss of this
    // box. Active-only (this instance writes the audit records); supervised
    // non-critical (the local chain stays intact + independently verifiable).
    if kirra_verifier::audit_shipper::spawn_audit_shipper(Arc::clone(&svc.app)) {
        tracing::info!(
            interval_ms = kirra_verifier::audit_shipper::AUDIT_SHIP_INTERVAL_MS,
            "WORM off-box audit shipper spawned (WS-4 audit survivability)"
        );
    }
}

/// One idempotent push of the cached fleet posture into the local asset.
///
/// Fail-closed: a poisoned OR stale cache yields NO push. The actuator gate
/// already fail-closes on a stale fleet posture, so leaving the asset's last
/// good posture in place is correct — we never write a stale or
/// not-yet-computed posture forward. Compare-before-write avoids churn (and a
/// generation bump / propagation pass) when the posture is unchanged.
/// #88 tightening: seed the LOCAL fabric asset fail-closed `LockedOut`.
///
/// `register_asset` seeds every asset `Degraded` (the documented interim) —
/// correct for PEERS, which have no lifting feed (cross-asset propagation only
/// degrades, never lifts, so a `LockedOut` peer would be bricked). But the ONE
/// locally governed asset named by `KIRRA_FABRIC_ASSET_ID` DOES have a lifting
/// feed (`sync_local_asset_posture`), so it can be fail-closed: it starts
/// `LockedOut` and the feed lifts it to a real posture on the first Active
/// recalc. On `PassiveStandby` (no recalc) it correctly stays `LockedOut` until
/// promotion. Call this right after each `register_asset` for the just-
/// registered asset id; it only acts when that id IS the configured local asset.
fn seed_local_asset_lockedout(svc: &ServiceState, registered_id: &str) {
    let local = std::env::var("KIRRA_FABRIC_ASSET_ID").ok();
    let local = local.as_deref().map(str::trim).filter(|s| !s.is_empty());
    seed_local_asset_lockedout_inner(svc, registered_id, local);
}

/// Env-free core of [`seed_local_asset_lockedout`] (testable). Overrides the
/// `Degraded` registration seed with fail-closed `LockedOut` IFF `registered_id`
/// is the configured local asset. A peer (or an unset `local_id`) is left at its
/// `Degraded` seed — peers rely on it.
fn seed_local_asset_lockedout_inner(svc: &ServiceState, registered_id: &str, local_id: Option<&str>) {
    let Some(local_id) = local_id else { return };
    if local_id != registered_id {
        return;
    }
    svc.fabric_router.update_asset_posture(
        local_id,
        AssetPosture {
            asset_id: local_id.to_string(),
            posture: FleetPosture::LockedOut,
            // generation 0 = never-computed sentinel; the feed's first push
            // (>= generation 1) supersedes it, exactly like the register seed.
            generation: 0,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["LOCAL_ASSET_FAILCLOSED_PENDING_FEED".to_string()],
        },
    );
    tracing::info!(
        asset_id = %local_id,
        "local fabric asset seeded fail-closed LockedOut; the verifier→fabric feed lifts it on the first Active recalc"
    );
}

fn sync_local_asset_posture(svc: &ServiceState, asset_id: &str) {
    let now = now_ms();
    let fleet = {
        let guard = match svc.posture_cache.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!(
                    "verifier→fabric feed: posture cache poisoned — skipping push (fail-closed)"
                );
                return;
            }
        };
        match guard.as_ref() {
            Some(c) if !c.is_stale(now) => c.posture,
            Some(_) => return, // stale → do not propagate a stale posture
            None => return,    // not yet computed
        }
    };

    let current = svc.fabric_router.asset_posture(asset_id);
    if let Some(ref existing) = current {
        if existing.posture == fleet {
            return; // unchanged — nothing to do
        }
    }
    let next_gen = current
        .as_ref()
        .map(|p| p.generation.saturating_add(1))
        .unwrap_or(1);

    let blocked_by = match fleet {
        FleetPosture::Nominal => vec![],
        FleetPosture::Degraded => vec!["VERIFIER_FLEET_POSTURE_DEGRADED".to_string()],
        FleetPosture::LockedOut => vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
    };

    let updated = AssetPosture {
        asset_id: asset_id.to_string(),
        posture: fleet,
        generation: next_gen,
        computed_at_ms: now,
        contributing_nodes: vec![],
        blocked_by,
    };
    // External-entry update: runs one bounded cross-asset propagation pass so
    // a LockedOut local asset degrades its dependents in the same fabric pass.
    svc.fabric_router
        .update_asset_posture_and_propagate(asset_id, updated);
    tracing::info!(
        asset_id = %asset_id,
        posture = ?fleet,
        generation = next_gen,
        "verifier→fabric posture feed: local asset posture updated"
    );
}

fn emit_posture_event(state: &AppState, event_type: &str, node_id: Option<String>) {
    let posture = node_id.as_ref().map(|id| state.calculate_posture(id));
    let _ = state.posture_tx.send(PostureStreamEvent {
        event_type: event_type.to_string(),
        node_id,
        emitted_at_ms: now_ms(),
        posture,
    });
}


// --- Request / response types -----------------------------------------------

#[derive(Deserialize)]
struct RegisterNodeRequest {
    node_id: String,
    #[serde(default)]
    ak_public_pem: Option<String>,
    #[serde(default)]
    expected_pcr16_digest_hex: Option<String>,
    /// #397 console — optional site/location label captured at registration.
    #[serde(default)]
    site: Option<String>,
    /// #398 console — optional firmware version label captured at registration.
    #[serde(default)]
    firmware_version: Option<String>,
    /// TPM-quote follow-up (#572): when `true`, the node MUST present a hardware
    /// TPM quote on `/attestation/verify` — a self-reported PCR16 digest alone is
    /// not accepted. Persisted to the `node_attestation_policy` table before the
    /// node record is committed, so a required-quote node is never live without its
    /// policy (fail-closed).
    ///
    /// WP-16 (MGA G-8): `Option<bool>` so an OMITTED field defers to the
    /// `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT` env gate (measured-boot fleets flip the
    /// default to quote-required), while an EXPLICIT value always wins — a node with
    /// no TPM can still register `require_tpm_quote: false` even when the fleet
    /// default is on. See `resolve_require_tpm_quote`.
    #[serde(default)]
    require_tpm_quote: Option<bool>,
}

/// WP-16 (MGA G-8) — parse the `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT` env gate: the
/// fleet-wide default TPM-quote requirement for a registration that does NOT set
/// the field explicitly. Default OFF (unset/empty/other → `false`), so a
/// deployment that does not opt in is byte-identical to prior behaviour. `1` /
/// `true` (case-insensitive, trimmed) enable it — the same convention as the other
/// bool env gates. Pure (takes the raw value), so it needs no `set_var` to test.
fn require_tpm_quote_fleet_default(raw: Option<&str>) -> bool {
    raw.map(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
    .unwrap_or(false)
}

/// WP-16 (MGA G-8) — resolve the EFFECTIVE quote requirement for a registration.
/// An EXPLICIT request field always wins (a TPM-less node can register
/// `require_tpm_quote: false` even under a quote-required fleet default); an
/// OMITTED field (`None`) defers to the fleet default. Pure, so the truth table is
/// unit-tested without env (INVARIANT #13).
fn resolve_require_tpm_quote(explicit: Option<bool>, fleet_default: bool) -> bool {
    explicit.unwrap_or(fleet_default)
}

/// WP-16 (MGA G-8) — true iff `s` is a SHA-256 PCR16 value: exactly 64 hex chars
/// (32 bytes). The TPM-quote parser enforces the SHA-256 PCR bank (`sha256:16`), so
/// a quote-required node's expected PCR16 value must be 64 hex — any other length
/// could never match a real quote's `pcrDigest`. Used to reject a quote-required
/// registration that carries an unusable expectation (Copilot #861).
fn is_valid_pcr16_sha256_hex(s: &str) -> bool {
    let s = s.trim();
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Deserialize)]
struct RegisterDependenciesRequest {
    node_id: String,
    depends_on: Vec<String>,
}

#[derive(Deserialize)]
struct VerifyAttestationRequest {
    node_id: String,
    nonce: u64,
    proof_hex: String,
    /// Measured-boot PCR16 digest the node presents on THIS attestation (hex).
    /// Bound into the AK-signed proof. Required (and matched against the
    /// registered `expected_pcr16_digest_hex`) for a node enrolled with a
    /// measured-boot expectation; `None`/absent for a node with no expectation
    /// (back-compat). See `attestation::verify_attestation_proof_with_pcr16`.
    #[serde(default)]
    presented_pcr16_digest_hex: Option<String>,
    /// Hardware TPM quote (TPM-quote follow-up to #572). When present it is
    /// verified via `tpm_quote::verify_tpm_quote` against the node's registered
    /// AK, the challenge nonce (canonical 8-byte big-endian `extraData`), and
    /// the expected PCR16 digest. REQUIRED for a node whose
    /// `node_attestation_policy.require_tpm_quote` is set; optional otherwise.
    #[serde(default)]
    tpm_quote: Option<TpmQuoteEvidence>,
}

/// The two hex fields of a TPM 2.0 quote: the marshaled `TPMS_ATTEST` bytes the
/// AK signed, and the Ed25519 signature over them. See
/// `tpm_quote::marshal_pcr16_quote` for the canonical body encoding.
#[derive(Deserialize)]
struct TpmQuoteEvidence {
    quote_msg_hex: String,
    signature_hex: String,
}

#[derive(Serialize)]
struct AttestationStatusResponse {
    node_id: String,
    status: String,
    registered_at_ms: u64,
}

#[derive(Deserialize)]
struct SensorFaultReportRequest {
    source_node_id: String,
    confidence_score: f64,
    hardware_fault_detected: bool,
}

#[derive(Deserialize)]
struct RegisterAvAssetRequest {
    node_id: String,
    subsystem_type: String,
    hardware_id: String,
    #[serde(default)]
    confidence_floor: Option<f64>,
}

// --- Handlers ----------------------------------------------------------------



/// Outcome of the offloaded federation commit (`submit_federated_report`), mapped
/// to an HTTP response on the async side. Side effects that must happen on the
/// async task (the `Fenced` self-demote of `mode_active`) are applied by the caller,
/// not inside the blocking closure.
enum FedCommitOutcome {
    Accepted,
    /// Clean rejection — `&'static str` reason for the `ReportEvaluation` body.
    Rejected(&'static str),
    /// 500 with this error message.
    InternalError(&'static str),
    /// Epoch-fenced mid-commit; carries the debug reason for the log line.
    Fenced(String),
}








#[derive(Serialize)]
struct AvSubsystemView {
    node_id: String,
    subsystem_type: String,
    hardware_id: String,
    confidence_floor: f64,
    last_telemetry_ms: u64,
    recovery_streak_count: u32,
    recovery_streak_start_ms: u64,
}


#[derive(Serialize)]
struct OperatorView {
    operator_id: String,
    operator_key_fingerprint: String,
    registered_at_ms: u64,
    revoked_at_ms: Option<u64>,
    active: bool,
}






#[derive(Deserialize)]
struct AuditExportQuery {
    limit: Option<u64>,
    offset: Option<u64>,
}


#[derive(Deserialize)]
struct RotateSigningKeyRequest {
    /// Base64 of the NEW 32-byte Ed25519 signing seed (the private key). The
    /// store must hold the private half to sign subsequent rows under the new
    /// key — a public-key-only rotation can never actually swap signing (#76).
    /// Admin-gated endpoint; transmit over TLS in production.
    new_signing_key_b64: String,
    reason: String,
}



/// Replay/freshness metadata required on every per-protocol industrial request.
/// Flattened on the wire: `sequence` and `timestamp_ms` sit at the top level
/// alongside the protocol message's own fields. The message's `source_node` is the
/// per-source replay key. (The unified `/industrial/evaluate` envelope carries the
/// same fields on `UnifiedIndustrialRequest` directly.)
#[derive(serde::Deserialize)]
struct ReplayGuarded<T> {
    sequence: u64,
    timestamp_ms: u64,
    #[serde(flatten)]
    message: T,
}








#[derive(Deserialize)]
struct RegisterIdentityRequest {
    node_id: String,
    ak_public_fingerprint_hex: String,
}







// --- Fabric handlers --------------------------------------------------------

#[derive(Deserialize)]
struct RegisterFabricAssetRequest {
    asset_id: String,
    asset_type: AssetType,
    display_name: String,
    kinematic_profile: KinematicProfileType,
    metadata: Option<std::collections::HashMap<String, String>>,
}







#[derive(Deserialize)]
struct CausalLogQuery {
    from_ms: Option<u64>,
    to_ms: Option<u64>,
    limit: Option<u32>,
    offset: Option<u32>,
}




// ---------------------------------------------------------------------------
// #46 — systemd `Type=notify` integration: READY notification + watchdog.
//
// READY=1 is sent once the listener is bound (the service is ready to accept);
// WATCHDOG=1 is pinged from the posture-engine liveness signal so a hung-but-
// alive process (dead posture worker → stale cache) misses the ping and systemd
// restarts it (fail-closed). No new dependency — sd_notify is a single line
// written to the `$NOTIFY_SOCKET` datagram socket. Every path is a best-effort
// no-op when not run under systemd (env vars unset).
// ---------------------------------------------------------------------------

/// Send one sd_notify message (e.g. `"READY=1"`, `"WATCHDOG=1"`) to the socket
/// at `socket` (a filesystem path, or an abstract socket when it begins with
/// `@`). Separated from [`sd_notify`] so it is testable without mutating
/// `NOTIFY_SOCKET` in-process (INVARIANT #13).
fn sd_notify_to(socket: &std::ffi::OsStr, message: &str) -> std::io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    let sock = UnixDatagram::unbound()?;
    let path_str = socket.to_string_lossy();
    if let Some(name) = path_str.strip_prefix('@') {
        // Linux abstract namespace socket (leading NUL).
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;
        let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
        sock.connect_addr(&addr)?;
    } else {
        sock.connect(std::path::Path::new(socket))?;
    }
    sock.send(message.as_bytes())?;
    Ok(())
}

/// Best-effort sd_notify. No-op when `NOTIFY_SOCKET` is unset (not run under
/// `Type=notify`); a send failure is logged, never fatal.
fn sd_notify(message: &str) {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(e) = sd_notify_to(&socket, message) {
        tracing::warn!(error = %e, message, "sd_notify: send failed");
    }
}

/// Whether the watchdog should ping this tick. On the Active node the ping is
/// GATED on posture-engine liveness (a fresh cache — the refresh loop restamps
/// it each interval; a hung worker lets it go stale → ping withheld → systemd
/// restarts, fail-closed). PassiveStandby has no posture engine by design, so
/// it pings as a plain keepalive (its liveness is the promotion monitor).
fn watchdog_should_ping(is_active: bool, cache_fresh: bool) -> bool {
    if is_active {
        cache_fresh
    } else {
        true
    }
}

/// Spawn the systemd watchdog keepalive (#46). No-op unless `WATCHDOG_USEC` is
/// set (i.e. the unit declares `WatchdogSec=`). Pings `WATCHDOG=1` at half the
/// configured interval, gated by [`watchdog_should_ping`].
fn spawn_systemd_watchdog(svc: Arc<ServiceState>) {
    let usec: u64 = match std::env::var("WATCHDOG_USEC").ok().and_then(|v| v.parse().ok()) {
        Some(u) if u > 0 => u,
        _ => return, // no WatchdogSec configured → nothing to feed
    };
    let period = std::time::Duration::from_micros(usec / 2); // systemd-recommended margin
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tracing::info!(watchdog_usec = usec, "systemd watchdog keepalive started");
        loop {
            tick.tick().await;
            let cache_fresh = svc
                .posture_cache
                .read()
                .ok()
                .and_then(|g| g.as_ref().map(|c| !c.is_stale(now_ms())))
                .unwrap_or(false);
            if watchdog_should_ping(svc.app.is_active(), cache_fresh) {
                sd_notify("WATCHDOG=1");
            } else {
                tracing::error!(
                    "systemd watchdog: Active posture engine appears stalled (cache stale) — \
                     withholding WATCHDOG ping; systemd will restart (SG-003 / SG9 fail-closed)"
                );
            }
        }
    });
}

// --- Entry point ------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Install a tracing subscriber FIRST, before any fallible startup step, so
    // the fail-closed startup diagnostics below (and all runtime logs) are
    // actually emitted. Without an installed subscriber, tracing events are
    // dropped on the floor and a fail-closed `exit(1)` would be SILENT — the
    // prior `.expect()`/`panic!` always reached stderr, so the conversion to
    // `tracing::error!` must be backed by a subscriber to preserve startup
    // diagnosability. Honors `RUST_LOG`; defaults to `info`. `try_init` tolerates
    // a subscriber already installed by an embedding harness instead of panicking.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let db_path = std::env::var("KIRRA_DB_PATH")
        .unwrap_or_else(|_| "kirra_verifier.sqlite".to_string());
    let listen_addr = std::env::var("KIRRA_VERIFIER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string());

    let mut store = match VerifierStore::new(&db_path) {
        Ok(store) => store,
        Err(err) => {
            tracing::error!(
                error = %err,
                db_path = %db_path,
                "startup failed: unable to initialize verifier store (fail-closed)"
            );
            std::process::exit(1);
        }
    };

    let mode = VerifierOperationMode::from_env();
    println!("Kirra Verifier starting in {mode:?} mode (db: {db_path})");

    // #84: load the CANopen-node-id → fleet-node-id map from config so an NMT
    // node-offline event marks the correct asset (effectful recalc). Sourced
    // from KIRRA_CANOPEN_NODE_MAP; unset → empty map (every offline is then
    // unattributed, handled fail-closed in evaluate_canopen_adapter).
    kirra_verifier::adapters::canopen::init_node_map_from_env();

    // DNP3 Analog Output magnitude envelope from KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE
    // ("min:max"); unset/invalid → analog control writes are denied (fail-closed).
    kirra_verifier::adapters::dnp3::init_analog_envelope_from_env();

    // CANopen SDO download per-target magnitude bounds from KIRRA_CANOPEN_SDO_BOUNDS
    // ("node:index:subindex=type:min:max", …) + KIRRA_CANOPEN_STRICT_BOUNDS. Unset →
    // SDO writes are posture-only; a configured target is faithfully decoded by its
    // declared type and bounded (fail-closed on breach/undecodable).
    kirra_verifier::adapters::canopen::init_sdo_bounds_from_env();

    // CIP per-attribute magnitude bounds from KIRRA_CIP_ATTR_BOUNDS
    // ("class:instance:attr=type:min:max", …) + KIRRA_CIP_STRICT_BOUNDS. Unset →
    // CIP writes are posture-only; a configured Set_Attribute_Single target is
    // faithfully decoded by its declared type and bounded (fail-closed on breach).
    kirra_verifier::adapters::ethernet_ip::init_cip_bounds_from_env();

    // #312: select the deployment's vehicle class from KIRRA_VEHICLE_CLASS
    // (courier | delivery-av | robotaxi). FAIL-CLOSED: unset/unknown aborts
    // startup (there is no default class — a wrong class would pick another
    // class's envelope). Drives the per-class kinematic contract in the actuator
    // gate (`enforce_actuator_safety_envelope`).
    kirra_verifier::gateway::contract_profiles::init_vehicle_class_from_env();

    let audit_signing_key: Option<ed25519_dalek::SigningKey> =
        std::env::var("KIRRA_LOG_SIGNING_KEY").ok()
            .filter(|s| !s.is_empty())
            .and_then(|b64_str| {
                use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
                b64e.decode(&b64_str).ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                    .map(|seed| ed25519_dalek::SigningKey::from_bytes(&seed))
            });
    let audit_verifying_key = audit_signing_key.as_ref().map(|sk| sk.verifying_key());

    // #165: admit the env-loaded signing key against the DURABLE trust map
    // (audit_trust_anchor + audit_key_ledger) before any signing happens.
    // First boot backfills the anchor; a matching active key resumes; a retired
    // key (restart-reverted) or a brand-new env key WITHOUT an explicit adopt
    // signal is FAIL-CLOSED — the process refuses to start rather than sign
    // under the wrong key. Adopt is opt-in via KIRRA_LOG_SIGNING_KEY_ADOPT=1;
    // an optional KIRRA_LOG_SIGNING_GENESIS_PIN pins the durable genesis.
    if let Some(ref key) = audit_signing_key {
        let adopt = std::env::var("KIRRA_LOG_SIGNING_KEY_ADOPT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let pinned = std::env::var("KIRRA_LOG_SIGNING_GENESIS_PIN")
            .ok()
            .filter(|s| !s.is_empty());
        let admission = match store.admit_signing_key(key.clone(), adopt, pinned.as_deref(), now_ms()) {
            Ok(admission) => admission,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "FAIL-CLOSED (#165): failed to admit audit signing key against the durable trust map"
                );
                std::process::exit(1);
            }
        };
        use kirra_verifier::verifier_store::KeyAdmission;
        // Each rejection is a fail-closed startup REFUSAL — keep the full operator
        // remediation guidance (HOW to recover) in the message, not just the cause.
        match admission {
            KeyAdmission::Resumed
            | KeyAdmission::BackfilledGenesis
            | KeyAdmission::AdoptedReanchor => {
                println!("Audit signing key admitted ({admission:?}).");
            }
            KeyAdmission::RetiredKeyRejected => {
                tracing::error!(
                    "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_KEY is a RETIRED audit key \
                     (a later rotation is the durable active key). Refusing to sign under \
                     a retired key. Provide the current active private key, or perform an \
                     explicit rotation."
                );
                std::process::exit(1);
            }
            KeyAdmission::UnadoptedNewKeyRejected => {
                tracing::error!(
                    "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_KEY is a NEW key not in the durable \
                     ledger and no adopt signal was given. Refusing to silently re-root audit \
                     trust. Set KIRRA_LOG_SIGNING_KEY_ADOPT=1 to consent to adopting it."
                );
                std::process::exit(1);
            }
            KeyAdmission::GenesisPinMismatch => {
                tracing::error!(
                    "FAIL-CLOSED (#165): KIRRA_LOG_SIGNING_GENESIS_PIN does not match the durable \
                     trust anchor's genesis. Refusing to start."
                );
                std::process::exit(1);
            }
            KeyAdmission::MigrationReversionRejected { chain_latest_key_id, env_key_id } => {
                tracing::error!(
                    chain_latest_key_id = %chain_latest_key_id,
                    env_key_id = %env_key_id,
                    "FAIL-CLOSED (#165 migration): the audit chain's latest rotation is to key \
                     {chain_latest_key_id} but KIRRA_LOG_SIGNING_KEY supplied {env_key_id}. The env \
                     key has reverted to a pre-rotation (or foreign) key; anchoring on it would \
                     re-root audit trust. RESOLUTION — supply the correct active key in \
                     KIRRA_LOG_SIGNING_KEY, OR set KIRRA_LOG_SIGNING_KEY_ADOPT=1 to consent to \
                     anchoring on the env key (recorded as a consented reanchor)."
                );
                std::process::exit(1);
            }
        }
    }

    let app_state = Arc::new(AppState::new(store, mode));

    // S3 Pass B2 (#115): spawn the audit-writer task and install its Sender
    // into AppState. The deny arm of the actuator-safety-envelope middleware
    // reaches the Sender via `svc.app.audit_writer_tx.get()` to push the
    // kinematic-violation audit record off the verdict path. Done before
    // the listener binds so no request can race the install.
    let audit_tx =
        kirra_verifier::audit_writer::spawn_audit_writer(Arc::clone(&app_state));
    app_state.install_audit_writer(audit_tx);

    // WP-17 (MGA G-17) — unified env configuration: (1) WARN on any KIRRA_* env var
    // the schema registry does not know (a typo / stale var an operator believes is
    // in effect), and (2) commit the effective boot-config SHA-256 digest to the
    // tamper-evident audit chain, so an operator can prove what configuration this
    // instance booted under and detect drift across restarts. Observability only —
    // the sweep never fails startup (a future var on an older binary is legitimate).
    {
        use kirra_verifier::env_config::{unknown_kirra_env_vars, EffectiveConfig};
        let env_names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
        let unknown = unknown_kirra_env_vars(env_names.iter().map(String::as_str));
        if !unknown.is_empty() {
            tracing::warn!(
                unknown_vars = %unknown.join(","),
                "unrecognized KIRRA_* environment variable(s) — a typo or a stale var \
                 that is NOT taking effect; see the env schema registry"
            );
        }
        let cfg = EffectiveConfig::from_env();
        let digest = cfg.effective_digest();
        let now = now_ms();
        let unknown_count = unknown.len();
        let digest_for_log = digest.clone();
        let mode_for_log = cfg.mode.clone();
        let version_for_log = cfg.config_version;
        // Non-failing (observability), but do NOT silently claim it was committed:
        // warn if the store task OR the append fails, so a missing on-chain digest is
        // visible rather than a phantom "committed" (Copilot #862).
        match app_state
            .store
            .call(move |store| {
                store.append_clearance_audit_event(
                    "EffectiveConfigDigest",
                    &json!({
                        "config_version": cfg.config_version,
                        "digest": digest,
                        "mode": cfg.mode,
                        "vehicle_class": cfg.vehicle_class,
                        "tls_enabled": cfg.tls_enabled,
                        "mtls_enabled": cfg.mtls_enabled,
                        "unknown_kirra_var_count": unknown_count,
                    })
                    .to_string(),
                    now,
                )
            })
            .await
        {
            Ok(Ok(())) => tracing::info!(
                config_version = version_for_log,
                effective_config_digest = %digest_for_log,
                mode = %mode_for_log,
                "effective boot configuration digested + committed to the audit chain (WP-17)"
            ),
            Ok(Err(e)) => tracing::warn!(error = %e,
                "WP-17: effective-config digest append FAILED — no on-chain config proof this boot"),
            Err(_) => tracing::warn!(
                "WP-17: effective-config digest store task failed — no on-chain config proof this boot"),
        }
    }

    // Learning-loop capture writer (Phase 1, #190) — DEFAULT OFF. Only spawned +
    // installed when KIRRA_CAPTURE_ENABLED is set; unset → no writer, and the
    // gateway emit is a pure no-op (capture_writer_tx stays None). Non-safety
    // side channel; mirrors the audit writer wiring above.
    if kirra_verifier::capture::capture_enabled() {
        let capture_tx = kirra_verifier::capture::spawn_capture_writer();
        app_state.install_capture_writer(capture_tx);
        tracing::info!("learning-loop capture ENABLED (KIRRA_CAPTURE_ENABLED) — verdict records → JSONL sink");
    }

    {
        let load_initial = app_state.store.call_read(|store| {
            let nodes = store.load_nodes().map_err(|e| e.to_string())?;
            let dependencies = store.load_dependencies()
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((nodes, dependencies))
        }).await;
        let (nodes, dependencies) = match load_initial {
            Ok(Ok(data)) => data,
            Ok(Err(err)) => {
                tracing::error!(
                    error = %err,
                    "startup failed: unable to load persisted nodes/dependencies (fail-closed)"
                );
                std::process::exit(1);
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "startup failed: store task failed loading initial state (fail-closed)"
                );
                std::process::exit(1);
            }
        };
        for node in nodes {
            app_state.nodes.insert(node.node_id.clone(), node);
        }
        for (node_id, deps) in dependencies {
            app_state.dependency_graph.insert(node_id, deps);
        }
    }

    // WS-0.2 / #G10: initialize the posture-generation counter from the
    // persisted high-water BEFORE any recalculation claims a generation (the
    // Active path's initial recalc and the standby promotion recalc both run
    // after this point). Without this call — missing from the binary until
    // now — every restart reset the live counter to 1: emitted generations
    // regressed across restarts (breaking the ordering federation peers and
    // SSE consumers rely on) and `save_last_generation`'s high-water guard
    // rejected every persist until the counter caught back up.
    // SAFETY: SG-HA-3 — store read runs off the tokio worker threads.
    {
        let app_gen = Arc::clone(&app_state);
        match tokio::task::spawn_blocking(move || {
            kirra_verifier::posture_engine::init_generation_from_store(&app_gen)
        })
        .await
        {
            Ok(Ok(last)) => tracing::info!(
                persisted_high_water = last,
                "posture: generation counter initialized from store (0 = fresh store)"
            ),
            Ok(Err(err)) => {
                tracing::error!(
                    error = %err,
                    "startup failed: unable to load the persisted posture-generation \
                     high-water (fail-closed — serving would time-reverse generations)"
                );
                std::process::exit(1);
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "startup failed: generation-init task failed to join (panic or cancellation) (fail-closed)"
                );
                std::process::exit(1);
            }
        }
    }

    let signing_key = audit_signing_key.clone();
    // #87: the causal log persists to the SAME store the rest of the service
    // uses, so forensic causal rows land in the production DB and chain there.
    let causal_store = app_state.store.clone();
    let svc_state = Arc::new(ServiceState {
        app: app_state,
        posture_cache: Arc::new(std::sync::RwLock::new(None)),
        // #395 console runtime — boot timestamp captured once at startup.
        started_at_ms: now_ms(),
        audit_verifying_key,
        fabric_router: Arc::new(FabricRouter::new()),
        fabric_telemetry: Arc::new(FabricTelemetry::new()),
        fabric_causal_log: Arc::new(FabricCausalLog::new(causal_store, signing_key)),
        posture_engine_tx: std::sync::OnceLock::new(),
        // KIRRA-OCCY-PMON-002: perception-derate composition. DEFAULT OFF —
        // pure no-op (state 1) until #126 wires a real perception ingest and a
        // deployment enables the monitor + starts the publisher worker.
        perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
    });

    {
        // Load the assets under one acquisition; register them OUTSIDE the
        // closure (registration borrows svc_state and calls back into the store
        // via seed_local_asset_lockedout — keep it off the held guard).
        // SAFETY: SG-HA-3 — read off the worker pool via read replica.
        let assets = svc_state.app.store.call_read(|store| store.load_fabric_assets()).await.ok().and_then(|r| r.ok());
        let assets_loaded = match assets {
            Some(assets) => {
                let n = assets.len();
                for asset in assets {
                    svc_state.fabric_router.register_asset(&asset);
                    // #88: the local fed asset is fail-closed LockedOut (peers
                    // keep the Degraded seed); a no-op for every peer.
                    seed_local_asset_lockedout(&svc_state, &asset.asset_id);
                }
                n
            }
            None => 0,
        };
        tracing::info!(count = assets_loaded, "Loaded fabric assets from store");
    }

    // Heartbeat-aware startup arbitration (HA epoch fence).
    //
    // A configured-Active instance must CLAIM the durable epoch before
    // it starts heartbeating, but must NOT steal from a live holder
    // (prevents a restarted old primary from stealing back from a
    // standby that has already promoted). The decision is:
    //
    //   1. Read (epoch E, active_id A) from the singleton ha_state row.
    //   2. Read primary heartbeat age. If A is some OTHER instance and
    //      heartbeat is fresh, stand down to PassiveStandby.
    //   3. Otherwise try_claim_epoch(E, my_id, now). On win, hold the
    //      epoch and proceed Active. On loss, stand down to PassiveStandby
    //      (a concurrent claim landed first — fence held).
    //
    // Even if clock skew makes a live holder LOOK stale, the worst case
    // here is an EXTRA failover: this node claims and bumps the epoch,
    // the real holder gets fenced at its gate (STEP 5) and self-demotes.
    // Still at most one effective writer.
    //
    // A configured-PassiveStandby instance does NOT attempt to claim at
    // startup; it spawns the promotion monitor as before. The monitor
    // will claim via perform_promotion when the primary heartbeat goes
    // stale (the same conditional CAS path).
    let my_id = ha_instance_id();
    let effective_mode = match mode {
        VerifierOperationMode::PassiveStandby => VerifierOperationMode::PassiveStandby,
        VerifierOperationMode::Active => {
            // SAFETY: SG-HA-3 — read probe off the worker pool via read replica.
            let arbitration = svc_state.app.store.call_read(|store| {
                let (epoch, holder) = store.current_active_holder().ok()?;
                let hb_str = store.load_engine_state(HEARTBEAT_KEY).ok()?;
                let now = now_ms();
                let hb_fresh = hb_str
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|ts| now.saturating_sub(ts) < PROMOTION_TIMEOUT_MS)
                    .unwrap_or(false);
                Some((epoch, holder, hb_fresh))
            }).await.ok().flatten();

            match arbitration {
                Some((epoch, Some(holder), true)) if holder != my_id => {
                    tracing::warn!(
                        my_id = %my_id,
                        live_holder = %holder,
                        epoch = epoch,
                        "another Active instance is alive at startup — starting as PassiveStandby instead"
                    );
                    VerifierOperationMode::PassiveStandby
                }
                Some((epoch, _holder, _stale_or_self)) => {
                    // SAFETY: SG-HA-3 — epoch claim is a durable write; off the worker pool.
                    let my_id_c = my_id.clone();
                    let claim = svc_state.app.store.call(move |s| {
                        Ok::<_, ()>(s.try_claim_epoch(epoch, &my_id_c, now_ms()).ok().flatten())
                    }).await.ok().and_then(|r| r.ok()).flatten();
                    match claim {
                        Some(new_epoch) => {
                            svc_state
                                .app
                                .held_epoch
                                .store(new_epoch, std::sync::atomic::Ordering::SeqCst);
                            tracing::info!(
                                my_id = %my_id,
                                epoch = new_epoch,
                                "Active startup: durable epoch claimed"
                            );
                            VerifierOperationMode::Active
                        }
                        None => {
                            tracing::error!(
                                my_id = %my_id,
                                observed_epoch = epoch,
                                "Active startup: epoch claim LOST — another instance won the race; \
                                 starting as PassiveStandby (fail-closed; one writer invariant preserved)"
                            );
                            VerifierOperationMode::PassiveStandby
                        }
                    }
                }
                None => {
                    // Could not read ha_state (store error). Fail closed to
                    // PassiveStandby rather than serve as an unfenced Active.
                    tracing::error!(
                        my_id = %my_id,
                        "Active startup: unable to read ha_state for epoch arbitration; \
                         starting as PassiveStandby (fail-closed)"
                    );
                    VerifierOperationMode::PassiveStandby
                }
            }
        }
    };

    // Bring the per-process mode atomic in line with the arbitrated decision.
    // (AppState::new initialized mode_active from the env-derived `mode`; if
    // arbitration downgraded Active→PassiveStandby, flip it off.)
    if effective_mode == VerifierOperationMode::PassiveStandby {
        svc_state
            .app
            .mode_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    match effective_mode {
        VerifierOperationMode::Active => {
            spawn_heartbeat_writer(Arc::clone(&svc_state.app));
            tracing::info!("Heartbeat writer started (Active mode)");
        }
        VerifierOperationMode::PassiveStandby => {
            // review H2: hand the promotion monitor an on-promote hook that
            // (re)starts the Active posture-freshness tasks on this node when it
            // is promoted at runtime. Without it a promoted standby serves a
            // stale cache one TTL later and fail-closes every gated route. The
            // hook captures `ServiceState` (which the lib promotion path does
            // not have — `spawn_local_asset_posture_feed` and `posture_engine_tx`
            // are bin/ServiceState-local), so the wiring stays where it belongs.
            let svc_for_promote = Arc::clone(&svc_state);
            spawn_promotion_monitor(
                Arc::clone(&svc_state.app),
                Arc::clone(&svc_state.posture_cache),
                Arc::new(move || wire_active_posture_freshness(&svc_for_promote)),
            );
            tracing::info!("Promotion monitor started (PassiveStandby mode)");
        }
    }

    // One-time, idempotent: anchor the v1/v2 hash boundary in the audit
    // chain. Active instances only (the anchor is a write; a passive
    // standby is read-only and must not write — a later promotion path
    // will run this and no-op via the idempotency guard). Runs AFTER
    // set_signing_key so the HASH_V2_MIGRATION event itself is signed
    // and therefore tamper-evident. The two info/error log lines below
    // are the OBSERVABLE PROOF the wiring is live — operators can
    // confirm the anchor ran (or was deliberately skipped) from the
    // startup log alone, since the assembled-app self-test that would
    // catch a missing call sits behind the build_app extraction follow-up
    // (#72). Do not remove the log lines.
    if svc_state.app.is_active() {
        // SAFETY: SG-HA-3 — startup writes off the worker pool.
        match svc_state.app.store.call(|store| store.ensure_hash_v2_migration_anchor(now_ms())).await {
            Ok(Ok(())) => tracing::info!("audit: hash-v2 migration anchor ensured"),
            Ok(Err(e)) => tracing::error!(error = %e, "audit: hash-v2 migration anchor FAILED at startup"),
            Err(_) => tracing::error!("audit: hash-v2 migration anchor FAILED — store task error"),
        }
        // Key-id backfill (#76): assign existing NULL-key_id rows the genesis
        // key's id so they verify after a future rotation. Idempotent; signed.
        match svc_state.app.store.call(|store| store.ensure_key_id_backfill_migration(now_ms())).await {
            Ok(Ok(())) => tracing::info!("audit: key-id backfill migration ensured"),
            Ok(Err(e)) => tracing::error!(error = %e, "audit: key-id backfill migration FAILED at startup"),
            Err(_) => tracing::error!("audit: key-id backfill migration FAILED — store task error"),
        }
        // Anchor-head backfill (#77): a chain written by a pre-#77 binary has no
        // signed head; sign one from the current tail so an upgraded store
        // presents a head BEFORE serving /system/audit/verify (no false
        // HEAD_ABSENT). Idempotent. Log-and-continue: a missing head is itself
        // caught fail-closed at verify time (head_verified = false).
        match svc_state.app.store.call(|store| store.ensure_audit_anchor_head(now_ms())).await {
            Ok(Ok(())) => tracing::info!("audit: anchor-head high-water mark ensured"),
            Ok(Err(e)) => tracing::error!(error = %e, "audit: anchor-head high-water mark FAILED at startup"),
            Err(_) => tracing::error!("audit: anchor-head high-water mark FAILED — store task error"),
        }
    } else {
        tracing::info!(
            "audit: hash-v2 + key-id migrations skipped — passive standby (read-only)"
        );
    }

    // ── Posture-cache freshness wiring (Active path only) ────────────────
    //
    // Without this, a fresh Active primary serves 503 for every functional
    // route: the posture cache starts as `None`, the routing gate
    // fail-closes on None or stale, and nothing on the Active path was
    // populating the cache. The three-part fix:
    //
    //   (a) one synchronous initial recalc BEFORE axum::serve so the gate
    //       has a populated cache on first request,
    //   (b) the serialized posture-engine worker spawned so event-driven
    //       triggers (NodeTrustChanged, DependencyGraphChanged, etc.)
    //       refresh the cache,
    //   (c) a periodic recompute-and-restamp loop at POSTURE_REFRESH_INTERVAL_MS
    //       (= TTL/2) — load-bearing: without it the cache goes stale
    //       one TTL after the last event and the gate fails closed
    //       fleet-wide. The same loop is the engine-liveness signal: if
    //       the loop stops (worker dead, channel full repeatedly), the
    //       cache goes stale and the gate fails closed — the desired
    //       fail-safe.
    //
    // PassiveStandby does not run this at startup — but its promotion path now
    // calls the SAME `wire_active_posture_freshness` on transition to Active
    // (review H2), so a runtime-promoted node keeps its cache fresh instead of
    // fail-closing one TTL after promotion.
    // SG-008 startup-invariant fact: set true once the watchdog is spawned on
    // the Active path (PassiveStandby leaves it false — and the sentinel does
    // not require it there).
    let mut watchdog_spawned = false;
    if svc_state.app.is_active() {
        // (a) SAFETY: SG-HA-3 — initial posture recompute includes durable DB
        // writes; run it off tokio worker threads. This is the caller-specific
        // half (see `wire_active_posture_freshness` doc): the Active startup
        // path recomputes synchronously BEFORE `axum::serve` so the gate has a
        // populated cache on the first request.
        let app_b = Arc::clone(&svc_state.app);
        let cache_b = Arc::clone(&svc_state.posture_cache);
        let initial_recalc = tokio::task::spawn_blocking(move || {
            kirra_verifier::posture_engine::recalculate_and_broadcast(&app_b, &cache_b);
        })
        .await;
        if let Err(err) = initial_recalc {
            tracing::error!(
                error = %err,
                "startup failed: initial posture recalc task panicked (fail-closed)"
            );
            std::process::exit(1);
        }
        tracing::info!("posture: initial recalc complete; cache populated");

        // (b)–(e) the ongoing-freshness tasks (worker, watchdog, periodic
        // refresh, local-asset feed) — shared verbatim with the promotion path.
        wire_active_posture_freshness(&svc_state);
        watchdog_spawned = true;
    } else {
        tracing::info!(
            "posture: freshness wiring skipped at startup — passive standby \
             (re-wired by the promotion path on transition to Active)"
        );
    }

    // Assemble the production router. Extracted into `build_app` (issue #72)
    // so the EXACT assembled router — identical routes, middleware layer
    // order, and state wiring — is what the binary-internal posture-gate
    // test exercises, rather than a representative stand-in.
    let app = build_app(Arc::clone(&svc_state));

    // SG-008 (ASIL D): fail closed BEFORE binding the listener. Build the boot
    // facts and evaluate the startup-invariant predicate; on any violation, log
    // and abort so no request can reach a half-initialized service. Bind is
    // strictly AFTER this check, so "the listener never binds before invariants
    // pass" holds by construction.
    let startup_ctx = StartupContext {
        hardware_root_trusted: matches!(
            kirra_verifier::startup_sentinel::StartupSentinel::verify_hardware_root(),
            kirra_verifier::startup_sentinel::StartupTrustState::Trusted
        ),
        admin_token_present: std::env::var("KIRRA_ADMIN_TOKEN")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        sqlite_wal: svc_state.app.store.call_read(|store| store.is_wal_mode()).await.ok().unwrap_or(false),
        mode_active: svc_state.app.is_active(),
        watchdog_spawned,
        posture_engine_running: svc_state.posture_engine_tx.get().is_some(),
    };
    if let Err(violation) = check_startup_invariants(&startup_ctx) {
        tracing::error!(
            invariant = %violation,
            "SG-008: startup invariant violated — aborting before listener bind (fail-closed)"
        );
        std::process::exit(1);
    }
    tracing::info!("SG-008: startup invariants satisfied; binding listener");

    // WS-1 Track 1.2: resolve the opt-in TLS serve mode and LOAD/validate the
    // rustls config BEFORE binding — a partial or invalid TLS config aborts here
    // (fail-closed), never after we have claimed the port or told systemd READY.
    // Default (neither env var) is Plaintext → byte-identical to before.
    let tls_config = match tls::resolve_tls_from_env() {
        Ok(tls::TlsResolution::Plaintext) => None,
        Ok(tls::TlsResolution::Tls { cert_path, key_path, client_ca_path }) => {
            match tls::load_server_config(&cert_path, &key_path, client_ca_path.as_deref()) {
                Ok(cfg) => {
                    tracing::info!(
                        cert = %cert_path.display(),
                        mtls = client_ca_path.is_some(),
                        "TLS termination enabled (in-process, ring provider)"
                    );
                    Some(cfg)
                }
                Err(err) => {
                    tracing::error!(error = %err, "TLS config invalid — aborting before bind (fail-closed)");
                    std::process::exit(1);
                }
            }
        }
        Err(err) => {
            tracing::error!(error = %err, "TLS config invalid — aborting before bind (fail-closed)");
            std::process::exit(1);
        }
    };

    println!("Kirra Verifier Service listening on {listen_addr} (db: {db_path})");
    let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(
                error = %err,
                listen_addr = %listen_addr,
                "startup failed: failed to bind listener (fail-closed)"
            );
            std::process::exit(1);
        }
    };

    // #46: the listener is bound and startup invariants passed (SG-008) — tell
    // systemd we are READY (Type=notify) and start the watchdog keepalive
    // (gated on posture-engine liveness; fail-closed on a stalled engine). Both
    // are no-ops outside a `Type=notify` / `WatchdogSec=` unit.
    sd_notify("READY=1");
    spawn_systemd_watchdog(Arc::clone(&svc_state));

    // #74: on safe-stop / shutdown, force a durable checkpoint so the audit chain
    // (and any NORMAL-connection writes) are fsync'd to disk — durable at the
    // moment that matters most (the incident preceding the stop). The HA epoch
    // and federation nonce burns are already FULL-synced per-commit.
    let shutdown_state = Arc::clone(&svc_state.app);
    let shutdown = async move {
        shutdown_signal().await;
        // Offload the WAL checkpoint (a `wal_checkpoint(TRUNCATE)` fsync — the
        // longest single store hold) so it runs on the blocking pool rather than the
        // runtime thread driving graceful shutdown.
        match shutdown_state.store.call(|store| store.durable_checkpoint()).await {
            Ok(Ok(())) => tracing::info!("audit: durable checkpoint flushed on shutdown"),
            Ok(Err(e)) => tracing::error!(error = %e, "audit: durable checkpoint FAILED on shutdown"),
            Err(_) => tracing::error!("audit: durable checkpoint skipped — store unavailable at shutdown"),
        }
    };

    let serve_result = match tls_config {
        // Opt-in in-process TLS (WS-1 Track 1.2). Per-connection handshake tasks;
        // the mesh-mTLS transport gate (#805) still composes on top when enabled.
        Some(cfg) => tls::serve_tls(listener, app, cfg, shutdown).await,
        // Default plaintext path — unchanged (`axum::serve` graceful shutdown).
        None => axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await,
    };
    if let Err(err) = serve_result {
        tracing::error!(error = %err, "server exited with error");
        std::process::exit(1);
    }
}

// ===========================================================================
// Operator console — Phase A (#103 SG6).
//
// A real UI served by this service against real store data, plus the ONE
// authenticated inbound affordance: recording a supervisor clearance grant.
//
// THE HONESTY RULE (Phase A): nothing here releases a vehicle. A grant is
// RECORDED AND SIGNED; release happens only when Phase B delivers it to the
// node's `ClearanceLoop`. The response, the audit payload, and the UI all say
// so (`delivery: pending-phase-b` / `PENDING-NODE-TRANSPORT`).
//
// Reachability: the `/console` plane is posture-EXEMPT (see
// `gateway::policy_layer::is_posture_exempt`) — it must work *during* LockedOut,
// which is exactly when an operator needs it. The reads are QM; the grant is
// gated by the supervisor key below.
// ===========================================================================



#[derive(Deserialize)]
struct ConsoleAuditQuery {
    limit: Option<u64>,
    offset: Option<u64>,
}




/// Query for #396 console analytics. `window_ms` defaults to 24h.
#[derive(Deserialize)]
struct ConsoleAnalyticsQuery {
    #[serde(default)]
    window_ms: Option<u64>,
}




/// Pure supervisor-key decision (testable without env — INV-13 forbids `set_var`
/// in multithreaded tests). REUSES the #255 mechanism: the value is the
/// `KIRRA_SUPERVISOR_RESET_KEY`, constant-time compared. Fail-closed:
/// unconfigured / empty / `> 64` bytes (INVARIANT #7) → 503; missing or
/// mismatched provided key → 401.
fn supervisor_key_ok(provided: Option<&str>, configured: Option<&str>) -> Result<(), StatusCode> {
    let configured = configured.filter(|v| !v.is_empty()).ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if configured.len() > 64 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let provided = provided.ok_or(StatusCode::UNAUTHORIZED)?;
    if !constant_time_compare(provided.as_bytes(), configured.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

/// Env-reading wrapper: pulls `KIRRA_SUPERVISOR_RESET_KEY` (env-only, INVARIANT
/// #7) and the `x-kirra-supervisor-key` header, then delegates to
/// [`supervisor_key_ok`].
fn check_supervisor_key(headers: &HeaderMap) -> Result<(), StatusCode> {
    let configured = std::env::var("KIRRA_SUPERVISOR_RESET_KEY").ok();
    let provided = headers
        .get("x-kirra-supervisor-key")
        .and_then(|h| h.to_str().ok());
    supervisor_key_ok(provided, configured.as_deref())
}

// ---------------------------------------------------------------------------
// #314 Phase 1 — operator-proven identity (the attestation pattern for humans)
// ---------------------------------------------------------------------------

/// Audit a clearance-grant rejection (never records key bytes / signatures).
/// SAFETY: SG-HA-3 — durable write offloaded via `call()` (caller must `.await`).
async fn audit_grant_rejection(
    app: &kirra_verifier::verifier::AppState,
    reason: &str,
    node_id: &str,
    operator_id: &str,
    now: u64,
) {
    let store = app.store.clone();
    let reason = reason.to_string();
    let node_id = node_id.to_string();
    let operator_id = operator_id.to_string();
    let _ = store.call(move |s| {
        let _ = s.append_clearance_audit_event(
            "OperatorClearanceGrantRejected",
            &json!({ "reason": reason, "node_id": node_id, "operator_id": operator_id })
                .to_string(),
            now,
        );
    }).await;
}

/// #412 / ADR-0013 — audit a REJECTED operator e-stop request to the signed
/// chain (distinct event type from a clearance rejection). A rejected stop never
/// commanded the MRC; the record is the non-repudiable trail of the attempt.
async fn audit_estop_rejection(
    app: &kirra_verifier::verifier::AppState,
    reason: &str,
    node_id: &str,
    operator_id: &str,
    now: u64,
) {
    let store = app.store.clone();
    let reason = reason.to_string();
    let node_id = node_id.to_string();
    let operator_id = operator_id.to_string();
    let _ = store.call(move |s| {
        let _ = s.append_clearance_audit_event(
            "OperatorStopRequestRejected",
            &json!({ "reason": reason, "node_id": node_id, "operator_id": operator_id })
                .to_string(),
            now,
        );
    }).await;
}

/// #326 — the operator clearance-challenge map key. Length-prefixing the
/// `operator_id` makes the `operator/node` split UNAMBIGUOUS regardless of any
/// delimiter characters in either id: `("a|b","c")` → `"3:a|b:c"` and
/// `("a","b|c")` → `"1:a:b|c"` are distinct, where the old `"{op}|{node}"` form
/// collided to `"a|b|c"`. Issue and consume MUST use this single constructor.
fn composite_challenge_key(operator_id: &str, node_id: &str) -> String {
    format!("{}:{}:{}", operator_id.len(), operator_id, node_id)
}

/// #326 — reject a structurally-dangerous identifier: empty, the legacy `|`
/// delimiter, or any control character (which could corrupt logs / keys). Charset
/// validation is belt-and-suspenders alongside the length-prefixed key above.
fn valid_identifier(s: &str) -> bool {
    !s.is_empty() && !s.contains('|') && !s.chars().any(char::is_control)
}

/// #327 — a short, stable fingerprint of the presented admin bearer token, for
/// attributing a sensitive admin action (operator reactivation) in the audit chain
/// WITHOUT recording the token itself. `None` if no bearer token is present (the
/// route is admin-gated, so in practice one always is).
fn admin_token_fingerprint(headers: &HeaderMap) -> Option<String> {
    use sha2::Digest;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))?;
    let mut h = sha2::Sha256::new();
    h.update(token.as_bytes());
    Some(hex::encode(&h.finalize()[..8]))
}

#[derive(Deserialize)]
struct RegisterOperatorRequest {
    operator_id: String,
    ed25519_pubkey_pem: String,
}



#[derive(Deserialize)]
struct ClearanceChallengeQuery {
    operator_id: String,
    node_id: String,
}


#[derive(Deserialize)]
struct ClearanceGrantRequest {
    node_id: String,
    operator_id: String,
    /// Operator-signed path: the challenge nonce (as issued) + the base64 Ed25519
    /// signature over `operator_grant_signing_payload(operator_id, node_id, nonce)`.
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    signature_b64: Option<String>,
}

/// #412 / ADR-0013 — a governor-routed authenticated EMERGENCY-STOP request. The
/// operator signs `operator_stop_signing_payload(operator_id, node_id, nonce)`
/// (domain-distinct from a clearance grant, so the two verbs are not
/// interchangeable). Unlike the RECORD-ONLY clearance grant, accepting this
/// REQUEST drives the governor to command the MRC (sticky fleet LockedOut) under
/// its own authority — the console never touches the actuator.
#[derive(Deserialize)]
struct OperatorStopRequest {
    node_id: String,
    operator_id: String,
    nonce: String,
    signature_b64: String,
}


/// Assembles the complete production router from a fully-initialized
/// `ServiceState`. Extracted verbatim from `main()` (issue #72) so the EXACT
/// assembled router can be exercised by the binary-internal posture-gate test
/// below — not a representative stand-in.
///
/// SECURITY-CRITICAL: the route groups, the per-group auth/envelope layers, and
/// especially the OUTERMOST `enforce_posture_routing` gate must remain in this
/// exact order. The posture gate is the last `.layer(...)` so it runs FIRST on
/// every request (before auth and the actuator envelope); the identity/admin/
/// actuator-posture layering inside each group is the fail-closed security
/// boundary. Do not reorder, drop, or rewire any of this.
fn build_app(svc_state: Arc<ServiceState>) -> Router {
    let identity_gated_routes = Router::new()
        .route("/system/posture/stream", get(system_posture_stream))
        .route("/federation/reports/submit", post(submit_federated_report))
        .route("/action_filter/evaluate", post(evaluate_action_filter))
        .route("/fleet/campaigns/report", post(report_node_artifact))
        .route("/industrial/evaluate", post(evaluate_industrial_adapter))
        .route("/industrial/ethernet-ip/evaluate", post(evaluate_ethernet_ip_adapter))
        .route("/industrial/canopen/evaluate", post(evaluate_canopen_adapter))
        .route("/industrial/dnp3/evaluate", post(evaluate_dnp3_adapter))
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_client_identity))
        // WS-1 (#G7): SCOPE_INTEGRATION_EVALUATE — the admin token (break-glass) OR
        // an `integrator`-role principal. Previously admin-token-only.
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_integration_scope))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_secure_transport));

    let admin_routes = Router::new()
        .route("/attestation/register", post(register_node))
        .route("/fleet/dependencies", post(register_dependencies))
        .route("/fleet/diagnostics/report", post(handle_sensor_fault_report))
        .route("/fleet/assets/register", post(handle_register_av_asset))
        .route("/fleet/av-subsystems", get(list_av_subsystems))
        .route("/system/backup/export", post(export_backup))
        .route("/system/audit/rotate-signing-key", post(handle_audit_rotate_key))
        // WS-1 (#G7) — API principal registry. SCOPE_ADMIN (admin-only); the mint
        // returns the plaintext token exactly once.
        .route("/system/principals", post(register_api_principal_handler).get(list_api_principals_handler))
        .route("/system/principals/{principal_id}/revoke", post(revoke_api_principal_handler))
        // WS-1 (#G7) Track 1.2 — mTLS cert-principal registry. SCOPE_ADMIN; pins a
        // CA-verified client cert (by SHA-256 fingerprint) to a scoped principal.
        .route("/system/cert-principals", post(register_cert_principal_handler).get(list_cert_principals_handler))
        .route("/system/cert-principals/{principal_id}/revoke", post(revoke_cert_principal_handler))
        // WS-4 / Track 3 (Fleet Plane) — OTA governor-artifact campaign control
        // plane. SCOPE_ADMIN; each lifecycle mutation writes an R156-shaped audit
        // entry. `advance` is fail-closed on fleet posture (non-Nominal → HALT).
        .route("/system/campaigns", post(create_campaign_handler).get(list_campaigns_handler))
        .route("/system/campaigns/summary", get(campaigns_summary_handler))
        .route("/system/campaigns/{campaign_id}", get(get_campaign_handler))
        .route("/system/campaigns/{campaign_id}/arm", post(arm_campaign_handler))
        .route("/system/campaigns/{campaign_id}/advance", post(advance_campaign_handler))
        .route("/system/campaigns/{campaign_id}/halt", post(halt_campaign_handler))
        .route("/federation/controllers/register", post(register_federation_controller))
        .route("/attestation/identity/register", post(register_node_identity))
        // #314 Phase 1 — operator registry. ADMIN-gated (separate power from the
        // supervisor key); posture-exempt by the /console/ path prefix.
        .route("/console/operators", post(register_operator).get(list_operators))
        .route("/console/operators/{operator_id}/revoke", post(revoke_operator))
        .route("/fabric/assets/register", post(handle_register_fabric_asset))
        .route("/fabric/assets", get(handle_list_fabric_assets))
        .route("/fabric/state", get(handle_fabric_state))
        .route("/fabric/telemetry", get(handle_fabric_telemetry))
        .route("/fabric/telemetry/{asset_id}", get(handle_fabric_telemetry_asset))
        .route("/fabric/command/{asset_id}", post(handle_fabric_command))
        .route("/fabric/causal-log", get(handle_fabric_causal_log))
        .route("/fabric/causal-log/{entry_id}", get(handle_fabric_causal_chain))
        // #G7 slice 3 — attribution runs INNER of require_admin_token (which
        // authenticates and records the resolved principal in the request
        // extensions). Scoped to these admin state-mutation routes only: NOT the
        // actuator (high-rate control) nor the self-auditing identity-gated
        // evaluations.
        .layer(middleware::from_fn_with_state(svc_state.clone(), record_admin_action_audit))
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_admin_token))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_secure_transport));

    // WS-1 (#G7) — read-only audit-chain verification/export, carved out of the
    // admin group so an `auditor`-role principal (least privilege — NO mutation
    // rights) can reach them. SCOPE_AUDIT_READ; the admin token still qualifies
    // (Admin holds every scope). The full-state `/system/backup/export` and the
    // `rotate-signing-key` mutation stay admin-only above.
    let auditor_routes = Router::new()
        .route("/system/audit/verify", get(verify_audit_chain))
        .route("/system/audit/causal/verify", get(verify_causal_chain))
        .route("/system/audit/export", get(handle_audit_export))
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_audit_scope))
        // #G7 — same transport-security boundary as every other gated group.
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_secure_transport));

    let actuator_routes = Router::new()
        .route("/actuator/motion/command", post(handle_actuator_motion_command))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            enforce_actuator_safety_envelope,
        ))
        // WS-1 (#G7): SCOPE_ACTUATOR_COMMAND — the admin token OR an `operator`-role
        // principal. Auth runs before the envelope; the transport gate runs first of all.
        .layer(middleware::from_fn_with_state(Arc::clone(&svc_state), require_actuator_scope))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_secure_transport));

    let attestation_routes = Router::new()
        .route("/attestation/challenge/{node_id}", post(issue_challenge))
        .route("/attestation/verify", post(verify_attestation))
        // #G7 (Copilot #805) — the challenge/verify flow exchanges attestation
        // nonces + signatures; when secure transport is required they must not be
        // processed off a plaintext leg either, even though the flow is otherwise
        // unauthenticated (the challenge-response provides its own guarantee).
        .layer(middleware::from_fn_with_state(svc_state.clone(), require_secure_transport));

    let probe_routes = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        // WS-0.5 — Prometheus fleet-safety series. Public read-only;
        // posture-exempt (pre-allowlisted in `is_posture_exempt`) so the
        // scrape survives LockedOut.
        .route("/metrics", get(metrics_endpoint));

    let read_routes = Router::new()
        .route("/attestation/status/{node_id}", get(get_node_status))
        .route("/fleet/posture", get(get_fleet_posture))
        .route("/fleet/posture/{node_id}", get(get_node_posture))
        .route("/fleet/history/{node_id}", get(get_node_history))
        .route("/fleet/flapping/{node_id}", get(get_node_flap_status))
        // WS-4 / Track 3 — node-facing OTA artifact assignment (which signed
        // governor artifact this node should run under the active campaigns).
        // Public read-only + posture-gated: denied under LockedOut (no artifact
        // adoption while the fleet is locked out).
        .route(
            "/fleet/campaigns/assignment/{node_id}",
            get(get_node_campaign_assignment),
        )
        .route("/federation/reports/{asset_id}", get(get_federated_reports));

    // #696 (HT2): origins are restrictable to a configured allowlist via
    // `KIRRA_CORS_ALLOWED_ORIGINS` (comma-separated). Default is `Any`
    // (back-compat); auth is `Authorization: Bearer` (no cookies / no
    // `allow_credentials`), so `Any` is not a CSRF vector — the allowlist is
    // defense-in-depth controlling which web origins may READ responses. A set
    // env with no parseable origin yields an empty allowlist (deny cross-origin),
    // logged — fail-closed rather than silently reverting to permissive.
    let cors = {
        let base = CorsLayer::new().allow_methods(Any).allow_headers(Any);
        match std::env::var("KIRRA_CORS_ALLOWED_ORIGINS") {
            Ok(v) if !v.trim().is_empty() => {
                // Partition rather than silently dropping: an UNPARSEABLE token is a
                // likely typo, and silently discarding it would let a misconfigured
                // allowlist look healthy in production. Collect the rejects and log
                // them explicitly so the misconfiguration is visible (Copilot #710).
                let mut origins: Vec<axum::http::HeaderValue> = Vec::new();
                let mut invalid: Vec<&str> = Vec::new();
                for tok in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    match tok.parse::<axum::http::HeaderValue>() {
                        Ok(hv) => origins.push(hv),
                        Err(_) => invalid.push(tok),
                    }
                }
                if !invalid.is_empty() {
                    tracing::warn!(
                        invalid = ?invalid,
                        accepted = origins.len(),
                        "KIRRA_CORS_ALLOWED_ORIGINS contained unparseable origin(s) — \
                         dropped (likely a typo); the remaining origins are enforced"
                    );
                }
                if origins.is_empty() {
                    tracing::error!(
                        value = %v,
                        "KIRRA_CORS_ALLOWED_ORIGINS set but no valid origin parsed — \
                         denying all cross-origin requests (fail-closed)"
                    );
                }
                base.allow_origin(origins)
            }
            // Unset / empty → permissive default (unchanged behaviour).
            _ => base.allow_origin(Any),
        }
    };

    // Operator console — Phase A (#103 SG6). Reads are QM; the one mutation
    // (clearance-grant recording) is gated by the supervisor key IN the handler,
    // so this group carries no auth layer. The whole `/console` plane is
    // posture-exempt (gateway::policy_layer::is_posture_exempt) so it stays
    // reachable during LockedOut — the posture it exists to recover from.
    let console_routes = Router::new()
        .route("/console", get(console_html))
        .route("/console/fleet", get(console_fleet))
        .route("/console/audit", get(console_audit))
        .route("/console/escalations", get(console_escalations))
        // #394 live console — public read-only observability views (no auth
        // layer, posture-exempt via `/console/` prefix). Mirror `console_audit`
        // fail-closed shape: store-lock poison / query error → 500 json error.
        .route("/console/runtime", get(console_runtime))
        .route("/console/analytics", get(console_analytics))
        .route("/console/sites", get(console_sites))
        .route("/console/versions", get(console_versions))
        // WS-4 / Track 3 — public read-only OTA rollout + adoption view.
        .route("/console/campaigns", get(console_campaigns))
        // #314 Phase 1 — operator clearance-challenge (unauthenticated; the nonce
        // alone grants nothing — only a valid signature over it does).
        .route("/console/clearance-challenge", get(clearance_challenge))
        .route("/console/clearance-grants", post(console_clearance_grant))
        // #412 / ADR-0013 — governor-routed authenticated emergency-stop REQUEST
        // (the clearance verb inverted). Operator-signed over the same challenge
        // nonce; accepting it makes the GOVERNOR command the MRC (sticky LockedOut)
        // under its own authority — the console never touches the actuator.
        .route("/console/estop-requests", post(console_estop_request));

    // WP-03 (MGA G-10) — control-plane backpressure. TWO isolated pools so a
    // flood of API traffic cannot starve the operator console (the LockedOut
    // recovery surface: clearance grants + the ADR-0013 e-stop request), and a
    // console flood cannot starve the API. Probe routes (`/health`, `/ready`,
    // `/metrics`) are EXEMPT — liveness and the Prometheus scrape must survive
    // overload exactly as they survive LockedOut. The posture gate below stays
    // outermost on everything, probes included (it has its own exempt list).
    let api_max = env_limit_or("KIRRA_HTTP_MAX_CONCURRENCY", 512);
    let console_max = env_limit_or("KIRRA_HTTP_CONSOLE_MAX_CONCURRENCY", 64);
    let body_max = env_limit_or("KIRRA_HTTP_MAX_BODY_BYTES", 256 * 1024);

    let api_routes = with_backpressure(
        Router::new()
            .merge(identity_gated_routes)
            .merge(admin_routes)
            .merge(auditor_routes)
            .merge(actuator_routes)
            .merge(attestation_routes)
            .merge(read_routes),
        api_max,
        body_max,
    );
    let console_routes = with_backpressure(console_routes, console_max, body_max);

    Router::new()
        .merge(probe_routes)
        .merge(api_routes)
        .merge(console_routes)
        .with_state(svc_state.clone())
        .layer(cors)
        // Outermost GATE: command-classification + posture-routing gate.
        // Runs BEFORE auth and the actuator envelope on every request;
        // is_posture_exempt allowlists liveness / observability paths so
        // probes stay reachable regardless of fleet posture. Returns 503
        // SERVICE_UNAVAILABLE on denial (transient server-state condition,
        // retryable once posture recovers).
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            enforce_posture_routing,
        ))
        // WP-05: request observability wraps EVERYTHING, the posture gate
        // included, so denials and sheds are observed too. It makes NO
        // admission decision — the posture gate above remains the outermost
        // *gate*; this layer only stamps a request id, opens the tracing
        // span, and records the latency histogram.
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            request_observability,
        ))
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM — the safe-stop / shutdown signals.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

// SG-008 / CERT-003 RTM coverage tests (the `sg_008_cert_tests` module) live
// alongside the predicate they certify in the `startup` submodule.

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
#[cfg(test)]
mod posture_gate_real_router_tests {
    use super::build_app;

    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    use kirra_verifier::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
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
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
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
        build_app(svc)
            .oneshot(req)
            .await
            .expect("router service should not panic")
            .status()
    }

    // --- OTA campaign control-plane (WS-4 / Track 3 · Fleet Plane) ---------

    const CAMPAIGN_DIGEST: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
            .route("/system/campaigns/summary", get(super::campaigns_summary_handler))
            // The node adoption report (bare here for logic testing; its identity gate
            // is proven by `ws1_scope_gated_routes_fail_closed_on_real_router`).
            .route("/fleet/campaigns/report", post(super::report_node_artifact))
            .route("/system/campaigns/{campaign_id}", get(super::get_campaign_handler))
            .route("/system/campaigns/{campaign_id}/arm", post(super::arm_campaign_handler))
            .route(
                "/system/campaigns/{campaign_id}/advance",
                post(super::advance_campaign_handler),
            )
            .route("/system/campaigns/{campaign_id}/halt", post(super::halt_campaign_handler))
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
            let status =
                status_through_real_app(state_with(FleetPosture::Nominal), method, path).await;
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

        let (st, j) =
            campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-e2e")))
                .await;
        assert_eq!(st, StatusCode::CREATED, "create; body={j}");
        assert_eq!(j["state"], "draft");
        assert_eq!(j["rollout_percent"], 0);

        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-e2e/arm", None).await;
        assert_eq!(st, StatusCode::OK, "arm");

        // Advance under Nominal → first stage (10%).
        let (st, j) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-e2e/advance", None).await;
        assert_eq!(st, StatusCode::OK, "advance; body={j}");
        assert_eq!(j["outcome"]["advanced"], true);
        assert_eq!(j["outcome"]["rollout_percent"], 10);
        assert_eq!(j["campaign"]["state"], "rolling");

        // The fleet regresses to LockedOut — flip the posture cache on the SAME svc
        // (store, and therefore the campaign, intact).
        *svc.posture_cache.write().expect("posture cache") =
            Some(CachedFleetPosture::new(FleetPosture::LockedOut));

        // The next advance HALTS fail-closed rather than rolling to 50%.
        let (st, j) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-e2e/advance", None).await;
        assert_eq!(st, StatusCode::OK, "halt-advance; body={j}");
        assert_eq!(j["outcome"]["halted"], true);
        assert_eq!(j["outcome"]["halt_reason"], "posture_locked_out");
        assert_eq!(j["campaign"]["state"], "halted");
        // Rollout never advanced past the last safe stage.
        assert_eq!(j["campaign"]["rollout_percent"], 10);

        // Terminal: a further advance is a 409 conflict (the engine authors no resume).
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-e2e/advance", None).await;
        assert_eq!(st, StatusCode::CONFLICT, "terminal campaign must reject advance");
    }

    /// The fleet rollout summary reflects real campaign state through the HTTP
    /// handler: counts by state, active-campaign stage progress, and a halted
    /// campaign surfaced with its reason.
    #[tokio::test]
    async fn campaign_summary_reflects_fleet_state_end_to_end() {
        let svc = state_with(FleetPosture::Nominal);

        // camp-roll: create → arm → advance → Rolling @ 10% (stage 1 of 3).
        campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-roll")))
            .await;
        campaign_req(svc.clone(), "POST", "/system/campaigns/camp-roll/arm", None).await;
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-roll/advance", None).await;
        assert_eq!(st, StatusCode::OK);

        // camp-draft: left in Draft. camp-halt: armed then operator-halted (halt is
        // only legal from an active state).
        campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-draft")))
            .await;
        campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-halt")))
            .await;
        campaign_req(svc.clone(), "POST", "/system/campaigns/camp-halt/arm", None).await;
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-halt/halt", None).await;
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
        campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-adopt")))
            .await;
        campaign_req(svc.clone(), "POST", "/system/campaigns/camp-adopt/arm", None).await;
        campaign_req(svc.clone(), "POST", "/system/campaigns/camp-adopt/advance", None).await;

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
        assert_eq!(st, StatusCode::OK, "summary must resolve, not 404 as a missing id");
        // A summary body has the count fields; a campaign body would not.
        assert!(j.get("total").is_some(), "got a summary, not a campaign; body={j}");
    }

    /// Fail-closed on posture UNAVAILABILITY: with a cold posture cache
    /// (`gate_posture` resolves empty/stale → LockedOut), the very first advance
    /// halts — a rollout never proceeds when fleet posture cannot be confirmed.
    #[tokio::test]
    async fn campaign_advance_fails_closed_when_posture_unavailable() {
        let svc = build_state(None); // cold cache

        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("camp-cold")))
                .await;
        assert_eq!(st, StatusCode::CREATED);
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-cold/arm", None).await;
        assert_eq!(st, StatusCode::OK);

        let (st, j) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/camp-cold/advance", None).await;
        assert_eq!(st, StatusCode::OK, "advance; body={j}");
        assert_eq!(j["outcome"]["halted"], true, "cold posture must halt, not roll");
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
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("dup"))).await;
        assert_eq!(st, StatusCode::CREATED);
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns", Some(&create_body("dup"))).await;
        assert_eq!(st, StatusCode::CONFLICT);

        // Advancing an unknown campaign → 404.
        let (st, _) =
            campaign_req(svc.clone(), "POST", "/system/campaigns/ghost/advance", None).await;
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
        let resp = build_app(svc)
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
        assert_eq!(st, StatusCode::OK, "public assignment read must be reachable; body={j}");
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

        async fn post_actuator(
            svc: Arc<ServiceState>,
            bearer: Option<&str>,
            body: &str,
        ) -> StatusCode {
            let mut rb = Request::builder()
                .method("POST")
                .uri("/actuator/motion/command")
                .header("content-type", "application/json");
            if let Some(tok) = bearer {
                rb = rb.header(header::AUTHORIZATION, format!("Bearer {tok}"));
            }
            build_app(svc)
                .oneshot(rb.body(Body::from(body.to_string())).expect("build request"))
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
        ];
        for (method, path) in cases {
            let status =
                status_through_real_app(state_with(FleetPosture::Nominal), method, path).await;
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
        let denied =
            status_through_real_app(Arc::clone(&svc), "GET", "/fleet/posture").await;
        assert_eq!(
            denied,
            StatusCode::SERVICE_UNAVAILABLE,
            "precondition: LockedOut denies the functional read"
        );

        let resp = build_app(Arc::clone(&svc))
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
            text.lines().any(|l| l.starts_with("kirra_gate_denials_total{")
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
}

// ---------------------------------------------------------------------------
// #88: verifier→fabric posture feed (single-local-asset model).
//
// Exercises `sync_local_asset_posture` directly (the env-gated spawn wrapper
// is thin): a registered local asset's fabric posture must track the cached
// fleet posture, fail closed on a stale cache, avoid churn when unchanged,
// and run the bounded cross-asset propagation pass.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod fabric_posture_feed_tests {
    use super::sync_local_asset_posture;

    use std::sync::Arc;

    use kirra_verifier::fabric::asset::{
        AssetType, FabricAsset, KinematicProfileType,
    };
    use kirra_verifier::fabric::router::FabricRouter;
    use kirra_verifier::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    const LOCAL: &str = "local-asset";

    fn asset(id: &str) -> FabricAsset {
        let now = now_ms();
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::RobotNominal,
            registered_at_ms: now,
            last_seen_ms: now,
            metadata: Default::default(),
        }
    }

    /// Builds an Active `ServiceState` whose cache holds `cached` and whose
    /// fabric router has `LOCAL` registered (seeded Degraded).
    fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
        let fabric_router = Arc::new(FabricRouter::new());
        fabric_router.register_asset(&asset(LOCAL));
        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    /// A FRESH cache entry (generated now) carrying `posture`.
    fn fresh(posture: FleetPosture) -> CachedFleetPosture {
        CachedFleetPosture::new_with_generation(posture, 1, now_ms())
    }

    #[test]
    fn fresh_cache_pushes_fleet_posture_to_local_asset() {
        let svc = state(Some(fresh(FleetPosture::Nominal)));
        // Seeded Degraded by register_asset.
        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded
        );

        sync_local_asset_posture(&svc, LOCAL);

        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::Nominal, "feed must mirror the fleet posture");
        assert!(after.blocked_by.is_empty(), "Nominal carries no blocked_by reason");
        assert_eq!(after.generation, 1, "seed gen 0 → first feed write gen 1");
    }

    #[test]
    fn stale_cache_does_not_push_keeps_last_good() {
        // generated_at far in the past → is_stale(now) == true.
        let stale = CachedFleetPosture::new_with_generation(
            FleetPosture::Nominal,
            7,
            now_ms().saturating_sub(60_000),
        );
        let svc = state(Some(stale));

        sync_local_asset_posture(&svc, LOCAL);

        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded,
            "a stale cache must NOT propagate forward (fail-closed): seed is kept"
        );
    }

    #[test]
    fn empty_cache_does_not_push() {
        let svc = state(None);
        sync_local_asset_posture(&svc, LOCAL);
        assert_eq!(
            svc.fabric_router.asset_posture(LOCAL).unwrap().posture,
            FleetPosture::Degraded,
            "a not-yet-computed cache must not overwrite the seed"
        );
    }

    #[test]
    fn unchanged_posture_does_not_bump_generation() {
        // Seed is Degraded; feeding Degraded again must be a no-op.
        let svc = state(Some(fresh(FleetPosture::Degraded)));
        let gen_before = svc.fabric_router.asset_posture(LOCAL).unwrap().generation;

        sync_local_asset_posture(&svc, LOCAL);

        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::Degraded);
        assert_eq!(
            after.generation, gen_before,
            "an unchanged posture must not bump the generation (no churn)"
        );
    }

    #[test]
    fn lockedout_fleet_posture_locks_the_local_asset() {
        let svc = state(Some(fresh(FleetPosture::LockedOut)));
        sync_local_asset_posture(&svc, LOCAL);
        let after = svc.fabric_router.asset_posture(LOCAL).unwrap();
        assert_eq!(after.posture, FleetPosture::LockedOut);
        assert_eq!(
            after.blocked_by,
            vec!["VERIFIER_FLEET_POSTURE_LOCKED_OUT".to_string()],
            "LockedOut feed must tag the reason for operators"
        );
    }
}

// ---------------------------------------------------------------------------
// #86 — the fabric command endpoint is AUTHORITATIVE: it applies the clamp
// server-side and returns the ENFORCED command (closing the prior fail-open
// where a clamp was reported but not applied). These tests drive the handler
// directly (no auth/router), asserting the response `command` carries the safe
// values, that a clamp is reported, Allow is unchanged, and Deny is denied.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod fabric_command_authoritative_tests {
    use super::handle_fabric_command;

    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::Json;

    use kirra_verifier::fabric::asset::{
        AssetPosture, AssetType, FabricAsset, KinematicProfileType,
    };
    use kirra_verifier::fabric::router::FabricRouter;
    use kirra_verifier::gateway::kinematics_contract::ProposedVehicleCommand;
    use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    const ASSET: &str = "av-01";

    fn svc_with_asset(posture: FleetPosture) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        let fabric_router = Arc::new(FabricRouter::new());

        let asset = FabricAsset {
            asset_id: ASSET.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: ASSET.to_string(),
            kinematic_profile: KinematicProfileType::AutomotiveNominal,
            registered_at_ms: 0,
            last_seen_ms: 0,
            metadata: Default::default(),
        };
        fabric_router.register_asset(&asset);
        // route_command reads the asset's fabric posture; set the one under test.
        fabric_router.update_asset_posture(
            ASSET,
            AssetPosture {
                asset_id: ASSET.to_string(),
                posture,
                generation: 1,
                computed_at_ms: 0,
                contributing_nodes: vec![],
                blocked_by: vec![],
            },
        );

        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    async fn post_command(svc: Arc<ServiceState>, cmd: ProposedVehicleCommand) -> serde_json::Value {
        let resp = handle_fabric_command(State(svc), Path(ASSET.to_string()), Ok(Json(cmd)))
            .await
            .into_response();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn cmd(linear: f64, current: f64, steering: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: linear,
            current_velocity_mps: current,
            delta_time_s: 0.1,
            steering_angle_deg: steering,
            current_steering_angle_deg: steering,
        }
    }

    #[tokio::test]
    async fn clamped_command_response_carries_enforced_values_within_envelope() {
        // 40 m/s exceeds the AutomotiveNominal envelope → ClampLinear.
        let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(40.0, 34.0, 0.0)).await;

        assert_eq!(v["allowed"], true);
        assert_eq!(v["clamp_occurred"], true, "a clamp must be reported as enforcement");
        assert_eq!(v["original_linear_velocity_mps"], 40.0);

        let enforced = v["enforced_linear_velocity_mps"].as_f64().expect("enforced velocity");
        assert!(enforced < 40.0, "enforced velocity must be clamped below the proposal (within envelope)");

        // THE KEY ASSERTION: the authoritative `command` carries the SAFE value,
        // so a client applying it is within envelope even ignoring `action`.
        let cmd_v = v["command"]["linear_velocity_mps"].as_f64().expect("command.linear");
        assert_eq!(cmd_v, enforced, "response.command must carry the enforced (clamped) velocity");
        assert!(cmd_v < 40.0, "the returned command is NOT the unclamped 40.0");
    }

    #[tokio::test]
    async fn allow_returns_command_unchanged() {
        // current == proposed → no rate-of-change clamp; within envelope → Allow.
        let v = post_command(svc_with_asset(FleetPosture::Nominal), cmd(10.0, 10.0, 1.0)).await;
        assert_eq!(v["allowed"], true);
        assert_eq!(v["clamp_occurred"], false);
        assert_eq!(v["command"]["linear_velocity_mps"].as_f64().unwrap(), 10.0);
        assert_eq!(v["command"]["steering_angle_deg"].as_f64().unwrap(), 1.0);
    }

    #[tokio::test]
    async fn lockedout_denies_and_omits_command() {
        let v = post_command(svc_with_asset(FleetPosture::LockedOut), cmd(10.0, 10.0, 0.0)).await;
        assert_eq!(v["allowed"], false, "LockedOut denies the command");
        assert!(v.get("command").is_none(), "a denied command carries no enforced command");
        assert!(v["denial_reason"].is_string(), "denial is recorded with a reason");
    }
}

// ---------------------------------------------------------------------------
// #147 — attestation nonce lifecycle: VERIFY-THEN-CONSUME at the handler.
//
// The crypto (verify_attestation_proof) and the store invariants (single-use,
// TTL, node-binding, CSPRNG) are tested in attestation.rs / verifier.rs. This
// proves the remaining handler-level invariant: a FAILED proof must NOT burn
// the pending nonce, so an attacker cannot force nonce exhaustion — the
// legitimate node can still attest with the same outstanding nonce.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod attestation_nonce_handler_tests {
    use super::{register_node, verify_attestation, RegisterNodeRequest, VerifyAttestationRequest};

    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

    use kirra_verifier::attestation::attestation_signing_payload;
    use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{
        AppState, NodeTrustState, RegisteredNode, VerifierOperationMode,
    };
    use kirra_verifier::verifier_store::VerifierStore;

    const NODE: &str = "edge-node-1";

    /// Test-only Ed25519 SubjectPublicKeyInfo PEM (RFC 8410 prefix; public key only).
    fn public_key_to_pem(vk: &VerifyingKey) -> String {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        const ED25519_SPKI_PREFIX: [u8; 12] =
            [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(vk.as_bytes());
        format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", B64.encode(&der))
    }

    fn sign_proof(sk: &SigningKey, node_id: &str, nonce: u64) -> String {
        hex::encode(sk.sign(&attestation_signing_payload(node_id, nonce)).to_bytes())
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
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
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
        verify_attestation(State(svc), Json(req)).await.into_response().status()
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
            sk.sign(&kirra_verifier::attestation::adoption_report_signing_payload(
                NODE, digest, ts,
            ))
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
        assert_eq!(attested_of(svc.clone()).await, Some(true), "stored as attested");

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
        assert_eq!(st, StatusCode::OK, "unsigned report still accepted (identity-gated)");
        assert_eq!(attested_of(svc.clone()).await, Some(false), "unsigned → not attested");

        // A signed report with a FAR-FUTURE timestamp is rejected (the monotonic
        // upsert would otherwise let it permanently wedge later legitimate updates).
        let future_ts = now_ms() + 3_600_000; // 1h ahead — beyond the skew allowance
        let future_sig = B64.encode(
            sk.sign(&kirra_verifier::attestation::adoption_report_signing_payload(
                NODE, digest, future_ts,
            ))
            .to_bytes(),
        );
        let st = report(serde_json::json!({
            "node_id": NODE, "applied_digest": digest,
            "signature": future_sig, "reported_at_ms": future_ts,
        }))
        .await
        .into_response()
        .status();
        assert_eq!(st, StatusCode::BAD_REQUEST, "far-future signed timestamp rejected");
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

    fn sign_proof_with_pcr16(sk: &SigningKey, node_id: &str, nonce: u64, presented: Option<&str>) -> String {
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
        verify_attestation(State(svc), Json(req)).await.into_response().status()
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
        assert_eq!(absent, StatusCode::FORBIDDEN, "expected PCR16, none presented → 403");
        assert!(svc.app.pending_challenges.contains_key(NODE), "a PCR16 refusal must not burn the nonce");

        // (b) A wrong digest Y (correctly signed) ≠ the expectation X → 403, preserved.
        let wrong = verify_with_pcr16(
            Arc::clone(&svc), nonce, sign_proof_with_pcr16(&node_key, NODE, nonce, Some(Y)), Some(Y),
        ).await;
        assert_eq!(wrong, StatusCode::FORBIDDEN, "mismatched PCR16 → 403");
        assert!(svc.app.pending_challenges.contains_key(NODE), "still not burned");

        // (c) The correct digest X bound into the signature → 200 OK, Trusted.
        let ok = verify_with_pcr16(
            Arc::clone(&svc), nonce, sign_proof_with_pcr16(&node_key, NODE, nonce, Some(X)), Some(X),
        ).await;
        assert_eq!(ok, StatusCode::OK, "matching bound PCR16 attests");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
            "node becomes Trusted after a valid PCR16-bound proof"
        );
    }

    // ---- Hardware TPM quote enforcement (live wiring) ---------------------

    /// The 32-byte PCR16 VALUE a quote node attests, in hex (exactly 64 chars — a
    /// real SHA-256 PCR value). The quote carries a HASH OVER this (`SHA256(value)`);
    /// the self-report proof carries the value.
    const PCR16_VALUE_HEX: &str =
        "abababababababababababababababababababababababababababababababcd";

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
        let tpm_quote = quote.map(|(q, s)| serde_json::json!({
            "quote_msg_hex": q, "signature_hex": s,
        }));
        let req: VerifyAttestationRequest = serde_json::from_value(serde_json::json!({
            "node_id": NODE, "nonce": nonce, "proof_hex": proof_hex,
            "presented_pcr16_digest_hex": presented,
            "tpm_quote": tpm_quote,
        }))
        .expect("build request");
        verify_attestation(State(svc), Json(req)).await.into_response().status()
    }

    /// A node whose policy requires a TPM quote is REFUSED when it presents only
    /// a (valid) self-reported proof and no quote — fail-closed, nonce preserved.
    #[tokio::test]
    async fn tpm_quote_required_but_absent_is_refused() {
        let node_key = SigningKey::from_bytes(&[7u8; 32]);
        let svc = svc_with_quote_node(public_key_to_pem(&node_key.verifying_key()), PCR16_VALUE_HEX);
        let nonce = 0x1122_3344_5566_7788;
        svc.app.issue_challenge(NODE, nonce, now_ms());

        let status = verify_with_quote(
            Arc::clone(&svc),
            nonce,
            sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
            Some(PCR16_VALUE_HEX),
            None, // no quote, but policy requires one
        ).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "policy requires a quote, none presented → 403");
        assert!(svc.app.pending_challenges.contains_key(NODE), "a quote refusal must not burn the nonce");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Unknown),
            "node is not trusted without the required quote"
        );
    }

    /// A valid TPM quote (correct nonce + PCR16 digest, AK-signed) attests the
    /// node → 200 OK, Trusted.
    #[tokio::test]
    async fn tpm_quote_valid_attests_node_trusted() {
        let node_key = SigningKey::from_bytes(&[7u8; 32]);
        let svc = svc_with_quote_node(public_key_to_pem(&node_key.verifying_key()), PCR16_VALUE_HEX);
        let nonce = 0x1122_3344_5566_7788;
        svc.app.issue_challenge(NODE, nonce, now_ms());

        let status = verify_with_quote(
            Arc::clone(&svc),
            nonce,
            sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
            Some(PCR16_VALUE_HEX),
            Some(quote_evidence(&node_key, nonce, PCR16_VALUE_HEX)),
        ).await;
        assert_eq!(status, StatusCode::OK, "valid quote attests");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
            "node becomes Trusted after a valid hardware quote"
        );
    }

    /// A quote signed by the WRONG key is refused (401) and the nonce is NOT
    /// burned, so the node can retry with a genuine quote.
    #[tokio::test]
    async fn tpm_quote_invalid_is_refused_and_nonce_preserved() {
        let node_key = SigningKey::from_bytes(&[7u8; 32]);
        let attacker = SigningKey::from_bytes(&[9u8; 32]); // not the registered AK
        let svc = svc_with_quote_node(public_key_to_pem(&node_key.verifying_key()), PCR16_VALUE_HEX);
        let nonce = 0x1122_3344_5566_7788;
        svc.app.issue_challenge(NODE, nonce, now_ms());

        // The base proof is genuine (node_key); only the QUOTE is forged.
        let status = verify_with_quote(
            Arc::clone(&svc),
            nonce,
            sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
            Some(PCR16_VALUE_HEX),
            Some(quote_evidence(&attacker, nonce, PCR16_VALUE_HEX)),
        ).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "quote signed by the wrong key → 401");
        assert!(svc.app.pending_challenges.contains_key(NODE), "an invalid quote must not burn the nonce");
    }

    /// A node with NO quote policy is unaffected: the self-report path attests
    /// without any quote (back-compat). Also proves a presented quote is still
    /// rejected if it does not verify, even when the policy does not require one.
    #[tokio::test]
    async fn tpm_quote_policy_absent_is_back_compat() {
        let node_key = SigningKey::from_bytes(&[7u8; 32]);
        // svc_with_pcr16_node sets NO attestation policy → require_tpm_quote=false.
        let svc = svc_with_pcr16_node(public_key_to_pem(&node_key.verifying_key()), PCR16_VALUE_HEX);
        let nonce = 0x1122_3344_5566_7788;
        svc.app.issue_challenge(NODE, nonce, now_ms());

        let status = verify_with_pcr16(
            Arc::clone(&svc),
            nonce,
            sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
            Some(PCR16_VALUE_HEX),
        ).await;
        assert_eq!(status, StatusCode::OK, "no quote policy → self-report path still attests");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
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
            assert!(super::require_tpm_quote_fleet_default(Some(on)), "{on:?} must enable");
        }
        for off in [None, Some(""), Some("0"), Some("false"), Some("yes"), Some("2"), Some("on")] {
            assert!(!super::require_tpm_quote_fleet_default(off), "{off:?} must stay off");
        }
    }

    /// The pure resolver: an EXPLICIT request field always wins; an OMITTED field
    /// (`None`) defers to the fleet default. This is the whole WP-16 policy.
    #[test]
    fn resolve_require_tpm_quote_explicit_wins_else_default() {
        assert!(super::resolve_require_tpm_quote(Some(true), false), "explicit true wins over off");
        assert!(!super::resolve_require_tpm_quote(Some(false), true), "explicit false opts out under an on default");
        assert!(super::resolve_require_tpm_quote(None, true), "omitted defers to an on default");
        assert!(!super::resolve_require_tpm_quote(None, false), "omitted under off default stays off (back-compat)");
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
        let status = register_node(State(Arc::clone(svc)), Json(req)).await.into_response().status();
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
        assert!(super::is_valid_pcr16_sha256_hex(&"ab".repeat(32)), "64 hex chars ok");
        assert!(super::is_valid_pcr16_sha256_hex(&format!("  {}  ", "cd".repeat(32))), "trimmed");
        assert!(!super::is_valid_pcr16_sha256_hex(&"ab".repeat(31)), "62 chars rejected");
        assert!(!super::is_valid_pcr16_sha256_hex(&"ab".repeat(33)), "66 chars rejected");
        assert!(!super::is_valid_pcr16_sha256_hex(""), "empty rejected");
        assert!(!super::is_valid_pcr16_sha256_hex(&format!("xy{}", "ab".repeat(31))), "non-hex rejected");
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
        let s = register_node(State(Arc::clone(&svc)), Json(no_material)).await.into_response().status();
        assert_eq!(s, StatusCode::BAD_REQUEST, "quote-required with no AK/PCR16 → 400");

        // require=true, AK present but PCR16 the wrong length → 400.
        let bad_pcr: RegisterNodeRequest = serde_json::from_value(serde_json::json!({
            "node_id": "n2", "require_tpm_quote": true,
            "ak_public_pem": ak, "expected_pcr16_digest_hex": "abab",
        }))
        .unwrap();
        let s = register_node(State(Arc::clone(&svc)), Json(bad_pcr)).await.into_response().status();
        assert_eq!(s, StatusCode::BAD_REQUEST, "a non-64-hex PCR16 under require_tpm_quote → 400");

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
        let status = register_node(State(Arc::clone(&svc)), Json(req)).await.into_response().status();
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
        assert_eq!(no_quote, StatusCode::FORBIDDEN, "an enrolled node must present a quote");
        assert!(svc.app.pending_challenges.contains_key(NODE), "the refusal preserves the nonce");

        // 3. A genuine TPM quote attests → Trusted.
        let ok = verify_with_quote(
            Arc::clone(&svc),
            nonce,
            sign_proof_with_pcr16(&node_key, NODE, nonce, Some(PCR16_VALUE_HEX)),
            Some(PCR16_VALUE_HEX),
            Some(quote_evidence(&node_key, nonce, PCR16_VALUE_HEX)),
        )
        .await;
        assert_eq!(ok, StatusCode::OK, "a valid quote attests the enrolled node");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
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
        let bad = verify(Arc::clone(&svc), nonce, sign_proof(&attacker_key, NODE, nonce)).await;
        assert_eq!(bad, StatusCode::UNAUTHORIZED, "a bad proof is refused");
        assert!(
            svc.app.pending_challenges.contains_key(NODE),
            "a FAILED proof must not burn the pending nonce"
        );

        // 2) The legitimate node, with the SAME outstanding nonce, now succeeds.
        let good = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
        assert_eq!(good, StatusCode::OK, "valid proof over the still-outstanding nonce attests");
        assert!(
            matches!(svc.app.nodes.get(NODE).unwrap().status, NodeTrustState::Trusted),
            "node becomes Trusted after a valid proof"
        );

        // 3) Single-use: the nonce is now consumed; a replay is a 409 conflict.
        let replay = verify(Arc::clone(&svc), nonce, sign_proof(&node_key, NODE, nonce)).await;
        assert_eq!(replay, StatusCode::CONFLICT, "the consumed nonce cannot be replayed");
    }
}

// ---------------------------------------------------------------------------
// #88 tightening — the LOCAL fed asset is seeded fail-closed LockedOut; PEERS
// keep the Degraded interim seed; the feed lifts the local asset on recalc.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod local_asset_lockedout_seed_tests {
    use super::{seed_local_asset_lockedout_inner, sync_local_asset_posture};

    use std::sync::Arc;

    use kirra_verifier::fabric::asset::{AssetType, FabricAsset, KinematicProfileType};
    use kirra_verifier::fabric::router::FabricRouter;
    use kirra_verifier::posture_cache::{
        now_ms, CachedFleetPosture, ServiceState, SharedPostureCache,
    };
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    const LOCAL: &str = "av-local";
    const PEER: &str = "av-peer";

    fn asset(id: &str) -> FabricAsset {
        let now = now_ms();
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::RobotNominal,
            registered_at_ms: now,
            last_seen_ms: now,
            metadata: Default::default(),
        }
    }

    /// ServiceState with LOCAL and PEER registered (both seeded Degraded by
    /// `register_asset`), and `cached` as the fleet posture cache.
    fn state(cached: Option<CachedFleetPosture>) -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(cached));
        let fabric_router = Arc::new(FabricRouter::new());
        fabric_router.register_asset(&asset(LOCAL));
        fabric_router.register_asset(&asset(PEER));
        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router,
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    fn posture_of(svc: &ServiceState, id: &str) -> FleetPosture {
        svc.fabric_router.asset_posture(id).expect("asset registered").posture
    }

    #[test]
    fn local_asset_seeded_lockedout_peer_stays_degraded() {
        let svc = state(None);
        // register_asset seeds BOTH Degraded.
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::Degraded);
        assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);

        // The seed runs once per registered id with LOCAL configured.
        seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
        seed_local_asset_lockedout_inner(&svc, PEER, Some(LOCAL));

        assert_eq!(
            posture_of(&svc, LOCAL),
            FleetPosture::LockedOut,
            "the configured local asset is fail-closed LockedOut"
        );
        assert_eq!(
            posture_of(&svc, PEER),
            FleetPosture::Degraded,
            "peers keep the documented Degraded interim seed"
        );
    }

    #[test]
    fn unset_local_id_leaves_degraded_seed_unchanged() {
        let svc = state(None);
        seed_local_asset_lockedout_inner(&svc, LOCAL, None);
        seed_local_asset_lockedout_inner(&svc, PEER, None);
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::Degraded, "unset → no local asset to special-case");
        assert_eq!(posture_of(&svc, PEER), FleetPosture::Degraded);
    }

    #[test]
    fn feed_lifts_lockedout_local_asset_on_recalc() {
        // Fresh Nominal fleet posture in the cache (as after the first Active recalc).
        let svc = state(Some(CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 1, now_ms())));
        seed_local_asset_lockedout_inner(&svc, LOCAL, Some(LOCAL));
        assert_eq!(posture_of(&svc, LOCAL), FleetPosture::LockedOut, "starts fail-closed LockedOut");

        // The feed lifts it to the real fleet posture.
        sync_local_asset_posture(&svc, LOCAL);
        assert_eq!(
            posture_of(&svc, LOCAL),
            FleetPosture::Nominal,
            "the feed lifts the local asset out of LockedOut on recalc"
        );
    }
}

// ---------------------------------------------------------------------------
// SG-012 / H-011 — DNP3 broadcast mandatory audit (TR-012 / TR-012a / TR-012b).
// A broadcast control MUST carry a tamper-evident record; if the mandatory
// audit write fails, the command is BLOCKED (fail-closed). Unicast audit
// failure is non-fatal. The store mutex is poisoned to simulate audit failure.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod dnp3_mandatory_audit_tests {
    use super::{evaluate_dnp3_adapter, ReplayGuarded};

    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;

    use kirra_verifier::adapters::dnp3::{Dnp3Message, Dnp3Object, DNP3_BROADCAST_ADDRESS};
    use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    fn svc() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        // Fresh Nominal posture so the gate admits the command (we test the
        // AUDIT mechanism, not the posture gate).
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));
        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    /// CROB control message (function 0x05 Direct_Operate + Group 12) to `dest`.
    fn control_msg(dest: u16) -> Dnp3Message {
        Dnp3Message {
            source_address: 0x0001,
            dest_address: dest,
            function_code: 0x05,
            data_link_control: 0,
            objects: vec![Dnp3Object { group: 12, variation: 1, data: vec![] }],
            source_node: "substation_01".to_string(),
        }
    }

    /// Poison the underlying store mutex by panicking inside a `StoreHandle::with`
    /// closure. NOTE (DB-actor migration phase 1): `StoreHandle` RECOVERS a poisoned
    /// lock internally (`into_inner`), so this no longer makes subsequent store
    /// access fail — it only exercises that the handle keeps working after a
    /// panicking holder. The former fail-closed-on-poison replay arm is gone with
    /// the bare-mutex; see the two tests below.
    fn poison_store(svc: &ServiceState) {
        let store = svc.app.store.clone();
        let _ = std::thread::spawn(move || {
            store.with(|_s| panic!("intentionally poisoning the store mutex for the audit-failure test"));
        })
        .join();
    }

    async fn post(svc: Arc<ServiceState>, msg: Dnp3Message) -> (StatusCode, serde_json::Value) {
        // Wrap with fresh replay metadata (seq 1 on a fresh per-test store, current
        // timestamp) so the replay/freshness gate admits and we reach the audit path.
        let guarded = ReplayGuarded { sequence: 1, timestamp_ms: now_ms(), message: msg };
        let resp = evaluate_dnp3_adapter(State(svc), Ok(Json(guarded))).await.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        (status, serde_json::from_slice(&bytes).expect("json body"))
    }

    #[tokio::test]
    async fn test_dnp3_broadcast_always_audited() {
        let svc = svc();
        let (status, v) = post(Arc::clone(&svc), control_msg(DNP3_BROADCAST_ADDRESS)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["allowed"], true, "broadcast admitted in Nominal");
        assert_eq!(v["adapter_details"]["is_broadcast"], true);
        // The mandatory audit entry was written to the tamper-evident log.
        let n = svc.app.store
            .with(|store| store.count_recent_posture_events("dnp3_adapter", 0)).unwrap();
        assert!(n >= 1, "a broadcast must always produce an audit entry, got {n}");
    }

    // NOTE on TR-012a/b interaction with the replay gate: the replay/freshness gate
    // is a PRIMARY security control that runs BEFORE evaluation and needs the store,
    // so it fail-closes (blocks) when the store is unavailable. A fully-poisoned
    // store therefore now blocks at the replay gate, ahead of the TR-012a/b audit
    // logic. The TR-012a "broadcast blocked when its mandatory audit write fails" and
    // TR-012b "unicast audit-write failure is non-fatal" branches still exist in the
    // handler and apply once the replay gate has PASSED (healthy store, failing audit
    // write). The broadcast-IS-audited path (healthy store) is covered by
    // `test_dnp3_broadcast_always_audited` above.

    // DB-actor migration phase 1: `StoreHandle` recovers a poisoned lock
    // internally, so a one-off panicking holder no longer wedges the store. The
    // replay gate therefore RUNS normally after a poison (rather than emitting the
    // old `INDUSTRIAL_REPLAY_STORE_POISONED` fail-closed reason, which is gone with
    // the bare mutex). These tests pin the new recovery behavior: a broadcast/
    // unicast control still evaluates after a transient poison.
    #[tokio::test]
    async fn test_store_recovers_after_poison_broadcast_still_evaluates() {
        let svc = svc();
        poison_store(&svc); // the handle recovers the poison; the gate runs normally
        let (status, v) = post(Arc::clone(&svc), control_msg(DNP3_BROADCAST_ADDRESS)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["allowed"], true, "a recovered store evaluates the command normally (Nominal)");
        assert_eq!(v["adapter_details"]["is_broadcast"], true);
    }

    #[tokio::test]
    async fn test_store_recovers_after_poison_unicast_still_evaluates() {
        let svc = svc();
        poison_store(&svc);
        let (status, v) = post(Arc::clone(&svc), control_msg(0x0005)).await;
        assert_eq!(status, StatusCode::OK);
        // Nominal posture admits the unicast control once the handle recovers.
        assert_eq!(v["allowed"], true, "a recovered store evaluates the unicast command normally");
    }
}

// ---------------------------------------------------------------------------
// #46 — systemd sd_notify / watchdog wiring.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod systemd_notify_tests {
    use super::{sd_notify_to, watchdog_should_ping};
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn watchdog_pings_active_only_when_cache_fresh() {
        // Active liveness is gated on a fresh posture cache (engine-liveness).
        assert!(watchdog_should_ping(true, true), "Active + fresh cache → ping");
        assert!(!watchdog_should_ping(true, false), "Active + STALE cache → withhold (fail-closed)");
        // PassiveStandby has no posture engine by design → plain keepalive.
        assert!(watchdog_should_ping(false, false), "PassiveStandby → keepalive ping");
        assert!(watchdog_should_ping(false, true), "PassiveStandby → keepalive ping");
    }

    #[test]
    fn sd_notify_to_delivers_the_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notify.sock");
        let listener = UnixDatagram::bind(&path).expect("bind notify socket");
        sd_notify_to(path.as_os_str(), "READY=1").expect("sd_notify_to send");
        let mut buf = [0u8; 64];
        let n = listener.recv(&mut buf).expect("recv");
        assert_eq!(&buf[..n], b"READY=1", "the exact sd_notify datagram must be delivered");
    }
}

// ===========================================================================
// Operator console — Phase A tests (#103 SG6).
// ===========================================================================
#[cfg(test)]
mod console_phase_a_tests {
    use super::{
        admin_token_fingerprint, build_app, composite_challenge_key, register_operator,
        supervisor_key_ok, valid_identifier, RegisterOperatorRequest,
    };

    use serde_json::json;
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::extract::{Json, State};
    use axum::http::{header, HeaderMap, Request, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt; // oneshot

    use kirra_verifier::posture_cache::{
        now_ms, ServiceState, SharedPostureCache, POSTURE_CACHE_TTL_MS,
    };
    use kirra_verifier::verifier::{
        AppState, NodeTrustState, RegisteredNode, VerifierOperationMode,
    };
    use kirra_verifier::verifier_store::VerifierStore;

    fn build_state() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
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

    fn seed_node(svc: &Arc<ServiceState>, node_id: &str) {
        let node = RegisteredNode {
            node_id: node_id.to_string(),
            status: NodeTrustState::Untrusted("post-collision latch".to_string()),
            registered_at_ms: 1,
            last_trust_update_ms: 1_700_000_000_000,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        };
        svc.app.persist_and_insert_node(node).expect("seed node");
    }

    async fn get(svc: Arc<ServiceState>, path: &str) -> (StatusCode, String) {
        let resp = build_app(svc)
            .oneshot(Request::builder().method("GET").uri(path).body(Body::empty()).unwrap())
            .await
            .expect("router");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn console_html_is_served() {
        let (status, body) = get(build_state(), "/console").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("OPERATOR CONSOLE"), "the embedded UI must be served");
    }

    #[tokio::test]
    async fn console_fleet_returns_seeded_node() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (status, body) = get(svc, "/console/fleet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("robot-01"), "fleet view must list the seeded node");
        assert!(body.contains("Untrusted"), "posture mapped from NodeTrustState");
        assert!(body.contains("post-collision latch"), "the Untrusted note carries through");
    }

    #[tokio::test]
    async fn console_audit_returns_a_page() {
        let (status, body) = get(build_state(), "/console/audit?limit=10").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"entries\""), "audit page passthrough");
        assert!(body.contains("\"chain_intact\""), "the chain-verified flag is exposed");
    }

    #[tokio::test]
    async fn grant_without_supervisor_env_is_fail_closed_503() {
        // No KIRRA_SUPERVISOR_RESET_KEY in the test env → fail-closed 503 (never
        // a silent accept). The 401/422 paths require the env set, which a
        // multithreaded test cannot do (INV-13); those are covered by the pure
        // `supervisor_key_ok` truth table + the store-level audit/grant tests.
        let svc = build_state();
        let resp = build_app(svc)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/console/clearance-grants")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"node_id":"robot-01","operator_id":"alice"}"#))
                    .unwrap(),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn console_plane_is_posture_exempt_with_cold_cache() {
        // `build_state()` has a COLD (None) posture cache. A non-exempt READ
        // would be denied 503 by the posture gate on a cold cache (fail-closed) —
        // the `/console` plane returns 200, proving it is posture-exempt
        // (reachable to observe and recover, e.g. during LockedOut). Regression
        // lock for `gateway::policy_layer::is_posture_exempt`.
        let (status, _) = get(build_state(), "/console/fleet").await;
        assert_eq!(status, StatusCode::OK, "the /console plane must be posture-exempt");
    }

    // --- #394 live console endpoints ----------------------------------------

    /// Seed a node with explicit trust status + optional site/firmware. Reuses
    /// the production write path (`persist_and_insert_node` — disk THEN memory).
    fn seed_node_full(
        svc: &Arc<ServiceState>,
        node_id: &str,
        status: NodeTrustState,
        site: Option<&str>,
        firmware_version: Option<&str>,
    ) {
        let node = RegisteredNode {
            node_id: node_id.to_string(),
            status,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: site.map(|s| s.to_string()),
            firmware_version: firmware_version.map(|s| s.to_string()),
        };
        svc.app.persist_and_insert_node(node).expect("seed node");
    }

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).expect("valid json")
    }

    #[test]
    fn audit_chain_len_counts_rows() {
        // #395 store-level: empty chain is 0; each chained write increments it.
        let mut store = VerifierStore::new(":memory:").expect("store");
        assert_eq!(store.audit_chain_len().expect("len"), 0);
        store
            .save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
            .expect("record grant");
        assert_eq!(store.audit_chain_len().expect("len"), 1);
    }

    #[tokio::test]
    async fn console_runtime_reports_live_state() {
        // #395: empty fleet → Active mode, 0 nodes, null heartbeat, 0 audit rows.
        let svc = build_state();
        let (status, body) = get(svc, "/console/runtime").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        assert_eq!(v["mode"], "Active");
        assert_eq!(v["total_nodes"], 0);
        assert_eq!(v["audit_entries"], 0);
        assert_eq!(v["posture_cache_ttl_ms"], POSTURE_CACHE_TTL_MS);
        assert!(v["ha_heartbeat_age_ms"].is_null(), "no heartbeat written yet");
        assert!(v["uptime_ms"].is_u64());
    }

    #[tokio::test]
    async fn console_sites_rolls_up_by_trust_status() {
        // #397: Trusted→nominal, Unknown→degraded, Untrusted→lockedout; NULL site
        // → unassigned. Two nodes at "alpha", one NULL-site node.
        let svc = build_state();
        seed_node_full(&svc, "n1", NodeTrustState::Trusted, Some("alpha"), None);
        seed_node_full(
            &svc,
            "n2",
            NodeTrustState::Untrusted("fault".into()),
            Some("alpha"),
            None,
        );
        seed_node_full(&svc, "n3", NodeTrustState::Unknown, None, None);

        let (status, body) = get(svc, "/console/sites").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        assert_eq!(v["unassigned"], 1, "the NULL-site node is unassigned");
        let alpha = v["sites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["site"] == "alpha")
            .expect("alpha site present");
        assert_eq!(alpha["total"], 2);
        assert_eq!(alpha["nominal"], 1, "Trusted maps to nominal");
        assert_eq!(alpha["lockedout"], 1, "Untrusted maps to lockedout");
        assert_eq!(alpha["degraded"], 0);
    }

    #[tokio::test]
    async fn console_versions_rolls_up_with_pct() {
        // #398: two nodes on v1.0, one NULL → unknown; total 3.
        let svc = build_state();
        seed_node_full(&svc, "n1", NodeTrustState::Trusted, None, Some("v1.0"));
        seed_node_full(&svc, "n2", NodeTrustState::Trusted, None, Some("v1.0"));
        seed_node_full(&svc, "n3", NodeTrustState::Trusted, None, None);

        let (status, body) = get(svc, "/console/versions").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        assert_eq!(v["total"], 3);
        assert_eq!(v["unknown"], 1);
        let v10 = v["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["version"] == "v1.0")
            .expect("v1.0 present");
        assert_eq!(v10["count"], 2);
        let pct = v10["pct"].as_f64().unwrap();
        assert!((pct - (2.0 / 3.0 * 100.0)).abs() < 1e-9, "pct = count/total*100");
    }

    #[tokio::test]
    async fn console_campaigns_shows_rollout_and_adoption() {
        // WS-4: the public console rollout view mirrors the admin summary — a Rolling
        // campaign with its stage progress + the adoption count from node reports.
        use kirra_verifier::ota_campaign::{Campaign, NodeArtifactStatus};
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let svc = build_state();

        // Seed a reachable Rolling@10% campaign (arm + one advance) and two adoption
        // reports on its digest, directly through the store.
        let mut c = Campaign::new("c-live", digest, "v2", vec!["fleet".into()], vec![10, 100], 1)
            .unwrap();
        c.arm(2).unwrap();
        c.advance(kirra_verifier::verifier::FleetPosture::Nominal, 3)
            .unwrap();
        svc.app
            .store
            .call(move |s| {
                s.insert_campaign(&c)?;
                for node in ["robot-1", "robot-2"] {
                    s.upsert_node_artifact_status(&NodeArtifactStatus {
                        node_id: node.into(),
                        applied_digest: digest.into(),
                        campaign_id: Some("c-live".into()),
                        artifact_version: Some("v2".into()),
                        reported_at_ms: 4,
                        attested: false,
                    })?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .await
            .expect("store task")
            .expect("seed campaign + reports");

        let (status, body) = get(svc, "/console/campaigns").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        assert_eq!(v["total"], 1);
        assert_eq!(v["rolling"], 1);
        let roll = &v["active"][0];
        assert_eq!(roll["campaign_id"], "c-live");
        assert_eq!(roll["rollout_percent"], 10);
        assert_eq!(roll["stage"], 1);
        assert_eq!(roll["stage_count"], 2);
        assert_eq!(roll["applied_nodes"], 2, "both reports adopted the digest");
    }

    #[tokio::test]
    async fn console_analytics_empty_and_seeded_do_not_panic() {
        // #396: empty store → valid shape, no panic.
        let svc = build_state();
        let (status, body) = get(svc.clone(), "/console/analytics").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        assert_eq!(v["window_ms"], 86_400_000u64);
        assert!(v["posture_transitions"].as_array().unwrap().len() == 24);
        assert!(v["denial_rate_series"].is_array());
        assert!(v["interventions_by_asset"].is_array());
        assert!(v["flapping_top"].as_array().unwrap().is_empty());

        // Seed a real chained posture event, then re-query: flapping_top picks it
        // up and a Nominal transition lands in a bucket.
        svc.app.store.with(|store| {
            let posture_json =
                serde_json::to_string(&kirra_verifier::verifier::FleetPosture::Nominal).unwrap();
            store
                .save_posture_event_chained(
                    "robot-09",
                    "ATTESTATION_TRUSTED",
                    &posture_json,
                    None,
                    now_ms(),
                )
                .expect("seed posture event");
        });
        let (status, body) = get(svc, "/console/analytics?window_ms=86400000").await;
        assert_eq!(status, StatusCode::OK);
        let v = parse(&body);
        let flap = v["flapping_top"].as_array().unwrap();
        assert!(
            flap.iter().any(|f| f["node_id"] == "robot-09"),
            "the seeded node appears in flapping_top"
        );
        let total_nominal: u64 = v["posture_transitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["to_nominal"].as_u64().unwrap())
            .sum();
        assert_eq!(total_nominal, 1, "one Nominal transition bucketed");
    }

    #[test]
    fn supervisor_key_ok_truth_table() {
        // unconfigured / empty / over-length → 503 (fail-closed, INV-7)
        assert_eq!(supervisor_key_ok(Some("k"), None), Err(StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(supervisor_key_ok(Some("k"), Some("")), Err(StatusCode::SERVICE_UNAVAILABLE));
        let too_long = "x".repeat(65);
        assert_eq!(supervisor_key_ok(Some("x"), Some(&too_long)), Err(StatusCode::SERVICE_UNAVAILABLE));
        // configured but no / wrong key → 401 ("no auth → 401")
        assert_eq!(supervisor_key_ok(None, Some("secret")), Err(StatusCode::UNAUTHORIZED));
        assert_eq!(supervisor_key_ok(Some("wrong"), Some("secret")), Err(StatusCode::UNAUTHORIZED));
        // correct key → Ok
        assert_eq!(supervisor_key_ok(Some("secret"), Some("secret")), Ok(()));
    }

    #[test]
    fn valid_grant_recorded_in_chain_with_pending_marker() {
        let store = VerifierStore::new(":memory:").expect("store");
        let app = AppState::new(store, VerifierOperationMode::Active);
        app.store.with(|s| {
            s.save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
                .expect("record grant");
        });
        let page = app.store.with(|s| s.load_audit_chain_page(50, 0, None)).expect("page");
        let found = page.entries.iter().any(|e| {
            let v = serde_json::to_value(e).unwrap();
            v.get("event_type").and_then(|x| x.as_str()) == Some("OperatorClearanceGrantIssued")
                && serde_json::to_string(&v).unwrap().contains("PENDING-NODE-TRANSPORT")
        });
        assert!(found, "the grant must be a signed chain event with the PENDING delivery marker");
    }

    #[test]
    fn rejected_attempt_is_audited() {
        let store = VerifierStore::new(":memory:").expect("store");
        let app = AppState::new(store, VerifierOperationMode::Active);
        app.store.with(|s| {
            s.append_clearance_audit_event(
                "OperatorClearanceGrantRejected",
                r#"{"reason":"empty_operator_id","node_id":"robot-01"}"#,
                1_700_000_000_000,
            )
            .expect("audit reject");
        });
        let page = app.store.with(|s| s.load_audit_chain_page(50, 0, None)).expect("page");
        assert!(
            page.entries.iter().any(|e| serde_json::to_value(e).unwrap()
                .get("event_type").and_then(|x| x.as_str()) == Some("OperatorClearanceGrantRejected")),
            "a rejected attempt must leave a signed audit row"
        );
    }

    #[test]
    fn grant_never_mutates_posture() {
        // The Phase-A honesty proof: recording a grant changes NO posture.
        let store = VerifierStore::new(":memory:").expect("store");
        let app = AppState::new(store, VerifierOperationMode::Active);
        seed_node_app(&app, "robot-01");

        let before = app.calculate_posture("robot-01");
        app.store.with(|s| {
            s.save_clearance_grant_chained("robot-01", "alice", 1_700_000_000_000)
                .expect("record grant");
        });
        let after = app.calculate_posture("robot-01");
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap(),
            "a recorded grant must NOT mutate posture (Phase A records; it does not release)"
        );
    }

    fn seed_node_app(app: &AppState, node_id: &str) {
        let node = RegisteredNode {
            node_id: node_id.to_string(),
            status: NodeTrustState::Untrusted("post-collision latch".to_string()),
            registered_at_ms: 1,
            last_trust_update_ms: 1_700_000_000_000,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        };
        app.persist_and_insert_node(node).expect("seed node");
    }

    // ===================================================================
    // #314 Phase 1 — operator-proven identity. The operator-signed flow uses
    // NO env (no admin / supervisor key), so it is fully exercisable here; the
    // operator is seeded via the store directly (the admin route's gating is
    // proved separately, since INV-13 forbids set_var in a multithread test).
    // ===================================================================

    use ed25519_dalek::{Signer, SigningKey};

    /// A deterministic test operator keypair + its SPKI PEM (reuses the in-repo
    /// RFC-8410 prefix convention from `attestation_nonce_handler_tests`).
    fn operator_keypair(seed: u8) -> (SigningKey, String) {
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        const ED25519_SPKI_PREFIX: [u8; 12] =
            [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(sk.verifying_key().as_bytes());
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            b64e.encode(&der)
        );
        (sk, pem)
    }

    fn sign_grant_b64(sk: &SigningKey, operator_id: &str, node_id: &str, nonce: &str) -> String {
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        let payload = kirra_verifier::attestation::operator_grant_signing_payload(
            operator_id, node_id, nonce,
        );
        b64e.encode(sk.sign(&payload).to_bytes())
    }

    /// #412 — sign the EMERGENCY-STOP payload (domain-distinct from a grant).
    fn sign_stop_b64(sk: &SigningKey, operator_id: &str, node_id: &str, nonce: &str) -> String {
        use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
        let payload = kirra_verifier::attestation::operator_stop_signing_payload(
            operator_id, node_id, nonce,
        );
        b64e.encode(sk.sign(&payload).to_bytes())
    }

    fn register_op(svc: &Arc<ServiceState>, operator_id: &str, pem: &str) {
        svc.app.store.with(|s| s.register_operator(operator_id, pem, 1)).unwrap();
    }

    fn parse_nonce(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .unwrap()
            .get("nonce")
            .and_then(|x| x.as_str())
            .expect("challenge body has a nonce")
            .to_string()
    }

    async fn post_json(
        svc: Arc<ServiceState>,
        path: &str,
        body: String,
        supervisor_key: Option<&str>,
    ) -> (StatusCode, String) {
        let mut rb = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(k) = supervisor_key {
            rb = rb.header("x-kirra-supervisor-key", k);
        }
        let resp = build_app(svc).oneshot(rb.body(Body::from(body)).unwrap()).await.expect("router");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    // ===================================================================
    // #325 / #326 / #327 — medium hardening. The admin-gated /console/operators
    // route is 503 without KIRRA_ADMIN_TOKEN (INV-13 forbids set_var here), so the
    // register handler is exercised by a DIRECT call (its admin gating is proved
    // separately by `require_admin_token`); the unauthenticated challenge/grant
    // routes go through the real router.
    // ===================================================================

    fn audit_has(svc: &Arc<ServiceState>, event_type: &str) -> bool {
        let page = svc.app.store.with(|s| s.load_audit_chain_page(200, 0, None)).unwrap();
        page.entries.iter().any(|e| e.event_type == event_type)
    }

    fn chain_json(svc: &Arc<ServiceState>) -> String {
        let page = svc.app.store.with(|s| s.load_audit_chain_page(200, 0, None)).unwrap();
        serde_json::to_string(&page.entries).unwrap()
    }

    fn admin_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        h
    }

    /// #326 — the composite challenge-map key resolves the `{op}|{node}` ambiguity
    /// (the old form collided `(a|b,c)` with `(a,b|c)` to `"a|b|c"`).
    #[test]
    fn composite_key_resolves_delimiter_ambiguity() {
        assert_ne!(
            composite_challenge_key("a|b", "c"),
            composite_challenge_key("a", "b|c"),
            "length-prefixing must distinguish (a|b,c) from (a,b|c)"
        );
        assert_eq!(composite_challenge_key("alice", "robot-01"), "5:alice:robot-01");
    }

    /// #326 — identifier charset: `|` and control characters rejected; clean ids pass.
    #[test]
    fn valid_identifier_rejects_pipe_and_controls() {
        assert!(valid_identifier("alice"));
        assert!(valid_identifier("op-7_A.B"));
        assert!(!valid_identifier(""), "empty rejected");
        assert!(!valid_identifier("a|b"), "pipe rejected");
        assert!(!valid_identifier("a\nb"), "newline rejected");
        assert!(!valid_identifier("a\tb"), "tab rejected");
        assert!(!valid_identifier("a\u{7}b"), "bell control char rejected");
    }

    /// #326 — the register route rejects a `|`-bearing / control-char operator_id
    /// with 400 and accepts a clean one (201). Handler called directly.
    #[tokio::test]
    async fn register_operator_rejects_bad_charset() {
        let svc = build_state();
        let (_sk, pem) = operator_keypair(3);
        let headers = admin_headers("t");

        let bad_pipe = RegisterOperatorRequest { operator_id: "a|b".into(), ed25519_pubkey_pem: pem.clone() };
        let r = register_operator(State(svc.clone()), headers.clone(), Json(bad_pipe)).await.into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "pipe in operator_id → 400");

        let bad_ctrl = RegisterOperatorRequest { operator_id: "a\nb".into(), ed25519_pubkey_pem: pem.clone() };
        let r = register_operator(State(svc.clone()), headers.clone(), Json(bad_ctrl)).await.into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "control char in operator_id → 400");

        let ok = RegisterOperatorRequest { operator_id: "alice".into(), ed25519_pubkey_pem: pem };
        let r = register_operator(State(svc), headers, Json(ok)).await.into_response();
        assert_eq!(r.status(), StatusCode::CREATED, "a clean id registers");
    }

    /// #325 — NO enumeration oracle: an unknown operator gets a uniform 200 with a
    /// nonce-shaped body and NOTHING stored (the decoy proof); an active operator
    /// gets a real stored challenge; and a grant attempt for the unknown operator
    /// still 403s at the unchanged grant-time check.
    #[tokio::test]
    async fn unknown_operator_challenge_is_a_decoy_no_oracle() {
        let svc = build_state();
        seed_node(&svc, "robot-01");

        // Unknown operator → 200 + nonce, but NOTHING stored (no map growth, no 403).
        let (status, body) =
            get(svc.clone(), "/console/clearance-challenge?operator_id=ghost&node_id=robot-01").await;
        assert_eq!(status, StatusCode::OK, "no 403 oracle — unknown operator still gets 200");
        assert!(!parse_nonce(&body).is_empty(), "the decoy response is nonce-shaped");
        assert!(svc.app.pending_clearance_challenges.is_empty(), "the decoy nonce is NEVER stored");

        // Active operator → 200 + nonce AND a real stored challenge under the key.
        let (_sk, pem) = operator_keypair(4);
        register_op(&svc, "alice", &pem);
        let (status, body) =
            get(svc.clone(), "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!parse_nonce(&body).is_empty());
        assert!(
            svc.app
                .pending_clearance_challenges
                .contains_key(&composite_challenge_key("alice", "robot-01")),
            "an active operator's challenge IS stored under the composite key"
        );

        // Grant-time still 403s for the unknown operator (the decoy buys nothing).
        let body = json!({
            "node_id": "robot-01", "operator_id": "ghost",
            "nonce": "abcd", "signature_b64": "AAAA"
        })
        .to_string();
        let (status, _) = post_json(svc, "/console/clearance-grants", body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "an unknown operator cannot redeem a decoy at grant time");
    }

    /// #327 — re-registering a REVOKED operator emits a distinct OperatorReactivated
    /// chain event (carrying the reactivating admin's token fingerprint), not a
    /// silent OperatorRegistered. A FRESH registration emits OperatorRegistered only.
    #[tokio::test]
    async fn reregistering_revoked_operator_audits_reactivation() {
        let svc = build_state();
        let (_sk, pem) = operator_keypair(9);
        let headers = admin_headers("admin-secret");

        // Fresh registration → OperatorRegistered only.
        let req = RegisterOperatorRequest { operator_id: "alice".into(), ed25519_pubkey_pem: pem.clone() };
        let r = register_operator(State(svc.clone()), headers.clone(), Json(req)).await.into_response();
        assert_eq!(r.status(), StatusCode::CREATED);
        assert!(audit_has(&svc, "OperatorRegistered"), "fresh registration is audited");
        assert!(!audit_has(&svc, "OperatorReactivated"), "a fresh registration is NOT a reactivation");

        // Revoke, then re-register → OperatorReactivated appears, attributed.
        svc.app.store.with(|s| s.revoke_operator("alice", 2)).unwrap();
        let req2 = RegisterOperatorRequest { operator_id: "alice".into(), ed25519_pubkey_pem: pem };
        let r = register_operator(State(svc.clone()), headers, Json(req2)).await.into_response();
        assert_eq!(r.status(), StatusCode::CREATED);
        assert!(audit_has(&svc, "OperatorReactivated"), "reactivation is distinctly audited");

        let fp = admin_token_fingerprint(&admin_headers("admin-secret")).unwrap();
        assert!(
            chain_json(&svc).contains(&fp),
            "the reactivation event carries the reactivating admin's token fingerprint"
        );
    }

    /// HAPPY PATH + the ADDITIVE PROOF: an operator-signed grant records with the
    /// key fingerprint in the signed chain, and the EXISTING Phase-B
    /// `take_pending_clearance_grant` consumes the new row shape unchanged.
    #[tokio::test]
    async fn operator_signed_grant_records_fingerprint_and_phase_b_consumes() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(7);
        register_op(&svc, "alice", &pem);

        let (cs, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        assert_eq!(cs, StatusCode::OK, "challenge issued; body={cb}");
        let nonce = parse_nonce(&cb);

        let sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig}).to_string();
        let (gs, gb) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
        assert_eq!(gs, StatusCode::OK, "operator-signed grant recorded; body={gb}");
        assert!(gb.contains("operator-signed"), "auth_method in response");
        let fp = kirra_verifier::attestation::operator_key_fingerprint(&pem).unwrap();
        assert!(gb.contains(&fp), "response carries the key fingerprint");

        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("OperatorClearanceGrantIssued"));
        assert!(ab.contains("operator-signed"), "chain event carries auth_method");
        assert!(ab.contains(&fp), "chain event carries the fingerprint (non-repudiation)");

        // THE ADDITIVE PROOF — Phase-B pickup is unchanged by the new columns.
        let picked = svc.app.store
            .with(|s| s.take_pending_clearance_grant("robot-01", 9_999_999_999_999)).unwrap()
            .expect("Phase-B consumes the operator-signed grant row");
        assert_eq!(picked.operator_id, "alice");
    }

    /// VERIFY-THEN-CONSUME: a replayed nonce is rejected on the second use.
    #[tokio::test]
    async fn nonce_replay_is_rejected_and_audited() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(8);
        register_op(&svc, "alice", &pem);
        let (_c, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        let nonce = parse_nonce(&cb);
        let sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig}).to_string();

        let (s1, _) = post_json(svc.clone(), "/console/clearance-grants", body.clone(), None).await;
        assert_eq!(s1, StatusCode::OK, "first use accepted");
        let (s2, b2) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
        assert_eq!(s2, StatusCode::UNAUTHORIZED, "replayed nonce rejected; body={b2}");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("nonce_replay_or_expired"), "the replay is audited");
    }

    /// BAD SIGNATURE (signed by the wrong key) → 401, audited. Verify happens
    /// before the nonce is consumed.
    #[tokio::test]
    async fn bad_signature_is_rejected_and_audited() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (_sk, pem) = operator_keypair(9);
        register_op(&svc, "alice", &pem);
        let (_c, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        let nonce = parse_nonce(&cb);
        let (wrong, _wpem) = operator_keypair(99);
        let sig = sign_grant_b64(&wrong, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig}).to_string();
        let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "wrong-key signature rejected; body={b}");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("bad_signature"));
    }

    /// UNKNOWN operator → 403, audited (load operator fails before anything else).
    #[tokio::test]
    async fn unknown_operator_is_rejected_403_audited() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let body = json!({"node_id":"robot-01","operator_id":"ghost","nonce":"00","signature_b64":"AAAA"}).to_string();
        let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "unknown operator rejected; body={b}");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("unknown_operator"));
    }

    /// REVOKED operator → 403, audited.
    #[tokio::test]
    async fn revoked_operator_is_rejected_403_audited() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(11);
        register_op(&svc, "alice", &pem);
        svc.app.store.with(|s| s.revoke_operator("alice", 2)).unwrap();
        let sig = sign_grant_b64(&sk, "alice", "robot-01", "00");
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":sig}).to_string();
        let (s, b) = post_json(svc.clone(), "/console/clearance-grants", body, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "revoked operator rejected; body={b}");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("revoked_operator"));
    }

    /// SEPARATE POWERS: operator registration is ADMIN-gated, NOT supervisor-gated.
    /// A supervisor key alone (no admin token) cannot register an operator. (Env
    /// unset → require_admin_token 503; with the env set it would be 401 — the
    /// admin_token_ok decision is unit-tested elsewhere. Either way: never 2xx.)
    #[tokio::test]
    async fn supervisor_key_cannot_register_operators_admin_gated() {
        let svc = build_state();
        let (_sk, pem) = operator_keypair(12);
        let body = json!({"operator_id":"alice","ed25519_pubkey_pem":pem}).to_string();
        let (s, _b) = post_json(svc, "/console/operators", body, Some("a-supervisor-value")).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE,
            "operator registration is admin-gated — the supervisor key does not open it");
    }

    /// BREAK-GLASS is DISTINCTLY audited. The success path needs the supervisor env
    /// (INV-13 forbids set_var here), so prove the distinct-audit property at the
    /// store level: the auth_method "supervisor-break-glass" lands in the signed
    /// chain, visibly different from "operator-signed".
    #[test]
    fn break_glass_auth_method_is_distinct_in_the_chain() {
        let store = VerifierStore::new(":memory:").expect("store");
        let app = AppState::new(store, VerifierOperationMode::Active);
        app.store.with(|s| {
            s.save_clearance_grant_chained_with_auth(
                "robot-01", "alice", 1_700_000_000_000, "supervisor-break-glass", None,
            ).unwrap();
        });
        let page = app.store.with(|s| s.load_audit_chain_page(50, 0, None)).unwrap();
        let blob = serde_json::to_string(&page.entries).unwrap();
        assert!(blob.contains("supervisor-break-glass"),
            "break-glass auth_method recorded distinctly in the signed chain");
    }

    /// #323 — a passive-standby instance must REJECT a clearance-grant write (the
    /// HA split-brain guard), mirroring every other mutation handler. The
    /// `/console` posture-exemption keeps it reachable, but is_active() fail-closes.
    #[tokio::test]
    async fn standby_instance_rejects_clearance_grant() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        // Demote this instance to passive standby.
        svc.app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
        // Any grant shape — the is_active guard fires FIRST, before auth.
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":"AAAA"}).to_string();
        let (s, _b) = post_json(svc, "/console/clearance-grants", body, None).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE,
            "a passive-standby instance must not accept grant writes (split-brain guard)");
    }

    // ----- #412 / ADR-0013 governor-routed authenticated e-stop request --------

    /// HAPPY PATH: an operator-signed stop request makes the GOVERNOR command the
    /// MRC under its own authority — supervisor_tripped is set and BOTH chain
    /// events (request + governor action) are recorded.
    #[tokio::test]
    async fn estop_request_commands_mrc_and_chains_both_events() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(31);
        register_op(&svc, "alice", &pem);
        let (_c, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        let nonce = parse_nonce(&cb);
        let sig = sign_stop_b64(&sk, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig}).to_string();

        let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
        assert_eq!(s, StatusCode::OK, "operator-signed stop accepted; body={b}");
        assert!(b.contains("stop_commanded") && b.contains("MRC_COMMANDED"), "body={b}");

        // The governor commanded the sticky MRC under its own authority.
        assert!(svc.app.supervisor_tripped.load(std::sync::atomic::Ordering::SeqCst),
            "an accepted e-stop must set the sticky supervisor_tripped flag (force_lockout)");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("OperatorStopRequested"), "the authenticated request is chained");
        assert!(ab.contains("GovernorMRCCommanded"), "the governor's MRC action is chained");
        let fp = kirra_verifier::attestation::operator_key_fingerprint(&pem).unwrap();
        assert!(ab.contains(&fp), "the request event carries the operator key fingerprint (non-repudiation)");
    }

    /// THE DOMAIN-SEPARATION SECURITY PROPERTY: a CLEARANCE (release) signature
    /// must NOT be accepted as a STOP request (different signing domain). Without
    /// domain separation an operator's release could be replayed as a stop.
    #[tokio::test]
    async fn clearance_signature_is_not_accepted_as_an_estop() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(32);
        register_op(&svc, "alice", &pem);
        let (_c, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        let nonce = parse_nonce(&cb);
        // Sign the GRANT payload, submit to the e-stop endpoint.
        let grant_sig = sign_grant_b64(&sk, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":grant_sig}).to_string();
        let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED,
            "a clearance signature must not satisfy the e-stop domain; body={b}");
        assert!(!svc.app.supervisor_tripped.load(std::sync::atomic::Ordering::SeqCst),
            "a rejected e-stop must NOT command the MRC");
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("OperatorStopRequestRejected") && ab.contains("bad_signature"));
    }

    /// UNKNOWN operator → 403, audited, no MRC.
    #[tokio::test]
    async fn estop_unknown_operator_rejected_403() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let body = json!({"node_id":"robot-01","operator_id":"ghost","nonce":"00","signature_b64":"AAAA"}).to_string();
        let (s, b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "unknown operator rejected; body={b}");
        assert!(!svc.app.supervisor_tripped.load(std::sync::atomic::Ordering::SeqCst));
        let (_s, ab) = get(svc.clone(), "/console/audit?limit=50").await;
        assert!(ab.contains("OperatorStopRequestRejected") && ab.contains("unknown_operator"));
    }

    /// REPLAY: a stop nonce is single-use — the second identical request is rejected.
    #[tokio::test]
    async fn estop_nonce_replay_is_rejected() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        let (sk, pem) = operator_keypair(33);
        register_op(&svc, "alice", &pem);
        let (_c, cb) = get(svc.clone(),
            "/console/clearance-challenge?operator_id=alice&node_id=robot-01").await;
        let nonce = parse_nonce(&cb);
        let sig = sign_stop_b64(&sk, "alice", "robot-01", &nonce);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":nonce,"signature_b64":sig}).to_string();
        let (s1, _) = post_json(svc.clone(), "/console/estop-requests", body.clone(), None).await;
        assert_eq!(s1, StatusCode::OK, "first stop accepted");
        let (s2, b2) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
        assert_eq!(s2, StatusCode::UNAUTHORIZED, "replayed stop nonce rejected; body={b2}");
    }

    /// HA split-brain guard: a passive-standby instance must not command the MRC.
    #[tokio::test]
    async fn standby_instance_rejects_estop_request() {
        let svc = build_state();
        seed_node(&svc, "robot-01");
        svc.app.mode_active.store(false, std::sync::atomic::Ordering::SeqCst);
        let body = json!({"node_id":"robot-01","operator_id":"alice","nonce":"00","signature_b64":"AAAA"}).to_string();
        let (s, _b) = post_json(svc.clone(), "/console/estop-requests", body, None).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE,
            "a passive-standby instance must not command the MRC (split-brain guard)");
        assert!(!svc.app.supervisor_tripped.load(std::sync::atomic::Ordering::SeqCst));
    }
}

// ---------------------------------------------------------------------------
// Store offload helper (heavy-op spawn_blocking path).
//
// `StoreHandle::call` moves the long-held SQLite ops (backup export,
// audit-chain verify, federation commit) off the tokio worker pool. These tests
// pin its contract: a closure runs to completion against the real store, a write
// is visible to a subsequent offloaded read, and `&mut self` writes + `&self`
// reads both work through the handle. Each runs on a multi-thread runtime so the
// spawn_blocking offload is actually exercised.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod store_offload_tests {
    use std::sync::Arc;
    use kirra_verifier::store_handle::StoreError;
    use kirra_verifier::verifier::{AppState, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    fn app() -> Arc<AppState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        Arc::new(AppState::new(store, VerifierOperationMode::Active))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offloaded_write_is_visible_to_an_offloaded_read() {
        let app = app();

        let wrote = app.store.call(|store| {
            store.save_engine_state("offload_probe", "42").is_ok()
        })
        .await;
        assert!(matches!(wrote, Ok(true)), "offloaded write must run to completion: {wrote:?}");

        let read = app.store.call(|store| {
            store.load_engine_state("offload_probe").ok().flatten()
        })
        .await;
        assert!(
            matches!(read, Ok(Some(ref v)) if v == "42"),
            "an offloaded read must observe the offloaded write; got {read:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offloaded_closure_return_value_is_propagated() {
        let app = app();
        // A pure read that computes a value off-thread and returns it intact.
        let n: Result<u64, StoreError> =
            app.store.call(|_store| 7u64 * 6).await;
        assert!(matches!(n, Ok(42)), "closure return value must propagate; got {n:?}");
    }
}

// ---------------------------------------------------------------------------
// Federation-submit handler E2E (closes the coverage gap flagged in the
// store-offload PR). Drives `submit_federated_report` directly against a real
// in-memory store with a registered controller and genuinely Ed25519-signed
// reports, exercising the full refactored path: offload via `with_store_blocking`
// → the locked commit closure → store persistence + nonce burn → outcome mapping.
//
// This is a HANDLER-level test, not full-router: the route is admin+identity
// gated via `KIRRA_ADMIN_TOKEN` (env), which cannot be set safely in the parallel
// test runner (INVARIANT #13). The auth/router layer is unchanged by the offload
// refactor; this test covers the handler logic the refactor actually touched.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod federation_submit_e2e_tests {
    use super::submit_federated_report;
    use std::sync::Arc;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{body::to_bytes, Json};
    use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use kirra_verifier::federation_reconciliation::{
        canonical_federation_payload_v2, FederatedTrustReportV2,
    };
    use kirra_verifier::posture_cache::{now_ms, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    fn service() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        // An Active node must hold a claimed epoch, or the federation commit's #79
        // in-transaction fence rejects every write as Fenced. Mirror startup's
        // claim-then-store: claim epoch 1 on the fresh ha_state row and publish it.
        {
            let claimed = app
                .store
                .with(|store| store.try_claim_epoch(0, "test-instance", 0))
                .unwrap()
                .expect("claim initial epoch on fresh store");
            app.held_epoch.store(claimed, std::sync::atomic::Ordering::SeqCst);
        }
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

    fn register(svc: &ServiceState, controller: &str, sk: &SigningKey) {
        let pk_b64 = b64.encode(sk.verifying_key().to_bytes());
        svc.app
            .store
            .with(|store| store.save_trusted_federation_controller(controller, &pk_b64, now_ms()))
            .expect("register controller");
    }

    /// A fresh, correctly Ed25519-signed v2 report (issued "now" → inside the
    /// 5 s replay window) for `controller`/`asset`/`nonce`.
    fn signed_report(
        sk: &SigningKey,
        controller: &str,
        asset: &str,
        nonce: &str,
        generation: Option<u64>,
    ) -> FederatedTrustReportV2 {
        let now = now_ms();
        let mut report = FederatedTrustReportV2 {
            source_controller_id: controller.to_string(),
            asset_id: asset.to_string(),
            posture: FleetPosture::Degraded,
            issued_at_ms: now,
            expires_at_ms: now + 30_000,
            nonce_hex: nonce.to_string(),
            signature_b64: String::new(),
            source_generation: generation,
        };
        let sig = sk.sign(canonical_federation_payload_v2(&report).as_bytes());
        report.signature_b64 = b64.encode(sig.to_bytes());
        report
    }

    async fn submit(
        svc: Arc<ServiceState>,
        report: FederatedTrustReportV2,
    ) -> (StatusCode, serde_json::Value) {
        let resp = submit_federated_report(State(svc), Json(report)).await.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepts_valid_report_persists_and_burns_nonce() {
        let svc = service();
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        register(&svc, "ctrl-a", &sk);

        let (status, body) =
            submit(svc.clone(), signed_report(&sk, "ctrl-a", "lidar_front", "nonce-aaaa", Some(412)))
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["accepted"], serde_json::json!(true), "valid report must be accepted: {body}");

        let (has_reports, burned) = svc.app.store.with(|store| {
            let has_reports = !store.load_federated_reports_for_asset("lidar_front").unwrap().is_empty();
            let burned = store.has_seen_federation_nonce("nonce-aaaa").unwrap();
            (has_reports, burned)
        });
        assert!(has_reports, "an accepted report must be persisted");
        assert!(burned, "an accepted report must burn its nonce");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replayed_nonce_is_rejected() {
        let svc = service();
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        register(&svc, "ctrl-a", &sk);
        let report = signed_report(&sk, "ctrl-a", "lidar_front", "nonce-dup", Some(1));

        let (_, first) = submit(svc.clone(), report.clone()).await;
        assert_eq!(first["accepted"], serde_json::json!(true), "first submit must be accepted: {first}");

        let (_, second) = submit(svc.clone(), report).await;
        assert_eq!(second["accepted"], serde_json::json!(false));
        assert_eq!(
            second["reason"], serde_json::json!("FEDERATED_NONCE_REPLAY"),
            "a replayed nonce must be rejected: {second}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unregistered_controller_is_rejected() {
        let svc = service();
        let sk = SigningKey::from_bytes(&[9u8; 32]); // never registered
        let (_, body) =
            submit(svc, signed_report(&sk, "ctrl-unknown", "lidar_front", "nonce-x", None)).await;
        assert_eq!(body["accepted"], serde_json::json!(false));
        assert_eq!(
            body["reason"], serde_json::json!("UNREGISTERED_FEDERATION_CONTROLLER"), "{body}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tampered_signature_is_rejected() {
        let svc = service();
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        register(&svc, "ctrl-a", &sk);
        let mut report = signed_report(&sk, "ctrl-a", "lidar_front", "nonce-bad", None);
        report.signature_b64 = b64.encode([0u8; 64]); // tamper after signing

        let (_, body) = submit(svc.clone(), report).await;
        assert_eq!(body["accepted"], serde_json::json!(false));
        assert_eq!(
            body["reason"], serde_json::json!("INVALID_FEDERATION_SIGNATURE"), "{body}"
        );
        // A signature-rejected report must NOT burn the nonce.
        assert!(
            !svc.app.store.with(|store| store.has_seen_federation_nonce("nonce-bad")).unwrap(),
            "a rejected report must not burn its nonce"
        );
    }
}

// ---------------------------------------------------------------------------
// Industrial replay/freshness gate — handler-level behavior (drives the DNP3
// handler, since the gate is shared across all four industrial handlers).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod industrial_replay_handler_tests {
    use super::{evaluate_dnp3_adapter, ReplayGuarded};
    use std::sync::Arc;
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::Json;
    use kirra_verifier::adapters::dnp3::Dnp3Message;
    use kirra_verifier::posture_cache::{now_ms, CachedFleetPosture, ServiceState, SharedPostureCache};
    use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};
    use kirra_verifier::verifier_store::VerifierStore;

    fn svc() -> Arc<ServiceState> {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let posture_cache: SharedPostureCache =
            Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(FleetPosture::Nominal))));
        Arc::new(ServiceState {
            app,
            posture_cache,
            started_at_ms: now_ms(),
            audit_verifying_key: None,
            fabric_router: Arc::new(kirra_verifier::fabric::router::FabricRouter::new()),
            fabric_telemetry: Arc::new(kirra_verifier::fabric::telemetry::FabricTelemetry::new()),
            fabric_causal_log: Arc::new(kirra_verifier::fabric::causal_log::FabricCausalLog::new_in_memory(None)),
            posture_engine_tx: std::sync::OnceLock::new(),
            perception_cap: kirra_verifier::gateway::perception_monitor::empty_perception_cap(),
            perception_monitor_enabled: false,
        })
    }

    // A benign DNP3 READ (fc 0x01) so the only gate exercised is replay/freshness
    // (a read is ReadTelemetry → admitted in Nominal, not a control, not bounded).
    fn read_msg(source: &str) -> Dnp3Message {
        Dnp3Message {
            source_address: 1, dest_address: 1, function_code: 0x01,
            data_link_control: 0, objects: vec![], source_node: source.to_string(),
        }
    }

    async fn post(svc: Arc<ServiceState>, msg: Dnp3Message, sequence: u64, timestamp_ms: u64) -> serde_json::Value {
        let g = ReplayGuarded { sequence, timestamp_ms, message: msg };
        let resp = evaluate_dnp3_adapter(State(svc), Ok(Json(g))).await.into_response();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_in_order_admitted_then_replay_and_regress_rejected() {
        let svc = svc();
        let now = now_ms();
        let v1 = post(svc.clone(), read_msg("plc-1"), 10, now).await;
        assert_eq!(v1["allowed"], true, "fresh in-order read admitted: {v1}");
        let v2 = post(svc.clone(), read_msg("plc-1"), 10, now).await;
        assert_eq!(v2["allowed"], false);
        assert_eq!(v2["denial_reason"], "INDUSTRIAL_MESSAGE_REPLAY", "replay rejected: {v2}");
        let v3 = post(svc.clone(), read_msg("plc-1"), 5, now).await;
        assert_eq!(v3["denial_reason"], "INDUSTRIAL_MESSAGE_REPLAY", "regress rejected: {v3}");
        let v4 = post(svc.clone(), read_msg("plc-1"), 11, now).await;
        assert_eq!(v4["allowed"], true, "higher seq admitted again: {v4}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_and_future_rejected_and_stale_does_not_burn_sequence() {
        let svc = svc();
        let now = now_ms();
        let stale = post(svc.clone(), read_msg("plc-2"), 1, now.saturating_sub(60_000)).await;
        assert_eq!(stale["denial_reason"], "INDUSTRIAL_MESSAGE_STALE", "{stale}");
        let future = post(svc.clone(), read_msg("plc-3"), 1, now + 60_000).await;
        assert_eq!(future["denial_reason"], "INDUSTRIAL_MESSAGE_FUTURE_DATED", "{future}");
        // The stale message (freshness-checked BEFORE the sequence advance) must NOT
        // have burned the sequence: a later in-window seq-1 from plc-2 is admitted.
        let ok = post(svc.clone(), read_msg("plc-2"), 1, now).await;
        assert_eq!(ok["allowed"], true, "a stale message must not advance the sequence: {ok}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn distinct_sources_have_independent_sequences() {
        let svc = svc();
        let now = now_ms();
        assert_eq!(post(svc.clone(), read_msg("plc-a"), 100, now).await["allowed"], true);
        // plc-b starts fresh; its seq 1 is admitted despite plc-a sitting at 100.
        assert_eq!(post(svc.clone(), read_msg("plc-b"), 1, now).await["allowed"], true);
    }
}

// ---------------------------------------------------------------------------
// P1 CI GUARD — the per-command audit-chain write must never run sync on a worker
// ---------------------------------------------------------------------------
#[cfg(test)]
mod store_offload_guard {
    //! Locks in the review-P1 offload: `save_posture_event_chained` is the hottest
    //! durable (fsync) write in the service. Running it synchronously on a tokio
    //! worker via `store.with(..)` head-of-line-blocks every other request handler
    //! and the fast loop behind one fsync. Every site must instead go through
    //! `StoreHandle::call(..)` (spawn_blocking). This guard scans the binary source
    //! and fails if any production `save_posture_event_chained` is reached through a
    //! synchronous `.store.with(` rather than `.store.call(` — so a future edit
    //! cannot silently reintroduce a worker-pinning audit write.

    #[test]
    fn audit_chain_write_is_never_on_a_sync_worker() {
        // Embeds a compile-time snapshot of this very file (path is relative to it).
        let sources: [&str; 11] = [
            include_str!("kirra_verifier_service.rs"),
            include_str!("kirra_verifier_service/attestation.rs"),
            include_str!("kirra_verifier_service/fleet.rs"),
            include_str!("kirra_verifier_service/audit.rs"),
            include_str!("kirra_verifier_service/action_filter.rs"),
            include_str!("kirra_verifier_service/industrial.rs"),
            include_str!("kirra_verifier_service/federation.rs"),
            include_str!("kirra_verifier_service/actuator.rs"),
            include_str!("kirra_verifier_service/fabric.rs"),
            include_str!("kirra_verifier_service/console.rs"),
            include_str!("kirra_verifier_service/operators.rs"),
        ];
        let mut violations: Vec<usize> = Vec::new();

        for src in sources {
        let mut nearest_access = ""; // "with" (sync) | "call" (off-worker)
        let mut in_test = false; // tests live only in the root file; submodules are all production
        for (idx, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
                    continue;
                }
                if trimmed.starts_with("#[cfg(test)]") {
                in_test = true;
            }
            // Track the nearest ENCLOSING store access (last one seen wins; the
            // production sites place the access immediately above the write).
                if line.contains(".store.call(")
                    || line.contains(".store.call_read(")
                    || trimmed.starts_with(".call(")
                    || trimmed.starts_with(".call_read(")
                {
                nearest_access = "call";
                } else if line.contains(".store.with(")
                    || line.contains(".store.with_read(")
                    || trimmed.starts_with(".with(")
                    || trimmed.starts_with(".with_read(")
                {
                nearest_access = "with";
            }
            if !in_test
                && line.contains("save_posture_event_chained")
                && nearest_access == "with"
            {
                violations.push(idx + 1);
            }
        }
        }

        assert!(
            violations.is_empty(),
            "P1 VIOLATION: `save_posture_event_chained` (the per-command audit-chain fsync) \
             reached via a SYNCHRONOUS `store.with(` at line(s) {violations:?} — a durable write \
             on a tokio worker head-of-line-blocks the whole service. Offload it via \
             `svc.app.store.call(move |store| ...).await` instead.",
        );
    }
}
