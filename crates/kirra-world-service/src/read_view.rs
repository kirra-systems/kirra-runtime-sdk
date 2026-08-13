//! **The answer boundary** — where Tier 3's "no bare values" rule can actually live.
//!
//! # What this is, and what it is not
//!
//! `WM_SCOPE.md` §9 asks for a small consumer wired **before** Tier 3, on the
//! argument that a real caller falsifies what a design document cannot. That
//! consumer was built, and **Fence B refused every host §9 nominates** — the
//! planner, perception and LLM crates all implement `CorridorSource`, which is
//! authoritative to the checker, so none of them may depend on Kirra World. §9
//! records the refusal.
//!
//! So this is **not** the external doer caller §9 asked for, and it should not
//! be read as one. What it is: the **shape that caller produced**, kept
//! somewhere durable instead of being lost, in the one crate that can legally
//! hold it — this one already depends on both `kirra-world` and
//! `kirra-world-store`, implements no `CorridorSource`, and sits outside the
//! safety closure.
//!
//! The falsification is already banked. What was still at risk was the *type*,
//! and a type that has met a caller is worth more to Tier 3 than one derived
//! from the rule alone.
//!
//! # The rule this exists to make structural
//!
//! Blueprint §14.2, rule 1:
//!
//! > **No API returns a bare value.** Every answer carries the value, the trust
//! > axes, the validity at the supplied clock, and a `ProvenanceHandle`. …
//! > *"a deliberate ergonomic cost: it makes 'I got a number and lost where it
//! > came from' impossible to write."*
//!
//! **It cannot be met by [`ProjectedClaim`]**, and not for want of care. That
//! type's fields are public, so
//!
//! ```ignore
//! let payload = &store.current("robot-01", now)?[0].payload;
//! ```
//!
//! compiles — no validity, no trust, no handle. `validity_at` and `grade_at` are
//! methods a caller must *remember* to call, and forgetting is the default.
//!
//! That is not a defect. `ProjectedClaim` is the projection **row**, and a row
//! is honestly a bag of columns. The rule belongs one layer out, at the boundary
//! where a row becomes an *answer* — which is here.
//!
//! # What [`WorldAnswer`] buys, and the bound on it
//!
//! It has no constructor that omits validity, trust or provenance, and only two
//! things can produce one — [`WorldView::ask`] and
//! [`crate::answer_ref::AnswerRef::resolve`]. Both funnel through the same
//! private `assemble`, so an answer in hand always carries them, and adding a
//! third producer means going through that funnel too.
//!
//! (This read *"`ask` is the only way to obtain one"* until refs landed. Caught
//! in review on #1434: a guarantee stated as a count of functions goes stale the
//! moment a second one is right, which is why it is now stated as the funnel.)
//!
//! **"Trust" here means the axes, not a summary of them.** Rule 1 says *the
//! trust axes*, and an earlier draft of this type carried only the collapsed
//! [`TrustGrade`] while quoting the rule verbatim — which would have been the
//! same overclaim this module exists to catch. Both are carried now:
//! [`WorldAnswer::axes`] is what the rule requires, and [`WorldAnswer::grade`]
//! is a convenience over it. Collapsing is fine; collapsing *and discarding the
//! reason* is not, because `Weak` can mean uncorroborated, stale, or awaiting
//! adjudication and a caller who needs to know which would be left with the raw
//! log.
//!
//! **The honest bound, because overclaiming here would be the same failure the
//! rule is about:** this closes the hole at *retrieval*. A caller cannot obtain
//! the value without being handed the rest. It does **not** stop that caller
//! destructuring and passing the value onward alone — Rust cannot prevent that
//! without infecting every downstream signature, and a rule advertised as
//! airtight when it is not is worse than one whose limit is written down.
//!
//! # Three rules, honoured deliberately
//!
//! 1. **No bare values** — [`WorldAnswer`], above.
//! 2. **Queries are bounded** — [`WorldView::ask`] asks for *one subject*, never
//!    the whole projection. D-9 measured 10.5 s p99 temporal queries at 100 000
//!    entities, and ADR-0041 D-12 bars graph and temporal queries from any
//!    control or safety deadline path. The bounded call is the habit to
//!    establish before eight verbs exist.
//! 3. **`Unknown` is a success** — [`WorldLookup::Unknown`] is a variant of the
//!    `Ok` value. The error channel is for storage faults only. Conflating them
//!    is how *"I don't know"* becomes an exception somebody catches and turns
//!    into a default value.
//!
//! # Fence A still holds, and this is where it would erode
//!
//! This crate is inside Fence A's walk. A read view has no route to an actuator
//! or an authorization: it reads a projection and returns owned data. That is
//! the whole point of putting the boundary in a crate the fence already watches
//! rather than in one it does not.

use kirra_world::evidence::{DigestError, EvidenceDigest};
use kirra_world::reference::EntityId;
use kirra_world::resolution::{resolve, RefusalReason, ResolutionOutcome};
use kirra_world_store::compaction::Resolution;
use kirra_world_store::entity_projection::IdentityView;
use kirra_world_store::snapshot::SnapshotCoordinate;
use kirra_world_store::subject_projection::SubjectSummaryAnswer;
use kirra_world_store::{ProjectedClaim, StoreError, TrustAxes, TrustGrade, Validity, WorldStore};

use crate::answer_ref::QueryKind;
use crate::freshness::{resolve_policy, FreshnessPolicy, FreshnessSource};
use crate::semantics::SemanticVersions;

/// Why an ask could not be answered at all.
///
/// Distinct from [`WorldLookup::Unknown`], and the split is rule 3: *absence of
/// knowledge is a success*, and only genuine faults use this channel.
#[derive(Debug)]
pub enum AskError {
    /// The store could not be read.
    Store(StoreError),
    /// A stored chain digest is not a digest.
    ///
    /// **Refused rather than served**, and the reasoning is this module's own:
    /// rule 1 says every answer carries a `ProvenanceHandle`, so an answer whose
    /// handle cannot be parsed is one that cannot be *cited* — serving it would
    /// break the rule this boundary exists to enforce, while looking like an
    /// ordinary answer.
    ///
    /// It belongs in the **error** channel rather than in `Unknown` because it
    /// is a storage fault: the stored bytes are not what the schema promises.
    /// `Unknown` means *"nothing is known"*, which would be a false statement
    /// here — something is known, and it is unreadable.
    CorruptProvenance {
        /// The claim's subject.
        subject: String,
        /// The claim's predicate.
        ///
        /// Carried because `world_current` is keyed by
        /// `PRIMARY KEY (subject, predicate_key)`, so the subject alone
        /// under-identifies the row the moment a subject has more than one
        /// predicate — which is the ordinary case, and is exactly why
        /// [`WorldView::ask`] returns a *list* of answers. An error that
        /// cannot name the row an operator must inspect is a report of a
        /// fault rather than a report of *which* fault.
        ///
        /// Domain shape (`Option`) rather than the storage spelling; the
        /// `Display` impl renders `predicate_key`'s empty-string form, since
        /// that is what an operator has to type against the table.
        predicate: Option<String>,
        /// What was wrong with the digest.
        cause: DigestError,
    },
    /// **No ruled freshness policy for this claim's semantics** — box 3e.
    ///
    /// `KIRRA-WM-FRESHNESS-POLICY-001`: unclassified semantics refuse. This is
    /// a **policy-resolution failure**, not a freshness state — which is why it
    /// is an error rather than a fourth `Validity` variant, and why it is not
    /// `Unknown`: something IS known about this subject, and the engine has not
    /// been told what its age means.
    ///
    /// Fail-closed by the `KIRRA_VEHICLE_CLASS` precedent. Serving it as
    /// `Timeless` would assert that the fact's age does not matter — a positive
    /// claim nobody made.
    UnclassifiedFreshness {
        /// The claim kind that has no ruling.
        kind: String,
        /// Its predicate, or `None` for a payload-only claim.
        predicate: Option<String>,
    },
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => write!(f, "store: {e:?}"),
            Self::CorruptProvenance {
                subject,
                predicate,
                cause,
            } => {
                // `predicate_key` is the predicate or '' -- rendered as stored,
                // so the message can be used against the table verbatim.
                let key = predicate.as_deref().unwrap_or("");
                write!(
                    f,
                    "unreadable provenance handle for world_current row \
                     (subject={subject:?}, predicate_key={key:?}): {cause}"
                )
            }
            Self::UnclassifiedFreshness { kind, predicate } => write!(
                f,
                "no ruled freshness policy for kind={kind} predicate={} — \
                 unclassified semantics refuse (KIRRA-WM-FRESHNESS-POLICY-001)",
                predicate.as_deref().unwrap_or("<none>")
            ),
        }
    }
}

impl std::error::Error for AskError {}

impl From<StoreError> for AskError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

use core::fmt;

/// Why the world had nothing to say. **Not an error** — see rule 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// No current claim for this subject at this clock.
    ///
    /// Note what this currently conflates: a subject never heard of, and one
    /// whose claim has **expired**, arrive here identically, because
    /// [`WorldStore::current`] filters on `holds_at` before this boundary sees
    /// anything. An operator whose coverage merely lapsed is therefore told
    /// "nothing known" and sent to a coverage question rather than a freshness
    /// one. A boundary cannot reconstruct the difference; only the store can
    /// report it, which makes it Tier 3's to fix.
    NoClaim,
    /// Claims exist, but none is admissible at this clock and budget.
    ///
    /// **Unreachable today, and deliberately kept.** Two filters — the
    /// projection's `claim_status = 'confirmed'` fold and `current()`'s
    /// `holds_at` — mean an inadmissible claim never reaches this boundary. That
    /// guarantee is pinned in
    /// `kirra-world-store/tests/inadmissible_never_read.rs`.
    ///
    /// Kept because neither filter is a *stated contract*. A boundary that
    /// silently begins serving rejected or expired facts if one of them changes
    /// is a worse outcome than a variant that never occurs.
    NoneAdmissible,
}

/// One answer, which cannot exist without the things that make it citable.
#[derive(Debug, Clone)]
pub struct WorldAnswer {
    subject: String,
    predicate: Option<String>,
    object: Option<String>,
    object_identity: ObjectIdentity,
    value: String,
    validity: Validity,
    axes: Option<TrustAxes>,
    grade: Option<TrustGrade>,
    provenance: EvidenceDigest,
    event_id: String,
}

impl WorldAnswer {
    /// The claim's subject.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The claim's OBJECT — the entity or value a relationship-shaped claim
    /// points at — `None` for claims that carry no relationship.
    ///
    /// **Added because a real consumer needed it and could not get it.**
    /// `mission_context` reads facts of the form `package_17 last_seen_at
    /// dock_b`, matching candidates against the object; before this existed the
    /// boundary modelled a subject-predicate-**payload** claim while the store
    /// stored subject-predicate-**object**-payload, so a triple-shaped fact
    /// could not cross the boundary at all. Nothing had ever needed to read an
    /// object, so nothing had noticed the field was missing — which is the
    /// specification-without-a-consumer failure `KIRRA-WM-CONSUMER-WITNESS-001`
    /// exists to prevent.
    ///
    /// # Invariant
    ///
    /// `object.is_some()` implies `predicate.is_some()`. That is not defended
    /// here; it is guaranteed upstream by `KIRRA-WM-CLAIM-SHAPES-001` at two
    /// layers — admission and the v5 schema trigger — because the alternative
    /// shape aliases a payload-only claim in `world_current`'s
    /// `(subject, '')` slot and destroys it.
    ///
    /// **This is the stored object, uninterpreted.** For what it resolves to
    /// through the identity graph, see [`WorldAnswer::object_identity`] — the
    /// two are kept separate so a caller can always see what was actually
    /// written, not only what it now means.
    #[must_use]
    pub fn object(&self) -> Option<&str> {
        self.object.as_deref()
    }

    /// **What the object resolves to through the identity graph** — box 3c.
    ///
    /// Resolved in the SAME snapshot as the claim itself, so this can never
    /// describe a different state of the world than [`WorldAnswer::object`] was
    /// read from.
    ///
    /// Prefer [`ObjectIdentity::matchable`] over matching on the variants by
    /// hand: it is what makes the fail-closed cases fail closed.
    #[must_use]
    pub fn object_identity(&self) -> &ObjectIdentity {
        &self.object_identity
    }

    /// The claim's predicate, `None` for predicate-less claims.
    #[must_use]
    pub fn predicate(&self) -> Option<&str> {
        self.predicate.as_deref()
    }

    /// The value itself.
    ///
    /// Reaching this requires already holding a `WorldAnswer`, which cannot be
    /// built without validity, grade and provenance beside it. The value never
    /// *arrives* alone — though a caller may still choose to carry it onward
    /// alone, which is the bound named in the module docs.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Validity resolved at the clock and budget the caller supplied.
    ///
    /// Resolved at construction rather than offered as a method, so it cannot be
    /// the step that gets skipped. That is the entire difference between this
    /// and [`ProjectedClaim`].
    #[must_use]
    pub fn validity(&self) -> Validity {
        self.validity
    }

    /// The three stored trust axes, or `None` when the claim is unlabelled.
    ///
    /// **This is what rule 1 actually requires** — *"the trust axes"*, not a
    /// summary of them. It is carried beside [`Self::grade`] rather than instead
    /// of it, and the distinction is not cosmetic.
    ///
    /// A grade is a **collapse**. `Weak` can mean uncorroborated, or stale, or
    /// awaiting adjudication, and a boundary that returned only the grade would
    /// have performed that collapse *and thrown away the reason* — leaving a
    /// caller who needs to know **why** with nowhere to look but the raw log.
    /// The store's own `grade_at` says the collapse should be "something a
    /// caller *does*, never something they receive by default"; returning only
    /// the result of it would invert exactly that.
    ///
    /// `None` is *"unlabelled"*, never *"assume the default"*.
    #[must_use]
    pub fn axes(&self) -> Option<TrustAxes> {
        self.axes
    }

    /// The collapsed trust grade, or `None` when the claim carries no axes.
    ///
    /// A convenience over [`Self::axes`] and [`Self::validity`], not a
    /// replacement for them: every answer carries the axes it was collapsed
    /// from, so taking the grade never costs the reason behind it.
    ///
    /// `None` means *"this claim is unlabelled"*, never *"assume the default"*.
    /// An unlabelled claim has no trust to grade, and manufacturing one is the
    /// failure separate axes exist to prevent.
    #[must_use]
    pub fn grade(&self) -> Option<TrustGrade> {
        self.grade
    }

    /// The provenance handle: the claim's chain digest.
    ///
    /// What makes the answer *citable* rather than merely believed — it locates
    /// the claim in the tamper-evident log, so a reader can verify it instead of
    /// trusting the API that served it.
    #[must_use]
    pub fn provenance(&self) -> &EvidenceDigest {
        &self.provenance
    }

    /// Identity of the event this answer came from.
    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }
}

/// The outcome of asking the world about a subject.
#[derive(Debug, Clone)]
pub enum WorldLookup {
    /// One or more admissible answers, in the store's key order.
    Answered(Vec<WorldAnswer>),
    /// Nothing usable, and why.
    Unknown(UnknownReason),
}

/// **What a claim's object turned out to be** — Tier 3 box 3c.
///
/// A claim's object often names an entity, and an entity's identity is itself a
/// projection: merges, splits and retirements are recorded as adjudications and
/// folded into the entity graph. So comparing an object as a raw string answers
/// the wrong question the moment anything has been merged.
///
/// Six variants for four caller behaviours, because collapsing them is the
/// defect box 3a was opened for: *match literally*, *match the canonical
/// entity*, *fail closed*, and *the graph could not be consulted at all* are
/// four different situations, and an operator debugging the fourth needs it not
/// to look like the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectIdentity {
    /// The claim carries no object. Nothing to resolve.
    NoObject,
    /// **This answer came from a log REPLAY — generation-pinned or bitemporal —
    /// where identity resolution is not performed.**
    ///
    /// Not a fault and not a stale graph: identity is a SECOND projection with
    /// its own coordinate, and the pinned read exists only for `world_current`.
    /// Resolving identity here would pair a generation-pinned claim with a
    /// transaction-time-pinned identity, mixing the two axes that box 3c closed.
    ///
    /// Kept distinct from [`Self::Unresolvable`] because they call for different
    /// responses: that one is an operator's problem to fix by folding, this one
    /// is a limit of what a ref can currently reconstruct.
    NotResolvedInReplay,
    /// **The identity graph is BEHIND the log: adjudications are recorded that
    /// it has not folded, so any resolution would be known-stale.**
    ///
    /// Not "the graph is empty" — an empty graph on a store with no
    /// adjudications is perfectly current, and objects there stand for
    /// themselves. This is the case where merges, splits or retirements *have*
    /// been recorded and the projection has not caught up, so resolving would
    /// answer from data already known to be superseded.
    ///
    /// Fail-closed rather than falling back to the raw string, because the
    /// fallback is silent: it would look exactly like resolution succeeding and
    /// finding nothing to redirect, which is the one answer a stale graph
    /// cannot support.
    Unresolvable,
    /// The object is not a well-formed entity id, so it cannot name one.
    Malformed,
    /// The graph was consulted and holds no such entity.
    ///
    /// Nothing is wrong: not every object is an adjudicated entity. The object
    /// stands for itself.
    NotAnEntity,
    /// Resolved to one live entity.
    Resolved {
        /// The canonical id. Equal to the stored object when nothing redirected
        /// it.
        entity: String,
        /// Redirect edges traversed. `0` means nothing was followed.
        hops: usize,
    },
    /// The id was partitioned and its successors did not reconverge.
    Ambiguous {
        /// The live entities the object turned out to be.
        successors: Vec<String>,
    },
    /// The recorded history contradicts itself.
    Refused(RefusalReason),
}

impl ObjectIdentity {
    /// The id a consumer should match against, if matching is safe at all.
    ///
    /// `None` for every variant where matching would be a guess: an
    /// unconsultable graph, a contradictory history, or an object that turned
    /// out to be more than one thing. Fail-closed by construction — a consumer
    /// cannot reach a candidate comparison for those cases without deliberately
    /// going around this method.
    pub fn matchable<'a>(&'a self, stored_object: Option<&'a str>) -> Option<&'a str> {
        match self {
            Self::Resolved { entity, .. } => Some(entity),
            Self::NotAnEntity | Self::Malformed => stored_object,
            Self::NoObject
            | Self::Unresolvable
            | Self::NotResolvedInReplay
            | Self::Ambiguous { .. }
            | Self::Refused(_) => None,
        }
    }
}

/// **A lookup and the point it was read at** — Tier 3 box 3c.
///
/// The coordinate is bundled rather than returned separately, on the precedent
/// [`HistoricalAnswer`] set: a caller must not be able to hold the answer
/// without holding which state of the world produced it. Sampling the
/// coordinate in a second call would also reintroduce the very race this box
/// closes — the answer would name a point it was not read at.
///
/// [`HistoricalAnswer`]: kirra_world_store::entity_projection::HistoricalAnswer
#[derive(Debug, Clone)]
pub struct ComposedLookup {
    lookup: WorldLookup,
    coordinate: SnapshotCoordinate,
}

impl ComposedLookup {
    /// What was found.
    pub fn lookup(&self) -> &WorldLookup {
        &self.lookup
    }

    /// Take ownership of what was found.
    pub fn into_lookup(self) -> WorldLookup {
        self.lookup
    }

    /// Where every projection stood when this was read.
    pub fn coordinate(&self) -> &SnapshotCoordinate {
        &self.coordinate
    }
}

/// A read-only view onto the world.
///
/// Read-only is structural, not a convention: this type holds a `&WorldStore`
/// and offers no mutation. Kirra World is evidence; an answer boundary serves it
/// and never adjudicates it.
pub struct WorldView<'a> {
    store: &'a WorldStore,
    freshness: FreshnessSource,
}

impl<'a> WorldView<'a> {
    /// Bind a view to a **freshness source** — box 3e.
    ///
    /// This took an `Option<u64>` until 3e, and `None` meant *"claims do not go
    /// stale"*. That was the defect: `Validity::Timeless` is a positive claim
    /// that a fact's age does not matter, and the engine asserted it about every
    /// fact in the store whenever a caller supplied nothing.
    ///
    /// [`FreshnessSource`] has no variant meaning "nothing supplied", so the old
    /// default is unrepresentable rather than merely discouraged. Either the
    /// ruled table decides, or the caller states a policy — and an unclassified
    /// class under [`FreshnessSource::Ruled`] refuses.
    #[must_use]
    pub fn new(store: &'a WorldStore, freshness: FreshnessSource) -> Self {
        Self { store, freshness }
    }

    /// Ask what is currently known about one subject.
    ///
    /// Bounded by construction — one subject, never the whole projection.
    ///
    /// # Errors
    ///
    /// Only on a storage fault. An empty or wholly-inadmissible result is
    /// [`WorldLookup::Unknown`], which is a **success**.
    /// # This is a composed read, and it is coherent by construction
    ///
    /// Claims and identity are two independently-folded projections. Both are
    /// read through ONE [`ReadSnapshot`], so an answer can never pair a claim
    /// from one state of the world with an identity resolution from another.
    /// The point they were read at rides back on the result — see
    /// [`ComposedLookup`].
    ///
    /// [`ReadSnapshot`]: kirra_world_store::snapshot::ReadSnapshot
    ///
    /// # The SUBJECT is deliberately not resolved
    ///
    /// Only the object is resolved through the identity graph. Resolving the
    /// subject would be a regression, not an improvement: `world_current` is
    /// keyed by the subject string **as written**, so rewriting a queried alias
    /// to its canonical id would look up a key nothing was ever stored under and
    /// return *fewer* claims than asking plainly. Reading the whole equivalence
    /// class and merging it is the operation that would actually be correct, and
    /// that is a query design of its own rather than something to slip in here.
    pub fn ask(&self, subject: &str, now_ms: i64) -> Result<ComposedLookup, AskError> {
        let snapshot = self.store.read_snapshot()?;
        let coordinate = snapshot.coordinate()?;
        let claims = snapshot.current(subject, now_ms)?;

        if claims.is_empty() {
            return Ok(ComposedLookup {
                lookup: WorldLookup::Unknown(UnknownReason::NoClaim),
                coordinate,
            });
        }

        // BOUNDED, and loaded once for the whole answer from the same snapshot
        // as the claims — box 3d.
        //
        // This was `snapshot.identity_view()`: the WHOLE entity graph, to answer
        // a question about one subject. Correct, coherent, and `O(entities)`
        // work for an `O(predicates)` question — the third instance of the
        // defect class #1440 and #1441 fixed, found by the gate rather than by
        // review, on the most-used query in the system.
        //
        // Only the objects this subject's claims actually name are seeded, so
        // the load is bounded by the subject's predicate count times the
        // resolver's own traversal budget. Still ONE view for the whole answer:
        // per-claim views would resolve each object against a different graph.
        let seeds: Vec<EntityId> = claims
            .iter()
            .filter_map(|c| c.object.as_deref())
            .filter_map(|o| EntityId::new(o).ok())
            .collect();
        let identity = snapshot.identity_view_for(&seeds)?;
        let identity_is_current = snapshot.identity_is_current()?;

        let clock = now_ms.max(0).unsigned_abs();
        let mut answers: Vec<WorldAnswer> = Vec::new();
        for c in claims.iter() {
            // ONE resolution per claim, used by both the admissibility test and
            // the binding. Resolving twice was not merely wasteful: it made the
            // fail-closed rule depend on two lookups agreeing, when the whole
            // point of `policy_for` is that there is one place to refuse.
            //
            // `?`, NOT a swallowed refusal. A first draft filtered with
            // `.unwrap_or(false)`, which dropped an unclassified claim silently
            // and turned a policy fault into a narrower answer — the exact
            // failure `one_unruled_claim_refuses_the_whole_query_rather_than_narrowing_it`
            // exists to catch, and it caught it.
            let budget = self.policy_for(c)?.budget();
            if !Self::is_admissible(c, clock, budget) {
                continue;
            }
            answers.push(Self::bind(
                c,
                clock,
                budget,
                &identity,
                identity_is_current,
            )?);
        }

        let lookup = if answers.is_empty() {
            WorldLookup::Unknown(UnknownReason::NoneAdmissible)
        } else {
            WorldLookup::Answered(answers)
        };
        Ok(ComposedLookup { lookup, coordinate })
    }

    /// **Ask what was true at `valid_at_ms`, as the store knew it at
    /// `as_known_at_ms`** — the bitemporal query, carrying its completeness.
    ///
    /// The answer boundary's first query family that can genuinely DEGRADE.
    /// `ask` reads `world_current`, which compaction structurally protects
    /// (`compact_range` refuses to remove a live projection head), so its
    /// completeness would be `Full` by construction and prove nothing. This one
    /// replays the log, which is precisely what compaction removes.
    ///
    /// # The completeness is the store's, propagated — not recomputed
    ///
    /// `WorldStore::as_of` already decides `Full` vs `Degraded` against the
    /// retained summaries on both temporal axes. A second judgement here would
    /// be a second implementation of the rule that governs whether an answer can
    /// be trusted, and the two would drift. This carries the store's verdict
    /// across the boundary unchanged; 3g is about propagation, not about a new
    /// opinion.
    ///
    /// # Errors
    ///
    /// As [`WorldView::ask`].
    pub fn ask_as_of(
        &self,
        subject: &str,
        valid_at_ms: i64,
        as_known_at_ms: i64,
    ) -> Result<TemporalLookup, AskError> {
        // COMPOSED at one transaction-time cut. Objects resolve through the
        // identity graph as it stood at `as_known_at_ms`, so an adjudication
        // recorded after that instant cannot rewrite this answer.
        let composed = self
            .store
            .as_of_composed(subject, valid_at_ms, as_known_at_ms)?;
        // Both halves MOVED out together. An earlier draft cloned the identity
        // view and then dropped the original — a full copy of the entity graph
        // on every call, for nothing. Caught in review on #1438.
        let (answer, identity) = composed.into_parts();
        let completeness = answer.resolution;

        let clock = valid_at_ms.max(0).unsigned_abs();
        let mut answers = Vec::new();
        for claim in &answer.claims {
            // Resolved once and shared by the admissibility test and the
            // assembly below — see the note in `ask`.
            let budget = self.policy_for(claim)?.budget();
            if !Self::is_admissible(claim, clock, budget) {
                continue;
            }
            answers.push(assemble(
                claim,
                clock,
                budget,
                // `true` for the same reason a pinned composition passes it:
                // the historical graph was replayed from the log up to the cut,
                // so it cannot lag it. `identity_is_current` answers a question
                // about the LIVE projection and would be the wrong question here.
                Self::resolve_object(claim.object.as_deref(), &identity, true),
            )?);
        }

        // Completeness is attached to BOTH outcomes. An `Unknown` that is
        // `Degraded` says "we deleted the evidence", which is a different fact
        // from "nothing was known" and the one 3g exists to keep.
        let lookup = if answers.is_empty() {
            WorldLookup::Unknown(if answer.claims.is_empty() {
                UnknownReason::NoClaim
            } else {
                UnknownReason::NoneAdmissible
            })
        } else {
            WorldLookup::Answered(answers)
        };
        Ok(TemporalLookup {
            lookup,
            completeness,
            semantics: SemanticVersions::for_query(QueryKind::AsOfSubject),
        })
    }

    /// **Every confirmed claim ever recorded about `subject`, and how much of
    /// that record survives** — the 3g follow-up.
    ///
    /// # This does NOT filter on admissibility, and the version set says it does
    ///
    /// Each claim's freshness policy is still RESOLVED, so 3e's fail-closed
    /// refusal on unclassified semantics reaches this family — an unruled claim
    /// refuses the whole query here exactly as it does in [`Self::ask`]. But no
    /// claim is dropped for being stale.
    ///
    /// The distinction is the whole design. `ask` answers *"what is true now"*,
    /// where serving a claim past its freshness budget is the error 3e exists
    /// to prevent. This answers *"what has ever been claimed"*, where a claim
    /// that has since gone stale is not a claim to hide — it is the record.
    /// Filtering here would make history lie about the past in exactly the way
    /// an admissibility filter on a lineage would hide the evidence an
    /// investigator came for.
    ///
    /// # The completeness is the store's, propagated
    ///
    /// [`WorldStore::history`] already decides `Full` vs `Degraded` against the
    /// retained citations and summaries. This carries that verdict across
    /// unchanged, exactly as [`Self::ask_as_of`] does. A second judgement here
    /// would be a second implementation of the rule governing whether an answer
    /// can be trusted, and the two would drift.
    ///
    /// That verdict is deliberately CONSERVATIVE and this does not sharpen it:
    /// the store's citation check is store-wide, so a retained citation with no
    /// summaries degrades every subject's history, including subjects the
    /// compacted span never touched. Over-reporting `Degraded` is the safe
    /// direction and the acceptance rule this family is held to — *"never
    /// report `Full` when required evidence may have been removed"* — permits
    /// it. Narrowing the check to the queried subject would turn a conservative
    /// signal into an exact-loss detector, and an exact-loss detector that is
    /// wrong is silent.
    ///
    /// # Errors
    ///
    /// As [`Self::ask`].
    pub fn history(&self, subject: &str) -> Result<TemporalLookup, AskError> {
        let answer = self.store.history(subject)?;
        let completeness = answer.resolution;

        let mut answers = Vec::new();
        for claim in &answer.claims {
            // Resolved and DISCARDED, deliberately. The `?` is the entire
            // reason this call is here: it propagates 3e's refusal for an
            // unclassified claim. The budget it yields is then handed to
            // `assemble` for the recorded grade, and is NOT used to filter.
            let budget = self.policy_for(claim)?.budget();
            answers.push(assemble(
                claim,
                // The claim's OWN transaction time is the clock, not a caller's
                // "now". A history entry's grade describes what the claim was
                // when it was made; grading the whole record against the moment
                // somebody happened to run the query would make the same
                // history read differently on every call.
                claim.txn_time_ms.max(0).unsigned_abs(),
                budget,
                // A replay does not resolve identity — the same limit
                // `ask_as_of` states, for the same reason: identity is a second
                // projection with its own coordinate.
                ObjectIdentity::NotResolvedInReplay,
            )?);
        }

        // `NoneAdmissible` is unreachable here BY CONSTRUCTION, because nothing
        // is filtered. An empty history means the log holds no confirmed claim
        // for this subject — and if evidence was compacted away, the
        // completeness alongside says so rather than the reason code.
        let lookup = if answers.is_empty() {
            WorldLookup::Unknown(UnknownReason::NoClaim)
        } else {
            WorldLookup::Answered(answers)
        };
        Ok(TemporalLookup {
            lookup,
            completeness,
            semantics: SemanticVersions::for_query(QueryKind::SubjectHistory),
        })
    }

    /// **The aggregate summary for `subject`, and how much of the evidence
    /// behind it survives** — the 3g follow-up.
    ///
    /// The read is BOUNDED — see
    /// [`WorldStore::subject_summary_with_coverage`], which narrows both of its
    /// sources in SQL rather than scanning the store.
    ///
    /// # Errors
    ///
    /// [`AskError::Store`] on any read failure.
    pub fn subject_summary(&self, subject: &str) -> Result<SummaryLookup, AskError> {
        // The BOUNDED per-subject read, not the bulk call filtered down. The
        // bulk one loads every retained row and every citation in the store and
        // groups once — right for "every subject", quadratically wrong as the
        // basis for "this subject". Caught in review on #1441, and the same
        // finding as #1440's unbounded lineage fetch against the same clause.
        let found = self.store.subject_summary_with_coverage(subject)?;

        Ok(SummaryLookup {
            summary: found,
            semantics: SemanticVersions::for_query(QueryKind::SubjectSummary),
        })
    }

    /// **The ruled disposition for one claim, or a refusal.**
    ///
    /// The single place a claim's semantics become a freshness policy, so the
    /// fail-closed rule cannot be routed around by a second lookup that forgot
    /// to refuse.
    ///
    /// Every caller resolves it **once per claim** and passes the resulting
    /// budget onward. An earlier draft called this twice — once behind an
    /// `admissible` wrapper and again when binding — which meant the refusal
    /// that makes 3e fail closed was evaluated twice for one claim, and would
    /// have kept working had one of the two sites stopped refusing. Caught in
    /// review on #1439.
    fn policy_for(&self, claim: &ProjectedClaim) -> Result<FreshnessPolicy, AskError> {
        resolve_policy(self.freshness, &claim.kind, claim.predicate.as_deref())
    }

    // SEMANTICS-PIN-BEGIN: answer_admissibility
    //
    // The boundary's own versioned rule — see `crate::semantics`. Digested by
    // `ci/check_world_semantics.py`; changing what it SERVES moves the corpus
    // digest too, and then `BOUNDARY_SEMANTICS`' version must be bumped so
    // recorded references refuse rather than replay under the new rule.

    /// Expired, or graded `Inadmissible`, is not servable.
    ///
    /// An **unlabelled** claim is admitted: it has no axes to grade, and
    /// refusing it would invent a trust judgement from the absence of one — the
    /// mirror of manufacturing a default grade.
    fn is_admissible(claim: &ProjectedClaim, clock: u64, budget: Option<u64>) -> bool {
        if claim.validity_at(clock, budget) == Validity::Expired {
            return false;
        }
        !matches!(
            claim.grade_at(clock, budget),
            Some(TrustGrade::Inadmissible)
        )
    }

    // SEMANTICS-PIN-END: answer_admissibility

    /// Bind a live claim into an answer, resolving its object identity.
    ///
    /// Delegates the field-by-field construction to [`assemble`], which is the
    /// one place a `WorldAnswer` is built; this adds only the identity
    /// resolution a live read can do and a pinned one cannot.
    ///
    /// Takes the budget rather than resolving it, so the caller's single
    /// [`Self::policy_for`] call is the only one — see that method.
    fn bind(
        claim: &ProjectedClaim,
        clock: u64,
        budget: Option<u64>,
        identity: &IdentityView,
        identity_is_current: bool,
    ) -> Result<WorldAnswer, AskError> {
        assemble(
            claim,
            clock,
            budget,
            Self::resolve_object(claim.object.as_deref(), identity, identity_is_current),
        )
    }

    /// Resolve one claim's object through the identity graph.
    ///
    /// The staleness check comes FIRST and is not an optimisation. `resolve`
    /// answers `Unknown` for an id the graph does not hold — and a graph that
    /// has not folded the adjudications naming that id does not hold it either,
    /// so a stale graph and an honest "not an entity" are the same answer from
    /// the resolver. Only the log can tell them apart, which is why
    /// [`ReadSnapshot::identity_is_current`] is consulted rather than the
    /// projection's own emptiness.
    ///
    /// [`ReadSnapshot::identity_is_current`]: kirra_world_store::snapshot::ReadSnapshot::identity_is_current
    fn resolve_object(
        object: Option<&str>,
        identity: &IdentityView,
        identity_is_current: bool,
    ) -> ObjectIdentity {
        let Some(object) = object else {
            return ObjectIdentity::NoObject;
        };
        if !identity_is_current {
            return ObjectIdentity::Unresolvable;
        }
        let Ok(id) = EntityId::new(object) else {
            return ObjectIdentity::Malformed;
        };
        match resolve(identity, &id) {
            ResolutionOutcome::Located { entity, hops } => ObjectIdentity::Resolved {
                entity: entity.as_str().to_string(),
                hops,
            },
            ResolutionOutcome::Ambiguous { successors } => ObjectIdentity::Ambiguous {
                successors: successors.iter().map(|e| e.as_str().to_string()).collect(),
            },
            ResolutionOutcome::Unknown => ObjectIdentity::NotAnEntity,
            ResolutionOutcome::Refused(reason) => ObjectIdentity::Refused(reason),
        }
    }
}

/// The boundary's admissibility rule, for a generation-pinned claim.
///
/// `pub(crate)` so [`crate::answer_ref::AnswerRef::resolve`] applies the SAME
/// test as a live `ask`, rather than a copy that could drift. A ref that
/// admitted claims the boundary would refuse would be re-executing a different
/// query than the one it names.
pub(crate) fn is_admissible_for_ref(
    claim: &ProjectedClaim,
    clock: u64,
    budget: Option<u64>,
) -> bool {
    WorldView::is_admissible(claim, clock, budget)
}

/// **Build a `WorldAnswer` from a claim and the identity of the same pin — 3h.**
///
/// The identity view must come from the SAME
/// [`read_composed_at_generation`][rc] call as the claim. That is not enforced
/// by the type system here; it is enforced upstream, by that call being the only
/// way to obtain the pair — which is why `AnswerRef::resolve` uses it rather
/// than fetching the halves itself.
///
/// # Why `identity_is_current` is `true` and not a check
///
/// The live path consults [`ReadSnapshot::identity_is_current`][ic] because a
/// live read reads a *projection* that may lag the log. A pinned composition
/// does not read the projection at all: it folds the adjudications itself, up
/// to the pinned generation, so the graph is complete at that coordinate by
/// construction. Passing `true` states a fact about the reconstruction rather
/// than skipping a check — and passing the live gate here would be actively
/// wrong, since it answers a question about a different object.
///
/// [rc]: kirra_world_store::snapshot::ReadSnapshot::read_composed_at_generation
/// [ic]: kirra_world_store::snapshot::ReadSnapshot::identity_is_current
pub(crate) fn bind_composed(
    claim: &ProjectedClaim,
    clock: u64,
    budget: Option<u64>,
    identity: &IdentityView,
) -> Result<WorldAnswer, AskError> {
    assemble(
        claim,
        clock,
        budget,
        WorldView::resolve_object(claim.object.as_deref(), identity, true),
    )
}

/// **The one place a `WorldAnswer` is built** — every field populated together.
///
/// Private, and both producers go through it. Rule 1 says every answer carries
/// the value, the trust axes, the validity at the supplied clock and a
/// provenance handle; a second hand-written construction site is how one of
/// those quietly stops being populated, so there is exactly one.
///
/// Object identity is the ONE thing a caller supplies, because it is the one
/// thing that genuinely differs: a live read resolves it, a generation-pinned
/// read cannot without mixing coordinate axes.
///
/// Fallible only on the provenance handle, which is the one field carrying an
/// invariant the row cannot enforce.
fn assemble(
    claim: &ProjectedClaim,
    clock: u64,
    budget: Option<u64>,
    object_identity: ObjectIdentity,
) -> Result<WorldAnswer, AskError> {
    let provenance = EvidenceDigest::new(claim.chain_digest.clone()).map_err(|cause| {
        AskError::CorruptProvenance {
            subject: claim.subject.clone(),
            predicate: claim.predicate.clone(),
            cause,
        }
    })?;
    Ok(WorldAnswer {
        subject: claim.subject.clone(),
        predicate: claim.predicate.clone(),
        object_identity,
        object: claim.object.clone(),
        value: claim.payload.clone(),
        validity: claim.validity_at(clock, budget),
        axes: claim.trust,
        grade: claim.grade_at(clock, budget),
        provenance,
        event_id: claim.event_id.clone(),
    })
}

/// **One subject's summary and its evidence coverage** — the 3g follow-up.
///
/// # Why this carries `SummaryCoverage` and not `Resolution`
///
/// Every other family's completeness is a [`Resolution`], and making this one
/// match would be tidier. It would also require inventing a field.
///
/// The two signals are computed from different things and have different
/// shapes. `Resolution::Degraded` carries `{spans, summaries}` — a
/// RANGE-oriented verdict, built from the compaction citations overlapping a
/// queried interval. [`SummaryCoverage::Degraded`] carries `{summaries}` alone
/// — a PER-SUBJECT verdict, recorded by the fold about the evidence behind one
/// row. Coercing the second into the first means passing `spans: vec![]`, which
/// reads as *"no compacted span bore on this"*. That is false, and false in the
/// reassuring direction, which is the one direction this architecture must
/// never round toward.
///
/// So the native type crosses the boundary unchanged. One source of truth per
/// family beats one type across families.
///
/// # Numerical reconciliation is NOT evidence coverage
///
/// [`SubjectSummaryAnswer::reconciled_observation_count`] and
/// [`SubjectSummaryAnswer::reconciled_first_observed_ms`] can both reconstruct
/// their pre-compaction values from citations. It is tempting to read that
/// success as completeness — the numbers add up, so nothing was lost.
///
/// They are not the same claim, and the gap between them is exactly where a
/// silent loss would live. A citation names a SPAN; it does not name the events
/// inside it. So `observation_count` and `first_observed_ms` reconstruct, while
/// `provenance_head` and `last_event_id` cannot — and for a subject whose every
/// contributing event was compacted, [`SubjectSummaryAnswer::retained`] is
/// `None` and those fields do not exist at all. An answer reporting `Complete`
/// because two counts reconciled would be asserting something true of two
/// numbers about the whole answer contract.
///
/// [`Self::is_degraded`] therefore reads `coverage` and ONLY `coverage`. There
/// is deliberately no constructor taking a reconciliation result.
#[derive(Debug, Clone)]
pub struct SummaryLookup {
    summary: Option<SubjectSummaryAnswer>,
    semantics: SemanticVersions,
}

impl SummaryLookup {
    /// The summary and its coverage, or `None` if the store has never
    /// summarised this subject.
    ///
    /// `None` means *"not summarised"*, which is distinct from a summary whose
    /// evidence was compacted: that one is `Some`, with `Degraded` coverage
    /// and possibly no retained
    /// aggregates. The store goes to real trouble to keep those apart —
    /// a fully compacted subject used to vanish entirely — and collapsing them
    /// here would undo it.
    #[must_use]
    pub fn summary(&self) -> Option<&SubjectSummaryAnswer> {
        self.summary.as_ref()
    }

    /// Whether evidence behind this summary has been compacted away.
    ///
    /// Derived from `coverage` alone — never from whether reconciliation
    /// succeeded. See the type's own note on why those differ.
    ///
    /// An unsummarised subject is NOT degraded: there is no evidence claimed
    /// and none missing.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.summary
            .as_ref()
            .is_some_and(|s| s.coverage.is_degraded())
    }

    /// The rules this answer was produced under.
    #[must_use]
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }
}

/// **A bitemporal answer and its completeness** — Tier 3 box 3g.
///
/// The two are carried TOGETHER and neither is optional, because 3g's whole
/// claim is that completeness survives *independently of the payload outcome*.
///
/// # Completeness rides on `Unknown` too, and that is the point
///
/// The tempting shape is to attach a resolution only to `Answered` — an empty
/// answer has nothing to be incomplete about, so why carry it? Because
/// *"nothing was known"* and *"we deleted it"* are then the same value, which is
/// the exact silent rewrite §11.3 forbids and the failure an incident
/// reconstruction most needs to distinguish. An `Unknown` that is `Degraded` is
/// the most important thing this type can say.
#[derive(Debug, Clone)]
pub struct TemporalLookup {
    lookup: WorldLookup,
    completeness: Resolution,
    semantics: SemanticVersions,
}

impl TemporalLookup {
    /// What was found.
    #[must_use]
    pub fn lookup(&self) -> &WorldLookup {
        &self.lookup
    }

    /// Whether the answer is the whole truth, or what remains of it.
    #[must_use]
    pub fn completeness(&self) -> &Resolution {
        &self.completeness
    }

    /// Whether compaction could have borne on this answer.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.completeness.is_degraded()
    }

    /// **The rules this answer was produced under.**
    ///
    /// Box 3a said the envelope owns *"completeness, freshness, provenance and
    /// versions"*, and then excluded versions with a reason: *"no reducer
    /// version exists to carry. Minting one here would be the decorative
    /// metadata 3b forbids; it lands with 3b's enforcement."* 3b built that
    /// enforcement, so the field is no longer decorative and the exclusion is
    /// spent.
    ///
    /// **Carried, not enforced — and the difference matters.** A recorded
    /// `AnswerRef` REFUSES on a version mismatch; this states which rules
    /// produced the answer so a caller comparing two answers can see that they
    /// were not produced alike. There is no `as_of` ref to refuse yet, and
    /// pretending otherwise is exactly the overclaim 3b exists to prevent.
    #[must_use]
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }
}
