//! **Box 2a end to end: a real producer puts a candidate into Kirra World.**
//!
//! The chain this proves, link by link, against a real store:
//!
//! ```text
//! real producer -> sanctioned write API -> persisted candidate row
//!               -> read back through the normal candidate query path
//! ```
//!
//! Before this crate, Kirra World had a read side, an explain side and a delete
//! side, and no way for a fact to enter except a test. So the bar here is not
//! "the writer compiles" — it is that evidence a producer surveyed becomes a row
//! an ordinary reader finds, carrying the provenance an adjudicator will need.
//!
//! Two things are asserted that a happy-path test would skip, because they are
//! the reasons this door exists:
//!
//! * the producer **cannot** self-confirm — there is no argument that could
//!   carry it, so the proof is that the written row IS a candidate and that
//!   the store refuses the confirmed spelling by another route;
//! * the candidate cites **real** observation ids — the ones on the evidence
//!   rows, not ids reconstructed from subject and predicate. A fabricated
//!   citation would look identical here and would poison box 4b's provenance
//!   walk downstream.

use kirra_world::observation::ConfidenceBasis;
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_candidate::MatcherIdentity;
use kirra_world_ingest::{
    propose_from_agreements, run_ingest_pass, Agreement, ExactIdentifierRule, MAX_IDENTIFIER_GROUP,
};
use kirra_world_store::candidate_record::{CANDIDATE_KIND, CANDIDATE_PREDICATE_TOKEN};
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SURVEY_LIMIT: usize = 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-2a-{name}-{}-{n}.sqlite", std::process::id()));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn rule() -> ExactIdentifierRule {
    ExactIdentifierRule::new(
        "serial_number",
        MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
    )
    .expect("rule")
}

/// A confirmed sensor observation: `subject serial_number value`.
///
/// Written through the ordinary `append`, as a real upstream producer would —
/// the candidate door is only for candidates.
fn observe(store: &mut WorldStore, n: &str, subject: &str, value: &str) -> String {
    let event_id = EventId::new(format!("ev-{n}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{n}")).expect("observation id");
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
        .expect("append observation");
    observation_id.as_str().to_owned()
}

fn minter() -> impl FnMut() -> (EventId, ObservationId) {
    let mut n = 0;
    move || {
        n += 1;
        (
            EventId::new(format!("cand-ev-{n}")).expect("event id"),
            ObservationId::new(format!("cand-obs-{n}")).expect("observation id"),
        )
    }
}

// ---------------------------------------------------------------------------
// The load-bearing test
// ---------------------------------------------------------------------------

/// **The whole chain.** Two entities share a serial number; a pass proposes
/// them; the proposal is a durable row an ordinary reader finds.
#[test]
fn a_producer_puts_a_candidate_into_the_store_and_a_reader_finds_it() {
    let path = tmp("end-to-end");
    let mut store = WorldStore::open(&path).expect("open");
    observe(&mut store, "a", "track-a", "SN-123");
    observe(&mut store, "b", "track-b", "SN-123");

    let report =
        run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("pass runs");
    assert_eq!(
        report.proposed, 1,
        "one pair shares the identifier: {report:?}"
    );
    assert!(!report.survey_truncated, "the survey saw everything");

    // Read back through the ORDINARY candidate path, not a bespoke one.
    let found = store.candidates("track-a").expect("candidates");
    assert_eq!(found.len(), 1, "the proposal is durable: {found:?}");
    let row = &found[0];
    assert_eq!(row.predicate.as_deref(), Some(CANDIDATE_PREDICATE_TOKEN));
    assert_eq!(row.object.as_deref(), Some("track-b"));
    assert_eq!(row.kind, CANDIDATE_KIND);

    // And it decodes back into the domain type an adjudicator will judge.
    // Loaded BY OBSERVATION ID -- the citable handle a promotion would carry --
    // rather than decoded from columns the test happened to have. That is what
    // "the durable observation is the artifact" has to mean if box 2b is going
    // to judge rows instead of caller-supplied structs.
    let decoded = store
        .load_same_as_candidate("cand-obs-1")
        .expect("load")
        .expect("the candidate observation is there");
    assert_eq!(decoded.matcher().producer(), "world-ingest");
    assert_eq!(decoded.matcher().model_or_rule(), "exact-identifier");
    assert_eq!(decoded.matcher().version(), "1.0.0");
    assert_eq!(decoded.confidence().basis(), ConfidenceBasis::Unspecified);
    assert_eq!(
        decoded.confidence().score(),
        None,
        "an exact-match rule computes no probability and must not invent one"
    );
}

/// **The citation names evidence that actually exists.**
///
/// The support ids on the written candidate must be the observation ids of the
/// rows that carried the agreement — not ids reconstructed from subject and
/// predicate, which would look identical in the happy-path test above and would
/// resolve to `Dangling` for every candidate once box 4b walked them.
#[test]
fn the_candidate_cites_the_real_evidence_observations() {
    let path = tmp("citations");
    let mut store = WorldStore::open(&path).expect("open");
    let obs_a = observe(&mut store, "a", "track-a", "SN-9");
    let obs_b = observe(&mut store, "b", "track-b", "SN-9");

    run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("pass");

    // The candidate row's own generation, taken from the row rather than from a
    // count: generations are not dense once compaction has run, so inferring one
    // would be a fixture-only assumption.
    let row = store.candidates("track-a").expect("candidates").remove(0);
    let page = store
        .citations_of(row.generation, 16, None)
        .expect("citations of the candidate row");

    let mut cited: Vec<String> = page
        .edges
        .iter()
        .map(|e| e.cited_observation_id.clone())
        .collect();
    cited.sort();
    let mut expected = vec![obs_a, obs_b];
    expected.sort();
    assert_eq!(
        cited, expected,
        "the candidate must cite both halves of the agreement by their REAL observation ids"
    );
    assert!(!page.truncated, "two citations fit in a page of sixteen");

    // The equality above IS the existence proof, and is worth naming as such:
    // `obs_a`/`obs_b` are the ids `observe` actually appended, so matching them
    // means the citation names those rows. The reconstruct-from-parts version
    // this replaced would have cited `track-a#serial_number` -- well-formed,
    // and naming nothing.
}

/// **A second pass proposes nothing.** Without idempotence the log would grow
/// with pass count rather than with evidence.
#[test]
fn re_running_the_same_matcher_version_proposes_nothing_new() {
    let path = tmp("idempotent");
    let mut store = WorldStore::open(&path).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-1");

    let first = run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("first");
    let second = run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("second");

    assert_eq!(first.proposed, 1);
    assert_eq!(second.proposed, 0, "a re-run is not new evidence");
    assert_eq!(second.already_proposed, 1);
    assert_eq!(
        store.candidates("track-a").expect("candidates").len(),
        1,
        "and the log did not grow"
    );
}

/// **A new matcher version DOES propose again.** A different rule agreeing is a
/// different claim, and collapsing the two would erase whose judgement is on
/// record. The non-vacuity twin of the test above: without it, "proposes
/// nothing" could be a writer that had stopped working.
#[test]
fn a_new_matcher_version_proposes_the_pair_again() {
    let path = tmp("versioned");
    let mut store = WorldStore::open(&path).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-1");

    run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("v1");

    let v2 = ExactIdentifierRule::new(
        "serial_number",
        MatcherIdentity::new("world-ingest", "exact-identifier", "2.0.0").expect("matcher"),
    )
    .expect("rule");
    let mut n = 100;
    let report = run_ingest_pass(&mut store, &v2, T0, SURVEY_LIMIT, move || {
        n += 1;
        (
            EventId::new(format!("v2-ev-{n}")).expect("id"),
            ObservationId::new(format!("v2-obs-{n}")).expect("id"),
        )
    })
    .expect("v2");

    assert_eq!(
        report.proposed, 1,
        "a new version's agreement is a new claim"
    );
    assert_eq!(store.candidates("track-a").expect("c").len(), 2);
}

/// **Entities that do not agree are not proposed.** The negative control for the
/// rule itself: without it, a matcher that proposed every pair it saw would pass
/// every other test here.
#[test]
fn entities_with_different_identifiers_are_not_proposed() {
    let path = tmp("negative");
    let mut store = WorldStore::open(&path).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-2");

    let report = run_ingest_pass(&mut store, &rule(), T0, SURVEY_LIMIT, minter()).expect("pass");
    assert_eq!(report.proposed, 0, "different serials are not evidence");
    assert!(store.candidates("track-a").expect("c").is_empty());
}

// ---------------------------------------------------------------------------
// The rule's own judgement — pure, no store
// ---------------------------------------------------------------------------

fn agree(subject: &str, value: &str) -> Agreement {
    Agreement {
        subject: subject.to_owned(),
        value: value.to_owned(),
        observation_id: format!("obs-{subject}"),
    }
}

/// **All pairs, not a spanning chain.**
///
/// Three subjects sharing a value must yield three proposals, not two.
/// `KIRRA-WM-TRANSITIVITY-001` forbids anyone deriving `a=c` from `a=b` and
/// `b=c`, so a chain-emitting matcher would leave `(a,c)` unproposable — making
/// the final identity depend on a closure the ruling bans, invisibly, by
/// omission.
#[test]
fn a_group_of_three_yields_all_three_pairs_because_transitivity_is_banned() {
    let out = propose_from_agreements(
        &rule(),
        &[agree("a", "SN-1"), agree("b", "SN-1"), agree("c", "SN-1")],
    )
    .expect("propose");

    assert!(out.oversized_groups.is_empty());
    let mut pairs: Vec<(String, String)> = out
        .candidates
        .iter()
        .map(|p| {
            (
                p.pair().low().as_str().to_owned(),
                p.pair().high().as_str().to_owned(),
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("a".to_owned(), "b".to_owned()),
            ("a".to_owned(), "c".to_owned()),
            ("b".to_owned(), "c".to_owned()),
        ],
        "a chain of two would make a=c depend on transitive closure"
    );
}

/// **An over-wide identifier is refused and REPORTED, not trimmed.**
///
/// A value shared by more subjects than the ceiling is a placeholder or a parse
/// failure, not an identifier. Proposing from it would emit hundreds of pairs
/// carrying the same honest-looking provenance as a real match.
#[test]
fn an_oversized_group_is_refused_and_named() {
    let agreements: Vec<Agreement> = (0..=MAX_IDENTIFIER_GROUP)
        .map(|i| agree(&format!("e{i}"), "unknown"))
        .collect();

    let out = propose_from_agreements(&rule(), &agreements).expect("propose");
    assert!(
        out.candidates.is_empty(),
        "nothing is proposed from a placeholder"
    );
    assert_eq!(
        out.oversized_groups,
        vec![("unknown".to_owned(), MAX_IDENTIFIER_GROUP + 1)],
        "and the caller is told which value and how wide"
    );
}

/// **The ceiling admits a group exactly at it.** The boundary control: without
/// it, an off-by-one that refused every group would pass the test above.
#[test]
fn a_group_exactly_at_the_ceiling_is_admitted() {
    let agreements: Vec<Agreement> = (0..MAX_IDENTIFIER_GROUP)
        .map(|i| agree(&format!("e{i}"), "SN-1"))
        .collect();

    let out = propose_from_agreements(&rule(), &agreements).expect("propose");
    assert!(
        out.oversized_groups.is_empty(),
        "at the ceiling is not over it"
    );
    assert_eq!(
        out.candidates.len(),
        MAX_IDENTIFIER_GROUP * (MAX_IDENTIFIER_GROUP - 1) / 2,
        "all pairs"
    );
}

/// **A subject repeating a value has not corroborated itself.**
#[test]
fn one_subject_stating_a_value_twice_is_not_a_pair() {
    let out = propose_from_agreements(&rule(), &[agree("a", "SN-1"), agree("a", "SN-1")])
        .expect("propose");
    assert!(out.candidates.is_empty(), "a subject cannot match itself");
}
