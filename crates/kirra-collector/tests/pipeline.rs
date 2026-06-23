// crates/kirra-collector/tests/pipeline.rs
//
// End-to-end pipeline tests against SYNTHETIC fixtures: hand-built capture
// records for both sources + an in-memory bag. No real rosbag needed (C2/C4
// deferred until the GPU bench is up).
//
// Covers: join hit/orphan counts; stratified sampling; Parquet partitioning;
// bulk_ref present + heavy frames absent; the orphan-rate gate; and (Phase 2)
// the dataset manifest — reproducible dataset_id, lineage, and quality.

use std::path::{Path, PathBuf};

use kirra_capture_schema::{
    CaptureOutcome, CaptureRecord, CaptureSource, PoseSnapshot, ProposedCommandSnapshot,
    TrajectoryCaptureExt,
};
use kirra_collector::bag::{BusMessage, InMemoryBag};
use kirra_collector::dataset::read_part_columns_and_rows;
use kirra_collector::{
    list_parquet_parts, read_jsonl_with_digests, run, CollectorConfig, CollectorError, Lineage,
};

// ---- fixtures --------------------------------------------------------------

fn gateway(seq: u64, t_wall_ms: u64, outcome: CaptureOutcome) -> CaptureRecord {
    CaptureRecord {
        decision_seq: seq,
        t_mono_ns: u128::from(seq) * 1000,
        t_wall_ms,
        source: CaptureSource::CommandGateway,
        proposed: Some(ProposedCommandSnapshot {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 40.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
            delta_time_s: 0.1,
        }),
        traj: None,
        outcome,
        deny_code: matches!(outcome, CaptureOutcome::Deny).then(|| "NAN_INF_LINEAR_VELOCITY".to_string()),
        safe_value: matches!(outcome, CaptureOutcome::ClampLinear).then_some(35.0),
        mrc: false,
        posture: "NOMINAL".to_string(),
        derate_enabled: false,
    }
}

fn trajectory(seq: u64, t_wall_ms: u64, traj_id: u64, outcome: CaptureOutcome) -> CaptureRecord {
    CaptureRecord {
        decision_seq: seq,
        t_mono_ns: u128::from(seq) * 1000,
        t_wall_ms,
        source: CaptureSource::SlowLoopTrajectory,
        proposed: None,
        traj: Some(TrajectoryCaptureExt {
            asset_id: "ego".to_string(),
            trajectory_id: traj_id,
            objects_ms: 500,
            point_count: 12,
            object_count: 3,
            first_pose: Some(PoseSnapshot { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 }),
            last_pose: Some(PoseSnapshot { x_m: 5.0, y_m: 1.0, heading_rad: 0.1 }),
            target_speed_mps: Some(8.0),
        }),
        outcome,
        deny_code: matches!(outcome, CaptureOutcome::Deny).then(|| "TRAJECTORY_MRC_FALLBACK".to_string()),
        safe_value: None,
        mrc: matches!(outcome, CaptureOutcome::Deny),
        posture: "NOMINAL".to_string(),
        derate_enabled: false,
    }
}

fn bus_msg(t_wall_ms: u64, ver: &str, traj_id: Option<u64>, reff: &str) -> BusMessage {
    BusMessage {
        t_wall_ms,
        doer_version: ver.to_string(),
        asset_id: traj_id.map(|_| "ego".to_string()),
        trajectory_id: traj_id,
        objects_ms: traj_id.map(|_| 500),
        bulk_ref: reff.to_string(),
    }
}

fn lineage() -> Lineage {
    Lineage { inputs: vec![], bag_backend: "test".to_string() }
}

fn unique_out(tag: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("kirra-collector-test-{tag}-{nanos}-{n}"))
}

fn write_jsonl(path: &Path, recs: &[CaptureRecord]) {
    let body: String =
        recs.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
    std::fs::write(path, body).unwrap();
}

/// The dataset's full column set — pinning this proves NO raw frame/point/object
/// payload column exists (only counts + endpoint poses + bulk_ref).
const EXPECTED_COLUMNS: &[&str] = &[
    "decision_seq", "source", "t_wall_ms", "t_mono_ns", "doer_version", "outcome", "deny_code",
    "safe_value", "mrc", "posture", "derate_enabled", "proposed_linear_velocity_mps",
    "proposed_current_velocity_mps", "proposed_steering_angle_deg",
    "proposed_current_steering_angle_deg", "proposed_delta_time_s", "asset_id", "trajectory_id",
    "objects_ms", "point_count", "object_count", "first_pose_x_m", "first_pose_y_m",
    "first_pose_heading_rad", "last_pose_x_m", "last_pose_y_m", "last_pose_heading_rad",
    "target_speed_mps", "bulk_ref",
];

// ---- pipeline tests --------------------------------------------------------

#[test]
fn happy_path_joins_both_sources_and_partitions() {
    let out = unique_out("happy");
    let records = vec![
        gateway(0, 1000, CaptureOutcome::Allow),
        gateway(1, 1100, CaptureOutcome::ClampLinear),
        trajectory(0, 2000, 42, CaptureOutcome::Allow),
        trajectory(1, 2100, 43, CaptureOutcome::Deny),
    ];
    let bag = InMemoryBag::new(
        "synthetic",
        vec![
            bus_msg(1005, "model_v1", None, "bag#gw0"),
            bus_msg(1105, "model_v1", None, "bag#gw1"),
            bus_msg(2005, "model_v1", Some(42), "bag#tj0"),
            bus_msg(2105, "model_v1", Some(43), "bag#tj1"),
        ],
    );
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 0.0, out_dir: out.clone() };

    let m = run(records, &bag, &lineage(), &cfg).expect("run should succeed");
    let recon = &m.quality;
    assert_eq!(recon.records_in, 4);
    assert_eq!(recon.records_in_command_gateway, 2);
    assert_eq!(recon.records_in_slow_loop_trajectory, 2);
    assert_eq!(recon.interventions_in, 2, "clamp + deny");
    assert_eq!(recon.passes_in, 2, "allow + accept");
    assert_eq!(recon.kept_after_sampling, 4, "pass_rate 1.0 keeps all");
    assert_eq!(recon.joined, 4);
    assert_eq!(recon.orphans, 0);
    assert_eq!(recon.orphan_rate, 0.0);

    // INV-3: manifest is ADDED at the dataset root; partitions unchanged.
    assert!(out.join("manifest.json").exists(), "manifest.json at dataset root");
    let gw_part = out.join("doer_version=model_v1/source=COMMAND_GATEWAY/part-000.parquet");
    let tj_part = out.join("doer_version=model_v1/source=SLOW_LOOP_TRAJECTORY/part-000.parquet");
    assert!(gw_part.exists(), "gateway partition must exist at {gw_part:?}");
    assert!(tj_part.exists(), "trajectory partition must exist at {tj_part:?}");

    let (cols, rows) = read_part_columns_and_rows(&gw_part).unwrap();
    assert_eq!(rows, 2, "two gateway rows");
    assert_eq!(cols, EXPECTED_COLUMNS, "schema must be exactly the summary columns");
    for forbidden in ["points", "objects", "object_list", "frame", "frames", "trajectory_points"] {
        assert!(!cols.iter().any(|c| c == forbidden), "no raw `{forbidden}` column");
    }
    let (_c, tj_rows) = read_part_columns_and_rows(&tj_part).unwrap();
    assert_eq!(tj_rows, 2, "two trajectory rows");
    cleanup(&out);
}

#[test]
fn stratified_sampling_keeps_interventions_drops_passes_at_zero_rate() {
    let out = unique_out("sample0");
    let records = vec![
        gateway(0, 1000, CaptureOutcome::Allow),
        gateway(1, 1100, CaptureOutcome::ClampLinear),
        trajectory(0, 2000, 42, CaptureOutcome::Allow),
        trajectory(1, 2100, 43, CaptureOutcome::Deny),
    ];
    let bag = InMemoryBag::new(
        "synthetic",
        vec![
            bus_msg(1105, "model_v1", None, "bag#gw1"),
            bus_msg(2105, "model_v1", Some(43), "bag#tj1"),
        ],
    );
    let cfg = CollectorConfig { pass_rate: 0.0, window_ms: 100, max_orphan_rate: 0.0, out_dir: out.clone() };

    let m = run(records, &bag, &lineage(), &cfg).expect("run should succeed");
    let recon = &m.quality;
    assert_eq!(recon.passes_in, 2);
    assert_eq!(recon.interventions_in, 2);
    assert_eq!(recon.passes_kept, 0, "pass_rate 0.0 drops every pass");
    assert_eq!(recon.interventions_kept, 2, "every intervention survives");
    assert_eq!(recon.kept_after_sampling, 2);
    assert_eq!(recon.joined, 2);
    assert_eq!(recon.orphans, 0);
    cleanup(&out);
}

#[test]
fn orphan_rate_gate_fails_loud_when_exceeded() {
    let out = unique_out("orphan");
    let records = vec![gateway(0, 1000, CaptureOutcome::ClampLinear)];
    let bag = InMemoryBag::new("synthetic", vec![bus_msg(99_000, "model_v1", None, "far")]);
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 0.0, out_dir: out.clone() };

    match run(records, &bag, &lineage(), &cfg) {
        Err(CollectorError::OrphanRateExceeded { manifest, max }) => {
            assert_eq!(manifest.quality.orphans, 1);
            assert_eq!(manifest.quality.joined, 0);
            assert_eq!(manifest.quality.orphan_rate, 1.0);
            assert_eq!(max, 0.0);
            // The flagged dataset is still identifiable: a manifest was written.
            assert!(out.join("manifest.json").exists());
            assert!(!manifest.dataset_id.is_empty());
        }
        other => panic!("expected OrphanRateExceeded, got {other:?}"),
    }
    cleanup(&out);
}

#[test]
fn orphan_under_ceiling_succeeds() {
    let out = unique_out("orphan_ok");
    let records = vec![
        gateway(0, 1000, CaptureOutcome::ClampLinear),
        gateway(1, 5000, CaptureOutcome::Deny),
    ];
    let bag = InMemoryBag::new("synthetic", vec![bus_msg(1005, "model_v1", None, "bag#gw0")]);
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 0.75, out_dir: out.clone() };

    let m = run(records, &bag, &lineage(), &cfg).expect("under ceiling → ok");
    assert_eq!(m.quality.joined, 1);
    assert_eq!(m.quality.orphans, 1);
    assert_eq!(m.quality.orphan_rate, 0.5);
    cleanup(&out);
}

#[test]
fn distinct_doer_versions_produce_distinct_partitions() {
    let out = unique_out("multiver");
    let records = vec![
        gateway(0, 1000, CaptureOutcome::ClampLinear),
        gateway(1, 2000, CaptureOutcome::ClampLinear),
    ];
    let bag = InMemoryBag::new(
        "synthetic",
        vec![
            bus_msg(1005, "model_v1", None, "bag#a"),
            bus_msg(2005, "model_v2", None, "bag#b"),
        ],
    );
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 0.0, out_dir: out.clone() };

    let m = run(records, &bag, &lineage(), &cfg).expect("run ok");
    assert_eq!(m.quality.joined, 2);
    assert_eq!(m.doer_versions, vec!["model_v1".to_string(), "model_v2".to_string()]);
    let parts = list_parquet_parts(&out).unwrap();
    assert_eq!(parts.len(), 2, "one partition per doer_version");
    cleanup(&out);
}

#[test]
fn read_jsonl_and_dedup_round_trips_through_the_pipeline() {
    let out = unique_out("jsonl");
    let dir = unique_out("jsonl_in");
    std::fs::create_dir_all(&dir).unwrap();
    let capture_path = dir.join("capture.jsonl");

    let r0 = gateway(0, 1000, CaptureOutcome::ClampLinear);
    let dup = gateway(0, 1000, CaptureOutcome::Allow); // same key → dropped
    let r1 = trajectory(0, 2000, 42, CaptureOutcome::Deny);
    write_jsonl(&capture_path, &[r0, dup, r1]);

    let (records, inputs) = read_jsonl_with_digests(&[capture_path]).unwrap();
    assert_eq!(records.len(), 3, "reads every non-blank line incl. the dup");
    assert_eq!(inputs[0].record_count, 3);
    assert_eq!(inputs[0].command_gateway, 2);
    assert_eq!(inputs[0].slow_loop_trajectory, 1);

    let bag = InMemoryBag::new(
        "synthetic",
        vec![
            bus_msg(1005, "model_v1", None, "bag#gw0"),
            bus_msg(2005, "model_v1", Some(42), "bag#tj0"),
        ],
    );
    let lin = Lineage { inputs, bag_backend: "bag-json".to_string() };
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 0.0, out_dir: out.clone() };
    let m = run(records, &bag, &lin, &cfg).expect("run ok");
    assert_eq!(m.quality.duplicates_dropped, 1, "the duplicate (source, decision_seq) is dropped");
    assert_eq!(m.quality.records_in, 2, "two distinct records survive dedup");
    assert_eq!(m.quality.joined, 2);
    cleanup(&out);
    cleanup(&dir);
}

// ---- Phase 2: manifest + lineage + reproducibility -------------------------

/// Run the same capture file + bag twice → identical `dataset_id`; a perturbed
/// input or a changed param → a different id. (`created_at` is excluded from the
/// id by construction — `compute_dataset_id` takes no such argument; see the
/// unit test in manifest.rs.)
#[test]
fn dataset_id_is_reproducible_and_input_sensitive() {
    let dir = unique_out("detin");
    std::fs::create_dir_all(&dir).unwrap();
    let cap = dir.join("cap.jsonl");
    write_jsonl(&cap, &[gateway(0, 1000, CaptureOutcome::ClampLinear), trajectory(0, 2000, 42, CaptureOutcome::Deny)]);
    let bag = InMemoryBag::new(
        "synthetic",
        vec![bus_msg(1005, "model_v1", None, "a"), bus_msg(2005, "model_v1", Some(42), "b")],
    );

    let run_id = |cap_path: &Path, pass_rate: f64| -> String {
        let (records, inputs) = read_jsonl_with_digests(&[cap_path.to_path_buf()]).unwrap();
        let lin = Lineage { inputs, bag_backend: "bag-json".to_string() };
        let cfg = CollectorConfig { pass_rate, window_ms: 100, max_orphan_rate: 1.0, out_dir: unique_out("idrun") };
        let m = run(records, &bag, &lin, &cfg).expect("run ok");
        cleanup(&cfg.out_dir);
        m.dataset_id
    };

    let id1 = run_id(&cap, 1.0);
    let id2 = run_id(&cap, 1.0);
    assert_eq!(id1, id2, "same inputs + params + build → identical dataset_id");

    let id_rate = run_id(&cap, 0.5);
    assert_ne!(id1, id_rate, "different pass_rate → different dataset_id");

    // Perturb one input record (different decision_seq → different bytes + content).
    let cap2 = dir.join("cap2.jsonl");
    write_jsonl(&cap2, &[gateway(0, 1000, CaptureOutcome::ClampLinear), trajectory(9, 2000, 42, CaptureOutcome::Deny)]);
    let id_perturbed = run_id(&cap2, 1.0);
    assert_ne!(id1, id_perturbed, "a perturbed input record → different dataset_id");

    cleanup(&dir);
}

/// The manifest records full lineage + the run's quality verbatim, and is written
/// as valid JSON at the dataset root.
#[test]
fn manifest_records_lineage_quality_and_partitions() {
    let dir = unique_out("manin");
    std::fs::create_dir_all(&dir).unwrap();
    let cap = dir.join("cap.jsonl");
    write_jsonl(
        &cap,
        &[
            gateway(0, 1000, CaptureOutcome::Allow),
            gateway(1, 1100, CaptureOutcome::ClampLinear),
            trajectory(0, 2000, 42, CaptureOutcome::Deny),
        ],
    );
    let (records, inputs) = read_jsonl_with_digests(std::slice::from_ref(&cap)).unwrap();
    let lin = Lineage { inputs, bag_backend: "bag-json".to_string() };
    let out = unique_out("manout");
    let cfg = CollectorConfig { pass_rate: 1.0, window_ms: 100, max_orphan_rate: 1.0, out_dir: out.clone() };
    let bag = InMemoryBag::new(
        "synthetic.json",
        vec![
            bus_msg(1005, "v1", None, "a"),
            bus_msg(1105, "v1", None, "b"),
            bus_msg(2005, "v1", Some(42), "c"),
        ],
    );

    let m = run(records, &bag, &lin, &cfg).expect("run ok");

    // Inputs lineage.
    assert_eq!(m.inputs.len(), 1);
    assert_eq!(m.inputs[0].record_count, 3);
    assert_eq!(m.inputs[0].command_gateway, 2);
    assert_eq!(m.inputs[0].slow_loop_trajectory, 1);
    assert_eq!(m.inputs[0].sha256.len(), 64, "sha256 hex is 64 chars");

    // Quality == the run reconciliation.
    assert_eq!(m.quality.records_in, 3);
    assert_eq!(m.quality.joined, 3);
    assert_eq!(m.quality.orphans, 0);

    // Partitions (gateway + trajectory, single doer_version) with row counts.
    assert_eq!(m.partitions.len(), 2);
    assert_eq!(m.partitions.iter().map(|p| p.row_count).sum::<usize>(), 3);
    assert!(m.partitions.iter().all(|p| p.relative_path.ends_with("part-000.parquet")));

    // Provenance present.
    assert!(!m.collector.version.is_empty());
    assert!(!m.schema.version.is_empty());
    assert_eq!(m.schema.crate_name, "kirra-capture-schema");
    assert_eq!(m.bag.backend, "bag-json");
    assert!(m.bag.uri.contains("synthetic"));
    assert_eq!(m.doer_versions, vec!["v1".to_string()]);
    assert_eq!(m.decision_t_wall_ms_min, Some(1000));
    assert_eq!(m.decision_t_wall_ms_max, Some(2000));

    // manifest.json written + valid JSON carrying the same id.
    let mf = out.join("manifest.json");
    assert!(mf.exists());
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&mf).unwrap()).unwrap();
    assert_eq!(v["dataset_id"].as_str().unwrap(), m.dataset_id);
    assert_eq!(v["quality"]["joined"].as_u64().unwrap(), 3);

    cleanup(&out);
    cleanup(&dir);
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}
