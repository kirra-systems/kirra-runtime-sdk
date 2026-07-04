// src/authz.rs
//
// WS-1 · G7 — per-principal API tokens, roles, and scoped authorization.
//
// This module is the PURE authorization core: role→scope RBAC and the single
// fail-closed decision predicate `authorize_request`. It reads NO process env and
// touches NO store — exactly like `security::admin_token_ok`, the env (the
// `KIRRA_ADMIN_TOKEN` root) and the store (the per-principal token lookup) are
// lifted out to the calling middleware, so the decision truth-table is unit-tested
// without `set_var` (CRITICAL SECURITY INVARIANT #13 forbids it in the parallel
// test runner).
//
// Trust model (invariants preserved):
//   * The shared `KIRRA_ADMIN_TOKEN` is the ROOT of the mutation gate and is
//     RETAINED as a break-glass superuser (all scopes). Absent/empty → 503,
//     unconditionally, regardless of principals (INVARIANT #1/#6, verbatim). A
//     per-principal token only ADDS least-privilege capability on top of a
//     configured admin root; it never substitutes for it.
//   * A per-principal token is compared by HASHED lookup (SHA-256), never `==` on
//     the raw secret; the root admin token still goes through the constant-time
//     `admin_token_ok` (INVARIANT #2).

use crate::security::admin_token_ok;

/// API principal roles (least-privilege RBAC). Each role holds a fixed scope set;
/// a scoped principal token replaces sharing the root admin token for a whole
/// class of callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRole {
    /// Superuser — every scope. The break-glass `KIRRA_ADMIN_TOKEN` resolves to
    /// this role.
    Admin,
    /// The integration surface: action-filter / industrial / federation-submit /
    /// posture-stream (`identity_gated_routes`).
    Integrator,
    /// Read-only audit access: audit-chain verify / causal-verify / export.
    Auditor,
    /// Actuator command submission (`/actuator/motion/command`).
    Operator,
}

// --- Scope constants — one per gated route GROUP in `build_app`. -------------
/// Full admin mutation surface (`admin_routes`). Only [`ApiRole::Admin`] holds it.
pub const SCOPE_ADMIN: &str = "admin";
/// The identity-gated integration surface (`identity_gated_routes`).
pub const SCOPE_INTEGRATION_EVALUATE: &str = "integration:evaluate";
/// Actuator motion command submission (`actuator_routes`).
pub const SCOPE_ACTUATOR_COMMAND: &str = "actuator:command";
/// Read-only audit-chain verification / export (`auditor_routes`).
pub const SCOPE_AUDIT_READ: &str = "audit:read";

impl ApiRole {
    /// The wire/DB string for this role.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ApiRole::Admin => "admin",
            ApiRole::Integrator => "integrator",
            ApiRole::Auditor => "auditor",
            ApiRole::Operator => "operator",
        }
    }

    /// Parse a role string, fail-closed: an unknown/corrupt value yields `None`
    /// (the caller then denies — a garbled DB role never authorizes).
    #[must_use]
    pub fn parse_role(s: &str) -> Option<ApiRole> {
        match s {
            "admin" => Some(ApiRole::Admin),
            "integrator" => Some(ApiRole::Integrator),
            "auditor" => Some(ApiRole::Auditor),
            "operator" => Some(ApiRole::Operator),
            _ => None,
        }
    }

    /// The fixed scope set this role holds.
    #[must_use]
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            ApiRole::Admin => &[
                SCOPE_ADMIN,
                SCOPE_INTEGRATION_EVALUATE,
                SCOPE_ACTUATOR_COMMAND,
                SCOPE_AUDIT_READ,
            ],
            ApiRole::Integrator => &[SCOPE_INTEGRATION_EVALUATE],
            ApiRole::Auditor => &[SCOPE_AUDIT_READ],
            ApiRole::Operator => &[SCOPE_ACTUATOR_COMMAND],
        }
    }

    /// True iff this role holds `scope`.
    #[must_use]
    pub fn has_scope(self, scope: &str) -> bool {
        self.scopes().contains(&scope)
    }
}

/// The store lookup result for a presented API token (resolved by its hash).
/// Lifted out of [`authorize_request`] so the decision needs no store — the
/// middleware does the `load_api_principal_by_token_hash` and fills this in.
#[derive(Debug, Clone)]
pub struct ResolvedPrincipal {
    pub principal_id: String,
    /// `None` = the stored role string did not parse (corrupt) → fail-closed deny.
    pub role: Option<ApiRole>,
    /// `true` = a revoked credential; treated as unauthenticated (not a valid token).
    pub revoked: bool,
}

/// The authorization outcome; the middleware maps each to an HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzOutcome {
    /// Authorized — continue.
    Allow,
    /// `KIRRA_ADMIN_TOKEN` absent/empty → server unconfigured. HTTP 503
    /// (INVARIANT #1/#6, verbatim — never fail-open).
    Unconfigured,
    /// No / unknown / revoked credential. HTTP 401.
    Unauthenticated,
    /// Authenticated principal that lacks the required scope. HTTP 403.
    Forbidden,
}

/// The full decision, carrying the attribution the middleware logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzDecision {
    pub outcome: AuthzOutcome,
    /// `"admin-token"` (break-glass root) | `"api-principal"` | `"none"`.
    pub auth_method: &'static str,
    pub principal_id: Option<String>,
    pub role: Option<ApiRole>,
}

impl AuthzDecision {
    fn deny(outcome: AuthzOutcome, auth_method: &'static str) -> Self {
        AuthzDecision { outcome, auth_method, principal_id: None, role: None }
    }
}

/// THE fail-closed authorization predicate. Pure: no env, no store.
///
/// Precedence (each step denies unless it can positively authorize):
/// 1. `admin_configured` absent/empty → `Unconfigured` (503), regardless of
///    everything else — the admin token is the gate's root (INVARIANT #1/#6).
/// 2. no bearer token → `Unauthenticated` (401).
/// 3. bearer equals the root admin token (constant-time) → `Allow` as
///    [`ApiRole::Admin`] (break-glass; back-compat: an admin-token-only deployment
///    is byte-identical).
/// 4. otherwise the `principal` (resolved by token hash) decides:
///    revoked → 401; role holds `required_scope` → `Allow`; role lacks it → 403
///    (Forbidden); unknown token / corrupt role → 401.
#[must_use]
pub fn authorize_request(
    required_scope: &str,
    admin_configured: Option<&str>,
    provided_token: Option<&str>,
    principal: Option<&ResolvedPrincipal>,
) -> AuthzDecision {
    // 1. Fail-closed root check — the admin token gates the whole surface.
    let admin = match admin_configured {
        Some(c) if !c.is_empty() => c,
        _ => return AuthzDecision::deny(AuthzOutcome::Unconfigured, "none"),
    };

    // 2. A configured server with no presented credential → 401.
    let provided = match provided_token {
        Some(p) => p,
        None => return AuthzDecision::deny(AuthzOutcome::Unauthenticated, "none"),
    };

    // 3. Break-glass root admin token → Admin (all scopes). Constant-time (#2).
    if admin_token_ok(Some(provided), Some(admin)) {
        return AuthzDecision {
            outcome: AuthzOutcome::Allow,
            auth_method: "admin-token",
            principal_id: None,
            role: Some(ApiRole::Admin),
        };
    }

    // 4. Scoped per-principal token (looked up by hash by the caller).
    match principal {
        // A revoked credential is not a valid token.
        Some(p) if p.revoked => AuthzDecision {
            outcome: AuthzOutcome::Unauthenticated,
            auth_method: "api-principal",
            principal_id: Some(p.principal_id.clone()),
            role: p.role,
        },
        Some(p) => match p.role {
            Some(role) if role.has_scope(required_scope) => AuthzDecision {
                outcome: AuthzOutcome::Allow,
                auth_method: "api-principal",
                principal_id: Some(p.principal_id.clone()),
                role: Some(role),
            },
            // Authenticated but under-scoped → 403 (distinct from 401 so an
            // operator can tell "wrong token" from "insufficient privilege").
            Some(role) => AuthzDecision {
                outcome: AuthzOutcome::Forbidden,
                auth_method: "api-principal",
                principal_id: Some(p.principal_id.clone()),
                role: Some(role),
            },
            // Corrupt/unknown stored role → fail-closed deny.
            None => AuthzDecision {
                outcome: AuthzOutcome::Unauthenticated,
                auth_method: "api-principal",
                principal_id: Some(p.principal_id.clone()),
                role: None,
            },
        },
        // Bearer present, not the admin token, and no principal holds its hash.
        None => AuthzDecision::deny(AuthzOutcome::Unauthenticated, "none"),
    }
}

/// SHA-256 hex of an API token — the value STORED and looked up. The plaintext
/// token is shown once at mint and never persisted.
#[must_use]
pub fn token_sha256_hex(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// A short, stable fingerprint of a token for audit/log attribution WITHOUT
/// recording the token — the first 8 bytes of its SHA-256 (matches the
/// `admin_token_fingerprint` shape used elsewhere).
#[must_use]
pub fn token_fingerprint(token: &str) -> String {
    token_sha256_hex(token)[..16].to_string()
}

/// Mint a fresh 256-bit API token, hex-encoded (64 chars), from the OS CSPRNG.
///
/// Fail-closed: a CSPRNG failure returns `Err` so the caller can map it to a 5xx
/// rather than panicking — a transient entropy failure must not crash the process
/// (in `panic=abort` builds) or drop the request. A token is NEVER minted from a
/// degraded randomness source.
pub fn generate_api_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(id: &str, role: ApiRole, revoked: bool) -> ResolvedPrincipal {
        ResolvedPrincipal { principal_id: id.into(), role: Some(role), revoked }
    }

    // --- RBAC matrix ---------------------------------------------------------

    #[test]
    fn admin_holds_every_scope() {
        for s in [
            SCOPE_ADMIN,
            SCOPE_INTEGRATION_EVALUATE,
            SCOPE_ACTUATOR_COMMAND,
            SCOPE_AUDIT_READ,
        ] {
            assert!(ApiRole::Admin.has_scope(s), "admin must hold {s}");
        }
    }

    #[test]
    fn non_admin_roles_are_least_privilege() {
        assert!(ApiRole::Integrator.has_scope(SCOPE_INTEGRATION_EVALUATE));
        assert!(!ApiRole::Integrator.has_scope(SCOPE_ADMIN));
        assert!(!ApiRole::Integrator.has_scope(SCOPE_ACTUATOR_COMMAND));
        assert!(!ApiRole::Integrator.has_scope(SCOPE_AUDIT_READ));

        assert!(ApiRole::Auditor.has_scope(SCOPE_AUDIT_READ));
        assert!(!ApiRole::Auditor.has_scope(SCOPE_ADMIN));

        assert!(ApiRole::Operator.has_scope(SCOPE_ACTUATOR_COMMAND));
        assert!(!ApiRole::Operator.has_scope(SCOPE_ADMIN));
    }

    #[test]
    fn role_string_round_trips_and_rejects_unknown() {
        for r in [ApiRole::Admin, ApiRole::Integrator, ApiRole::Auditor, ApiRole::Operator] {
            assert_eq!(ApiRole::parse_role(r.as_str()), Some(r));
        }
        assert_eq!(ApiRole::parse_role("superuser"), None);
        assert_eq!(ApiRole::parse_role(""), None);
        assert_eq!(ApiRole::parse_role("Admin"), None, "role match is case-sensitive");
    }

    // --- authorize_request truth table --------------------------------------

    #[test]
    fn unconfigured_admin_token_is_503_regardless_of_principal() {
        // INVARIANT #1/#6 — absent/empty admin root → Unconfigured, even with a
        // valid scoped principal presenting a good token.
        let p = principal("svc-a", ApiRole::Integrator, false);
        for admin in [None, Some("")] {
            let d = authorize_request(SCOPE_INTEGRATION_EVALUATE, admin, Some("tok"), Some(&p));
            assert_eq!(d.outcome, AuthzOutcome::Unconfigured);
        }
    }

    #[test]
    fn configured_but_no_bearer_is_401() {
        let d = authorize_request(SCOPE_ADMIN, Some("root"), None, None);
        assert_eq!(d.outcome, AuthzOutcome::Unauthenticated);
        assert_eq!(d.auth_method, "none");
    }

    #[test]
    fn root_admin_token_allows_every_scope_as_break_glass() {
        for s in [SCOPE_ADMIN, SCOPE_INTEGRATION_EVALUATE, SCOPE_ACTUATOR_COMMAND, SCOPE_AUDIT_READ] {
            let d = authorize_request(s, Some("root-token"), Some("root-token"), None);
            assert_eq!(d.outcome, AuthzOutcome::Allow, "admin token must satisfy {s}");
            assert_eq!(d.auth_method, "admin-token");
            assert_eq!(d.role, Some(ApiRole::Admin));
        }
    }

    #[test]
    fn integrator_reaches_integration_but_not_admin_or_actuator() {
        let p = principal("svc-integ", ApiRole::Integrator, false);
        let allow = authorize_request(SCOPE_INTEGRATION_EVALUATE, Some("root"), Some("scoped"), Some(&p));
        assert_eq!(allow.outcome, AuthzOutcome::Allow);
        assert_eq!(allow.auth_method, "api-principal");
        assert_eq!(allow.principal_id.as_deref(), Some("svc-integ"));

        for s in [SCOPE_ADMIN, SCOPE_ACTUATOR_COMMAND, SCOPE_AUDIT_READ] {
            let d = authorize_request(s, Some("root"), Some("scoped"), Some(&p));
            assert_eq!(d.outcome, AuthzOutcome::Forbidden, "integrator must be 403 on {s}");
        }
    }

    #[test]
    fn auditor_reaches_audit_read_only() {
        let p = principal("svc-audit", ApiRole::Auditor, false);
        assert_eq!(
            authorize_request(SCOPE_AUDIT_READ, Some("root"), Some("scoped"), Some(&p)).outcome,
            AuthzOutcome::Allow
        );
        assert_eq!(
            authorize_request(SCOPE_ADMIN, Some("root"), Some("scoped"), Some(&p)).outcome,
            AuthzOutcome::Forbidden
        );
    }

    #[test]
    fn revoked_principal_is_401() {
        let p = principal("svc-old", ApiRole::Integrator, true);
        let d = authorize_request(SCOPE_INTEGRATION_EVALUATE, Some("root"), Some("scoped"), Some(&p));
        assert_eq!(d.outcome, AuthzOutcome::Unauthenticated);
    }

    #[test]
    fn unknown_token_is_401() {
        // Bearer present, not the admin token, no principal holds its hash.
        let d = authorize_request(SCOPE_INTEGRATION_EVALUATE, Some("root"), Some("mystery"), None);
        assert_eq!(d.outcome, AuthzOutcome::Unauthenticated);
    }

    #[test]
    fn corrupt_stored_role_fails_closed() {
        let p = ResolvedPrincipal { principal_id: "svc-x".into(), role: None, revoked: false };
        let d = authorize_request(SCOPE_INTEGRATION_EVALUATE, Some("root"), Some("scoped"), Some(&p));
        assert_eq!(d.outcome, AuthzOutcome::Unauthenticated);
    }

    // --- token helpers -------------------------------------------------------

    #[test]
    fn token_hash_is_stable_deterministic_and_distinct() {
        let a = token_sha256_hex("alpha");
        assert_eq!(a, token_sha256_hex("alpha"));
        assert_ne!(a, token_sha256_hex("beta"));
        assert_eq!(a.len(), 64, "sha-256 hex is 64 chars");
        assert_eq!(token_fingerprint("alpha"), a[..16]);
    }

    #[test]
    fn generated_tokens_are_unique_256_bit_hex() {
        let a = generate_api_token().expect("CSPRNG available in test");
        let b = generate_api_token().expect("CSPRNG available in test");
        assert_eq!(a.len(), 64);
        assert_ne!(a, b, "two mints must not collide");
    }
}
