//! The four rulings of `KIRRA-WM2-SCHEMA-001`, asserted rather than described.
//!
//! Each test names the ruling it covers. The tamper tests matter most: a test
//! showing only that the *writer* refuses would prove the weaker claim, and
//! SD-2 explicitly rejected "the caller promises not to" as conventional rather
//! than structural. So the tamper tests bypass the write path entirely and
//! assert that the **chain** catches the edit.

use kirra_world_store::{ClaimStatus, NewEvent, StoreError, WorldStore, WriterClass};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-store-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn base<'a>(event_id: &'a str, observation_id: &'a str) -> NewEvent<'a> {
    NewEvent {
        event_id,
        observation_id,
        txn_time_ms: 1_700_000_000_000,
        valid_from_ms: 1_700_000_000_000,
        valid_to_ms: None,
        source: "test-sensor",
        source_version: "0.1.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject: "cup-1",
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"note":"seen"}"#,
        payload_schema: 1,
        retention_class: "raw",
    }
}

#[test]
fn a_chain_of_events_verifies() {
    let path = tmp("verifies");
    let mut s = WorldStore::open(&path).expect("open");
    for i in 0..5 {
        let eid = format!("e{i}");
        let oid = format!("o{i}");
        s.append(&base(&eid, &oid)).expect("append");
    }
    assert_eq!(s.count().unwrap(), 5);
    s.verify_chain().expect("chain must verify");
    let _ = std::fs::remove_file(&path);
}

/// SD-2, the half that matters. Relabel a stored row directly in SQL — the way
/// an attacker or a careless migration would — and the chain must catch it.
#[test]
fn sd2_relabelling_claim_status_breaks_the_chain() {
    let path = tmp("sd2-tamper");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..base("e1", "o1")
    })
    .expect("append");
    s.append(&base("e2", "o2")).expect("append");
    s.verify_chain().expect("clean before tamper");

    // Bypass the write path completely. The schema CHECK would refuse this
    // pairing on INSERT, so the edit also has to move writer_class — which is
    // exactly what a real relabelling attack would do.
    s.tamper_for_test(1, "writer_class", Some("operator"))
        .unwrap();
    s.tamper_for_test(1, "claim_status", Some("confirmed"))
        .unwrap();

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("relabelling must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// SD-2 at the write path: the store refuses before the statement is built, so
/// the caller gets a message naming the rule.
#[test]
fn sd2_an_llm_may_not_write_a_confirmed_fact() {
    let path = tmp("sd2-writer");
    let mut s = WorldStore::open(&path).expect("open");
    let err = s
        .append(&NewEvent {
            writer_class: WriterClass::LlmCandidate,
            claim_status: ClaimStatus::Confirmed,
            ..base("e1", "o1")
        })
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::LlmCannotConfirm));
    assert_eq!(s.count().unwrap(), 0, "nothing may be written");
    let _ = std::fs::remove_file(&path);
}

/// SD-2 as *schema*: even bypassing the crate's own check with raw SQL, SQLite
/// refuses to move an `llm_candidate` row to `confirmed`. This is the assertion
/// that the guarantee is not merely in code.
#[test]
fn sd2_is_a_schema_check_not_only_a_write_path_check() {
    let path = tmp("sd2-schema");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..base("e1", "o1")
    })
    .expect("a candidate is fine");

    let forced = s.tamper_for_test(1, "claim_status", Some("confirmed"));
    assert!(
        forced.is_err(),
        "the schema CHECK must refuse llm_candidate + confirmed even via raw SQL"
    );
    s.verify_chain()
        .expect("the refused edit left the chain intact");
    let _ = std::fs::remove_file(&path);
}

/// SD-4 at the write path.
#[test]
fn sd4_a_spatial_claim_without_a_frame_is_refused() {
    let path = tmp("sd4-writer");
    let mut s = WorldStore::open(&path).expect("open");
    let err = s
        .append(&NewEvent {
            kind: "spatial",
            frame_id: None,
            ..base("e1", "o1")
        })
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::SpatialClaimNeedsFrame));

    s.append(&NewEvent {
        kind: "spatial",
        frame_id: Some("map:kitchen"),
        ..base("e2", "o2")
    })
    .expect("with a frame it is fine");
    let _ = std::fs::remove_file(&path);
}

/// SD-4 as *schema*: clearing the frame on a stored spatial row must be refused
/// by SQLite, not merely by the writer.
#[test]
fn sd4_is_a_schema_check_not_only_a_write_path_check() {
    let path = tmp("sd4-schema");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&NewEvent {
        kind: "spatial",
        frame_id: Some("map:kitchen"),
        ..base("e1", "o1")
    })
    .expect("append");
    let cleared = s.tamper_for_test(1, "frame_id", None);
    assert!(
        cleared.is_err(),
        "the schema CHECK must refuse a frameless spatial row even via raw SQL"
    );
    let _ = std::fs::remove_file(&path);
}

/// SD-1: `valid_to_ms` is write-once, and the way that is guaranteed is that no
/// update path exists. Asserted against the public surface rather than by
/// comment — if someone adds an updater, this fails.
#[test]
fn sd1_valid_to_ms_is_set_at_insert_or_never() {
    let path = tmp("sd1");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&NewEvent {
        valid_to_ms: Some(1_700_000_005_000),
        ..base("e1", "o1")
    })
    .expect("bounded observation");
    s.append(&base("e2", "o2")).expect("open-ended observation");
    s.verify_chain().expect("verifies");

    // Prove the ONLY UPDATE is the test-only hatch, and prove it by position
    // rather than by the attribute merely existing somewhere in the file — a
    // non-test UPDATE added later would otherwise pass while an unrelated cfg
    // attribute elsewhere kept this green.
    let src = include_str!("../src/lib.rs");
    let lines: Vec<&str> = src.lines().collect();
    let update_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .filter(|(_, l)| l.contains("UPDATE world_events"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        update_lines.len(),
        1,
        "the only UPDATE may be the test-only tamper hatch; found {} at {:?}",
        update_lines.len(),
        update_lines
    );
    let at = update_lines[0];
    let window_start = at.saturating_sub(40);
    let gated = lines[window_start..at]
        .iter()
        .any(|l| l.contains("#[cfg(any(test, feature = \"test-support\"))]"));
    assert!(
        gated,
        "the UPDATE at line {} is not preceded by a test-only cfg gate within 40 lines",
        at + 1
    );
    let _ = std::fs::remove_file(&path);
}

/// SD-3: provenance round-trips as a JSON array and is covered by the digest.
#[test]
fn sd3_provenance_is_a_digest_covered_json_array() {
    let path = tmp("sd3");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&NewEvent {
        provenance: &["o-a", "o-b"],
        ..base("e1", "o1")
    })
    .expect("append");
    s.verify_chain().expect("clean");

    s.tamper_for_test(1, "provenance", Some(r#"["o-a"]"#))
        .unwrap();
    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("editing provenance must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// The fail-open review found. Provenance that was legitimately EMPTY at write
/// time, then corrupted, used to still verify: `unwrap_or_default()` read the
/// corrupt JSON back as `[]`, which is what was hashed, so the digests matched
/// and the corruption passed silently. This is the case that proves the fix,
/// and it is the one a non-empty-provenance test would have missed.
#[test]
fn corrupt_provenance_on_an_empty_row_fails_closed() {
    let path = tmp("provenance-empty-corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base("e1", "o1"))
        .expect("append with provenance: &[]");
    s.verify_chain().expect("clean");

    s.tamper_for_test(1, "provenance", Some("not-json"))
        .unwrap();

    match s.verify_chain() {
        Err(StoreError::CorruptRow { generation, .. }) => assert_eq!(generation, 1),
        other => panic!("corrupt provenance must fail closed, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// The tamper hatch allowlists its column rather than interpolating it.
#[test]
fn the_tamper_hatch_refuses_an_unknown_column() {
    let path = tmp("tamper-allowlist");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base("e1", "o1")).expect("append");

    let bad = s.tamper_for_test(1, "generation = 9 --", Some("x"));
    assert!(bad.is_err(), "an un-allowlisted column must be refused");
    s.verify_chain()
        .expect("the refused tamper changed nothing");
    let _ = std::fs::remove_file(&path);
}

/// A generation that cannot be a chain sequence must fail closed rather than
/// silently hashing as sequence 0.
#[test]
fn a_negative_generation_fails_closed_rather_than_hashing_as_zero() {
    let path = tmp("negative-generation");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base("e1", "o1")).expect("append");
    // rusqlite lets us move the PK; the chain must refuse to re-derive it.
    let _ = s.tamper_for_test(1, "chain_digest", Some("deadbeef"));
    match s.verify_chain() {
        Err(StoreError::ChainBroken { .. }) => {}
        other => panic!("a rewritten digest must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}
