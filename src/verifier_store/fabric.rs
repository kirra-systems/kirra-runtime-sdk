// src/verifier_store/fabric.rs
// fabric domain — split from verifier_store.rs (pure move).

use super::*;

impl VerifierStore {
    pub fn save_fabric_asset(&self, asset: &crate::fabric::asset::FabricAsset) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO fabric_assets
             (asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, last_seen_ms, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                asset.asset_id,
                serde_json::to_string(&asset.asset_type).unwrap_or_default(),
                asset.display_name,
                serde_json::to_string(&asset.kinematic_profile).unwrap_or_default(),
                asset.registered_at_ms as i64,
                asset.last_seen_ms as i64,
                serde_json::to_string(&asset.metadata).unwrap_or_else(|_| "{}".to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn load_fabric_assets(&self) -> Result<Vec<crate::fabric::asset::FabricAsset>> {
        let mut stmt = self.conn.prepare(
            "SELECT asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, last_seen_ms, metadata_json
             FROM fabric_assets ORDER BY registered_at_ms"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut assets = Vec::new();
        for row in rows {
            let (asset_id, asset_type_s, display_name, profile_s, reg_ms, last_ms, meta_s) = row?;
            let asset_type = serde_json::from_str(&asset_type_s)
                .unwrap_or(crate::fabric::asset::AssetType::Unknown);
            let kinematic_profile = serde_json::from_str(&profile_s)
                .unwrap_or(crate::fabric::asset::KinematicProfileType::Custom);
            let metadata = serde_json::from_str(&meta_s).unwrap_or_default();
            assets.push(crate::fabric::asset::FabricAsset {
                asset_id,
                asset_type,
                display_name,
                kinematic_profile,
                registered_at_ms: reg_ms as u64,
                last_seen_ms: last_ms as u64,
                metadata,
            });
        }
        Ok(assets)
    }

    /// Append a causal-log event to the hash-chained, signed, persisted ledger.
    ///
    /// Mirrors the audit ledger's [`super::append_audit_event_tx`]:
    /// reads the prev `(record_hash, sequence)` (fail-closed on real read
    /// errors; only `QueryReturnedNoRows` is genesis), computes the record hash
    /// (binding the causality edges), signs the canonical causal payload, records
    /// the content-addressed `key_id`, INSERTs the row, and advances the signed
    /// causal anchor-head in the SAME transaction. Returns the fully-populated
    /// `CausalLogEntry`.
    pub fn append_causal_event(
        &mut self,
        event: &CausalEventInput<'_>,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<crate::fabric::causal_log::CausalLogEntry> {
        let CausalEventInput {
            entry_id,
            asset_id,
            event_type,
            payload,
            caused_by,
            affects_assets,
            fabric_generation,
            timestamp_ms,
        } = *event;
        use ed25519_dalek::Signer;
        use kirra_audit_hash::{
            canonical_causal_anchor_head_payload, canonical_causal_signing_payload,
            compute_causal_record_hash, verifying_key_id,
        };

        let tx = self.conn.unchecked_transaction()?;

        // Read previous (record_hash, sequence). FAIL CLOSED on real read errors;
        // only an empty table is legitimate genesis.
        let prev = tx.query_row(
            "SELECT record_hash_hex, sequence FROM fabric_causal_log \
             ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        );
        let (previous_hash, prev_seq) = match prev {
            Ok((h, seq)) => (h, seq),
            Err(rusqlite::Error::QueryReturnedNoRows) => ("0".repeat(64), -1),
            Err(e) => return Err(e), // FAIL CLOSED — never fork-to-genesis on read error
        };
        let sequence: u64 = (prev_seq + 1) as u64;

        let record_hash = compute_causal_record_hash(&kirra_audit_hash::CausalRecordHashInput {
            previous_hash: &previous_hash,
            entry_id,
            asset_id,
            event_type,
            payload,
            caused_by,
            affects_assets,
            timestamp_ms,
            fabric_generation,
            sequence,
        });

        let signature_b64: Option<String> = signing_key.map(|k| {
            let payload_str = canonical_causal_signing_payload(
                &previous_hash,
                &record_hash,
                event_type,
                timestamp_ms,
                sequence,
            );
            b64e.encode(k.sign(payload_str.as_bytes()).to_bytes())
        });
        let key_id: Option<String> = signing_key.map(|k| verifying_key_id(&k.verifying_key()));

        let caused_by_json = serde_json::to_string(caused_by).unwrap_or_else(|_| "[]".to_string());
        let affects_json =
            serde_json::to_string(affects_assets).unwrap_or_else(|_| "[]".to_string());

        tx.execute(
            "INSERT INTO fabric_causal_log
             (entry_id, sequence, timestamp_ms, asset_id, event_type, payload,
              caused_by, affects_assets, fabric_generation,
              previous_hash_hex, record_hash_hex, signature_b64, key_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                entry_id,
                sequence as i64,
                timestamp_ms as i64,
                asset_id,
                event_type,
                payload,
                caused_by_json,
                affects_json,
                fabric_generation as i64,
                previous_hash,
                record_hash,
                signature_b64,
                key_id,
            ],
        )?;

        // Advance the signed anchor-HEAD high-water mark in the SAME tx.
        let head_sig: Option<String> = signing_key.map(|k| {
            let payload_str = canonical_causal_anchor_head_payload(sequence, &record_hash);
            b64e.encode(k.sign(payload_str.as_bytes()).to_bytes())
        });
        tx.execute(
            "INSERT INTO fabric_causal_anchor_head (id, sequence, record_hash_hex, signature_b64, key_id)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 sequence        = excluded.sequence,
                 record_hash_hex = excluded.record_hash_hex,
                 signature_b64   = excluded.signature_b64,
                 key_id          = excluded.key_id",
            params![sequence as i64, record_hash, head_sig, key_id],
        )?;

        tx.commit()?;

        Ok(crate::fabric::causal_log::CausalLogEntry {
            entry_id: entry_id.to_string(),
            sequence,
            timestamp_ms,
            asset_id: asset_id.to_string(),
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            caused_by: caused_by.to_vec(),
            affects_assets: affects_assets.to_vec(),
            fabric_generation,
            previous_hash,
            record_hash,
            signature_b64,
            key_id,
        })
    }

    /// Decode one `fabric_causal_log` row from a query row. Column order must
    /// match the SELECT in the loaders below.
    fn causal_entry_from_row(
        row: &rusqlite::Row,
    ) -> Result<crate::fabric::causal_log::CausalLogEntry> {
        let entry_id: String = row.get(0)?;
        let sequence: i64 = row.get(1)?;
        let timestamp_ms: i64 = row.get(2)?;
        let asset_id: String = row.get(3)?;
        let event_type: String = row.get(4)?;
        let payload: String = row.get(5)?;
        let caused_by_json: String = row.get(6)?;
        let affects_json: String = row.get(7)?;
        let fabric_generation: i64 = row.get(8)?;
        let previous_hash: String = row.get(9)?;
        let record_hash: String = row.get(10)?;
        let signature_b64: Option<String> = row.get(11)?;
        let key_id: Option<String> = row.get(12)?;
        Ok(crate::fabric::causal_log::CausalLogEntry {
            entry_id,
            sequence: sequence.max(0) as u64,
            timestamp_ms: timestamp_ms.max(0) as u64,
            asset_id,
            event_type,
            payload,
            caused_by: serde_json::from_str(&caused_by_json).unwrap_or_default(),
            affects_assets: serde_json::from_str(&affects_json).unwrap_or_default(),
            fabric_generation: fabric_generation.max(0) as u64,
            previous_hash,
            record_hash,
            signature_b64,
            key_id,
        })
    }

    /// Load every causal-log entry in chain (id ASC) order.
    pub fn load_causal_entries(&self) -> Result<Vec<crate::fabric::causal_log::CausalLogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::causal_entry_from_row(row)?);
        }
        Ok(out)
    }

    /// Load causal-log entries whose timestamp falls in `[from_ms, to_ms]`,
    /// in chain order, bounded by `limit`/`offset`.
    pub fn load_causal_entries_in_range(
        &self,
        from_ms: u64,
        to_ms: u64,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::fabric::causal_log::CausalLogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log \
             WHERE timestamp_ms BETWEEN ?1 AND ?2 \
             ORDER BY id ASC LIMIT ?3 OFFSET ?4",
        )?;
        let mut rows = stmt.query(params![
            from_ms as i64,
            to_ms.min(i64::MAX as u64) as i64,
            limit as i64,
            offset as i64,
        ])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::causal_entry_from_row(row)?);
        }
        Ok(out)
    }

    /// Count of causal-log entries.
    pub fn count_causal_entries(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM fabric_causal_log", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
    }

    /// Verify the causal-log forensic chain (#87). Mirrors
    /// [`Self::verify_audit_chain_full`]: walks the rows checking prev-linkage,
    /// recomputed record hash (which binds the edges), and sequence monotonicity;
    /// verifies each row's signature under the key its `key_id` names (selected
    /// from the SHARED audit keyring — causal rows are signed by the same audit
    /// key and rotations live in the audit chain); then checks the signed
    /// anchor-head high-water mark for tail truncation/tamper.
    pub fn verify_causal_chain_integrity(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<CausalChainVerifyResult> {
        use kirra_audit_hash::{
            canonical_causal_anchor_head_payload, canonical_causal_signing_payload,
            compute_causal_record_hash,
        };

        // REUSE the #76 audit keyring (genesis from durable anchor + verified
        // rotations). Causal rows are signed by the SAME audit key. If no
        // verifying key is supplied, skip signature verification (like audit).
        let (keyring, genesis_id) = self.audit_keyring_seed(verifying_key)?;
        let keyring = match verifying_key {
            Some(g) => self.build_audit_keyring(g)?,
            None => keyring,
        };

        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log ORDER BY id ASC",
        )?;

        let mut chain_intact = true;
        let mut total_entries: u64 = 0;
        let mut latest_hash = "0".repeat(64);
        let mut expected_previous_hash = "0".repeat(64);
        let mut signed_entries: u64 = 0;
        let mut unsigned_entries: u64 = 0;
        let mut signature_valid = true;
        let mut first_invalid_signature_index: Option<u64> = None;
        let mut first_signed_at_ms: Option<u64> = None;
        let mut prev_seq: Option<i64> = None;
        let mut last_sequence: Option<i64> = None;

        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let entry_id: String = row.get(0)?;
            let sequence: i64 = row.get(1)?;
            let timestamp_ms: i64 = row.get(2)?;
            let asset_id: String = row.get(3)?;
            let event_type: String = row.get(4)?;
            let payload: String = row.get(5)?;
            let caused_by_json: String = row.get(6)?;
            let affects_json: String = row.get(7)?;
            let fabric_generation: i64 = row.get(8)?;
            let previous_hash_hex: String = row.get(9)?;
            let record_hash_hex: String = row.get(10)?;
            let sig_b64: Option<String> = row.get(11)?;
            let key_id_opt: Option<String> = row.get(12)?;

            // Prev-linkage.
            if previous_hash_hex != expected_previous_hash {
                chain_intact = false;
            }
            // Sequence monotonicity: first row 0, each next prev+1.
            match prev_seq {
                None => {
                    if sequence != 0 {
                        chain_intact = false;
                    }
                }
                Some(p) => {
                    if sequence != p + 1 {
                        chain_intact = false;
                    }
                }
            }
            prev_seq = Some(sequence);

            let caused_by: Vec<String> = serde_json::from_str(&caused_by_json).unwrap_or_default();
            let affects_assets: Vec<String> =
                serde_json::from_str(&affects_json).unwrap_or_default();
            let recalc = compute_causal_record_hash(&kirra_audit_hash::CausalRecordHashInput {
                previous_hash: &previous_hash_hex,
                entry_id: &entry_id,
                asset_id: &asset_id,
                event_type: &event_type,
                payload: &payload,
                caused_by: &caused_by,
                affects_assets: &affects_assets,
                timestamp_ms: timestamp_ms.max(0) as u64,
                fabric_generation: fabric_generation.max(0) as u64,
                sequence: sequence.max(0) as u64,
            });
            if recalc != record_hash_hex {
                chain_intact = false;
            }
            expected_previous_hash = record_hash_hex.clone();
            latest_hash = record_hash_hex.clone();
            last_sequence = Some(sequence);

            match &sig_b64 {
                None => unsigned_entries += 1,
                Some(s) => {
                    signed_entries += 1;
                    if first_signed_at_ms.is_none() {
                        first_signed_at_ms = Some(timestamp_ms.max(0) as u64);
                    }
                    if verifying_key.is_some() {
                        let signer_id = key_id_opt
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let ok = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload_str = canonical_causal_signing_payload(
                                    &previous_hash_hex,
                                    &record_hash_hex,
                                    &event_type,
                                    timestamp_ms.max(0) as u64,
                                    sequence.max(0) as u64,
                                );
                                audit_verify_sig(vk, &payload_str, s)
                            }
                            None => false, // unknown key_id → FAIL-CLOSED
                        };
                        if !ok && first_invalid_signature_index.is_none() {
                            first_invalid_signature_index = Some(total_entries);
                            signature_valid = false;
                        }
                    }
                }
            }

            total_entries += 1;
        }

        let signing_enabled = verifying_key.is_some();
        let public_key_b64 = verifying_key.map(|vk| b64e.encode(vk.as_bytes()));

        // Anchor-HEAD high-water check — detects tail truncation/deletion.
        let (head_verified, head_status): (bool, String) = if total_entries == 0 {
            (true, "EMPTY_CHAIN".to_string())
        } else {
            let head = self.conn.query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM fabric_causal_anchor_head WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            );
            match head {
                Err(rusqlite::Error::QueryReturnedNoRows) => (false, "HEAD_ABSENT".to_string()),
                Err(e) => return Err(e),
                Ok((h_seq, h_hash, h_sig, h_key_id)) => {
                    if Some(h_seq) != last_sequence || h_hash != latest_hash {
                        let truncated = match last_sequence {
                            Some(t) => t < h_seq,
                            None => true,
                        };
                        let status = if truncated {
                            "TRUNCATION_DETECTED"
                        } else {
                            "HEAD_TAIL_MISMATCH"
                        };
                        (false, status.to_string())
                    } else if signing_enabled {
                        match h_sig {
                            None => (false, "HEAD_UNSIGNED".to_string()),
                            Some(sig) => {
                                let signer_id =
                                    h_key_id.or_else(|| genesis_id.clone()).unwrap_or_default();
                                match keyring.get(&signer_id) {
                                    None => (false, "HEAD_KEY_UNKNOWN".to_string()),
                                    Some(vk) => {
                                        let payload_str = canonical_causal_anchor_head_payload(
                                            h_seq.max(0) as u64,
                                            &h_hash,
                                        );
                                        if audit_verify_sig(vk, &payload_str, &sig) {
                                            (true, "OK".to_string())
                                        } else {
                                            (false, "HEAD_SIGNATURE_INVALID".to_string())
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        (true, "OK_UNSIGNED".to_string())
                    }
                }
            }
        };

        Ok(CausalChainVerifyResult {
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
}

// ---------------------------------------------------------------------------
// ADR-0035 Stage 2.5 seam step (family 2) — the fabric-asset storage trait
//
// The fabric family's pure-CRUD core — the ASSET REGISTRY — seamed as CRUD like
// the clean six (NodeStore idiom): the trait shares the inherent method names, so
// inherent methods win resolution (every existing `store.save_fabric_asset(...)` /
// `store.load_fabric_assets(...)` caller is untouched and the SQLite impl delegates
// via `self.method()` WITHOUT recursion). A second in-memory backend + a shared
// conformance test prove the registry contract is backend-portable.
//
// Scope, matching OperatorStore / OtaCampaignStore: this trait models the pure,
// non-audit asset registry only. The forensic CAUSAL LEDGER stays INHERENT-ONLY —
// `append_causal_event` is a hash-chained, signed write (the causal analogue of the
// audit chain), and `load_causal_entries*` / `count_causal_entries` /
// `verify_causal_chain_integrity` read/verify that chained, key-signed data. It is
// the authority tier, not a portable storage contract, so it is deliberately out of
// this seam (the same reason `update_campaign` and the clearance-grant writes stay
// inherent).
// ---------------------------------------------------------------------------

/// The fabric-asset registry storage contract — upsert a `FabricAsset` by
/// `asset_id` and read the registry back (ordered by `registered_at_ms`).
/// Backend-agnostic; carries none of the causal-ledger audit machinery.
pub trait FabricAssetStore {
    /// Backend error type (SQLite: `rusqlite::Error`; in-memory: [`std::convert::Infallible`]).
    type Error;

    /// Upsert an asset by `asset_id` (INSERT-OR-REPLACE — re-saving the same id
    /// overwrites, never duplicates).
    fn save_fabric_asset(
        &self,
        asset: &crate::fabric::asset::FabricAsset,
    ) -> std::result::Result<(), Self::Error>;

    /// Load every registered asset, ordered by `registered_at_ms` (ascending).
    fn load_fabric_assets(
        &self,
    ) -> std::result::Result<Vec<crate::fabric::asset::FabricAsset>, Self::Error>;
}

/// The production SQLite backend: delegates to the inherent `VerifierStore` methods
/// over the `fabric_assets` table. `self.method()` resolves to the INHERENT method
/// (inherent wins over the trait), so this is delegation, not recursion.
impl FabricAssetStore for VerifierStore {
    type Error = rusqlite::Error;

    fn save_fabric_asset(&self, asset: &crate::fabric::asset::FabricAsset) -> Result<()> {
        self.save_fabric_asset(asset)
    }
    fn load_fabric_assets(&self) -> Result<Vec<crate::fabric::asset::FabricAsset>> {
        self.load_fabric_assets()
    }
}

/// The in-memory [`FabricAssetStore`] backend — a portability-proof reference
/// modelling the `fabric_assets` table as a map keyed by `asset_id`. Realizes the
/// SAME upsert + ordered-load semantics WITHOUT a database. Interior mutability
/// (the trait's `save` is `&self`, matching the SQLite `Connection`'s `&self`
/// writes). Single-process only.
///
/// `Error = Infallible` is honest: `INSERT OR REPLACE` never conflicts, and every
/// method RECOVERS from a poisoned `Mutex` rather than unwrapping, so a panic in
/// another thread can never make a `FabricAssetStore` op panic.
#[derive(Debug, Default)]
pub struct InMemoryFabricAssetStore {
    assets: std::sync::Mutex<std::collections::HashMap<String, crate::fabric::asset::FabricAsset>>,
}

impl FabricAssetStore for InMemoryFabricAssetStore {
    type Error = std::convert::Infallible;

    fn save_fabric_asset(
        &self,
        asset: &crate::fabric::asset::FabricAsset,
    ) -> std::result::Result<(), std::convert::Infallible> {
        self.assets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(asset.asset_id.clone(), asset.clone());
        Ok(())
    }

    fn load_fabric_assets(
        &self,
    ) -> std::result::Result<Vec<crate::fabric::asset::FabricAsset>, std::convert::Infallible> {
        let mut all: Vec<crate::fabric::asset::FabricAsset> = self
            .assets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        // Ordered by registered_at_ms ASC (then asset_id for a deterministic tie-break).
        all.sort_by(|a, b| {
            a.registered_at_ms
                .cmp(&b.registered_at_ms)
                .then_with(|| a.asset_id.cmp(&b.asset_id))
        });
        Ok(all)
    }
}

/// The fabric-asset registry contract, driven through [`FabricAssetStore`] so it
/// runs IDENTICALLY against every backend: empty read, save→load roundtrip (all
/// fields — type, kinematic profile, metadata — preserved), the UPSERT invariant
/// (re-saving an id overwrites, never duplicates), and `registered_at_ms` ordering.
///
/// `pub` (not `#[cfg(test)]`) by design — the shared backend-conformance suite,
/// mirroring `assert_node_store_contract`. Panics on any violation; call from a test.
///
/// PRECONDITION: `store` must start empty.
pub fn assert_fabric_asset_store_contract<S: FabricAssetStore>(store: &S)
where
    S::Error: core::fmt::Debug,
{
    use crate::fabric::asset::{AssetType, FabricAsset, KinematicProfileType};
    fn asset(id: &str, at: u64, atype: AssetType, profile: KinematicProfileType) -> FabricAsset {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("site".to_string(), "dock-7".to_string());
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: atype,
            display_name: format!("asset {id}"),
            kinematic_profile: profile,
            registered_at_ms: at,
            last_seen_ms: at,
            metadata,
        }
    }

    // Empty registry.
    assert!(store.load_fabric_assets().unwrap().is_empty());

    // Save + read back (all fields preserved).
    let a1 = asset(
        "drone-1",
        2_000,
        AssetType::Drone,
        KinematicProfileType::DroneNominal,
    );
    store.save_fabric_asset(&a1).unwrap();
    let loaded = store.load_fabric_assets().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].asset_id, "drone-1");
    assert_eq!(loaded[0].asset_type, AssetType::Drone);
    assert_eq!(
        loaded[0].kinematic_profile,
        KinematicProfileType::DroneNominal
    );
    assert_eq!(
        loaded[0].metadata.get("site").map(String::as_str),
        Some("dock-7")
    );

    // A second asset registered EARLIER — load orders by registered_at_ms ASC.
    let a2 = asset(
        "robot-1",
        1_000,
        AssetType::Robot,
        KinematicProfileType::RobotNominal,
    );
    store.save_fabric_asset(&a2).unwrap();
    let ids: Vec<String> = store
        .load_fabric_assets()
        .unwrap()
        .into_iter()
        .map(|a| a.asset_id)
        .collect();
    assert_eq!(
        ids,
        vec!["robot-1", "drone-1"],
        "ordered by registered_at_ms ASC"
    );

    // UPSERT: re-saving drone-1 with a new type/name overwrites, count stays 2.
    let a1b = asset(
        "drone-1",
        2_000,
        AssetType::AutonomousVehicle,
        KinematicProfileType::AutomotiveNominal,
    );
    store.save_fabric_asset(&a1b).unwrap();
    let all = store.load_fabric_assets().unwrap();
    assert_eq!(all.len(), 2, "upsert must not duplicate");
    let d1 = all.iter().find(|a| a.asset_id == "drone-1").unwrap();
    assert_eq!(
        d1.asset_type,
        AssetType::AutonomousVehicle,
        "upsert overwrites"
    );
}

#[cfg(test)]
mod fabric_asset_store_contract_tests {
    use super::*;

    #[test]
    fn sqlite_backend_satisfies_the_fabric_asset_store_contract() {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        assert_fabric_asset_store_contract(&store);
    }

    #[test]
    fn in_memory_backend_satisfies_the_fabric_asset_store_contract() {
        assert_fabric_asset_store_contract(&InMemoryFabricAssetStore::default());
    }
}
