// crates/kirra-collector/src/manifest.rs
//
// Dataset manifest + lineage (docs/COLLECTOR_DESIGN.md Phase 2). Every dataset
// gets a `manifest.json` at its root recording WHAT produced it (collector +
// schema versions, git commit), WHAT went in (per-input sha256 + counts, the bag
// reference), HOW (the run params), the quality `Reconciliation`, the partitions
// written, and a reproducible `dataset_id` — so a model trained on this dataset
// can point back at exactly the data + provenance behind it.
//
// [M1] `dataset_id` is a sha256 over the LOGICAL content + lineage:
//   { sorted input sha256s, schema version, collector version, params,
//     content_digest }. It EXCLUDES `created_at` and the PHYSICAL Parquet bytes
//   (arrow/parquet encoding + metadata timestamps are nondeterministic) — so the
//   same inputs + params + collector build always reproduce the same id.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::dataset::{DatasetRow, PartitionInfo};
use crate::reconcile::Reconciliation;
use crate::{hex, CollectorConfig, CollectorError, InputDigest, Lineage};

const COLLECTOR_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_COMMIT: &str = env!("KIRRA_GIT_COMMIT");
const SCHEMA_VERSION: &str = env!("KIRRA_SCHEMA_VERSION");

/// The sampling scheme recorded in the manifest params.
pub const SAMPLING_KIND: &str = "fnv-keyed, deterministic";

/// `dataset_id` algorithm tag — bump if the canonical id construction changes
/// (so ids are never silently comparable across algorithm versions).
const DATASET_ID_ALGO: &str = "kirra-collector-dataset-id-v1";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CollectorInfo {
    pub version: String,
    pub git_commit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SchemaInfo {
    pub crate_name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BagRef {
    pub backend: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManifestParams {
    pub pass_rate: f64,
    pub window_ms: u64,
    pub max_orphan_rate: f64,
    pub sampling: String,
}

/// The full dataset manifest, written to `<out>/manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    /// Reproducible logical id [M1] — excludes `created_at` + physical bytes.
    pub dataset_id: String,
    /// Wall-clock build time (ISO-8601 UTC). Provenance only; NOT in the id.
    pub created_at: String,
    pub collector: CollectorInfo,
    pub schema: SchemaInfo,
    pub inputs: Vec<InputDigest>,
    pub bag: BagRef,
    pub params: ManifestParams,
    /// The run's quality accounting — the single source of truth (serialized as-is).
    pub quality: Reconciliation,
    pub partitions: Vec<PartitionInfo>,
    pub doer_versions: Vec<String>,
    pub decision_t_wall_ms_min: Option<u64>,
    pub decision_t_wall_ms_max: Option<u64>,
    /// sha256 over the canonical logical rows (partition-layout independent).
    pub content_digest: String,
}

/// sha256 over the canonical logical rows — sorted by `(source, decision_seq)`
/// (unique per run after dedup), each serialized to canonical JSON. Independent
/// of partition layout + physical Parquet encoding.
#[must_use]
pub fn content_digest(rows: &[DatasetRow]) -> String {
    let mut sorted: Vec<&DatasetRow> = rows.iter().collect();
    sorted.sort_by(|a, b| (a.source.as_str(), a.decision_seq).cmp(&(b.source.as_str(), b.decision_seq)));
    let mut h = Sha256::new();
    for row in sorted {
        let line = serde_json::to_string(row).expect("DatasetRow serializes infallibly");
        h.update(line.as_bytes());
        h.update(b"\n");
    }
    hex(&h.finalize())
}

/// Compute the reproducible `dataset_id` [M1]. Deliberately takes NO `created_at`
/// and NO physical Parquet bytes — only the logical content + lineage — so the
/// same inputs/params/build always yield the same id (INV-4).
#[must_use]
pub fn compute_dataset_id(
    input_sha256s: &[String],
    schema_version: &str,
    collector_version: &str,
    params: &ManifestParams,
    content_digest: &str,
) -> String {
    let mut inputs = input_sha256s.to_vec();
    inputs.sort();
    let mut h = Sha256::new();
    h.update(DATASET_ID_ALGO.as_bytes());
    h.update(b"\n");
    for s in &inputs {
        h.update(s.as_bytes());
        h.update(b"\n");
    }
    h.update(format!("schema={schema_version}\n").as_bytes());
    h.update(format!("collector={collector_version}\n").as_bytes());
    h.update(
        format!(
            "pass_rate={}\nwindow_ms={}\nmax_orphan_rate={}\nsampling={}\n",
            params.pass_rate, params.window_ms, params.max_orphan_rate, params.sampling
        )
        .as_bytes(),
    );
    h.update(format!("content={content_digest}\n").as_bytes());
    hex(&h.finalize())
}

/// Build the manifest after the dataset is written.
#[must_use]
pub fn build_manifest(
    lineage: &Lineage,
    bag_uri: &str,
    cfg: &CollectorConfig,
    quality: &Reconciliation,
    rows: &[DatasetRow],
    partitions: &[PartitionInfo],
) -> Manifest {
    let params = ManifestParams {
        pass_rate: cfg.pass_rate,
        window_ms: cfg.window_ms,
        max_orphan_rate: cfg.max_orphan_rate,
        sampling: SAMPLING_KIND.to_string(),
    };
    let content_digest = content_digest(rows);
    let input_sha256s: Vec<String> = lineage.inputs.iter().map(|i| i.sha256.clone()).collect();
    let dataset_id =
        compute_dataset_id(&input_sha256s, SCHEMA_VERSION, COLLECTOR_VERSION, &params, &content_digest);

    let mut doer_versions: Vec<String> = partitions.iter().map(|p| p.doer_version.clone()).collect();
    doer_versions.sort();
    doer_versions.dedup();

    let mut min = None;
    let mut max = None;
    for r in rows {
        min = Some(min.map_or(r.t_wall_ms, |m: u64| m.min(r.t_wall_ms)));
        max = Some(max.map_or(r.t_wall_ms, |m: u64| m.max(r.t_wall_ms)));
    }

    Manifest {
        dataset_id,
        created_at: now_iso8601_utc(),
        collector: CollectorInfo {
            version: COLLECTOR_VERSION.to_string(),
            git_commit: GIT_COMMIT.to_string(),
        },
        schema: SchemaInfo {
            crate_name: "kirra-capture-schema".to_string(),
            version: SCHEMA_VERSION.to_string(),
        },
        inputs: lineage.inputs.clone(),
        bag: BagRef {
            backend: lineage.bag_backend.clone(),
            uri: bag_uri.to_string(),
        },
        params,
        quality: quality.clone(),
        partitions: partitions.to_vec(),
        doer_versions,
        decision_t_wall_ms_min: min,
        decision_t_wall_ms_max: max,
        content_digest,
    }
}

/// Write the manifest to `<out_dir>/manifest.json` (pretty JSON).
pub fn write_manifest(manifest: &Manifest, out_dir: &Path) -> Result<PathBuf, CollectorError> {
    std::fs::create_dir_all(out_dir).map_err(CollectorError::Io)?;
    let path = out_dir.join("manifest.json");
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| CollectorError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    std::fs::write(&path, json).map_err(CollectorError::Io)?;
    Ok(path)
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`. Self-contained (no chrono dep);
/// uses Howard Hinnant's civil-from-days. Provenance only — excluded from the id.
fn now_iso8601_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// (year, month, day) from days since the Unix epoch (1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ManifestParams {
        ManifestParams {
            pass_rate: 1.0,
            window_ms: 100,
            max_orphan_rate: 0.05,
            sampling: SAMPLING_KIND.to_string(),
        }
    }

    #[test]
    fn dataset_id_is_stable_and_input_sensitive() {
        let base = compute_dataset_id(&["aaa".into(), "bbb".into()], "0.1.0", "0.1.0", &params(), "cd");
        // Same inputs → same id (order-independent on the input list).
        assert_eq!(
            base,
            compute_dataset_id(&["bbb".into(), "aaa".into()], "0.1.0", "0.1.0", &params(), "cd")
        );
        // A different input hash → different id.
        assert_ne!(
            base,
            compute_dataset_id(&["aaa".into(), "ccc".into()], "0.1.0", "0.1.0", &params(), "cd")
        );
        // A different param → different id.
        let mut p2 = params();
        p2.pass_rate = 0.5;
        assert_ne!(base, compute_dataset_id(&["aaa".into(), "bbb".into()], "0.1.0", "0.1.0", &p2, "cd"));
        // A different content digest → different id.
        assert_ne!(
            base,
            compute_dataset_id(&["aaa".into(), "bbb".into()], "0.1.0", "0.1.0", &params(), "cd2")
        );
    }

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_993), (2022, 1, 1));
        // 2000-01-01 is 10957 days after the epoch.
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }
}
