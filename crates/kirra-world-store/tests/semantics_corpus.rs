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
use kirra_world_store::projection::ProjectedClaim;
use kirra_world_store::semantics::{
    self, corpus_digest, corpus_rendering, entity_corpus, render_entities, render_subjects,
    render_world_current, subject_corpus, world_current_corpus, RuleId, SEMANTICS,
};
use kirra_world_store::subject_projection::{
    ProjectedSubject, SubjectKey, SubjectObservation, SummaryKind,
};

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
