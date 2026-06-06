// crates/kirra-collector/src/lib.rs
//
// kirra-collector — the OFFLINE learning-loop collector (docs/COLLECTOR_DESIGN.md).
// It reads the two dark capture JSONL streams, keeps every intervention while
// sampling passes [D5], joins each kept record to a bus recording [D2], attaches
// the doer's model version [D3], writes a training-ready Parquet dataset [D4],
// and (Phase 2) emits a `manifest.json` with lineage + a reproducible
// `dataset_id` so a model trained on it can point back at exactly this data.
//
// §0 SAFETY BOUNDARY: this crate depends on `kirra-capture-schema` ONLY (plus
// serde / serde_json / arrow / parquet / sha2) — NEVER on `kirra-runtime-sdk`.
// It is offline, out-of-vehicle, produces only a dataset, and is mechanically
// incapable of linking or reaching the verdict path. `cargo tree -p
// kirra-collector` is the enforced check.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use kirra_capture_schema::{CaptureOutcome, CaptureRecord, CaptureSource};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub mod bag;
pub mod dataset;
pub mod join;
pub mod manifest;
pub mod reconcile;
pub mod sample;

use bag::BagReader;
use join::JoinOutcome;
use manifest::Manifest;
use reconcile::Reconciliation;

/// Stable string token for a capture source — the partition value and join key.
#[must_use]
pub fn source_token(s: CaptureSource) -> &'static str {
    match s {
        CaptureSource::CommandGateway => "COMMAND_GATEWAY",
        CaptureSource::SlowLoopTrajectory => "SLOW_LOOP_TRAJECTORY",
    }
}

/// Stable string token for an outcome — the dataset's training label. Matches the
/// schema crate's SCREAMING_SNAKE serde rename so labels are consistent on disk.
#[must_use]
pub fn outcome_token(o: CaptureOutcome) -> &'static str {
    match o {
        CaptureOutcome::Allow => "ALLOW",
        CaptureOutcome::ClampLinear => "CLAMP_LINEAR",
        CaptureOutcome::ClampSteering => "CLAMP_STEERING",
        CaptureOutcome::Deny => "DENY",
    }
}

/// Lowercase hex of a byte slice.
#[must_use]
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 of a byte slice, lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// Per-input-file provenance (manifest `inputs[]`). The `sha256` is over the raw
/// file bytes; `record_count` and the per-source counts are this file's
/// contribution.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputDigest {
    pub name: String,
    pub sha256: String,
    pub record_count: usize,
    pub command_gateway: usize,
    pub slow_loop_trajectory: usize,
}

/// Provenance fed into `run` alongside the records — kept separate from
/// `CollectorConfig` (which is pure algorithm params).
#[derive(Debug, Clone)]
pub struct Lineage {
    /// Per-capture-file digests (sha256 + counts).
    pub inputs: Vec<InputDigest>,
    /// Which bag backend produced the join (`bag-json`, `db3`, `mcap`).
    pub bag_backend: String,
}

/// Collector run configuration (the algorithm/run params — NOT provenance).
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Pass-sampling rate in [0, 1]; 1.0 keeps every pass [D5].
    pub pass_rate: f64,
    /// Join window (± ms) around each record's `t_wall_ms` [D2].
    pub window_ms: u64,
    /// Fail the run if the orphan rate exceeds this (data-quality gate).
    pub max_orphan_rate: f64,
    /// Output dataset root; partitions + `manifest.json` are written beneath it.
    pub out_dir: PathBuf,
}

/// Errors the collector can fail with.
#[derive(Debug)]
pub enum CollectorError {
    Io(std::io::Error),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    /// The orphan rate exceeded `max_orphan_rate`. The dataset + manifest were
    /// still written; the manifest is carried so the caller can report the
    /// `dataset_id` + quality of the flagged dataset.
    OrphanRateExceeded { manifest: Box<Manifest>, max: f64 },
}

impl std::fmt::Display for CollectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectorError::Io(e) => write!(f, "io error: {e}"),
            CollectorError::Arrow(e) => write!(f, "arrow error: {e}"),
            CollectorError::Parquet(e) => write!(f, "parquet error: {e}"),
            CollectorError::OrphanRateExceeded { manifest, max } => write!(
                f,
                "orphan rate {:.3} exceeds ceiling {:.3} ({} of {} kept records unjoined)",
                manifest.quality.orphan_rate,
                max,
                manifest.quality.orphans,
                manifest.quality.kept_after_sampling
            ),
        }
    }
}

impl std::error::Error for CollectorError {}

/// Read capture records from one or more JSONL files AND compute per-file
/// provenance (sha256 over the raw bytes + record / per-source counts). Blank
/// lines are skipped; malformed lines are logged to stderr and skipped (a single
/// bad line never aborts a session's ingest).
pub fn read_jsonl_with_digests(
    paths: &[PathBuf],
) -> std::io::Result<(Vec<CaptureRecord>, Vec<InputDigest>)> {
    let mut all = Vec::new();
    let mut digests = Vec::new();
    for path in paths {
        let bytes = fs::read(path)?;
        let sha256 = sha256_hex(&bytes);
        let (mut record_count, mut command_gateway, mut slow_loop_trajectory) = (0, 0, 0);
        for (idx, raw) in bytes.split(|b| *b == b'\n').enumerate() {
            let line = std::str::from_utf8(raw).unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<CaptureRecord>(line) {
                Ok(rec) => {
                    record_count += 1;
                    match rec.source {
                        CaptureSource::CommandGateway => command_gateway += 1,
                        CaptureSource::SlowLoopTrajectory => slow_loop_trajectory += 1,
                    }
                    all.push(rec);
                }
                Err(e) => eprintln!(
                    "warn: skipping malformed capture line {}:{}: {e}",
                    path.display(),
                    idx + 1
                ),
            }
        }
        digests.push(InputDigest {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            sha256,
            record_count,
            command_gateway,
            slow_loop_trajectory,
        });
    }
    Ok((all, digests))
}

/// Read capture records only (provenance discarded). Thin wrapper over
/// `read_jsonl_with_digests` so existing callers are unchanged.
pub fn read_jsonl(paths: &[PathBuf]) -> std::io::Result<Vec<CaptureRecord>> {
    Ok(read_jsonl_with_digests(paths)?.0)
}

/// Deduplicate by `(source, decision_seq)` (the primary key [D2]) and return a
/// stably-ordered vec plus the count of duplicates dropped. First occurrence
/// wins; BTreeMap gives a deterministic `(source, decision_seq)` ordering so the
/// dataset is reproducible.
#[must_use]
pub fn index_dedup(records: Vec<CaptureRecord>) -> (Vec<CaptureRecord>, usize) {
    let mut map: BTreeMap<(&'static str, u64), CaptureRecord> = BTreeMap::new();
    let mut duplicates = 0usize;
    for rec in records {
        let key = (source_token(rec.source), rec.decision_seq);
        if map.contains_key(&key) {
            duplicates += 1;
            continue;
        }
        map.insert(key, rec);
    }
    (map.into_values().collect(), duplicates)
}

/// The whole pipeline: dedup → stratified sample → join → write Parquet → write
/// manifest → reconcile. Writes the dataset + `manifest.json` under
/// `cfg.out_dir` and returns the `Manifest` (which embeds the `Reconciliation` as
/// `quality` and the reproducible `dataset_id`). Fails (after writing both) if
/// the orphan rate exceeds the ceiling — the error still carries the manifest.
pub fn run(
    records: Vec<CaptureRecord>,
    bag: &dyn BagReader,
    lineage: &Lineage,
    cfg: &CollectorConfig,
) -> Result<Manifest, CollectorError> {
    let (deduped, duplicates_dropped) = index_dedup(records);

    let mut recon = Reconciliation {
        records_in: deduped.len(),
        duplicates_dropped,
        records_in_command_gateway: 0,
        records_in_slow_loop_trajectory: 0,
        interventions_in: 0,
        passes_in: 0,
        kept_after_sampling: 0,
        interventions_kept: 0,
        passes_kept: 0,
        joined: 0,
        orphans: 0,
        applied_pass_rate: cfg.pass_rate,
        orphan_rate: 0.0,
    };

    let mut rows = Vec::new();
    for rec in &deduped {
        match rec.source {
            CaptureSource::CommandGateway => recon.records_in_command_gateway += 1,
            CaptureSource::SlowLoopTrajectory => recon.records_in_slow_loop_trajectory += 1,
        }
        let intervention = sample::is_intervention(rec);
        if intervention {
            recon.interventions_in += 1;
        } else {
            recon.passes_in += 1;
        }

        if !sample::keep(rec, cfg.pass_rate) {
            continue;
        }
        recon.kept_after_sampling += 1;
        if intervention {
            recon.interventions_kept += 1;
        } else {
            recon.passes_kept += 1;
        }

        match join::join_record(rec, bag, cfg.window_ms) {
            JoinOutcome::Joined(m) => {
                recon.joined += 1;
                rows.push(dataset::build_row(rec, &m));
            }
            JoinOutcome::Orphan => recon.orphans += 1,
        }
    }

    // Canonical order for a reproducible content digest [M2] (already the
    // dedup order; explicit for robustness).
    rows.sort_by(|a, b| (a.source.as_str(), a.decision_seq).cmp(&(b.source.as_str(), b.decision_seq)));

    let partitions = dataset::write_dataset(&rows, &cfg.out_dir)?;
    recon.orphan_rate = recon.orphan_rate();

    let manifest = manifest::build_manifest(lineage, bag.bag_uri(), cfg, &recon, &rows, &partitions);
    manifest::write_manifest(&manifest, &cfg.out_dir)?;

    if recon.orphan_rate > cfg.max_orphan_rate {
        return Err(CollectorError::OrphanRateExceeded {
            manifest: Box::new(manifest),
            max: cfg.max_orphan_rate,
        });
    }
    Ok(manifest)
}

/// Convenience: list the part files under a dataset root (sorted), for callers
/// that want to enumerate what was written.
pub fn list_parquet_parts(out_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut parts = Vec::new();
    fn walk(dir: &Path, parts: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, parts)?;
            } else if path.extension().is_some_and(|e| e == "parquet") {
                parts.push(path);
            }
        }
        Ok(())
    }
    walk(out_dir, &mut parts)?;
    parts.sort();
    Ok(parts)
}
