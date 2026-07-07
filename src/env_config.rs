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
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

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

/// A versioned snapshot of the boot-time service configuration actually in effect.
/// Its [`effective_digest`](Self::effective_digest) is committed to the audit chain
/// at startup. Field order + set are FROZEN per [`CONFIG_SCHEMA_VERSION`]: the
/// digest is a canonical function of these values, so two instances that booted
/// under the same config produce the same digest, and any change is detectable.
///
/// Captures the stable boot knobs (not secrets — never the admin token or key
/// bytes; only whether a key source is configured). v1 is the service spine; the
/// captured set expands under a version bump as more reads fold in.
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

impl EffectiveConfig {
    /// Build the snapshot from already-extracted values (pure — no env). Normalizes
    /// mode/vehicle-class casing and applies the documented defaults for addr/db so
    /// the digest reflects the EFFECTIVE value, not the raw presence.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_values(
        verifier_addr: Option<&str>,
        db_path: Option<&str>,
        mode_raw: Option<&str>,
        vehicle_class: Option<&str>,
        tls_cert: Option<&str>,
        tls_key: Option<&str>,
        tls_client_ca: Option<&str>,
        trusted_ingress: Option<&str>,
        require_secure_transport: Option<&str>,
        require_tpm_quote_default: Option<&str>,
        audit_ship_path: Option<&str>,
        audit_signing_key: Option<&str>,
    ) -> Self {
        let mode = match mode_raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "passive" | "passive_standby" | "standby" => "passive_standby",
            _ => "active",
        }
        .to_string();
        // TLS is on only when BOTH cert and key are present; mTLS additionally needs
        // the client CA. (A half-config is a fail-closed startup abort elsewhere; the
        // snapshot records the effective on/off the same way.)
        let tls_enabled = non_empty(tls_cert).is_some() && non_empty(tls_key).is_some();
        let mtls_enabled = tls_enabled && non_empty(tls_client_ca).is_some();
        Self {
            config_version: CONFIG_SCHEMA_VERSION,
            verifier_addr: non_empty(verifier_addr).unwrap_or("0.0.0.0:8090").to_string(),
            db_path: non_empty(db_path).unwrap_or("kirra_verifier.sqlite").to_string(),
            mode,
            vehicle_class: non_empty(vehicle_class).unwrap_or("").to_ascii_lowercase(),
            tls_enabled,
            mtls_enabled,
            trusted_ingress: env_flag(trusted_ingress),
            require_secure_transport: env_flag(require_secure_transport),
            require_tpm_quote_default: env_flag(require_tpm_quote_default),
            audit_shipping_enabled: non_empty(audit_ship_path).is_some(),
            audit_signing_key_configured: non_empty(audit_signing_key).is_some(),
        }
    }

    /// The production wrapper: read the environment once and build the snapshot.
    #[must_use]
    pub fn from_env() -> Self {
        let g = |k: &str| std::env::var(k).ok();
        Self::from_values(
            g("KIRRA_VERIFIER_ADDR").as_deref(),
            g("KIRRA_DB_PATH").as_deref(),
            g("KIRRA_VERIFIER_MODE").as_deref(),
            g("KIRRA_VEHICLE_CLASS").as_deref(),
            g("KIRRA_TLS_CERT_PATH").as_deref(),
            g("KIRRA_TLS_KEY_PATH").as_deref(),
            g("KIRRA_TLS_CLIENT_CA_PATH").as_deref(),
            g("KIRRA_TRUSTED_INGRESS_MODE").as_deref(),
            g("KIRRA_REQUIRE_SECURE_TRANSPORT").as_deref(),
            g("KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT").as_deref(),
            g("KIRRA_AUDIT_SHIP_PATH").as_deref(),
            g("KIRRA_LOG_SIGNING_KEY").as_deref(),
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

// NOTE (WP-17 follow-up, recorded): the per-module env READS (verifier.rs's three
// `from_env` structs, standby_monitor, backpressure, the industrial adapters, the
// TLS resolver) are not yet routed THROUGH this struct — each stays its own
// fail-closed read for now. Folding them in field-by-field (so `EffectiveConfig`
// becomes the single loader every module borrows from) is mechanical and versioned
// (each fold bumps CONFIG_SCHEMA_VERSION as the captured set grows). This slice
// lands the registry, the sweep, and the audit-chained digest — the observability
// + single-source-of-truth spine — without a risky big-bang refactor.

#[cfg(test)]
mod tests {
    use super::*;

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
        let c = EffectiveConfig::from_values(
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
        let s = EffectiveConfig::from_values(
            Some("   "), Some("/db"), Some("STANDBY"), Some("Courier"),
            None, None, None, None, None, None, None, None,
        );
        assert_eq!(s.verifier_addr, "0.0.0.0:8090");
        assert_eq!(s.mode, "passive_standby");
        assert_eq!(s.vehicle_class, "courier");
    }

    #[test]
    fn tls_requires_both_cert_and_key_mtls_also_ca() {
        let cert_only = EffectiveConfig::from_values(
            None, None, None, Some("robotaxi"), Some("/c.pem"), None, None, None, None, None, None, None,
        );
        assert!(!cert_only.tls_enabled, "cert without key is not effective TLS (half-config aborts elsewhere)");

        let tls = EffectiveConfig::from_values(
            None, None, None, Some("robotaxi"), Some("/c.pem"), Some("/k.pem"), None, None, None, None, None, None,
        );
        assert!(tls.tls_enabled && !tls.mtls_enabled);

        let mtls = EffectiveConfig::from_values(
            None, None, None, Some("robotaxi"), Some("/c.pem"), Some("/k.pem"), Some("/ca.pem"),
            None, None, None, None, None,
        );
        assert!(mtls.tls_enabled && mtls.mtls_enabled);
    }

    #[test]
    fn flags_follow_the_one_true_convention() {
        let on = EffectiveConfig::from_values(
            None, None, None, Some("robotaxi"), None, None, None,
            Some("1"), Some("TRUE"), Some(" true "), Some("/ship.log"), Some("seedbytes"),
        );
        assert!(on.trusted_ingress && on.require_secure_transport && on.require_tpm_quote_default);
        assert!(on.audit_shipping_enabled && on.audit_signing_key_configured);

        let off = EffectiveConfig::from_values(
            None, None, None, Some("robotaxi"), None, None, None,
            Some("yes"), Some("0"), Some(""), Some("  "), None,
        );
        assert!(!off.trusted_ingress && !off.require_secure_transport && !off.require_tpm_quote_default);
        assert!(!off.audit_shipping_enabled, "a whitespace ship path is not enabled");
        assert!(!off.audit_signing_key_configured);
    }

    #[test]
    fn digest_is_deterministic_and_change_sensitive() {
        let a = EffectiveConfig::from_values(
            Some("0.0.0.0:8090"), Some("/db"), Some("active"), Some("robotaxi"),
            None, None, None, None, None, None, None, None,
        );
        let b = a.clone();
        assert_eq!(a.effective_digest(), b.effective_digest(), "same config → same digest");
        assert_eq!(a.effective_digest().len(), 64, "sha-256 hex");

        // Any captured field change moves the digest.
        let c = EffectiveConfig::from_values(
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
        let with_secret = EffectiveConfig::from_values(
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
        let other_secret = EffectiveConfig::from_values(
            Some("0.0.0.0:8090"), Some("/db"), Some("active"), Some("robotaxi"),
            None, None, None, None, None, None, None, Some("A_DIFFERENT_SEED"),
        );
        assert_eq!(
            with_secret.effective_digest(),
            other_secret.effective_digest(),
            "two different key VALUES hash the same — only 'configured' is captured"
        );
    }
}
