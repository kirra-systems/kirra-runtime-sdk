//! **Tier 4 box 4a — the citation relation as real structure.**
//!
//! `KIRRA-WM-PROVENANCE-GRAPH-001`:
//!
//! > Materialize the citation relation at append, but **do not materialize its
//! > resolved target.** That distinction is what preserves historical
//! > correctness.
//!
//! `WM_SCOPE.md` §7 made `Explain` wait on *"derivation edges being real
//! structure rather than a JSON array of identifiers"*. These tests are about
//! the structure existing **and about what it is forbidden to contain**, which
//! is the harder half: a table with a resolved target column would pass every
//! test about the edges being present, and would have destroyed the property
//! Tier 4 exists to have.
//!
//! # The four properties
//!
//! 1. **The edge is the citation, not its resolution.** An edge is byte-identical
//!    whether or not the cited observation exists. This is the 4a-level
//!    precondition for 4b's historical honesty: if resolution were captured
//!    here, a query pinned before a target appeared could not report it as
//!    dangling, because the answer would already have been decided at write.
//! 2. **The recorded array is reproduced exactly** — order and duplicates.
//! 3. **Rebuilding from retained sources reproduces the table.** This is what
//!    makes it a deterministic index rather than evidence.
//! 4. **An edge never outlives its source event.** Compaction takes both.
//!
//! Plus the ambiguity the coverage floor exists to resolve: an empty edge set
//! means *"cited nothing"* only when the index actually covers that generation.

use kirra_world_store::provenance_edges::{CitationEdge, MAX_CITATIONS_PAGE};
use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass,
    PROVENANCE_EDGES_FLOOR_KEY,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-cite-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    clean(&p);
    p
}

fn clean(p: &std::path::Path) {
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

/// Append one claim about `subject`, citing `cited`, and return its generation.
fn append_citing(s: &mut WorldStore, tag: &str, subject: &str, at_ms: i64, cited: &[&str]) -> i64 {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{tag}")).expect("obs id");
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
        provenance: cited,
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject,
        subject_ref: None,
        predicate: Some("last_seen_at"),
        object: Some("dock_a"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append")
}

fn cited_ids(edges: &[CitationEdge]) -> Vec<(i64, &str)> {
    edges
        .iter()
        .map(|e| (e.ordinal, e.cited_observation_id.as_str()))
        .collect()
}

/// A source's citations at the full page size — the shape most tests want.
fn cites(s: &WorldStore, generation: i64) -> Vec<CitationEdge> {
    s.citations_of(generation, MAX_CITATIONS_PAGE, None)
        .expect("citations")
        .edges
}

/// Every edge in the store, ordered — the comparison subject for rebuilds.
fn all_edges(s: &WorldStore) -> Vec<(i64, i64, String)> {
    s.raw_query_edges_for_test()
}

// ---------------------------------------------------------------------------
// 1. The edge is the citation, not its resolution
// ---------------------------------------------------------------------------

/// **The decisive test: an edge does not change when its target appears.**
///
/// The user's ruling in one assertion. A source cites `obs-x` at a time when no
/// event carries that observation id; later an event carrying `obs-x` is
/// appended. The recorded edge must be **identical** — because the edge records
/// what the source claimed to cite, and the source's claim did not change.
///
/// If this ever fails, 4a has started storing resolution, and 4b's historical
/// honesty becomes unimplementable: the answer to *"did this resolve at T?"*
/// would have been fixed at write time, when the only true answer depends on
/// the coordinate the question is asked at.
#[test]
fn an_edge_is_identical_whether_or_not_its_target_exists() {
    let path = tmp("cite-not-resolve");
    let mut s = WorldStore::open(&path).expect("open");

    // T1 — cite an observation nothing carries.
    let source = append_citing(&mut s, "source", "package_17", T0, &["obs-x"]);
    let at_t1 = cites(&s, source);
    assert_eq!(
        cited_ids(&at_t1),
        vec![(0, "obs-x")],
        "the citation is recorded even though obs-x resolves to nothing"
    );

    // T2 — an event carrying obs-x is appended. The citation now has something
    // to resolve to. That is a fact about the LOG, not about the source event.
    let observation_id = ObservationId::new("obs-x").expect("obs id");
    let event_id = EventId::new("ev-target").expect("event id");
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: T0 + 1,
        valid_from_ms: T0 + 1,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject: "package_17",
        subject_ref: None,
        // `last_seen_at`, not an invented predicate. These fixtures need a
        // second EVENT, not a second vocabulary term — and minting one would
        // force a real freshness ruling (`check_freshness_coverage`) on a
        // predicate the domain does not actually use. Caught by that gate.
        predicate: Some("last_seen_at"),
        object: Some("dock_b"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append target");

    let at_t2 = cites(&s, source);
    assert_eq!(
        at_t2, at_t1,
        "the source's recorded citation must not change because the log later \
         grew something that resolves it — an edge that moved here would have \
         decided, at write time, an answer that depends on the query's coordinate"
    );

    drop(s);
    clean(&path);
}

/// **And the same when the target becomes PLURAL.**
///
/// `world_events.observation_id` is not unique. A second event carrying the
/// same id makes the citation resolve to two rows — still without touching the
/// edge, because plurality is a property of the log at a coordinate.
#[test]
fn an_edge_is_unchanged_when_its_target_becomes_plural() {
    let path = tmp("plural");
    let mut s = WorldStore::open(&path).expect("open");
    let source = append_citing(&mut s, "source", "package_17", T0, &["obs-shared"]);
    let before = cites(&s, source);
    // Pinned, not assumed. Comparing `before` to `after` proves nothing if both
    // are empty, and an implementation that indexed only resolvable citations
    // would make both empty — so this assertion is what stops the comparison
    // below from passing for the wrong reason. Found by mutating exactly that.
    assert_eq!(
        cited_ids(&before),
        vec![(0, "obs-shared")],
        "the citation must be recorded before anything resolves it"
    );

    for (i, tag) in ["first", "second"].iter().enumerate() {
        let observation_id = ObservationId::new("obs-shared").expect("obs id");
        let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0 + 1 + i as i64,
            valid_from_ms: T0 + 1 + i as i64,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some("dock_b"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
    }

    assert_eq!(
        cites(&s, source),
        before,
        "two rows now carry obs-shared; the citation that named it is unchanged"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// 2. The recorded array is reproduced exactly
// ---------------------------------------------------------------------------

#[test]
fn order_and_duplicates_survive_into_the_index() {
    let path = tmp("verbatim");
    let mut s = WorldStore::open(&path).expect("open");
    // Deliberately unsorted, and with a repeat: the array is the source's
    // statement, and an index that tidied it would describe a provenance the
    // hash does not cover.
    let g = append_citing(
        &mut s,
        "source",
        "package_17",
        T0,
        &["obs-c", "obs-a", "obs-c"],
    );
    assert_eq!(
        cited_ids(&cites(&s, g)),
        vec![(0, "obs-c"), (1, "obs-a"), (2, "obs-c")]
    );

    drop(s);
    clean(&path);
}

#[test]
fn an_event_citing_nothing_has_no_edges() {
    let path = tmp("empty");
    let mut s = WorldStore::open(&path).expect("open");
    let g = append_citing(&mut s, "source", "package_17", T0, &[]);
    assert!(cites(&s, g).is_empty());

    // ...and this store SAYS the index covers that generation, which is what
    // makes the empty set readable as "cited nothing" rather than "unknown".
    assert_eq!(
        s.provenance_edges_floor().expect("floor"),
        0,
        "a store born at v7 indexes every append, so nothing is uncovered"
    );

    drop(s);
    clean(&path);
}

/// Adjudications are appended through `append_adjudication`, which holds an open
/// transaction across its call to `append`. The edge write must nest inside it —
/// a `BEGIN` there would fail with "cannot start a transaction within a
/// transaction", which is why the implementation uses a SAVEPOINT.
#[test]
fn an_adjudications_justification_is_indexed_through_the_nested_write() {
    use kirra_world::adjudication::{AssertIdentity, IdentityAdjudication, Justification};
    use kirra_world::observation::{ClockDomain, DomainInstant};
    use kirra_world::reference::EntityId;
    use kirra_world_store::adjudication_record::AdjudicationRow;

    let path = tmp("adjudication");
    let mut s = WorldStore::open(&path).expect("open");

    let event_id = EventId::new("ev-adj").expect("event id");
    let observation_id = ObservationId::new("obs-adj").expect("obs id");
    let justification =
        Justification::new([ObservationId::new("obs-just").expect("obs")]).expect("justification");
    let adjudication = IdentityAdjudication::Assert(AssertIdentity::new(
        EntityId::new("dock_a").expect("entity"),
        justification,
        DomainInstant {
            ms: 1,
            domain: ClockDomain::System,
        },
    ));

    let g = s
        .append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0,
                valid_from_ms: T0,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            &adjudication,
        )
        .expect("append adjudication");

    assert_eq!(
        cited_ids(&cites(&s, g)),
        vec![(0, "obs-just")],
        "the justification IS the adjudication's provenance, and must be indexed \
         like any other citation"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// 3. Rebuild equivalence — the index is a function of retained sources
// ---------------------------------------------------------------------------

/// **The property that makes this an index rather than evidence.**
///
/// Drop the whole table, restore the migration's coverage floor, and rebuild
/// from the stored `provenance` columns. The result must equal what the append
/// path wrote — which is the v6→v7 migration path exactly, since that is a
/// store with events and no edges.
///
/// The two sides are produced from different INPUTS — the caller's `&[&str]` at
/// append, the hash-covered JSON at backfill — through the same derivation. If
/// they ever diverge, the index has stopped being reproducible from the
/// evidence, and it can no longer be checked against anything.
#[test]
fn rebuilding_from_retained_sources_reproduces_the_index() {
    let path = tmp("rebuild");
    let mut s = WorldStore::open(&path).expect("open");
    append_citing(&mut s, "a", "package_17", T0, &["obs-1", "obs-2"]);
    append_citing(&mut s, "b", "package_17", T0 + 1, &[]);
    append_citing(
        &mut s,
        "c",
        "package_18",
        T0 + 2,
        &["obs-2", "obs-2", "obs-3"],
    );

    let appended = all_edges(&s);
    assert!(!appended.is_empty(), "the fixture must index something");

    // Simulate a v6 store migrated to v7: table installed, empty, floor at head.
    s.raw_execute_for_test("DELETE FROM provenance_edges")
        .expect("clear");
    s.raw_execute_for_test(&format!(
        "INSERT INTO world_store_meta (key, value) VALUES ('{PROVENANCE_EDGES_FLOOR_KEY}', '3')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value"
    ))
    .expect("set floor");
    assert_eq!(s.provenance_edges_floor().expect("floor"), 3);
    assert!(all_edges(&s).is_empty(), "the simulated migration is empty");

    let written = s.backfill_provenance_edges().expect("backfill");
    assert_eq!(written, appended.len(), "every edge is rewritten");
    assert_eq!(
        all_edges(&s),
        appended,
        "a backfilled store and an append-indexed store must hold the SAME rows"
    );
    assert_eq!(
        s.provenance_edges_floor().expect("floor"),
        0,
        "a completed backfill claims full coverage"
    );

    drop(s);
    clean(&path);
}

#[test]
fn the_backfill_is_idempotent() {
    let path = tmp("idempotent");
    let mut s = WorldStore::open(&path).expect("open");
    append_citing(&mut s, "a", "package_17", T0, &["obs-1", "obs-1"]);
    let once = {
        s.backfill_provenance_edges().expect("first");
        all_edges(&s)
    };
    s.backfill_provenance_edges().expect("second");
    assert_eq!(
        all_edges(&s),
        once,
        "running the backfill over an already-indexed store converges rather \
         than duplicating or interleaving ordinals"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// Rule 2 — the citation read is bounded, and says when it stopped
// ---------------------------------------------------------------------------

/// **Nothing bounds how many observations an event may cite.**
///
/// Not the schema, not the domain, not the caller's argument — so a
/// whole-array read is structurally unbounded, which is exactly the shape box
/// 3d found looking bounded at the API while the store method underneath was
/// not. `citations_of` therefore takes a limit and reports truncation.
#[test]
fn a_source_citing_more_than_a_page_is_truncated_and_says_so() {
    let path = tmp("bounded");
    let mut s = WorldStore::open(&path).expect("open");
    let many: Vec<String> = (0..MAX_CITATIONS_PAGE + 7)
        .map(|i| format!("obs-{i}"))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let g = append_citing(&mut s, "wide", "package_17", T0, &refs);

    let page = s
        .citations_of(g, MAX_CITATIONS_PAGE, None)
        .expect("citations");
    assert_eq!(page.edges.len(), MAX_CITATIONS_PAGE, "the page is bounded");
    assert!(page.truncated, "and it must SAY it stopped short");
    assert_eq!(
        page.next_after_ordinal(),
        Some((MAX_CITATIONS_PAGE - 1) as i64)
    );

    // Continuing reaches the tail, and the tail reports itself complete.
    let rest = s
        .citations_of(g, MAX_CITATIONS_PAGE, page.next_after_ordinal())
        .expect("citations");
    assert_eq!(rest.edges.len(), 7);
    assert!(!rest.truncated);
    assert_eq!(rest.next_after_ordinal(), None);
    assert_eq!(
        rest.edges[0].cited_observation_id,
        format!("obs-{MAX_CITATIONS_PAGE}"),
        "continuation resumes exactly after the served ordinal — no gap, no repeat"
    );

    drop(s);
    clean(&path);
}

/// A source with *precisely* a page of citations is COMPLETE.
///
/// The off-by-one the extra probe row exists to get right: inferring truncation
/// from `edges.len() == limit` would report this one cut short.
#[test]
fn a_source_with_exactly_one_page_is_not_reported_truncated() {
    let path = tmp("exact");
    let mut s = WorldStore::open(&path).expect("open");
    let exact: Vec<String> = (0..MAX_CITATIONS_PAGE)
        .map(|i| format!("obs-{i}"))
        .collect();
    let refs: Vec<&str> = exact.iter().map(String::as_str).collect();
    let g = append_citing(&mut s, "exact", "package_17", T0, &refs);

    let page = s
        .citations_of(g, MAX_CITATIONS_PAGE, None)
        .expect("citations");
    assert_eq!(page.edges.len(), MAX_CITATIONS_PAGE);
    assert!(
        !page.truncated,
        "a full page is not the same as a truncated one"
    );

    drop(s);
    clean(&path);
}

/// Refused, never clamped — the `SelfFilterMask` / `LineagePage` discipline.
#[test]
fn an_unusable_page_bound_is_refused() {
    let path = tmp("refuse");
    let mut s = WorldStore::open(&path).expect("open");
    let g = append_citing(&mut s, "a", "package_17", T0, &["obs-1"]);

    assert!(
        s.citations_of(g, 0, None).is_err(),
        "a zero limit returns nothing while more remains — an infinite loop \
         dressed as a paginated read"
    );
    assert!(
        s.citations_of(g, MAX_CITATIONS_PAGE + 1, None).is_err(),
        "over the ceiling is refused rather than silently clamped, which would \
         answer a different question and report it as the one asked"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// The coverage floor — "cited nothing" vs "not indexed yet"
// ---------------------------------------------------------------------------

/// **An empty edge set is ambiguous, and the floor is what disambiguates it.**
///
/// Two stores can hold a source event with zero edges and mean opposite things:
/// one where that source genuinely cited nothing, and one migrated from v6
/// whose backfill has not run. Without the floor they are byte-identical, and a
/// reader would report "cited nothing" for every event in an un-backfilled
/// store — a positive claim about provenance, made about the whole log, silently.
///
/// This is the same absent-because-unknown versus absent-because-empty
/// distinction Tier 3 spent case 8 on, at the storage layer.
#[test]
fn the_floor_separates_cited_nothing_from_not_indexed_yet() {
    let path = tmp("floor");
    let mut s = WorldStore::open(&path).expect("open");
    let genuinely_empty = append_citing(&mut s, "a", "package_17", T0, &[]);
    let has_citations = append_citing(&mut s, "b", "package_17", T0 + 1, &["obs-1"]);

    // Store A: fully indexed. An empty set here MEANS cited nothing.
    assert_eq!(s.provenance_edges_floor().expect("floor"), 0);
    assert!(cites(&s, genuinely_empty).is_empty());

    // Store B: the same rows, index not yet built. Both sources now look empty,
    // and only the floor tells a reader not to believe it.
    s.raw_execute_for_test("DELETE FROM provenance_edges")
        .expect("clear");
    s.raw_execute_for_test(&format!(
        "INSERT INTO world_store_meta (key, value) VALUES ('{PROVENANCE_EDGES_FLOOR_KEY}', '2')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value"
    ))
    .expect("set floor");

    assert!(cites(&s, has_citations).is_empty());
    assert_eq!(
        s.provenance_edges_floor().expect("floor"),
        2,
        "a source with real citations now reads as empty, and ONLY the floor \
         distinguishes this store from the one above"
    );

    drop(s);
    clean(&path);
}

/// A store predating the key claims no coverage at all, rather than full
/// coverage by omission. Fail-closed: "no claim" reads as "covers nothing".
#[test]
fn a_store_with_no_recorded_floor_claims_no_coverage() {
    let path = tmp("no-floor");
    let mut s = WorldStore::open(&path).expect("open");
    append_citing(&mut s, "a", "package_17", T0, &["obs-1"]);
    append_citing(&mut s, "b", "package_17", T0 + 1, &["obs-2"]);

    s.raw_execute_for_test(&format!(
        "DELETE FROM world_store_meta WHERE key = '{PROVENANCE_EDGES_FLOOR_KEY}'"
    ))
    .expect("drop the key");

    assert_eq!(
        s.provenance_edges_floor().expect("floor"),
        2,
        "absent means the log head, not 0 — a missing claim must not read as a \
         claim of completeness"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// 4. An edge never outlives its source event
// ---------------------------------------------------------------------------

/// **Compaction takes the edges with the events.**
///
/// The invariant the whole "index, never evidence" claim rests on. An edge whose
/// source row is gone would be a citation still readable after the hash-covered
/// statement it came from was deleted — with nothing left to check it against,
/// and no way for a reader to tell it apart from one that is still backed.
#[test]
fn compaction_removes_the_edges_of_the_events_it_removes() {
    let path = tmp("compaction");
    let mut s = WorldStore::open(&path).expect("open");
    let doomed = append_citing(&mut s, "a", "package_17", T0, &["obs-1", "obs-2"]);
    let doomed2 = append_citing(&mut s, "b", "package_17", T0 + 1, &["obs-3"]);
    let survivor = append_citing(&mut s, "c", "package_17", T0 + 2, &["obs-4"]);

    assert!(!cites(&s, doomed).is_empty());

    s.compact_range(doomed, doomed2, T0 + 9_000)
        .expect("compact");

    assert!(
        cites(&s, doomed).is_empty(),
        "the source event is gone; its citations must go with it"
    );
    assert!(
        cites(&s, doomed2).is_empty(),
        "every generation in the compacted range, not just the first"
    );
    assert_eq!(
        cited_ids(&cites(&s, survivor)),
        vec![(0, "obs-4")],
        "a source outside the compacted range keeps its citations — otherwise \
         the test above would pass against a compaction that deleted everything"
    );

    drop(s);
    clean(&path);
}

/// The counterpart to the deletion: what a reader is left with is *degraded*,
/// not a confident empty answer. The distinction is already carried by the
/// citation/summary machinery; this pins that compaction did not quietly leave
/// the index looking intact.
#[test]
fn a_compacted_source_leaves_no_edge_claiming_to_be_backed() {
    let path = tmp("no-orphans");
    let mut s = WorldStore::open(&path).expect("open");
    append_citing(&mut s, "a", "package_17", T0, &["obs-1"]);
    append_citing(&mut s, "b", "package_17", T0 + 1, &["obs-2"]);
    append_citing(&mut s, "c", "package_17", T0 + 2, &["obs-3"]);
    s.compact_range(1, 2, T0 + 9_000).expect("compact");

    let orphans = s.raw_count_orphan_edges_for_test();
    assert_eq!(
        orphans, 0,
        "no edge may name a source generation the log no longer holds"
    );

    drop(s);
    clean(&path);
}
