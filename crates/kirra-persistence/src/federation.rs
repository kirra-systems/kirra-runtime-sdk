// src/verifier_store/federation.rs
// federation domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn save_federated_report_chained(
        &mut self,
        report: &FederatedTrustReport,
        source_generation: Option<u64>,
        source_epoch: Option<u64>,
        received_at_ms: u64,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        // #791 I1 structural normalization: `source_epoch` without a generation is
        // malformed (`epoch_field_well_formed`) and the gateway rejects it before
        // this call; defensively, an epoch riding in with no generation is DROPPED
        // here (mirroring `canonical_federation_payload_v2`, which canonicalizes
        // the ill-formed shape WITHOUT the epoch) — it then takes the epoch-less
        // path below, which is the fail-closed one once the peer's high-water
        // carries an epoch. It can never launder a tuple-gate bypass.
        let source_epoch = if source_generation.is_some() {
            source_epoch
        } else {
            None
        };
        // #74: route the whole federation commit — report + NONCE BURN + audit —
        // through the FULL (force-synced) connection. A burned nonce must survive
        // power-loss or anti-replay is defeated (the 5 s freshness window only
        // partially bounds replay). Federation reports are rare, so the per-commit
        // fsync is off the hot path and inconsequential to throughput.
        let signing_key = self.signing_key.clone(); // durable_mut() borrows self
                                                    // #79: IMMEDIATE so the durable write lock is held before the epoch
                                                    // re-check below — no concurrent `try_claim_epoch` can interleave
                                                    // between the fence read and this commit.
        let tx = self
            .durable_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // #79 HA epoch fence — FIRST statement, before any mutation. A node
        // fenced after the request-path gate check cannot land a stale report.
        Self::assert_epoch_held(&tx, held_epoch)?;

        // Item 20 + #791 I1 — per-(controller, asset) HIGH-WATER gate, now the
        // LEXICOGRAPHIC tuple `(epoch, generation)`. Runs inside the Immediate
        // write lock (no interleave). An epoch-less report has effective epoch 0
        // (the pre-epoch legacy rung). Outcomes:
        //   * EPOCH regress: `eff_epoch < hw_epoch` → abort (a fenced old primary
        //     still publishing under its superseded epoch);
        //   * OMISSION-DOWNGRADE (hard reject, owner decision #791): the peer's
        //     high-water carries epoch ≥ 1 but this report omits `source_epoch`
        //     (whether v1 or generation-only v2) → abort. Accepting it would let
        //     a stripped-field replay ride BELOW the tuple gate — the same
        //     downgrade-by-omission EP-13 refuses for Uptane metadata;
        //   * same epoch, generation regress/replay: `gen <= hw_gen` → abort;
        //   * same epoch, forward GAP: `gen > hw_gen + 1` → accept + in-chain
        //     FEDERATION_GENERATION_GAP marker (below, after the report row);
        //   * EPOCH ADVANCE: `eff_epoch > hw_epoch` → accept regardless of the
        //     generation (a freshly-promoted controller legitimately RESETS its
        //     generation; higher epoch orders newer BY CONSTRUCTION — no counter
        //     catch-up, no gap marker) + in-chain FEDERATION_EPOCH_ADVANCE marker.
        // All rejects drop `tx` → atomic rollback: report NOT persisted, nonce NOT
        // burned, high-water NOT advanced. Fail-closed.
        let mut gap_from: Option<u64> = None;
        let mut epoch_advance_from: Option<u64> = None;
        let eff_epoch = source_epoch.unwrap_or(0);
        let high_water: Option<(u64, u64)> = tx
            .query_row(
                "SELECT last_epoch, last_generation FROM federation_generation_highwater
                 WHERE source_controller_id = ?1 AND asset_id = ?2",
                params![report.source_controller_id, report.asset_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;

        if let Some((hw_epoch, hw_gen)) = high_water {
            if hw_epoch >= 1 && source_epoch.is_none() {
                // `found: 0` — the effective epoch an omitting report claims.
                return Err(DurableWriteError::EpochRegress {
                    found: 0,
                    high_water: hw_epoch,
                });
            }
            if let Some(gen) = source_generation {
                if eff_epoch < hw_epoch {
                    return Err(DurableWriteError::EpochRegress {
                        found: eff_epoch,
                        high_water: hw_epoch,
                    });
                }
                if eff_epoch == hw_epoch {
                    if gen <= hw_gen {
                        return Err(DurableWriteError::GenerationRegress {
                            found: gen,
                            high_water: hw_gen,
                        });
                    }
                    if gen > hw_gen + 1 {
                        gap_from = Some(hw_gen);
                    }
                } else {
                    epoch_advance_from = Some(hw_epoch);
                }
            }
            // A v1 report (no generation) against an epoch-0 high-water keeps its
            // legacy behaviour: no gate, no advance.
        }

        // Advance the tuple high-water within the same tx (UPSERT; the gates
        // above guarantee lexicographic strict advance for an existing row).
        // Only ordering-carrying (v2) reports advance it, as before.
        if let Some(gen) = source_generation {
            tx.execute(
                "INSERT INTO federation_generation_highwater
                     (source_controller_id, asset_id, last_epoch, last_generation, last_seen_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source_controller_id, asset_id)
                 DO UPDATE SET last_epoch = ?3, last_generation = ?4, last_seen_ms = ?5",
                params![
                    report.source_controller_id,
                    report.asset_id,
                    eff_epoch as i64,
                    gen as i64,
                    received_at_ms as i64,
                ],
            )?;
        }

        let posture_json =
            serde_json::to_string(&report.posture).map_err(|_| rusqlite::Error::InvalidQuery)?;

        tx.execute(
            "INSERT INTO federated_trust_reports
             (source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, received_at_ms, source_generation, source_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                report.source_controller_id, report.asset_id, posture_json,
                report.issued_at_ms as i64, report.expires_at_ms as i64, received_at_ms as i64,
                source_generation.map(|g| g as i64),
                source_epoch.map(|e| e as i64),
            ],
        )?;

        // H1 — AUTHORITATIVE single-use nonce claim. This MUST stay a plain `INSERT`
        // (never `INSERT OR IGNORE`): the `nonce_hex PRIMARY KEY` UNIQUE violation is
        // what atomically rejects a concurrent replay that raced past the request-path
        // `has_seen_federation_nonce` check. `OR IGNORE` would let the report row above
        // commit while silently no-op-ing the burn → DOUBLE-ACCEPT. The violation is
        // surfaced as the distinct `NonceReplay` (not a generic Db error) so the caller
        // returns a clean replay rejection + audit instead of an opaque 500.
        if let Err(e) = tx.execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![
                report.nonce_hex,
                report.source_controller_id,
                received_at_ms as i64
            ],
        ) {
            if is_unique_violation(&e) {
                // tx drops here → the report INSERT above is rolled back atomically.
                return Err(DurableWriteError::NonceReplay);
            }
            return Err(DurableWriteError::Db(e));
        }

        let audit = serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": posture_json,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
            "received_at_ms": received_at_ms,
        });
        ChainedAuditAppender {
            signing_key: signing_key.as_ref(),
        }
        .append_within(
            &tx,
            "FEDERATED_TRUST_REPORT_ACCEPTED",
            &audit.to_string(),
            received_at_ms as i64,
        )?;

        // Item 20 — in-chain AUDIT_GAP marker. A forward generation jump means this
        // controller's intermediate reports never reached us (a partition / drop):
        // the chain MUST record that we are missing generations `hw+1 ..= gen-1`, so a
        // later auditor sees an explicit, tamper-evident "coverage gap" instead of an
        // unexplained generation discontinuity. Same tx as the accepted report → the
        // marker is committed iff the report is.
        if let (Some(hw), Some(gen)) = (gap_from, source_generation) {
            let gap = serde_json::json!({
                "source_controller_id": report.source_controller_id,
                "asset_id": report.asset_id,
                "last_accepted_generation": hw,
                "observed_generation": gen,
                "missing_from_generation": hw + 1,
                "missing_through_generation": gen - 1,
                "skipped_generations": gen - hw - 1,
            });
            ChainedAuditAppender {
                signing_key: signing_key.as_ref(),
            }
            .append_within(
                &tx,
                "FEDERATION_GENERATION_GAP",
                &gap.to_string(),
                received_at_ms as i64,
            )?;
        }

        // #791 I1 — in-chain EPOCH-ADVANCE marker. A higher epoch means the source
        // controller failed over (a standby promoted via the durable epoch CAS)
        // and its generation counter may legitimately RESET — a later auditor
        // reading the chain must see the explicit failover signature explaining
        // the generation discontinuity, not an unexplained drop. Same tx as the
        // accepted report → the marker is committed iff the report is.
        if let (Some(prev_epoch), Some(gen)) = (epoch_advance_from, source_generation) {
            let adv = serde_json::json!({
                "source_controller_id": report.source_controller_id,
                "asset_id": report.asset_id,
                "previous_epoch": prev_epoch,
                "observed_epoch": eff_epoch,
                "observed_generation": gen,
            });
            ChainedAuditAppender {
                signing_key: signing_key.as_ref(),
            }
            .append_within(
                &tx,
                "FEDERATION_EPOCH_ADVANCE",
                &adv.to_string(),
                received_at_ms as i64,
            )?;
        }

        // Bounded retention (review M2): the nonce table is the durable anti-replay
        // set, but it only ever grew (rising disk + fsync cost). A nonce aged past
        // the retention horizon can NEVER reopen a replay slot — a report bearing it
        // carries a FIXED, signed `issued_at_ms`, so a replay fails the freshness
        // gate (`FEDERATION_REPLAY_WINDOW_MS`) regardless of whether the nonce row
        // still exists. The horizon is set FAR above the freshness window to absorb
        // clock skew; we never delete a nonce that could still be inside any
        // plausible replay window. Pruned in the same accept transaction.
        let cutoff = (received_at_ms as i64).saturating_sub(FEDERATION_NONCE_RETENTION_MS);
        tx.execute(
            "DELETE FROM federation_report_nonces WHERE seen_at_ms < ?1",
            params![cutoff],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_federation_controllers
             (controller_id, public_key_b64, registered_at_ms)
             VALUES (?1, ?2, ?3)",
            params![controller_id, public_key_b64, registered_at_ms as i64],
        )?;
        Ok(())
    }

    pub fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT public_key_b64 FROM trusted_federation_controllers
             WHERE controller_id = ?1",
        )?;
        match stmt.query_row(params![controller_id], |row| row.get::<_, String>(0)) {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM federation_report_nonces WHERE nonce_hex = ?1",
            params![nonce_hex],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Per-source monotonic sequence gate for industrial-message replay protection.
    ///
    /// Returns `Ok(true)` when `sequence` is STRICTLY greater than the last-seen
    /// high-water mark for `source_id` — the message is fresh-in-order and the
    /// high-water mark is advanced to it — or `Ok(false)` when `sequence <=` the
    /// stored mark (a replay or out-of-order regress → the caller must reject; the
    /// mark is NOT advanced). The first message from a new source establishes the
    /// baseline (any sequence accepted once).
    ///
    /// The check-and-advance is a single atomic `INSERT … ON CONFLICT … DO UPDATE …
    /// WHERE ? > last_sequence`: under the store mutex, two concurrent ingests of the
    /// same captured sequence cannot both win (the conditional UPDATE makes it a true
    /// compare-and-set, like the federation nonce burn / HA epoch CAS). Durable, so a
    /// replay cannot ride a restart.
    pub fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO industrial_message_seq (source_id, last_sequence, last_seen_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_id) DO UPDATE SET last_sequence = ?2, last_seen_ms = ?3
             WHERE ?2 > industrial_message_seq.last_sequence",
            params![source_id, sequence as i64, now_ms as i64],
        )?;
        Ok(changed > 0)
    }

    /// Atomically *burn* a nonce: claim it on first use, reject it on replay.
    ///
    /// Returns `Ok(true)` if the nonce was newly recorded (first time seen →
    /// the caller may proceed) and `Ok(false)` if it was already present (a
    /// replay → the caller must reject). This is the verify-AND-consume primitive
    /// the untrusted fleet carrier needs: a single `INSERT OR IGNORE` against the
    /// `nonce_hex PRIMARY KEY` makes the claim atomic — there is no check-then-act
    /// window for two concurrent ingests of the same captured payload to both win.
    ///
    /// The write rides the `synchronous=FULL` durable connection (same as the
    /// federation report nonce burn), falling back to the main connection for an
    /// in-memory store. The `seen_at_ms` column is diagnostic only — replay
    /// correctness depends solely on the primary-key conflict, never on the clock.
    pub fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let seen_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let changed = self.durable_ref().execute(
            "INSERT OR IGNORE INTO federation_report_nonces
                 (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![nonce_hex, "fleet-grant-lane", seen_at_ms],
        )?;
        Ok(changed > 0)
    }

    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, source_generation, source_epoch
             FROM federated_trust_reports
             WHERE asset_id = ?1
             ORDER BY received_at_ms DESC",
        )?;
        let rows = stmt.query_map(params![asset_id], |row| {
            let source: String = row.get(0)?;
            let aid: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let issued: i64 = row.get(3)?;
            let expires: i64 = row.get(4)?;
            let generation: Option<i64> = row.get(5)?;
            let epoch: Option<i64> = row.get(6)?;
            Ok(serde_json::json!({
                "source_controller_id": source,
                "asset_id": aid,
                "posture": posture_json,
                "issued_at_ms": issued as u64,
                "expires_at_ms": expires as u64,
                "source_generation": generation.map(|g| g as u64),
                "source_epoch": epoch.map(|e| e as u64),
            }))
        })?;
        rows.collect()
    }

    /// Typed loader for generation-ordered reconciliation (#329 v2 wiring). Returns
    /// the stored reports for an asset as `FederatedTrustReportV2` so the caller can
    /// run `authoritative_posture`. `nonce_hex` / `signature_b64` are NOT persisted in
    /// this table (they are consumed/verified at ingest), so they are left empty here —
    /// the reconciliation API (`reconcile_reports` / `authoritative_posture`) reads only
    /// `asset_id`, `posture`, `source_generation`, and `issued_at_ms`. A row whose
    /// `posture_json` fails to deserialize is fail-closed-skipped (never silently
    /// treated as Nominal). Ordered newest-first, matching `load_federated_reports_for_asset`.
    pub fn load_federated_report_v2s_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, source_generation, source_epoch
             FROM federated_trust_reports
             WHERE asset_id = ?1
             ORDER BY received_at_ms DESC",
        )?;
        let rows = stmt.query_map(params![asset_id], |row| {
            let source: String = row.get(0)?;
            let aid: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let issued: i64 = row.get(3)?;
            let expires: i64 = row.get(4)?;
            let generation: Option<i64> = row.get(5)?;
            let epoch: Option<i64> = row.get(6)?;
            Ok((
                source,
                aid,
                posture_json,
                issued as u64,
                expires as u64,
                generation.map(|g| g as u64),
                epoch.map(|e| e as u64),
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (source, aid, posture_json, issued, expires, generation, epoch) = row?;
            // Fail-closed: a corrupt posture is skipped, never coerced to Nominal.
            let Ok(posture) = serde_json::from_str::<kirra_core::FleetPosture>(&posture_json)
            else {
                continue;
            };
            out.push(
                kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2 {
                    source_controller_id: source,
                    asset_id: aid,
                    posture,
                    issued_at_ms: issued,
                    expires_at_ms: expires,
                    nonce_hex: String::new(),
                    signature_b64: String::new(),
                    source_generation: generation,
                    source_epoch: epoch,
                },
            );
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// WP-18 store-trait family — the `FederationStore` seam.
//
// The PORTABLE subset of the federation family: the trusted-controller key
// registry + the durable anti-replay primitives (nonce set + the per-source
// monotonic sequence gate). These are the coordination writes a non-SQLite
// backend must realize; the audit-chained `save_federated_report_chained`
// commit stays inherent + SQLite-specific (audit-appender + epoch-fence
// coupled), exactly as `NodeStore` leaves the audit-chained node ops inherent.
//
// The trait method names MATCH the inherent `VerifierStore` methods, so the
// SQLite impl's `self.method()` resolves to the INHERENT method (inherent wins
// over the trait) — delegation, not recursion.
// ---------------------------------------------------------------------------

/// The federation coordination storage contract — the trusted-controller key
/// registry plus the two durable anti-replay primitives (single-use nonce claim
/// and the per-source strictly-advancing sequence gate). Backend-agnostic.
pub trait FederationStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`std::convert::Infallible`]).
    type Error;

    /// Upsert a trusted controller's base64 Ed25519 public key by `controller_id`
    /// (INSERT-OR-REPLACE — re-registering an id overwrites its key).
    fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> std::result::Result<(), Self::Error>;

    /// Load a controller's stored base64 public key, or `None` if unregistered.
    fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> std::result::Result<Option<String>, Self::Error>;

    /// Has this nonce already been recorded (a read-only replay pre-check)?
    fn has_seen_federation_nonce(&self, nonce_hex: &str) -> std::result::Result<bool, Self::Error>;

    /// Atomically claim a nonce: `Ok(true)` on first use (proceed), `Ok(false)`
    /// on replay (reject). The single-use burn primitive.
    fn burn_federation_nonce(&self, nonce_hex: &str) -> std::result::Result<bool, Self::Error>;

    /// Per-source strictly-advancing sequence gate: `Ok(true)` iff `sequence` is
    /// strictly greater than the source's high-water (advances it); `Ok(false)` on
    /// a replay/regress (`<=`, no advance). A new source establishes the baseline.
    fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> std::result::Result<bool, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore`
/// methods. `self.method()` resolves to the INHERENT method (inherent wins over
/// the trait), so this is delegation, not recursion.
impl FederationStore for VerifierStore {
    type Error = rusqlite::Error;

    fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        self.save_trusted_federation_controller(controller_id, public_key_b64, registered_at_ms)
    }
    fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>> {
        self.load_trusted_federation_controller_key(controller_id)
    }
    fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        self.has_seen_federation_nonce(nonce_hex)
    }
    fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        self.burn_federation_nonce(nonce_hex)
    }
    fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> Result<bool> {
        self.industrial_seq_check_and_advance(source_id, sequence, now_ms)
    }
}

/// The in-memory [`FederationStore`] backend — a portability-proof reference
/// modelling the controller registry as a map, the nonce table as a set, and the
/// per-source sequence gate as a map. Realizes the SAME upsert / burn / strict-
/// advance semantics WITHOUT a database. Interior mutability (the trait methods
/// are `&self`, matching the SQLite `Connection`'s `&self` writes). Single-process.
///
/// `Error = Infallible` is honest: every method RECOVERS from a poisoned `Mutex`
/// (`lock().unwrap_or_else(PoisonError::into_inner)`) rather than unwrapping — the
/// maps carry no cross-call invariant a torn write could break, so recovered data
/// is safe to use.
#[derive(Debug, Default)]
pub struct InMemoryFederationStore {
    controllers: std::sync::Mutex<std::collections::HashMap<String, String>>,
    nonces: std::sync::Mutex<std::collections::HashSet<String>>,
    seq_highwater: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl FederationStore for InMemoryFederationStore {
    type Error = std::convert::Infallible;

    fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        _registered_at_ms: u64,
    ) -> std::result::Result<(), std::convert::Infallible> {
        self.controllers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(controller_id.to_string(), public_key_b64.to_string());
        Ok(())
    }
    fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> std::result::Result<Option<String>, std::convert::Infallible> {
        Ok(self
            .controllers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(controller_id)
            .cloned())
    }
    fn has_seen_federation_nonce(
        &self,
        nonce_hex: &str,
    ) -> std::result::Result<bool, std::convert::Infallible> {
        Ok(self
            .nonces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(nonce_hex))
    }
    fn burn_federation_nonce(
        &self,
        nonce_hex: &str,
    ) -> std::result::Result<bool, std::convert::Infallible> {
        // `HashSet::insert` returns true iff the value was NOT already present —
        // exactly the SQLite `INSERT OR IGNORE` "newly claimed" semantics.
        Ok(self
            .nonces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(nonce_hex.to_string()))
    }
    fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        _now_ms: u64,
    ) -> std::result::Result<bool, std::convert::Infallible> {
        let mut map = self.seq_highwater.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(source_id) {
            // Existing source: STRICT advance only (mirrors the SQL conditional
            // `WHERE ?2 > last_sequence` compare-and-set).
            Some(&hw) if sequence <= hw => Ok(false),
            _ => {
                map.insert(source_id.to_string(), sequence);
                Ok(true)
            }
        }
    }
}

/// The federation-coordination contract, driven through the [`FederationStore`]
/// trait so it runs IDENTICALLY against every backend: controller key upsert +
/// read-back + unknown-is-None, the nonce burn (first-claim true / replay false)
/// with its `has_seen` pre-check, and the per-source strict-advance sequence gate
/// (baseline accept, replay/equal reject without advancing, strict advance).
///
/// `pub` (not `#[cfg(test)]`) by design: the shared backend-conformance suite,
/// like the other store seams. Panics on any violation — call it from a test.
///
/// PRECONDITION: `store` must start empty.
pub fn assert_federation_store_contract<S: FederationStore>(store: &S)
where
    S::Error: core::fmt::Debug,
{
    // --- controller key registry ---
    assert!(store
        .load_trusted_federation_controller_key("ctrl-A")
        .unwrap()
        .is_none());
    store
        .save_trusted_federation_controller("ctrl-A", "AAAAkey", 1)
        .unwrap();
    assert_eq!(
        store
            .load_trusted_federation_controller_key("ctrl-A")
            .unwrap()
            .as_deref(),
        Some("AAAAkey")
    );
    // Re-register overwrites the key in place (upsert), never duplicates.
    store
        .save_trusted_federation_controller("ctrl-A", "BBBBkey", 2)
        .unwrap();
    assert_eq!(
        store
            .load_trusted_federation_controller_key("ctrl-A")
            .unwrap()
            .as_deref(),
        Some("BBBBkey")
    );

    // --- nonce burn / replay ---
    assert!(!store.has_seen_federation_nonce("nonce-1").unwrap());
    assert!(
        store.burn_federation_nonce("nonce-1").unwrap(),
        "first claim of a nonce must succeed"
    );
    assert!(
        store.has_seen_federation_nonce("nonce-1").unwrap(),
        "a burned nonce must read as seen"
    );
    assert!(
        !store.burn_federation_nonce("nonce-1").unwrap(),
        "re-claiming a burned nonce must be rejected (replay)"
    );
    // A distinct nonce is independent.
    assert!(store.burn_federation_nonce("nonce-2").unwrap());

    // --- per-source strict-advance sequence gate ---
    // Baseline: any first sequence from a new source is accepted.
    assert!(store
        .industrial_seq_check_and_advance("src-A", 5, 100)
        .unwrap());
    // Replay (equal) and regress (<) are both rejected WITHOUT advancing.
    assert!(!store
        .industrial_seq_check_and_advance("src-A", 5, 101)
        .unwrap());
    assert!(!store
        .industrial_seq_check_and_advance("src-A", 3, 102)
        .unwrap());
    // Strict advance is accepted and moves the high-water.
    assert!(store
        .industrial_seq_check_and_advance("src-A", 6, 103)
        .unwrap());
    assert!(!store
        .industrial_seq_check_and_advance("src-A", 6, 104)
        .unwrap());
    // A different source keeps its own independent high-water.
    assert!(store
        .industrial_seq_check_and_advance("src-B", 1, 105)
        .unwrap());
}

#[cfg(test)]
mod federation_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_federation_store_contract() {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_federation_store_contract(&store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_federation_store_contract() {
        assert_federation_store_contract(&InMemoryFederationStore::default());
    }
}
