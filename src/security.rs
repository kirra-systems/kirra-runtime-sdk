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

// The env-configured per-principal registry that lived here (#802/#803:
// PrincipalRegistry / Role / AdminPrincipal / authorize_admin / admin_rbac_allows)
// was RETIRED by the WS-1 unification in favour of the DB-backed, scope-based
// authz engine in `crate::authz` (tokens stored as SHA-256 hashes, minted /
// revoked via the admin-scoped /system/principals endpoints). One token system,
// one RBAC model. `admin_token_ok` above remains the root-token predicate the
// authz engine builds on (INVARIANTS #1/#2/#6 verbatim).

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
