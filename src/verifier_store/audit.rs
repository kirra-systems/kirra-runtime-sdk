// src/verifier_store/audit.rs
// audit domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    /// The durable trust anchor's genesis key-id, if an anchor has been written.
    pub fn audit_trust_anchor_genesis_id(&self) -> Result<Option<String>> {
        let r = self.durable_ref().query_row(
            "SELECT genesis_key_id FROM audit_trust_anchor WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match r {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// All ledger rows in `seq` order.
    pub(crate) fn audit_key_ledger_rows(&self) -> Result<Vec<LedgerRow>> {
        let mut stmt = self.durable_ref().prepare(
            "SELECT key_id, role, pubkey_b64, signature_b64, prev_key_id, created_at_ms \
             FROM audit_key_ledger ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LedgerRow {
                key_id: row.get(0)?,
                role: row.get(1)?,
                pubkey_b64: row.get(2)?,
                signature_b64: row.get(3)?,
                prev_key_id: row.get(4)?,
                created_at_ms: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// The active key-id: the highest-`seq` ledger row that is NOT a forensic
    /// `backfill` row (those record lost-private-key history, never an active
    /// signer). `None` when the ledger is empty.
    pub fn audit_key_ledger_active_id(&self) -> Result<Option<String>> {
        let r = self.durable_ref().query_row(
            "SELECT key_id FROM audit_key_ledger WHERE role != 'backfill' \
             ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match r {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Resolve an audit verifying key by its `key_id` fingerprint from the
    /// durable `audit_key_ledger` (#329 residual — audit-key rotation/history).
    ///
    /// Returns the verifying key for the FIRST **self-attested** ledger row whose
    /// `key_id` matches — i.e. a `genesis` / `rotation` / `reanchor` row whose
    /// content-addressing holds and whose self-signature verifies
    /// ([`ledger_row_is_self_attested`]). A forensic `backfill` row (empty
    /// signature, lost-private-key history) is NOT trusted as a verification key
    /// — `None`, never a key. This is what lets a ROTATED-OUT key still verify
    /// the audit-chain rows it signed, across a key rotation.
    ///
    /// Fail-closed: an unknown fingerprint, a non-self-attested row, or a
    /// malformed stored key all resolve to `None`. `Err` is reserved for an
    /// actual store (SQL) failure.
    pub fn resolve_audit_verifying_key(
        &self,
        key_id: &str,
    ) -> Result<Option<ed25519_dalek::VerifyingKey>> {
        for r in self.audit_key_ledger_rows()? {
            if r.key_id == key_id && ledger_row_is_self_attested(&r) {
                // `ledger_row_is_self_attested` already re-checked content-addressing
                // (decoded key fingerprint == key_id) and the self-signature.
                return Ok(audit_decode_vk(&r.pubkey_b64));
            }
        }
        Ok(None)
    }

    /// Resolve the durable genesis verifying key from the anchor + the ledger's
    /// genesis row. `None` when no anchor exists (pre-#165 chains).
    fn audit_genesis_vk(&self) -> Result<Option<ed25519_dalek::VerifyingKey>> {
        let Some(genesis_id) = self.audit_trust_anchor_genesis_id()? else {
            return Ok(None);
        };
        for r in self.audit_key_ledger_rows()? {
            if r.key_id == genesis_id {
                if let Some(vk) = audit_decode_vk(&r.pubkey_b64) {
                    if crate::audit_chain::verifying_key_id(&vk) == genesis_id {
                        return Ok(Some(vk));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Seed the per-row verification keyring (#76 + #165): genesis from the
    /// DURABLE anchor (falling back to `fallback_vk` only when no anchor exists,
    /// i.e. a pre-#165 chain), plus every self-attested ledger key. Returns the
    /// keyring and the genesis key-id used to attribute NULL-key_id legacy rows.
    pub(crate) fn audit_keyring_seed(
        &self,
        fallback_vk: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<(std::collections::HashMap<String, ed25519_dalek::VerifyingKey>, Option<String>)> {
        let mut keyring = std::collections::HashMap::new();

        let durable_genesis = self.audit_genesis_vk()?;
        let genesis_id = match (&durable_genesis, fallback_vk) {
            // Durable anchor wins — a mutated env key can never re-root trust.
            (Some(gvk), _) => {
                let gid = crate::audit_chain::verifying_key_id(gvk);
                keyring.insert(gid.clone(), *gvk);
                Some(gid)
            }
            // Pre-#165 fallback: the passed-in (env) key is the genesis.
            (None, Some(fvk)) => {
                let gid = crate::audit_chain::verifying_key_id(fvk);
                keyring.insert(gid.clone(), *fvk);
                Some(gid)
            }
            (None, None) => None,
        };

        // Extend with every self-attested ledger key (rotations + reanchors +
        // genesis). Forensic `backfill` rows (no self-signature) are skipped;
        // their keys remain reachable via the in-chain KEY_ROTATION replay.
        for r in self.audit_key_ledger_rows()? {
            if ledger_row_is_self_attested(&r) {
                if let Some(vk) = audit_decode_vk(&r.pubkey_b64) {
                    keyring.insert(r.key_id.clone(), vk);
                }
            }
        }
        Ok((keyring, genesis_id))
    }

    /// Collect the (key_id, pubkey_b64) of each in-chain KEY_ROTATION event in
    /// id order — used at first-boot to backfill forensic ledger rows so the
    /// ledger reflects pre-#165 in-process rotation history.
    fn collect_chain_key_rotations(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_json FROM audit_log_chain \
             WHERE event_type = 'KEY_ROTATION' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for ej in rows {
            let ej = ej?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ej) {
                if let (Some(pk), Some(kid)) =
                    (v["new_public_key_b64"].as_str(), v["new_key_id"].as_str())
                {
                    // Content-addressed sanity: only carry rows whose announced
                    // id matches the announced pubkey.
                    if let Some(vk) = audit_decode_vk(pk) {
                        if crate::audit_chain::verifying_key_id(&vk) == kid {
                            out.push((kid.to_string(), pk.to_string()));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Admit the env-loaded signing key against the durable trust map (#165),
    /// returning a [`KeyAdmission`] the caller acts on. Fail-closed variants do
    /// NOT set the in-memory signing key. See [`KeyAdmission`] for the cases.
    ///
    /// - No anchor → FIRST-BOOT BACKFILL: write anchor{genesis = env} + a
    ///   self-signed genesis ledger row, and reconcile any pre-existing in-chain
    ///   KEY_ROTATION rows into forensic `backfill` ledger rows. Adopt env —
    ///   UNLESS a migration reversion is detected (see runbook below).
    /// - Anchor + env == active → resume.
    /// - Anchor + env is a RETIRED ledger id → fail-closed (gap-1).
    /// - Anchor + env is a NEW id → fail-closed unless `adopt`, in which case a
    ///   self-signed `reanchor` ledger row is recorded and env adopted (gap-2).
    /// - `pinned_genesis` (optional) is checked against the anchor; mismatch is
    ///   fail-closed.
    ///
    /// All durable writes ride a single `synchronous=FULL` transaction.
    ///
    /// # Migration-reversion runbook (`KeyAdmission::MigrationReversionRejected`)
    ///
    /// At the upgrade moment, a pre-#165 chain may record an in-process
    /// KEY_ROTATION (e.g. A→B) whose latest result key the env key does NOT
    /// match — i.e. env has reverted to a pre-rotation key (A), or is foreign to
    /// the chain. Those pre-#165 in-process rotations were never durable (the
    /// very bug #165 closes), so anchoring genesis on the reverted env key would
    /// silently re-root audit trust away from what the chain records as active.
    /// This is the PRIMARY safeguard: fail closed. It fires ONLY when the chain
    /// has ≥1 rotation AND its latest key != env; clean upgrades (no pre-#165
    /// rotations) and correct upgrades (env updated to the latest rotation key)
    /// are unaffected. RESOLUTION, operator's choice:
    ///   1. Supply the correct active private key in `KIRRA_LOG_SIGNING_KEY`
    ///      (the key the chain's latest rotation names), then restart; OR
    ///   2. Set `KIRRA_LOG_SIGNING_KEY_ADOPT=1` to consent to anchoring on the
    ///      env key — recorded as an explicit, self-signed `reanchor` ledger row
    ///      (a logged operator decision, never a silent anchor).
    pub fn admit_signing_key(
        &mut self,
        env_key: ed25519_dalek::SigningKey,
        adopt: bool,
        pinned_genesis: Option<&str>,
        now_ms: u64,
    ) -> Result<KeyAdmission> {
        // #79 fence exemption (NARROW, deliberate): this is a BOOTSTRAP write.
        // It runs once during startup signing-key admission — BEFORE the Active
        // epoch arbitration in `main()` claims an epoch — so `held_epoch` is
        // legitimately 0 here and no fence applies. It is not reachable on the
        // Active request path (its only production caller is startup), so a
        // superseded node cannot reach it. The two REQUEST-PATH durable writes
        // (`save_federated_report_chained`, `record_key_rotation`) ARE fenced
        // via `assert_epoch_held`. Do not broaden this exemption.
        use ed25519_dalek::Signer;
        let env_vk = env_key.verifying_key();
        let k_env = crate::audit_chain::verifying_key_id(&env_vk);
        let env_pub_b64 = b64e.encode(env_vk.as_bytes());

        // Optional operator-pinned genesis check (only meaningful once an anchor
        // exists; on first boot the pin is established by the backfill below).
        if let (Some(pin), Some(genesis)) = (pinned_genesis, self.audit_trust_anchor_genesis_id()?) {
            if pin != genesis {
                return Ok(KeyAdmission::GenesisPinMismatch);
            }
        }

        match self.audit_trust_anchor_genesis_id()? {
            // ---- FIRST-BOOT BACKFILL (no durable anchor yet) -----------------
            None => {
                let rotations = self.collect_chain_key_rotations()?;
                // The key the chain asserts SHOULD be active = the result of its
                // latest KEY_ROTATION (rotations are in id order).
                let chain_latest = rotations.last().map(|(kid, _)| kid.clone());
                // #165 migration hardening: a pre-#165 chain whose latest rotation
                // is NOT the env key means env has reverted to a pre-rotation key
                // (or is foreign to the chain). Anchoring genesis on it would
                // silently re-root trust away from what the chain records as
                // active — fail closed unless the operator explicitly consents.
                // Covers both "env is an earlier key in the lineage" and "env is
                // foreign to the chain": both are the same re-rooting risk.
                let reversion = matches!(&chain_latest, Some(latest) if *latest != k_env);
                if reversion && !adopt {
                    return Ok(KeyAdmission::MigrationReversionRejected {
                        chain_latest_key_id: chain_latest.unwrap(),
                        env_key_id: k_env,
                    });
                }

                let genesis_sig = b64e.encode(
                    env_key
                        .sign(ledger_signing_payload(&k_env, None, "genesis", &env_pub_b64, now_ms as i64).as_bytes())
                        .to_bytes(),
                );
                let tx = self.durable_mut().transaction()?;
                tx.execute(
                    "INSERT INTO audit_trust_anchor (id, genesis_key_id, created_at_ms) \
                     VALUES (1, ?1, ?2)",
                    params![k_env, now_ms as i64],
                )?;
                tx.execute(
                    "INSERT INTO audit_key_ledger \
                     (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                     VALUES (?1, NULL, 'genesis', ?2, ?3, ?4)",
                    params![k_env, env_pub_b64, genesis_sig, now_ms as i64],
                )?;
                // Forensic reconcile of pre-#165 in-process rotations. These keys'
                // private halves are gone (the very bug #165 closes), so the rows
                // carry an EMPTY self-signature (role='backfill') — history for
                // audit, never an active signer. The running env key stays active.
                let mut prev = k_env.clone();
                for (kid, pk) in rotations {
                    if kid == k_env {
                        continue; // env key already represented by the genesis row
                    }
                    tx.execute(
                        "INSERT INTO audit_key_ledger \
                         (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                         VALUES (?1, ?2, 'backfill', ?3, '', ?4)",
                        params![kid, prev, pk, now_ms as i64],
                    )?;
                    prev = kid;
                }
                // CONSENTED reversion (reversion && adopt): record an explicit,
                // self-signed `reanchor` ledger row (prev = the chain's latest
                // key) so the operator's decision is a logged record, not a
                // silent anchor. This also makes K_env unambiguously the active
                // (max-seq non-backfill) key.
                if reversion {
                    let reanchor_sig = b64e.encode(
                        env_key
                            .sign(
                                ledger_signing_payload(
                                    &k_env,
                                    chain_latest.as_deref(),
                                    "reanchor",
                                    &env_pub_b64,
                                    now_ms as i64,
                                )
                                .as_bytes(),
                            )
                            .to_bytes(),
                    );
                    tx.execute(
                        "INSERT INTO audit_key_ledger \
                         (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                         VALUES (?1, ?2, 'reanchor', ?3, ?4, ?5)",
                        params![k_env, chain_latest, env_pub_b64, reanchor_sig, now_ms as i64],
                    )?;
                }
                tx.commit()?; // FULL → fsync
                self.signing_key = Some(env_key);
                Ok(if reversion {
                    KeyAdmission::AdoptedReanchor
                } else {
                    KeyAdmission::BackfilledGenesis
                })
            }
            // ---- ANCHOR EXISTS: strict admission -----------------------------
            Some(_genesis_id) => {
                let active = self.audit_key_ledger_active_id()?;
                if active.as_deref() == Some(k_env.as_str()) {
                    self.signing_key = Some(env_key);
                    return Ok(KeyAdmission::Resumed);
                }
                // Is the env key present anywhere in the ledger (retired key)?
                let in_ledger = self
                    .audit_key_ledger_rows()?
                    .iter()
                    .any(|r| r.key_id == k_env);
                if in_ledger {
                    // Gap-1: a restart reverted to a retired key. Refuse to sign.
                    return Ok(KeyAdmission::RetiredKeyRejected);
                }
                // New id. Gap-2: only adopt with an explicit operator signal.
                if !adopt {
                    return Ok(KeyAdmission::UnadoptedNewKeyRejected);
                }
                // Consented re-anchor. We cannot sign an in-chain KEY_ROTATION
                // under the old active key (its private half is not in env at
                // boot), so the adopt is recorded as a self-signed `reanchor`
                // ledger row. Prior rows keep verifying under the durable genesis
                // anchor; new rows verify under this adopted key (it enters the
                // keyring via `audit_keyring_seed`'s self-attested-ledger pass).
                let prev_active = active.clone();
                let reanchor_sig = b64e.encode(
                    env_key
                        .sign(
                            ledger_signing_payload(
                                &k_env,
                                prev_active.as_deref(),
                                "reanchor",
                                &env_pub_b64,
                                now_ms as i64,
                            )
                            .as_bytes(),
                        )
                        .to_bytes(),
                );
                let tx = self.durable_mut().transaction()?;
                tx.execute(
                    "INSERT INTO audit_key_ledger \
                     (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                     VALUES (?1, ?2, 'reanchor', ?3, ?4, ?5)",
                    params![k_env, prev_active, env_pub_b64, reanchor_sig, now_ms as i64],
                )?;
                tx.commit()?; // FULL → fsync
                self.signing_key = Some(env_key);
                Ok(KeyAdmission::AdoptedReanchor)
            }
        }
    }

    /// #395 console runtime — total rows in the tamper-evident audit chain.
    /// Read-only `COUNT(*)`; surfaces the ledger depth for the live console.
    pub fn audit_chain_len(&self) -> Result<u64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain",
            [],
            |row| row.get(0),
        )
    }

    pub(crate) fn init_audit_chain_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_log_chain (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type        TEXT NOT NULL,
                event_json        TEXT NOT NULL,
                previous_hash_hex TEXT NOT NULL,
                record_hash_hex   TEXT NOT NULL,
                created_at_ms     INTEGER NOT NULL,
                signature_b64     TEXT
            )",
            [],
        )?;
        // Ignore "duplicate column name" error — column may already exist on upgraded databases
        if let Err(e) = conn.execute("ALTER TABLE audit_log_chain ADD COLUMN signature_b64 TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        // Hash-v2 migration columns (additive, defaulted, idempotent).
        // Existing rows: hash_version=1, sequence=NULL — verified with v1 algorithm.
        // New rows: hash_version=2 + monotonic sequence — see audit_chain::compute_record_hash_v2.
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN hash_version INTEGER NOT NULL DEFAULT 1",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN sequence INTEGER",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        // Key-rotation support (#76): content-addressed id of the signing key
        // per row. NULL on pre-upgrade rows (all signed under the genesis key);
        // backfilled by ensure_key_id_backfill_migration.
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN key_id TEXT",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federated_trust_reports (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                source_controller_id TEXT NOT NULL,
                asset_id             TEXT NOT NULL,
                posture_json         TEXT NOT NULL,
                issued_at_ms         INTEGER NOT NULL,
                expires_at_ms        INTEGER NOT NULL,
                received_at_ms       INTEGER NOT NULL,
                -- v2 generation-ordered reconciliation: the source controller's
                -- posture-engine generation at issue time, when supplied. NULL = a
                -- v1 report (no generation) → falls back to timestamp ordering.
                -- Inside the signed payload, so it cannot be forged or stripped.
                source_generation    INTEGER
            )",
            [],
        )?;
        // Additive, idempotent — upgrade a pre-v2 federated_trust_reports table.
        // Mirrors the clearance_grants ADD-COLUMN convention above.
        if let Err(e) = conn.execute(
            "ALTER TABLE federated_trust_reports ADD COLUMN source_generation INTEGER",
            [],
        ) {
            if !e.to_string().contains("duplicate column name") {
                return Err(e);
            }
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS trusted_federation_controllers (
                controller_id    TEXT PRIMARY KEY,
                public_key_b64   TEXT NOT NULL,
                registered_at_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federation_report_nonces (
                nonce_hex            TEXT PRIMARY KEY,
                source_controller_id TEXT NOT NULL,
                seen_at_ms           INTEGER NOT NULL
            )",
            [],
        )?;
        // Per-(controller, asset) generation HIGH-WATER mark. The nonce table stops a
        // byte-identical replay, but two genuinely-distinct, validly-signed reports
        // from one controller — a newer (higher-generation) and an older one — can
        // still arrive out of order inside the freshness window; without a high-water
        // gate the stale one would last-write-win at storage and the read-time
        // reconciliation (`authoritative_posture`) would then have to undo it. This
        // table makes generation regression fail-closed AT INGEST: a report whose
        // generation is <= the stored mark for its (controller, asset) is rejected
        // before it persists. A forward JUMP (generation > mark + 1) is accepted but
        // leaves an in-chain FEDERATION_GENERATION_GAP audit marker recording the
        // skipped generations (reports lost to a partition).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federation_generation_highwater (
                source_controller_id TEXT NOT NULL,
                asset_id             TEXT NOT NULL,
                last_generation      INTEGER NOT NULL,
                last_seen_ms         INTEGER NOT NULL,
                PRIMARY KEY (source_controller_id, asset_id)
            )",
            [],
        )?;
        // Per-asset report lookups order by received_at_ms; the nonce retention
        // sweep deletes by seen_at_ms on every federation accept. Both scan
        // unindexed columns without these.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_federated_reports_asset
                ON federated_trust_reports(asset_id, received_at_ms);
             CREATE INDEX IF NOT EXISTS idx_federation_nonces_seen
                ON federation_report_nonces(seen_at_ms);",
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS attestation_identity_registry (
                node_id                    TEXT PRIMARY KEY,
                ak_public_fingerprint_hex  TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL,
                registration_source        TEXT NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    /// Reconstruct the audit-chain keyring (key_id → VerifyingKey) by replaying
    /// the chain's `KEY_ROTATION` events in id order, bootstrapped from the
    /// GENESIS verifying key (#76). Each rotation row is signed by the PRIOR
    /// (already-trusted) key and carries the NEW key's pubkey + key_id; a
    /// rotation is only honored if it verifies under a key already in the ring
    /// AND the announced key_id matches the announced pubkey's fingerprint.
    /// The chain is thus self-describing — no external key-registry table (which
    /// would be mutable, un-anchored trust state). Genesis is the only anchor.
    pub(crate) fn build_audit_keyring(
        &self,
        genesis_vk: &ed25519_dalek::VerifyingKey,
    ) -> Result<std::collections::HashMap<String, ed25519_dalek::VerifyingKey>> {
        // #165: seed genesis from the DURABLE anchor (self-attested ledger keys
        // included); the passed-in `genesis_vk` is only the pre-#165 fallback.
        let (mut keyring, genesis_id_opt) = self.audit_keyring_seed(Some(genesis_vk))?;
        let genesis_id =
            genesis_id_opt.unwrap_or_else(|| crate::audit_chain::verifying_key_id(genesis_vk));

        let mut stmt = self.conn.prepare(
            "SELECT event_json, previous_hash_hex, record_hash_hex, created_at_ms, \
             signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain WHERE event_type = 'KEY_ROTATION' ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let event_json: String = row.get(0)?;
            let prev: String = row.get(1)?;
            let rec: String = row.get(2)?;
            let ts: i64 = row.get(3)?;
            let sig_b64: Option<String> = row.get(4)?;
            let hash_version: i64 = row.get(5)?;
            let seq: Option<i64> = row.get(6)?;
            let key_id: Option<String> = row.get(7)?;

            // The rotation must be signed by a key already trusted (the prior
            // key). A NULL key_id means a legacy/genesis-signed row.
            let signer_id = key_id.unwrap_or_else(|| genesis_id.clone());
            let Some(signer_vk) = keyring.get(&signer_id).copied() else { continue };
            let Some(ref sig) = sig_b64 else { continue };
            let payload = audit_signing_payload(hash_version, &prev, &rec, "KEY_ROTATION", ts, seq);
            if !audit_verify_sig(&signer_vk, &payload, sig) {
                continue; // an unverifiable rotation introduces no new trust
            }
            extend_keyring_from_rotation(&mut keyring, &event_json);
        }
        Ok(keyring)
    }

    pub fn verify_audit_chain_full(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<AuditChainVerifyResult> {
        // SELECT now includes event_type + hash_version + sequence + key_id so
        // the verifier can dispatch per hash_version and select the verifying
        // key PER ROW (#76).
        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain ORDER BY id ASC",
        )?;

        // Keyring seeded from the DURABLE anchor (#165) — genesis from
        // `audit_trust_anchor` + self-attested ledger keys — falling back to the
        // passed-in env key only for pre-#165 chains. Extended in id order as
        // verified KEY_ROTATION rows are encountered. A signed row is verified
        // under the key its key_id names — old rows under their ORIGINAL key.
        let (mut keyring, genesis_id) = self.audit_keyring_seed(verifying_key)?;

        let mut chain_intact = true;
        let mut total_entries: u64 = 0;
        let mut latest_hash = "0".repeat(64);
        let mut expected_previous_hash = "0".repeat(64);
        let mut signed_entries: u64 = 0;
        let mut unsigned_entries: u64 = 0;
        let mut signature_valid = true;
        let mut first_invalid_signature_index: Option<u64> = None;
        let mut first_signed_at_ms: Option<u64> = None;
        // Last-seen v2 sequence; v2 rows must monotonically increment by 1.
        let mut prev_v2_seq: Option<i64> = None;
        // #77: the last row's sequence (chain tail), for the anchor-head check.
        let mut last_sequence: Option<i64> = None;

        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let _id: i64 = row.get(0)?;
            let event_type: String = row.get(1)?;
            let event_json: String = row.get(2)?;
            let previous_hash_hex: String = row.get(3)?;
            let record_hash_hex: String = row.get(4)?;
            let created_at_ms: i64 = row.get(5)?;
            let sig_b64: Option<String> = row.get(6)?;
            let hash_version: i64 = row.get(7)?;
            let sequence_opt: Option<i64> = row.get(8)?;
            let key_id_opt: Option<String> = row.get(9)?;

            // Chain linkage check applies to every row regardless of version.
            if previous_hash_hex != expected_previous_hash {
                chain_intact = false;
            }
            // Recompute hash per version. v1 omits event_type (relabeling
            // weakness retained for legacy rows); v2 binds event_type and
            // sequence so this same cheap check catches relabeling/reorder.
            let recalc = match hash_version {
                1 => crate::audit_chain::AuditChainLinker::compute_record_hash_v1(
                    &previous_hash_hex,
                    &event_json,
                    created_at_ms,
                ),
                2 => {
                    let seq = sequence_opt.unwrap_or(-1).max(0) as u64;
                    // v2 sequence monotonicity: each v2 row must be prev_v2 + 1.
                    if let Some(prev) = prev_v2_seq {
                        if sequence_opt != Some(prev + 1) {
                            chain_intact = false;
                        }
                    } else {
                        // First v2 row must start at sequence 0.
                        if sequence_opt != Some(0) {
                            chain_intact = false;
                        }
                    }
                    prev_v2_seq = sequence_opt;
                    crate::audit_chain::AuditChainLinker::compute_record_hash_v2(
                        &previous_hash_hex,
                        &event_type,
                        &event_json,
                        created_at_ms,
                        seq,
                    )
                }
                _ => {
                    // Unknown hash version — fail closed.
                    chain_intact = false;
                    String::new()
                }
            };
            if recalc != record_hash_hex {
                chain_intact = false;
            }
            expected_previous_hash = record_hash_hex.clone();
            latest_hash = record_hash_hex.clone();
            last_sequence = sequence_opt; // #77: track the chain tail's sequence

            // Signature verification — select the verifying key PER ROW by its
            // key_id from the keyring (#76). Old rows verify under their ORIGINAL
            // key; a verified KEY_ROTATION extends the keyring for later rows.
            match &sig_b64 {
                None => {
                    unsigned_entries += 1;
                }
                Some(s) => {
                    signed_entries += 1;
                    if first_signed_at_ms.is_none() {
                        first_signed_at_ms = Some(created_at_ms as u64);
                    }
                    if verifying_key.is_some() {
                        // NULL key_id = a pre-backfill row, signed under genesis.
                        let signer_id = key_id_opt
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let ok = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload = audit_signing_payload(
                                    hash_version, &previous_hash_hex, &record_hash_hex,
                                    &event_type, created_at_ms, sequence_opt,
                                );
                                audit_verify_sig(vk, &payload, s)
                            }
                            // Unknown key_id → FAIL-CLOSED (not a skip).
                            None => false,
                        };
                        if !ok && first_invalid_signature_index.is_none() {
                            first_invalid_signature_index = Some(total_entries);
                            signature_valid = false;
                        }
                        // Extend trust only via a row that itself verified.
                        if ok && event_type == "KEY_ROTATION" {
                            extend_keyring_from_rotation(&mut keyring, &event_json);
                        }
                    }
                }
            }

            total_entries += 1;
        }

        let signing_enabled = verifying_key.is_some();
        let public_key_b64 = verifying_key.map(|vk| {
            b64e.encode(vk.as_bytes())
        });

        // #77: anchor-HEAD high-water check — detects tail TRUNCATION/DELETION
        // (which the per-row chain walk above cannot see: deleting the last rows
        // leaves the surviving prefix internally consistent). Compare the signed
        // head to the chain tail. Fail-closed (`head_verified = false`) on: head
        // absent on a non-empty chain; tail behind the head (truncation); head
        // signature invalid / unknown key (tamper). Kept SEPARATE from
        // `chain_intact` so an unanchored legacy chain (rows present, no head)
        // does not retroactively flip the row-walk verdict.
        let (head_verified, head_status): (bool, String) = if total_entries == 0 {
            // Empty chain → no head required.
            (true, "EMPTY_CHAIN".to_string())
        } else {
            let head = self.conn.query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM audit_anchor_head WHERE id = 1",
                [],
                |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                )),
            );
            match head {
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    // A properly-opened store backfills the head at startup
                    // (`ensure_audit_anchor_head`); its absence on a non-empty
                    // chain is fail-closed (deleted head or un-migrated store).
                    (false, "HEAD_ABSENT".to_string())
                }
                Err(e) => return Err(e),
                Ok((h_seq, h_hash, h_sig, h_key_id)) => {
                    if Some(h_seq) != last_sequence || h_hash != latest_hash {
                        // Position mismatch. Tail strictly behind the head is the
                        // truncation/deletion case; anything else is a head/tail
                        // divergence (forged rows past the head, reorder, etc.).
                        let truncated = match last_sequence {
                            Some(t) => t < h_seq,
                            None => true,
                        };
                        let status = if truncated { "TRUNCATION_DETECTED" } else { "HEAD_TAIL_MISMATCH" };
                        (false, status.to_string())
                    } else if signing_enabled {
                        // Signed chain ⇒ the head must carry a valid signature
                        // under a known key (same #76 keyring as the rows).
                        match h_sig {
                            None => (false, "HEAD_UNSIGNED".to_string()),
                            Some(sig) => {
                                let signer_id = h_key_id
                                    .or_else(|| genesis_id.clone())
                                    .unwrap_or_default();
                                match keyring.get(&signer_id) {
                                    None => (false, "HEAD_KEY_UNKNOWN".to_string()),
                                    Some(vk) => {
                                        let payload = crate::audit_chain::canonical_anchor_head_payload(
                                            h_seq.max(0) as u64,
                                            &h_hash,
                                        );
                                        if audit_verify_sig(vk, &payload, &sig) {
                                            (true, "OK".to_string())
                                        } else {
                                            (false, "HEAD_SIGNATURE_INVALID".to_string())
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // Unsigned chain (no verifying key supplied): the head
                        // position matches the tail; there is no signature to check.
                        (true, "OK_UNSIGNED".to_string())
                    }
                }
            }
        };

        Ok(AuditChainVerifyResult {
            chain_intact,
            total_entries,
            latest_hash,
            signing_enabled,
            signed_entries,
            unsigned_entries,
            signature_valid,
            first_invalid_signature_index,
            first_signed_at_ms,
            public_key_b64,
            head_verified,
            head_status,
        })
    }

    pub fn load_audit_chain_page(
        &self,
        limit: u64,
        offset: u64,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<AuditExportPage> {
        let total: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain",
            [],
            |row| row.get::<_, i64>(0),
        ).map(|n| n as u64)?;

        // Reconstruct the full keyring once (the page is DESC/paginated, so we
        // can't replay rotations within a page) — then annotate each row's
        // signature status under the key its key_id names (#76).
        let genesis_id = verifying_key.map(crate::audit_chain::verifying_key_id);
        let keyring = match verifying_key {
            Some(g) => self.build_audit_keyring(g)?,
            None => std::collections::HashMap::new(),
        };

        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;

        let public_key_b64 = verifying_key.map(|vk| b64e.encode(vk.as_bytes()));

        let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
            let id: i64 = row.get(0)?;
            let event_type: String = row.get(1)?;
            let event_json: String = row.get(2)?;
            let prev_hash: String = row.get(3)?;
            let entry_hash: String = row.get(4)?;
            let timestamp_ms: i64 = row.get(5)?;
            let sig_b64: Option<String> = row.get(6)?;
            let hash_version: i64 = row.get(7)?;
            let sequence_opt: Option<i64> = row.get(8)?;
            let key_id: Option<String> = row.get(9)?;

            Ok((id, event_type, event_json, prev_hash, entry_hash, timestamp_ms,
                sig_b64, hash_version, sequence_opt, key_id))
        })?;

        let mut entries = Vec::new();
        for row_result in rows {
            let (id, event_type, event_json, prev_hash, entry_hash, timestamp_ms,
                 sig_b64, hash_version, sequence_opt, key_id) = row_result?;

            let signature_status = match &sig_b64 {
                None => "unsigned".to_string(),
                Some(s) => {
                    if verifying_key.is_some() {
                        // NULL key_id = pre-backfill row signed under genesis.
                        let signer_id = key_id
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let verified = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload = audit_signing_payload(
                                    hash_version, &prev_hash, &entry_hash,
                                    &event_type, timestamp_ms, sequence_opt,
                                );
                                audit_verify_sig(vk, &payload, s)
                            }
                            None => false, // unknown key_id → fail-closed
                        };
                        if verified { "valid".to_string() } else { "invalid".to_string() }
                    } else {
                        "invalid".to_string()
                    }
                }
            };

            entries.push(AuditExportEntry {
                id,
                timestamp_ms: timestamp_ms as u64,
                event_type,
                source: "verifier".to_string(),
                payload: event_json,
                prev_hash,
                entry_hash,
                signature_b64: sig_b64,
                signature_status,
            });
        }

        let chain_intact = self.verify_audit_chain_integrity()?;

        Ok(AuditExportPage {
            entries,
            total,
            public_key_b64,
            chain_intact,
        })
    }

    /// Rotate the audit signing key (#76). The KEY_ROTATION row is signed by the
    /// OLD key (so it verifies under the prior, trusted key) and records the NEW
    /// key's pubkey + content-addressed key_id in its payload. The in-memory
    /// signing key is then swapped to the NEW key, so subsequent rows sign under
    /// it. The whole operation runs under the store mutex (callers hold the lock)
    /// and the append+swap is one critical section — atomic w.r.t. concurrent
    /// appends, and never a cosmetic rotation.
    ///
    /// Receives the new `SigningKey` (not just the pubkey): the store cannot sign
    /// future rows under a key it does not hold the private half of, so the
    /// public-key-only flow could never actually swap signing.
    pub fn record_key_rotation(
        &mut self,
        new_signing_key: ed25519_dalek::SigningKey,
        reason: &str,
        now_ms: u64,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        use ed25519_dalek::Signer;
        let new_vk = new_signing_key.verifying_key();
        let new_public_key_b64 = b64e.encode(new_vk.as_bytes());
        let new_key_id = crate::audit_chain::verifying_key_id(&new_vk);

        // The OLD key signs the in-chain KEY_ROTATION row (so it verifies under a
        // key already trusted). Clone it out before borrowing self mutably for
        // the durable transaction.
        let old_key = self.signing_key.clone();
        let old_key_id = old_key
            .as_ref()
            .map(|k| crate::audit_chain::verifying_key_id(&k.verifying_key()));

        let payload = serde_json::json!({
            "new_public_key_b64": new_public_key_b64,
            "new_key_id": new_key_id,
            "reason": reason,
            "rotated_at_ms": now_ms,
        });

        // The NEW key self-signs its ledger row (binds key_id ↔ pubkey).
        let ledger_sig = b64e.encode(
            new_signing_key
                .sign(
                    ledger_signing_payload(
                        &new_key_id,
                        old_key_id.as_deref(),
                        "rotation",
                        &new_public_key_b64,
                        now_ms as i64,
                    )
                    .as_bytes(),
                )
                .to_bytes(),
        );

        // #165: ONE durable (synchronous=FULL) transaction commits BOTH the
        // in-chain KEY_ROTATION row (signed by the OLD key) AND the durable
        // audit_key_ledger row (self-signed by the NEW key). They are atomic and
        // fsync'd together — both-present-or-neither across a hard restart. Only
        // this rare, security-critical event rides FULL; the per-command audit
        // path stays on the NORMAL connection.
        {
            // #79: IMMEDIATE so the durable write lock is held before the epoch
            // re-check — no concurrent claim can interleave before this commit.
            let tx = self
                .durable_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // #79 HA epoch fence — FIRST statement, before any mutation. A node
            // fenced after the request-path gate cannot land a stale rotation.
            Self::assert_epoch_held(&tx, held_epoch)?;
            crate::audit_chain::AuditChainLinker::append_audit_event_tx(
                &tx,
                "KEY_ROTATION",
                &payload.to_string(),
                now_ms as i64,
                old_key.as_ref(),
            )?;
            tx.execute(
                "INSERT INTO audit_key_ledger \
                 (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                 VALUES (?1, ?2, 'rotation', ?3, ?4, ?5)",
                params![new_key_id, old_key_id, new_public_key_b64, ledger_sig, now_ms as i64],
            )?;
            tx.commit()?; // FULL → fsync; durable active-key record updated
        }

        // Swap the in-memory signing key to the NEW key AFTER the durable commit
        // (atomic under the store lock the caller holds — no append interleaves).
        // The durable ledger — not the dropped advisory engine-state row — is now
        // the authoritative record of the active key.
        self.signing_key = Some(new_signing_key);
        Ok(())
    }

    /// One-time, idempotent backfill (#76): existing rows have a NULL `key_id`
    /// (they were all signed under the genesis key, since rotation was
    /// previously cosmetic). Assign them the genesis key's id and anchor the
    /// migration with a signed `KEY_ID_BACKFILL` event. NO signatures are
    /// rewritten — only the new `key_id` column is populated. Rides the same
    /// boot-time pattern as `ensure_hash_v2_migration_anchor`.
    pub fn ensure_key_id_backfill_migration(&mut self, now_ms: u64) -> Result<()> {
        // Genesis id from the current signing key (the only key the chain has
        // ever been signed under, pre-rotation). No signing key → nothing to do.
        let genesis_id = match self.signing_key.as_ref() {
            Some(sk) => crate::audit_chain::verifying_key_id(&sk.verifying_key()),
            None => return Ok(()),
        };
        // Idempotent: already anchored?
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'",
            [],
            |r| r.get(0),
        )?;
        if existing > 0 {
            return Ok(());
        }
        let null_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        if null_count == 0 {
            // Brand-new chain (rows already carry key_id) — nothing to backfill.
            return Ok(());
        }
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        tx.execute(
            "UPDATE audit_log_chain SET key_id = ?1 WHERE key_id IS NULL",
            params![genesis_id],
        )?;
        let payload = format!(
            "{{\"genesis_key_id\":\"{genesis_id}\",\"backfilled_rows\":{null_count},\"migrated_at_ms\":{now_ms}}}"
        );
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "KEY_ID_BACKFILL",
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn verify_audit_chain_integrity(&self) -> Result<bool> {
        // Cheap hash-only integrity check. Post hash-v2 migration this
        // catches event_type relabeling and v2 sequence reorder/gaps on
        // v2 rows without needing the signing key. v1 rows retain the
        // pre-migration relabeling weakness (cannot be retroactively
        // strengthened without destructive rewrite) — that boundary is
        // anchored by the HASH_V2_MIGRATION event.
        let mut stmt = self.conn.prepare(
            "SELECT event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, hash_version, sequence \
             FROM audit_log_chain ORDER BY id ASC",
        )?;

        let mut expected_previous_hash = "0".repeat(64);
        let mut prev_v2_seq: Option<i64> = None;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let event_type: String = row.get(0)?;
            let event_json: String = row.get(1)?;
            let previous_hash_hex: String = row.get(2)?;
            let record_hash_hex: String = row.get(3)?;
            let created_at_ms: i64 = row.get(4)?;
            let hash_version: i64 = row.get(5)?;
            let sequence_opt: Option<i64> = row.get(6)?;

            if previous_hash_hex != expected_previous_hash {
                return Ok(false);
            }
            let recalc = match hash_version {
                1 => crate::audit_chain::AuditChainLinker::compute_record_hash_v1(
                    &previous_hash_hex,
                    &event_json,
                    created_at_ms,
                ),
                2 => {
                    let seq = sequence_opt.unwrap_or(-1).max(0) as u64;
                    if let Some(prev) = prev_v2_seq {
                        if sequence_opt != Some(prev + 1) {
                            return Ok(false);
                        }
                    } else if sequence_opt != Some(0) {
                        return Ok(false);
                    }
                    prev_v2_seq = sequence_opt;
                    crate::audit_chain::AuditChainLinker::compute_record_hash_v2(
                        &previous_hash_hex,
                        &event_type,
                        &event_json,
                        created_at_ms,
                        seq,
                    )
                }
                _ => return Ok(false), // unknown version → fail closed
            };
            if recalc != record_hash_hex {
                return Ok(false);
            }
            expected_previous_hash = record_hash_hex;
        }

        Ok(true)
    }

    /// Idempotent one-time anchor for the v1 → v2 hash boundary. Should
    /// be called at service startup after `VerifierStore::new`. If a
    /// `HASH_V2_MIGRATION` event already exists in the chain this is a
    /// no-op; otherwise it appends one event whose payload records the
    /// pre-migration v1 head and v1 row count, providing a partial defence
    /// against silent truncation at the boundary.
    ///
    /// Note: v1 rows retain the pre-migration relabeling weakness (cannot
    /// be retroactively strengthened without destructive re-hashing).
    /// Only v2 and future rows benefit from event_type being bound into
    /// the cheap hash-only integrity check.
    pub fn ensure_hash_v2_migration_anchor(&mut self, now_ms: u64) -> rusqlite::Result<()> {
        // Already anchored?
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'HASH_V2_MIGRATION'",
            [],
            |r| r.get(0),
        )?;
        if existing > 0 {
            return Ok(());
        }
        // Snapshot the v1 head (last row with hash_version=1) and v1 count.
        let v1_total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE hash_version = 1",
            [],
            |r| r.get(0),
        )?;
        if v1_total == 0 {
            // Nothing to anchor — a brand-new chain skips the marker.
            return Ok(());
        }
        let v1_head: String = self.conn.query_row(
            "SELECT record_hash_hex FROM audit_log_chain \
             WHERE hash_version = 1 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let payload = format!(
            "{{\"v1_head_record_hash\":\"{v1_head}\",\"v1_total_count\":{v1_total},\"migrated_at_ms\":{now_ms}}}"
        );
        let tx = Self::audit_tx(&mut self.conn)?; // #685: Immediate — non-forking audit append
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "HASH_V2_MIGRATION",
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// #77 backfill: ensure the signed anchor-HEAD exists for an already-populated
    /// chain — e.g. a chain written by a pre-#77 binary and opened after upgrade,
    /// whose rows predate head maintenance. If the chain is non-empty and no head
    /// row exists, sign the current tail's `(sequence, record_hash)` with the
    /// loaded signing key and write the head. Idempotent: a no-op once the head
    /// exists or while the chain is empty. Runs at startup AFTER the signing key
    /// is admitted (same point as `ensure_hash_v2_migration_anchor`), so the
    /// backfilled head is signed — and so a legitimately-upgraded store presents a
    /// head BEFORE it serves `/system/audit/verify` (no false `HEAD_ABSENT`).
    /// `_now_ms` is accepted only for call-site symmetry with the other `ensure_*`
    /// migrations (the head payload binds sequence+hash, not a timestamp).
    pub fn ensure_audit_anchor_head(&mut self, _now_ms: u64) -> rusqlite::Result<()> {
        let head_exists: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_anchor_head WHERE id = 1",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if head_exists {
            return Ok(());
        }
        // Current tail (highest id). Empty chain → nothing to anchor.
        let tail = self.conn.query_row(
            "SELECT record_hash_hex, sequence FROM audit_log_chain ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
        );
        let (record_hash, seq_opt) = match tail {
            Ok(t) => t,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()), // empty chain
            Err(e) => return Err(e),
        };
        // Only anchor a v2 tail (sequence present); a v1-only tail predates the
        // sequence/head model and is anchored once the hash-v2 migration appends.
        let Some(seq) = seq_opt else { return Ok(()) };
        let seq = seq.max(0) as u64;

        let (signature_b64, key_id): (Option<String>, Option<String>) =
            match self.signing_key.as_ref() {
                Some(key) => {
                    use ed25519_dalek::Signer;
                    let payload = crate::audit_chain::canonical_anchor_head_payload(seq, &record_hash);
                    let sig = b64e.encode(key.sign(payload.as_bytes()).to_bytes());
                    let kid = crate::audit_chain::verifying_key_id(&key.verifying_key());
                    (Some(sig), Some(kid))
                }
                None => (None, None),
            };
        self.conn.execute(
            "INSERT INTO audit_anchor_head (id, sequence, record_hash_hex, signature_b64, key_id)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 sequence        = excluded.sequence,
                 record_hash_hex = excluded.record_hash_hex,
                 signature_b64   = excluded.signature_b64,
                 key_id          = excluded.key_id",
            params![seq as i64, record_hash, signature_b64, key_id],
        )?;
        Ok(())
    }

    /// TEST-ONLY: drop `audit_log_chain` so the next
    /// `ensure_hash_v2_migration_anchor` (and any chained audit write) fails —
    /// used to exercise the fail-closed promotion-abort path (#78). Never
    /// compiled into production builds.
    #[cfg(test)]
    pub fn break_audit_chain_table_for_test(&self) {
        self.conn
            .execute("DROP TABLE IF EXISTS audit_log_chain", [])
            .expect("test seam: drop audit_log_chain");
    }

    /// TEST-ONLY: seed one legacy `hash_version = 1` row so a subsequent
    /// `ensure_hash_v2_migration_anchor` actually WRITES the `HASH_V2_MIGRATION`
    /// marker (on a clean chain `v1_total == 0`, so the anchor is a no-op). Lets
    /// a test prove the anchor was ensured during promotion (#78).
    #[cfg(test)]
    pub fn seed_legacy_v1_audit_row_for_test(&self) {
        self.conn
            .execute(
                "INSERT INTO audit_log_chain \
                 (event_type, event_json, previous_hash_hex, record_hash_hex, created_at_ms, hash_version) \
                 VALUES ('LEGACY_V1', '{}', '', 'deadbeef', 1, 1)",
                [],
            )
            .expect("test seam: seed legacy v1 audit row");
    }

    /// TEST-ONLY: count `audit_log_chain` rows of a given `event_type`.
    #[cfg(test)]
    pub fn count_audit_events_for_test(&self, event_type: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = ?1",
                params![event_type],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }
}
