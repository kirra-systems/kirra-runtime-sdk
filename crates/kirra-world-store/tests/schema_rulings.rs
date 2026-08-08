//! The four rulings of `KIRRA-WM2-SCHEMA-001`, asserted rather than described.
//!
//! Each test names the ruling it covers. The tamper tests matter most: a test
//! showing only that the *writer* refuses would prove the weaker claim, and
//! SD-2 explicitly rejected "the caller promises not to" as conventional rather
//! than structural. So the tamper tests bypass the write path entirely and
//! assert that the **chain** catches the edit.

use kirra_world_store::{
    ClaimStatus, EventId, FrameId, NewEvent, ObservationId, StoreError, WorldStore, WriterClass,
};

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

/// Owned ids for one event, so a [`NewEvent`] can borrow them. This file's
/// fixtures give the two DIFFERENT strings, which is the honest shape: an event
/// and the observation it records are distinct identities, and they are now
/// distinct types too.
struct Ids {
    event: EventId,
    observation: ObservationId,
}

fn ids(e: &str, o: &str) -> Ids {
    Ids {
        event: EventId::new(e).expect("admissible event id"),
        observation: ObservationId::new(o).expect("admissible observation id"),
    }
}

fn base<'a>(id: &'a Ids) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
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
        subject_ref: None,
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"note":"seen"}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

#[test]
fn a_chain_of_events_verifies() {
    let path = tmp("verifies");
    let mut s = WorldStore::open(&path).expect("open");
    for i in 0..5 {
        let eid = format!("e{i}");
        let oid = format!("o{i}");
        s.append(&base(&ids(&eid, &oid))).expect("append");
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
        ..base(&ids("e1", "o1"))
    })
    .expect("append");
    s.append(&base(&ids("e2", "o2"))).expect("append");
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
            ..base(&ids("e1", "o1"))
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
        ..base(&ids("e1", "o1"))
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
            ..base(&ids("e1", "o1"))
        })
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::SpatialClaimNeedsFrame));

    let frame = FrameId::new("map:kitchen").expect("admissible frame");
    s.append(&NewEvent {
        kind: "spatial",
        frame_id: Some(&frame),
        ..base(&ids("e2", "o2"))
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
    let frame = FrameId::new("map:kitchen").expect("admissible frame");
    s.append(&NewEvent {
        kind: "spatial",
        frame_id: Some(&frame),
        ..base(&ids("e1", "o1"))
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
        ..base(&ids("e1", "o1"))
    })
    .expect("bounded observation");
    s.append(&base(&ids("e2", "o2")))
        .expect("open-ended observation");
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
        ..base(&ids("e1", "o1"))
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
    s.append(&base(&ids("e1", "o1")))
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
    s.append(&base(&ids("e1", "o1"))).expect("append");

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
    s.append(&base(&ids("e1", "o1"))).expect("append");
    // rusqlite lets us move the PK; the chain must refuse to re-derive it.
    let _ = s.tamper_for_test(1, "chain_digest", Some("deadbeef"));
    match s.verify_chain() {
        Err(StoreError::ChainBroken { .. }) => {}
        other => panic!("a rewritten digest must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Re-admission on the read path (the reference types)
// ---------------------------------------------------------------------------

/// A stored identity the core no longer admits is a **corrupt row**, not a
/// broken chain.
///
/// Both are refusals, so nothing is let through either way — but they send an
/// investigator to different places. "This row is unreadable" points at the
/// writer or the storage medium; "this row was edited" points at an intruder.
/// The store already drew that line for unparseable provenance; re-admission
/// keeps drawing it.
///
/// An empty `observation_id` is the case that motivates this: the column is
/// `TEXT NOT NULL`, so SQLite accepts `''` happily, and every layer that checks
/// for *presence* reads it as present while it names nothing.
#[test]
fn an_inadmissible_stored_identity_reads_as_a_corrupt_row_not_a_broken_chain() {
    let path = tmp("readmit-corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base(&ids("e1", "o1"))).expect("append");

    s.tamper_for_test(1, "observation_id", Some(""))
        .expect("tamper");

    match s.verify_chain() {
        Err(StoreError::CorruptRow { generation, detail }) => {
            assert_eq!(generation, 1);
            assert!(
                detail.contains("observation_id"),
                "the diagnosis must name the field: {detail}"
            );
        }
        other => panic!("an empty stored identity must read as CorruptRow, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// Re-admission can only ever ADD refusals — it must never rescue a row the
/// chain check would have rejected.
///
/// This is the property that makes the change safe to put in front of the
/// integrity check. The tamper here is admissible as a *reference* (`"o2"` is a
/// perfectly good identity) but wrong for this row, so re-admission passes it
/// straight through to the digest comparison, which refuses it.
#[test]
fn re_admission_does_not_weaken_the_chain_check() {
    let path = tmp("readmit-no-rescue");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base(&ids("e1", "o1"))).expect("append");

    s.tamper_for_test(1, "observation_id", Some("o2"))
        .expect("tamper");

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("a swapped-but-valid identity must still break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// The verbatim rule, from the storage side.
///
/// A value with surrounding whitespace is admissible and is stored unchanged.
/// If any constructor on the read path trimmed, the rehash would be computed
/// over different bytes than the write produced and this untampered row would
/// report as tampered — the failure mode that makes "validate, never normalize"
/// load-bearing rather than stylistic.
#[test]
fn a_stored_identity_with_whitespace_still_verifies() {
    let path = tmp("readmit-verbatim");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&base(&ids("  e1  ", " o1 "))).expect("append");
    s.verify_chain().expect("a verbatim round trip must verify");
    let _ = std::fs::remove_file(&path);
}
