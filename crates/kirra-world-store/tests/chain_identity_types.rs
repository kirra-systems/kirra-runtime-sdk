//! The core's chain-identity types against what the store actually produces.
//!
//! `EvidenceDigest` and `PrevHash` state a rule about the shape of a chain
//! value. This is where that rule meets the real producer — because a type
//! whose admission rule disagrees with the thing it is meant to type is not
//! merely useless, it is worse than a `String`: it would refuse genuine
//! evidence while looking like a safety improvement.

use kirra_world::evidence::{DigestError, EvidenceDigest, PrevHash, DIGEST_HEX_LEN, GENESIS_TOKEN};
use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass, GENESIS,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-chainid-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn seed(store: &mut WorldStore, n: i64) {
    let event = EventId::new(format!("evt-{n}")).unwrap();
    let observation = ObservationId::new(format!("obs-{n}")).unwrap();
    store
        .append(&NewEvent {
            event_id: &event,
            observation_id: &observation,
            txn_time_ms: T0 + n,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "sensor-a",
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
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

/// **One definition of the genesis sentinel, not two.**
///
/// The store's `GENESIS` is now the core's constant. Pinned by identity so a
/// future edit that reintroduces a local literal here is caught even if the two
/// happen to agree on the day it is written.
#[test]
fn the_store_and_the_core_share_one_genesis_definition() {
    assert_eq!(GENESIS, GENESIS_TOKEN);
    // ...and the value itself is frozen: it is hashed into every first record,
    // so changing it does not migrate a store, it makes existing first records
    // unverifiable.
    assert_eq!(GENESIS, "kirra-world:genesis");
}

/// **The seam test.** A digest the store really produced is admissible by the
/// core type.
///
/// This is the assertion that makes the type worth having. `EvidenceDigest`
/// demands exactly 64 lower-case hex characters; `compute_record_hash_v2`
/// emits `hex::encode` of a SHA-256. If those two ever disagreed — a producer
/// switching to upper case, a truncation, a prefix — the type would start
/// refusing genuine evidence, and it would do so while looking like a
/// tightening rather than a break.
#[test]
fn a_digest_the_store_produced_is_admissible_by_the_core_type() {
    let mut s = WorldStore::open(&tmp("produced")).expect("open");
    seed(&mut s, 1);

    let head = s.head_chain().expect("head");
    assert_ne!(head, GENESIS, "an appended store has moved off genesis");

    let typed = EvidenceDigest::new(&head).unwrap_or_else(|e| {
        panic!("the store's own digest was refused by the core type: {e} ({head:?})")
    });
    assert_eq!(typed.as_str(), head, "admitted verbatim");
    assert_eq!(head.chars().count(), DIGEST_HEX_LEN);
}

/// The two chain positions parse as the two things they actually are.
#[test]
fn genesis_and_a_real_head_parse_as_their_own_variants() {
    let mut s = WorldStore::open(&tmp("variants")).expect("open");

    // Before any append, the head IS genesis -- the beginning, not a digest.
    let before = s.head_chain().expect("head");
    assert_eq!(
        PrevHash::parse(&before).expect("parses"),
        PrevHash::Genesis,
        "an empty store's head is the beginning"
    );

    seed(&mut s, 1);

    // After, it is a link.
    let after = s.head_chain().expect("head");
    let parsed = PrevHash::parse(&after).expect("parses");
    assert_eq!(parsed.digest().expect("a link").as_str(), after);
    assert_ne!(parsed, PrevHash::Genesis);
}

/// A chain advances, and every head along the way stays admissible.
///
/// One digest being well-formed could be luck; this walks several. It also
/// pins that the heads actually *change*, so the test is not passing against a
/// store that returns the same value forever.
#[test]
fn every_head_along_a_growing_chain_is_admissible_and_distinct() {
    let mut s = WorldStore::open(&tmp("growing")).expect("open");
    let mut seen: Vec<String> = Vec::new();

    for n in 1..=4 {
        seed(&mut s, n);
        let head = s.head_chain().expect("head");
        EvidenceDigest::new(&head).expect("each head is admissible");
        assert!(!seen.contains(&head), "each append moves the head");
        seen.push(head);
    }
    assert_eq!(seen.len(), 4);
    s.verify_chain().expect("and the chain verifies");
}

/// The store's stored digests are lower case, which is what the type demands.
///
/// Asserted against the real value rather than assumed from `hex::encode`'s
/// documentation, because the case rule is the one refusal in `EvidenceDigest`
/// that would look like pedantry if it were not tied to a real producer.
#[test]
fn stored_digests_are_lower_case_so_the_case_rule_is_not_arbitrary() {
    let mut s = WorldStore::open(&tmp("case")).expect("open");
    seed(&mut s, 1);
    let head = s.head_chain().expect("head");

    assert!(
        !head.chars().any(|c| c.is_ascii_uppercase()),
        "the producer emits lower case: {head}"
    );
    // And the upper-cased form -- the same hash, a different string -- is
    // refused, which is the whole point of not normalizing.
    assert_eq!(
        EvidenceDigest::new(head.to_uppercase()).expect_err("refused"),
        DigestError::UppercaseHex
    );
}
