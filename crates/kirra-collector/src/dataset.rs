// crates/kirra-collector/src/dataset.rs
//
// The dataset writer (docs/COLLECTOR_DESIGN.md [D4]). One flat row per kept,
// joined decision, written as Parquet partitioned `doer_version=<v>/source=<s>/`.
// The row holds the triple SUMMARY (the proposal/trajectory summary + outcome
// label + posture + join keys) plus `bulk_ref` — a reference into the bag for the
// heavy frames. It deliberately has NO column for raw points / object lists /
// sensor frames: those live in the bag and are NEVER copied into Parquet.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Float64Array, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use kirra_capture_schema::CaptureRecord;

use crate::bag::BusMatch;
use crate::{outcome_token, source_token, CollectorError};

/// Provenance for one written partition (recorded in the dataset manifest).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PartitionInfo {
    pub doer_version: String,
    pub source: String,
    pub row_count: usize,
    /// Parquet path RELATIVE to the dataset root (e.g.
    /// `doer_version=v1/source=COMMAND_GATEWAY/part-000.parquet`).
    pub relative_path: String,
}

/// One row of the training dataset — the joined summary. Flat (one nullable
/// column per source-specific field) so it maps directly onto an Arrow batch.
/// `Serialize` is used only for the canonical content digest (Phase 2), never on
/// the verdict/wire path.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DatasetRow {
    // --- common (every record) ---
    pub decision_seq: u64,
    pub source: String,
    pub t_wall_ms: u64,
    /// u128 has no Arrow type; carried as a decimal string (it is an ordering
    /// key, never arithmetic in the dataset).
    pub t_mono_ns: String,
    pub doer_version: String,
    pub outcome: String,
    pub deny_code: Option<String>,
    pub safe_value: Option<f64>,
    pub mrc: bool,
    pub posture: String,
    pub derate_enabled: bool,
    // --- gateway-only (None on trajectory rows) ---
    pub proposed_linear_velocity_mps: Option<f64>,
    pub proposed_current_velocity_mps: Option<f64>,
    pub proposed_steering_angle_deg: Option<f64>,
    pub proposed_current_steering_angle_deg: Option<f64>,
    pub proposed_delta_time_s: Option<f64>,
    // --- trajectory-only (None on gateway rows) ---
    pub asset_id: Option<String>,
    pub trajectory_id: Option<u64>,
    pub objects_ms: Option<u64>,
    pub point_count: Option<u64>,
    pub object_count: Option<u64>,
    pub first_pose_x_m: Option<f64>,
    pub first_pose_y_m: Option<f64>,
    pub first_pose_heading_rad: Option<f64>,
    pub last_pose_x_m: Option<f64>,
    pub last_pose_y_m: Option<f64>,
    pub last_pose_heading_rad: Option<f64>,
    pub target_speed_mps: Option<f64>,
    // --- join ---
    /// Reference into the bag for the heavy frames — NOT the frames themselves.
    pub bulk_ref: String,
}

/// Build a dataset row from a record + its bus match. The `doer_version` and
/// `bulk_ref` come from the bus side [D3]; the rest is the record's own summary.
#[must_use]
pub fn build_row(rec: &CaptureRecord, m: &BusMatch) -> DatasetRow {
    let p = rec.proposed.as_ref();
    let t = rec.traj.as_ref();
    DatasetRow {
        decision_seq: rec.decision_seq,
        source: source_token(rec.source).to_string(),
        t_wall_ms: rec.t_wall_ms,
        t_mono_ns: rec.t_mono_ns.to_string(),
        doer_version: m.doer_version.clone(),
        outcome: outcome_token(rec.outcome).to_string(),
        deny_code: rec.deny_code.clone(),
        safe_value: rec.safe_value,
        mrc: rec.mrc,
        posture: rec.posture.clone(),
        derate_enabled: rec.derate_enabled,
        proposed_linear_velocity_mps: p.map(|x| x.linear_velocity_mps),
        proposed_current_velocity_mps: p.map(|x| x.current_velocity_mps),
        proposed_steering_angle_deg: p.map(|x| x.steering_angle_deg),
        proposed_current_steering_angle_deg: p.map(|x| x.current_steering_angle_deg),
        proposed_delta_time_s: p.map(|x| x.delta_time_s),
        asset_id: t.map(|x| x.asset_id.clone()),
        trajectory_id: t.map(|x| x.trajectory_id),
        objects_ms: t.map(|x| x.objects_ms),
        point_count: t.map(|x| x.point_count as u64),
        object_count: t.map(|x| x.object_count as u64),
        first_pose_x_m: t.and_then(|x| x.first_pose.map(|p| p.x_m)),
        first_pose_y_m: t.and_then(|x| x.first_pose.map(|p| p.y_m)),
        first_pose_heading_rad: t.and_then(|x| x.first_pose.map(|p| p.heading_rad)),
        last_pose_x_m: t.and_then(|x| x.last_pose.map(|p| p.x_m)),
        last_pose_y_m: t.and_then(|x| x.last_pose.map(|p| p.y_m)),
        last_pose_heading_rad: t.and_then(|x| x.last_pose.map(|p| p.heading_rad)),
        target_speed_mps: t.and_then(|x| x.target_speed_mps),
        bulk_ref: m.bulk_ref.clone(),
    }
}

/// The Arrow schema for the dataset. Field order MUST match `build_batch` below.
#[must_use]
pub fn dataset_schema() -> SchemaRef {
    let f = |name: &str, dt: DataType, nullable: bool| Field::new(name, dt, nullable);
    Arc::new(Schema::new(vec![
        f("decision_seq", DataType::UInt64, false),
        f("source", DataType::Utf8, false),
        f("t_wall_ms", DataType::UInt64, false),
        f("t_mono_ns", DataType::Utf8, false),
        f("doer_version", DataType::Utf8, false),
        f("outcome", DataType::Utf8, false),
        f("deny_code", DataType::Utf8, true),
        f("safe_value", DataType::Float64, true),
        f("mrc", DataType::Boolean, false),
        f("posture", DataType::Utf8, false),
        f("derate_enabled", DataType::Boolean, false),
        f("proposed_linear_velocity_mps", DataType::Float64, true),
        f("proposed_current_velocity_mps", DataType::Float64, true),
        f("proposed_steering_angle_deg", DataType::Float64, true),
        f("proposed_current_steering_angle_deg", DataType::Float64, true),
        f("proposed_delta_time_s", DataType::Float64, true),
        f("asset_id", DataType::Utf8, true),
        f("trajectory_id", DataType::UInt64, true),
        f("objects_ms", DataType::UInt64, true),
        f("point_count", DataType::UInt64, true),
        f("object_count", DataType::UInt64, true),
        f("first_pose_x_m", DataType::Float64, true),
        f("first_pose_y_m", DataType::Float64, true),
        f("first_pose_heading_rad", DataType::Float64, true),
        f("last_pose_x_m", DataType::Float64, true),
        f("last_pose_y_m", DataType::Float64, true),
        f("last_pose_heading_rad", DataType::Float64, true),
        f("target_speed_mps", DataType::Float64, true),
        f("bulk_ref", DataType::Utf8, false),
    ]))
}

fn build_batch(schema: &SchemaRef, rows: &[&DatasetRow]) -> Result<RecordBatch, CollectorError> {
    macro_rules! f64_opt {
        ($field:ident) => {
            Arc::new(Float64Array::from_iter(rows.iter().map(|r| r.$field))) as ArrayRef
        };
    }
    macro_rules! u64_opt {
        ($field:ident) => {
            Arc::new(UInt64Array::from_iter(rows.iter().map(|r| r.$field))) as ArrayRef
        };
    }
    macro_rules! str_opt {
        ($field:ident) => {
            Arc::new(StringArray::from_iter(rows.iter().map(|r| r.$field.clone()))) as ArrayRef
        };
    }
    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.decision_seq))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.source.as_str()))),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.t_wall_ms))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.t_mono_ns.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.doer_version.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.outcome.as_str()))),
        str_opt!(deny_code),
        f64_opt!(safe_value),
        Arc::new(BooleanArray::from_iter(rows.iter().map(|r| Some(r.mrc)))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.posture.as_str()))),
        Arc::new(BooleanArray::from_iter(rows.iter().map(|r| Some(r.derate_enabled)))),
        f64_opt!(proposed_linear_velocity_mps),
        f64_opt!(proposed_current_velocity_mps),
        f64_opt!(proposed_steering_angle_deg),
        f64_opt!(proposed_current_steering_angle_deg),
        f64_opt!(proposed_delta_time_s),
        str_opt!(asset_id),
        u64_opt!(trajectory_id),
        u64_opt!(objects_ms),
        u64_opt!(point_count),
        u64_opt!(object_count),
        f64_opt!(first_pose_x_m),
        f64_opt!(first_pose_y_m),
        f64_opt!(first_pose_heading_rad),
        f64_opt!(last_pose_x_m),
        f64_opt!(last_pose_y_m),
        f64_opt!(last_pose_heading_rad),
        f64_opt!(target_speed_mps),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.bulk_ref.as_str()))),
    ];
    RecordBatch::try_new(Arc::clone(schema), columns).map_err(CollectorError::Arrow)
}

/// Read back a written Parquet part's column names + total row count. Used by
/// tests and (Phase 2) join-quality metrics — lets callers verify a dataset
/// without taking a direct arrow/parquet dependency.
pub fn read_part_columns_and_rows(path: &Path) -> Result<(Vec<String>, usize), CollectorError> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let file = fs::File::open(path).map_err(CollectorError::Io)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(CollectorError::Parquet)?;
    let columns: Vec<String> = builder.schema().fields().iter().map(|f| f.name().clone()).collect();
    let reader = builder.build().map_err(CollectorError::Parquet)?;
    let mut rows = 0usize;
    for batch in reader {
        rows += batch.map_err(CollectorError::Arrow)?.num_rows();
    }
    Ok((columns, rows))
}

/// Replace any path-unsafe characters in a partition value with `_`.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect()
}

/// Write the rows as Parquet, partitioned `doer_version=<v>/source=<s>/`. Returns
/// one `PartitionInfo` per partition written (deterministic order). Heavy frames
/// are never materialized — only the summary columns + `bulk_ref` are.
///
/// Rows within each partition are sorted by `(source, decision_seq)` so the
/// output is reproducible regardless of the caller's input order — the basis for
/// the manifest's stable `content_digest` [M2].
pub fn write_dataset(rows: &[DatasetRow], out_dir: &Path) -> Result<Vec<PartitionInfo>, CollectorError> {
    let schema = dataset_schema();
    // Group by (doer_version, source); BTreeMap → deterministic partition order.
    let mut groups: BTreeMap<(String, String), Vec<&DatasetRow>> = BTreeMap::new();
    for r in rows {
        groups.entry((r.doer_version.clone(), r.source.clone())).or_default().push(r);
    }
    let mut written = Vec::new();
    for ((doer_version, source), mut group) in groups {
        group.sort_by(|a, b| (a.source.as_str(), a.decision_seq).cmp(&(b.source.as_str(), b.decision_seq)));
        let rel = format!(
            "doer_version={}/source={}/part-000.parquet",
            sanitize(&doer_version),
            sanitize(&source)
        );
        let path = out_dir.join(&rel);
        fs::create_dir_all(path.parent().expect("partition path has a parent"))
            .map_err(CollectorError::Io)?;
        let batch = build_batch(&schema, &group)?;
        let file = fs::File::create(&path).map_err(CollectorError::Io)?;
        // UNCOMPRESSED keeps the output codec-independent + byte-deterministic;
        // compression is a later tuning knob.
        let props = WriterProperties::builder()
            .set_compression(Compression::UNCOMPRESSED)
            .build();
        let mut writer =
            ArrowWriter::try_new(file, Arc::clone(&schema), Some(props)).map_err(CollectorError::Parquet)?;
        writer.write(&batch).map_err(CollectorError::Parquet)?;
        writer.close().map_err(CollectorError::Parquet)?;
        written.push(PartitionInfo {
            doer_version,
            source,
            row_count: group.len(),
            relative_path: rel,
        });
    }
    Ok(written)
}
