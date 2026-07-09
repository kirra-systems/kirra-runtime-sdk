// src/config.rs
//
// G18 config governance (WS-2): the industrial Modbus gateway's runtime config
// (`src/main.rs` → the water-flow PLC deployment). This surface is now VERSIONED
// (fail-closed on a schema newer than the binary understands) and carries an
// effective-config DIGEST — a stable fingerprint answering "which config is this
// process running?" for audit/attestation (docs/roadmap/PRODUCT_EXECUTION_PLAN.md).

use crate::kirra_core::ContractProfile;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The config-schema version THIS binary understands. A config declaring a HIGHER
/// version is refused: the binary cannot be sure it interprets a newer schema, so
/// it fails closed rather than mis-reading fields (e.g. a renamed safety bound).
/// Bump this — and document the migration — whenever the config shape changes.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// An unversioned config (pre-governance file) is treated as the CURRENT schema, so
/// existing `config/asset_profile.json`-style files still load. New configs SHOULD
/// declare `config_version` explicitly.
fn default_config_version() -> u32 {
    CONFIG_SCHEMA_VERSION
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkConfig {
    pub proxy_listen_port: u16,
    pub plc_target_port: u16,
    pub admin_reset_port: u16,
    pub metrics_http_port: u16,
    pub max_concurrent_connections: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TelemetryConfig {
    pub log_directory: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KirraRuntimeConfig {
    /// Config-schema version. Absent → the current schema (back-compat). Validated
    /// fail-closed: `0`, or a version NEWER than [`CONFIG_SCHEMA_VERSION`], is refused.
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    pub network: NetworkConfig,
    pub telemetry: TelemetryConfig,
    pub contract: ContractProfile,
}

impl KirraRuntimeConfig {
    /// SHA-256 (hex) of the canonical serialization of the effective config — a
    /// stable fingerprint for audit/attestation ("which config is this process
    /// running?"). Structs serialize in declaration order, so the encoding is
    /// deterministic.
    ///
    /// **Fail-closed:** a serialization failure returns `Err` rather than a
    /// valid-looking-but-wrong digest. `serde_json` refuses non-finite floats
    /// (`NaN`/`Inf`), so this can only fire on a config that never passed
    /// [`validate_safety_invariants`](Self::validate_safety_invariants) (which now
    /// rejects non-finite contract fields) — the caller treats it as a boot halt.
    pub fn effective_digest(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_string(self)
            .map_err(|e| format!("CONFIG_DIGEST_SERIALIZE_FAILED: {e}"))?;
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        Ok(hex::encode(hasher.finalize()))
    }

    pub fn validate_safety_invariants(&self) -> Result<(), &'static str> {
        // Schema-version gate FIRST (fail-closed): a config from an unknown future
        // schema must never be interpreted against this binary's field meanings.
        if self.config_version == 0 {
            return Err(
                "CONFIG_INVALID: config_version must be >= 1 (0 is not a valid schema version).",
            );
        }
        if self.config_version > CONFIG_SCHEMA_VERSION {
            return Err("CONFIG_INVALID: config_version is newer than this binary supports — refusing a config from an unknown future schema (fail-closed).");
        }

        let n = &self.network;
        let c = &self.contract;

        // Non-finite (NaN/Inf) safety bounds are meaningless AND slip past the
        // ordering checks below (every comparison with NaN is false), so reject them
        // explicitly. This also guarantees the config is JSON-serializable, so
        // `effective_digest` cannot fail on a validated config.
        let finite = [
            c.min_permissible_ceiling,
            c.max_permissible_ceiling,
            c.max_angular_velocity_ceiling,
            c.max_rate_of_change_dt,
            c.fallback_safe_setpoint,
            c.constraint_cap_min,
            c.constraint_cap_max,
            c.engineering_scale_factor,
        ];
        if finite.iter().any(|v| !v.is_finite()) {
            return Err("CONFIG_INVALID: contract safety bounds must all be finite (no NaN/Inf).");
        }

        let ports = [
            n.proxy_listen_port,
            n.plc_target_port,
            n.admin_reset_port,
            n.metrics_http_port,
        ];
        for i in 0..ports.len() {
            for j in (i + 1)..ports.len() {
                if ports[i] == ports[j] {
                    return Err("CONFIG_INVALID: Network ports must be completely distinct loopback channels.");
                }
            }
        }
        if n.max_concurrent_connections == 0 || n.max_concurrent_connections > 128 {
            return Err("CONFIG_INVALID: Thread pool limits must fall within the range [1, 128].");
        }
        if c.min_permissible_ceiling >= c.max_permissible_ceiling {
            return Err("CONFIG_INVALID: Minimum boundary envelope cannot equal or exceed Maximum boundary limits.");
        }
        if c.engineering_scale_factor <= 0.0 {
            return Err("CONFIG_INVALID: Engineering scale factor calculations must be strictly positive non-zero parameters.");
        }
        if c.max_rate_of_change_dt <= 0.001 {
            return Err("CONFIG_INVALID: Maximum tracking acceleration steps must exceed minimum threshold zones.");
        }
        if c.max_angular_velocity_ceiling <= 0.0 {
            return Err("CONFIG_INVALID: Maximum permitted turning angular rates must be strictly positive values.");
        }
        if c.fallback_safe_setpoint < c.min_permissible_ceiling
            || c.fallback_safe_setpoint > c.max_permissible_ceiling
        {
            return Err("CONFIG_INVALID: Fallback safe setpoint maps outside permissible core tracking boundaries.");
        }
        if c.constraint_cap_min < c.min_permissible_ceiling
            || c.constraint_cap_max > c.max_permissible_ceiling
        {
            return Err("CONFIG_INVALID: Posture tracking caps expand past absolute hard engineering bounds.");
        }
        if c.constraint_cap_min >= c.constraint_cap_max {
            return Err("CONFIG_INVALID: Degraded processing bounds parameters are logically inverted or equivalent.");
        }

        Ok(())
    }

    pub fn load_and_validate<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("FILE_OPEN_ERROR: {}", e))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| format!("FILE_READ_ERROR: {}", e))?;

        let parsed: Self = serde_json::from_str(&contents)
            .map_err(|e| format!("JSON_DESERIALIZE_ERROR: {}", e))?;
        parsed
            .validate_safety_invariants()
            .map_err(|e| e.to_string())?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A valid reference config (mirrors config/asset_profile.json's shape).
    fn valid_json(version_field: &str) -> String {
        format!(
            r#"{{
                {version_field}
                "network": {{
                    "proxy_listen_port": 5502, "plc_target_port": 5503,
                    "admin_reset_port": 5504, "metrics_http_port": 8080,
                    "max_concurrent_connections": 32
                }},
                "telemetry": {{ "log_directory": "/var/log/kirra" }},
                "contract": {{
                    "asset_register_offset": 10,
                    "min_permissible_ceiling": 1000.0, "max_permissible_ceiling": 3000.0,
                    "max_angular_velocity_ceiling": 1.5, "max_rate_of_change_dt": 100.0,
                    "fallback_safe_setpoint": 1200.0,
                    "constraint_cap_min": 1100.0, "constraint_cap_max": 2000.0,
                    "engineering_scale_factor": 10.0
                }}
            }}"#
        )
    }

    fn parse(json: &str) -> KirraRuntimeConfig {
        serde_json::from_str(json).expect("valid json")
    }

    #[test]
    fn absent_version_defaults_to_current_schema() {
        // Back-compat: a pre-governance (unversioned) config loads as the current schema.
        let cfg = parse(&valid_json(""));
        assert_eq!(cfg.config_version, CONFIG_SCHEMA_VERSION);
        assert!(cfg.validate_safety_invariants().is_ok());
    }

    #[test]
    fn explicit_current_version_is_accepted() {
        // Track CONFIG_SCHEMA_VERSION so this keeps testing "current version" after a bump.
        let cfg = parse(&valid_json(&format!(
            r#""config_version": {CONFIG_SCHEMA_VERSION},"#
        )));
        assert_eq!(cfg.config_version, CONFIG_SCHEMA_VERSION);
        assert!(cfg.validate_safety_invariants().is_ok());
    }

    #[test]
    fn zero_version_is_refused() {
        let cfg = parse(&valid_json(r#""config_version": 0,"#));
        assert!(cfg.validate_safety_invariants().is_err());
    }

    #[test]
    fn future_version_is_refused_fail_closed() {
        // A config from a newer schema than this binary understands must not run.
        let cfg = parse(&valid_json(&format!(
            r#""config_version": {},"#,
            CONFIG_SCHEMA_VERSION + 1
        )));
        let err = cfg.validate_safety_invariants().unwrap_err();
        assert!(
            err.contains("newer than this binary supports"),
            "got: {err}"
        );
    }

    #[test]
    fn digest_is_deterministic_and_change_sensitive() {
        let a = parse(&valid_json(r#""config_version": 1,"#));
        let b = parse(&valid_json(r#""config_version": 1,"#));
        let da = a.effective_digest().expect("digest");
        assert_eq!(
            da,
            b.effective_digest().expect("digest"),
            "same config → same digest"
        );
        assert_eq!(da.len(), 64, "sha-256 hex is 64 chars");

        // A changed safety bound changes the digest — the fingerprint tracks content.
        let mut c = a.clone();
        c.contract.max_permissible_ceiling = 3001.0;
        assert_ne!(da, c.effective_digest().expect("digest"));
    }

    #[test]
    fn non_finite_contract_bound_is_refused() {
        // A NaN safety bound is meaningless and slips past ordering checks (NaN
        // comparisons are all false) — validation must reject it explicitly, which
        // also keeps effective_digest serializable.
        let mut cfg = parse(&valid_json(r#""config_version": 1,"#));
        cfg.contract.max_permissible_ceiling = f64::NAN;
        let err = cfg.validate_safety_invariants().unwrap_err();
        assert!(err.contains("finite"), "got: {err}");
    }
}
