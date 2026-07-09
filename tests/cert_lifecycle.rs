//! WP-15 (MGA G-19) — mTLS cert-principal LIFECYCLE, proven through the exact
//! store↔authz composition the `authorize_scope` middleware performs for the
//! Track 1.2 client-cert path, WITHOUT process env or a live TLS listener
//! (INVARIANT #13 forbids `set_var` in the parallel runner).
//!
//! The middleware's real work on the mTLS path (see `auth.rs`): with NO bearer
//! presented, resolve a `cert_principals` row by the verified leaf's SHA-256
//! fingerprint, fold BOTH revocation AND expiry (`is_expired(now)`) into the
//! single invalid-credential flag, then call the pure `authorize_request`
//! predicate with `provided_token = None`. This test drives that same composition
//! against a real in-memory `VerifierStore` across time, so it proves the seam the
//! pure-predicate unit tests (`authz::tests`) and the TLS-fingerprint tests
//! (`tls.rs`) each cover only half of.

use kirra_verifier::authz::SCOPE_INTEGRATION_EVALUATE;
use kirra_verifier::authz::{authorize_request, ApiRole, AuthzOutcome, ResolvedPrincipal};
use kirra_verifier::verifier_store::VerifierStore;

const ADMIN_ROOT: &str = "root-admin-token";
const FP: &str = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44";

fn store() -> VerifierStore {
    VerifierStore::new(":memory:").expect("in-memory store")
}

/// Resolve a cert principal by fingerprint EXACTLY as the mTLS middleware does at
/// `now_ms`: expiry OR revocation ⇒ the invalid-credential flag; a miss ⇒ None.
fn resolve_cert(
    store: &VerifierStore,
    fingerprint: &str,
    now_ms: u64,
) -> Option<ResolvedPrincipal> {
    store
        .load_cert_principal_by_fingerprint(fingerprint)
        .expect("query")
        .map(|rec| ResolvedPrincipal {
            role: ApiRole::parse_role(&rec.role),
            revoked: rec.revoked_at_ms.is_some() || rec.is_expired(now_ms),
            principal_id: rec.principal_id,
        })
}

/// The full mTLS decision at `now_ms`: no bearer token (a client cert never
/// satisfies the root), principal resolved from the fingerprint.
fn decide(store: &VerifierStore, fingerprint: &str, now_ms: u64) -> AuthzOutcome {
    let principal = resolve_cert(store, fingerprint, now_ms);
    authorize_request(
        SCOPE_INTEGRATION_EVALUATE,
        Some(ADMIN_ROOT),
        None, // mTLS path: no bearer presented
        principal.as_ref(),
    )
    .outcome
}

#[test]
fn valid_before_expiry_then_rejected_at_and_after_it() {
    let mut s = store();
    // integrator cert, notAfter = 5_000.
    s.register_cert_principal("svc", FP, "integrator", Some(5_000), 1_000)
        .unwrap();
    assert_eq!(
        decide(&s, FP, 4_999),
        AuthzOutcome::Allow,
        "before notAfter, a valid cert authorizes by role"
    );
    assert_eq!(
        decide(&s, FP, 5_000),
        AuthzOutcome::Unauthenticated,
        "at notAfter (inclusive) the cert is expired → 401 fail-closed"
    );
    assert_eq!(
        decide(&s, FP, 9_999),
        AuthzOutcome::Unauthenticated,
        "past notAfter the cert stays rejected"
    );
}

#[test]
fn renewal_restores_authorization_without_a_restart() {
    let mut s = store();
    s.register_cert_principal("svc", FP, "integrator", Some(5_000), 1_000)
        .unwrap();
    // Lapsed at now = 6_000.
    assert_eq!(decide(&s, FP, 6_000), AuthzOutcome::Unauthenticated);

    // Renew: re-pin the SAME principal with the renewed leaf's fingerprint + a later
    // notAfter. This is a plain store write — no process restart — and the very next
    // resolution honors it.
    const FP_RENEWED: &str = "ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00";
    s.register_cert_principal("svc", FP_RENEWED, "integrator", Some(50_000), 6_000)
        .unwrap();

    assert_eq!(
        decide(&s, FP, 6_000),
        AuthzOutcome::Unauthenticated,
        "the OLD (lapsed) leaf no longer resolves after renewal"
    );
    assert_eq!(
        decide(&s, FP_RENEWED, 6_000),
        AuthzOutcome::Allow,
        "the renewed cert authorizes immediately, no restart"
    );
    assert_eq!(decide(&s, FP_RENEWED, 49_999), AuthzOutcome::Allow);
    assert_eq!(
        decide(&s, FP_RENEWED, 50_000),
        AuthzOutcome::Unauthenticated,
        "the renewed cert in turn lapses at its own notAfter"
    );
}

#[test]
fn revocation_is_honored_on_the_next_resolution() {
    let mut s = store();
    // Far-future expiry so ONLY revocation can end it.
    s.register_cert_principal("svc", FP, "integrator", Some(1_000_000), 1_000)
        .unwrap();
    assert_eq!(decide(&s, FP, 2_000), AuthzOutcome::Allow);
    assert!(s.revoke_cert_principal("svc", 3_000).unwrap());
    assert_eq!(
        decide(&s, FP, 4_000),
        AuthzOutcome::Unauthenticated,
        "a revoked cert is rejected on the next resolution (a fresh handshake), unexpired"
    );
}

#[test]
fn an_untracked_expiry_cert_never_ages_out() {
    let mut s = store();
    // No notAfter → the pin authorizes until explicitly revoked (back-compat).
    s.register_cert_principal("svc", FP, "integrator", None, 1_000)
        .unwrap();
    assert_eq!(decide(&s, FP, u64::MAX), AuthzOutcome::Allow);
}
