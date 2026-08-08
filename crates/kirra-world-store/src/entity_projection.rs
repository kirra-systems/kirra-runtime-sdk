//! **Entity lifecycle as a projection over adjudication rows** — §6.3, ADR-0041.
//!
//! §6.3: *"A query at a past instant resolves identity as it was adjudicated
//! then — **because identity is a projection like everything else**."* This is
//! that projection. [`crate::adjudication_record`] put the judgements in the
//! log; this folds them into what each entity *is now*.
//!
//! # No DDL in the ratified surface, and no schema bump
//!
//! `WM2_EVENT_SCHEMA.md` §7, under *what this ruling does not decide*:
//!
//! > **Projection schemas.** `entities_projection` / `relationships_projection`
//! > are rebuildable views and follow from the fold, not from this table.
//!
//! So this table is named in the ratified document as a **rebuildable view**,
//! deliberately outside the schema surface. `SCHEMA_VERSION` is untouched.
//!
//! Like [`crate::projection::PROJECTIONS_V1`], the DDL is installed **by the
//! first fold, never at `open`** — and that is load-bearing rather than
//! stylistic. ADR-0041 **D-20**'s `log_only_bytes` is the on-disk size of a
//! store holding only the event log; creating projection tables at `open` adds
//! their root pages to *every* store, including one that never projects,
//! silently moving that figure and invalidating the D-2 comparison the
//! retention horizons rest on.
//!
//! # The reducer does not restate what a verb means
//!
//! [`fold_adjudication`] applies [`IdentityAdjudication::resulting_lifecycles`]
//! — the domain's own statement of each verb's consequences, already walked
//! against [`Lifecycle::advance_to`] from every live state by the adjudication
//! module's seam test. A second implementation here would agree today and drift
//! later; this cannot, because there is only one.
//!
//! # Contradiction poisons the ENTITY, not the fold
//!
//! Two individually valid events can contradict in aggregate: `a` merged into
//! `b` (terminal), then `a` merged into `c` by another operator. Neither event
//! can see the other, so no constructor refuses it — exactly the shape of
//! [`crate::resolution`]'s `RedirectCycle`, which is one instance of this same
//! problem.
//!
//! Three responses were available and two are worse:
//!
//! * **Fail the fold.** Not fail-closed — *fail-bricked*. One contradictory
//!   pair about one entity stops identity answers for **every** entity, and the
//!   log is append-only so the offending event cannot be removed: a rebuild
//!   replays it and wedges again. The blast radius is unbounded in the fault.
//! * **Skip the event.** Produces a projection that disagrees with the log while
//!   looking healthy — the "no projection-only fact" invariant inverted, and
//!   confidently wrong, which is the one outcome this store exists to prevent.
//! * **Poison the entity.** The blast radius matches the fault, nothing is
//!   invented (no winner is picked), and the caller learns at the point of
//!   asking about *that* entity. §14.4 makes it an **outcome**, not an error;
//!   §14.3's `ContradictionDetected` is the event it gives something to fire on.
//!
//! The third is taken. It also matches the precedent already set: `resolution`
//! answers a cyclic redirect per query rather than refusing to resolve anything.
//!
//! ## Poison is STICKY, and recovery needs a verb that does not exist
//!
//! Once contradicted, an entity stays contradicted no matter what follows, and
//! the **first** contradiction is the one retained.
//!
//! The justification is *diagnostic stability*, not determinism, and the
//! difference is worth stating because the first draft of this paragraph got it
//! wrong. It claimed stickiness is "what keeps rebuild-from-zero equal to an
//! incremental fold". It is not: the reducer is a pure function of the event
//! sequence either way, so a liftable poison would still replay identically
//! from any checkpoint. Running the negative control settled it — mutating
//! stickiness away leaves `folding_in_two_halves_equals_folding_at_once`
//! passing, and only `poison_is_sticky_and_keeps_the_first_contradiction` fails.
//!
//! What stickiness actually buys: the recorded contradiction names the **first**
//! disagreement, which is the one an operator needs, rather than whichever
//! happened last. And a contradicted entity is not advanced by later events,
//! because advancing from a state the projection has already declared untrusted
//! would be picking a winner one event at a time.
//!
//! Stated plainly because it is a real limitation and the obvious hope is
//! wrong: **no sequence of today's four verbs clears a contradiction.** There
//! is no "the second merge was mistaken" verb — `ForgetEntity` retires an
//! entity, it does not adjudicate between two prior claims. Resolution needs a
//! fifth verb and its own ruling. What this design buys is not easy recovery;
//! it is that the damage stays proportional to the fault while the evidence
//! stays intact for whoever writes that verb.

use std::collections::BTreeMap;

use kirra_world::adjudication::IdentityAdjudication;
use kirra_world::entity::{Lifecycle, LifecycleState};
use kirra_world::reference::EntityId;

/// The lazily-installed projection table. See the module docs for why this is
/// not schema DDL and why it is not installed at `open`.
pub const ENTITY_PROJECTION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS entities_projection (
    entity_id       TEXT    PRIMARY KEY,
    lifecycle       TEXT    NOT NULL,
    -- JSON array. One id for `merged`, N for `superseded`, absent otherwise.
    redirect        TEXT,
    -- The entity this one was split out of, for `split`.
    origin          TEXT,
    contradicted    INTEGER NOT NULL DEFAULT 0,
    -- The refused transition, as `from -> attempted @ generation`.
    contradiction   TEXT
);
"#;

/// A contradiction between two individually valid adjudications.
///
/// Carries the refused transition rather than a message, so an operator sees
/// *what* disagreed rather than that something did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contradiction {
    /// The state the entity was in.
    pub held: LifecycleState,
    /// The state an adjudication tried to move it to.
    pub attempted: LifecycleState,
    /// The generation of the event that tried.
    pub generation: i64,
}

/// One entity's projected state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedEntity {
    /// Which entity.
    pub entity: EntityId,
    /// Its lifecycle as of the folded prefix.
    ///
    /// When [`Self::contradiction`] is `Some`, this is the state held **before**
    /// the refused transition — the projection does not pick a winner.
    pub lifecycle: Lifecycle,
    /// The first contradiction seen, if any. Sticky.
    pub contradiction: Option<Contradiction>,
}

impl ProjectedEntity {
    /// Whether this entity's identity is self-contradictory.
    #[must_use]
    pub fn is_contradicted(&self) -> bool {
        self.contradiction.is_some()
    }
}

/// **The pure fold step.** Apply one adjudication to the accumulator.
///
/// `BTreeMap` rather than `HashMap` for the same reason
/// [`crate::projection::fold_all`] uses one: the state digest is taken over
/// iteration order, and a digest that depended on hash seeding would compare
/// unequal to itself.
///
/// # Ordering
///
/// Callers apply this in generation order, and `generation` is recorded on a
/// contradiction so the refusal names the event that caused it.
///
/// # An entity first seen in a consequence is created there
///
/// A merge may name a source this fold has never met — `AssertIdentity` is the
/// newest verb, and nothing requires a log to have used it. Creating the entity
/// directly in its resulting state keeps the redirect, which is the information
/// §6.3 promises to hold forever; refusing would discard it to enforce an
/// ordering the log never promised.
pub fn fold_adjudication(
    acc: &mut BTreeMap<String, ProjectedEntity>,
    adjudication: &IdentityAdjudication,
    generation: i64,
) {
    // `Assert` states no lifecycle consequence -- it CREATES. Handled here
    // because `resulting_lifecycles` is empty for it, deliberately: creation is
    // not a transition.
    if let IdentityAdjudication::Assert(a) = adjudication {
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
            // Asserting an id that already exists contradicts the mint's
            // never-reuse guarantee: two events each claim to have brought the
            // same identity into being.
            Some(existing) => {
                if existing.contradiction.is_none() {
                    existing.contradiction = Some(Contradiction {
                        held: existing.lifecycle.state(),
                        attempted: LifecycleState::Provisional,
                        generation,
                    });
                }
            }
        }
        return;
    }

    for (entity, next) in adjudication.resulting_lifecycles() {
        let key = entity.as_str().to_owned();
        match acc.get_mut(&key) {
            None => {
                acc.insert(
                    key,
                    ProjectedEntity {
                        entity,
                        lifecycle: next,
                        contradiction: None,
                    },
                );
            }
            Some(held) => {
                // Sticky: a contradicted entity is not advanced further, and
                // the FIRST contradiction is retained -- see the module docs for
                // why that is a diagnostic choice rather than a determinism one.
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

/// Fold a whole sequence of `(generation, adjudication)` in order.
#[must_use]
pub fn fold_all<'a, I>(adjudications: I) -> BTreeMap<String, ProjectedEntity>
where
    I: IntoIterator<Item = (i64, &'a IdentityAdjudication)>,
{
    let mut acc = BTreeMap::new();
    for (generation, a) in adjudications {
        fold_adjudication(&mut acc, a, generation);
    }
    acc
}

/// The stored token for a lifecycle state.
#[must_use]
pub fn lifecycle_token(l: &Lifecycle) -> &'static str {
    match l {
        Lifecycle::Provisional => "provisional",
        Lifecycle::Established => "established",
        Lifecycle::Dormant => "dormant",
        Lifecycle::Retired => "retired",
        Lifecycle::Merged { .. } => "merged",
        Lifecycle::Split { .. } => "split",
        Lifecycle::Superseded { .. } => "superseded",
    }
}

/// The successors a lifecycle redirects to, as stored JSON.
///
/// One id for `Merged`, N for `Superseded`, `None` otherwise — the shape
/// `resolution::resolve` walks.
#[must_use]
pub fn redirect_json(l: &Lifecycle) -> Option<String> {
    let ids: Vec<&str> = match l {
        Lifecycle::Merged { into } => vec![into.as_str()],
        Lifecycle::Superseded { by } => by.iter().map(EntityId::as_str).collect(),
        _ => return None,
    };
    Some(
        serde_json::Value::Array(
            ids.into_iter()
                .map(|s| serde_json::Value::String(s.to_owned()))
                .collect(),
        )
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_world::adjudication::{
        AssertIdentity, ForgetEntity, Justification, MergeEntities, RetirementReason, SplitEntity,
    };
    use kirra_world::observation::{ClockDomain, DomainInstant};
    use kirra_world::reference::ObservationId;

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).expect("entity id")
    }

    fn just() -> Justification {
        Justification::new([ObservationId::new("obs-1").expect("obs")]).expect("justification")
    }

    fn at() -> DomainInstant {
        DomainInstant {
            ms: 1,
            domain: ClockDomain::System,
        }
    }

    fn assert_id(e: &str) -> IdentityAdjudication {
        IdentityAdjudication::Assert(AssertIdentity::new(eid(e), just(), at()))
    }

    fn merge(sources: &[&str], into: &str) -> IdentityAdjudication {
        IdentityAdjudication::Merge(
            MergeEntities::new(
                sources.iter().map(|s| eid(s)).collect::<Vec<_>>(),
                eid(into),
                just(),
                at(),
            )
            .expect("merge"),
        )
    }

    fn partition(source: &str, into: &[&str]) -> IdentityAdjudication {
        IdentityAdjudication::Split(
            SplitEntity::partition(
                eid(source),
                into.iter().map(|s| eid(s)).collect::<Vec<_>>(),
                just(),
                at(),
            )
            .expect("partition"),
        )
    }

    fn forget(e: &str) -> IdentityAdjudication {
        IdentityAdjudication::Forget(ForgetEntity::new(
            eid(e),
            RetirementReason::new("decommissioned").expect("reason"),
            just(),
            at(),
        ))
    }

    fn get<'a>(acc: &'a BTreeMap<String, ProjectedEntity>, e: &str) -> &'a ProjectedEntity {
        acc.get(e).unwrap_or_else(|| panic!("no row for {e}"))
    }

    #[test]
    fn an_assertion_creates_a_provisional_entity() {
        let acc = fold_all([(1, &assert_id("e-1"))]);
        assert_eq!(get(&acc, "e-1").lifecycle, Lifecycle::Provisional);
        assert!(!get(&acc, "e-1").is_contradicted());
    }

    /// §6.3: the merged-away id keeps a row and answers with a redirect.
    #[test]
    fn a_merge_redirects_each_source_and_leaves_the_target_alone() {
        let acc = fold_all([
            (1, &assert_id("a")),
            (2, &assert_id("b")),
            (3, &assert_id("keep")),
            (4, &merge(&["a", "b"], "keep")),
        ]);
        assert_eq!(
            get(&acc, "a").lifecycle,
            Lifecycle::Merged { into: eid("keep") }
        );
        assert_eq!(
            get(&acc, "b").lifecycle,
            Lifecycle::Merged { into: eid("keep") }
        );
        assert_eq!(
            get(&acc, "keep").lifecycle,
            Lifecycle::Provisional,
            "a merge target takes no transition -- it is what the others became"
        );
    }

    #[test]
    fn a_partition_supersedes_the_source_and_marks_the_products() {
        let acc = fold_all([(1, &assert_id("p")), (2, &partition("p", &["p1", "p2"]))]);
        assert_eq!(
            get(&acc, "p").lifecycle,
            Lifecycle::Superseded {
                by: vec![eid("p1"), eid("p2")]
            }
        );
        assert_eq!(
            get(&acc, "p1").lifecycle,
            Lifecycle::Split { from: eid("p") }
        );
    }

    /// An entity first met in a consequence is created there rather than
    /// dropped — otherwise the redirect §6.3 promises would be discarded to
    /// enforce an ordering the log never promised.
    #[test]
    fn a_source_never_asserted_still_gets_its_redirect() {
        let acc = fold_all([(1, &merge(&["ghost"], "keep"))]);
        assert_eq!(
            get(&acc, "ghost").lifecycle,
            Lifecycle::Merged { into: eid("keep") }
        );
    }

    // -- Contradiction -------------------------------------------------

    /// **The case no constructor can refuse.** Two operators merge `a` into
    /// different targets; each event is individually valid.
    #[test]
    fn a_second_merge_of_the_same_source_contradicts_that_entity() {
        let acc = fold_all([
            (1, &assert_id("a")),
            (2, &merge(&["a"], "b")),
            (3, &merge(&["a"], "c")),
        ]);
        let a = get(&acc, "a");
        assert_eq!(
            a.contradiction,
            Some(Contradiction {
                held: LifecycleState::Merged,
                attempted: LifecycleState::Merged,
                generation: 3,
            })
        );
        assert_eq!(
            a.lifecycle,
            Lifecycle::Merged { into: eid("b") },
            "the projection keeps what it held and does NOT pick a winner"
        );
    }

    /// **The blast radius matches the fault.** Everything else still folds.
    #[test]
    fn a_contradicted_entity_does_not_poison_the_rest_of_the_fold() {
        let acc = fold_all([
            (1, &merge(&["a"], "b")),
            (2, &merge(&["a"], "c")),
            (3, &assert_id("unrelated")),
            (4, &forget("unrelated")),
        ]);
        assert!(get(&acc, "a").is_contradicted());
        assert!(!get(&acc, "unrelated").is_contradicted());
        assert_eq!(get(&acc, "unrelated").lifecycle, Lifecycle::Retired);
    }

    /// Sticky, and the FIRST contradiction is the one kept.
    ///
    /// This is the ONLY test that discriminates either half: the negative
    /// controls for "not sticky" and "keep the last one" both leave
    /// `folding_in_two_halves_equals_folding_at_once` passing, because the
    /// reducer stays deterministic under both. Recorded so nobody reads that
    /// test as covering this one.
    #[test]
    fn poison_is_sticky_and_keeps_the_first_contradiction() {
        let acc = fold_all([
            (1, &merge(&["a"], "b")),
            (2, &merge(&["a"], "c")),
            (3, &merge(&["a"], "d")),
            (4, &forget("a")),
        ]);
        let a = get(&acc, "a");
        assert_eq!(
            a.contradiction.as_ref().expect("contradicted").generation,
            2
        );
        assert_eq!(
            a.lifecycle,
            Lifecycle::Merged { into: eid("b") },
            "no later event advances a contradicted entity"
        );
    }

    /// Asserting an id twice contradicts the mint's never-reuse guarantee.
    #[test]
    fn asserting_the_same_id_twice_is_a_contradiction() {
        let acc = fold_all([(1, &assert_id("e-1")), (2, &assert_id("e-1"))]);
        assert!(get(&acc, "e-1").is_contradicted());
    }

    /// Retiring a merged entity is refused by `advance_to` (Merged is
    /// terminal), so it contradicts rather than silently retiring it.
    #[test]
    fn a_transition_out_of_a_terminal_state_contradicts() {
        let acc = fold_all([(1, &merge(&["a"], "b")), (2, &forget("a"))]);
        assert_eq!(
            get(&acc, "a").contradiction,
            Some(Contradiction {
                held: LifecycleState::Merged,
                attempted: LifecycleState::Retired,
                generation: 2,
            })
        );
    }

    // -- Determinism ---------------------------------------------------

    /// **Rebuild-from-zero equals incremental** — `WM_SCOPE` §0a's Knowledge
    /// tier invariant, at the level of the pure reducer.
    ///
    /// Folding a prefix and then the tail must equal folding the whole
    /// sequence, *including* when a contradiction falls across the split.
    ///
    /// **What this does and does not catch.** It is a split-point-independence
    /// guard: it would fail on a reducer whose result depended on where the
    /// checkpoint fell — an accumulator read that is not part of the state, or a
    /// nondeterministic iteration order. It does **not** discriminate the
    /// contradiction policy: every mutation tried against it (non-sticky poison,
    /// last-contradiction-wins, picking a winner) stays deterministic and leaves
    /// it green. Those are `poison_is_sticky_and_keeps_the_first_contradiction`'s
    /// job.
    #[test]
    fn folding_in_two_halves_equals_folding_at_once() {
        let events: Vec<(i64, IdentityAdjudication)> = vec![
            (1, assert_id("a")),
            (2, assert_id("b")),
            (3, merge(&["a"], "b")),
            (4, merge(&["a"], "c")),
            (5, partition("b", &["b1", "b2"])),
            (6, forget("b1")),
        ];

        let whole = fold_all(events.iter().map(|(g, a)| (*g, a)));
        for split in 0..=events.len() {
            let mut acc = fold_all(events[..split].iter().map(|(g, a)| (*g, a)));
            for (g, a) in &events[split..] {
                fold_adjudication(&mut acc, a, *g);
            }
            assert_eq!(acc, whole, "incremental fold split at {split} diverged");
        }
    }

    #[test]
    fn the_stored_redirect_is_the_shape_resolution_walks() {
        assert_eq!(
            redirect_json(&Lifecycle::Merged { into: eid("b") }).as_deref(),
            Some(r#"["b"]"#)
        );
        assert_eq!(
            redirect_json(&Lifecycle::Superseded {
                by: vec![eid("x"), eid("y")]
            })
            .as_deref(),
            Some(r#"["x","y"]"#)
        );
        assert_eq!(redirect_json(&Lifecycle::Established), None);
        assert_eq!(
            redirect_json(&Lifecycle::Split { from: eid("p") }),
            None,
            "an origin is not a redirect -- a split product is live and is itself"
        );
    }
}
