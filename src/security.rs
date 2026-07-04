// src/security.rs

use std::sync::atomic::{AtomicU8, Ordering};

pub struct VolatileZeroizer;

impl VolatileZeroizer {
    #[allow(clippy::needless_range_loop)]
    // Intentional: uses write_volatile to prevent the compiler from
    // optimizing away the zeroing loop as dead code after last use.
    // Replacing with iterator assignment (*item = 0) would allow the
    // optimizer to elide the write entirely, re-introducing a
    // memory-residue side channel for secrets cleared before drop.
    // This is the canonical Rust pattern for secret-zeroing (see zeroize crate).
    // Per CERT-005 RSR-007: security-critical behavior must not be
    // altered by style fixes. Do not auto-fix this lint.
    #[inline]
    pub fn zeroize(slice: &mut [u8]) {
        for i in 0..slice.len() {
            unsafe { std::ptr::write_volatile(&mut slice[i], 0u8); }
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

pub fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    let bitwise_accumulator = AtomicU8::new(0);
    let length_match = a.len() == b.len();
    let length_mask = if length_match { 0u8 } else { 0xFFu8 };

    // Cover the FULL length of both inputs (minimum 64 to preserve the prior
    // floor on work). A fixed 64-iteration loop silently ignored bytes past
    // index 64, so two distinct secrets sharing a 64-byte prefix compared equal
    // — a fail-open for any token longer than 64 bytes (KIRRA_ADMIN_TOKEN has no
    // length bound). The length_mask still forces a reject on a length mismatch.
    let span = a.len().max(b.len()).max(64);
    for i in 0..span {
        let byte_a = if i < a.len() { unsafe { std::ptr::read_volatile(&a[i]) } } else { 0u8 };
        let byte_b = if i < b.len() { unsafe { std::ptr::read_volatile(&b[i]) } } else { 0u8 };
        bitwise_accumulator.fetch_or(byte_a ^ byte_b, Ordering::SeqCst);
    }

    bitwise_accumulator.fetch_or(length_mask, Ordering::SeqCst);
    bitwise_accumulator.load(Ordering::SeqCst) == 0
}

/// SG-015 (ASIL B) — admin-token authorization decision, fail-closed.
///
/// Returns `true` (allow) only when a non-empty admin token is configured AND a
/// token was provided AND the two match under `constant_time_compare`. Every
/// other case denies:
///   - `configured` absent or empty  → deny (fail-closed; the caller maps this
///     to HTTP 503 per CRITICAL SECURITY INVARIANT #1/#6 — never fail-open),
///   - `provided` absent             → deny (no bearer credential),
///   - mismatch                      → deny.
///
/// The comparison goes through `constant_time_compare` (INVARIANT #2) — `==` is
/// never used on the secret. This is `pub` (not `pub(crate)`) because the
/// `require_admin_token` axum middleware lives in the `kirra_verifier_service`
/// BINARY crate, which links this library crate and must call it — exactly like
/// the sibling `pub fn constant_time_compare`. Unit-tested in-crate below.
//
// Verifies: SG-015
pub fn admin_token_ok(provided: Option<&str>, configured: Option<&str>) -> bool {
    // Fail-closed: a missing or empty configured token authorizes nothing.
    let configured = match configured {
        Some(c) if !c.is_empty() => c,
        _ => return false,
    };
    let provided = match provided {
        Some(p) => p,
        None => return false,
    };
    constant_time_compare(provided.as_bytes(), configured.as_bytes())
}

/// Env var naming the per-principal admin tokens (#G7).
pub const PRINCIPAL_TOKENS_ENV: &str = "KIRRA_PRINCIPAL_TOKENS";

/// The capability scope of a principal (#G7 slice 2 — per-route RBAC).
///
/// v2 is a coarse, method-based model that is enforceable in the single admin
/// middleware without restructuring the router: `ReadOnly` permits only
/// nullipotent (safe) HTTP methods on the admin surface, so a read-only monitoring
/// / audit token can read (`GET` audit-verify, fabric state, subsystem lists) but
/// is denied EVERY mutation and the actuator. `Admin` is unrestricted (the root
/// token's scope). Finer per-endpoint capabilities are a tracked follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    /// Full admin — every admin/actuator route (the root token's scope).
    #[default]
    Admin,
    /// Read-only — nullipotent methods only; denied all mutations + the actuator.
    ReadOnly,
}

impl Role {
    /// Parse a role token (case-insensitive). `None` for an unrecognized value so
    /// the caller can fail closed on a typo rather than silently granting Admin.
    #[must_use]
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(Role::Admin),
            "readonly" | "read-only" | "ro" => Some(Role::ReadOnly),
            _ => None,
        }
    }

    /// May this role perform a MUTATING request? `Admin` → yes; `ReadOnly` → no.
    /// The middleware classifies a method as mutating (anything not GET/HEAD/
    /// OPTIONS) and denies (403) a `ReadOnly` principal on a mutating route.
    #[must_use]
    pub fn permits_mutation(&self) -> bool {
        matches!(self, Role::Admin)
    }

    /// Lowercase label for logs/audit.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::ReadOnly => "readonly",
        }
    }
}

/// The authenticated caller identity resolved by the admin gate (#G7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminPrincipal {
    /// Authenticated with the root `KIRRA_ADMIN_TOKEN` — always [`Role::Admin`].
    Root,
    /// Authenticated with a registered per-principal token (carries its id, for
    /// audit attribution, and its RBAC [`Role`]).
    Named { id: String, role: Role },
}

impl AdminPrincipal {
    /// A short, log/audit-safe label for the principal (never the token).
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            AdminPrincipal::Root => "root",
            AdminPrincipal::Named { id, .. } => id.as_str(),
        }
    }

    /// The principal's RBAC role. The root token is always [`Role::Admin`].
    #[must_use]
    pub fn role(&self) -> Role {
        match self {
            AdminPrincipal::Root => Role::Admin,
            AdminPrincipal::Named { role, .. } => *role,
        }
    }
}

/// A registry of per-principal admin-equivalent tokens (#G7 — key/identity
/// lifecycle). It lets an operator issue, ROTATE, and REVOKE a token per
/// principal — and attribute each mutation to a named identity — WITHOUT sharing
/// the single root `KIRRA_ADMIN_TOKEN`, whose compromise otherwise exposes the
/// whole admin+actuator surface.
///
/// **Purely additive & fail-closed.** The root token still authorizes exactly as
/// before (INVARIANT #1/#6 unchanged), and a principal token NEVER authorizes
/// unless a non-empty root token is also configured — [`authorize_admin`] denies
/// before the registry is consulted when the root token is absent/empty, so this
/// extension cannot fail open. A principal carries a [`Role`] (#G7 slice 2): an
/// entry with no explicit role defaults to [`Role::Admin`] (backward-compatible
/// with slice 1, root-token-equivalent); an explicit but UNRECOGNIZED role drops
/// the entry (fail-closed against a typo silently granting Admin).
#[derive(Debug, Default, Clone)]
pub struct PrincipalRegistry {
    /// `(principal_id, role, token)` tuples. A `Vec` (not a map) so [`resolve`]
    /// compares against EVERY entry with no early-out that could leak set
    /// membership by timing. [`resolve`]: PrincipalRegistry::resolve
    principals: Vec<(String, Role, String)>,
}

impl PrincipalRegistry {
    /// Load from `KIRRA_PRINCIPAL_TOKENS` (the process environment).
    #[must_use]
    pub fn from_env() -> Self {
        Self::parse(std::env::var(PRINCIPAL_TOKENS_ENV).ok().as_deref())
    }

    /// Pure parser (the testable core of [`from_env`]). Entries are
    /// `principal_id[:role]=token`, separated by commas, semicolons, or newlines
    /// and trimmed of surrounding whitespace. The `role` is optional (`admin` |
    /// `readonly`, case-insensitive); when omitted it defaults to [`Role::Admin`]
    /// (backward-compatible with slice 1). An entry is IGNORED — never a usable
    /// credential (fail-closed against malformed config) — when it has no `=`, an
    /// empty id, an empty token, OR an explicit-but-UNRECOGNIZED role (so a typo'd
    /// role can never silently become Admin). Duplicate ids are allowed (each token
    /// is independently valid, supporting overlapping-window rotation).
    #[must_use]
    pub fn parse(spec: Option<&str>) -> Self {
        let mut principals = Vec::new();
        if let Some(spec) = spec {
            for entry in spec.split([',', ';', '\n']) {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                // Split on the FIRST '=' only — tokens may themselves contain '='.
                let Some((id_role, token)) = entry.split_once('=') else {
                    continue; // no '=' → not a principal=token pair
                };
                let token = token.trim();
                if token.is_empty() {
                    continue; // empty token is never a credential
                }
                // The id part may carry an optional `:role` suffix (first ':' splits).
                let (id, role) = match id_role.split_once(':') {
                    Some((id, role_str)) => match Role::parse(role_str) {
                        Some(role) => (id.trim(), role),
                        None => continue, // explicit but unrecognized role → drop (fail-closed)
                    },
                    None => (id_role.trim(), Role::default()), // no role → Admin
                };
                if id.is_empty() {
                    continue; // empty id is never a credential
                }
                principals.push((id.to_string(), role, token.to_string()));
            }
        }
        Self { principals }
    }

    /// Number of registered principal-token ENTRIES (duplicates allowed for
    /// overlapping-window rotation, so this is a count of tokens, not unique ids).
    #[must_use]
    pub fn len(&self) -> usize {
        self.principals.len()
    }

    /// Is the registry empty (no per-principal tokens configured)?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    /// Constant-time resolve: return the principal id whose token matches
    /// `provided`, comparing against EVERY entry via [`constant_time_compare`]
    /// with NO early-out (so a match does not leak its position by timing).
    ///
    /// `None` if nothing matches. Parse forbids empty tokens, so an empty
    /// `provided` can never match. A token that matches the SAME (id, role) more
    /// than once (overlapping-window rotation) resolves to it; a token that matches
    /// MULTIPLE DISTINCT (id, role) pairs is a misconfiguration whose attribution
    /// or scope would be ambiguous, so it is treated as **deny** (`None`) —
    /// fail-closed, never a non-deterministic audit identity OR privilege (Copilot
    /// #802). Returns the matched `(id, role)` on success.
    #[must_use]
    pub fn resolve(&self, provided: &str) -> Option<(&str, Role)> {
        let mut matched: Option<(&str, Role)> = None;
        let mut ambiguous = false;
        for (id, role, token) in &self.principals {
            // Deliberately no `break` — evaluate all entries in constant work.
            if constant_time_compare(provided.as_bytes(), token.as_bytes()) {
                match matched {
                    None => matched = Some((id.as_str(), *role)),
                    // Same (id, role) repeated (rotation) is fine; any distinct
                    // id OR role is ambiguous (never pick a scope non-deterministically).
                    Some((pid, prole)) if pid != id.as_str() || prole != *role => ambiguous = true,
                    Some(_) => {}
                }
            }
        }
        if ambiguous {
            None
        } else {
            matched
        }
    }
}

/// Fail-closed admin authorization (#G7) — the single decision the
/// `require_admin_token` middleware gates on, extending [`admin_token_ok`] with
/// the per-principal [`PrincipalRegistry`].
///
/// Returns `Some(principal)` (allow, with attribution) ONLY when a non-empty root
/// admin token is configured AND the provided bearer either matches the root
/// token OR a registered principal token. Every other case denies (`None`):
///   - configured root token absent/empty → `None` (caller → HTTP 503; INV #1/#6),
///   - provided absent                    → `None` (401),
///   - no match in root or registry       → `None` (401).
///
/// The root token is authorized via `admin_token_ok` FIRST; the registry is only
/// consulted when a non-empty root token IS configured, so a principal token can
/// never authorize without a root token — this extension cannot fail open.
//
// Verifies: SG-015 (extended)
#[must_use]
pub fn authorize_admin(
    provided: Option<&str>,
    configured_admin: Option<&str>,
    registry: &PrincipalRegistry,
) -> Option<AdminPrincipal> {
    // Root token path — the exact fail-closed predicate (INV #1/#2/#6).
    if admin_token_ok(provided, configured_admin) {
        return Some(AdminPrincipal::Root);
    }
    // Principal tokens are additive and ONLY valid when a non-empty root token is
    // configured. If the root token is absent/empty, deny here — never fall
    // through to the registry (that would be a fail-open with no root configured).
    match configured_admin {
        Some(c) if !c.is_empty() => {}
        _ => return None,
    }
    let provided = provided?;
    registry
        .resolve(provided)
        .map(|(id, role)| AdminPrincipal::Named { id: id.to_string(), role })
}

/// The #G7-slice-2 RBAC decision applied AFTER authentication: may a principal
/// with `role` issue a request with HTTP method `method` on the admin surface?
///
/// A nullipotent (safe) method — `GET` / `HEAD` / `OPTIONS` — is always allowed;
/// any other (mutating) method requires a role that [`Role::permits_mutation`].
/// So a `ReadOnly` principal reads the admin GETs (audit-verify, fabric state,
/// subsystem lists) but is denied every POST mutation and the actuator (all POST).
/// Pure so the middleware's decision is unit-testable without process env.
#[must_use]
pub fn admin_rbac_allows(role: Role, method: &str) -> bool {
    let mutating = !matches!(method, "GET" | "HEAD" | "OPTIONS");
    !mutating || role.permits_mutation()
}

pub struct AdministrativeKeyContainer {
    private_auth_key: Vec<u8>,
}

impl AdministrativeKeyContainer {
    pub fn new(initial_key: Vec<u8>) -> Self {
        Self { private_auth_key: initial_key }
    }

    pub fn rotate_key(&mut self, new_key: Vec<u8>) {
        let old_key = std::mem::replace(&mut self.private_auth_key, new_key);
        let mut old_to_zeroize = old_key;
        VolatileZeroizer::zeroize(&mut old_to_zeroize);
    }

    #[inline]
    pub fn verify_token_constant_time(&self, raw_token: &[u8]) -> bool {
        constant_time_compare(raw_token, &self.private_auth_key)
    }
}

impl Drop for AdministrativeKeyContainer {
    fn drop(&mut self) {
        VolatileZeroizer::zeroize(&mut self.private_auth_key);
    }
}

// ---------------------------------------------------------------------------
// SG-015 (ASIL B) — admin token absent fail-closed
// ---------------------------------------------------------------------------
//
// Verifies: SG-015. `admin_token_ok` is the decision `require_admin_token`
// gates on; the middleware keeps mapping "configured absent/empty" to HTTP 503
// and "provided absent / mismatch" to 401, but the comparison itself now goes
// through this single constant-time predicate. These tests pin the fail-closed
// truth table without touching process env vars (forbidden in the multithreaded
// test runner — CRITICAL SECURITY INVARIANT #13), which is exactly why the env
// indirection was factored out into a pure function.
#[cfg(test)]
mod sg_015_admin_token_tests {
    use super::admin_token_ok;

    #[test]
    fn test_absent_configured_token_denies() {
        // Mirrors KIRRA_ADMIN_TOKEN unset → fail-closed (caller → 503).
        assert!(!admin_token_ok(Some("anything"), None),
            "no configured admin token must deny (fail-closed)");
    }

    #[test]
    fn test_empty_configured_token_denies() {
        // Mirrors KIRRA_ADMIN_TOKEN="" → fail-closed (caller → 503).
        assert!(!admin_token_ok(Some("anything"), Some("")),
            "empty configured admin token must deny (fail-closed)");
        // Empty configured denies even an empty provided token (no fail-open).
        assert!(!admin_token_ok(Some(""), Some("")),
            "empty==empty must NOT authorize");
        assert!(!admin_token_ok(None, Some("")),
            "empty configured denies regardless of provided");
    }

    #[test]
    fn test_absent_provided_token_denies() {
        assert!(!admin_token_ok(None, Some("s3cret-admin-token")),
            "a request with no bearer token must deny");
    }

    #[test]
    fn test_wrong_provided_token_denies() {
        assert!(!admin_token_ok(Some("wrong"), Some("s3cret-admin-token")),
            "a mismatched token must deny");
        // Length-mismatch path of constant_time_compare also denies.
        assert!(!admin_token_ok(Some("s3cret-admin-token-extra"), Some("s3cret-admin-token")),
            "a longer-but-prefix-matching token must deny");
    }

    #[test]
    fn test_correct_token_allows() {
        assert!(admin_token_ok(Some("s3cret-admin-token"), Some("s3cret-admin-token")),
            "the exact configured token must authorize");
    }

    #[test]
    fn test_long_tokens_sharing_64_byte_prefix_are_distinguished() {
        // Regression: a fixed 64-iteration loop ignored bytes past index 64, so
        // two distinct >64-byte secrets sharing a 64-byte prefix compared equal.
        use super::constant_time_compare;
        let prefix = "A".repeat(64);
        let a = format!("{prefix}aaaaaaaaaa");
        let b = format!("{prefix}bbbbbbbbbb");
        assert_eq!(a.len(), b.len(), "same length isolates the prefix-only bug");
        assert!(!constant_time_compare(a.as_bytes(), b.as_bytes()),
            "tokens differing only past byte 64 must NOT compare equal");
        assert!(constant_time_compare(a.as_bytes(), a.as_bytes()),
            "an identical >64-byte token must still compare equal");
    }
}

// ---------------------------------------------------------------------------
// #G7 — per-principal admin tokens (rotation / revocation / attribution)
// ---------------------------------------------------------------------------
//
// The registry and `authorize_admin` are pure (INVARIANT #13 — no env in the
// multithreaded test runner; env is read only by the thin `from_env` wrapper).
// The LOAD-BEARING invariant these pin: a per-principal token can NEVER authorize
// when the root KIRRA_ADMIN_TOKEN is absent/empty (INV #1/#6 preserved — no
// fail-open), and the root token still authorizes exactly as before.
#[cfg(test)]
mod g7_principal_token_tests {
    use super::{authorize_admin, AdminPrincipal, PrincipalRegistry, Role};

    fn registry() -> PrincipalRegistry {
        PrincipalRegistry::parse(Some("alice=alice-token, bob = bob-token ;carol=carol=eq"))
    }

    #[test]
    fn parse_keeps_wellformed_drops_malformed() {
        let r = registry();
        assert_eq!(r.len(), 3, "alice, bob, carol are well-formed");
        // No explicit role → Admin (backward-compatible with slice 1).
        assert_eq!(r.resolve("alice-token"), Some(("alice", Role::Admin)));
        assert_eq!(r.resolve("bob-token"), Some(("bob", Role::Admin)), "surrounding whitespace trimmed");
        assert_eq!(r.resolve("carol=eq"), Some(("carol", Role::Admin)), "only the FIRST '=' splits id from token");
        assert_eq!(r.resolve("nope"), None);

        // Malformed entries are dropped (never a usable credential).
        let bad = PrincipalRegistry::parse(Some("no-equals, =empty-id, empty-token=, ,  "));
        assert!(bad.is_empty(), "no '=', empty id, and empty token are all dropped");
        assert_eq!(PrincipalRegistry::parse(None).len(), 0);
    }

    #[test]
    fn resolve_empty_provided_never_matches() {
        assert_eq!(registry().resolve(""), None, "parse forbids empty tokens, so '' matches nothing");
    }

    /// #G7 slice 2 — explicit roles parse; a readonly principal is scoped; an
    /// explicit-but-unrecognized role drops the entry (fail-closed, never Admin).
    #[test]
    fn parse_roles_and_scope() {
        let r = PrincipalRegistry::parse(Some(
            "monitor:readonly=mon-tok, ops:admin=ops-tok, legacy=leg-tok, typo:supervisor=bad-tok",
        ));
        assert_eq!(r.len(), 3, "the unrecognized-role entry is dropped");
        assert_eq!(r.resolve("mon-tok"), Some(("monitor", Role::ReadOnly)));
        assert_eq!(r.resolve("ops-tok"), Some(("ops", Role::Admin)));
        assert_eq!(r.resolve("leg-tok"), Some(("legacy", Role::Admin)), "no role → Admin default");
        assert_eq!(r.resolve("bad-tok"), None, "typo'd role never becomes a credential");
        // The scope predicate the middleware enforces.
        assert!(Role::Admin.permits_mutation());
        assert!(!Role::ReadOnly.permits_mutation(), "read-only must not mutate");
        assert_eq!(Role::parse("RO"), Some(Role::ReadOnly));
        assert_eq!(Role::parse("nonsense"), None);
    }

    #[test]
    fn same_id_rotation_resolves_but_distinct_id_collision_denies() {
        // Overlapping-window rotation: SAME id, two tokens → both resolve to the id.
        let rot = PrincipalRegistry::parse(Some("alice=old-tok, alice=new-tok"));
        assert_eq!(rot.resolve("old-tok"), Some(("alice", Role::Admin)));
        assert_eq!(rot.resolve("new-tok"), Some(("alice", Role::Admin)));

        // Misconfiguration: the SAME token for DISTINCT ids → ambiguous attribution
        // → deny (fail-closed, Copilot #802). A unique token still resolves.
        let collide = PrincipalRegistry::parse(Some("alice=shared, bob=shared, carol=carol-tok"));
        assert_eq!(collide.resolve("shared"), None, "a token mapping to 2 ids must deny");
        assert_eq!(collide.resolve("carol-tok"), Some(("carol", Role::Admin)), "unambiguous tokens still resolve");

        // #G7 slice 2 — same id but DISTINCT ROLE for one token is also ambiguous
        // (never pick a privilege non-deterministically) → deny.
        let role_collide = PrincipalRegistry::parse(Some("dave:admin=d-tok, dave:readonly=d-tok"));
        assert_eq!(role_collide.resolve("d-tok"), None, "a token mapping to 2 roles must deny");
    }

    #[test]
    fn authorize_root_token_is_root_principal() {
        let r = registry();
        let p = authorize_admin(Some("root-secret"), Some("root-secret"), &r);
        assert_eq!(p, Some(AdminPrincipal::Root));
        assert_eq!(p.unwrap().role(), Role::Admin, "the root token is always Admin");
    }

    #[test]
    fn authorize_principal_token_is_named_and_attributed() {
        let r = PrincipalRegistry::parse(Some("bob=bob-token, monitor:readonly=mon-token"));
        assert_eq!(
            authorize_admin(Some("bob-token"), Some("root-secret"), &r),
            Some(AdminPrincipal::Named { id: "bob".to_string(), role: Role::Admin }),
            "a registered principal token authorizes and is attributed to its id + role"
        );
        // A readonly principal authenticates AND carries its scoped role.
        let mon = authorize_admin(Some("mon-token"), Some("root-secret"), &r).unwrap();
        assert_eq!(mon.label(), "monitor");
        assert_eq!(mon.role(), Role::ReadOnly);
        assert!(!mon.role().permits_mutation(), "the middleware will 403 this principal on a mutation");
        assert_eq!(
            authorize_admin(Some("unknown-token"), Some("root-secret"), &r),
            None,
            "an unregistered token denies"
        );
        assert_eq!(authorize_admin(None, Some("root-secret"), &r), None, "no bearer denies");
    }

    /// #G7 slice 2 — the RBAC method decision the middleware applies. Admin may
    /// mutate; ReadOnly is confined to nullipotent methods (so a read-only token
    /// is 403'd on every POST admin route AND the actuator).
    #[test]
    fn admin_rbac_allows_read_only_only_on_safe_methods() {
        use super::admin_rbac_allows;
        for m in ["GET", "HEAD", "OPTIONS", "POST", "PUT", "DELETE", "PATCH"] {
            assert!(admin_rbac_allows(Role::Admin, m), "admin may {m}");
        }
        for m in ["GET", "HEAD", "OPTIONS"] {
            assert!(admin_rbac_allows(Role::ReadOnly, m), "read-only may {m}");
        }
        for m in ["POST", "PUT", "DELETE", "PATCH"] {
            assert!(!admin_rbac_allows(Role::ReadOnly, m), "read-only must NOT {m}");
        }
    }

    /// THE load-bearing #G7 invariant (INV #1/#6): with NO root admin token
    /// configured, a valid PRINCIPAL token must STILL be denied — the whole admin
    /// surface is 503 without the root token, and the registry can never fail open.
    #[test]
    fn principal_token_denied_when_root_token_absent_or_empty() {
        let r = registry();
        assert_eq!(
            authorize_admin(Some("bob-token"), None, &r),
            None,
            "no root token configured → a principal token must NOT authorize (INV #6)"
        );
        assert_eq!(
            authorize_admin(Some("bob-token"), Some(""), &r),
            None,
            "empty root token → a principal token must NOT authorize (INV #6)"
        );
        // And the root token itself is still denied when unconfigured.
        assert_eq!(authorize_admin(Some("anything"), None, &r), None);
    }
}
