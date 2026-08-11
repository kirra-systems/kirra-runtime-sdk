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
    -- The refused transition, as JSON:
    --   {"held":..,"attempted":..,"generation":..}
    -- Structured, not a rendered sentence: a reload must return what the fold
    -- recorded rather than a placeholder. See `contradiction_json`.
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

// SEMANTICS-PIN-BEGIN: entity_fold
//
// Digested by `ci/check_world_semantics.py` and pinned in
// `semantics::SEMANTICS` — see `crate::semantics` for the two-check rationale.

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

// SEMANTICS-PIN-END: entity_fold

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

    /// **The digest distinguishes contradiction payloads, not just the flag.**
    ///
    /// Two projections that refuse the same entities but name different
    /// generations must not compare equal: the generation is what an operator
    /// uses to find the disagreeing event, so an invariant check blind to it
    /// would pass while pointing two stores at different history.
    #[test]
    fn the_digest_covers_the_contradiction_payload() {
        let base = fold_all([(1, &merge(&["a"], "b")), (2, &merge(&["a"], "c"))]);
        // Same entities, same lifecycles, same contradicted-ness -- only the
        // recorded generation differs.
        let mut shifted = base.clone();
        shifted.get_mut("a").expect("a").contradiction = Some(Contradiction {
            held: LifecycleState::Merged,
            attempted: LifecycleState::Merged,
            generation: 99,
        });

        assert_ne!(
            state_digest_of(&base),
            state_digest_of(&shifted),
            "a differing contradiction generation must change the digest"
        );
        // Non-vacuous: identical input digests identically.
        assert_eq!(state_digest_of(&base), state_digest_of(&base.clone()));
    }

    /// And it distinguishes a split's ORIGIN, which is otherwise invisible —
    /// `redirect_json` is `None` for `Split`, so a digest over redirect alone
    /// would rate two products of different parents equal.
    #[test]
    fn the_digest_covers_a_split_origin() {
        let mut a = BTreeMap::new();
        a.insert(
            "x".to_owned(),
            ProjectedEntity {
                entity: eid("x"),
                lifecycle: Lifecycle::Split { from: eid("p1") },
                contradiction: None,
            },
        );
        let mut b = a.clone();
        b.get_mut("x").expect("x").lifecycle = Lifecycle::Split { from: eid("p2") };
        assert_ne!(state_digest_of(&a), state_digest_of(&b));
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

/// The `projection_checkpoint` name this fold advances.
///
/// Its own checkpoint, not `world_current`'s: the two folds consume different
/// row sets and advance independently, and sharing a cursor would make one
/// fold's progress silently skip the other's input.
pub const ENTITY_PROJECTION: &str = "entities_projection";

/// Rebuild a [`Lifecycle`] from its three stored columns.
///
/// **Fail-closed.** Every mismatch is a corrupt row rather than a value to
/// repair: the columns are only ever written by [`fold_adjudication`], so a
/// `merged` row with no redirect means the file was edited underneath the
/// store, and inventing a target would be fabricating a redirect §6.3 says is
/// resolvable forever.
///
/// # Errors
///
/// [`EntityRowError`] naming which column disagreed with which token.
pub fn lifecycle_from_columns(
    token: &str,
    redirect: Option<&str>,
    origin: Option<&str>,
) -> Result<Lifecycle, EntityRowError> {
    let ids = |r: Option<&str>| -> Result<Vec<EntityId>, EntityRowError> {
        let raw = r.ok_or(EntityRowError::MissingRedirect)?;
        let parsed: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| EntityRowError::MalformedRedirect)?;
        let arr = parsed.as_array().ok_or(EntityRowError::MalformedRedirect)?;
        arr.iter()
            .map(|v| {
                v.as_str()
                    .ok_or(EntityRowError::MalformedRedirect)
                    .and_then(|s| EntityId::new(s).map_err(|_| EntityRowError::MalformedRedirect))
            })
            .collect()
    };
    match token {
        "provisional" => Ok(Lifecycle::Provisional),
        "established" => Ok(Lifecycle::Established),
        "dormant" => Ok(Lifecycle::Dormant),
        "retired" => Ok(Lifecycle::Retired),
        "merged" => {
            let v = ids(redirect)?;
            match v.len() {
                // Exactly one. A merge redirects to ONE thing -- that is the
                // whole distinction `Superseded` exists to keep visible.
                1 => Ok(Lifecycle::Merged {
                    into: v.into_iter().next().expect("len checked"),
                }),
                n => Err(EntityRowError::WrongRedirectArity {
                    token: "merged",
                    found: n,
                }),
            }
        }
        "superseded" => {
            let v = ids(redirect)?;
            if v.is_empty() {
                // The case `resolution` refuses outright: a supersession naming
                // nothing is unanswerable, and reading it back would recreate
                // the row that made an existing id report as absent.
                return Err(EntityRowError::WrongRedirectArity {
                    token: "superseded",
                    found: 0,
                });
            }
            Ok(Lifecycle::Superseded { by: v })
        }
        "split" => {
            let from = origin.ok_or(EntityRowError::MissingOrigin)?;
            Ok(Lifecycle::Split {
                from: EntityId::new(from).map_err(|_| EntityRowError::MalformedOrigin)?,
            })
        }
        _ => Err(EntityRowError::UnknownLifecycle {
            token: token.to_owned(),
        }),
    }
}

/// Why a stored projection row could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRowError {
    /// The lifecycle token is not one of the seven.
    UnknownLifecycle {
        /// The token found.
        token: String,
    },
    /// A redirecting state carried no redirect.
    MissingRedirect,
    /// The redirect column is not a JSON array of admissible ids.
    MalformedRedirect,
    /// The redirect arity disagrees with the state.
    WrongRedirectArity {
        /// Which state.
        token: &'static str,
        /// How many ids were found.
        found: usize,
    },
    /// A `split` row carried no origin.
    MissingOrigin,
    /// The origin is not an admissible entity id.
    MalformedOrigin,
    /// The contradiction column is not what the fold writes.
    MalformedContradiction,
}

impl core::fmt::Display for EntityRowError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownLifecycle { token } => write!(f, "unknown lifecycle token `{token}`"),
            Self::MissingRedirect => write!(f, "a redirecting lifecycle carried no redirect"),
            Self::MalformedRedirect => write!(f, "the redirect column is not a JSON id array"),
            Self::WrongRedirectArity { token, found } => {
                write!(f, "`{token}` cannot carry {found} successors")
            }
            Self::MissingOrigin => write!(f, "a split row carried no origin"),
            Self::MalformedOrigin => write!(f, "the origin is not an admissible entity id"),
            Self::MalformedContradiction => {
                write!(f, "the contradiction column is not what the fold writes")
            }
        }
    }
}

impl std::error::Error for EntityRowError {}

/// The origin of a `Split`, for the `origin` column.
#[must_use]
pub fn origin_of(l: &Lifecycle) -> Option<&EntityId> {
    match l {
        Lifecycle::Split { from } => Some(from),
        _ => None,
    }
}

/// The stored token for a [`LifecycleState`].
#[must_use]
pub fn state_token(s: LifecycleState) -> &'static str {
    match s {
        LifecycleState::Provisional => "provisional",
        LifecycleState::Established => "established",
        LifecycleState::Dormant => "dormant",
        LifecycleState::Retired => "retired",
        LifecycleState::Merged => "merged",
        LifecycleState::Split => "split",
        LifecycleState::Superseded => "superseded",
    }
}

fn state_from_token(t: &str) -> Option<LifecycleState> {
    Some(match t {
        "provisional" => LifecycleState::Provisional,
        "established" => LifecycleState::Established,
        "dormant" => LifecycleState::Dormant,
        "retired" => LifecycleState::Retired,
        "merged" => LifecycleState::Merged,
        "split" => LifecycleState::Split,
        "superseded" => LifecycleState::Superseded,
        _ => return None,
    })
}

/// A contradiction as stored JSON.
///
/// Structured rather than a rendered sentence, so a reload returns what the
/// fold recorded instead of a placeholder. An operator-facing sentence can be
/// rendered from this; the reverse cannot be done without inventing the fields.
#[must_use]
pub fn contradiction_json(c: &Contradiction) -> String {
    serde_json::json!({
        "held": state_token(c.held),
        "attempted": state_token(c.attempted),
        "generation": c.generation,
    })
    .to_string()
}

/// Read a stored contradiction back.
///
/// # Errors
///
/// [`EntityRowError::MalformedContradiction`] on anything the fold would not
/// have written — refused rather than defaulted, because a contradiction with
/// invented fields points an operator at the wrong event.
pub fn contradiction_from_json(raw: &str) -> Result<Contradiction, EntityRowError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| EntityRowError::MalformedContradiction)?;
    let o = v
        .as_object()
        .ok_or(EntityRowError::MalformedContradiction)?;
    let tok = |k: &str| -> Result<LifecycleState, EntityRowError> {
        o.get(k)
            .and_then(serde_json::Value::as_str)
            .and_then(state_from_token)
            .ok_or(EntityRowError::MalformedContradiction)
    };
    Ok(Contradiction {
        held: tok("held")?,
        attempted: tok("attempted")?,
        generation: o
            .get("generation")
            .and_then(serde_json::Value::as_i64)
            .ok_or(EntityRowError::MalformedContradiction)?,
    })
}

/// **A loaded snapshot of the entity projection, resolvable in place.**
///
/// The bridge that makes `kirra_world::resolution::resolve` live against a real
/// store — sub-slice 3 of the schema slice.
///
/// # Why a loaded view and not a `WorldStore` impl
///
/// [`AdjudicationGraph::lifecycle_of`] returns `Option<Lifecycle>` and has **no
/// error channel**. Implementing it directly on `WorldStore` would mean every query
/// doing I/O that can fail, with nowhere to report the failure — so a read
/// error would become `None`, and `None` means *"the graph has no such
/// entity"*. That reports an existing id as absent: exactly the bug
/// `EmptySupersession` was added to fix in the resolver, reintroduced one layer
/// down where the resolver cannot see it.
///
/// So the fallible work happens **once, at load**, and returns a `Result`:
/// [`crate::WorldStore::identity_view`] refuses a corrupt projection rather
/// than resolving over a partial one. What is left is a pure map, and the
/// trait's contract is honoured by construction rather than by remembering to.
///
/// A snapshot also means a resolution walk sees ONE state of the world
/// throughout — a per-query reader could observe a fold landing mid-walk and
/// answer from two different generations at once.
#[derive(Debug, Clone)]
pub struct IdentityView {
    rows: BTreeMap<String, ProjectedEntity>,
    generation: i64,
}

impl IdentityView {
    /// Build a view over already-loaded rows.
    #[must_use]
    pub fn new(rows: BTreeMap<String, ProjectedEntity>, generation: i64) -> Self {
        Self { rows, generation }
    }

    /// The fold generation this snapshot was taken at.
    ///
    /// Carried so an answer can say *which* state of the world it came from —
    /// §6.3's *"a query at a past instant resolves identity as it was
    /// adjudicated then"* needs the instant to be nameable.
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// How many entities the snapshot holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the snapshot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// One entity's projected state.
    #[must_use]
    pub fn get(&self, id: &EntityId) -> Option<&ProjectedEntity> {
        self.rows.get(id.as_str())
    }
}

impl kirra_world::resolution::AdjudicationGraph for IdentityView {
    fn lifecycle_of(&self, id: &EntityId) -> Option<Lifecycle> {
        self.rows.get(id.as_str()).map(|e| e.lifecycle.clone())
    }

    fn is_contradicted(&self, id: &EntityId) -> bool {
        self.rows
            .get(id.as_str())
            .is_some_and(ProjectedEntity::is_contradicted)
    }
}

/// **The identity graph as it stood at a past instant** — §6.3, Tier 2 box 2d.
///
/// §6.3: *"A query at a past instant resolves identity as it was adjudicated
/// then — **because identity is a projection like everything else**."* An
/// [`IdentityView`] is that projection folded over the whole log; this is the
/// same projection folded over a **prefix** of it.
///
/// # It restrains the GRAPH, it does not re-answer the question
///
/// The distinction is the whole slice, and the cheap wrong version is easy to
/// write: resolve against current state and label the answer with a timestamp.
/// That answers today's question and lies about when. What happens here instead
/// is that the fold is re-run over only the adjudications recorded by the
/// instant, and then [`kirra_world::resolution::resolve`] — the **same**
/// function, unmodified — walks the smaller graph.
///
/// So there is no historical resolver, no second set of outcome variants, and
/// no shortcut. `KIRRA-WM-TRANSITIVITY-001` disqualified union-find and
/// required that transitive resolution preserve the accepted path and refuse a
/// contradictory graph rather than repairing it; a second engine here would be
/// a second place for that to be got wrong. `resolve_at` is `resolve` over a
/// restricted graph, and that is the entire mechanism.
///
/// # Which of the two clocks this cuts on
///
/// **Transaction time** (`txn_time_ms`) — *what had been recorded by then* —
/// matching the `as_known_at_ms` axis of [`crate::WorldStore::as_of`].
///
/// Valid time is deliberately **not** consulted, and the reason is that an
/// adjudication is not a claim about the world that holds over an interval. It
/// is a judgement, and a judgement's effect on identity begins when it is
/// recorded. `MergeEntities` carries a `DomainInstant` for *when the operator
/// decided*; nothing in the model gives a merge an interval over which it
/// stops being true, and `valid_to_ms` on an adjudication row is always NULL
/// because [`crate::WorldStore::append_adjudication`] hardcodes it so. Filtering
/// on an axis the writer never populates would look rigorous and do nothing.
///
/// Stated rather than assumed because the store IS bitemporal and the omission
/// would otherwise read as an oversight: a genuine valid-time identity question
/// ("who did we think this was, for the period the robot was in bay 3") is a
/// different query and would need its own ruling.
pub struct HistoricalIdentityView {
    view: IdentityView,
    as_known_at_ms: i64,
    resolution: crate::compaction::Resolution,
}

impl HistoricalIdentityView {
    /// Build a historical view over an already-restricted fold.
    ///
    /// Private to the crate: the restriction is what makes this type honest,
    /// and a public constructor would let a caller build one from the *current*
    /// projection and label it with any instant it liked — the precise lie the
    /// type exists to prevent. [`crate::WorldStore::identity_view_at`] is the
    /// only way to obtain one.
    pub(crate) fn new(
        view: IdentityView,
        as_known_at_ms: i64,
        resolution: crate::compaction::Resolution,
    ) -> Self {
        Self {
            view,
            as_known_at_ms,
            resolution,
        }
    }

    /// The transaction-time instant this view was cut at.
    #[must_use]
    pub fn as_known_at_ms(&self) -> i64 {
        self.as_known_at_ms
    }

    /// Whether compaction could have removed an adjudication bearing on this
    /// answer. See [`crate::WorldStore::identity_view_at`] for why this is
    /// derived rather than asserted.
    #[must_use]
    pub fn resolution(&self) -> &crate::compaction::Resolution {
        &self.resolution
    }

    /// The restricted projection itself.
    #[must_use]
    pub fn view(&self) -> &IdentityView {
        &self.view
    }

    /// **Resolve an id against the graph as it stood at this instant.**
    ///
    /// Delegates to [`kirra_world::resolution::resolve`]. The answer carries
    /// the instant and the resolution alongside the outcome, so a caller cannot
    /// hold a historical answer without holding *when* it was true and whether
    /// anything was missing from it.
    #[must_use]
    pub fn resolve_at(&self, id: &EntityId) -> HistoricalAnswer {
        HistoricalAnswer {
            outcome: kirra_world::resolution::resolve(&self.view, id),
            as_known_at_ms: self.as_known_at_ms,
            resolution: self.resolution.clone(),
        }
    }
}

impl kirra_world::resolution::AdjudicationGraph for HistoricalIdentityView {
    fn lifecycle_of(&self, id: &EntityId) -> Option<Lifecycle> {
        self.view.lifecycle_of(id)
    }

    fn is_contradicted(&self, id: &EntityId) -> bool {
        self.view.is_contradicted(id)
    }
}

/// One historical identity answer: the outcome, when it was true, and whether
/// that is all of it.
///
/// The same shape as [`crate::compaction::TemporalAnswer`] and for the same
/// stated reason — a caller cannot obtain the outcome without the resolution
/// being in front of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalAnswer {
    /// What the id resolved to, from the restricted graph.
    ///
    /// The **same** four variants as a present-tense resolution. A historical
    /// query has no extra answers and no missing ones.
    pub outcome: kirra_world::resolution::ResolutionOutcome,
    /// The transaction-time instant the graph was cut at.
    pub as_known_at_ms: i64,
    /// Whether a compacted span could have borne on the answer.
    pub resolution: crate::compaction::Resolution,
}

impl HistoricalAnswer {
    /// Shorthand for [`crate::compaction::Resolution::is_degraded`].
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.resolution.is_degraded()
    }
}

/// A digest over a projection accumulator, in key order.
///
/// Takes the accumulator rather than reading the table, so the value written
/// into the checkpoint is the one the fold actually produced.
///
/// **The contradiction payload is included, not just the flag.** Digesting only
/// *whether* an entity is contradicted would let two projections that refuse
/// the same entities but name different generations compare equal — and the
/// generation is the field an operator uses to find the disagreeing event, so
/// the invariant check would pass while pointing two stores at different
/// history.
#[must_use]
pub fn state_digest_of(rows: &BTreeMap<String, ProjectedEntity>) -> String {
    let mut buf = String::new();
    for (id, e) in rows {
        buf.push_str(id);
        buf.push('\u{1f}');
        buf.push_str(lifecycle_token(&e.lifecycle));
        buf.push('\u{1f}');
        buf.push_str(redirect_json(&e.lifecycle).as_deref().unwrap_or(""));
        buf.push('\u{1f}');
        buf.push_str(origin_of(&e.lifecycle).map_or("", EntityId::as_str));
        buf.push('\u{1f}');
        buf.push_str(
            &e.contradiction
                .as_ref()
                .map(contradiction_json)
                .unwrap_or_default(),
        );
        buf.push('\u{1e}');
    }
    crate::sha256_hex(buf.as_bytes())
}
