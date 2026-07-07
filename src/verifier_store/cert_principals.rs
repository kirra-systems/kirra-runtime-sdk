// src/verifier_store/cert_principals.rs
// cert_principals domain (WS-1 · G7 · Track 1.2) — mTLS client-certificate principals.
//
// A client cert (already CA-verified by rustls) is pinned to a principal by the
// SHA-256 hex of its leaf DER. Resolution is by fingerprint. Mirrors the
// `api_principals` (token) module — a cert is just another least-privilege
// sub-credential on top of the KIRRA_ADMIN_TOKEN root.

use super::*;

impl VerifierStore {
    /// Register (or rotate/renew) a cert principal. Re-registration overwrites the
    /// fingerprint + role + expiry and CLEARS any prior revocation — this IS the
    /// renewal seam (WP-15): renewing a cert is re-pinning the new leaf's
    /// fingerprint and its new `not_after_ms`, which takes effect on the next
    /// resolution with no restart. `cert_sha256` is the SHA-256 hex of the client
    /// cert's leaf DER; `not_after_ms` is its X.509 notAfter (`None` = untracked).
    pub fn register_cert_principal(
        &mut self,
        principal_id: &str,
        cert_sha256: &str,
        role: &str,
        not_after_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<()> {
        // SQLite INTEGER is signed 64-bit. Convert CHECKED, not `as i64`: a
        // `not_after_ms > i64::MAX` would silently wrap to a NEGATIVE value and read
        // back as a huge u64 — effectively disabling expiry (a fail-OPEN failure).
        // Refuse it (Copilot #857). Such a value is ~292M years past epoch — never a
        // real notAfter, so this only ever catches a bug/tamper, never a valid cert.
        let not_after_i64 = match not_after_ms {
            Some(v) => Some(i64::try_from(v).map_err(|_| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not_after_ms exceeds i64::MAX",
                )))
            })?),
            None => None,
        };
        self.conn.execute(
            "INSERT INTO cert_principals
                 (principal_id, cert_sha256, role, created_at_ms, revoked_at_ms, not_after_ms)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT(principal_id) DO UPDATE SET
                 cert_sha256   = excluded.cert_sha256,
                 role          = excluded.role,
                 created_at_ms = excluded.created_at_ms,
                 revoked_at_ms = NULL,
                 not_after_ms  = excluded.not_after_ms",
            params![principal_id, cert_sha256, role, now_ms as i64, not_after_i64],
        )?;
        Ok(())
    }

    /// Revoke a cert principal. Returns `true` if an ACTIVE principal was revoked,
    /// `false` if absent or already revoked.
    pub fn revoke_cert_principal(&mut self, principal_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE cert_principals SET revoked_at_ms = ?2
             WHERE principal_id = ?1 AND revoked_at_ms IS NULL",
            params![principal_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Resolve a cert principal by the SHA-256 hex of the presented leaf cert.
    /// Returns the record (active OR revoked OR expired — the caller fail-closes on
    /// both via [`CertPrincipalRecord::is_valid_at`]), or `None` if no principal
    /// holds that fingerprint. Lookup is by fingerprint only.
    pub fn load_cert_principal_by_fingerprint(
        &self,
        cert_sha256: &str,
    ) -> Result<Option<CertPrincipalRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms
                 FROM cert_principals WHERE cert_sha256 = ?1",
                params![cert_sha256],
                Self::map_cert_principal_row,
            )
            .optional()
    }

    /// Read-only listing of every registered cert principal. Never returns the
    /// fingerprint — the handler exposes only id / role / status / expiry.
    pub fn load_cert_principals(&self) -> Result<Vec<CertPrincipalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms
             FROM cert_principals ORDER BY principal_id",
        )?;
        let rows = stmt.query_map([], Self::map_cert_principal_row)?;
        rows.collect()
    }

    /// WP-15 (MGA G-19) — census the cert-principal registry by lifecycle state at
    /// `now_ms`, for the `/metrics` expiry gauges and the periodic warning sweep.
    /// `warn_window_ms` is the "expiring soon" horizon (an active cert whose expiry
    /// falls within it counts toward `expiring_soon`). Each principal is classified
    /// into exactly one of {revoked, expired, active}, revocation first.
    pub fn cert_expiry_summary(
        &self,
        now_ms: u64,
        warn_window_ms: u64,
    ) -> Result<CertExpirySummary> {
        let mut s = CertExpirySummary::default();
        for rec in self.load_cert_principals()? {
            s.total += 1;
            if !rec.is_active() {
                s.revoked += 1;
            } else if rec.is_expired(now_ms) {
                s.expired += 1;
            } else {
                // Active (not revoked, not expired).
                s.active += 1;
                match rec.not_after_ms {
                    None => s.no_expiry += 1,
                    Some(exp) if exp.saturating_sub(now_ms) <= warn_window_ms => {
                        s.expiring_soon += 1
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(s)
    }

    fn map_cert_principal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CertPrincipalRecord> {
        Ok(CertPrincipalRecord {
            principal_id: row.get(0)?,
            role: row.get(1)?,
            created_at_ms: row.get::<_, i64>(2)? as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            // FAIL-CLOSED read (Copilot #857): a NEGATIVE stored `not_after_ms` (only
            // reachable via corruption/tamper — the write path refuses > i64::MAX)
            // maps to `Some(0)` = "expired at epoch", so `is_expired(now)` is true for
            // any `now`. A tampered expiry can therefore only make a cert MORE
            // restricted (always-expired), never reinterpret to a huge never-expiring
            // value. `u64::try_from` fails only for negatives → clamp to 0.
            not_after_ms: row
                .get::<_, Option<i64>>(4)?
                .map(|v| u64::try_from(v).unwrap_or(0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::verifier_store::VerifierStore;

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    #[test]
    fn register_then_resolve_by_fingerprint() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp-a", "integrator", None, 1_000).unwrap();
        let rec = s.load_cert_principal_by_fingerprint("fp-a").unwrap().expect("present");
        assert_eq!(rec.principal_id, "svc-a");
        assert_eq!(rec.role, "integrator");
        assert!(rec.is_active());
        assert!(s.load_cert_principal_by_fingerprint("nope").unwrap().is_none());
    }

    #[test]
    fn rotation_overwrites_fingerprint_and_clears_revocation() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp-old", "integrator", None, 1_000).unwrap();
        assert!(s.revoke_cert_principal("svc-a", 2_000).unwrap());
        assert!(s
            .load_cert_principal_by_fingerprint("fp-old")
            .unwrap()
            .unwrap()
            .revoked_at_ms
            .is_some());
        // Re-register rotates the pinned cert and reactivates.
        s.register_cert_principal("svc-a", "fp-new", "auditor", None, 3_000).unwrap();
        assert!(
            s.load_cert_principal_by_fingerprint("fp-old").unwrap().is_none(),
            "the rotated-out fingerprint no longer resolves"
        );
        let rec = s.load_cert_principal_by_fingerprint("fp-new").unwrap().unwrap();
        assert_eq!(rec.role, "auditor");
        assert!(rec.is_active());
    }

    #[test]
    fn revoke_is_idempotent_and_reports_transition() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp", "operator", None, 1_000).unwrap();
        assert!(s.revoke_cert_principal("svc-a", 2_000).unwrap(), "first revoke transitions");
        assert!(!s.revoke_cert_principal("svc-a", 3_000).unwrap(), "second revoke is a no-op");
        assert!(!s.revoke_cert_principal("absent", 3_000).unwrap(), "absent principal → false");
    }

    #[test]
    fn same_fingerprint_on_a_new_principal_is_a_unique_conflict() {
        // The UNIQUE(cert_sha256) column means one cert pins to at most one
        // principal — pinning the same fingerprint under a DIFFERENT id errors
        // (the handler maps this to 409). `ON CONFLICT(principal_id)` only rotates
        // the SAME id, so it does not absorb this case.
        let mut s = store();
        s.register_cert_principal("svc-a", "shared-fp", "operator", None, 1_000).unwrap();
        let err = s.register_cert_principal("svc-b", "shared-fp", "operator", None, 1_000);
        assert!(err.is_err(), "a second principal on the same fingerprint must conflict");
        // Re-pinning the SAME principal with the same fp is fine (idempotent rotate).
        assert!(s.register_cert_principal("svc-a", "shared-fp", "auditor", None, 2_000).is_ok());
    }

    #[test]
    fn distinct_from_token_principals() {
        // A cert principal and a token principal are separate credentials, even with
        // the same principal_id string — different tables, resolved by different keys.
        let mut s = store();
        s.register_api_principal("svc-a", "tokhash", "admin", 1_000).unwrap();
        s.register_cert_principal("svc-a", "certfp", "auditor", None, 1_000).unwrap();
        assert_eq!(s.load_api_principal_by_token_hash("tokhash").unwrap().unwrap().role, "admin");
        assert_eq!(s.load_cert_principal_by_fingerprint("certfp").unwrap().unwrap().role, "auditor");
        // Cross-lookups miss.
        assert!(s.load_cert_principal_by_fingerprint("tokhash").unwrap().is_none());
        assert!(s.load_api_principal_by_token_hash("certfp").unwrap().is_none());
    }

    // --- WP-15 (MGA G-19) cert lifecycle: expiry ----------------------------

    #[test]
    fn expiry_is_persisted_and_gates_validity_at_a_time() {
        let mut s = store();
        // notAfter = 5_000. Valid before, expired at/after (inclusive boundary).
        s.register_cert_principal("svc-a", "fp", "integrator", Some(5_000), 1_000).unwrap();
        let rec = s.load_cert_principal_by_fingerprint("fp").unwrap().unwrap();
        assert_eq!(rec.not_after_ms, Some(5_000));
        assert!(rec.is_active(), "expiry is independent of the revocation flag");
        assert!(rec.is_valid_at(4_999), "before notAfter → valid");
        assert!(!rec.is_valid_at(5_000), "notAfter is an inclusive bound → expired at the instant");
        assert!(!rec.is_valid_at(6_000), "past notAfter → expired");
        assert!(rec.is_expired(5_000) && !rec.is_expired(4_999));
    }

    #[test]
    fn untracked_expiry_never_ages_out() {
        let mut s = store();
        s.register_cert_principal("svc-a", "fp", "integrator", None, 1_000).unwrap();
        let rec = s.load_cert_principal_by_fingerprint("fp").unwrap().unwrap();
        assert_eq!(rec.not_after_ms, None);
        assert!(!rec.is_expired(u64::MAX), "no tracked expiry → never expired");
        assert!(rec.is_valid_at(u64::MAX));
    }

    #[test]
    fn renewal_extends_the_expiry_without_a_new_principal() {
        // The renewal seam: re-registering the SAME principal with a later notAfter
        // (and the renewed leaf's fingerprint) extends validity — no restart, and a
        // resolution that was expired becomes valid again immediately.
        let mut s = store();
        s.register_cert_principal("svc-a", "fp-old", "integrator", Some(5_000), 1_000).unwrap();
        let expired = s.load_cert_principal_by_fingerprint("fp-old").unwrap().unwrap();
        assert!(!expired.is_valid_at(6_000), "lapsed before renewal");
        // Renew: new leaf fingerprint + later expiry.
        s.register_cert_principal("svc-a", "fp-new", "integrator", Some(20_000), 6_000).unwrap();
        assert!(
            s.load_cert_principal_by_fingerprint("fp-old").unwrap().is_none(),
            "the old (expired) leaf no longer resolves after renewal"
        );
        let renewed = s.load_cert_principal_by_fingerprint("fp-new").unwrap().unwrap();
        assert!(renewed.is_valid_at(6_000) && renewed.is_valid_at(19_999));
        assert_eq!(renewed.not_after_ms, Some(20_000));
    }

    #[test]
    fn not_after_ms_past_i64_max_is_refused_not_wrapped() {
        // Copilot #857: a u64 expiry > i64::MAX would wrap to a negative on `as i64`
        // and read back as a huge "never expires" value (fail-OPEN). Refuse it.
        let mut s = store();
        let err = s.register_cert_principal("svc", "fp", "integrator", Some(u64::MAX), 1_000);
        assert!(err.is_err(), "an expiry beyond i64::MAX must be refused, not truncated");
        assert!(
            s.load_cert_principal_by_fingerprint("fp").unwrap().is_none(),
            "the refused registration persisted nothing"
        );
        // The largest representable value is accepted.
        assert!(s
            .register_cert_principal("svc", "fp", "integrator", Some(i64::MAX as u64), 1_000)
            .is_ok());
    }

    #[test]
    fn negative_stored_expiry_reads_as_expired_fail_closed() {
        // Copilot #857: a NEGATIVE not_after_ms in the DB (only reachable via
        // corruption/tamper) must fail CLOSED — read as already-expired, never as a
        // huge never-expiring u64. Inject one directly (bypassing the checked write).
        let mut s = store();
        s.register_cert_principal("svc", "fp", "integrator", Some(5_000), 1_000).unwrap();
        s.conn
            .execute("UPDATE cert_principals SET not_after_ms = -1 WHERE principal_id = 'svc'", [])
            .unwrap();
        let rec = s.load_cert_principal_by_fingerprint("fp").unwrap().unwrap();
        assert_eq!(rec.not_after_ms, Some(0), "negative clamps to epoch-0, not a huge u64");
        assert!(rec.is_expired(1), "a tampered negative expiry reads as always-expired");
        assert!(!rec.is_valid_at(0), "and never authorizes");
    }

    #[test]
    fn expiry_summary_classifies_every_lifecycle_state() {
        let mut s = store();
        // active, comfortably in-window (warn window below covers it as expiring_soon)
        s.register_cert_principal("soon", "fp-soon", "integrator", Some(10_500), 1_000).unwrap();
        // active, far from expiry
        s.register_cert_principal("far", "fp-far", "integrator", Some(100_000), 1_000).unwrap();
        // active, no expiry tracked
        s.register_cert_principal("forever", "fp-forever", "integrator", None, 1_000).unwrap();
        // expired (not revoked)
        s.register_cert_principal("stale", "fp-stale", "integrator", Some(5_000), 1_000).unwrap();
        // revoked (revocation wins over its expiry)
        s.register_cert_principal("gone", "fp-gone", "integrator", Some(100_000), 1_000).unwrap();
        assert!(s.revoke_cert_principal("gone", 2_000).unwrap());

        // now = 10_000; warn window = 1_000 → "soon" (exp 10_500, Δ=500) is within, "far" is not.
        let sum = s.cert_expiry_summary(10_000, 1_000).unwrap();
        assert_eq!(sum.total, 5);
        assert_eq!(sum.revoked, 1);
        assert_eq!(sum.expired, 1, "the lapsed, non-revoked cert");
        assert_eq!(sum.active, 3, "soon + far + forever");
        assert_eq!(sum.expiring_soon, 1, "only 'soon' is inside the 1s warn window");
        assert_eq!(sum.no_expiry, 1, "'forever' has no tracked expiry");
    }
}
