//! **Resolving an identifier through the adjudication graph** — §6.3, §14.2,
//! §14.4.
//!
//! §6.3 states the guarantee and, in the same breath, the cost:
//!
//! > *"Both original IDs remain resolvable forever and answer with a
//! > redirect. […] Cost, stated honestly: identity queries need an indirection
//! > through the merge graph, and the merge graph grows."*
//!
//! [`mod@crate::adjudication`] records the events and [`mod@crate::entity`]
//! holds the states they produce, but until now nothing *followed* the
//! redirects. [`Entity::redirects_to`] and [`Entity::superseded_by`] are
//! single-hop accessors: they answer "where does this one point", not "what is
//! this id, now". An entity merged twice needs the second question, and every
//! caller writing its own loop would be a place the cycle check is missing.
//!
//! [`Entity::redirects_to`]: crate::entity::Entity::redirects_to
//! [`Entity::superseded_by`]: crate::entity::Entity::superseded_by
//!
//! # The four answers, and why not `Option`
//!
//! [`ResolutionOutcome`] fills the placeholder `kirra_world::ResolutionOutcome`
//! carried since the crate was scaffolded, and takes the shape that placeholder
//! already argued for: **`Option::None` collapses "we looked and it is not
//! there", "we could not look", and "we looked and are not sure" into one
//! value** — three answers a caller must treat differently, and the distinction
//! is unrecoverable once lost.
//!
//! Those three, plus the answer itself, are the four variants. They line up
//! with §14.2's `WhereIs -> Located | Unknown | Ambiguous` and with §14.4's
//! sharp separation of **outcomes** from **errors**: all four are *outcomes*,
//! including [`ResolutionOutcome::Refused`]. A malformed graph is not an
//! exception for somebody to catch and turn into a default value.
//!
//! # Why `Ambiguous` exists at all
//!
//! Because of `KIRRA-WM-SPLIT-SURVIVAL-001`. A merged id redirects to one
//! thing; a partitioned id redirects to *N*, and a resolver that had to pick
//! one would be inventing the answer. §6.3 promised resolvability, not
//! singularity — and the ruling's whole argument for a separate
//! [`Lifecycle::Superseded`] state was that collapsing it into `Merged` would
//! make this distinction invisible at the call site. This is that call site.
//!
//! [`Lifecycle::Superseded`]: crate::entity::Lifecycle::Superseded
//!
//! # What this module does NOT do
//!
//! It does not do candidate clustering — matching an observation to an entity.
//! That is the other half of `WM_SCOPE.md`'s entity-resolution box, it is
//! marked *pure* by §6.3's pipeline, and `KIRRA-WM-CANDIDATE-ID-001` constrains
//! how its membership may be keyed. Nothing here mints, matches or scores; this
//! walks judgements already recorded.

use crate::entity::Lifecycle;
use crate::reference::EntityId;

/// The most redirect edges a resolution will follow, in total, before refusing.
///
/// A backstop, not a modelling claim: a healthy merge graph is a handful of
/// hops, and anything near this says the graph is degenerate rather than deep.
///
/// # Total edges, not depth — and the name says so for a reason
///
/// This was called `MAX_REDIRECT_DEPTH` while the check counted every edge the
/// walk traversed. The two differ exactly where it matters: a **wide** partition
/// is shallow but spends the budget just as a long chain does, so a name
/// promising a depth bound would have made a legitimate 65-way split look like a
/// pathologically deep one in the refusal it produced.
///
/// The total-edge bound is the one worth having — it bounds the *work*, which a
/// depth bound does not — so the name moved to match the check rather than the
/// reverse. It still bounds the recursion in [`resolve`] as a consequence:
/// nesting cannot exceed edges traversed, so stack depth is bounded by a
/// constant no caller can raise.
pub const MAX_REDIRECT_EDGES: usize = 64;

/// What the adjudication history says an identifier is now.
///
/// See the module docs for why this is four variants and not `Option`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolutionOutcome {
    /// **One live entity.** The id either was never redirected, or every
    /// redirect from it converged on this one.
    Located {
        /// What the id resolves to. Equal to the queried id when nothing
        /// redirected it.
        entity: EntityId,
        /// Redirect edges traversed to answer — §6.3's *"the merge graph
        /// grows"*, made observable.
        ///
        /// Edges **traversed**, not the length of the winning path: a fan-out
        /// that reconverges walks every branch, and reporting only the shortest
        /// would understate exactly the cost this field exists to expose.
        /// `0` means nothing was followed.
        hops: usize,
    },
    /// **The id was partitioned and its successors did not reconverge.**
    ///
    /// §14.2's third `WhereIs` return value. Every successor is itself fully
    /// resolved, so none of these is a dead id the caller must walk again, and
    /// they are ordered as the recorded events name them.
    ///
    /// Never empty and never one — a single survivor is [`Self::Located`],
    /// because "ambiguous between one thing" is not an ambiguity.
    Ambiguous {
        /// The live entities the id turned out to be.
        successors: Vec<EntityId>,
    },
    /// **We looked, and the graph does not contain this id.**
    ///
    /// Distinct from `Refused`: nothing is wrong, the answer is that there is
    /// no such entity.
    Unknown,
    /// **We could not answer, and this is why.** §14.4's `Refused(reason)`.
    ///
    /// An outcome rather than an error, deliberately: each reason below means
    /// the recorded history contradicts itself, which is a fact about the data
    /// the caller should see, not a fault to unwrap.
    Refused(RefusalReason),
}

/// Why a resolution refused to answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// A redirect leads back to an id already on the path being walked.
    ///
    /// **Not preventable at construction**, which is why it is checked here.
    /// `MergeEntities` refuses a merge into one of its own sources, so no
    /// single event can build a cycle — but *a* merged into *b* and, later, *b*
    /// merged into *a* are two individually valid events, and neither can see
    /// the other. The contradiction exists only in the accumulated graph.
    RedirectCycle {
        /// The id the walk arrived back at.
        at: EntityId,
    },
    /// The walk exceeded [`MAX_REDIRECT_EDGES`].
    TraversalBudgetExceeded {
        /// The bound that was hit.
        limit: usize,
    },
    /// A recorded redirect names an entity the graph does not have.
    ///
    /// A merge or split event named this successor, so something recorded it;
    /// its absence means the history is incomplete. Answering `Unknown` here
    /// would report "no such entity" for a *question that was asked about a
    /// different id* — the queried id exists, and its redirect is broken.
    DanglingRedirect {
        /// The entity holding the redirect.
        from: EntityId,
        /// The successor it names, which is missing.
        to: EntityId,
    },
    /// A [`Lifecycle::Superseded`] carries **no** successors.
    ///
    /// [`Lifecycle::Superseded`]: crate::entity::Lifecycle::Superseded
    ///
    /// The type's own documentation says `by` is *"never empty and never one"*,
    /// and through `SplitEntity::partition` it cannot be — but `Lifecycle` is a
    /// plain public enum, so `Superseded { by: Vec::new() }` is constructible,
    /// and a store decoding a corrupt row can produce one. **Prose held the
    /// invariant; the type did not.**
    ///
    /// Refused rather than resolved because it is *unanswerable*: the id was
    /// superseded, so it is not itself, and it names nothing it became. Without
    /// this arm the walk finds no successors, collects no answers, and reports
    /// `Unknown` — "no such entity" about an id that **exists**, which is the
    /// exact confusion [`Self::DanglingRedirect`] was introduced to prevent.
    ///
    /// # Why a ONE-element list is not refused here
    ///
    /// A single-successor supersession violates the same documented invariant,
    /// but it is unambiguously *answerable*: it resolves to that successor, and
    /// refusing would discard a correct answer to the question asked. The
    /// asymmetry is deliberate — this module answers identity questions from
    /// recorded history and is not the schema validator. Rejecting a malformed
    /// row belongs at the decoding boundary, fail-closed, where the store can
    /// refuse it before it is ever walked.
    EmptySupersession {
        /// The entity whose supersession names nothing.
        at: EntityId,
    },
    /// **The recorded history for this entity contradicts itself.**
    ///
    /// Two individually valid adjudications disagreed in aggregate — `a` merged
    /// into `b`, then `a` merged into `c` by another operator — so no
    /// constructor could refuse either, and the projection that folded them
    /// declined to pick a winner.
    ///
    /// A sibling of [`Self::RedirectCycle`] rather than a different kind of
    /// thing: a cycle *is* one instance of this fault, caught by the walk
    /// instead of by the fold. Both are answered per query rather than by
    /// refusing to resolve anything, which is what keeps the damage
    /// proportional to the contradiction.
    ///
    /// Reported for an entity reached **through** a redirect as much as for one
    /// queried directly: an answer that routed through a contradicted identity
    /// is not trustworthy just because the question named something else.
    ContradictoryHistory {
        /// The entity whose history disagrees with itself.
        at: EntityId,
    },
}

/// The graph of recorded judgements, as far as resolution needs it.
///
/// A trait rather than a concrete map because `kirra-world` has **zero
/// dependencies by design**: the store owns the rows, and this is the narrowest
/// seam that lets the algorithm live in the domain core with the data anywhere
/// else. It asks for a lifecycle rather than an `Entity` for the same reason —
/// resolution reads nothing else, and a wider seam would let it start to.
pub trait AdjudicationGraph {
    /// The lifecycle recorded for an id, or `None` if the graph has no such
    /// entity.
    ///
    /// # `None` means ABSENT, never "could not look"
    ///
    /// There is no error channel here, and that is a constraint on
    /// implementors rather than an oversight: an implementation backed by
    /// storage must **not** turn a read failure into `None`, because that
    /// reports an existing id as no-such-entity — the precise confusion
    /// [`RefusalReason::DanglingRedirect`] and
    /// [`RefusalReason::EmptySupersession`] exist to prevent, reintroduced one
    /// layer down.
    ///
    /// The way to honour that is to do the fallible work *before* resolving:
    /// load the state, fail closed there, and implement this over what was
    /// loaded. `kirra_world_store::IdentityView` is that shape.
    fn lifecycle_of(&self, id: &EntityId) -> Option<Lifecycle>;

    /// Whether this entity's recorded history contradicts itself.
    ///
    /// Defaulted to `false` so a graph with no notion of contradiction — a
    /// test fixture, an in-memory map — is unaffected. A projection that folds
    /// adjudications overrides it; see
    /// [`RefusalReason::ContradictoryHistory`].
    fn is_contradicted(&self, _id: &EntityId) -> bool {
        false
    }
}

/// **Resolve an identifier through every redirect recorded against it.**
///
/// The §6.3 guarantee, delivered: a merged-away id still answers, and answers
/// with what it became rather than with itself.
///
/// # The walk
///
/// Gray/black, matching the house idiom (`kirra_safety_authority::dag`): gray is
/// the ids on the current path, so arriving back at one is a
/// [`RefusalReason::RedirectCycle`]; black is the ids already fully expanded, so
/// arriving at one again is a **diamond, not a cycle**, and is skipped in
/// silence. Conflating the two would report a legitimate reconvergence — *a*
/// split into *b* and *c*, both later merged into *d* — as a corrupt graph.
///
/// # What counts as an answer
///
/// Every state except `Merged` and `Superseded` resolves to itself, `Retired`
/// included. Retirement is not a redirect: a forgotten entity is still that
/// entity, and §6.3 is explicit that `ForgetEntity` *"is not deletion"*.
/// Suppressing it belongs to whichever projection has a policy about showing
/// retired things — putting that decision here would make identity and
/// availability the same question, and a caller could no longer tell "no such
/// entity" from "that one, retired".
pub fn resolve<G: AdjudicationGraph>(graph: &G, id: &EntityId) -> ResolutionOutcome {
    let mut walk = Walk {
        graph,
        gray: Vec::new(),
        black: Vec::new(),
        answers: Vec::new(),
        hops: 0,
    };

    match walk.expand(id, None) {
        Err(reason) => ResolutionOutcome::Refused(reason),
        Ok(()) => match walk.answers.len() {
            // Reachable ONLY when the queried id is itself absent. That was
            // asserted here before it was true: an empty `Superseded` also
            // collected no answers and fell through to `Unknown`, reporting
            // "no such entity" about an id the graph had. It is now refused as
            // `EmptySupersession`, which is what restores this claim —
            // `unknown_is_reachable_only_when_the_id_is_genuinely_absent`
            // walks every state to keep it true.
            0 => ResolutionOutcome::Unknown,
            1 => ResolutionOutcome::Located {
                entity: walk.answers.remove(0),
                hops: walk.hops,
            },
            _ => ResolutionOutcome::Ambiguous {
                successors: walk.answers,
            },
        },
    }
}

/// The mutable state of one walk. Private — resolution is a function, and a
/// caller able to hold this between calls could reuse a `gray` set across
/// queries, which is how the cycle check would come to depend on call order.
struct Walk<'g, G: AdjudicationGraph> {
    graph: &'g G,
    /// Ids on the current path. Membership here means a cycle.
    gray: Vec<EntityId>,
    /// Ids fully expanded. Membership here means a diamond.
    black: Vec<EntityId>,
    /// Live entities found, in the order the recorded events name them.
    answers: Vec<EntityId>,
    /// Redirect edges traversed.
    hops: usize,
}

impl<G: AdjudicationGraph> Walk<'_, G> {
    /// Expand one id. `via` is the entity whose redirect we arrived on, and is
    /// `None` only for the queried id itself — which is what separates "the
    /// question names nothing" from "a recorded redirect is broken".
    fn expand(&mut self, id: &EntityId, via: Option<&EntityId>) -> Result<(), RefusalReason> {
        if self.gray.contains(id) {
            return Err(RefusalReason::RedirectCycle { at: id.clone() });
        }
        if self.black.contains(id) {
            return Ok(());
        }

        // Checked before the lifecycle is read, and before any answer is
        // collected: a contradicted entity has a lifecycle -- the projection
        // keeps what it held -- so trusting it here would answer `Located` from
        // a state the fold has already declared untrustworthy.
        if self.graph.is_contradicted(id) {
            return Err(RefusalReason::ContradictoryHistory { at: id.clone() });
        }

        let Some(lifecycle) = self.graph.lifecycle_of(id) else {
            return match via {
                Some(from) => Err(RefusalReason::DanglingRedirect {
                    from: from.clone(),
                    to: id.clone(),
                }),
                None => Ok(()),
            };
        };

        let successors: Vec<EntityId> = match &lifecycle {
            Lifecycle::Merged { into } => vec![into.clone()],
            // An empty list is refused rather than walked: with no successors
            // the walk would collect no answers and the id would report as
            // `Unknown` despite existing. See `RefusalReason::EmptySupersession`
            // for why a ONE-element list is answered instead.
            Lifecycle::Superseded { by } if by.is_empty() => {
                return Err(RefusalReason::EmptySupersession { at: id.clone() });
            }
            Lifecycle::Superseded { by } => by.clone(),
            // Every other state is an answer. See `resolve`'s docs for why
            // `Retired` is among them.
            _ => {
                if !self.answers.contains(id) {
                    self.answers.push(id.clone());
                }
                self.black.push(id.clone());
                return Ok(());
            }
        };

        self.gray.push(id.clone());
        for next in &successors {
            self.hops += 1;
            if self.hops > MAX_REDIRECT_EDGES {
                return Err(RefusalReason::TraversalBudgetExceeded {
                    limit: MAX_REDIRECT_EDGES,
                });
            }
            self.expand(next, Some(id))?;
        }
        // Popped before blackening: an id is gray only while it is on the path.
        self.gray.pop();
        self.black.push(id.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::Lifecycle;

    fn eid(s: &str) -> EntityId {
        EntityId::new(s).expect("valid id")
    }

    /// The smallest graph that can be written down: a list of pairs.
    struct Graph(Vec<(EntityId, Lifecycle)>);

    impl AdjudicationGraph for Graph {
        fn lifecycle_of(&self, id: &EntityId) -> Option<Lifecycle> {
            self.0.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone())
        }
    }

    fn graph(rows: &[(&str, Lifecycle)]) -> Graph {
        Graph(rows.iter().map(|(k, v)| (eid(k), v.clone())).collect())
    }

    fn merged(into: &str) -> Lifecycle {
        Lifecycle::Merged { into: eid(into) }
    }

    fn superseded(by: &[&str]) -> Lifecycle {
        Lifecycle::Superseded {
            by: by.iter().map(|s| eid(s)).collect(),
        }
    }

    // -- The ordinary answers ------------------------------------------

    #[test]
    fn a_live_entity_resolves_to_itself_with_no_hops() {
        let g = graph(&[("a", Lifecycle::Established)]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("a"),
                hops: 0,
            }
        );
    }

    /// §6.3: *"Both original IDs remain resolvable forever and answer with a
    /// redirect."* The whole point of the module in one assertion.
    #[test]
    fn a_merged_away_id_still_answers_and_answers_with_its_successor() {
        let g = graph(&[("a", merged("b")), ("b", Lifecycle::Established)]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("b"),
                hops: 1,
            }
        );
    }

    /// **The reason this module exists rather than a call to `redirects_to`.**
    ///
    /// A single-hop accessor answers `b` here, which is a dead id. Only a walk
    /// answers `c`.
    #[test]
    fn a_chain_of_merges_is_followed_to_the_end() {
        let g = graph(&[
            ("a", merged("b")),
            ("b", merged("c")),
            ("c", Lifecycle::Established),
        ]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("c"),
                hops: 2,
            }
        );
    }

    #[test]
    fn an_id_the_graph_does_not_have_is_unknown_not_refused() {
        let g = graph(&[("a", Lifecycle::Established)]);
        assert_eq!(resolve(&g, &eid("nobody")), ResolutionOutcome::Unknown);
    }

    /// A `Split` product is a **live** state, so it resolves to itself — it is
    /// the origin marker on something that exists, not a redirect off it.
    #[test]
    fn a_split_product_resolves_to_itself() {
        let g = graph(&[("b", Lifecycle::Split { from: eid("a") })]);
        assert_eq!(
            resolve(&g, &eid("b")),
            ResolutionOutcome::Located {
                entity: eid("b"),
                hops: 0,
            }
        );
    }

    /// **Retirement is not a redirect.**
    ///
    /// §6.3: `ForgetEntity` *"retires an entity and suppresses it from default
    /// projections. It is **not** deletion."* A retired id must therefore still
    /// answer with itself — reporting `Unknown` would make "we never had one"
    /// and "we retired that one" the same answer, and only one of them is true.
    #[test]
    fn a_retired_entity_still_resolves_to_itself() {
        let g = graph(&[("a", Lifecycle::Retired)]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("a"),
                hops: 0,
            }
        );
    }

    // -- Ambiguity, which is the split ruling's call site ---------------

    #[test]
    fn a_partitioned_id_is_ambiguous_between_its_successors() {
        let g = graph(&[
            ("a", superseded(&["b", "c"])),
            ("b", Lifecycle::Established),
            ("c", Lifecycle::Established),
        ]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Ambiguous {
                successors: vec![eid("b"), eid("c")],
            }
        );
    }

    /// **The successors are themselves resolved.**
    ///
    /// Without this, `Ambiguous` would hand back ids that were merged away
    /// afterwards — the caller would then have to re-resolve each one, which is
    /// the loop this module exists to stop being written at every call site.
    #[test]
    fn ambiguous_successors_are_never_dead_ids() {
        let g = graph(&[
            ("a", superseded(&["b", "c"])),
            ("b", merged("b2")),
            ("b2", Lifecycle::Established),
            ("c", Lifecycle::Established),
        ]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Ambiguous {
                successors: vec![eid("b2"), eid("c")],
            }
        );
    }

    /// **A fan-out that reconverges is not ambiguous.**
    ///
    /// `a` turned out to be `b` and `c`; `b` and `c` both turned out to be `d`.
    /// The history says one thing, so the answer is one thing. Reporting
    /// `Ambiguous { [d, d] }` would invent a disagreement the events do not
    /// contain.
    #[test]
    fn successors_that_reconverge_collapse_to_one_answer() {
        let g = graph(&[
            ("a", superseded(&["b", "c"])),
            ("b", merged("d")),
            ("c", merged("d")),
            ("d", Lifecycle::Established),
        ]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("d"),
                hops: 4,
            },
            "four edges traversed: a->b, b->d, a->c, c->d"
        );
    }

    /// A diamond is **not** a cycle, and the black set is what keeps them
    /// apart. `d` is reached twice; the second arrival is a reconvergence.
    #[test]
    fn a_diamond_is_not_reported_as_a_cycle() {
        let g = graph(&[
            ("a", superseded(&["b", "c"])),
            ("b", merged("d")),
            ("c", merged("d")),
            ("d", merged("e")),
            ("e", Lifecycle::Established),
        ]);
        assert!(
            matches!(resolve(&g, &eid("a")), ResolutionOutcome::Located { .. }),
            "reconvergence through a shared redirect must not read as a cycle"
        );
    }

    // -- Refusals ------------------------------------------------------

    /// **The failure no constructor can prevent.**
    ///
    /// `MergeEntities` refuses a merge into one of its own sources, so no
    /// single event builds this. Two events do, and neither can see the other.
    /// Without the gray set this walk does not terminate.
    #[test]
    fn a_two_event_redirect_cycle_is_refused() {
        let g = graph(&[("a", merged("b")), ("b", merged("a"))]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::RedirectCycle { at: eid("a") })
        );
    }

    #[test]
    fn a_longer_cycle_is_refused_at_the_id_it_returns_to() {
        let g = graph(&[("a", merged("b")), ("b", merged("c")), ("c", merged("b"))]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::RedirectCycle { at: eid("b") }),
            "the cycle is b->c->b; `a` is a tail leading into it, not part of it"
        );
    }

    /// A redirect naming an entity the graph does not have is **not**
    /// `Unknown`. The queried id exists; its history is broken.
    #[test]
    fn a_redirect_to_a_missing_entity_is_refused_not_unknown() {
        let g = graph(&[("a", merged("gone"))]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::DanglingRedirect {
                from: eid("a"),
                to: eid("gone"),
            })
        );
    }

    #[test]
    fn a_chain_longer_than_the_bound_is_refused_rather_than_walked() {
        // 0 -> 1 -> ... -> N, with N past the bound and no terminal state, so
        // only the bound can stop it.
        let n = MAX_REDIRECT_EDGES + 5;
        let rows: Vec<(EntityId, Lifecycle)> = (0..n)
            .map(|i| {
                (
                    eid(&format!("e{i}")),
                    Lifecycle::Merged {
                        into: eid(&format!("e{}", i + 1)),
                    },
                )
            })
            .collect();

        assert_eq!(
            resolve(&Graph(rows), &eid("e0")),
            ResolutionOutcome::Refused(RefusalReason::TraversalBudgetExceeded {
                limit: MAX_REDIRECT_EDGES,
            })
        );
    }

    /// **The budget is spent by width as well as by depth**, which is why the
    /// constant is not called `MAX_REDIRECT_DEPTH`.
    ///
    /// This graph is two levels deep and refuses anyway. Pinned as a *decision*
    /// rather than left to be discovered from a confusing refusal: under the old
    /// name, this — a legitimate wide partition — reported "depth exceeded" on a
    /// graph of depth two.
    #[test]
    fn a_wide_partition_spends_the_budget_even_though_it_is_shallow() {
        let wide: Vec<EntityId> = (0..=MAX_REDIRECT_EDGES)
            .map(|i| eid(&format!("p{i}")))
            .collect();
        let mut rows: Vec<(EntityId, Lifecycle)> =
            vec![(eid("a"), Lifecycle::Superseded { by: wide.clone() })];
        rows.extend(wide.into_iter().map(|p| (p, Lifecycle::Established)));

        assert_eq!(
            resolve(&Graph(rows), &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::TraversalBudgetExceeded {
                limit: MAX_REDIRECT_EDGES,
            }),
            "a one-level fan-out wider than the budget must refuse on the budget"
        );
    }

    /// **An existing id must never report as absent.**
    ///
    /// A `Superseded` naming nothing was resolving to `Unknown` — "no such
    /// entity" about an id the graph had — which is exactly what
    /// `DanglingRedirect` exists to prevent one hop later. It reached `Unknown`
    /// by collecting no answers, so nothing in the walk had to be wrong for the
    /// outcome to be.
    ///
    /// The type says `by` is never empty and `SplitEntity::partition` enforces
    /// that, but `Lifecycle` is a plain public enum: the value below compiles,
    /// and a store decoding a corrupt row can produce it.
    #[test]
    fn a_supersession_that_names_nothing_is_refused_not_reported_absent() {
        let g = graph(&[("a", Lifecycle::Superseded { by: Vec::new() })]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::EmptySupersession { at: eid("a") })
        );
    }

    /// The same, reached through a redirect rather than at the root — the arm
    /// must not depend on where in the walk the malformed row turns up.
    #[test]
    fn an_empty_supersession_is_refused_deeper_in_the_walk_too() {
        let g = graph(&[
            ("a", merged("b")),
            ("b", Lifecycle::Superseded { by: Vec::new() }),
        ]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::EmptySupersession { at: eid("b") })
        );
    }

    /// **A one-element supersession is answered, not refused**, and that is a
    /// decision rather than an oversight — see `RefusalReason::EmptySupersession`.
    ///
    /// It breaks the same documented invariant as the empty case, but unlike it
    /// the question has an unambiguous answer, and refusing would discard a
    /// correct one. Rejecting the malformed row belongs at the decoding
    /// boundary, not in the walk.
    #[test]
    fn a_one_element_supersession_still_answers() {
        let g = graph(&[("a", superseded(&["b"])), ("b", Lifecycle::Established)]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("b"),
                hops: 1,
            }
        );
    }

    /// **`Unknown` means absent, and nothing else.**
    ///
    /// `resolve` claims `Unknown` is reachable only when the queried id is not
    /// in the graph. That claim was already in the code as a comment while
    /// being false, so it is asserted here rather than asserted in prose: every
    /// lifecycle state, including the malformed ones, is queried, and none of
    /// them may answer `Unknown`.
    #[test]
    fn unknown_is_reachable_only_when_the_id_is_genuinely_absent() {
        let every_state = [
            Lifecycle::Provisional,
            Lifecycle::Established,
            Lifecycle::Dormant,
            Lifecycle::Retired,
            Lifecycle::Merged { into: eid("z") },
            Lifecycle::Split { from: eid("z") },
            Lifecycle::Superseded { by: vec![eid("z")] },
            Lifecycle::Superseded {
                by: vec![eid("z"), eid("z2")],
            },
            // The malformed one that used to answer `Unknown`.
            Lifecycle::Superseded { by: Vec::new() },
        ];

        for state in every_state {
            let g = Graph(vec![
                (eid("a"), state.clone()),
                (eid("z"), Lifecycle::Established),
                (eid("z2"), Lifecycle::Dormant),
            ]);
            assert_ne!(
                resolve(&g, &eid("a")),
                ResolutionOutcome::Unknown,
                "an id the graph HAS reported as absent, in state {state:?}"
            );
        }

        // ...and the one case that must: genuinely not there.
        assert_eq!(
            resolve(&Graph(Vec::new()), &eid("a")),
            ResolutionOutcome::Unknown,
            "the assertions above would hold vacuously if nothing answered Unknown"
        );
    }

    /// The bound is a backstop, not a working limit: a chain right up to it
    /// still answers.
    #[test]
    fn a_chain_exactly_at_the_bound_still_resolves() {
        let mut rows: Vec<(EntityId, Lifecycle)> = (0..MAX_REDIRECT_EDGES)
            .map(|i| {
                (
                    eid(&format!("e{i}")),
                    Lifecycle::Merged {
                        into: eid(&format!("e{}", i + 1)),
                    },
                )
            })
            .collect();
        rows.push((
            eid(&format!("e{MAX_REDIRECT_EDGES}")),
            Lifecycle::Established,
        ));

        assert_eq!(
            resolve(&Graph(rows), &eid("e0")),
            ResolutionOutcome::Located {
                entity: eid(&format!("e{MAX_REDIRECT_EDGES}")),
                hops: MAX_REDIRECT_EDGES,
            }
        );
    }

    // -- Contradiction -------------------------------------------------

    /// A graph that reports contradictions, for the two tests below.
    struct Contradicting(Graph, Vec<EntityId>);

    impl AdjudicationGraph for Contradicting {
        fn lifecycle_of(&self, id: &EntityId) -> Option<Lifecycle> {
            self.0.lifecycle_of(id)
        }
        fn is_contradicted(&self, id: &EntityId) -> bool {
            self.1.contains(id)
        }
    }

    /// **A contradicted entity refuses rather than answering from a state the
    /// fold declined to trust.**
    ///
    /// The projection keeps the lifecycle it held when the contradiction
    /// arrived, so `lifecycle_of` returns something perfectly well-formed here.
    /// Answering `Located` from it would launder a disagreement into a
    /// confident answer.
    #[test]
    fn a_contradicted_entity_refuses_instead_of_answering() {
        let g = Contradicting(
            graph(&[("a", merged("b")), ("b", Lifecycle::Established)]),
            vec![eid("a")],
        );
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Refused(RefusalReason::ContradictoryHistory { at: eid("a") })
        );
    }

    /// **And it refuses for a question that merely routes through it.**
    ///
    /// `z` is not contradicted; `a` is, and `z` redirects to it. An answer that
    /// travelled through a contradicted identity is not trustworthy because the
    /// question named something else.
    #[test]
    fn a_redirect_through_a_contradicted_entity_also_refuses() {
        let g = Contradicting(
            graph(&[
                ("z", merged("a")),
                ("a", merged("b")),
                ("b", Lifecycle::Established),
            ]),
            vec![eid("a")],
        );
        assert_eq!(
            resolve(&g, &eid("z")),
            ResolutionOutcome::Refused(RefusalReason::ContradictoryHistory { at: eid("a") }),
            "the refusal names the contradicted entity, not the one asked about"
        );
    }

    /// The default is `false`, so every graph that does not model
    /// contradiction is byte-identical in behaviour.
    #[test]
    fn a_graph_without_contradictions_is_unaffected() {
        let g = graph(&[("a", merged("b")), ("b", Lifecycle::Established)]);
        assert_eq!(
            resolve(&g, &eid("a")),
            ResolutionOutcome::Located {
                entity: eid("b"),
                hops: 1,
            }
        );
    }

    // -- The shape of the answer ---------------------------------------

    /// **`Ambiguous` is never one.** A partition whose successors reconverge is
    /// `Located`, and the type must not be able to say "ambiguous between one
    /// thing" — a caller branching on the variant would treat a settled answer
    /// as unsettled.
    ///
    /// Every case here is a partition, so a naive implementation returns
    /// `Ambiguous` for all of them; the first three converge and must not.
    ///
    /// The expected outcome is asserted **per case** rather than by an
    /// `if let Ambiguous { assert len >= 2 }` over the loop. That was the first
    /// spelling, and it asserted nothing whatsoever: none of the converging
    /// cases reaches the `Ambiguous` arm, so the length check never executed
    /// and the test passed on an empty body. The last case exists to keep that
    /// arm populated, and `saw_ambiguous` below fails the test if a future
    /// change empties it again.
    #[test]
    fn ambiguity_survives_only_where_the_successors_really_disagree() {
        struct Case(&'static str, Graph, ResolutionOutcome);
        let cases = vec![
            Case(
                "one successor absorbs the other",
                graph(&[
                    ("a", superseded(&["b", "c"])),
                    ("b", merged("c")),
                    ("c", Lifecycle::Established),
                ]),
                ResolutionOutcome::Located {
                    entity: eid("c"),
                    hops: 3,
                },
            ),
            Case(
                "both merge into a third",
                graph(&[
                    ("a", superseded(&["b", "c"])),
                    ("b", merged("d")),
                    ("c", merged("d")),
                    ("d", Lifecycle::Established),
                ]),
                ResolutionOutcome::Located {
                    entity: eid("d"),
                    hops: 4,
                },
            ),
            Case(
                "converge, then redirect once more together",
                graph(&[
                    ("a", superseded(&["b", "b2"])),
                    ("b", merged("x")),
                    ("b2", merged("x")),
                    ("x", merged("y")),
                    ("y", Lifecycle::Dormant),
                ]),
                ResolutionOutcome::Located {
                    entity: eid("y"),
                    hops: 5,
                },
            ),
            Case(
                "genuinely two things",
                graph(&[
                    ("a", superseded(&["b", "c"])),
                    ("b", Lifecycle::Established),
                    ("c", Lifecycle::Dormant),
                ]),
                ResolutionOutcome::Ambiguous {
                    successors: vec![eid("b"), eid("c")],
                },
            ),
        ];

        let mut saw_ambiguous = false;
        for Case(name, g, expected) in &cases {
            let got = resolve(g, &eid("a"));
            if let ResolutionOutcome::Ambiguous { successors } = &got {
                saw_ambiguous = true;
                assert!(successors.len() >= 2, "{name}: {successors:?}");
            }
            assert_eq!(&got, expected, "{name}");
        }
        assert!(
            saw_ambiguous,
            "no case produced an Ambiguity, so the length invariant above was \
             never exercised — this is how the first version of this test \
             passed while asserting nothing"
        );
    }

    /// `hops` counts edges **traversed**, not the shortest path. Pinned because
    /// the two readings agree on every linear chain and disagree only here, so
    /// a change of meaning would otherwise go unnoticed.
    #[test]
    fn hops_reports_the_whole_walk_not_the_winning_branch() {
        let g = graph(&[
            ("a", superseded(&["b", "c"])),
            ("b", merged("d")),
            ("c", merged("d")),
            ("d", Lifecycle::Established),
        ]);
        let ResolutionOutcome::Located { hops, .. } = resolve(&g, &eid("a")) else {
            panic!("expected a single answer");
        };
        assert_eq!(hops, 4, "shortest path is 2; the walk is 4");
    }
}
