// src/bin/kirra_verifier_service.rs
// Kirra Verifier Service — distributed legitimacy fabric entry point.

use axum::{
    extract::rejection::JsonRejection,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tower_http::cors::{Any, CorsLayer};

use kirra_fleet_types::federation::{RegisterFederationControllerRequest, ReportEvaluation};
use kirra_fleet_types::federation_reconciliation::{
    authoritative_posture, dissenting_restriction, evaluate_federated_report_v2,
    verify_federated_report_signature_v2, FederatedTrustReportV2,
};
use kirra_industrial::adapters::canopen::{CanOpenAdapter, CanOpenMessage};
use kirra_industrial::adapters::dnp3::{Dnp3Adapter, Dnp3Message};
use kirra_industrial::adapters::ethernet_ip::{EtherNetIpAdapter, EtherNetIpMessage};
use kirra_industrial::protocol_adapter::{
    evaluate_unified_industrial_request, IndustrialProtocol, UnifiedIndustrialRequest,
};
use kirra_verifier::action_filter::{evaluate_action_claim, ActionClaim};
use kirra_verifier::authz::{
    authorize_request, generate_api_token, token_fingerprint, token_sha256_hex, ApiRole,
    AuthzOutcome, ResolvedPrincipal, SCOPE_ACTUATOR_COMMAND, SCOPE_ADMIN, SCOPE_AUDIT_READ,
    SCOPE_INTEGRATION_EVALUATE,
};
use kirra_verifier::env_config::EffectiveConfig;
use kirra_verifier::posture_cache::{now_ms, ServiceState, POSTURE_CACHE_TTL_MS};
use kirra_verifier::posture_engine_v2::{
    resolve_posture_snapshot_silent, resolve_posture_with_reason, LockoutReason,
};
use kirra_verifier::security::{admin_token_ok, constant_time_compare};
use kirra_verifier::standby_monitor::{
    instance_id as ha_instance_id, spawn_heartbeat_writer, spawn_promotion_monitor, HEARTBEAT_KEY,
    PROMOTION_TIMEOUT_MS,
};
use kirra_verifier::verifier::{
    request_transport_is_secure, validate_client_identity_headers, AppState, BackupExport,
    FlapStatus, FleetPosture, HealthResponse, NodeTrustState, PostureStreamEvent, RegisteredNode,
    VerifierOperationMode,
};
use kirra_verifier::verifier_store::{DurableWriteError, VerifierStore};

/// EP-12 (Config Slice B): the boot-validated configuration snapshot, set ONCE
/// in `main` after validation. The spawn-registry closures (which run again on
/// standby promotion via `wire_active_posture_freshness`) read the audit-ship
/// path from here — the same values the digest was computed over.
static EFFECTIVE_CONFIG: std::sync::OnceLock<EffectiveConfig> = std::sync::OnceLock::new();
use kirra_core::kinematics_contract::ProposedVehicleCommand;
use kirra_fabric_types::asset::{AssetPosture, AssetType, FabricAsset, KinematicProfileType};
use kirra_verifier::fabric::causal_log::FabricCausalLog;
use kirra_verifier::fabric::router::FabricRouter;
use kirra_verifier::fabric::telemetry::FabricTelemetry;
use kirra_verifier::gateway::policy_layer::{
    enforce_actuator_safety_envelope, enforce_posture_routing, EnforcementOutcome,
};
use kirra_verifier::recovery_hysteresis::{evaluate_recovery_report, HysteresisDecision};

// Route handlers, split by domain into sibling submodules. Each holds
// `pub(crate)` handler fns that share the binary's helpers, DTOs and `use`
// imports via `use super::*` (descendant-module visibility). Re-exported
// below so `build_app` and the in-file tests reference them unqualified.
#[path = "kirra_verifier_service/action_filter.rs"]
mod action_filter;
#[path = "kirra_verifier_service/actuator.rs"]
mod actuator;
#[path = "kirra_verifier_service/attestation.rs"]
mod attestation;
#[path = "kirra_verifier_service/audit.rs"]
mod audit;
#[path = "kirra_verifier_service/console.rs"]
mod console;
#[path = "kirra_verifier_service/fabric.rs"]
mod fabric;
#[path = "kirra_verifier_service/federation.rs"]
mod federation;
#[path = "kirra_verifier_service/fleet.rs"]
mod fleet;
#[path = "kirra_verifier_service/industrial.rs"]
mod industrial;
#[path = "kirra_verifier_service/operators.rs"]
mod operators;
#[path = "kirra_verifier_service/principals.rs"]
mod principals;
// WS-4 / Track 3 (Fleet Plane) — OTA governor-artifact campaign handlers
// (create / list / get / arm / advance / halt). ADMIN-scoped at the router layer.
#[path = "kirra_verifier_service/campaigns.rs"]
mod campaigns;
// EP-17 explainable safety verdicts — GET /verdicts/{id} renders a denied
// actuator command as a signed, human-readable artifact. AUDITOR-scoped.
#[path = "kirra_verifier_service/verdicts.rs"]
mod verdicts;
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
#[path = "kirra_verifier_service/posture_feed.rs"]
mod posture_feed;
use posture_feed::*;
#[path = "kirra_verifier_service/systemd.rs"]
mod systemd;
use systemd::*;

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
use action_filter::*;
use actuator::*;
use attestation::*;
use audit::*;
use auth::*;
use campaigns::*;
use console::*;
use fabric::*;
use federation::*;
use fleet::*;
use industrial::*;
use observability::request_observability;
use operators::*;
use principals::*;
use startup::*;

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

    let db_path =
        std::env::var("KIRRA_DB_PATH").unwrap_or_else(|_| "kirra_verifier.sqlite".to_string());
    let listen_addr =
        std::env::var("KIRRA_VERIFIER_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".to_string());

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
    kirra_industrial::adapters::canopen::init_node_map_from_env();

    // DNP3 Analog Output magnitude envelope from KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE
    // ("min:max"); unset/invalid → analog control writes are denied (fail-closed).
    kirra_industrial::adapters::dnp3::init_analog_envelope_from_env();

    // CANopen SDO download per-target magnitude bounds from KIRRA_CANOPEN_SDO_BOUNDS
    // ("node:index:subindex=type:min:max", …) + KIRRA_CANOPEN_STRICT_BOUNDS. Unset →
    // SDO writes are posture-only; a configured target is faithfully decoded by its
    // declared type and bounded (fail-closed on breach/undecodable).
    kirra_industrial::adapters::canopen::init_sdo_bounds_from_env();

    // CIP per-attribute magnitude bounds from KIRRA_CIP_ATTR_BOUNDS
    // ("class:instance:attr=type:min:max", …) + KIRRA_CIP_STRICT_BOUNDS. Unset →
    // CIP writes are posture-only; a configured Set_Attribute_Single target is
    // faithfully decoded by its declared type and bounded (fail-closed on breach).
    kirra_industrial::adapters::ethernet_ip::init_cip_bounds_from_env();

    // EP-12 (Config Slice B): build the boot-time EffectiveConfig snapshot ONCE,
    // VALIDATING every migrated variable (vehicle class, HA timing knobs, HA
    // gates). FAIL-CLOSED: a malformed value refuses startup here — it can never
    // reach a module that would have silently defaulted it at use. The migrated
    // modules (contract_profiles, standby_monitor, lease, audit_shipper) perform
    // no environment reads; they consume this snapshot.
    let effective_cfg = EFFECTIVE_CONFIG
        .get_or_init(|| match EffectiveConfig::from_env() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "FATAL: invalid boot configuration — refusing to start");
                std::process::exit(1);
            }
        })
        .clone();

    // #312: pin the (already-validated) deployment vehicle class. FAIL-CLOSED
    // semantics unchanged: unset/unknown was refused by the config validation
    // above (there is no default class — a wrong class would pick another
    // class's envelope). Drives the per-class kinematic contract in the actuator
    // gate (`enforce_actuator_safety_envelope`).
    kirra_verifier::gateway::contract_profiles::init_vehicle_class(
        effective_cfg.vehicle_class_typed,
    );

    // EP-12: resolve + pin the HA instance identity from the captured raw inputs
    // (KIRRA_INSTANCE_ID → HOSTNAME → machine-id/id-file fallbacks). One
    // resolution at boot; every later `instance_id()` read returns this value.
    kirra_verifier::standby_monitor::init_instance_id(effective_cfg.resolve_instance_id());

    let audit_signing_key: Option<ed25519_dalek::SigningKey> =
        std::env::var("KIRRA_LOG_SIGNING_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|b64_str| {
                use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
                b64e.decode(&b64_str)
                    .ok()
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
        let admission = match store.admit_signing_key(
            key.clone(),
            adopt,
            pinned.as_deref(),
            now_ms(),
        ) {
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
            KeyAdmission::MigrationReversionRejected {
                chain_latest_key_id,
                env_key_id,
            } => {
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
    let audit_tx = kirra_verifier::audit_writer::spawn_audit_writer(Arc::clone(&app_state));
    app_state.install_audit_writer(audit_tx);

    // WP-17 (MGA G-17) — unified env configuration: (1) WARN on any KIRRA_* env var
    // the schema registry does not know (a typo / stale var an operator believes is
    // in effect), and (2) commit the effective boot-config SHA-256 digest to the
    // tamper-evident audit chain, so an operator can prove what configuration this
    // instance booted under and detect drift across restarts. Observability only —
    // the sweep never fails startup (a future var on an older binary is legitimate).
    {
        use kirra_verifier::env_config::unknown_kirra_env_vars;
        let env_names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
        let unknown = unknown_kirra_env_vars(env_names.iter().map(String::as_str));
        if !unknown.is_empty() {
            tracing::warn!(
                unknown_vars = %unknown.join(","),
                "unrecognized KIRRA_* environment variable(s) — a typo or a stale var \
                 that is NOT taking effect; see the env schema registry"
            );
        }
        // EP-12: reuse the boot-validated snapshot (built once, above) — the
        // digest and the injected module configs come from the SAME values.
        let cfg = effective_cfg.clone();
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
    if kirra_core::capture::capture_enabled() {
        let capture_tx = kirra_core::capture::spawn_capture_writer();
        app_state.install_capture_writer(capture_tx);
        tracing::info!(
            "learning-loop capture ENABLED (KIRRA_CAPTURE_ENABLED) — verdict records → JSONL sink"
        );
    }

    {
        let load_initial = app_state
            .store
            .call_read(|store| {
                let nodes = store.load_nodes().map_err(|e| e.to_string())?;
                let dependencies = store.load_dependencies().map_err(|e| e.to_string())?;
                Ok::<_, String>((nodes, dependencies))
            })
            .await;
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
            app_state.fleet.nodes.insert(node.node_id.clone(), node);
        }
        for (node_id, deps) in dependencies {
            app_state.fleet.dependency_graph.insert(node_id, deps);
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
        perception_cap: kirra_core::perception_monitor::empty_perception_cap(),
        perception_monitor_enabled: false,
        last_actuator_verdict: kirra_verifier::posture_cache::empty_last_verdict_cell(),
    });

    {
        // Load the assets under one acquisition; register them OUTSIDE the
        // closure (registration borrows svc_state and calls back into the store
        // via seed_local_asset_lockedout — keep it off the held guard).
        // SAFETY: SG-HA-3 — read off the worker pool via read replica.
        let assets = svc_state
            .app
            .store
            .call_read(|store| store.load_fabric_assets())
            .await
            .ok()
            .and_then(|r| r.ok());
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
            let arbitration = svc_state
                .app
                .store
                .call_read(|store| {
                    let (epoch, holder) = store.current_active_holder().ok()?;
                    let hb_str = store.load_engine_state(HEARTBEAT_KEY).ok()?;
                    let now = now_ms();
                    let hb_fresh = hb_str
                        .as_deref()
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|ts| now.saturating_sub(ts) < PROMOTION_TIMEOUT_MS)
                        .unwrap_or(false);
                    Some((epoch, holder, hb_fresh))
                })
                .await
                .ok()
                .flatten();

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
                    let claim = svc_state
                        .app
                        .store
                        .call(move |s| {
                            Ok::<_, ()>(s.try_claim_epoch(epoch, &my_id_c, now_ms()).ok().flatten())
                        })
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .flatten();
                    match claim {
                        Some(new_epoch) => {
                            svc_state
                                .app
                                .ha_fence
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
            .ha_fence
            .mode_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    // EP-12: the HA loops consume the boot-validated timing bundle (heartbeat
    // interval / promotion timeout + poll / force-promote / lease gate) —
    // a malformed knob already refused startup at config validation.
    let ha_timings = effective_cfg.ha_timings();
    match effective_mode {
        VerifierOperationMode::Active => {
            spawn_heartbeat_writer(Arc::clone(&svc_state.app), ha_timings);
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
                ha_timings,
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
        match svc_state
            .app
            .store
            .call(|store| store.ensure_hash_v2_migration_anchor(now_ms()))
            .await
        {
            Ok(Ok(())) => tracing::info!("audit: hash-v2 migration anchor ensured"),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "audit: hash-v2 migration anchor FAILED at startup")
            }
            Err(_) => tracing::error!("audit: hash-v2 migration anchor FAILED — store task error"),
        }
        // Key-id backfill (#76): assign existing NULL-key_id rows the genesis
        // key's id so they verify after a future rotation. Idempotent; signed.
        match svc_state
            .app
            .store
            .call(|store| store.ensure_key_id_backfill_migration(now_ms()))
            .await
        {
            Ok(Ok(())) => tracing::info!("audit: key-id backfill migration ensured"),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "audit: key-id backfill migration FAILED at startup")
            }
            Err(_) => tracing::error!("audit: key-id backfill migration FAILED — store task error"),
        }
        // Anchor-head backfill (#77): a chain written by a pre-#77 binary has no
        // signed head; sign one from the current tail so an upgraded store
        // presents a head BEFORE serving /system/audit/verify (no false
        // HEAD_ABSENT). Idempotent. Log-and-continue: a missing head is itself
        // caught fail-closed at verify time (head_verified = false).
        match svc_state
            .app
            .store
            .call(|store| store.ensure_audit_anchor_head(now_ms()))
            .await
        {
            Ok(Ok(())) => tracing::info!("audit: anchor-head high-water mark ensured"),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "audit: anchor-head high-water mark FAILED at startup")
            }
            Err(_) => {
                tracing::error!("audit: anchor-head high-water mark FAILED — store task error")
            }
        }
    } else {
        tracing::info!("audit: hash-v2 + key-id migrations skipped — passive standby (read-only)");
    }

    // ── WP-20 s2: execution-manager boot gate (fail-closed) ──────────────
    //
    // The declarative task manifest (`execution_manager::TASK_MANIFEST`) is the
    // reviewed source of truth for the supervised background loops and their
    // dependency order. Resolve it into a startup order BEFORE any loop is
    // spawned and ABORT if it is unorderable (duplicate / unknown-dep / cycle) —
    // a malformed future manifest edit must never run tasks in an undefined
    // order (the same fail-closed discipline as SG-008). The resolved order is
    // logged so the boot record documents the canonical supervised-loop sequence.
    // (Driving the actual spawn sites from this order + applying SchedulingClass
    // as SCHED_FIFO/affinity syscalls + feeding DeadlineStats into /metrics are
    // the recorded WP-20 follow-ups.)
    match kirra_verifier::execution_manager::resolve_startup_order(
        kirra_verifier::execution_manager::TASK_MANIFEST,
    ) {
        Ok(order) => tracing::info!(
            startup_order = ?order,
            "execution manager: task manifest resolved — supervised-loop startup order"
        ),
        Err(e) => {
            tracing::error!(
                error = %e,
                "execution manager: task manifest is unorderable — aborting before spawn (fail-closed)"
            );
            std::process::exit(1);
        }
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

    // ADR-0033 ROS release-token signer (opt-in; fail-closed — actuator.rs).
    let ros_release_signer = provision_ros_release_signer();

    // Assemble the production router. Extracted into `build_app` (issue #72)
    // so the EXACT assembled router — identical routes, middleware layer
    // order, and state wiring — is what the binary-internal posture-gate
    // test exercises, rather than a representative stand-in.
    let app = build_app(Arc::clone(&svc_state), ros_release_signer);

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
        sqlite_wal: svc_state
            .app
            .store
            .call_read(|store| store.is_wal_mode())
            .await
            .ok()
            .unwrap_or(false),
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
        Ok(tls::TlsResolution::Tls {
            cert_path,
            key_path,
            client_ca_path,
        }) => match tls::load_server_config(&cert_path, &key_path, client_ca_path.as_deref()) {
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
        },
        Err(err) => {
            tracing::error!(error = %err, "TLS config invalid — aborting before bind (fail-closed)");
            std::process::exit(1);
        }
    };

    // Sec1 (#1044) — enforce AOU-TRANSPORT-PROXY-001. When the transport-security
    // gate is ON but in-process TLS is OFF, the gate can only derive "secure" from
    // the forwarded-proto header, which is trustworthy ONLY behind a proxy that
    // unconditionally overwrites it. Warn LOUDLY so an operator cannot mistake the
    // gate for a guarantee on a directly-reachable plaintext listener (with
    // in-process TLS the gate uses the real connection and needs no such trust).
    if svc_state.app.transport.security.require_secure_transport && tls_config.is_none() {
        tracing::warn!(
            aou = "AOU-TRANSPORT-PROXY-001",
            "KIRRA_REQUIRE_SECURE_TRANSPORT is ON but in-process TLS is OFF: the gate falls \
             back to the forwarded-proto header, which is SOUND ONLY behind a proxy that \
             UNCONDITIONALLY overwrites X-Forwarded-Proto. A directly-reachable plaintext \
             listener would let a client spoof it. Set KIRRA_TLS_CERT_PATH/KIRRA_TLS_KEY_PATH \
             for unspoofable connection-derived enforcement, or guarantee the proxy obligation."
        );
    }

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
        match shutdown_state
            .store
            .call(|store| store.durable_checkpoint())
            .await
        {
            Ok(Ok(())) => tracing::info!("audit: durable checkpoint flushed on shutdown"),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "audit: durable checkpoint FAILED on shutdown")
            }
            Err(_) => {
                tracing::error!("audit: durable checkpoint skipped — store unavailable at shutdown")
            }
        }
    };

    let serve_result = match tls_config {
        // Opt-in in-process TLS (WS-1 Track 1.2). Per-connection handshake tasks;
        // the mesh-mTLS transport gate (#805) still composes on top when enabled.
        Some(cfg) => tls::serve_tls(listener, app, cfg, shutdown).await,
        // Default plaintext path — unchanged (`axum::serve` graceful shutdown).
        None => {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
        }
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
    let configured = configured
        .filter(|v| !v.is_empty())
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
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
    let _ = store
        .call(move |s| {
            let _ = s.append_clearance_audit_event(
                "OperatorClearanceGrantRejected",
                &json!({ "reason": reason, "node_id": node_id, "operator_id": operator_id })
                    .to_string(),
                now,
            );
        })
        .await;
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
    let _ = store
        .call(move |s| {
            let _ = s.append_clearance_audit_event(
                "OperatorStopRequestRejected",
                &json!({ "reason": reason, "node_id": node_id, "operator_id": operator_id })
                    .to_string(),
                now,
            );
        })
        .await;
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
fn build_app(
    svc_state: Arc<ServiceState>,
    ros_release_signer: Option<Arc<kirra_verifier::governor_release::RosReleaseSigner>>,
) -> Router {
    let identity_gated_routes = Router::new()
        .route("/system/posture/stream", get(system_posture_stream))
        .route("/federation/reports/submit", post(submit_federated_report))
        .route("/action_filter/evaluate", post(evaluate_action_filter))
        .route("/fleet/campaigns/report", post(report_node_artifact))
        .route("/industrial/evaluate", post(evaluate_industrial_adapter))
        .route(
            "/industrial/ethernet-ip/evaluate",
            post(evaluate_ethernet_ip_adapter),
        )
        .route(
            "/industrial/canopen/evaluate",
            post(evaluate_canopen_adapter),
        )
        .route("/industrial/dnp3/evaluate", post(evaluate_dnp3_adapter))
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_client_identity,
        ))
        // WS-1 (#G7): SCOPE_INTEGRATION_EVALUATE — the admin token (break-glass) OR
        // an `integrator`-role principal. Previously admin-token-only.
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_integration_scope,
        ))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_secure_transport,
        ));

    let admin_routes = Router::new()
        .route("/attestation/register", post(register_node))
        .route("/fleet/dependencies", post(register_dependencies))
        .route(
            "/fleet/diagnostics/report",
            post(handle_sensor_fault_report),
        )
        .route("/fleet/assets/register", post(handle_register_av_asset))
        .route("/fleet/av-subsystems", get(list_av_subsystems))
        .route("/system/backup/export", post(export_backup))
        .route(
            "/system/audit/rotate-signing-key",
            post(handle_audit_rotate_key),
        )
        // WS-1 (#G7) — API principal registry. SCOPE_ADMIN (admin-only); the mint
        // returns the plaintext token exactly once.
        .route(
            "/system/principals",
            post(register_api_principal_handler).get(list_api_principals_handler),
        )
        .route(
            "/system/principals/{principal_id}/revoke",
            post(revoke_api_principal_handler),
        )
        // WS-1 (#G7) Track 1.2 — mTLS cert-principal registry. SCOPE_ADMIN; pins a
        // CA-verified client cert (by SHA-256 fingerprint) to a scoped principal.
        .route(
            "/system/cert-principals",
            post(register_cert_principal_handler).get(list_cert_principals_handler),
        )
        .route(
            "/system/cert-principals/{principal_id}/revoke",
            post(revoke_cert_principal_handler),
        )
        // WS-4 / Track 3 (Fleet Plane) — OTA governor-artifact campaign control
        // plane. SCOPE_ADMIN; each lifecycle mutation writes an R156-shaped audit
        // entry. `advance` is fail-closed on fleet posture (non-Nominal → HALT).
        .route(
            "/system/campaigns",
            post(create_campaign_handler).get(list_campaigns_handler),
        )
        .route("/system/campaigns/summary", get(campaigns_summary_handler))
        .route("/system/campaigns/{campaign_id}", get(get_campaign_handler))
        .route(
            "/system/campaigns/{campaign_id}/arm",
            post(arm_campaign_handler),
        )
        .route(
            "/system/campaigns/{campaign_id}/advance",
            post(advance_campaign_handler),
        )
        .route(
            "/system/campaigns/{campaign_id}/halt",
            post(halt_campaign_handler),
        )
        .route(
            "/federation/controllers/register",
            post(register_federation_controller),
        )
        .route(
            "/attestation/identity/register",
            post(register_node_identity),
        )
        // #314 Phase 1 — operator registry. ADMIN-gated (separate power from the
        // supervisor key); posture-exempt by the /console/ path prefix.
        .route(
            "/console/operators",
            post(register_operator).get(list_operators),
        )
        .route(
            "/console/operators/{operator_id}/revoke",
            post(revoke_operator),
        )
        .route(
            "/fabric/assets/register",
            post(handle_register_fabric_asset),
        )
        .route("/fabric/assets", get(handle_list_fabric_assets))
        .route("/fabric/state", get(handle_fabric_state))
        .route("/fabric/telemetry", get(handle_fabric_telemetry))
        .route(
            "/fabric/telemetry/{asset_id}",
            get(handle_fabric_telemetry_asset),
        )
        .route("/fabric/command/{asset_id}", post(handle_fabric_command))
        .route("/fabric/causal-log", get(handle_fabric_causal_log))
        .route(
            "/fabric/causal-log/{entry_id}",
            get(handle_fabric_causal_chain),
        )
        // #G7 slice 3 — attribution runs INNER of require_admin_token (which
        // authenticates and records the resolved principal in the request
        // extensions). Scoped to these admin state-mutation routes only: NOT the
        // actuator (high-rate control) nor the self-auditing identity-gated
        // evaluations.
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            record_admin_action_audit,
        ))
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_admin_token,
        ))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_secure_transport,
        ));

    // WS-1 (#G7) — read-only audit-chain verification/export, carved out of the
    // admin group so an `auditor`-role principal (least privilege — NO mutation
    // rights) can reach them. SCOPE_AUDIT_READ; the admin token still qualifies
    // (Admin holds every scope). The full-state `/system/backup/export` and the
    // `rotate-signing-key` mutation stay admin-only above.
    let auditor_routes = Router::new()
        .route("/system/audit/verify", get(verify_audit_chain))
        .route("/system/audit/causal/verify", get(verify_causal_chain))
        .route("/system/audit/export", get(handle_audit_export))
        // EP-17 — one denial as a signed, explained artifact (reads the
        // chained record + the denied command's raw inputs → audit-read tier).
        .route("/verdicts/{verdict_id}", get(verdicts::get_verdict_handler))
        .route("/system/verdicts/last", get(verdicts::last_verdict))
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_audit_scope,
        ))
        // #G7 — same transport-security boundary as every other gated group.
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_secure_transport,
        ));

    let actuator_routes = Router::new()
        .route(
            "/actuator/motion/command",
            post(handle_actuator_motion_command),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            enforce_actuator_safety_envelope,
        ));
    let actuator_routes = layer_release_signer(actuator_routes, ros_release_signer)
        // WS-1 (#G7): SCOPE_ACTUATOR_COMMAND — the admin token OR an `operator`-role
        // principal. Auth runs before the envelope; the transport gate runs first of all.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&svc_state),
            require_actuator_scope,
        ))
        // #G7 — OUTERMOST: reject an insecure-transport request before auth even
        // reads the credential (no-op unless KIRRA_REQUIRE_SECURE_TRANSPORT is on).
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_secure_transport,
        ));

    let attestation_routes = Router::new()
        .route("/attestation/challenge/{node_id}", post(issue_challenge))
        .route("/attestation/verify", post(verify_attestation))
        // #G7 (Copilot #805) — the challenge/verify flow exchanges attestation
        // nonces + signatures; when secure transport is required they must not be
        // processed off a plaintext leg either, even though the flow is otherwise
        // unauthenticated (the challenge-response provides its own guarantee).
        .layer(middleware::from_fn_with_state(
            svc_state.clone(),
            require_secure_transport,
        ));

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
    // `KIRRA_CORS_ALLOWED_ORIGINS` (comma-separated). Auth is `Authorization:
    // Bearer` (no cookies / no `allow_credentials`), so a permissive origin is
    // not a CSRF vector — the allowlist is defense-in-depth controlling which web
    // origins may READ responses. A set env with no parseable origin yields an
    // empty allowlist (deny cross-origin), logged — fail-closed rather than
    // silently reverting to permissive.
    //
    // Sec4 (#1050): the UNSET default is now **deny cross-origin** (empty
    // allowlist), not `Any`. Cross-origin reads must be opted into explicitly —
    // either by naming the origins in `KIRRA_CORS_ALLOWED_ORIGINS`, or, for the
    // genuinely-open case, by setting `KIRRA_CORS_ALLOW_ANY_ORIGIN=1`. Same-origin
    // requests (the console SPA served by the verifier itself) never need CORS, so
    // this default breaks no first-party surface; it only stops arbitrary web
    // origins from reading already-public responses by default.
    let cors = {
        let base = CorsLayer::new().allow_methods(Any).allow_headers(Any);
        let allow_any = std::env::var("KIRRA_CORS_ALLOW_ANY_ORIGIN")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
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
            // Unset / empty → deny cross-origin by default (Sec4 #1050), UNLESS the
            // operator explicitly opts into the open case.
            _ if allow_any => {
                tracing::warn!(
                    "KIRRA_CORS_ALLOW_ANY_ORIGIN is set — allowing ANY cross-origin \
                     web read of (already-public) responses; prefer an explicit \
                     KIRRA_CORS_ALLOWED_ORIGINS allowlist"
                );
                base.allow_origin(Any)
            }
            _ => {
                tracing::info!(
                    "CORS defaulting to DENY cross-origin (Sec4): set \
                     KIRRA_CORS_ALLOWED_ORIGINS to a web-origin allowlist, or \
                     KIRRA_CORS_ALLOW_ANY_ORIGIN=1 to allow any (same-origin \
                     requests are unaffected)"
                );
                base.allow_origin(Vec::<axum::http::HeaderValue>::new())
            }
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
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
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

#[cfg(test)]
#[path = "kirra_verifier_service/posture_gate_real_router_tests.rs"]
mod posture_gate_real_router_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/fabric_posture_feed_tests.rs"]
mod fabric_posture_feed_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/fabric_command_authoritative_tests.rs"]
mod fabric_command_authoritative_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/attestation_nonce_handler_tests.rs"]
mod attestation_nonce_handler_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/local_asset_lockedout_seed_tests.rs"]
mod local_asset_lockedout_seed_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/dnp3_mandatory_audit_tests.rs"]
mod dnp3_mandatory_audit_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/systemd_notify_tests.rs"]
mod systemd_notify_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/console_phase_a_tests.rs"]
mod console_phase_a_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/store_offload_tests.rs"]
mod store_offload_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/ros_release_mint_tests.rs"]
mod ros_release_mint_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/federation_submit_e2e_tests.rs"]
mod federation_submit_e2e_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/industrial_replay_handler_tests.rs"]
mod industrial_replay_handler_tests;

#[cfg(test)]
#[path = "kirra_verifier_service/store_offload_guard.rs"]
mod store_offload_guard;
