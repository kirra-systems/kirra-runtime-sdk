//! **The conformance corpus, and the controls that prove it is not decorative.**
//!
//! Two jobs, and the second is the one worth reading.
//!
//! # 1. Conformance
//!
//! For every declared reducer, the corpus rendering's digest must equal the
//! digest declared in [`kirra_world_store::semantics::SEMANTICS`]. Change a
//! reducer's behaviour and this reds, naming the rule and showing the rendering
//! that moved.
//!
//! # 2. Sensitivity — the part a corpus usually gets wrong
//!
//! A conformance corpus is only worth its declaration if it *discriminates*. A
//! corpus whose inputs all flow down the happy path produces the same rendering
//! under a mutated rule, so the digest holds, the test stays green, and the
//! version it pins tracks nothing. That failure is invisible: everything passes.
//!
//! So each rule carries a table of **behaviour variants** — deliberately
//! divergent reimplementations of the reducer, each flipping exactly one rule —
//! and every variant must render differently from the real fold over the *same*
//! corpus input. Each variant is a named, historically-real way the fold could
//! be wrong:
//!
//! * `generation_leads_valid_time` — the classic bitemporal bug, where a
//!   late-arriving event about the past overwrites the present.
//! * `no_tiebreak` — an incomplete order, which makes the fold input-order
//!   dependent and `rebuild_from_zero_equals_incremental` false.
//! * `last_observed_follows_head` — the regression `subject_fold_step`'s own
//!   docs record having shipped once.
//!
//! This is the standing form of *"change fold behaviour without changing the
//! version and check the gate catches it"*. Run as a one-off mutation that
//! answer expires the moment someone edits the corpus; run as a table it is
//! re-answered on every commit, per behavioural axis.
//!
//! **What this does NOT prove.** A variant table proves sensitivity to the axes
//! it names. An axis nobody thought of is not covered, and no test can close
//! that — which is why the source pin exists alongside, catching edits the
//! corpus is blind to.
//!
//! # The battery this was measured against
//!
//! Run against the real `projection::supersedes`, not a fixture, with the
//! generation-leads-valid-time flip applied to the shipped reducer:
//!
//! | Mutation | Rust conformance | `check_world_semantics.py` |
//! |---|---|---|
//! | flip the fold rule | **RED** (corpus digest moved) | **RED** (source pin moved) |
//! | …then re-pin BOTH digests, leave `version` at 1 | green | **RED** (digest moved at a fixed version) |
//! | …then bump to v2 and add a baseline row | green | green — and the end-to-end ref pin reds until the recorded set is re-pinned |
//!
//! The second row is the whole of box 3b. The Rust test cannot see it: re-pin
//! the digest and it is satisfied, with the version untouched and every
//! recorded reference now replaying under a rule it does not name. The third
//! row shows the bump genuinely reaching a reference rather than stopping at a
//! constant.
//!
//! The FIRST run of this file also found a real hole: `world_current_corpus`
//! had the backdated claim in the middle of the generation sequence, where the
//! mutated and real folds converge on the same final state. See that function's
//! docs — the control did its job before the corpus was ever declared correct.

use std::collections::BTreeMap;

use kirra_world::adjudication::IdentityAdjudication;
use kirra_world::entity::Lifecycle;
use kirra_world_store::entity_projection::{Contradiction, ProjectedEntity};
use kirra_world_store::lineage::{LineageEvent, PageBoundary, SelectedLineage};
use kirra_world_store::projection::ProjectedClaim;
use kirra_world_store::provenance_graph::{
    Branch, BranchContinuation, Carriers, CitationLookup, CitationResolution, DanglingReason,
    GraphOutcome, InMemoryCitations, NodeCitations, NotWalkedReason, ProvenanceNode,
    ProvenanceTree,
};
use kirra_world_store::semantics::{
    self, corpus_digest, corpus_rendering, entity_corpus, render_entities, render_subjects,
    render_world_current, subject_corpus, world_current_corpus, RuleId, SEMANTICS,
};
use kirra_world_store::subject_projection::{
    ProjectedSubject, SubjectKey, SubjectObservation, SummaryKind,
};
use kirra_world_store::ClaimStatus;

// ---------------------------------------------------------------------------
// 1. Conformance
// ---------------------------------------------------------------------------

/// **The declared digest must be the corpus's actual digest.**
///
/// The failure message is deliberately a procedure rather than a value: the
/// choice a developer faces here — re-pin, or bump and re-pin — is exactly the
/// choice 3b exists to force, and a message that only printed two hashes would
/// invite the wrong half of it.
///
/// Every rule is checked before the test fails, rather than stopping at the
/// first — a change that moved two reducers should report both, or the second
/// is discovered only after the first is fixed and re-run.
#[test]
fn every_declared_corpus_digest_matches_the_reducer() {
    let mut drifted = String::new();
    for spec in SEMANTICS {
        let actual = corpus_digest(spec.rule);
        if actual != spec.corpus_digest {
            drifted.push_str(&format!(
                "\n  {} (declared version {})\n    declared: {}\n    actual:   {}\n    rendering now:\n      {}\n",
                spec.rule.as_str(),
                spec.version,
                spec.corpus_digest,
                actual,
                corpus_rendering(spec.rule).replace('\u{1e}', "\n      "),
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "\n\nreducer behaviour no longer matches its declaration:\n{drifted}\n\
         If this was a DELIBERATE semantics change: bump `version` AND re-pin\n\
         `corpus_digest` in `semantics::SEMANTICS`, and add the new version's row\n\
         to `ci/world_semantics_baseline.json`. A recorded AnswerRef must refuse\n\
         to replay under the new rule rather than silently adopt it.\n\
         \n\
         If it was NOT deliberate, a reducer changed by accident.\n"
    );
}

/// A declaration whose digest was pasted from a different rule would satisfy the
/// test above only by coincidence, but a placeholder digest would not satisfy it
/// at all — so this guards the *shape*, which a copy-paste can preserve.
#[test]
fn no_declaration_carries_a_placeholder_digest() {
    for spec in SEMANTICS {
        for (field, value) in [
            ("corpus_digest", spec.corpus_digest),
            ("source_pin", spec.source_pin),
        ] {
            assert_eq!(
                value.len(),
                64,
                "{}.{field} is not a sha256",
                spec.rule.as_str()
            );
            assert!(
                value.chars().all(|c| c.is_ascii_hexdigit()),
                "{}.{field} is not hex",
                spec.rule.as_str()
            );
            assert_ne!(
                value,
                "0".repeat(64),
                "{}.{field} is still the placeholder",
                spec.rule.as_str()
            );
        }
        assert!(spec.version >= 1, "versions start at 1");
    }
}

// ---------------------------------------------------------------------------
// 2. Sensitivity controls
// ---------------------------------------------------------------------------

/// Assert a variant fold renders differently from the real one.
///
/// Takes the rendering rather than a digest so a failure can show what the
/// variant produced — a variant that renders identically is a *hole in the
/// corpus*, and the reader needs to see which rows failed to move.
fn assert_variant_is_caught(rule: RuleId, variant: &str, rendered: &str) {
    assert_ne!(
        rendered,
        corpus_rendering(rule),
        "\n\nthe `{}` corpus does NOT discriminate the `{variant}` variant.\n\
         \n\
         A reducer with this rule flipped folds the corpus to the same state, so\n\
         the declared version would stay green through that behaviour change.\n\
         Extend the corpus input until this axis is exercised.\n",
        rule.as_str()
    );
}

// --- world_current -------------------------------------------------------

/// Fold the claim corpus with a caller-supplied supersession rule.
fn fold_current_with(
    supersedes: impl Fn(&ProjectedClaim, &ProjectedClaim) -> bool,
    key: impl Fn(&ProjectedClaim) -> (String, String),
) -> BTreeMap<(String, String), ProjectedClaim> {
    let mut acc: BTreeMap<(String, String), ProjectedClaim> = BTreeMap::new();
    for claim in world_current_corpus() {
        let k = key(&claim);
        match acc.get(&k) {
            Some(held) if !supersedes(&claim, held) => {}
            _ => {
                acc.insert(k, claim);
            }
        }
    }
    acc
}

fn real_key(c: &ProjectedClaim) -> (String, String) {
    (c.subject.clone(), c.predicate.clone().unwrap_or_default())
}

/// Transaction order leading valid time: a backdated event overwrites the
/// present. This is the bug `projection::supersedes` names as the whole reason
/// valid time leads generation.
#[test]
fn the_claim_corpus_catches_generation_leading_valid_time() {
    let folded = fold_current_with(
        |i, h| (i.generation, i.valid_from_ms) > (h.generation, h.valid_from_ms),
        real_key,
    );
    assert_variant_is_caught(
        RuleId::WorldCurrentFold,
        "generation_leads_valid_time",
        &render_world_current(&folded),
    );
}

/// An order with no tiebreak is not total, and a fold under it depends on
/// arrival order.
#[test]
fn the_claim_corpus_catches_a_missing_tiebreak() {
    let folded = fold_current_with(|i, h| i.valid_from_ms > h.valid_from_ms, real_key);
    assert_variant_is_caught(
        RuleId::WorldCurrentFold,
        "no_tiebreak",
        &render_world_current(&folded),
    );
}

/// Keying on subject alone collapses distinct predicates into one slot.
#[test]
fn the_claim_corpus_catches_a_subject_only_key() {
    let folded = fold_current_with(
        |i, h| (i.valid_from_ms, i.generation) > (h.valid_from_ms, h.generation),
        |c| (c.subject.clone(), String::new()),
    );
    assert_variant_is_caught(
        RuleId::WorldCurrentFold,
        "subject_only_key",
        &render_world_current(&folded),
    );
}

/// Never superseding — a fold that silently stopped adopting anything after the
/// first claim per key.
#[test]
fn the_claim_corpus_catches_a_fold_that_never_supersedes() {
    let folded = fold_current_with(|_, _| false, real_key);
    assert_variant_is_caught(
        RuleId::WorldCurrentFold,
        "never_supersedes",
        &render_world_current(&folded),
    );
}

// --- entities ------------------------------------------------------------

/// Fold the identity corpus, optionally dropping one of two rules.
fn fold_entities_with(
    assert_overwrites: bool,
    create_from_consequence: bool,
) -> BTreeMap<String, ProjectedEntity> {
    let mut acc: BTreeMap<String, ProjectedEntity> = BTreeMap::new();
    for (generation, adjudication) in entity_corpus() {
        if let IdentityAdjudication::Assert(a) = &adjudication {
            let key = a.entity().as_str().to_owned();
            match acc.get_mut(&key) {
                None => {
                    acc.insert(
                        key,
                        ProjectedEntity {
                            entity: a.entity().clone(),
                            lifecycle: Lifecycle::Provisional,
                            contradiction: None,
                        },
                    );
                }
                Some(existing) => {
                    if assert_overwrites {
                        // The variant: re-asserting an existing id RESETS it
                        // instead of recording that two events each claim to
                        // have brought the same identity into being.
                        existing.lifecycle = Lifecycle::Provisional;
                    } else if existing.contradiction.is_none() {
                        existing.contradiction = Some(Contradiction {
                            held: existing.lifecycle.state(),
                            attempted: kirra_world::entity::LifecycleState::Provisional,
                            generation,
                        });
                    }
                }
            }
            continue;
        }

        for (entity, next) in adjudication.resulting_lifecycles() {
            let key = entity.as_str().to_owned();
            match acc.get_mut(&key) {
                None => {
                    if create_from_consequence {
                        acc.insert(
                            key,
                            ProjectedEntity {
                                entity,
                                lifecycle: next,
                                contradiction: None,
                            },
                        );
                    }
                }
                Some(held) => {
                    if held.contradiction.is_some() {
                        continue;
                    }
                    match held.lifecycle.advance_to(next.clone()) {
                        Ok(moved) => held.lifecycle = moved,
                        Err(_) => {
                            held.contradiction = Some(Contradiction {
                                held: held.lifecycle.state(),
                                attempted: next.state(),
                                generation,
                            });
                        }
                    }
                }
            }
        }
    }
    acc
}

/// The control that the variant harness itself is faithful.
///
/// Without this, a variant that diverged for an unrelated reason — a
/// transcription slip in the reimplementation — would still "pass" by rendering
/// differently, and the table would prove nothing about the axis it names. Both
/// flags at their real settings must reproduce the shipped fold exactly.
#[test]
fn the_entity_variant_harness_reproduces_the_real_fold() {
    assert_eq!(
        render_entities(&fold_entities_with(false, true)),
        corpus_rendering(RuleId::EntityFold),
        "the variant harness diverges from the real reducer with every rule at \
         its real setting — so any difference it reports is its own bug, not the \
         variant's"
    );
}

/// Re-asserting an existing id must be recorded as a contradiction, not
/// silently accepted as a reset.
#[test]
fn the_entity_corpus_catches_an_assert_that_overwrites() {
    assert_variant_is_caught(
        RuleId::EntityFold,
        "assert_overwrites",
        &render_entities(&fold_entities_with(true, true)),
    );
}

/// An entity first met in a consequence — a split's outputs — must be created
/// there, or the identity graph loses entities the log named.
#[test]
fn the_entity_corpus_catches_refusing_to_create_from_a_consequence() {
    assert_variant_is_caught(
        RuleId::EntityFold,
        "no_create_from_consequence",
        &render_entities(&fold_entities_with(false, false)),
    );
}

// --- subject summaries ---------------------------------------------------

/// Fold the subject corpus with selectable time-bound and head rules.
fn fold_subjects_with(
    last_observed_follows_head: bool,
    head_follows_time: bool,
    kind_in_key: bool,
) -> BTreeMap<SubjectKey, ProjectedSubject> {
    let mut acc: BTreeMap<SubjectKey, ProjectedSubject> = BTreeMap::new();
    for incoming in subject_corpus() {
        let kind = SummaryKind::from_event_column(incoming.subject_kind.as_deref())
            .expect("corpus kinds are valid");
        let key = if kind_in_key {
            (kind, incoming.subject.clone())
        } else {
            (SummaryKind::Unlabelled, incoming.subject.clone())
        };
        match acc.get_mut(&key) {
            None => {
                acc.insert(key, new_summary(kind, &incoming));
            }
            Some(held) => {
                held.observation_count += 1;
                held.first_observed_ms = held.first_observed_ms.min(incoming.txn_time_ms);
                if !last_observed_follows_head {
                    held.last_observed_ms = held.last_observed_ms.max(incoming.txn_time_ms);
                }

                let advances = if head_follows_time {
                    incoming.txn_time_ms > held.last_observed_ms
                } else {
                    incoming.generation > held.last_generation
                };
                if advances {
                    held.last_generation = incoming.generation;
                    held.provenance_head = incoming.chain_digest.clone();
                    held.last_event_id = incoming.event_id.clone();
                    if last_observed_follows_head {
                        held.last_observed_ms = incoming.txn_time_ms;
                    }
                }
            }
        }
    }
    acc
}

fn new_summary(kind: SummaryKind, o: &SubjectObservation) -> ProjectedSubject {
    ProjectedSubject {
        subject_kind: kind,
        subject: o.subject.clone(),
        first_observed_ms: o.txn_time_ms,
        last_observed_ms: o.txn_time_ms,
        provenance_head: o.chain_digest.clone(),
        observation_count: 1,
        last_generation: o.generation,
        last_event_id: o.event_id.clone(),
    }
}

/// The same faithfulness control as the entity harness.
#[test]
fn the_subject_variant_harness_reproduces_the_real_fold() {
    assert_eq!(
        render_subjects(&fold_subjects_with(false, false, true)),
        corpus_rendering(RuleId::SubjectSummaryFold),
        "the variant harness diverges from the real reducer at its real settings"
    );
}

/// Tying the time bound to the head regresses the maximum when a later
/// generation carries an earlier transaction time — the regression
/// `subject_fold_step` records having shipped once.
#[test]
fn the_subject_corpus_catches_last_observed_following_the_head() {
    assert_variant_is_caught(
        RuleId::SubjectSummaryFold,
        "last_observed_follows_head",
        &render_subjects(&fold_subjects_with(true, false, true)),
    );
}

/// A head chosen by a tie-breakable transaction time is not reproducible.
#[test]
fn the_subject_corpus_catches_a_head_chosen_by_time() {
    assert_variant_is_caught(
        RuleId::SubjectSummaryFold,
        "head_follows_time",
        &render_subjects(&fold_subjects_with(false, true, true)),
    );
}

/// Dropping the kind from the key merges summaries the vocabulary keeps apart.
#[test]
fn the_subject_corpus_catches_a_key_without_the_kind() {
    assert_variant_is_caught(
        RuleId::SubjectSummaryFold,
        "kind_not_in_key",
        &render_subjects(&fold_subjects_with(false, false, false)),
    );
}

// --- lineage_selection ---------------------------------------------------

/// Which behavioural axis a lineage variant flips.
///
/// One struct of switches rather than four near-identical copies of the rule:
/// a copy that drifted from the real selection would weaken every control at
/// once, silently, and the drift would be invisible because the controls only
/// ever assert *inequality*.
#[derive(Clone, Copy)]
struct LineageVariant {
    /// Skip the sort, keeping input order.
    unordered: bool,
    /// Ignore the `generation <= at_generation` bound.
    unbounded: bool,
    /// Ignore the subject filter.
    any_subject: bool,
    /// Treat the cursor as inclusive rather than exclusive.
    inclusive_cursor: bool,
    /// Report `More` whenever the page is exactly full, successor or not.
    eager_more: bool,
    /// Drop unconfirmed candidates, as the claim fold does.
    confirmed_only: bool,
}

impl LineageVariant {
    /// Every switch off — the real rule, reimplemented.
    fn faithful() -> Self {
        Self {
            unordered: false,
            unbounded: false,
            any_subject: false,
            inclusive_cursor: false,
            eager_more: false,
            confirmed_only: false,
        }
    }
}

/// Select with one axis flipped, and render it under every corpus query.
fn render_lineage_variant(v: LineageVariant) -> String {
    let mut out = String::new();
    for (label, subject, at_generation, page) in semantics::lineage_queries() {
        let mut selected: Vec<LineageEvent> = semantics::lineage_corpus()
            .into_iter()
            .filter(|e| v.any_subject || e.subject == subject)
            .filter(|e| v.unbounded || e.generation <= at_generation)
            .filter(|e| {
                page.after_generation().is_none_or(|a| {
                    if v.inclusive_cursor {
                        e.generation >= a
                    } else {
                        e.generation > a
                    }
                })
            })
            .filter(|e| !v.confirmed_only || e.claim_status == ClaimStatus::Confirmed)
            .collect();
        if !v.unordered {
            selected.sort_by_key(|e| e.generation);
        }

        let boundary =
            if selected.len() > page.limit() || (v.eager_more && selected.len() == page.limit()) {
                selected.truncate(page.limit());
                PageBoundary::More {
                    next_after_generation: selected.last().map_or(0, |e| e.generation),
                }
            } else {
                PageBoundary::Complete
            };

        out.push_str(label);
        out.push('\u{1f}');
        out.push_str(&semantics::render_lineage(&SelectedLineage {
            events: selected,
            boundary,
        }));
        out.push('\u{1e}');
    }
    out
}

/// **The faithfulness control.** The variant harness with every switch off must
/// reproduce the real rendering EXACTLY.
///
/// Without this, the five controls below would all pass against a harness that
/// had drifted from the real rule — they only assert that something differs, and
/// a harness wrong in some sixth way differs from the real rule for a reason
/// none of them names. This is what makes their inequality mean *"this axis"*
/// rather than merely *"something"*.
#[test]
fn the_lineage_variant_harness_reproduces_the_real_rule_when_faithful() {
    assert_eq!(
        render_lineage_variant(LineageVariant::faithful()),
        corpus_rendering(RuleId::LineageSelection),
        "the variant harness has drifted from `select_lineage`; every control \
         below is measuring the drift rather than the axis it names"
    );
}

/// Losing the sort makes the answer depend on the order rows came back in —
/// and a cursor over an unordered sequence cannot be resumed at all.
#[test]
fn the_lineage_corpus_catches_an_unordered_selection() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "unordered",
        &render_lineage_variant(LineageVariant {
            unordered: true,
            ..LineageVariant::faithful()
        }),
    );
}

/// **The historical-correctness axis.** Dropping the generation bound serves
/// evidence appended after the pinned coordinate — 2d's "resolve current state
/// and label it historical", one tier up.
#[test]
fn the_lineage_corpus_catches_ignoring_the_generation_bound() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "unbounded",
        &render_lineage_variant(LineageVariant {
            unbounded: true,
            ..LineageVariant::faithful()
        }),
    );
}

/// A lineage that ignores its subject is another subject's evidence.
#[test]
fn the_lineage_corpus_catches_ignoring_the_subject() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "any_subject",
        &render_lineage_variant(LineageVariant {
            any_subject: true,
            ..LineageVariant::faithful()
        }),
    );
}

/// An inclusive cursor repeats the previous page's last event on every page —
/// the kind of defect that looks like a duplicate rather than like a bug.
#[test]
fn the_lineage_corpus_catches_an_inclusive_cursor() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "inclusive_cursor",
        &render_lineage_variant(LineageVariant {
            inclusive_cursor: true,
            ..LineageVariant::faithful()
        }),
    );
}

/// **The off-by-one at the page boundary.** Reporting `More` because the page
/// filled — rather than because something follows — advertises a successor that
/// does not exist. The `exactly_full` corpus query is the row that catches it,
/// and it is in the corpus for this reason alone.
#[test]
fn the_lineage_corpus_catches_reporting_more_on_a_merely_full_page() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "eager_more",
        &render_lineage_variant(LineageVariant {
            eager_more: true,
            ..LineageVariant::faithful()
        }),
    );
}

/// Filtering to confirmed claims — as `world_current` legitimately does — hides
/// what an LLM proposed, which is the question an investigator is asking.
#[test]
fn the_lineage_corpus_catches_dropping_unconfirmed_candidates() {
    assert_variant_is_caught(
        RuleId::LineageSelection,
        "confirmed_only",
        &render_lineage_variant(LineageVariant {
            confirmed_only: true,
            ..LineageVariant::faithful()
        }),
    );
}

// ---------------------------------------------------------------------------
// Coverage of the control table itself
// ---------------------------------------------------------------------------

/// Every declared rule must carry at least one sensitivity control.
///
/// A rule whose corpus is never challenged is a corpus nobody has shown to
/// discriminate anything — which is the state this file exists to end, so the
/// obligation is machine-checked rather than left to whoever adds the next
/// reducer.
#[test]
fn every_rule_has_at_least_one_sensitivity_control() {
    // Kept as an explicit list rather than derived, so ADDING a rule to
    // `RuleId::all()` reds here until its controls are written.
    let controlled = [
        RuleId::WorldCurrentFold,
        RuleId::EntityFold,
        RuleId::SubjectSummaryFold,
        RuleId::LineageSelection,
        RuleId::CitationResolution,
    ];
    for rule in RuleId::all() {
        assert!(
            controlled.contains(rule),
            "{} has no sensitivity control — its corpus discriminates nothing \
             that has been demonstrated",
            rule.as_str()
        );
    }
    assert_eq!(controlled.len(), semantics::SEMANTICS.len());
}

// --- citation_resolution -------------------------------------------------

/// Which behavioural axis a citation-resolution variant flips.
///
/// The same one-struct-of-switches shape as [`LineageVariant`], for the same
/// reason: seven near-identical copies of a walk would drift apart, and the
/// controls only ever assert *inequality*, so the drift would never show.
///
/// Every switch here is a collapse the ruling names, or the mechanism that
/// prevents one. They are not hypothetical failure modes — each is the tidier
/// implementation someone reaches for first.
#[derive(Clone, Copy)]
struct CitationVariant {
    /// Resolve against every carrier, ignoring the pin — the graph that is
    /// resolvable *today* rather than the one that was resolvable *then*.
    unpinned: bool,
    /// Collapse several carriers to the newest.
    newest_wins: bool,
    /// Detect cycles by visitation rather than path membership, which makes a
    /// diamond look circular.
    visited_not_path: bool,
    /// Treat a generation below the coverage floor as having cited nothing.
    floor_ignored: bool,
    /// Read the surviving edges of an event that was compacted away.
    compacted_source_ignored: bool,
    /// Report every dangle as never-recorded, deleted evidence included.
    never_qualify_dangle: bool,
    /// Deduplicate a source's repeated citations.
    dedupe_citations: bool,
}

impl CitationVariant {
    /// Every switch off — the real rule, reimplemented.
    fn faithful() -> Self {
        Self {
            unpinned: false,
            newest_wins: false,
            visited_not_path: false,
            floor_ignored: false,
            compacted_source_ignored: false,
            never_qualify_dangle: false,
            dedupe_citations: false,
        }
    }
}

struct CitationWalk<'a> {
    store: &'a InMemoryCitations,
    at: i64,
    v: CitationVariant,
    max_depth: usize,
    max_nodes: usize,
    floor: i64,
    spans: Vec<(i64, i64)>,
    nodes: Vec<ProvenanceNode>,
    path: Vec<i64>,
    visited: Vec<i64>,
    outcome: GraphOutcome,
}

impl CitationWalk<'_> {
    fn expand(&mut self, generation: i64, depth: usize, parent: Option<usize>, via: Option<i64>) {
        let index = self.nodes.len();
        self.nodes.push(ProvenanceNode {
            generation,
            depth,
            parent,
            via_ordinal: via,
            citations: NodeCitations::BelowCoverageFloor,
        });
        self.visited.push(generation);
        let citations = self.citations_of(generation, depth);
        self.nodes[index].citations = citations;
    }

    fn citations_of(&mut self, generation: i64, depth: usize) -> NodeCitations {
        let retained = self.store.is_retained(generation).expect("infallible");
        if !retained && !self.v.compacted_source_ignored {
            self.outcome.degraded = true;
            return NodeCitations::EvidenceCompacted;
        }
        if generation <= self.floor && !self.v.floor_ignored {
            self.outcome.coverage_limited = true;
            return NodeCitations::BelowCoverageFloor;
        }
        let (edges, truncated) = self.store.citations(generation).expect("infallible");
        if truncated {
            self.outcome.truncated = true;
        }

        self.path.push(generation);
        let mut seen: Vec<String> = Vec::new();
        let mut branches = Vec::new();
        for edge in edges {
            if self.v.dedupe_citations {
                if seen.contains(&edge.cited_observation_id) {
                    continue;
                }
                seen.push(edge.cited_observation_id.clone());
            }
            let pin = if self.v.unpinned { i64::MAX } else { self.at };
            let carriers = self
                .store
                .carriers(&edge.cited_observation_id, pin)
                .expect("infallible");
            let resolution = self.resolve(&carriers, pin);
            if matches!(
                resolution,
                CitationResolution::Dangling {
                    reason: DanglingReason::PossiblyCompacted { .. }
                }
            ) {
                self.outcome.degraded = true;
            }
            let continuation = self.continue_into(&resolution, depth, edge.ordinal);
            branches.push(Branch {
                ordinal: edge.ordinal,
                cited_observation_id: edge.cited_observation_id,
                resolution,
                continuation,
            });
        }
        self.path.pop();
        NodeCitations::Indexed {
            branches,
            truncated,
        }
    }

    fn resolve(&self, carriers: &Carriers, pin: i64) -> CitationResolution {
        let mut visible: Vec<i64> = carriers
            .generations
            .iter()
            .copied()
            .filter(|g| *g <= pin)
            .collect();
        visible.sort_unstable();
        if visible.len() > 1 {
            if self.v.newest_wins {
                return CitationResolution::Resolved {
                    target_generation: *visible.last().expect("non-empty"),
                };
            }
            return CitationResolution::Plural {
                target_generations: visible,
                truncated: carriers.truncated,
            };
        }
        if let Some(only) = visible.first() {
            return CitationResolution::Resolved {
                target_generation: *only,
            };
        }
        let spans: Vec<i64> = self
            .spans
            .iter()
            .filter(|(lo, _)| *lo <= pin)
            .map(|(lo, _)| *lo)
            .collect();
        CitationResolution::Dangling {
            reason: if spans.is_empty() || self.v.never_qualify_dangle {
                DanglingReason::NeverVisible
            } else {
                DanglingReason::PossiblyCompacted { spans }
            },
        }
    }

    fn continue_into(
        &mut self,
        resolution: &CitationResolution,
        depth: usize,
        ordinal: i64,
    ) -> BranchContinuation {
        let target = match resolution {
            CitationResolution::Resolved { target_generation } => *target_generation,
            CitationResolution::Plural { .. } => {
                return BranchContinuation::NotWalked(NotWalkedReason::Plural)
            }
            CitationResolution::Dangling { .. } => {
                return BranchContinuation::NotWalked(NotWalkedReason::Nothing)
            }
        };
        let loops = if self.v.visited_not_path {
            self.visited.contains(&target)
        } else {
            self.path.contains(&target)
        };
        if loops {
            self.outcome.cycle_detected = true;
            return BranchContinuation::NotWalked(NotWalkedReason::CycleDetected {
                back_to_generation: target,
            });
        }
        if depth + 1 > self.max_depth {
            self.outcome.truncated = true;
            return BranchContinuation::NotWalked(NotWalkedReason::DepthLimit);
        }
        if self.nodes.len() >= self.max_nodes {
            self.outcome.truncated = true;
            return BranchContinuation::NotWalked(NotWalkedReason::NodeLimit);
        }
        let parent = self.nodes.len().saturating_sub(1);
        let node = self.nodes.len();
        self.expand(target, depth + 1, Some(parent), Some(ordinal));
        BranchContinuation::Walked { node }
    }
}

/// Walk with one axis flipped, and render it under every corpus walk.
fn render_citation_variant(v: CitationVariant) -> String {
    let store = semantics::citation_corpus();
    let floor = store.coverage_floor().expect("infallible");
    let mut out = String::new();
    for (label, root, at, max_depth, max_nodes) in semantics::citation_walks() {
        out.push_str(label);
        out.push('\u{1f}');
        if root <= floor && !v.floor_ignored {
            out.push_str(&format!("refused:index_incomplete:{root}:{floor}"));
        } else {
            let mut walk = CitationWalk {
                store: &store,
                at,
                v,
                max_depth,
                max_nodes,
                floor,
                spans: store.compacted_spans().expect("infallible"),
                nodes: Vec::new(),
                path: Vec::new(),
                visited: Vec::new(),
                outcome: GraphOutcome::default(),
            };
            walk.expand(root, 0, None, None);
            out.push_str(&semantics::render_provenance(&ProvenanceTree {
                root_generation: root,
                at_generation: at,
                nodes: walk.nodes,
                outcome: walk.outcome,
                rule_version: semantics::version_of(RuleId::CitationResolution),
            }));
        }
        out.push('\u{1e}');
    }
    out
}

/// **The faithfulness control**, doing the same job it does for lineage: without
/// it the seven controls below would all pass against a harness that had drifted
/// from the real walk, and their inequality would mean *"something differs"*
/// rather than *"this axis differs"*.
#[test]
fn the_citation_variant_harness_reproduces_the_real_rule_when_faithful() {
    assert_eq!(
        render_citation_variant(CitationVariant::faithful()),
        corpus_rendering(RuleId::CitationResolution),
        "the variant harness has drifted from `walk_provenance`; every control \
         below is measuring the drift rather than the axis it names"
    );
}

/// **The historical-correctness axis**, and the single most important control in
/// this file: resolving against every carrier rather than the visible ones is
/// exactly the collapse `KIRRA-WM-PROVENANCE-GRAPH-001` exists to forbid.
#[test]
fn the_citation_corpus_catches_resolving_against_the_present() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "unpinned",
        &render_citation_variant(CitationVariant {
            unpinned: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// Plural collapsed to the newest carrier names one event as the source of a
/// claim the store cannot attribute — and produces a tidier tree while doing it.
#[test]
fn the_citation_corpus_catches_newest_carrier_wins() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "newest_wins",
        &render_citation_variant(CitationVariant {
            newest_wins: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// Cycle detection by visitation reports a diamond — two claims resting on one
/// observation — as malformed provenance.
#[test]
fn the_citation_corpus_catches_visitation_used_as_cycle_detection() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "visited_not_path",
        &render_citation_variant(CitationVariant {
            visited_not_path: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// Box 4a's floor, consumed. Ignoring it turns *"the index makes no claim"* into
/// *"this source cited nothing"* — a positive claim about provenance, made
/// silently, about every source in an un-backfilled store.
#[test]
fn the_citation_corpus_catches_an_ignored_coverage_floor() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "floor_ignored",
        &render_citation_variant(CitationVariant {
            floor_ignored: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// Reading the edges of a compacted event is the index promoting itself to
/// evidence — a citation still readable after the hash-covered statement it came
/// from was deleted.
#[test]
fn the_citation_corpus_catches_reading_a_compacted_sources_edges() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "compacted_source_ignored",
        &render_citation_variant(CitationVariant {
            compacted_source_ignored: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// Reporting deleted evidence as never-recorded is §11.3's silent rewrite: an
/// investigation cannot tell *"nothing was known"* from *"we deleted it"*.
#[test]
fn the_citation_corpus_catches_an_unqualified_dangle() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "never_qualify_dangle",
        &render_citation_variant(CitationVariant {
            never_qualify_dangle: true,
            ..CitationVariant::faithful()
        }),
    );
}

/// A source citing the same observation twice said so twice. Deduplicating
/// describes a provenance array the hash does not cover.
#[test]
fn the_citation_corpus_catches_deduplicated_citations() {
    assert_variant_is_caught(
        RuleId::CitationResolution,
        "dedupe_citations",
        &render_citation_variant(CitationVariant {
            dedupe_citations: true,
            ..CitationVariant::faithful()
        }),
    );
}
