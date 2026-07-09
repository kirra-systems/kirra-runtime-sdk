//! WS-1 (#G7) — RBAC authorization, proven through the EXACT store↔authz
//! composition the `authorize_scope` middleware performs, WITHOUT process env or a
//! live router (INVARIANT #13 forbids `set_var` in the parallel runner).
//!
//! The middleware's real work is: mint/resolve a principal by token hash in the
//! store, map its role, then call the pure `authorize_request` predicate against a
//! configured admin root. This test drives that same composition against a real
//! in-memory `VerifierStore`, so it proves the seam the pure-function unit tests
//! (`authz::tests`) and the router-gating tests each cover only half of.

use kirra_verifier::authz::{
    authorize_request, token_sha256_hex, ApiRole, AuthzOutcome, ResolvedPrincipal,
    SCOPE_ACTUATOR_COMMAND, SCOPE_ADMIN, SCOPE_AUDIT_READ, SCOPE_INTEGRATION_EVALUATE,
};
use kirra_verifier::verifier_store::VerifierStore;

const ADMIN_ROOT: &str = "root-admin-token";

/// Mint a principal in the store (as the mint handler does: store the token HASH),
/// then resolve it back by hash exactly as the middleware does, into the
/// `ResolvedPrincipal` the pure predicate consumes.
fn mint_and_resolve(
    store: &mut VerifierStore,
    principal_id: &str,
    role: ApiRole,
    token: &str,
    revoke: bool,
) -> ResolvedPrincipal {
    let hash = token_sha256_hex(token);
    store
        .register_api_principal(principal_id, &hash, role.as_str(), 1_000)
        .expect("register");
    if revoke {
        assert!(store
            .revoke_api_principal(principal_id, 2_000)
            .expect("revoke"));
    }
    let rec = store
        .load_api_principal_by_token_hash(&hash)
        .expect("query")
        .expect("principal present");
    ResolvedPrincipal {
        role: ApiRole::parse_role(&rec.role),
        revoked: rec.revoked_at_ms.is_some(),
        principal_id: rec.principal_id,
    }
}

/// Resolve a presented token against the store the way the middleware does: the
/// admin root short-circuits (no lookup); otherwise look up by hash (miss → None).
fn resolve(store: &VerifierStore, token: &str) -> Option<ResolvedPrincipal> {
    if token == ADMIN_ROOT {
        return None; // admin path never consults the principal store
    }
    let hash = token_sha256_hex(token);
    store
        .load_api_principal_by_token_hash(&hash)
        .expect("query")
        .map(|rec| ResolvedPrincipal {
            role: ApiRole::parse_role(&rec.role),
            revoked: rec.revoked_at_ms.is_some(),
            principal_id: rec.principal_id,
        })
}

fn decide(store: &VerifierStore, scope: &str, token: &str) -> AuthzOutcome {
    let principal = resolve(store, token);
    authorize_request(scope, Some(ADMIN_ROOT), Some(token), principal.as_ref()).outcome
}

#[test]
fn integrator_principal_reaches_only_the_integration_surface() {
    let mut store = VerifierStore::new(":memory:").unwrap();
    mint_and_resolve(
        &mut store,
        "svc-integ",
        ApiRole::Integrator,
        "integ-tok",
        false,
    );

    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "integ-tok"),
        AuthzOutcome::Allow
    );
    // Least privilege: 403 on every other surface.
    for scope in [SCOPE_ADMIN, SCOPE_ACTUATOR_COMMAND, SCOPE_AUDIT_READ] {
        assert_eq!(
            decide(&store, scope, "integ-tok"),
            AuthzOutcome::Forbidden,
            "integrator must be forbidden on {scope}"
        );
    }
}

#[test]
fn auditor_principal_reaches_only_audit_read() {
    let mut store = VerifierStore::new(":memory:").unwrap();
    mint_and_resolve(
        &mut store,
        "svc-audit",
        ApiRole::Auditor,
        "audit-tok",
        false,
    );

    assert_eq!(
        decide(&store, SCOPE_AUDIT_READ, "audit-tok"),
        AuthzOutcome::Allow
    );
    for scope in [
        SCOPE_ADMIN,
        SCOPE_INTEGRATION_EVALUATE,
        SCOPE_ACTUATOR_COMMAND,
    ] {
        assert_eq!(decide(&store, scope, "audit-tok"), AuthzOutcome::Forbidden);
    }
}

#[test]
fn operator_principal_reaches_only_actuator_command() {
    let mut store = VerifierStore::new(":memory:").unwrap();
    mint_and_resolve(&mut store, "svc-op", ApiRole::Operator, "op-tok", false);

    assert_eq!(
        decide(&store, SCOPE_ACTUATOR_COMMAND, "op-tok"),
        AuthzOutcome::Allow
    );
    assert_eq!(
        decide(&store, SCOPE_ADMIN, "op-tok"),
        AuthzOutcome::Forbidden
    );
    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "op-tok"),
        AuthzOutcome::Forbidden
    );
}

#[test]
fn break_glass_admin_token_still_satisfies_every_group() {
    // Back-compat: an admin-token-only deployment (no principals minted) reaches
    // every gated group exactly as before this change.
    let store = VerifierStore::new(":memory:").unwrap();
    for scope in [
        SCOPE_ADMIN,
        SCOPE_INTEGRATION_EVALUATE,
        SCOPE_ACTUATOR_COMMAND,
        SCOPE_AUDIT_READ,
    ] {
        assert_eq!(
            decide(&store, scope, ADMIN_ROOT),
            AuthzOutcome::Allow,
            "admin token must satisfy {scope}"
        );
    }
}

#[test]
fn revoked_principal_token_is_unauthenticated_after_store_round_trip() {
    let mut store = VerifierStore::new(":memory:").unwrap();
    mint_and_resolve(&mut store, "svc-old", ApiRole::Integrator, "old-tok", true);
    // Even on its own scope, a revoked credential no longer authorizes.
    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "old-tok"),
        AuthzOutcome::Unauthenticated
    );
}

#[test]
fn unknown_token_is_unauthenticated() {
    let store = VerifierStore::new(":memory:").unwrap();
    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "never-minted"),
        AuthzOutcome::Unauthenticated
    );
}

#[test]
fn rotated_token_supersedes_the_old_one() {
    let mut store = VerifierStore::new(":memory:").unwrap();
    mint_and_resolve(&mut store, "svc-rot", ApiRole::Integrator, "tok-v1", false);
    // Rotate: same principal, new token, new role.
    mint_and_resolve(&mut store, "svc-rot", ApiRole::Auditor, "tok-v2", false);

    // The old token no longer resolves → unauthenticated.
    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "tok-v1"),
        AuthzOutcome::Unauthenticated
    );
    // The new token carries the new role's scope.
    assert_eq!(
        decide(&store, SCOPE_AUDIT_READ, "tok-v2"),
        AuthzOutcome::Allow
    );
    assert_eq!(
        decide(&store, SCOPE_INTEGRATION_EVALUATE, "tok-v2"),
        AuthzOutcome::Forbidden
    );
}
