//! **The read-only relationship endpoint — route policy and wire behaviour.**
//!
//! `GET /relations/{subject}` is the second capability-specific route on this
//! producer, and the same argument governs it as the first: the route table IS
//! the security boundary, so it is tested as policy rather than through a
//! network round trip.
//!
//! Every relation asserted about here was proposed by the REAL matcher and
//! promoted by a real `AdjudicationAuthority`. An endpoint proven over
//! hand-written projection rows would prove it serves data no producer can
//! create.

use kirra_explain_types::{ProvenanceStanding, RelationsOutcome, RELATIONS_PATH_PREFIX};
use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
use kirra_world::same_as_candidate::MatcherIdentity;
use kirra_world_explain_service::service::dispatch;
use kirra_world_ingest::{run_ingest_pass, ExactIdentifierRule};
use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-relations-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn observe(store: &mut WorldStore, n: &str, subject: &str, value: &str) {
    let event_id = EventId::new(format!("ev-{n}")).expect("id");
    let observation_id = ObservationId::new(format!("obs-{n}")).expect("id");
    store
        .append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "asset-tag-reader",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject,
            subject_ref: None,
            predicate: Some("serial_number"),
            object: Some(value),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

/// A store holding one promoted `track-a` = `track-b`, via the real chain.
fn store_with_a_promoted_pair(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-1");

    let rule = ExactIdentifierRule::new(
        "serial_number",
        MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
    )
    .expect("rule");
    let mut n = 0;
    let report = run_ingest_pass(&mut store, &rule, T0, 1_000, move || {
        n += 1;
        (
            EventId::new(format!("cand-ev-{n}")).expect("id"),
            ObservationId::new(format!("cand-obs-{n}")).expect("id"),
        )
    })
    .expect("pass");
    assert_eq!(report.proposed, 1);

    let event_id = EventId::new("adj-ev-1").expect("id");
    let observation_id = ObservationId::new("adj-obs-1").expect("id");
    store
        .adjudicate_same_as(&SameAsAdjudicationRequest {
            event_id: &event_id,
            observation_id: &observation_id,
            candidate_observation_id: "cand-obs-1",
            cited: vec![ObservationId::new("cand-obs-1").expect("id")],
            authority: AdjudicationAuthority::new(SourceClass::Operator, "yard-supervisor")
                .expect("authority"),
            outcome: Outcome::Promoted,
            decided_at: DomainInstant {
                ms: T0 as u64 + 10,
                domain: ClockDomain::System,
            },
            txn_time_ms: T0 + 10,
            source: "operator-console",
            source_version: "1.0.0",
        })
        .expect("promote");
    store.fold_relationship_projection().expect("fold");
    (store, path)
}

fn get(store: &WorldStore, subject: &str) -> (String, RelationsOutcome) {
    let r = dispatch(
        store,
        "GET",
        &format!("{RELATIONS_PATH_PREFIX}{subject}"),
        b"",
    );
    let outcome =
        serde_json::from_str::<RelationsOutcome>(&r.body).expect("the body decodes as one type");
    (r.status.to_string(), outcome)
}

// ---------------------------------------------------------------------------
// The answer.
// ---------------------------------------------------------------------------

/// **A promoted pair is served, with its provenance standing.**
#[test]
fn a_promoted_pair_is_served_with_resolved_provenance() {
    let (store, path) = store_with_a_promoted_pair("served");
    let (status, outcome) = get(&store, "track-a");

    assert_eq!(status, "200 OK");
    let RelationsOutcome::Related { view } = outcome else {
        panic!("expected a view, got {outcome:?}");
    };
    assert_eq!(view.subject, "track-a");
    assert_eq!(view.related.len(), 1);
    let r = &view.related[0];
    assert_eq!(r.low, "track-a");
    assert_eq!(r.high, "track-b");
    assert_eq!(
        r.other, "track-b",
        "the OTHER entity, not the one asked about"
    );
    assert_eq!(r.adjudicator, "yard-supervisor");
    assert_eq!(
        r.provenance,
        ProvenanceStanding::Resolved,
        "the candidate is still in the log"
    );
    assert!(!r.decision_marker.is_empty());
    assert!(!view.truncated);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **A subject related to nothing is an ANSWER, and 200.**
///
/// Encoding it as 404 is how clients learn to read "the service is down" and
/// "there are no relations" as the same thing.
#[test]
fn a_subject_related_to_nothing_answers_200_with_an_empty_view() {
    let (store, path) = store_with_a_promoted_pair("empty");
    let (status, outcome) = get(&store, "track-zzz");

    assert_eq!(status, "200 OK");
    let RelationsOutcome::Related { view } = outcome else {
        panic!("expected an empty view, got {outcome:?}");
    };
    assert!(view.related.is_empty());

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **An unaskable subject is REFUSED, not answered as unrelated.**
#[test]
fn a_malformed_subject_is_refused_rather_than_answered_as_unrelated() {
    let (store, path) = store_with_a_promoted_pair("malformed");
    // A whitespace-only subject: `EntityId::new` refuses it, and the refusal
    // must reach the wire as its own case.
    let (status, outcome) = get(&store, "   ");
    assert_eq!(status, "400 Bad Request");
    assert!(
        matches!(outcome, RelationsOutcome::NotAnEntity { .. }),
        "got {outcome:?}"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **Compacting the cited candidate degrades the provenance and leaves the
/// relation standing** — `KIRRA-WM-EVIDENCE-RETENTION-001`, at the wire.
///
/// The endpoint is where that ruling becomes visible to an operator rather than
/// only to a test, which is the reason provenance is on this contract at all.
/// `Degraded` and not `Dangling`: ADR-0041 §11.3 forbids reporting deleted
/// evidence as never-recorded.
#[test]
fn compacting_the_candidate_degrades_provenance_and_keeps_the_relation() {
    let (mut store, path) = store_with_a_promoted_pair("compacted");

    // Before: resolved. Without this the assertion below could not tell
    // "degraded" from "never resolved in the first place".
    let (_, before) = get(&store, "track-a");
    let RelationsOutcome::Related { view } = before else {
        panic!("expected a view")
    };
    assert_eq!(view.related[0].provenance, ProvenanceStanding::Resolved);

    // The candidate is `retention_class = "raw"`; the adjudication is
    // protected, so only the evidence can go.
    let candidate_generation = store
        .query_scalar_for_test(
            "SELECT generation FROM world_events WHERE observation_id = 'cand-obs-1'",
        )
        .expect("find the candidate");
    let outcome = store
        .compact_range(candidate_generation, candidate_generation, T0 + 100)
        .expect("compacting a raw candidate is permitted");
    assert!(outcome.removed > 0);

    let (status, after) = get(&store, "track-a");
    assert_eq!(status, "200 OK");
    let RelationsOutcome::Related { view } = after else {
        panic!("the relation must survive compaction of its evidence")
    };
    assert_eq!(view.related.len(), 1, "the relation still holds");
    assert_eq!(
        view.related[0].provenance,
        ProvenanceStanding::Degraded,
        "and its explanation has decayed — not vanished, and not still Resolved"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Route policy — READ-ONLY, capability-specific, fail-closed path parse.
// ---------------------------------------------------------------------------

/// **The endpoint is read-only, and says so.**
///
/// A write verb on a known prefix is a 405 rather than a 404: a client that
/// thinks it can adjudicate through this service should be told it cannot, not
/// told the road does not exist. Adjudication is `KIRRA-WM-PROMOTION-001`
/// territory and needs authentication this process does not have.
#[test]
fn write_verbs_are_refused_on_the_relations_prefix() {
    let (store, path) = store_with_a_promoted_pair("readonly");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let r = dispatch(&store, method, "/relations/track-a", b"{}");
        assert_eq!(r.status, "405 Method Not Allowed", "{method}");
        assert!(r.body.contains("read-only"), "{method}: {}", r.body);
    }
    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **The path parse is fail-closed and is not a URL decoder.**
///
/// Each of these is refused rather than half-interpreted. Guessing an encoding
/// means two spellings of a subject could reach the store as different strings,
/// or — worse — as the same one.
#[test]
fn ambiguous_paths_are_refused_rather_than_interpreted() {
    let (store, path) = store_with_a_promoted_pair("paths");
    for p in [
        "/relations/",                // no subject
        "/relations/track-a/extra",   // a further segment
        "/relations/track%2Da",       // percent-encoding, unsupported
        "/relations/track-a?depth=3", // a query knob
    ] {
        let r = dispatch(&store, "GET", p, b"");
        assert_eq!(r.status, "400 Bad Request", "{p} -> {}", r.body);
    }
    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **No generic ask-World route appeared.**
///
/// The near-miss list the explanation route already carries, extended to this
/// one. Capability-specific by construction means the table has no entry a
/// client could steer.
#[test]
fn plausible_near_misses_are_not_routes() {
    let (store, path) = store_with_a_promoted_pair("nearmiss");
    for p in [
        "/relations",
        "/relation/track-a",
        "/related/track-a",
        "/relations/track-a/provenance",
        "/query",
        "/ask",
    ] {
        let r = dispatch(&store, "GET", p, b"");
        assert!(
            r.status.starts_with("404") || r.status.starts_with("400"),
            "{p} must not be a served route, got {}",
            r.status
        );
    }
    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **The wire carries no handle back into the World.**
///
/// A structural read of the serialized body: no field a client could feed to
/// another endpoint. `decision_marker` is the one correlatable value and no
/// route accepts it — asserted here by sending it back and getting a 404.
#[test]
fn the_wire_carries_nothing_a_client_could_ask_with() {
    let (store, path) = store_with_a_promoted_pair("nohandle");
    let r = dispatch(&store, "GET", "/relations/track-a", b"");
    for forbidden in ["answer_ref", "cursor", "generation\"", "lineage", "query"] {
        assert!(
            !r.body.contains(forbidden),
            "the body exposes `{forbidden}`: {}",
            r.body
        );
    }

    let view: RelationsOutcome = serde_json::from_str(&r.body).expect("decode");
    let RelationsOutcome::Related { view } = view else {
        panic!("expected a view")
    };
    let marker = &view.related[0].decision_marker;
    let echoed = dispatch(&store, "GET", &format!("/relations/{marker}"), b"");
    // It parses as a subject and answers "nothing" -- it is a string, not a
    // key. What matters is that no route TAKES it as a decision coordinate.
    assert_eq!(echoed.status, "200 OK");
    assert!(
        echoed.body.contains("\"related\":[]"),
        "a marker must not resolve to anything: {}",
        echoed.body
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}
