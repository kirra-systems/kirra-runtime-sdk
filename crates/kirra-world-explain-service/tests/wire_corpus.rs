//! **The non-vacuity control, producer half — Tier 4 box 3b.**
//!
//! `crates/kirra-explain-types/wire_corpus/` holds one JSON file per
//! distinguishable state of the explanation wire. This suite proves those bytes
//! are **what the real producer emits**; `kirra-mick/tests/wire_corpus.rs`
//! proves the real renderer handles all of them. Composed, that is the
//! end-to-end property:
//!
//! ```text
//! real project_explanation ──▶ real serde_json ──▶ corpus/*.json
//!                                                       │
//!                                            (this suite pins the left arrow)
//!                                                       │
//!            corpus/*.json ──▶ real serde_json ──▶ real render_explanation
//!                                            (Mick's suite pins the right one)
//! ```
//!
//! # Why a corpus rather than one test that links both halves
//!
//! Because linking both halves is the thing the architecture forbids. The
//! producer may not depend on `kirra-mick` and Mick may not depend on
//! `kirra-world*`; a dev-dependency would satisfy neither gate honestly, and
//! `check_kirra_world_bidirectional_fence` says why in its own words — *"a dev
//! edge does not ship, but it is how a normal edge gets argued for later"*.
//!
//! The corpus is also the better artifact. A link-time test proves the two ends
//! agree; a checked-in corpus additionally makes the bytes REVIEWABLE — a
//! reader can open the file and see exactly what crosses the boundary, and
//! confirm for themselves that no coordinate is in it.
//!
//! # How the cases are built
//!
//! Both routes are real, and which one a case uses depends on what it needs:
//!
//! * **Through `dispatch`** — the cases a real store can be driven to: a folded
//!   claim, a cited chain, an unrecorded subject, and an unfolded store (whose
//!   lineage query is genuinely refused). These carry the actual server bytes.
//! * **Through `project_explanation` with real [`StoreLabels`]** — the states a
//!   real SQLite fixture cannot be coaxed into producing on demand: a plural
//!   citation, a cycle, a depth-limited walk, compacted evidence. The tree is
//!   constructed; the projection, the labels and the codec are the shipped
//!   ones, and the events the labels read are really in the store.
//!
//! The second route is the same reasoning box 4c.1's suite gives for its fake
//! label source, inverted: there the tree was under test, so the labels were
//! stubbed; here the WIRE is under test, so the labels are real and the tree is
//! the fixture.
//!
//! # Regenerating
//!
//! `KIRRA_EXPLAIN_CORPUS_UPDATE=1 cargo test -p kirra-world-explain-service
//! --test wire_corpus`. Review the diff: a change here is a change to what
//! crosses a process boundary.

use kirra_explain_types::{ExplainOutcome, ALL_SEMANTICS, EXPLAIN_CURRENT_SUBJECT_PATH};
use kirra_world_explain_service::service::dispatch;
use kirra_world_service::explain::project_explanation;
use kirra_world_service::explain_labels::StoreLabels;
use kirra_world_store::provenance_graph::{
    Branch, BranchContinuation, CitationResolution, DanglingReason, GraphOutcome, NodeCitations,
    NotWalkedReason, ProvenanceNode, ProvenanceTree,
};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kirra-explain-types/wire_corpus")
}

// ---------------------------------------------------------------------------
// Store fixtures
// ---------------------------------------------------------------------------

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-corpus-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

/// Append one event and return its generation.
fn append(s: &mut WorldStore, tag: &str, obs: &str, subject: &str, cites: &[&str]) -> i64 {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(obs.to_string()).expect("obs id");
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: cites,
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

// ---------------------------------------------------------------------------
// Tree fixtures — shapes a SQLite fixture cannot be driven to on demand
// ---------------------------------------------------------------------------

fn node(
    generation: i64,
    depth: usize,
    parent: Option<usize>,
    citations: NodeCitations,
) -> ProvenanceNode {
    ProvenanceNode {
        generation,
        depth,
        parent,
        via_ordinal: parent.map(|_| 0),
        citations,
    }
}

fn branch(cited: &str, resolution: CitationResolution, cont: BranchContinuation) -> Branch {
    Branch {
        ordinal: 0,
        cited_observation_id: cited.to_string(),
        resolution,
        continuation: cont,
    }
}

fn indexed(branches: Vec<Branch>, truncated: bool) -> NodeCitations {
    NodeCitations::Indexed {
        branches,
        truncated,
    }
}

fn tree(at: i64, nodes: Vec<ProvenanceNode>, outcome: GraphOutcome) -> ProvenanceTree {
    ProvenanceTree {
        root_generation: nodes.first().map_or(0, |n| n.generation),
        at_generation: at,
        nodes,
        outcome,
        rule_version: 1,
    }
}

fn outcome_flags(truncated: bool, cycle: bool, degraded: bool, coverage: bool) -> GraphOutcome {
    GraphOutcome {
        truncated,
        cycle_detected: cycle,
        degraded,
        coverage_limited: coverage,
    }
}

/// A store holding three folded claims, so a synthetic tree naming generations
/// 1..3 gets REAL labels rather than the deleted-claim substitute.
fn store_with_three_claims(name: &str) -> (WorldStore, std::path::PathBuf, [i64; 3]) {
    let path = tmp(name);
    let mut s = WorldStore::open(&path).expect("open");
    let g1 = append(&mut s, "a", "obs-a", "package_17", &[]);
    let g2 = append(&mut s, "b", "obs-b", "package_17", &["obs-a"]);
    let g3 = append(&mut s, "c", "obs-c", "pallet_4", &["obs-b"]);
    s.fold().expect("fold");
    (s, path, [g1, g2, g3])
}

/// Project a constructed tree through the REAL projection and REAL labels.
fn project(store: &WorldStore, t: &ProvenanceTree, head: i64) -> ExplainOutcome {
    let labels = StoreLabels::new(store);
    ExplainOutcome::Explained {
        explanation: project_explanation(t, &labels, head).expect("projection"),
    }
}

/// Ask the real route table, and decode what it actually wrote.
fn via_dispatch(store: &WorldStore, subject: &str) -> ExplainOutcome {
    let body = format!("{{\"subject_id\":\"{subject}\"}}");
    let res = dispatch(store, "POST", EXPLAIN_CURRENT_SUBJECT_PATH, body.as_bytes());
    serde_json::from_str(&res.body).expect("the producer's own body decodes")
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

fn cases() -> Vec<(&'static str, ExplainOutcome)> {
    let mut out: Vec<(&'static str, ExplainOutcome)> = Vec::new();

    // --- through the real route table -------------------------------------
    {
        let (s, _p, _g) = store_with_three_claims("dispatch");
        // A claim that cites another: resolved branch, walked continuation.
        out.push(("current_cited_chain", via_dispatch(&s, "pallet_4")));
        // Nothing retained about the subject.
        out.push(("nothing_recorded", via_dispatch(&s, "no_such_subject")));
    }
    {
        // An UNFOLDED store. Worth a corpus entry because the answer is not the
        // one it looks like it should be: appended-but-unfolded comes back
        // `NothingRecorded`, not a refusal, because the explanation pins to the
        // PROJECTED coordinate and at that coordinate nothing is recorded yet.
        // Pinned here so a change to that behaviour has to be deliberate.
        let path = tmp("unfolded");
        let mut s = WorldStore::open(&path).expect("open");
        append(&mut s, "u", "obs-u", "package_17", &[]);
        out.push(("unfolded_store", via_dispatch(&s, "package_17")));
    }

    // `Unavailable`, through the real error-to-wire conversion.
    //
    // HONEST SCOPE, because this is the one case the route above cannot
    // produce: a store failure is not provocable in-process. Clobbering the
    // SQLite file underneath an open handle was tried and the page cache
    // serves the request anyway, so a "corrupted store" fixture would assert
    // that corruption is undetectable rather than that failure is reported.
    //
    // So the case is built from a real `ExplainError` through
    // `ExplainOutcome::unavailable`, which is the exact expression the
    // service's error arm evaluates. What is NOT proven here is the arm's
    // SELECTION — and that half is covered where it can be provoked for real:
    // `kirra_mick::explain_client` fails an unreachable producer closed to this
    // same variant, with nothing listening on port 1.
    out.push((
        "unavailable",
        ExplainOutcome::unavailable(kirra_world_explain_service::ExplainError::LineageRefused),
    ));

    // --- through the real projection, with real labels ---------------------
    let (s, _p, [g1, g2, g3]) = store_with_three_claims("projection");
    let head = g3;

    // Historical (pinned behind the head) + a plural citation the walk would
    // not expand.
    out.push((
        "historical_plural_citation",
        project(
            &s,
            &tree(
                g2,
                vec![node(
                    g2,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-a",
                            CitationResolution::Plural {
                                target_generations: vec![g1, g2],
                                truncated: false,
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::Plural),
                        )],
                        false,
                    ),
                )],
                outcome_flags(false, false, false, false),
            ),
            head,
        ),
    ));

    // A citation nothing ever carried.
    out.push((
        "dangling_never_recorded",
        project(
            &s,
            &tree(
                g2,
                vec![node(
                    g2,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-missing",
                            CitationResolution::Dangling {
                                reason: DanglingReason::NeverVisible,
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::Nothing),
                        )],
                        false,
                    ),
                )],
                outcome_flags(false, false, false, false),
            ),
            head,
        ),
    ));

    // A citation a compacted window could have carried — degraded, and the
    // renderer must not narrate it as "never recorded".
    out.push((
        "dangling_possibly_deleted",
        project(
            &s,
            &tree(
                g2,
                vec![node(
                    g2,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-gone",
                            CitationResolution::Dangling {
                                reason: DanglingReason::PossiblyCompacted {
                                    spans: vec![g1],
                                    truncated: false,
                                },
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::Nothing),
                        )],
                        false,
                    ),
                )],
                outcome_flags(false, false, true, false),
            ),
            head,
        ),
    ));

    // A legitimate bound, and MORE citations than one page admits.
    out.push((
        "truncated_at_depth_with_more_citations",
        project(
            &s,
            &tree(
                g3,
                vec![node(
                    g3,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-b",
                            CitationResolution::Resolved {
                                target_generation: g2,
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::DepthLimit),
                        )],
                        true,
                    ),
                )],
                outcome_flags(true, false, false, false),
            ),
            head,
        ),
    ));

    // The other bound.
    out.push((
        "truncated_at_node_limit",
        project(
            &s,
            &tree(
                g3,
                vec![node(
                    g3,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-b",
                            CitationResolution::Resolved {
                                target_generation: g2,
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::NodeLimit),
                        )],
                        false,
                    ),
                )],
                outcome_flags(true, false, false, false),
            ),
            head,
        ),
    ));

    // Circular evidence. NOT a bound: raising a limit cannot help, and the
    // renderer's obligation is to say malformed rather than truncated.
    out.push((
        "cycle_detected",
        project(
            &s,
            &tree(
                g2,
                vec![node(
                    g2,
                    0,
                    None,
                    indexed(
                        vec![branch(
                            "obs-b",
                            CitationResolution::Resolved {
                                target_generation: g2,
                            },
                            BranchContinuation::NotWalked(NotWalkedReason::CycleDetected {
                                back_to_generation: g2,
                            }),
                        )],
                        false,
                    ),
                )],
                outcome_flags(false, true, false, false),
            ),
            head,
        ),
    ));

    // The event itself is gone: its statement AND its edges went with it.
    out.push((
        "evidence_deleted_by_compaction",
        project(
            &s,
            &tree(
                g2,
                vec![
                    node(
                        g2,
                        0,
                        None,
                        indexed(
                            vec![branch(
                                "obs-a",
                                CitationResolution::Resolved {
                                    target_generation: g1,
                                },
                                BranchContinuation::Walked { node: 1 },
                            )],
                            false,
                        ),
                    ),
                    node(g1, 1, Some(0), NodeCitations::EvidenceCompacted),
                ],
                outcome_flags(false, false, true, false),
            ),
            head,
        ),
    ));

    // The index makes no claim about this generation. Nothing was deleted — a
    // backfill fixes it — and saying "deleted" would be a different fact.
    out.push((
        "citations_not_indexed",
        project(
            &s,
            &tree(
                g2,
                vec![node(g2, 0, None, NodeCitations::BelowCoverageFloor)],
                outcome_flags(false, false, false, true),
            ),
            head,
        ),
    ));

    out
}

// ---------------------------------------------------------------------------
// The two obligations
// ---------------------------------------------------------------------------

/// **The corpus on disk is what the real producer emits.**
///
/// Without this the corpus is a museum: Mick's suite would keep rendering an
/// artifact shape the producer stopped writing years ago and would keep
/// passing. With it, a change to the projection, the labels or the codec shows
/// up here as a diff in the bytes that cross the boundary.
#[test]
fn the_corpus_is_what_the_producer_actually_emits() {
    let dir = corpus_dir();
    let update = std::env::var("KIRRA_EXPLAIN_CORPUS_UPDATE").is_ok();
    if update {
        std::fs::create_dir_all(&dir).expect("corpus dir");
    }
    let mut written = std::collections::BTreeSet::new();

    for (name, outcome) in cases() {
        let json = serde_json::to_string_pretty(&outcome).expect("encodes") + "\n";
        let path = dir.join(format!("{name}.json"));
        written.insert(format!("{name}.json"));
        if update {
            std::fs::write(&path, &json).expect("write corpus entry");
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "corpus entry {name}.json is missing ({e}) — regenerate with \
                 KIRRA_EXPLAIN_CORPUS_UPDATE=1"
            )
        });
        assert_eq!(
            on_disk, json,
            "corpus entry {name}.json is stale. What crosses the process \
             boundary changed; review the diff, then regenerate with \
             KIRRA_EXPLAIN_CORPUS_UPDATE=1"
        );
    }

    // An ORPHAN file would silently widen what Mick's suite renders without any
    // producer ever emitting it — a case that proves nothing about this system.
    let on_disk: std::collections::BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("corpus dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    assert_eq!(
        on_disk, written,
        "the corpus directory and the generator disagree about which cases exist"
    );
}

/// **The corpus spans the input space it claims to cover.**
///
/// A control that exists is not a control that covers, which is the lesson the
/// dotted-key gate bypass taught at some cost: the check was real, its fixture
/// was not representative, and four of six spellings walked past it.
///
/// So the coverage is asserted against [`ALL_SEMANTICS`] — one enumeration,
/// defined beside the types, built from exhaustive matches so a NEW variant is
/// a compile error rather than a quiet hole. If a state cannot be produced, the
/// corpus is short and this fails by name.
#[test]
fn the_corpus_spans_every_distinguishable_semantic() {
    let mut seen = std::collections::BTreeSet::new();
    for (_, outcome) in cases() {
        seen.extend(outcome.semantics());
    }
    let missing: Vec<&&str> = ALL_SEMANTICS
        .iter()
        .filter(|s| !seen.contains(*s))
        .collect();
    assert!(
        missing.is_empty(),
        "the corpus does not exercise: {missing:?} — a renderer obligation for \
         these states would be tested against nothing"
    );
    let unknown: Vec<&&str> = seen.iter().filter(|s| !ALL_SEMANTICS.contains(s)).collect();
    assert!(
        unknown.is_empty(),
        "the corpus produced semantics not in ALL_SEMANTICS: {unknown:?}"
    );
}
