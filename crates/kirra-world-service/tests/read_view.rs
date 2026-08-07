//! The answer boundary's contract — the three Tier 3 rules, against a real store.
//!
//! Each test names the rule it pins and, where a guard is involved, what breaks
//! if the guard is removed. The second test is the odd one: it pins the
//! *falsification* that made this module necessary, and is meant to stop
//! compiling once Tier 3 lands.

use kirra_world::evidence::DigestError;
use kirra_world_service::read_view::{AskError, UnknownReason, WorldLookup, WorldView};
use kirra_world_store::{
    Adjudication, ClaimStatus, Corroboration, EventId, NewEvent, ObservationId, Origin, TrustAxes,
    TrustGrade, Validity, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-readview-{}-{}.sqlite",
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

fn axes(corr: Corroboration, adj: Adjudication) -> TrustAxes {
    TrustAxes::new(Origin::Observed, corr, adj).expect("constructible")
}

fn seed(
    store: &mut WorldStore,
    n: i64,
    subject: &str,
    valid_to_ms: Option<i64>,
    trust: Option<&TrustAxes>,
    claim_status: ClaimStatus,
) {
    let event = EventId::new(format!("evt-{n}")).unwrap();
    let observation = ObservationId::new(format!("obs-{n}")).unwrap();
    store
        .append(&NewEvent {
            event_id: &event,
            observation_id: &observation,
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: WriterClass::Sensor,
            claim_status,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject,
            predicate: Some("colour"),
            object: Some("red"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust,
        })
        .expect("append");
    store.rebuild_projections().expect("project");
}

/// Like [`seed`], but with the predicate under the caller's control.
///
/// Needed because `world_current` is keyed by `(subject, predicate_key)`, so a
/// test about *which row* is damaged cannot use a fixture that pins one
/// predicate for every claim.
fn seed_with_predicate(
    store: &mut WorldStore,
    n: i64,
    subject: &str,
    predicate: &str,
    trust: Option<&TrustAxes>,
) {
    let event = EventId::new(format!("evt-{n}")).unwrap();
    let observation = ObservationId::new(format!("obs-{n}")).unwrap();
    store
        .append(&NewEvent {
            event_id: &event,
            observation_id: &observation,
            txn_time_ms: T0,
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
            subject,
            predicate: Some(predicate),
            object: Some("red"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust,
        })
        .expect("append");
    store.rebuild_projections().expect("project");
}

fn open(name: &str) -> WorldStore {
    WorldStore::open(&tmp(name)).expect("open")
}

// ---------------------------------------------------------------------------
// Rule 1 — no bare values
// ---------------------------------------------------------------------------

/// An answer arrives with validity, trust and provenance already attached.
#[test]
fn an_answer_carries_validity_trust_and_provenance() {
    let mut s = open("bundle");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);

    let view = WorldView::new(&s, Some(60_000));
    let WorldLookup::Answered(answers) = view.ask("cup-1", T0).expect("ask") else {
        panic!("expected an answer");
    };
    assert_eq!(answers.len(), 1);
    let ans = &answers[0];

    assert_eq!(ans.value(), r#"{"n":1}"#);
    assert_eq!(ans.subject(), "cup-1");
    assert_eq!(ans.predicate(), Some("colour"));

    // The three things the rule says may never be lost. Fresh rather than
    // Timeless because the caller supplied a budget, so this claim CAN go stale
    // and is merely within it at now == valid_from.
    assert_eq!(ans.validity(), Validity::Fresh);
    assert_eq!(ans.grade(), Some(TrustGrade::Strong));
    // The provenance handle is TYPED now, so "not an empty string" is no longer
    // something a test has to assert -- `EvidenceDigest` cannot be empty, cannot
    // be non-hex and cannot be the wrong length. What is left worth asserting is
    // that it is the store's real digest rather than a plausible-looking one.
    assert_eq!(
        ans.provenance().as_str(),
        s.head_chain().expect("head"),
        "the handle locates this claim in the actual chain"
    );
    assert_eq!(ans.event_id(), "evt-1");
}

/// **The falsification this module exists to answer**, pinned so it cannot
/// quietly stop being true.
///
/// `ProjectedClaim`'s fields are public, so the payload is reachable with no
/// validity, no trust and no handle. Tier 3's rule 1 therefore cannot be met by
/// the store's row type, and has to be met at a boundary — which is the entire
/// argument for `read_view` existing.
///
/// **When Tier 3 lands and the rule holds structurally at the store, this test
/// should FAIL TO COMPILE.** That is the intended outcome, not a regression, and
/// whoever hits it should delete this test rather than work around it.
#[test]
fn the_store_row_is_a_bare_value_today() {
    let mut s = open("bare");
    seed(&mut s, 1, "cup-1", None, None, ClaimStatus::Confirmed);

    let claims = s.current("cup-1", T0).expect("read");
    // Reaching the payload asks for no validity, no grade and no handle --
    // the row carries those columns, but nothing makes you look at them.
    // This compiles.
    let bare = &claims[0].payload;
    assert_eq!(bare, r#"{"n":1}"#);
}

/// **Rule 1 says "the trust axes", and the answer carries them** — not just the
/// grade collapsed out of them.
///
/// An earlier draft carried only [`WorldAnswer::grade`] while the module docs
/// quoted the rule verbatim, which was an overclaim. This pins the fix.
#[test]
fn an_answer_carries_the_axes_not_only_the_collapsed_grade() {
    let mut s = open("axes");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);

    let view = WorldView::new(&s, Some(60_000));
    let WorldLookup::Answered(answers) = view.ask("cup-1", T0).expect("ask") else {
        panic!("expected an answer");
    };
    let got = answers[0]
        .axes()
        .expect("a labelled claim carries its axes");
    assert_eq!(got.origin(), Origin::Observed);
    assert_eq!(got.corroboration(), Corroboration::Corroborated(2));
    assert_eq!(got.adjudication(), Adjudication::Confirmed);
}

/// **The reason behind a grade survives the collapse.**
///
/// This is why carrying both matters rather than being belt-and-braces. Two
/// claims can grade **identically for different reasons**, and a boundary
/// returning only the grade would make them indistinguishable — leaving a caller
/// who needs to know why with nowhere to look but the raw log.
///
/// Both below grade `Strong`. One is corroborated by two sources; the other has
/// a single source and rests entirely on its adjudication. Same summary, very
/// different evidentiary basis, and only the axes tell them apart.
///
/// **Why this pairing and not a weaker one:** `Weak` is unreachable through this
/// boundary at all. `trust_grade` yields it only when the adjudication is not
/// `Confirmed`, and the derivation `CHECK` plus the projection's confirmed-only
/// fold mean every *labelled* claim that gets here is `Confirmed`. So the
/// reachable set is `Strong`, `Adequate` and `None` — the same emergent
/// narrowing that makes `Inadmissible` unreachable, one notch less severe.
#[test]
fn two_claims_can_share_a_grade_for_different_reasons() {
    let mut s = open("why");
    let corroborated = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    let single_source = axes(Corroboration::Uncorroborated, Adjudication::Confirmed);
    seed(
        &mut s,
        1,
        "a",
        None,
        Some(&corroborated),
        ClaimStatus::Confirmed,
    );
    seed(
        &mut s,
        2,
        "b",
        None,
        Some(&single_source),
        ClaimStatus::Confirmed,
    );

    let view = WorldView::new(&s, Some(60_000));

    let WorldLookup::Answered(a1) = view.ask("a", T0).expect("ask") else {
        panic!("expected an answer");
    };
    let WorldLookup::Answered(a2) = view.ask("b", T0).expect("ask") else {
        panic!("expected an answer");
    };

    // Identical summaries.
    assert_eq!(a1[0].grade(), Some(TrustGrade::Strong));
    assert_eq!(a2[0].grade(), Some(TrustGrade::Strong));

    // Different reasons -- recoverable ONLY from the axes. Without them the
    // grade would have been the end of the trail.
    assert_eq!(
        a1[0].axes().expect("labelled").corroboration(),
        Corroboration::Corroborated(2)
    );
    assert_eq!(
        a2[0].axes().expect("labelled").corroboration(),
        Corroboration::Uncorroborated
    );
}

/// An unlabelled claim grades to `None`, never a manufactured default.
///
/// Revert that and this fails: defaulting to any grade invents a trust
/// judgement from the absence of one, which is what separate axes prevent.
#[test]
fn an_unlabelled_claim_has_no_grade_rather_than_a_default() {
    let mut s = open("unlabelled");
    seed(&mut s, 1, "cup-1", None, None, ClaimStatus::Confirmed);

    let view = WorldView::new(&s, Some(60_000));
    let WorldLookup::Answered(answers) = view.ask("cup-1", T0).expect("ask") else {
        panic!("expected an answer");
    };
    assert_eq!(answers[0].grade(), None);
    assert_eq!(
        answers[0].axes(),
        None,
        "an unlabelled claim carries no axes either -- absence, not a default set"
    );
}

// ---------------------------------------------------------------------------
// Rule 3 — Unknown is a success
// ---------------------------------------------------------------------------

/// An unknown subject is `Ok(Unknown)`, never `Err`.
///
/// Revert to an error and this fails. That is the point: an error gets caught
/// upstream and turned into a default value, which is how "I don't know"
/// becomes a confident wrong answer.
#[test]
fn an_unknown_subject_is_a_success_not_an_error() {
    let s = open("unknown");
    let view = WorldView::new(&s, Some(60_000));

    let out = view.ask("never-heard-of-it", T0);
    assert!(
        out.is_ok(),
        "absence of knowledge must not use the error channel"
    );
    assert!(matches!(
        out.expect("ok"),
        WorldLookup::Unknown(UnknownReason::NoClaim)
    ));
}

/// An expired claim is not served — and reports `NoClaim`, not `NoneAdmissible`.
///
/// Asserted as **observed**, not as designed. `current()` filters on `holds_at`
/// before this boundary sees anything, so expiry and never-heard-of-it are
/// indistinguishable here. Recorded rather than smoothed over: an operator whose
/// coverage merely lapsed is being sent to a coverage question rather than a
/// freshness one, and only the store can tell them apart.
///
/// Both sides asserted, so this pins the boundary rather than the exclusion
/// alone — a filter that excluded everything would pass a one-sided test.
#[test]
fn an_expired_claim_is_not_served() {
    let mut s = open("expired");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(
        &mut s,
        1,
        "cup-1",
        Some(T0 + 1_000),
        Some(&a),
        ClaimStatus::Confirmed,
    );

    let view = WorldView::new(&s, Some(60_000));
    assert!(matches!(
        view.ask("cup-1", T0 + 500).expect("ask"),
        WorldLookup::Answered(_)
    ));
    assert!(matches!(
        view.ask("cup-1", T0 + 5_000).expect("ask"),
        WorldLookup::Unknown(UnknownReason::NoClaim)
    ));
}

/// An inadmissible claim cannot reach this boundary at all.
///
/// The store-side guarantee (pinned in
/// `kirra-world-store/tests/inadmissible_never_read.rs`) seen from the consuming
/// end. Worth asserting on both sides: that file proves the store never emits
/// one, this proves the boundary never invents one on its own.
#[test]
fn an_inadmissible_claim_never_reaches_the_boundary() {
    let mut s = open("inadmissible");
    let rejected = axes(Corroboration::Contradicted(2), Adjudication::Rejected);
    seed(
        &mut s,
        1,
        "cup-1",
        None,
        Some(&rejected),
        ClaimStatus::Candidate,
    );

    let view = WorldView::new(&s, Some(60_000));
    assert!(matches!(
        view.ask("cup-1", T0).expect("ask"),
        WorldLookup::Unknown(UnknownReason::NoClaim)
    ));
}

// ---------------------------------------------------------------------------
// The staleness budget belongs to the caller
// ---------------------------------------------------------------------------

/// Two callers can disagree about the same claim at the same instant.
///
/// Which is why the budget is a parameter and not a column: a floor plan and a
/// person's location go stale at wildly different rates. Both are still
/// *served* — stale is a reportable state, not a refusal.
#[test]
fn the_staleness_budget_belongs_to_the_asking_caller() {
    let mut s = open("staleness");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(
        &mut s,
        1,
        "cup-1",
        Some(T0 + 10_000_000),
        Some(&a),
        ClaimStatus::Confirmed,
    );

    let later = T0 + 120_000;

    let impatient = WorldView::new(&s, Some(60_000));
    let WorldLookup::Answered(a1) = impatient.ask("cup-1", later).expect("ask") else {
        panic!("expected an answer");
    };
    assert_eq!(a1[0].validity(), Validity::Stale);

    let patient = WorldView::new(&s, Some(600_000));
    let WorldLookup::Answered(a2) = patient.ask("cup-1", later).expect("ask") else {
        panic!("expected an answer");
    };
    assert_eq!(a2[0].validity(), Validity::Fresh);

    // Same claim, same instant, different verdicts -- and the same provenance,
    // so the disagreement is about policy rather than about which claim was read.
    assert_eq!(a1[0].provenance(), a2[0].provenance());
}

// ---------------------------------------------------------------------------
// The provenance handle is a handle, not a string
// ---------------------------------------------------------------------------

/// **An unreadable provenance handle is refused, not served.**
///
/// The reasoning is this boundary's own: rule 1 says every answer carries a
/// `ProvenanceHandle`, so an answer whose handle cannot be parsed is one that
/// cannot be *cited* — serving it would break the rule this module exists to
/// enforce, while looking like an ordinary answer.
///
/// It surfaces as an **error** rather than as `Unknown`, and that split is rule
/// 3 read carefully. `Unknown` means *"nothing is known"*, which would be a
/// false statement here: something is known and it is unreadable. That is a
/// storage fault, which is the error channel's stated purpose.
///
/// The corruption has to be **planted with raw SQL** because the write path
/// will not produce one — which is itself worth noticing: this refusal guards
/// against a store damaged from outside, not against a bug in the append path.
#[test]
fn an_unreadable_provenance_handle_is_refused_rather_than_served() {
    let mut s = open("corrupt-handle");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);

    // Answerable to begin with -- so the refusal below is caused by the
    // corruption and not by the fixture never having worked.
    let view = WorldView::new(&s, Some(60_000));
    assert!(matches!(
        view.ask("cup-1", T0).expect("ask"),
        WorldLookup::Answered(_)
    ));

    const PLANTED: &str = "not-a-digest";
    s.raw_execute_for_test(&format!(
        "UPDATE world_current SET chain_digest = '{PLANTED}'"
    ))
    .expect("plant the corruption");

    let view = WorldView::new(&s, Some(60_000));
    match view.ask("cup-1", T0) {
        Err(AskError::CorruptProvenance {
            subject,
            predicate,
            cause,
        }) => {
            assert_eq!(subject, "cup-1", "names the row to go and look at");
            assert_eq!(
                predicate.as_deref(),
                Some("colour"),
                "and names it fully -- world_current is keyed by \
                 (subject, predicate_key), so a subject alone is not a row"
            );
            // Derived from the planted value rather than hardcoded, so the
            // assertion cannot drift from what was actually written.
            assert_eq!(
                cause,
                DigestError::WrongLength {
                    len: PLANTED.chars().count()
                }
            );
        }
        Err(other) => panic!("wrong error: {other}"),
        Ok(lookup) => panic!(
            "a claim with an uncitable handle was served as {lookup:?} -- \
             rule 1 says every answer carries a ProvenanceHandle"
        ),
    }
}

/// **The error names the damaged row, not merely the damaged subject.**
///
/// `world_current` is `PRIMARY KEY (subject, predicate_key)`, so one subject
/// holds one row per predicate — which is why [`WorldView::ask`] returns a
/// *list*. An error carrying only the subject would send an operator to a
/// subject that may have any number of rows, all but one of them fine, with
/// nothing to say which to inspect.
///
/// Here `cup-1` has two predicates and exactly one is corrupted. The test is
/// worth more than the assertion in the test above because it can distinguish
/// a correct answer from a lucky one: with a single predicate, an error that
/// named the wrong predicate — or none — would still pass.
#[test]
fn the_error_identifies_which_of_a_subject_s_rows_is_damaged() {
    let mut s = open("corrupt-which-row");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed_with_predicate(&mut s, 1, "cup-1", "colour", Some(&a));
    seed_with_predicate(&mut s, 2, "cup-1", "position", Some(&a));

    // Two rows, both answerable, so the refusal below is caused by the
    // corruption rather than by a fixture that never worked.
    let view = WorldView::new(&s, Some(60_000));
    let WorldLookup::Answered(before) = view.ask("cup-1", T0).expect("ask") else {
        panic!("expected answers");
    };
    assert_eq!(before.len(), 2, "one row per predicate");

    const PLANTED: &str = "not-a-digest";
    s.raw_execute_for_test(&format!(
        "UPDATE world_current SET chain_digest = '{PLANTED}' \
         WHERE subject = 'cup-1' AND predicate_key = 'position'"
    ))
    .expect("plant the corruption in ONE row");

    let view = WorldView::new(&s, Some(60_000));
    match view.ask("cup-1", T0) {
        Err(AskError::CorruptProvenance {
            subject, predicate, ..
        }) => {
            assert_eq!(subject, "cup-1");
            assert_eq!(
                predicate.as_deref(),
                Some("position"),
                "the DAMAGED row, not the intact sibling on the same subject"
            );
        }
        Err(other) => panic!("wrong error: {other}"),
        Ok(lookup) => panic!("a damaged row was served as {lookup:?}"),
    }
}

/// The rendered message carries the storage spelling of the key.
///
/// An operator repairing this queries `world_current`, where a claim with no
/// predicate is stored as `''` rather than as an absent value. A message that
/// said `None` would not be usable against the table it points at.
#[test]
fn the_message_renders_the_key_as_storage_spells_it() {
    let err = AskError::CorruptProvenance {
        subject: "cup-1".into(),
        predicate: None,
        cause: DigestError::WrongLength { len: 3 },
    };
    let shown = err.to_string();
    assert!(
        shown.contains("predicate_key=\"\""),
        "an absent predicate is stored as the empty string: {shown}"
    );
    assert!(shown.contains("subject=\"cup-1\""), "{shown}");
}

/// The same corruption does **not** read as `Unknown`.
///
/// Stated separately from the test above because the two failures are different
/// mistakes with different consequences. Returning `Unknown` would tell an
/// operator *"nothing is known about this subject"* when the truth is *"the
/// record is damaged"* — sending them to look for a coverage gap that does not
/// exist, while the damage goes unreported.
#[test]
fn a_corrupt_handle_is_not_reported_as_absence_of_knowledge() {
    let mut s = open("corrupt-not-unknown");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);
    s.raw_execute_for_test("UPDATE world_current SET chain_digest = ''")
        .expect("plant the corruption");

    let view = WorldView::new(&s, Some(60_000));
    let out = view.ask("cup-1", T0);
    assert!(
        !matches!(out, Ok(WorldLookup::Unknown(_))),
        "damage must not be reported as absence"
    );
    assert!(matches!(out, Err(AskError::CorruptProvenance { .. })));
}
