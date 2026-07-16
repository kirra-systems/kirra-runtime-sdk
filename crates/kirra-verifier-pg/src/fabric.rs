//! `PgVerifierStore` — FabricAssetStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PgVerifierStore {
    fn row_to_fabric_asset(row: &postgres::Row) -> FabricAsset {
        // Same lenient decode as the SQLite backend: an undecodable enum falls back
        // (asset_type → Unknown, profile → Custom), a bad metadata blob → empty map —
        // never a panic or a skipped row.
        let asset_type_s: String = row.get(1);
        let profile_s: String = row.get(3);
        let meta_s: String = row.get(6);
        FabricAsset {
            asset_id: row.get(0),
            asset_type: serde_json::from_str(&asset_type_s).unwrap_or(AssetType::Unknown),
            display_name: row.get(2),
            kinematic_profile: serde_json::from_str(&profile_s)
                .unwrap_or(KinematicProfileType::Custom),
            // Fail-CLOSED read (matches the save-path guard + `not_after_ms`): a
            // corrupt NEGATIVE stored timestamp maps to 0 rather than wrapping to a
            // huge u64 that would corrupt ordering/visibility. `u64::try_from` fails
            // only for negatives → 0.
            registered_at_ms: u64::try_from(row.get::<_, i64>(4)).unwrap_or(0),
            last_seen_ms: u64::try_from(row.get::<_, i64>(5)).unwrap_or(0),
            metadata: serde_json::from_str(&meta_s).unwrap_or_default(),
        }
    }
}

impl FabricAssetStore for PgVerifierStore {
    type Error = PgStoreError;

    fn save_fabric_asset(&self, asset: &FabricAsset) -> Result<(), PgStoreError> {
        // Fail-closed on out-of-domain timestamps (the #936 lesson) rather than
        // wrapping a u64 to a negative BIGINT.
        let reg_ms =
            i64::try_from(asset.registered_at_ms).map_err(|_| PgStoreError::OutOfDomain {
                field: "registered_at_ms",
                value: asset.registered_at_ms,
            })?;
        let last_ms = i64::try_from(asset.last_seen_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "last_seen_ms",
            value: asset.last_seen_ms,
        })?;
        // The enum fields + metadata map are JSON-serialized into TEXT, exactly as
        // the SQLite backend (same encoding → same bytes round-trip).
        let asset_type = serde_json::to_string(&asset.asset_type).unwrap_or_default();
        let profile = serde_json::to_string(&asset.kinematic_profile).unwrap_or_default();
        let metadata = serde_json::to_string(&asset.metadata).unwrap_or_else(|_| "{}".to_string());
        // Upsert by asset_id (SQLite: INSERT OR REPLACE).
        self.lock().execute(
            "INSERT INTO fabric_assets \
                 (asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, \
                  last_seen_ms, metadata_json) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (asset_id) DO UPDATE SET \
                 asset_type        = EXCLUDED.asset_type, \
                 display_name      = EXCLUDED.display_name, \
                 kinematic_profile = EXCLUDED.kinematic_profile, \
                 registered_at_ms  = EXCLUDED.registered_at_ms, \
                 last_seen_ms      = EXCLUDED.last_seen_ms, \
                 metadata_json     = EXCLUDED.metadata_json",
            &[
                &asset.asset_id,
                &asset_type,
                &asset.display_name,
                &profile,
                &reg_ms,
                &last_ms,
                &metadata,
            ],
        )?;
        Ok(())
    }

    fn load_fabric_assets(&self) -> Result<Vec<FabricAsset>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, \
                    last_seen_ms, metadata_json \
             FROM fabric_assets ORDER BY registered_at_ms, asset_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_fabric_asset).collect())
    }
}
