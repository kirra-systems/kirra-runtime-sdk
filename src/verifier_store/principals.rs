// src/verifier_store/principals.rs
// api_principals domain (WS-1 · G7) — per-principal scoped bearer tokens.
//
// Only the SHA-256 hex of a token is ever stored; resolution is by hash, never
// plaintext. Mirrors the `operators` registry shape.

use super::*;

impl VerifierStore {
    /// Register (or rotate) an API principal. Re-registration overwrites the token
    /// hash + role and CLEARS any prior revocation (a fresh token for a principal
    /// is an active principal). `token_sha256` is the SHA-256 hex of the bearer
    /// token — the plaintext never reaches the store.
    pub fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO api_principals
                 (principal_id, token_sha256, role, created_at_ms, revoked_at_ms)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(principal_id) DO UPDATE SET
                 token_sha256  = excluded.token_sha256,
                 role          = excluded.role,
                 created_at_ms = excluded.created_at_ms,
                 revoked_at_ms = NULL",
            params![principal_id, token_sha256, role, now_ms as i64],
        )?;
        Ok(())
    }

    /// Revoke an API principal (sets `revoked_at_ms`). Returns `true` if an ACTIVE
    /// principal was revoked, `false` if absent or already revoked.
    pub fn revoke_api_principal(&mut self, principal_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE api_principals SET revoked_at_ms = ?2
             WHERE principal_id = ?1 AND revoked_at_ms IS NULL",
            params![principal_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Resolve an API principal by the SHA-256 hex of its presented token. Returns
    /// the record (active OR revoked — the caller fail-closes on revoked), or
    /// `None` if no principal holds that token hash. Lookup is by hash only.
    pub fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT principal_id, role, created_at_ms, revoked_at_ms
                 FROM api_principals WHERE token_sha256 = ?1",
                params![token_sha256],
                Self::map_api_principal_row,
            )
            .optional()
    }

    /// Read-only listing of every registered API principal. Never returns the
    /// token hash — the handler exposes only id / role / status.
    pub fn load_api_principals(&self) -> Result<Vec<ApiPrincipalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms
             FROM api_principals ORDER BY principal_id",
        )?;
        let rows = stmt.query_map([], Self::map_api_principal_row)?;
        rows.collect()
    }

    fn map_api_principal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiPrincipalRecord> {
        Ok(ApiPrincipalRecord {
            principal_id: row.get(0)?,
            role: row.get(1)?,
            created_at_ms: row.get::<_, i64>(2)? as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        })
    }
}

// ---------------------------------------------------------------------------
// ADR-0035 Stage 2 (trait-seam inversion) — the API-principal storage trait
//
// The THIRD `VerifierStorage`-family seam lifted off `VerifierStore` (after
// `EpochFence` and `NodeStore`), extending the same seam program toward the
// clean persistence/authority split. Same discipline as `NodeStore`: the trait
// shares the inherent method NAMES, inherent methods win resolution (so every
// existing `store.register_api_principal(...)` / `load_api_principal_by_token_hash(...)`
// caller is untouched and the SQLite impl delegates via `self.method()` WITHOUT
// recursion), and a second in-memory backend + a shared conformance suite prove
// the contract is genuinely backend-portable.
//
// The two writers are `&mut self` (mirroring the inherent signatures verbatim —
// this is a pure ADDITIVE seam, no inherent change), so the conformance driver
// takes `&mut S` (the one honest shape difference from `NodeStore`'s `&self`
// `save_node`). The registry also enforces one domain failure mode the portable
// contract must preserve on EVERY backend: `token_sha256` is `UNIQUE` (one token
// hash pins to at most one principal), so registering a hash already held by a
// DIFFERENT principal must ERROR — hence the in-memory backend's `Error` is a
// real enum ([`InMemPrincipalError`]), not `Infallible` (matching `CertPrincipalStore`).
// ---------------------------------------------------------------------------

/// The API-principal registry storage contract — register/rotate a per-principal
/// scoped bearer token (stored only as its SHA-256), revoke it, resolve a record
/// by token hash, and list all principals. Backend-agnostic; only ever holds the
/// token HASH, never plaintext.
pub trait PrincipalStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`InMemPrincipalError`]).
    type Error;

    /// Register or rotate a principal by `principal_id`: overwrite the token hash +
    /// role and CLEAR any prior revocation (a fresh token reactivates a principal).
    /// Errors if `token_sha256` is already held by a DIFFERENT principal (the
    /// `UNIQUE(token_sha256)` constraint — one token authorizes one principal).
    fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// Revoke a principal. Returns `true` iff an ACTIVE principal transitioned to
    /// revoked; `false` if absent or already revoked.
    fn revoke_api_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> std::result::Result<bool, Self::Error>;

    /// Resolve the record whose CURRENT token hash equals `token_sha256` (active OR
    /// revoked — the caller fail-closes on revoked), or `None`. Lookup by hash only.
    fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> std::result::Result<Option<ApiPrincipalRecord>, Self::Error>;

    /// List every registered principal, ordered by `principal_id`. Never the hash.
    fn load_api_principals(&self) -> std::result::Result<Vec<ApiPrincipalRecord>, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore` methods
/// over the `api_principals` table. `self.method()` resolves to the INHERENT method
/// (inherent wins over the trait), so this is delegation, not recursion.
impl PrincipalStore for VerifierStore {
    type Error = rusqlite::Error;

    fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.register_api_principal(principal_id, token_sha256, role, now_ms)
    }
    fn revoke_api_principal(&mut self, principal_id: &str, now_ms: u64) -> Result<bool> {
        self.revoke_api_principal(principal_id, now_ms)
    }
    fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>> {
        self.load_api_principal_by_token_hash(token_sha256)
    }
    fn load_api_principals(&self) -> Result<Vec<ApiPrincipalRecord>> {
        self.load_api_principals()
    }
}

/// The failure mode of the in-memory [`PrincipalStore`] backend — the one the
/// portable contract preserves across every backend (the SQLite backend surfaces
/// the same condition as a `rusqlite::Error` from the `UNIQUE(token_sha256)`
/// constraint).
#[derive(Debug, PartialEq, Eq)]
pub enum InMemPrincipalError {
    /// `token_sha256` is already held by a DIFFERENT principal (the `UNIQUE`
    /// column) — one token hash authorizes at most one principal.
    TokenHashConflict,
}

/// The in-memory [`PrincipalStore`] backend — a portability-proof reference
/// modelling the `api_principals` table as a map keyed by `principal_id`, each
/// row carrying the CURRENT token hash + role + timestamps. Realizes the SAME
/// register/rotate/revoke/resolve semantics — INCLUDING the unique-token-hash
/// refusal — WITHOUT a database. Single-process.
#[derive(Debug, Default)]
pub struct InMemoryPrincipalStore {
    rows: std::collections::HashMap<String, InMemoryPrincipalRow>,
}

#[derive(Debug, Clone)]
struct InMemoryPrincipalRow {
    token_sha256: String,
    role: String,
    created_at_ms: u64,
    revoked_at_ms: Option<u64>,
}

impl InMemoryPrincipalRow {
    fn to_record(&self, principal_id: &str) -> ApiPrincipalRecord {
        ApiPrincipalRecord {
            principal_id: principal_id.to_string(),
            role: self.role.clone(),
            created_at_ms: self.created_at_ms,
            revoked_at_ms: self.revoked_at_ms,
        }
    }
}

impl PrincipalStore for InMemoryPrincipalStore {
    type Error = InMemPrincipalError;

    fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> std::result::Result<(), InMemPrincipalError> {
        // UNIQUE(token_sha256): the hash may already be held only by the SAME
        // principal (an idempotent re-register); a DIFFERENT id is a conflict, and
        // is rejected BEFORE any mutation so a refused registration persists nothing.
        if let Some((holder, _)) = self
            .rows
            .iter()
            .find(|(_, r)| r.token_sha256 == token_sha256)
        {
            if holder != principal_id {
                return Err(InMemPrincipalError::TokenHashConflict);
            }
        }
        // Upsert by principal_id; rotation overwrites the hash/role and CLEARS
        // revocation — matching the SQLite `ON CONFLICT … SET … revoked_at_ms = NULL`.
        self.rows.insert(
            principal_id.to_string(),
            InMemoryPrincipalRow {
                token_sha256: token_sha256.to_string(),
                role: role.to_string(),
                created_at_ms: now_ms,
                revoked_at_ms: None,
            },
        );
        Ok(())
    }

    fn revoke_api_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> std::result::Result<bool, InMemPrincipalError> {
        match self.rows.get_mut(principal_id) {
            Some(row) if row.revoked_at_ms.is_none() => {
                row.revoked_at_ms = Some(now_ms);
                Ok(true)
            }
            // Absent or already revoked → no transition.
            _ => Ok(false),
        }
    }

    fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> std::result::Result<Option<ApiPrincipalRecord>, InMemPrincipalError> {
        Ok(self
            .rows
            .iter()
            .find(|(_, row)| row.token_sha256 == token_sha256)
            .map(|(id, row)| row.to_record(id)))
    }

    fn load_api_principals(
        &self,
    ) -> std::result::Result<Vec<ApiPrincipalRecord>, InMemPrincipalError> {
        let mut out: Vec<ApiPrincipalRecord> = self
            .rows
            .iter()
            .map(|(id, row)| row.to_record(id))
            .collect();
        out.sort_by(|a, b| a.principal_id.cmp(&b.principal_id));
        Ok(out)
    }
}

/// The API-principal registry contract, driven through the [`PrincipalStore`] trait
/// so it runs IDENTICALLY against every backend: empty read, register→resolve-by-hash
/// roundtrip (id + role preserved, active), rotation (the rotated-out hash stops
/// resolving; role + activation update), revoke-is-idempotent + reports the
/// transition + resolves-while-revoked, and the ordered listing.
///
/// `pub` (not `#[cfg(test)]`) by design: the shared backend-conformance suite, run
/// below against the SQLite and in-memory backends (and available to an external
/// backend crate, exactly as `assert_node_store_contract` is). Panics on any
/// violation (assert-based) — call it from a test.
///
/// PRECONDITION: `store` must start empty.
pub fn assert_principal_store_contract<S: PrincipalStore>(store: &mut S)
where
    S::Error: core::fmt::Debug,
{
    // Empty registry.
    assert!(store
        .load_api_principal_by_token_hash("nope")
        .unwrap()
        .is_none());
    assert!(store.load_api_principals().unwrap().is_empty());

    // Register + resolve by hash (id + role preserved, active).
    store
        .register_api_principal("svc-a", "hash-a", "integrator", 1_000)
        .unwrap();
    let rec = store
        .load_api_principal_by_token_hash("hash-a")
        .unwrap()
        .expect("svc-a present");
    assert_eq!(rec.principal_id, "svc-a");
    assert_eq!(rec.role, "integrator");
    assert!(rec.is_active());

    // Revoke transitions once, then is a no-op; the OLD hash still resolves the
    // now-revoked record.
    assert!(
        store.revoke_api_principal("svc-a", 2_000).unwrap(),
        "first revoke transitions"
    );
    assert!(
        !store.revoke_api_principal("svc-a", 3_000).unwrap(),
        "second revoke is a no-op"
    );
    assert!(
        !store.revoke_api_principal("absent", 3_000).unwrap(),
        "absent principal → false"
    );
    assert!(store
        .load_api_principal_by_token_hash("hash-a")
        .unwrap()
        .expect("still resolvable while revoked")
        .revoked_at_ms
        .is_some());

    // Rotation overwrites the hash + role and reactivates; the rotated-out hash
    // no longer resolves.
    store
        .register_api_principal("svc-a", "hash-a2", "auditor", 4_000)
        .unwrap();
    assert!(
        store
            .load_api_principal_by_token_hash("hash-a")
            .unwrap()
            .is_none(),
        "the rotated-out hash no longer resolves"
    );
    let rotated = store
        .load_api_principal_by_token_hash("hash-a2")
        .unwrap()
        .expect("rotated token resolves");
    assert_eq!(rotated.role, "auditor");
    assert!(rotated.is_active());

    // A second principal; the listing is ordered by id and hides no secret shape.
    store
        .register_api_principal("svc-b", "hash-b", "operator", 5_000)
        .unwrap();

    // UNIQUE(token_sha256): the same hash under a DIFFERENT principal errors and
    // persists nothing; the SAME principal re-registering its own hash is fine.
    assert!(
        store
            .register_api_principal("svc-c", "hash-b", "operator", 6_000)
            .is_err(),
        "a second principal on the same token hash must conflict"
    );
    assert!(
        store
            .load_api_principal_by_token_hash("hash-b")
            .unwrap()
            .expect("still the original holder")
            .principal_id
            == "svc-b",
        "the conflicting registration persisted nothing"
    );
    assert!(
        store
            .load_api_principal_by_token_hash("hash-a2")
            .unwrap()
            .is_some(),
        "svc-c must not have been created"
    );
    assert!(
        store
            .register_api_principal("svc-b", "hash-b", "auditor", 7_000)
            .is_ok(),
        "same principal re-registering its own hash is fine"
    );

    let ids: Vec<String> = store
        .load_api_principals()
        .unwrap()
        .into_iter()
        .map(|p| p.principal_id)
        .collect();
    assert_eq!(ids, ["svc-a", "svc-b"], "listing ordered by principal_id");
}

#[cfg(test)]
mod tests {
    use crate::verifier_store::VerifierStore;

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    #[test]
    fn register_then_resolve_by_hash() {
        let mut s = store();
        s.register_api_principal("svc-a", "hash-a", "integrator", 1_000)
            .unwrap();
        let rec = s
            .load_api_principal_by_token_hash("hash-a")
            .unwrap()
            .expect("present");
        assert_eq!(rec.principal_id, "svc-a");
        assert_eq!(rec.role, "integrator");
        assert!(rec.is_active());
        assert!(s
            .load_api_principal_by_token_hash("nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn rotation_overwrites_hash_and_clears_revocation() {
        let mut s = store();
        s.register_api_principal("svc-a", "hash-old", "integrator", 1_000)
            .unwrap();
        assert!(s.revoke_api_principal("svc-a", 2_000).unwrap());
        // The old hash still resolves the (now revoked) record.
        assert!(s
            .load_api_principal_by_token_hash("hash-old")
            .unwrap()
            .unwrap()
            .revoked_at_ms
            .is_some());
        // Re-register rotates the token and reactivates.
        s.register_api_principal("svc-a", "hash-new", "auditor", 3_000)
            .unwrap();
        assert!(
            s.load_api_principal_by_token_hash("hash-old")
                .unwrap()
                .is_none(),
            "the rotated-out hash no longer resolves"
        );
        let rec = s
            .load_api_principal_by_token_hash("hash-new")
            .unwrap()
            .unwrap();
        assert_eq!(rec.role, "auditor");
        assert!(rec.is_active());
    }

    #[test]
    fn revoke_is_idempotent_and_reports_transition() {
        let mut s = store();
        s.register_api_principal("svc-a", "h", "operator", 1_000)
            .unwrap();
        assert!(
            s.revoke_api_principal("svc-a", 2_000).unwrap(),
            "first revoke transitions"
        );
        assert!(
            !s.revoke_api_principal("svc-a", 3_000).unwrap(),
            "second revoke is a no-op"
        );
        assert!(
            !s.revoke_api_principal("absent", 3_000).unwrap(),
            "absent principal → false"
        );
    }

    #[test]
    fn list_orders_by_id_and_hides_no_secret() {
        let mut s = store();
        s.register_api_principal("svc-b", "hb", "auditor", 1_000)
            .unwrap();
        s.register_api_principal("svc-a", "ha", "integrator", 1_000)
            .unwrap();
        let all = s.load_api_principals().unwrap();
        assert_eq!(
            all.iter()
                .map(|p| p.principal_id.as_str())
                .collect::<Vec<_>>(),
            ["svc-a", "svc-b"]
        );
    }
}

#[cfg(test)]
mod principal_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_principal_store_contract() {
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_principal_store_contract(&mut store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_principal_store_contract() {
        assert_principal_store_contract(&mut InMemoryPrincipalStore::default());
    }
}
