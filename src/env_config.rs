//! WP-17 (MGA G-17) — unified verifier environment configuration.
//!
//! The verifier reads its configuration from ~30 `KIRRA_*` environment variables
//! scattered across a dozen modules, each with its own ad-hoc parse. Three gaps
//! that this module closes (the scattered per-module reads themselves fold in
//! incrementally — see the module note at the bottom):
//!
//! 1. **A single source of truth** — [`KIRRA_ENV_KEYS`] is the canonical registry
//!    of every `KIRRA_*` variable the service honors (name + required-ness +
//!    one-line purpose). Documentation and the sweep below both derive from it, so
//!    they can never silently drift from the code.
//! 2. **An unknown-variable sweep** — [`unknown_kirra_env_vars`] flags any
//!    `KIRRA_*` in the process environment that the registry does NOT know: a typo
//!    (`KIRRA_ADMIN_TOEKN`) or a stale var an operator believes is taking effect.
//!    Surfaced as a startup WARN (observability — never a hard fail; a future var
//!    on an older binary is legitimate).
//! 3. **An effective-config digest in the audit chain** — [`EffectiveConfig`] is a
//!    versioned snapshot of the boot-time service configuration actually in effect;
//!    its SHA-256 [`digest`](EffectiveConfig::effective_digest) is appended to the
//!    tamper-evident audit chain at startup, so an operator can prove *what*
//!    configuration a running instance booted under and detect drift across
//!    restarts.
//!
//! Everything here is PURE over injected values (`from_values` / the sweep take
//! their inputs), with thin `*_from_env` wrappers for production — so the whole
//! truth table is unit-tested without `std::env::set_var` (CRITICAL SECURITY
//! INVARIANT #13 forbids it in the parallel test runner).

use sha2::{Digest, Sha256};

/// The effective-config schema version. Bumped when the captured field SET of
/// [`EffectiveConfig`] changes (adding/removing a field changes every digest), so
/// a digest is only ever compared within one schema version.
///
/// v2 (EP-12, Config Slice B): the HA timing knobs (heartbeat interval /
/// promotion timeout / promotion poll / force-promote / lease gate) and the
/// audit-ship sink path fold into the captured set, and construction becomes
/// VALIDATING — a malformed value in any migrated variable fails at boot
/// ([`ConfigError`]), never silently defaulting at use.
pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// One row of the `KIRRA_*` env schema — the single source of truth for a variable.
#[derive(Debug, Clone, Copy)]
pub struct EnvKeySpec {
    /// The variable name, e.g. `KIRRA_ADMIN_TOKEN`.
    pub name: &'static str,
    /// True if the service refuses to run (fail-closed) without it in at least one
    /// deployment shape. (Advisory metadata for the generated docs; the actual
    /// fail-closed enforcement lives at each read site + the startup sentinel.)
    pub required: bool,
    /// One-line purpose, mirrored into the generated env documentation.
    pub purpose: &'static str,
}

/// The canonical registry of every `KIRRA_*` variable the verifier **service**
/// honors. Adding a new `KIRRA_*` read anywhere in the service MUST add its row
/// here — the registry-completeness test and the unknown-var sweep both key off
/// it, so an unregistered variable is caught in CI (a stale doc) or at runtime (a
/// spurious "unknown var" WARN for a var the code actually reads).
///
/// Scope: the verifier service binary + its library modules. The industrial Modbus
/// gateway (`main.rs`) and the ROS2/parko node have their own configs.
pub const KIRRA_ENV_KEYS: &[EnvKeySpec] = &[
    EnvKeySpec { name: "KIRRA_ADMIN_TOKEN", required: true, purpose: "Bearer token for mutation routes; absent/empty → 503 (fail-closed)" },
    EnvKeySpec { name: "KIRRA_SUPERVISOR_RESET_KEY", required: false, purpose: "Reset-op key; must be non-empty and ≤64 bytes when used" },
    EnvKeySpec { name: "KIRRA_VERIFIER_MODE", required: false, purpose: "active | passive_standby (read-only standby)" },
    EnvKeySpec { name: "KIRRA_DB_PATH", required: false, purpose: "SQLite file path (default kirra_verifier.sqlite)" },
    EnvKeySpec { name: "KIRRA_VERIFIER_ADDR", required: false, purpose: "Listen address (default 0.0.0.0:8090)" },
    EnvKeySpec { name: "KIRRA_VEHICLE_CLASS", required: true, purpose: "courier | delivery-av | robotaxi; unset/unknown aborts startup (fail-closed)" },
    EnvKeySpec { name: "KIRRA_TRUSTED_INGRESS_MODE", required: false, purpose: "Enable client-id header enforcement" },
    EnvKeySpec { name: "KIRRA_CLIENT_ID_HEADER", required: false, purpose: "Header name for identity-gated routes (default x-kirra-client-id)" },
    EnvKeySpec { name: "KIRRA_REQUIRE_SECURE_TRANSPORT", required: false, purpose: "Require https on gated routes (fail-closed gate)" },
    EnvKeySpec { name: "KIRRA_FORWARDED_PROTO_HEADER", required: false, purpose: "Proxy proto header (default x-forwarded-proto)" },
    EnvKeySpec { name: "KIRRA_INSTANCE_ID", required: false, purpose: "Unique HA instance id (default hostname/machine-id)" },
    EnvKeySpec { name: "KIRRA_INSTANCE_ID_FILE", required: false, purpose: "Persistent instance-id file path" },
    EnvKeySpec { name: "KIRRA_HEARTBEAT_INTERVAL", required: false, purpose: "HA heartbeat write interval ms (rejects 0)" },
    EnvKeySpec { name: "KIRRA_PROMOTION_POLL", required: false, purpose: "Standby heartbeat poll interval ms" },
    EnvKeySpec { name: "KIRRA_PROMOTION_TIMEOUT", required: false, purpose: "Standby promotes if primary silent this long ms" },
    EnvKeySpec { name: "KIRRA_HA_LEASE_ENABLED", required: false, purpose: "EP-03 lease-based failover trigger (1/true; default off = legacy heartbeat timeout)" },
    EnvKeySpec { name: "KIRRA_FORCE_PROMOTE", required: false, purpose: "Force this instance Active (break-glass)" },
    EnvKeySpec { name: "KIRRA_LOG_SIGNING_KEY", required: false, purpose: "base64 Ed25519 seed for the audit signing key" },
    EnvKeySpec { name: "KIRRA_LOG_SIGNING_KEY_ADOPT", required: false, purpose: "Adopt a new/rotated audit signing key (opt-in)" },
    EnvKeySpec { name: "KIRRA_LOG_SIGNING_GENESIS_PIN", required: false, purpose: "Pin the durable audit-chain genesis" },
    EnvKeySpec { name: "KIRRA_TLS_CERT_PATH", required: false, purpose: "PEM cert-chain for in-process TLS (with KEY_PATH)" },
    EnvKeySpec { name: "KIRRA_TLS_KEY_PATH", required: false, purpose: "PEM private key for in-process TLS (with CERT_PATH)" },
    EnvKeySpec { name: "KIRRA_TLS_CLIENT_CA_PATH", required: false, purpose: "PEM client-CA → opt-in mTLS (server TLS must be on)" },
    EnvKeySpec { name: "KIRRA_HTTP_MAX_CONCURRENCY", required: false, purpose: "API-plane concurrency pool (default 512; load-shed 429)" },
    EnvKeySpec { name: "KIRRA_HTTP_CONSOLE_MAX_CONCURRENCY", required: false, purpose: "Console concurrency pool (default 64)" },
    EnvKeySpec { name: "KIRRA_HTTP_MAX_BODY_BYTES", required: false, purpose: "Request-body cap (default 262144; 413 over)" },
    EnvKeySpec { name: "KIRRA_CORS_ALLOWED_ORIGINS", required: false, purpose: "Comma-separated CORS allow-list (empty → deny)" },
    EnvKeySpec { name: "KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT", required: false, purpose: "WP-16 fleet default: omitted require_tpm_quote → quote-required" },
    EnvKeySpec { name: "KIRRA_AUDIT_SHIP_PATH", required: false, purpose: "WORM off-box audit-ship sink file (opt-in shipper)" },
    EnvKeySpec { name: "KIRRA_FABRIC_ASSET_ID", required: false, purpose: "Local fabric asset id for the verifier→fabric feed" },
    EnvKeySpec { name: "KIRRA_CAPTURE_ENABLED", required: false, purpose: "Enable the learning-loop capture writer (non-safety)" },
    EnvKeySpec { name: "KIRRA_CAPTURE_SINK_PATH", required: false, purpose: "Capture JSONL sink path" },
    EnvKeySpec { name: "KIRRA_CANOPEN_NODE_MAP", required: false, purpose: "CANopen node-id → fleet-node map (#84)" },
    EnvKeySpec { name: "KIRRA_CANOPEN_SDO_BOUNDS", required: false, purpose: "Per-target CANopen SDO magnitude bounds (#85)" },
    EnvKeySpec { name: "KIRRA_CANOPEN_STRICT_BOUNDS", required: false, purpose: "Deny an unconfigured CANopen SDO download (high-assurance)" },
    EnvKeySpec { name: "KIRRA_CIP_ATTR_BOUNDS", required: false, purpose: "Per-attribute CIP (EtherNet/IP) magnitude bounds (#85)" },
    EnvKeySpec { name: "KIRRA_CIP_STRICT_BOUNDS", required: false, purpose: "Deny an unconfigured CIP Set_Attribute_Single (high-assurance)" },
    EnvKeySpec { name: "KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE", required: false, purpose: "DNP3 Analog Output g41 magnitude envelope min:max" },
    EnvKeySpec { name: "KIRRA_GOVERNOR_SIGNING_KEY_SOURCE", required: false, purpose: "Governor release-signing key source (file/dev-fixed/tpm)" },
    EnvKeySpec { name: "KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV", required: false, purpose: "Admit the dev-fixed governor key source" },
];

/// True iff `name` is a registered `KIRRA_*` key.
#[must_use]
pub fn is_known_kirra_key(name: &str) -> bool {
    KIRRA_ENV_KEYS.iter().any(|k| k.name == name)
}

/// Pure unknown-variable sweep: given the process environment's variable NAMES,
/// return the `KIRRA_*` names the registry does not know — sorted and deduped.
/// A non-`KIRRA_`-prefixed name is ignored (the registry only governs our own
/// namespace). Pure over the iterator, so it is tested without touching real env.
pub fn unknown_kirra_env_vars<'a>(env_keys: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut unknown: Vec<String> = env_keys
        .filter(|k| k.starts_with("KIRRA_") && !is_known_kirra_key(k))
        .map(str::to_string)
        .collect();
    unknown.sort();
    unknown.dedup();
    unknown
}

/// Why the boot-time configuration was REFUSED — a malformed value in a
/// migrated `KIRRA_*` variable. EP-12's contract: a bad value fails at BOOT
/// (the binary logs this error and exits), never silently defaulting at use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// The offending variable name (e.g. `KIRRA_HEARTBEAT_INTERVAL`).
    pub key: &'static str,
    /// The rejected raw value (trimmed).
    pub value: String,
    /// What a valid value looks like.
    pub expected: &'static str,
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "invalid {}={:?} — expected {} (fail-closed: fix the environment and restart)",
            self.key, self.value, self.expected
        )
    }
}

impl std::error::Error for ConfigError {}

/// The RAW (pre-validation) boot values — one optional string per migrated
/// variable, exactly as the environment presents them. `Default` is "nothing
/// set". Pure data: [`EffectiveConfig::from_values`] consumes it without
/// touching the process environment, so the whole validation truth table is
/// unit-tested without `set_var` (INVARIANT #13).
#[derive(Debug, Clone, Default)]
pub struct RawConfig<'a> {
    pub verifier_addr: Option<&'a str>,
    pub db_path: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub vehicle_class: Option<&'a str>,
    pub tls_cert: Option<&'a str>,
    pub tls_key: Option<&'a str>,
    pub tls_client_ca: Option<&'a str>,
    pub trusted_ingress: Option<&'a str>,
    pub require_secure_transport: Option<&'a str>,
    pub require_tpm_quote_default: Option<&'a str>,
    pub audit_ship_path: Option<&'a str>,
    pub audit_signing_key: Option<&'a str>,
    // --- EP-12 Slice B: HA (standby_monitor / lease) ---
    pub heartbeat_interval_ms: Option<&'a str>,
    pub promotion_timeout_ms: Option<&'a str>,
    pub promotion_poll_ms: Option<&'a str>,
    pub force_promote: Option<&'a str>,
    pub ha_lease_enabled: Option<&'a str>,
    // --- EP-12 Slice B: instance identity (raw inputs; resolution is I/O) ---
    pub instance_id: Option<&'a str>,
    pub hostname: Option<&'a str>,
    pub instance_id_file: Option<&'a str>,
}

/// A versioned snapshot of the boot-time service configuration actually in effect.
/// Its [`effective_digest`](Self::effective_digest) is committed to the audit chain
/// at startup. Field order + set are FROZEN per [`CONFIG_SCHEMA_VERSION`]: the
/// digest is a canonical function of these values, so two instances that booted
/// under the same config produce the same digest, and any change is detectable.
///
/// Captures the stable boot knobs (not secrets — never the admin token or key
/// bytes; only whether a key source is configured). Per-INSTANCE identity (the
/// instance id and its fallback inputs) is carried for injection but
/// `#[serde(skip)]`-ped out of the digest, so two instances of one fleet booted
/// under the same fleet config still produce the same digest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EffectiveConfig {
    pub config_version: u32,
    pub verifier_addr: String,
    pub db_path: String,
    pub mode: String,
    pub vehicle_class: String,
    pub tls_enabled: bool,
    pub mtls_enabled: bool,
    pub trusted_ingress: bool,
    pub require_secure_transport: bool,
    pub require_tpm_quote_default: bool,
    pub audit_shipping_enabled: bool,
    pub audit_signing_key_configured: bool,
    // --- v2 (EP-12): validated HA timings + the audit-ship sink path ---
    pub heartbeat_interval_ms: u64,
    pub promotion_timeout_ms: u64,
    pub promotion_poll_ms: u64,
    pub force_promote: bool,
    pub ha_lease_enabled: bool,
    pub audit_ship_path: Option<String>,
    /// The validated typed vehicle class (same information as `vehicle_class`,
    /// already canonical there — skipped from the digest to keep the canonical
    /// encoding primitives-only).
    #[serde(skip)]
    pub vehicle_class_typed: crate::gateway::contract_profiles::VehicleClass,
    /// Per-instance identity inputs (raw): deliberately OUTSIDE the digest —
    /// identity is per-instance by design, and the digest compares fleet
    /// config. Resolved (with filesystem fallbacks) by
    /// [`Self::resolve_instance_id`].
    #[serde(skip)]
    pub instance_id_raw: Option<String>,
    #[serde(skip)]
    pub hostname_raw: Option<String>,
    #[serde(skip)]
    pub instance_id_file_raw: Option<String>,
}

/// A trimmed, non-empty view of an optional value (whitespace-only counts as unset).
fn non_empty(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// Parse a bool env value with the service-wide convention: `1` or `true`
/// (case-insensitive, trimmed) → true; everything else (incl. absent) → false.
fn env_flag(raw: Option<&str>) -> bool {
    raw.map(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
    .unwrap_or(false)
}

/// Parse a millisecond knob: absent/empty → `default`; a non-numeric value —
/// or `0` when `reject_zero` (a 0 disables the #689 promotion-floor clamp AND
/// panics `tokio::time::interval`) — is a boot-time [`ConfigError`].
fn parse_ms(
    key: &'static str,
    raw: Option<&str>,
    default: u64,
    reject_zero: bool,
) -> Result<u64, ConfigError> {
    let Some(v) = non_empty(raw) else { return Ok(default) };
    match v.parse::<u64>() {
        Ok(0) if reject_zero => Err(ConfigError {
            key,
            value: v.to_string(),
            expected: "a positive integer (milliseconds); 0 is rejected",
        }),
        Ok(n) => Ok(n),
        Err(_) => Err(ConfigError {
            key,
            value: v.to_string(),
            expected: "an integer number of milliseconds",
        }),
    }
}

/// Parse a STRICT boolean gate: absent/empty → `false`; `1`/`true` → `true`;
/// `0`/`false` → `false` (case-insensitive, trimmed); anything else is a
/// boot-time [`ConfigError`]. Stricter than [`env_flag`] on purpose — for the
/// migrated HA gates a typo (`ture`) silently reading as "off" would leave the
/// operator believing a failover feature is armed when it is not.
fn strict_flag(key: &'static str, raw: Option<&str>) -> Result<bool, ConfigError> {
    match non_empty(raw) {
        None => Ok(false),
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") => Ok(true),
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false") => Ok(false),
        Some(v) => Err(ConfigError {
            key,
            value: v.to_string(),
            expected: "1/true or 0/false",
        }),
    }
}

impl EffectiveConfig {
    /// Build the snapshot from already-extracted values (pure — no env, no I/O).
    /// Normalizes mode/vehicle-class casing and applies the documented defaults so
    /// the digest reflects the EFFECTIVE value, not the raw presence.
    ///
    /// VALIDATING (EP-12): a malformed value in a migrated variable — the vehicle
    /// class, any HA timing knob, or an HA gate — is a [`ConfigError`], and the
    /// binary treats it as a fatal boot error. A bad value can therefore never
    /// reach the module that would have silently defaulted it at use.
    pub fn from_values(raw: RawConfig<'_>) -> Result<Self, ConfigError> {
        let mode = match raw.mode.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "passive" | "passive_standby" | "standby" => "passive_standby",
            _ => "active",
        }
        .to_string();
        // TLS is on only when BOTH cert and key are present; mTLS additionally needs
        // the client CA. (A half-config is a fail-closed startup abort elsewhere; the
        // snapshot records the effective on/off the same way.)
        let tls_enabled = non_empty(raw.tls_cert).is_some() && non_empty(raw.tls_key).is_some();
        let mtls_enabled = tls_enabled && non_empty(raw.tls_client_ca).is_some();

        // #312 — the vehicle class is REQUIRED and validated here (fail at boot):
        // there is no default class; a wrong class would select another class's
        // envelope. (`init_vehicle_class` then only receives an already-valid class.)
        let class_raw = non_empty(raw.vehicle_class).unwrap_or("");
        let vehicle_class_typed = class_raw
            .parse::<crate::gateway::contract_profiles::VehicleClass>()
            .map_err(|_| ConfigError {
                key: "KIRRA_VEHICLE_CLASS",
                value: class_raw.to_string(),
                expected: "one of courier | delivery-av | robotaxi (no default — fail-closed)",
            })?;

        Ok(Self {
            config_version: CONFIG_SCHEMA_VERSION,
            verifier_addr: non_empty(raw.verifier_addr).unwrap_or("0.0.0.0:8090").to_string(),
            db_path: non_empty(raw.db_path).unwrap_or("kirra_verifier.sqlite").to_string(),
            mode,
            vehicle_class: vehicle_class_typed.as_str().to_string(),
            tls_enabled,
            mtls_enabled,
            trusted_ingress: env_flag(raw.trusted_ingress),
            require_secure_transport: env_flag(raw.require_secure_transport),
            require_tpm_quote_default: env_flag(raw.require_tpm_quote_default),
            audit_shipping_enabled: non_empty(raw.audit_ship_path).is_some(),
            audit_signing_key_configured: non_empty(raw.audit_signing_key).is_some(),
            heartbeat_interval_ms: parse_ms(
                "KIRRA_HEARTBEAT_INTERVAL",
                raw.heartbeat_interval_ms,
                crate::standby_monitor::HEARTBEAT_INTERVAL_MS,
                /* reject_zero (#707) */ true,
            )?,
            promotion_timeout_ms: parse_ms(
                "KIRRA_PROMOTION_TIMEOUT",
                raw.promotion_timeout_ms,
                crate::standby_monitor::PROMOTION_TIMEOUT_MS,
                // 0 is tolerated here: the #689 floor clamp at use raises any
                // too-small timeout to the safe minimum (and logs).
                false,
            )?,
            promotion_poll_ms: parse_ms(
                "KIRRA_PROMOTION_POLL",
                raw.promotion_poll_ms,
                crate::standby_monitor::PROMOTION_POLL_MS,
                /* reject_zero (tokio interval panics on 0) */ true,
            )?,
            force_promote: strict_flag("KIRRA_FORCE_PROMOTE", raw.force_promote)?,
            ha_lease_enabled: strict_flag("KIRRA_HA_LEASE_ENABLED", raw.ha_lease_enabled)?,
            audit_ship_path: non_empty(raw.audit_ship_path).map(str::to_string),
            vehicle_class_typed,
            instance_id_raw: non_empty(raw.instance_id).map(str::to_string),
            hostname_raw: non_empty(raw.hostname).map(str::to_string),
            instance_id_file_raw: non_empty(raw.instance_id_file).map(str::to_string),
        })
    }

    /// The production wrapper: read the environment once and build the snapshot.
    pub fn from_env() -> Result<Self, ConfigError> {
        let g = |k: &str| std::env::var(k).ok();
        let vals = [
            g("KIRRA_VERIFIER_ADDR"),
            g("KIRRA_DB_PATH"),
            g("KIRRA_VERIFIER_MODE"),
            g("KIRRA_VEHICLE_CLASS"),
            g("KIRRA_TLS_CERT_PATH"),
            g("KIRRA_TLS_KEY_PATH"),
            g("KIRRA_TLS_CLIENT_CA_PATH"),
            g("KIRRA_TRUSTED_INGRESS_MODE"),
            g("KIRRA_REQUIRE_SECURE_TRANSPORT"),
            g("KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT"),
            g("KIRRA_AUDIT_SHIP_PATH"),
            g("KIRRA_LOG_SIGNING_KEY"),
            g("KIRRA_HEARTBEAT_INTERVAL"),
            g("KIRRA_PROMOTION_TIMEOUT"),
            g("KIRRA_PROMOTION_POLL"),
            g("KIRRA_FORCE_PROMOTE"),
            g("KIRRA_HA_LEASE_ENABLED"),
            g("KIRRA_INSTANCE_ID"),
            g("HOSTNAME"),
            g("KIRRA_INSTANCE_ID_FILE"),
        ];
        Self::from_values(RawConfig {
            verifier_addr: vals[0].as_deref(),
            db_path: vals[1].as_deref(),
            mode: vals[2].as_deref(),
            vehicle_class: vals[3].as_deref(),
            tls_cert: vals[4].as_deref(),
            tls_key: vals[5].as_deref(),
            tls_client_ca: vals[6].as_deref(),
            trusted_ingress: vals[7].as_deref(),
            require_secure_transport: vals[8].as_deref(),
            require_tpm_quote_default: vals[9].as_deref(),
            audit_ship_path: vals[10].as_deref(),
            audit_signing_key: vals[11].as_deref(),
            heartbeat_interval_ms: vals[12].as_deref(),
            promotion_timeout_ms: vals[13].as_deref(),
            promotion_poll_ms: vals[14].as_deref(),
            force_promote: vals[15].as_deref(),
            ha_lease_enabled: vals[16].as_deref(),
            instance_id: vals[17].as_deref(),
            hostname: vals[18].as_deref(),
            instance_id_file: vals[19].as_deref(),
        })
    }

    /// The validated HA timing bundle the standby-monitor loops consume — the
    /// injection seam that replaced their per-loop env reads (EP-12).
    #[must_use]
    pub fn ha_timings(&self) -> crate::standby_monitor::HaTimings {
        crate::standby_monitor::HaTimings {
            heartbeat_interval_ms: self.heartbeat_interval_ms,
            promotion_timeout_ms: self.promotion_timeout_ms,
            promotion_poll_ms: self.promotion_poll_ms,
            force_promote: self.force_promote,
            lease: self
                .ha_lease_enabled
                .then(crate::lease::LeaseParams::default_params),
        }
    }

    /// Resolve this instance's HA identity from the captured raw inputs, with
    /// the documented filesystem fallbacks (machine-id → persisted id file →
    /// loud unstable last resort). The one deliberately-impure accessor:
    /// identity resolution touches the filesystem, so it runs once at boot in
    /// the binary, never in a hot path.
    #[must_use]
    pub fn resolve_instance_id(&self) -> String {
        crate::standby_monitor::resolve_instance_id(
            self.instance_id_raw.as_deref(),
            self.hostname_raw.as_deref(),
            self.instance_id_file_raw.as_deref(),
        )
    }

    /// A deterministic, canonical serialization of the snapshot — the exact bytes
    /// hashed for the digest. Uses `serde_json` (fields in declaration order, control
    /// chars escaped) so the encoding is INJECTIVE: two distinct `EffectiveConfig`
    /// values can never collide (a hand-rolled `k=v\n` format could, if a value
    /// contained `\nkey=`, undermining drift detection — Copilot #862). No secret
    /// bytes ever appear (the struct stores booleans like `audit_signing_key_configured`,
    /// never key material). Serialization of a primitives-only struct is infallible;
    /// the `unwrap_or_default` is a panic-free guard that never triggers in practice.
    fn canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The SHA-256 hex digest of the canonical snapshot (64 chars). Deterministic
    /// and change-sensitive: identical config → identical digest; any captured
    /// field change → a different digest.
    #[must_use]
    pub fn effective_digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.canonical_json().as_bytes());
        hex::encode(h.finalize())
    }
}

// NOTE (WP-17 → EP-12 progress ledger): Config Slice B (EP-12, v2) routed the
// FIRST module families through this struct — the gateway/actuator envelope
// class (`contract_profiles`: `init_vehicle_class` consumes the validated
// class), HA (`standby_monitor` + `lease`: the loops consume `ha_timings()`,
// identity via `resolve_instance_id()`), and the audit-shipper monitor
// (`spawn_audit_shipper` takes the captured path). Those modules now have ZERO
// direct env reads (greppable), and a malformed value in any of their
// variables fails at BOOT (`ConfigError`), not at use. STILL UN-MIGRATED
// (each remains its own fail-closed read): verifier.rs's three `from_env`
// structs, backpressure, the industrial adapters, the TLS resolver, and the
// audit signing-key/genesis-pin reads in the binary. Folding those in stays
// mechanical and versioned (each fold bumps CONFIG_SCHEMA_VERSION as the
// captured set grows).

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-v2 positional constructor shape, preserved for the existing
    /// tests (all of which pass VALID migrated values, so `unwrap` is fine —
    /// the new EP-12 tests below exercise the `Err` paths explicitly).
    #[allow(clippy::too_many_arguments)]
    fn legacy(
        verifier_addr: Option<&str>,
        db_path: Option<&str>,
        mode: Option<&str>,
        vehicle_class: Option<&str>,
        tls_cert: Option<&str>,
        tls_key: Option<&str>,
        tls_client_ca: Option<&str>,
        trusted_ingress: Option<&str>,
        require_secure_transport: Option<&str>,
        require_tpm_quote_default: Option<&str>,
        audit_ship_path: Option<&str>,
        audit_signing_key: Option<&str>,
    ) -> EffectiveConfig {
        EffectiveConfig::from_values(RawConfig {
            verifier_addr,
            db_path,
            mode,
            vehicle_class,
            tls_cert,
            tls_key,
            tls_client_ca,
            trusted_ingress,
            require_secure_transport,
            require_tpm_quote_default,
            audit_ship_path,
            audit_signing_key,
            ..RawConfig::default()
        })
        .expect("valid legacy-test config")
    }

    #[test]
    fn registry_has_no_duplicates_and_covers_the_core() {
        let mut names: Vec<&str> = KIRRA_ENV_KEYS.iter().map(|k| k.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "the env registry must have no duplicate keys");
        // Spot-check the load-bearing ones are registered.
        for k in [
            "KIRRA_ADMIN_TOKEN",
            "KIRRA_VEHICLE_CLASS",
            "KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT",
            "KIRRA_TLS_CERT_PATH",
        ] {
            assert!(is_known_kirra_key(k), "{k} must be in the registry");
        }
        assert!(!is_known_kirra_key("KIRRA_NOT_A_REAL_VAR"));
        assert!(!is_known_kirra_key("PATH"));
    }

    #[test]
    fn unknown_sweep_flags_only_unregistered_kirra_vars() {
        let env = [
            "KIRRA_ADMIN_TOKEN",   // known
            "KIRRA_ADMIN_TOEKN",   // typo → unknown
            "KIRRA_DB_PATH",       // known
            "KIRRA_MYSTERY",       // unknown
            "PATH",                // not KIRRA_ → ignored
            "HOME",                // ignored
        ];
        let unknown = unknown_kirra_env_vars(env.iter().copied());
        assert_eq!(unknown, vec!["KIRRA_ADMIN_TOEKN".to_string(), "KIRRA_MYSTERY".to_string()]);
    }

    #[test]
    fn unknown_sweep_is_empty_when_all_known() {
        let env = ["KIRRA_ADMIN_TOKEN", "KIRRA_VERIFIER_ADDR", "OTHER"];
        assert!(unknown_kirra_env_vars(env.iter().copied()).is_empty());
    }

    #[test]
    fn effective_config_applies_defaults_and_normalizes() {
        // Empty everything → the documented defaults + off flags.
        let c = legacy(
            None, None, None, Some("robotaxi"), None, None, None, None, None, None, None, None,
        );
        assert_eq!(c.verifier_addr, "0.0.0.0:8090");
        assert_eq!(c.db_path, "kirra_verifier.sqlite");
        assert_eq!(c.mode, "active");
        assert_eq!(c.vehicle_class, "robotaxi");
        assert!(!c.tls_enabled && !c.mtls_enabled);
        assert!(!c.require_tpm_quote_default);
        assert_eq!(c.config_version, CONFIG_SCHEMA_VERSION);

        // Standby aliases normalize; a whitespace addr counts as unset.
        let s = legacy(
            Some("   "), Some("/db"), Some("STANDBY"), Some("Courier"),
            None, None, None, None, None, None, None, None,
        );
        assert_eq!(s.verifier_addr, "0.0.0.0:8090");
        assert_eq!(s.mode, "passive_standby");
        assert_eq!(s.vehicle_class, "courier");
    }

    #[test]
    fn tls_requires_both_cert_and_key_mtls_also_ca() {
        let cert_only = legacy(
            None, None, None, Some("robotaxi"), Some("/c.pem"), None, None, None, None, None, None, None,
        );
        assert!(!cert_only.tls_enabled, "cert without key is not effective TLS (half-config aborts elsewhere)");

        let tls = legacy(
            None, None, None, Some("robotaxi"), Some("/c.pem"), Some("/k.pem"), None, None, None, None, None, None,
        );
        assert!(tls.tls_enabled && !tls.mtls_enabled);

        let mtls = legacy(
            None, None, None, Some("robotaxi"), Some("/c.pem"), Some("/k.pem"), Some("/ca.pem"),
            None, None, None, None, None,
        );
        assert!(mtls.tls_enabled && mtls.mtls_enabled);
    }

    #[test]
    fn flags_follow_the_one_true_convention() {
        let on = legacy(
            None, None, None, Some("robotaxi"), None, None, None,
            Some("1"), Some("TRUE"), Some(" true "), Some("/ship.log"), Some("seedbytes"),
        );
        assert!(on.trusted_ingress && on.require_secure_transport && on.require_tpm_quote_default);
        assert!(on.audit_shipping_enabled && on.audit_signing_key_configured);

        let off = legacy(
            None, None, None, Some("robotaxi"), None, None, None,
            Some("yes"), Some("0"), Some(""), Some("  "), None,
        );
        assert!(!off.trusted_ingress && !off.require_secure_transport && !off.require_tpm_quote_default);
        assert!(!off.audit_shipping_enabled, "a whitespace ship path is not enabled");
        assert!(!off.audit_signing_key_configured);
    }

    #[test]
    fn digest_is_deterministic_and_change_sensitive() {
        let a = legacy(
            Some("0.0.0.0:8090"), Some("/db"), Some("active"), Some("robotaxi"),
            None, None, None, None, None, None, None, None,
        );
        let b = a.clone();
        assert_eq!(a.effective_digest(), b.effective_digest(), "same config → same digest");
        assert_eq!(a.effective_digest().len(), 64, "sha-256 hex");

        // Any captured field change moves the digest.
        let c = legacy(
            Some("0.0.0.0:8090"), Some("/db"), Some("passive_standby"), Some("robotaxi"),
            None, None, None, None, None, None, None, None,
        );
        assert_ne!(a.effective_digest(), c.effective_digest(), "mode change → different digest");
    }

    #[test]
    fn signing_key_bytes_never_enter_the_digest_input() {
        // Construct WITH a signing-key secret present (Copilot #862: the prior test
        // passed a None key, so it could not have caught an accidental leak). Only the
        // `audit_signing_key_configured` boolean is captured — the bytes must be absent
        // from the exact input the digest hashes.
        let with_secret = legacy(
            Some("0.0.0.0:8090"), Some("/db"), Some("active"), Some("robotaxi"),
            None, None, None, None, None, None, None, Some("SUPER_SECRET_SEED_BYTES"),
        );
        assert!(with_secret.audit_signing_key_configured, "the key IS registered as configured");
        assert!(
            !with_secret.canonical_json().contains("SUPER_SECRET_SEED_BYTES"),
            "the signing-key bytes must NEVER appear in the digest input"
        );
        // And the digest reflects only the boolean: a config that differs ONLY by a
        // present-vs-absent key still moves the digest (configured is captured), but a
        // config with a DIFFERENT key value hashes identically (bytes not captured).
        let other_secret = legacy(
            Some("0.0.0.0:8090"), Some("/db"), Some("active"), Some("robotaxi"),
            None, None, None, None, None, None, None, Some("A_DIFFERENT_SEED"),
        );
        assert_eq!(
            with_secret.effective_digest(),
            other_secret.effective_digest(),
            "two different key VALUES hash the same — only 'configured' is captured"
        );
    }

    // -----------------------------------------------------------------------
    // EP-12 (Config Slice B) — a bad value in a MIGRATED variable fails at
    // BOOT (`from_values` → Err), never silently defaulting at use. One test
    // per migrated module family, all pure (no set_var — INVARIANT #13).
    // -----------------------------------------------------------------------

    /// A valid baseline every bad-value test perturbs one field of.
    fn valid_raw() -> RawConfig<'static> {
        RawConfig { vehicle_class: Some("robotaxi"), ..RawConfig::default() }
    }

    #[test]
    fn a_bad_vehicle_class_fails_at_boot() {
        // Gateway/actuator envelope class (contract_profiles): unset, empty,
        // and a near-miss typo are all refused — there is NO default class.
        for bad in [None, Some(""), Some("   "), Some("robotxi"), Some("fast")] {
            let err = EffectiveConfig::from_values(RawConfig {
                vehicle_class: bad,
                ..RawConfig::default()
            })
            .expect_err("an unset/unknown vehicle class must refuse boot");
            assert_eq!(err.key, "KIRRA_VEHICLE_CLASS", "{err}");
        }
        // The three valid classes normalize case and construct.
        for good in ["courier", "Delivery-AV", "ROBOTAXI"] {
            let c = EffectiveConfig::from_values(RawConfig {
                vehicle_class: Some(good),
                ..RawConfig::default()
            })
            .expect("a valid class constructs");
            assert_eq!(c.vehicle_class, c.vehicle_class_typed.as_str());
        }
    }

    #[test]
    fn a_bad_heartbeat_interval_fails_at_boot() {
        // HA (standby_monitor): non-numeric or zero (the #707 hazard — 0
        // disables the #689 clamp AND panics tokio::interval) refuse boot.
        for bad in ["abc", "0", "-5", "2.5", "2000ms"] {
            let err = EffectiveConfig::from_values(RawConfig {
                heartbeat_interval_ms: Some(bad),
                ..valid_raw()
            })
            .expect_err("a malformed heartbeat interval must refuse boot");
            assert_eq!(err.key, "KIRRA_HEARTBEAT_INTERVAL", "{err}");
        }
        // Absent → the documented default; a valid value is carried verbatim.
        let d = EffectiveConfig::from_values(valid_raw()).unwrap();
        assert_eq!(d.heartbeat_interval_ms, crate::standby_monitor::HEARTBEAT_INTERVAL_MS);
        let v = EffectiveConfig::from_values(RawConfig {
            heartbeat_interval_ms: Some("2500"),
            ..valid_raw()
        })
        .unwrap();
        assert_eq!(v.heartbeat_interval_ms, 2_500);
    }

    #[test]
    fn a_bad_promotion_poll_or_timeout_fails_at_boot() {
        for bad in ["fast", "0"] {
            let err = EffectiveConfig::from_values(RawConfig {
                promotion_poll_ms: Some(bad),
                ..valid_raw()
            })
            .expect_err("a malformed/zero promotion poll must refuse boot");
            assert_eq!(err.key, "KIRRA_PROMOTION_POLL", "{err}");
        }
        let err = EffectiveConfig::from_values(RawConfig {
            promotion_timeout_ms: Some("ten seconds"),
            ..valid_raw()
        })
        .expect_err("a non-numeric promotion timeout must refuse boot");
        assert_eq!(err.key, "KIRRA_PROMOTION_TIMEOUT");
        // Timeout 0 is admitted here: the #689 floor clamp at use raises it
        // (validated numeric, policy-clamped downstream — unchanged).
        let z = EffectiveConfig::from_values(RawConfig {
            promotion_timeout_ms: Some("0"),
            ..valid_raw()
        })
        .unwrap();
        assert_eq!(z.promotion_timeout_ms, 0);
    }

    #[test]
    fn a_bad_ha_gate_fails_at_boot_instead_of_silently_disarming() {
        // HA gates (lease.rs / force-promote): the STRICT flag — a typo like
        // "ture" must not read as "off" while the operator believes the
        // feature is armed.
        for (field_name, raw) in [
            ("KIRRA_FORCE_PROMOTE", RawConfig { force_promote: Some("maybe"), ..valid_raw() }),
            ("KIRRA_HA_LEASE_ENABLED", RawConfig { ha_lease_enabled: Some("ture"), ..valid_raw() }),
            ("KIRRA_HA_LEASE_ENABLED", RawConfig { ha_lease_enabled: Some("yes"), ..valid_raw() }),
        ] {
            let err = EffectiveConfig::from_values(raw)
                .expect_err("an unrecognized HA gate value must refuse boot");
            assert_eq!(err.key, field_name, "{err}");
        }
        // The documented on/off spellings still work; default is off.
        let on = EffectiveConfig::from_values(RawConfig {
            ha_lease_enabled: Some("TRUE"),
            force_promote: Some("1"),
            ..valid_raw()
        })
        .unwrap();
        assert!(on.ha_lease_enabled && on.force_promote);
        let off = EffectiveConfig::from_values(RawConfig {
            ha_lease_enabled: Some("0"),
            force_promote: Some("false"),
            ..valid_raw()
        })
        .unwrap();
        assert!(!off.ha_lease_enabled && !off.force_promote);
        assert!(!EffectiveConfig::from_values(valid_raw()).unwrap().ha_lease_enabled);
    }

    #[test]
    fn ha_timings_carries_the_validated_values_and_defaults_match() {
        // The injection seam the standby-monitor loops consume: defaults equal
        // HaTimings::default() (what an unset environment produced before), and
        // configured values arrive verbatim with the lease gate mapped to the
        // default-TTL LeaseParams.
        let d = EffectiveConfig::from_values(valid_raw()).unwrap().ha_timings();
        assert_eq!(d, crate::standby_monitor::HaTimings::default());

        let t = EffectiveConfig::from_values(RawConfig {
            heartbeat_interval_ms: Some("1000"),
            promotion_timeout_ms: Some("9000"),
            promotion_poll_ms: Some("500"),
            ha_lease_enabled: Some("1"),
            ..valid_raw()
        })
        .unwrap()
        .ha_timings();
        assert_eq!(t.heartbeat_interval_ms, 1_000);
        assert_eq!(t.promotion_timeout_ms, 9_000);
        assert_eq!(t.promotion_poll_ms, 500);
        assert_eq!(t.lease, Some(crate::lease::LeaseParams::default_params()));
        assert!(!t.force_promote);
    }

    #[test]
    fn v2_fields_move_the_digest_but_instance_identity_does_not() {
        // A captured v2 knob change (heartbeat cadence) moves the digest…
        let a = EffectiveConfig::from_values(valid_raw()).unwrap();
        let b = EffectiveConfig::from_values(RawConfig {
            heartbeat_interval_ms: Some("3000"),
            ..valid_raw()
        })
        .unwrap();
        assert_ne!(a.effective_digest(), b.effective_digest());

        // …but per-INSTANCE identity is serde-skipped: two instances of one
        // fleet differing only in identity produce the SAME digest (the digest
        // compares fleet config, and identity never enters the hashed bytes).
        let i1 = EffectiveConfig::from_values(RawConfig {
            instance_id: Some("node-a"),
            hostname: Some("host-a"),
            ..valid_raw()
        })
        .unwrap();
        let i2 = EffectiveConfig::from_values(RawConfig {
            instance_id: Some("node-b"),
            hostname: Some("host-b"),
            ..valid_raw()
        })
        .unwrap();
        assert_eq!(i1.effective_digest(), i2.effective_digest());
        assert!(!i1.canonical_json().contains("node-a"));
    }
}
