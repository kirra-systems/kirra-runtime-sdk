//! **Tier 3 box 3c — a composed read cannot mix projection states.**
//!
//! The box asks that an answer composing several projections read them at ONE
//! coherent point, or report each coordinate explicitly. Two claims are pinned
//! here, and they are different claims:
//!
//! 1. **The snapshot is real.** A fold that commits after a snapshot was
//!    established is invisible to it — for BOTH projections — so a composed
//!    read cannot see identity's new state beside claims' old one. The
//!    non-vacuity guard matters more than usual: a test that only asserts the
//!    snapshot still shows the old rows would also pass if the concurrent write
//!    silently failed, so each case also proves the write LANDED by reading it
//!    through a fresh reader.
//!
//! 2. **The two generations are not comparable, and must never be compared.**
//!    `world_current` advances its checkpoint past every event *considered*;
//!    the entity fold advances only to the last *adjudication* it folded. A
//!    store with both projections fully folded therefore sits at two different
//!    generations, legitimately. This is pinned as a fact so that anyone who
//!    later "tightens" coherence into an equality check gets a red test naming
//!    the reason instead of a mystery.
//!
//! # Why a second `WorldStore` on the same file
//!
//! The concurrent fold has to come from another connection: `read_snapshot`
//! borrows the store, and a fold needs `&mut`. That is not a test artifact —
//! it is the situation the box is about, one reader composing an answer while
//! something else advances the projections underneath.

use kirra_world::adjudication::{AssertIdentity, IdentityAdjudication, Justification};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-snapshot-coherence-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    cleanup(&p);
    p
}

fn cleanup(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = path.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

fn assert_identity(s: &mut WorldStore, tag: &str, entity: &str, at_ms: i64) {
    let event_id = EventId::new(format!("ev-adj-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-adj-{tag}")).expect("obs");
    let justification =
        Justification::new([ObservationId::new("obs-1").expect("obs")]).expect("justification");
    let at = DomainInstant {
        ms: 1,
        domain: ClockDomain::System,
    };
    s.append_adjudication(
        &AdjudicationRow {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: at_ms,
            valid_from_ms: at_ms,
            source: "operator-console",
            source_version: "1.0.0",
            writer_class: WriterClass::Operator,
        },
        &IdentityAdjudication::Assert(AssertIdentity::new(eid(entity), justification, at)),
    )
    .expect("append adjudication");
}

fn claim(s: &mut WorldStore, tag: &str, subject: &str, object: &str, at_ms: i64) {
    let event_id = EventId::new(format!("ev-claim-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-claim-{tag}")).expect("obs");
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: at_ms,
        valid_from_ms: at_ms,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject,
        subject_ref: None,
        predicate: Some("last_seen_at"),
        object: Some(object),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append claim");
}

fn fold_both(s: &mut WorldStore) {
    s.fold().expect("fold claims");
    s.fold_entity_projection().expect("fold entities");
}

// ---------------------------------------------------------------------------
// 1. The snapshot is real
// ---------------------------------------------------------------------------

/// **A concurrent fold is invisible to a snapshot — on both projections.**
///
/// This is the whole box in one test. The composed read takes claims and
/// identity through one snapshot; between those two reads, another connection
/// appends and folds BOTH projections. If the snapshot were not real, the
/// second read would see the new state and the answer would be composed from
/// two different worlds.
#[test]
fn a_fold_landing_mid_composition_is_invisible_to_both_halves() {
    let path = tmp("midcomposition");

    let mut writer = WorldStore::open(&path).expect("open writer");
    claim(&mut writer, "1", "package_17", "dock_b", T0);
    assert_identity(&mut writer, "1", "dock_b", T0);
    fold_both(&mut writer);
    drop(writer);

    let reader = WorldStore::open(&path).expect("open reader");
    let snap = reader.read_snapshot().expect("snapshot");

    // First half of the composed read. This establishes the snapshot.
    let before_claims = snap.current("package_17", T0).expect("claims");
    assert_eq!(before_claims.len(), 1, "the seeded claim is visible");
    let coordinate_before = snap.coordinate().expect("coordinate");

    // Something else advances BOTH projections, and commits.
    let mut other = WorldStore::open(&path).expect("open second writer");
    claim(&mut other, "2", "package_17", "dock_c", T0 + 1);
    assert_identity(&mut other, "2", "dock_c", T0 + 1);
    fold_both(&mut other);

    // Non-vacuity: the write really landed. Without this, every assertion
    // below would also hold if the concurrent fold had silently done nothing.
    let fresh = WorldStore::open(&path).expect("open fresh reader");
    assert!(
        fresh
            .identity_view()
            .expect("fresh identity")
            .get(&eid("dock_c"))
            .is_some(),
        "the concurrent fold must have landed, or this test proves nothing"
    );
    assert_ne!(
        fresh.projection_coordinate().expect("fresh coordinate"),
        coordinate_before,
        "the store moved on, or this test proves nothing"
    );

    // Second half of the composed read, through the SAME snapshot.
    let view = snap.identity_view().expect("identity");
    assert!(
        view.get(&eid("dock_c")).is_none(),
        "the snapshot must not see an entity folded after it was established"
    );
    assert!(
        view.get(&eid("dock_b")).is_some(),
        "the snapshot must still see what it was established with"
    );

    // And the claims half has not moved either.
    let after_claims = snap.current("package_17", T0).expect("claims again");
    assert_eq!(
        after_claims.len(),
        before_claims.len(),
        "the claims half of the composed read must not move under the reader"
    );
    assert_eq!(
        snap.coordinate().expect("coordinate again"),
        coordinate_before,
        "one snapshot reports one coordinate for its whole lifetime"
    );

    drop(snap);
    drop(reader);
    drop(other);
    drop(fresh);
    cleanup(&path);
}

/// **NEGATIVE CONTROL — the unguarded path really does mix states.**
///
/// The test above is only worth something if the interleaving it survives is one
/// that would otherwise bite. So this runs the identical scenario through the
/// ordinary `&self` readers — the pre-3c composition — and asserts it observes
/// the concurrent fold: claims read before, identity read after, and the two
/// halves disagree about which entities exist.
///
/// If this control ever goes green-by-agreement (both halves seeing one state),
/// the guarded test above has stopped proving anything and the scenario needs
/// rebuilding — which is exactly what a control is for.
#[test]
fn the_unguarded_composition_observes_the_fold_it_should_not() {
    let path = tmp("unguarded");

    let mut writer = WorldStore::open(&path).expect("open writer");
    claim(&mut writer, "1", "package_17", "dock_b", T0);
    assert_identity(&mut writer, "1", "dock_b", T0);
    fold_both(&mut writer);
    drop(writer);

    let reader = WorldStore::open(&path).expect("open reader");

    // First half, unguarded.
    let before_claims = reader.current("package_17", T0).expect("claims");
    assert_eq!(before_claims.len(), 1);

    let mut other = WorldStore::open(&path).expect("open second writer");
    claim(&mut other, "2", "package_17", "dock_c", T0 + 1);
    assert_identity(&mut other, "2", "dock_c", T0 + 1);
    fold_both(&mut other);

    // Second half, unguarded — and it sees a world the first half did not.
    let view = reader.identity_view().expect("identity");
    assert!(
        view.get(&eid("dock_c")).is_some(),
        "the unguarded composition must observe the concurrent fold — if it does \
         not, the snapshot test above is not being asked a real question"
    );

    drop(reader);
    drop(other);
    cleanup(&path);
}

/// **A new snapshot sees the new state.**
///
/// The companion to the test above, and the reason it is not merely asserting
/// that reads are broken: coherence must not be achieved by never seeing
/// anything. A snapshot taken after the fold sees the fold.
#[test]
fn a_snapshot_taken_after_a_fold_sees_it() {
    let path = tmp("aftershot");

    let mut writer = WorldStore::open(&path).expect("open writer");
    claim(&mut writer, "1", "package_17", "dock_b", T0);
    assert_identity(&mut writer, "1", "dock_b", T0);
    fold_both(&mut writer);
    assert_identity(&mut writer, "2", "dock_c", T0 + 1);
    fold_both(&mut writer);
    drop(writer);

    let reader = WorldStore::open(&path).expect("open reader");
    let snap = reader.read_snapshot().expect("snapshot");
    assert!(
        snap.identity_view()
            .expect("identity")
            .get(&eid("dock_c"))
            .is_some(),
        "a snapshot taken after the fold must see it"
    );

    drop(snap);
    drop(reader);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// 2. The two generations are not comparable
// ---------------------------------------------------------------------------

/// **Two fully-folded projections legitimately sit at different generations.**
///
/// `fold_range` advances `world_current`'s checkpoint to `MAX(generation) FROM
/// world_events` — every event CONSIDERED, so a batch that adopts nothing still
/// moves it. `fold_entity_range` advances only to the generation of the last
/// adjudication it folded. Append one ordinary claim after an adjudication and
/// the two checkpoints separate, with both folds complete and nothing wrong.
///
/// Pinned because the intuitive way to "prove" snapshot consistency is to
/// assert the coordinates are equal, and that check would fail here — on a
/// healthy store. Coherence comes from the snapshot, not from these numbers
/// agreeing.
#[test]
fn the_two_projection_generations_differ_on_a_healthy_store() {
    let path = tmp("incomparable");

    let mut s = WorldStore::open(&path).expect("open");
    assert_identity(&mut s, "1", "dock_b", T0);
    // A non-adjudication event: the entity fold consumes nothing from it.
    claim(&mut s, "1", "package_17", "dock_b", T0 + 1);
    fold_both(&mut s);

    let coordinate = s.projection_coordinate().expect("coordinate");
    let claims_generation = coordinate.world_current().generation();
    let entity_generation = coordinate.entities().generation();

    assert!(
        claims_generation > entity_generation,
        "the claims checkpoint passes every event considered ({claims_generation}), \
         the entity checkpoint stops at the last adjudication ({entity_generation}) \
         — an equality check between them would report false drift on a healthy store"
    );

    // Both folds really are complete: re-folding moves neither.
    let before = coordinate.clone();
    fold_both(&mut s);
    assert_eq!(
        s.projection_coordinate().expect("coordinate after refold"),
        before,
        "a fold with nothing new to consume must not move either checkpoint"
    );

    drop(s);
    cleanup(&path);
}

/// **The coordinate carries a digest, not just a position.**
///
/// A generation alone cannot distinguish a fold that advanced from a rebuild
/// that landed on the same head. The digest is what the fold itself commits
/// beside the generation, so it is what an observer records.
#[test]
fn the_coordinate_reports_a_digest_for_a_folded_projection() {
    let path = tmp("digest");

    let mut s = WorldStore::open(&path).expect("open");
    let empty = s.projection_coordinate().expect("coordinate");
    assert!(
        empty.world_current().is_unfolded(),
        "an unfolded projection reports generation 0"
    );
    assert_eq!(
        empty.world_current().state_digest(),
        "",
        "an unfolded projection has no state to digest"
    );

    claim(&mut s, "1", "package_17", "dock_b", T0);
    fold_both(&mut s);

    let folded = s.projection_coordinate().expect("coordinate");
    assert!(!folded.world_current().is_unfolded());
    assert!(
        !folded.world_current().state_digest().is_empty(),
        "a folded projection commits a digest beside its generation"
    );
    assert_eq!(
        folded.world_current().state_digest(),
        s.projection_state_digest().expect("state digest"),
        "the recorded digest is the projection's actual state, not a stale copy"
    );

    // Every projection is reported, including ones this store never folded --
    // so a reader can tell "not read" from "not there".
    assert_eq!(folded.all().len(), 3);
    assert!(folded.subject_summary().is_unfolded());

    drop(s);
    cleanup(&path);
}
