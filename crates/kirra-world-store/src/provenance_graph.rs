//! **The historical provenance graph — Tier 4 box 4b.**
//!
//! `KIRRA-WM-PROVENANCE-GRAPH-001`:
//!
//! > Materialize the citation relation at append; **do not materialize its
//! > resolved target.** Tier 4 reports the provenance graph that was resolvable
//! > at the answer's historical coordinate, not the graph that happens to be
//! > resolvable today.
//!
//! Box 4a built the first half and stopped deliberately: an edge says *"generation
//! G, at position N, cited observation id X"* and nothing more. This module is
//! the second half — where X is resolved **against the events visible at a
//! pinned generation**, which is the only coordinate at which the question has
//! a stable answer.
//!
//! # Why resolution is a query and not a column
//!
//! At generation *T*, a cited id may be carried by exactly one visible event, by
//! several, or by none. All three are ordinary, none is an error, and which one
//! holds **changes over time for a fixed citation**: `world_events.observation_id`
//! is indexed but not unique, nothing requires a cited id to exist, and an id
//! that resolves to nothing today may be carried by an event appended tomorrow.
//!
//! So the same edge has different correct answers at different pins, and any
//! design that stores the answer has silently chosen one of them — the one that
//! happened to hold at write time. That is 2d's trap one tier up, and the reason
//! 4a has nowhere to put a target column.
//!
//! # The four collapses this module exists to refuse
//!
//! Each is a way of turning *"I cannot tell"* into a confident, tidy, wrong
//! answer, and each has an outcome here instead:
//!
//! | Tempting collapse | What it destroys | Instead |
//! |---|---|---|
//! | plural → newest carrier | names ONE event as the source of a claim the store cannot attribute | [`CitationResolution::Plural`] |
//! | dangling → absent child | *"rested on nothing"* becomes indistinguishable from *"rested on evidence Kirra cannot find"* | [`CitationResolution::Dangling`] |
//! | uncovered index → empty citations | a positive claim about provenance, made about every source in an un-backfilled store | [`NodeCitations::BelowCoverageFloor`] |
//! | cycle → truncation | tells an operator to raise a limit, when the truth is that the evidence is circular | [`NotWalkedReason::CycleDetected`] |
//!
//! # It is a tree, not a shared-node graph
//!
//! Two paths reaching the same event produce two nodes. The alternative —
//! pointing both branches at one node — was rejected because a shared node has
//! no single depth or parent, so the two fields that make a node explainable
//! stop being well-defined exactly when the structure gets interesting. The
//! duplication is bounded by [`GraphSpec::max_nodes`], which is the same budget
//! that bounds everything else here.
//!
//! # Cycle detection is path membership, not visitation
//!
//! A branch closes a cycle when its target is **on the current path**, not when
//! it has been seen before. The distinction is the gray-set/black-set split the
//! fleet DAG traversal makes for the same reason, and conflating them is the
//! classic way a cycle check silently becomes a memoisation bug: a diamond —
//! two claims resting on one observation, which is ordinary provenance — would
//! be reported as malformed.

use crate::provenance_edges::{CitationEdge, MAX_CITATIONS_PAGE};

/// The deepest citation chain a walk will follow.
///
/// Rule 2 — *queries are bounded* — in the depth dimension. Provenance chains in
/// a real log are short; a walk that needs more than this is either malformed or
/// is being used for an export, which is a different (operational) question.
pub const MAX_PROVENANCE_DEPTH: usize = 32;

/// The most nodes a walk will materialize.
///
/// Depth alone does not bound a walk: a single event may cite any number of
/// observations, so a depth-1 graph is already unbounded in width. Two
/// dimensions, because one of them would leave the other open.
pub const MAX_PROVENANCE_NODES: usize = 256;

/// The most carriers considered for one citation.
///
/// Matches [`MAX_CITATIONS_PAGE`] for the reason that constant matches
/// `MAX_LINEAGE_PAGE`: these are all "evidence a caller is walking", and
/// separate ceilings would be separate numbers to keep in step for no stated
/// reason. Reaching it makes a citation [`CitationResolution::Plural`] with
/// `truncated` set — never a silent choice among the ones that fit.
pub const MAX_CARRIERS: usize = MAX_CITATIONS_PAGE;

/// The most compacted spans named on one dangling citation.
///
/// Matches [`MAX_CARRIERS`] for the same reason it matches [`MAX_CITATIONS_PAGE`]:
/// these are all "evidence a caller is walking", and separate ceilings would be
/// separate numbers to keep in step for no stated reason.
///
/// # Why capping this list is safe, and capping the *decision* would not be
///
/// The span list does two different jobs. It decides
/// [`DanglingReason::NeverVisible`] vs [`DanglingReason::PossiblyCompacted`],
/// which needs only to know whether **any** qualifying span exists — and it
/// enumerates the spans an investigator would go read, which is the part that
/// grows with the store's compaction history. Truncating the enumeration can
/// only ever shorten an already non-empty list, so it can never flip the
/// qualification back to `NeverVisible`. That direction is the whole point: this
/// module may over-report compaction at the cost of a caveat, and may never
/// under-report it, which would be a false claim that nothing was recorded.
pub const MAX_COMPACTED_SPANS: usize = MAX_CITATIONS_PAGE;

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// Which bound a [`GraphSpecError`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecDimension {
    /// [`GraphSpec::max_depth`].
    Depth,
    /// [`GraphSpec::max_nodes`].
    Nodes,
}

impl std::fmt::Display for SpecDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Depth => f.write_str("depth"),
            Self::Nodes => f.write_str("nodes"),
        }
    }
}

/// A walk bound that cannot be honoured.
///
/// Refusal rather than clamping, which is [`crate::lineage::PageSpecError`]'s
/// ruling and holds here for the same reason: a clamped bound answers a smaller
/// question and reports it as the one that was asked. It carries the dimension
/// because two bounds that failed identically would send a caller to the wrong
/// argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSpecError {
    /// A bound of zero, which cannot produce even the root.
    ZeroLimit {
        /// Which bound.
        dimension: SpecDimension,
    },
    /// A bound above the ceiling this module enforces.
    LimitTooLarge {
        /// Which bound.
        dimension: SpecDimension,
        /// What was asked for.
        requested: usize,
        /// The ceiling.
        maximum: usize,
    },
}

impl std::fmt::Display for GraphSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroLimit { dimension } => write!(f, "{dimension} limit must be non-zero"),
            Self::LimitTooLarge {
                dimension,
                requested,
                maximum,
            } => write!(f, "{dimension} limit {requested} exceeds maximum {maximum}"),
        }
    }
}

impl std::error::Error for GraphSpecError {}

/// The bounds one walk runs under.
///
/// Validated at construction so an out-of-range bound cannot reach the walk,
/// which is what lets the walk itself be total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSpec {
    max_depth: usize,
    max_nodes: usize,
}

impl GraphSpec {
    /// Build a spec, refusing a zero or over-ceiling bound in either dimension.
    ///
    /// # Errors
    ///
    /// [`GraphSpecError`] naming the offending dimension.
    pub fn new(max_depth: usize, max_nodes: usize) -> Result<Self, GraphSpecError> {
        for (value, dimension, maximum) in [
            (max_depth, SpecDimension::Depth, MAX_PROVENANCE_DEPTH),
            (max_nodes, SpecDimension::Nodes, MAX_PROVENANCE_NODES),
        ] {
            if value == 0 {
                return Err(GraphSpecError::ZeroLimit { dimension });
            }
            if value > maximum {
                return Err(GraphSpecError::LimitTooLarge {
                    dimension,
                    requested: value,
                    maximum,
                });
            }
        }
        Ok(Self {
            max_depth,
            max_nodes,
        })
    }

    /// The widest walk this module permits.
    ///
    /// Named rather than `Default` so a caller that wants the maximum says so:
    /// a default bound is the shape of bound people stop noticing.
    #[must_use]
    pub fn widest() -> Self {
        Self {
            max_depth: MAX_PROVENANCE_DEPTH,
            max_nodes: MAX_PROVENANCE_NODES,
        }
    }

    /// The deepest citation chain this walk will follow. The root is depth 0.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// The most nodes this walk will materialize, root included.
    #[must_use]
    pub fn max_nodes(&self) -> usize {
        self.max_nodes
    }
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// Why a cited observation id could not be attributed to a single event, or
/// that it could.
///
/// All three arms are **first-class**. `Plural` is not a degenerate `Resolved`
/// and `Dangling` is not an absent branch; both are answers, and the whole
/// point of the type is that a caller cannot obtain a target without having
/// been told which of the three it is holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationResolution {
    /// Exactly one visible event carries the cited id.
    Resolved {
        /// That event's generation.
        target_generation: i64,
    },
    /// Several visible events carry it.
    ///
    /// Reported in full rather than reduced. "Newest wins" is available and
    /// wrong: it names one event as the source of a claim the store cannot
    /// attribute, and it does so invisibly.
    Plural {
        /// The carriers' generations, ascending.
        target_generations: Vec<i64>,
        /// Whether more carriers exist than [`MAX_CARRIERS`] admits.
        truncated: bool,
    },
    /// No visible event carries it.
    Dangling {
        /// Whether anything could have been deleted from under it.
        reason: DanglingReason,
    },
}

/// Which kind of nothing a [`CitationResolution::Dangling`] is.
///
/// *Nothing ever carried this id* and *whatever carried it was compacted away*
/// look identical at the node — both are "no visible carrier" — and they are
/// completely different facts in an incident reconstruction. Inferring the first
/// from the second is the silent rewrite §11.3 forbids.
///
/// # Which way it is allowed to be wrong
///
/// The compacted reading is a **necessary condition**, deliberately, exactly as
/// [`crate::Resolution::Degraded`] is: a span that *could* have held a carrier
/// qualifies, whether or not it did. So this may say `PossiblyCompacted` where
/// the id never existed. It may never say `NeverVisible` where evidence was
/// deleted — over-reporting costs a caveat, under-reporting is a false claim
/// that nothing was ever recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DanglingReason {
    /// No compacted span at or below the pin could have carried it.
    NeverVisible,
    /// A compacted span could have. The spans are named so an investigator can
    /// go to the compaction citation rather than conclude nothing was there.
    PossiblyCompacted {
        /// The `lo_generation` of each qualifying span — the key of its
        /// compaction citation. At most [`MAX_COMPACTED_SPANS`] of them.
        ///
        /// May be empty **only** when `truncated` is set: that is a store which
        /// reported more qualifying spans than it returned and returned none the
        /// walk could keep. Naming nothing is honest there; saying
        /// `NeverVisible` would not be.
        spans: Vec<i64>,
        /// Whether more qualifying spans exist than [`MAX_COMPACTED_SPANS`]
        /// admits.
        ///
        /// Carried per-qualification rather than inferred from the list length,
        /// because a full list and a truncated one are the same length and mean
        /// different things — the second is not a complete account of what was
        /// deleted.
        truncated: bool,
    },
}

/// What happened below one citation branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchContinuation {
    /// The target's own provenance is in the tree.
    Walked {
        /// Index into [`ProvenanceTree::nodes`].
        node: usize,
    },
    /// It is not, and this is why.
    NotWalked(NotWalkedReason),
}

/// Why a branch was not walked.
///
/// Five reasons rather than one flag, because they call for five different
/// responses: nothing to do, a store that cannot attribute, raise a bound, raise
/// a different bound, and *the evidence is circular*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotWalkedReason {
    /// The citation dangles — there is nothing to walk.
    Nothing,
    /// The citation is plural.
    ///
    /// Walking every carrier is the other defensible choice and is rejected
    /// here: the tree's meaning is that every walked edge is a **determinate**
    /// provenance link, and expanding an ambiguous one would put evidence under
    /// a claim without being able to say it belongs there. The carriers are
    /// named in the resolution, so a caller that wants them can ask again.
    Plural,
    /// [`GraphSpec::max_depth`] was reached.
    DepthLimit,
    /// [`GraphSpec::max_nodes`] was reached.
    NodeLimit,
    /// The target is already on the path from the root — following it would
    /// loop.
    CycleDetected {
        /// The generation the path returns to.
        back_to_generation: i64,
    },
}

/// One recorded citation, resolved at the pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Position in the source's provenance array, dense from 0.
    pub ordinal: i64,
    /// The observation id the source claimed to cite, verbatim.
    pub cited_observation_id: String,
    /// What it resolved to at this coordinate.
    pub resolution: CitationResolution,
    /// What happened below it.
    pub continuation: BranchContinuation,
}

/// What is known about one event's citations.
///
/// Three cases rather than a `Vec`, because an empty `Vec` would mean all three
/// and they are not the same claim. The first is an assertion about the source;
/// the other two are admissions about the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeCitations {
    /// The index covers this generation. An empty `branches` here **is** the
    /// claim that the source cited nothing, and is only reachable when the
    /// coverage floor supports it.
    Indexed {
        /// The citations, in recorded order, duplicates kept.
        branches: Vec<Branch>,
        /// Whether the source has more citations than one page admits.
        truncated: bool,
    },
    /// The citation index does not cover this generation, so what this event
    /// cited is **unknown**. Box 4a's floor, consumed.
    BelowCoverageFloor,
    /// The event itself was compacted away, and its edges went with it. What it
    /// cited is unknowable: the surviving index is not evidence and must not
    /// stand in for the deleted statement.
    EvidenceCompacted,
}

/// One event in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceNode {
    /// The event this node stands for.
    pub generation: i64,
    /// Distance from the root, which is 0.
    pub depth: usize,
    /// Index of the node that cited this one, `None` for the root.
    pub parent: Option<usize>,
    /// Which citation of the parent reached it, `None` for the root.
    pub via_ordinal: Option<i64>,
    /// What this event cited.
    pub citations: NodeCitations,
}

/// The four ways a tree can be less than the whole answer.
///
/// A struct of independent facts rather than an enum, deliberately. An enum
/// would force a precedence among things that are all true at once — a walk can
/// hit a bound AND find a cycle AND cross deleted evidence — and whichever
/// arm lost the ordering would be invisible. The ruling names `Truncated` and
/// `CycleDetected` as distinct outcomes; two booleans are strictly more distinct
/// than two arms of which only one survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphOutcome {
    /// A legitimate bound was reached: depth, node budget, or a citation page.
    pub truncated: bool,
    /// Provenance loops back on itself. Malformed, not bounded — raising a
    /// limit cannot help.
    pub cycle_detected: bool,
    /// Evidence was deleted by compaction: a source's own statement, or a
    /// possible carrier of a citation.
    pub degraded: bool,
    /// At least one node's citations are unknown because the index does not
    /// cover its generation. Distinct from `degraded`: nothing was deleted, the
    /// index simply makes no claim, and a backfill fixes it.
    pub coverage_limited: bool,
}

impl GraphOutcome {
    /// Whether the tree is the whole provenance graph at the pin.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.truncated && !self.cycle_detected && !self.degraded && !self.coverage_limited
    }

    /// Whether compaction removed evidence this answer would otherwise contain.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }
}

/// A source event's provenance, resolved at one historical coordinate.
///
/// The pin is carried in the answer rather than left to the caller because the
/// answer is only meaningful with it: the same root at a different generation is
/// a different and equally correct tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceTree {
    /// The event being explained.
    pub root_generation: i64,
    /// The coordinate it was explained at.
    pub at_generation: i64,
    /// Nodes in pre-order, root first. Indices are stable within one tree and
    /// are what [`BranchContinuation::Walked`] refers to.
    pub nodes: Vec<ProvenanceNode>,
    /// Whether anything is missing, and why.
    pub outcome: GraphOutcome,
    /// The declared version of the rule that produced this tree.
    ///
    /// Carried for the reason a recorded answer carries its rule versions: a
    /// tree compared against one produced under different semantics is
    /// comparing two different questions.
    pub rule_version: u32,
}

// ---------------------------------------------------------------------------
// The storage seam
// ---------------------------------------------------------------------------

/// The carriers of one observation id at a pin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Carriers {
    /// Generations of visible events carrying the id, ascending.
    pub generations: Vec<i64>,
    /// Whether more exist than [`MAX_CARRIERS`] admits.
    pub truncated: bool,
}

/// The compacted spans that could bear on one pin.
///
/// The bounded counterpart of [`Carriers`], and bounded for the same reason: a
/// walk must not have a dimension that grows with the store. Here that dimension
/// is the compaction history, which grows for as long as retention runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactedSpans {
    /// Qualifying spans as `(lo_generation, hi_generation)`, ascending by `lo`,
    /// at most [`MAX_COMPACTED_SPANS`] of them.
    pub spans: Vec<(i64, i64)>,
    /// Whether more qualify than were returned.
    pub truncated: bool,
}

/// **What a walk needs from storage** — five primitive questions, no judgement.
///
/// The split is deliberate and matches [`crate::lineage::select_lineage`]'s: an
/// implementation answers narrow factual questions, and every decision that can
/// change what a tree *says* lives in [`walk_provenance`], in one place, under
/// one version. An implementation that filtered, ranked or deduplicated would be
/// making the rule, and two implementations would then be two rules.
pub trait CitationLookup {
    /// Whatever the backing store fails with.
    type Error;

    /// The highest generation the citation index does **not** cover.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn coverage_floor(&self) -> Result<i64, Self::Error>;

    /// Whether the event at `generation` is still in the log.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn is_retained(&self, generation: i64) -> Result<bool, Self::Error>;

    /// One page of the citations recorded by `generation`, in recorded order.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn citations(&self, generation: i64) -> Result<(Vec<CitationEdge>, bool), Self::Error>;

    /// Events carrying `observation_id` at or below `at_generation`, ascending,
    /// at most [`MAX_CARRIERS`] of them plus a truncation flag.
    ///
    /// The generation filter is stated here **and re-applied** by the walk. An
    /// implementation may push it into an index — that is what the SQL one does
    /// — but the bound is a judgement about historical visibility, so the walk
    /// does not depend on the implementation having got it right.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn carriers(&self, observation_id: &str, at_generation: i64) -> Result<Carriers, Self::Error>;

    /// Compacted spans that could bear on `at_generation`, ascending by `lo`,
    /// at most [`MAX_COMPACTED_SPANS`] of them plus a truncation flag.
    ///
    /// The pin is stated here **and re-applied** by the walk, exactly as
    /// [`CitationLookup::carriers`]' bound is: an implementation may push the
    /// filter into SQL — that is what the store does, and it is what makes this
    /// bounded — but which spans bear on a pin is a judgement, so the walk does
    /// not depend on the implementation having got it right.
    ///
    /// Ascending by `lo` is load-bearing rather than cosmetic: a limit has to
    /// drop something, and dropping a deterministic something is what keeps two
    /// walks at the same pin returning the same answer.
    ///
    /// # Errors
    ///
    /// Implementation-defined.
    fn compacted_spans(&self, at_generation: i64) -> Result<CompactedSpans, Self::Error>;
}

/// What a walk refuses, as distinct from what its storage failed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkError<E> {
    /// The root is at or below the coverage floor, so its citations are
    /// unknown. Refused rather than answered, because an empty provenance here
    /// would be a confident claim the index does not support.
    IndexIncomplete {
        /// The root that was asked about.
        requested: i64,
        /// The floor at the time of asking.
        floor: i64,
    },
    /// The backing store failed.
    Lookup(E),
}

// SEMANTICS-PIN-BEGIN: citation_resolution
//
// The versioned rule — see `crate::semantics`. Digested by
// `ci/check_world_semantics.py`; changing how a citation resolves, which
// branches are walked, the order nodes come back in, or where a walk stops moves
// the corpus digest, and then `SEMANTICS`' version must be bumped so recorded
// provenance answers refuse rather than replay under the new rule.

/// **What one cited observation id resolves to at a pin.**
///
/// Three decisions, each of which changes what an explanation says:
///
/// 1. **Visibility** — a carrier counts only at or below `at_generation`. This
///    is the historical-correctness rule: an event appended after the pinned
///    coordinate was not evidence then, and counting it would resolve the graph
///    that happens to be resolvable today.
/// 2. **Cardinality** — zero, one, or several, kept as three outcomes. One
///    carrier resolves; several are reported in full; none dangles.
/// 3. **The dangling qualification** — a compacted span at or below the pin
///    could have held a carrier, so a dangle in its shadow is reported as
///    possibly-deleted rather than never-recorded.
///
/// Carriers are re-filtered here even though the storage seam is asked for a
/// bounded, pre-filtered set: the filter is a judgement, and a judgement that
/// lives in two places has two versions.
#[must_use]
pub fn resolve_citation(
    carriers: &Carriers,
    at_generation: i64,
    compacted: &CompactedSpans,
) -> CitationResolution {
    let mut visible: Vec<i64> = carriers
        .generations
        .iter()
        .copied()
        .filter(|g| *g <= at_generation)
        .collect();
    visible.sort_unstable();

    // Truncation forces plural regardless of what fits: more carriers exist, so
    // "exactly one" is false even when one came back.
    if carriers.truncated || visible.len() > 1 {
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

    // A span whose first removed generation is at or below the pin could have
    // held a carrier. Its hi bound is deliberately not consulted: a span
    // straddling the pin still removed events the pin could see.
    let mut spans: Vec<i64> = compacted
        .spans
        .iter()
        .filter(|(lo, _hi)| *lo <= at_generation)
        .map(|(lo, _hi)| *lo)
        .collect();
    let truncated = compacted.truncated || spans.len() > MAX_COMPACTED_SPANS;
    spans.truncate(MAX_COMPACTED_SPANS);
    CitationResolution::Dangling {
        // `NeverVisible` requires BOTH that no span qualifies AND that the store
        // is not holding any back. An empty list under truncation means the
        // store said more exist than it returned — naming none of them is the
        // honest answer there, and claiming nothing was ever recorded is not.
        reason: if spans.is_empty() && !truncated {
            DanglingReason::NeverVisible
        } else {
            DanglingReason::PossiblyCompacted { spans, truncated }
        },
    }
}

/// **Walk a source event's provenance at a pin.**
///
/// Pre-order depth-first from `root_generation`, resolving each citation with
/// [`resolve_citation`] and descending only into determinate ones. Four
/// decisions beyond the resolution rule:
///
/// 1. **Order** — pre-order DFS, the root at index 0. Total and stable, which is
///    what makes `Walked { node }` a meaningful reference.
/// 2. **What is walked** — a `Resolved` target only. Dangling has nothing below
///    it, and plural is ambiguous ([`NotWalkedReason::Plural`] records why
///    expanding it was rejected).
/// 3. **Where it stops** — the node budget counts every materialized node
///    including the root; the depth budget counts edges from the root. Both are
///    truncation.
/// 4. **A cycle is path membership** — a target already on the path from the
///    root, which a diamond is not.
///
/// # Errors
///
/// [`WalkError::IndexIncomplete`] when the root is at or below the coverage
/// floor, or [`WalkError::Lookup`] from the storage seam.
pub fn walk_provenance<S: CitationLookup>(
    lookup: &S,
    root_generation: i64,
    at_generation: i64,
    spec: GraphSpec,
    rule_version: u32,
) -> Result<ProvenanceTree, WalkError<S::Error>> {
    let floor = lookup.coverage_floor().map_err(WalkError::Lookup)?;
    // The root is refused rather than reported: a caller asking to explain this
    // event gets nothing it could mistake for "it cited nothing". A DESCENDANT
    // below the floor is different — the question was about the root, and the
    // honest answer names the part it could not reach.
    if root_generation <= floor {
        return Err(WalkError::IndexIncomplete {
            requested: root_generation,
            floor,
        });
    }
    // Once per walk, not once per node: the pin is fixed for the whole walk, so
    // every branch resolves against the same bounded set.
    let spans = lookup
        .compacted_spans(at_generation)
        .map_err(WalkError::Lookup)?;

    let mut walk = Walk {
        lookup,
        at_generation,
        spec,
        floor,
        spans,
        nodes: Vec::new(),
        path: Vec::new(),
        outcome: GraphOutcome::default(),
    };
    walk.expand(root_generation, 0, None, None)?;
    Ok(ProvenanceTree {
        root_generation,
        at_generation,
        nodes: walk.nodes,
        outcome: walk.outcome,
        rule_version,
    })
}

struct Walk<'a, S: CitationLookup> {
    lookup: &'a S,
    at_generation: i64,
    spec: GraphSpec,
    floor: i64,
    spans: CompactedSpans,
    nodes: Vec<ProvenanceNode>,
    /// The generations on the path from the root — the gray set. Membership,
    /// not visitation: a node already popped is a diamond, not a cycle.
    path: Vec<i64>,
    outcome: GraphOutcome,
}

impl<S: CitationLookup> Walk<'_, S> {
    fn expand(
        &mut self,
        generation: i64,
        depth: usize,
        parent: Option<usize>,
        via_ordinal: Option<i64>,
    ) -> Result<usize, WalkError<S::Error>> {
        // Reserve the index before descending, so children can refer to their
        // parent and so the node budget counts this node against its siblings.
        let index = self.nodes.len();
        self.nodes.push(ProvenanceNode {
            generation,
            depth,
            parent,
            via_ordinal,
            citations: NodeCitations::BelowCoverageFloor,
        });

        let citations = self.citations_of(generation, depth, index)?;
        self.nodes[index].citations = citations;
        Ok(index)
    }

    /// `index` is the node being expanded — the parent of everything this call
    /// descends into. It is threaded in rather than recovered from the node
    /// vector's tail: after a branch walks deeper, the last node is somewhere
    /// in that branch's subtree, not the node doing the citing.
    fn citations_of(
        &mut self,
        generation: i64,
        depth: usize,
        index: usize,
    ) -> Result<NodeCitations, WalkError<S::Error>> {
        // Order matters: a compacted event has no edges BECAUSE it is gone, and
        // reporting that as a coverage gap would send an operator to a backfill
        // that cannot bring the statement back.
        if !self
            .lookup
            .is_retained(generation)
            .map_err(WalkError::Lookup)?
        {
            self.outcome.degraded = true;
            return Ok(NodeCitations::EvidenceCompacted);
        }
        if generation <= self.floor {
            self.outcome.coverage_limited = true;
            return Ok(NodeCitations::BelowCoverageFloor);
        }

        let (edges, truncated) = self
            .lookup
            .citations(generation)
            .map_err(WalkError::Lookup)?;
        if truncated {
            self.outcome.truncated = true;
        }

        self.path.push(generation);
        let mut branches = Vec::with_capacity(edges.len());
        for edge in edges {
            let carriers = self
                .lookup
                .carriers(&edge.cited_observation_id, self.at_generation)
                .map_err(WalkError::Lookup)?;
            let resolution = resolve_citation(&carriers, self.at_generation, &self.spans);
            if matches!(
                resolution,
                CitationResolution::Dangling {
                    reason: DanglingReason::PossiblyCompacted { .. }
                }
            ) {
                self.outcome.degraded = true;
            }
            let continuation = self.continue_into(&resolution, depth, edge.ordinal, index)?;
            branches.push(Branch {
                ordinal: edge.ordinal,
                cited_observation_id: edge.cited_observation_id,
                resolution,
                continuation,
            });
        }
        self.path.pop();

        Ok(NodeCitations::Indexed {
            branches,
            truncated,
        })
    }

    fn continue_into(
        &mut self,
        resolution: &CitationResolution,
        depth: usize,
        ordinal: i64,
        parent: usize,
    ) -> Result<BranchContinuation, WalkError<S::Error>> {
        let target = match resolution {
            CitationResolution::Resolved { target_generation } => *target_generation,
            CitationResolution::Plural { .. } => {
                return Ok(BranchContinuation::NotWalked(NotWalkedReason::Plural))
            }
            CitationResolution::Dangling { .. } => {
                return Ok(BranchContinuation::NotWalked(NotWalkedReason::Nothing))
            }
        };
        // A cycle is checked FIRST: it is a fact about the evidence, and a
        // malformed loop that happened to also exceed a bound must not be
        // reported as a bound, which would send an operator to raise it.
        if self.path.contains(&target) {
            self.outcome.cycle_detected = true;
            return Ok(BranchContinuation::NotWalked(
                NotWalkedReason::CycleDetected {
                    back_to_generation: target,
                },
            ));
        }
        if depth + 1 > self.spec.max_depth() {
            self.outcome.truncated = true;
            return Ok(BranchContinuation::NotWalked(NotWalkedReason::DepthLimit));
        }
        if self.nodes.len() >= self.spec.max_nodes() {
            self.outcome.truncated = true;
            return Ok(BranchContinuation::NotWalked(NotWalkedReason::NodeLimit));
        }
        let node = self.expand(target, depth + 1, Some(parent), Some(ordinal))?;
        Ok(BranchContinuation::Walked { node })
    }
}

// SEMANTICS-PIN-END: citation_resolution

// ---------------------------------------------------------------------------
// An in-memory lookup
// ---------------------------------------------------------------------------

/// A [`CitationLookup`] over plain maps.
///
/// Two jobs, both real. It is the semantics corpus's backing store, so the
/// versioned rule is exercised without a database in the loop — and it is the
/// portability proof that the walk depends on the seam and not on SQLite, the
/// same role `InMemoryEpochFence` plays for the persistence crate's fences.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCitations {
    /// Retained events, `generation → observation_id`.
    events: std::collections::BTreeMap<i64, String>,
    /// Recorded citations, `generation → cited observation ids in order`.
    edges: std::collections::BTreeMap<i64, Vec<String>>,
    /// The coverage floor.
    floor: i64,
    /// Compacted spans, `(lo, hi)`.
    spans: Vec<(i64, i64)>,
}

impl InMemoryCitations {
    /// An empty store with a fully-covering index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a retained event carrying `observation_id` and citing `cited`.
    #[must_use]
    pub fn with_event(mut self, generation: i64, observation_id: &str, cited: &[&str]) -> Self {
        self.events.insert(generation, observation_id.to_string());
        self.edges
            .insert(generation, cited.iter().map(|c| (*c).to_string()).collect());
        self
    }

    /// Record citations for a generation whose event is NOT retained — the
    /// shape a compacted source would have if its edges had survived it, which
    /// is what makes the retention check observable.
    #[must_use]
    pub fn with_unretained_edges(mut self, generation: i64, cited: &[&str]) -> Self {
        self.edges
            .insert(generation, cited.iter().map(|c| (*c).to_string()).collect());
        self
    }

    /// Set the coverage floor.
    #[must_use]
    pub fn with_floor(mut self, floor: i64) -> Self {
        self.floor = floor;
        self
    }

    /// Record a compacted span.
    #[must_use]
    pub fn with_compacted_span(mut self, lo: i64, hi: i64) -> Self {
        self.spans.push((lo, hi));
        self
    }
}

impl CitationLookup for InMemoryCitations {
    type Error = std::convert::Infallible;

    fn coverage_floor(&self) -> Result<i64, Self::Error> {
        Ok(self.floor)
    }

    fn is_retained(&self, generation: i64) -> Result<bool, Self::Error> {
        Ok(self.events.contains_key(&generation))
    }

    fn citations(&self, generation: i64) -> Result<(Vec<CitationEdge>, bool), Self::Error> {
        let cited = self.edges.get(&generation).cloned().unwrap_or_default();
        let all = crate::provenance_edges::citation_edges(&cited);
        let truncated = all.len() > MAX_CITATIONS_PAGE;
        Ok((
            all.into_iter().take(MAX_CITATIONS_PAGE).collect(),
            truncated,
        ))
    }

    fn carriers(&self, observation_id: &str, at_generation: i64) -> Result<Carriers, Self::Error> {
        let mut generations: Vec<i64> = self
            .events
            .iter()
            .filter(|(g, obs)| **g <= at_generation && obs.as_str() == observation_id)
            .map(|(g, _)| *g)
            .collect();
        let truncated = generations.len() > MAX_CARRIERS;
        generations.truncate(MAX_CARRIERS);
        Ok(Carriers {
            generations,
            truncated,
        })
    }

    fn compacted_spans(&self, at_generation: i64) -> Result<CompactedSpans, Self::Error> {
        // The same shape as the store's SQL: filter to the pin, order by `lo`,
        // probe one past the ceiling so a full page is not mistaken for a cut
        // one. The portability proof is only a proof if both do this the same.
        let mut qualifying: Vec<(i64, i64)> = self
            .spans
            .iter()
            .copied()
            .filter(|(lo, _hi)| *lo <= at_generation)
            .collect();
        qualifying.sort_unstable();
        let truncated = qualifying.len() > MAX_COMPACTED_SPANS;
        qualifying.truncate(MAX_COMPACTED_SPANS);
        Ok(CompactedSpans {
            spans: qualifying,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: u32 = 1;

    fn walk(lookup: &InMemoryCitations, root: i64, at: i64) -> ProvenanceTree {
        walk_provenance(lookup, root, at, GraphSpec::widest(), V).expect("walk")
    }

    fn only(tree: &ProvenanceTree, node: usize) -> &Branch {
        match &tree.nodes[node].citations {
            NodeCitations::Indexed { branches, .. } => {
                assert_eq!(branches.len(), 1, "expected one citation");
                &branches[0]
            }
            other => panic!("no indexed citations: {other:?}"),
        }
    }

    /// **Truncating the span list can never turn a compacted dangle into a
    /// never-recorded one.**
    ///
    /// The load-bearing property of the bound, and first for that reason. The
    /// qualification asks whether ANY span could have held a carrier; the list
    /// only says which. So a cap may shorten the answer and may never reverse
    /// it — the direction this module's `DanglingReason` docs call the
    /// difference between a caveat and a false claim.
    #[test]
    fn a_truncated_span_list_still_qualifies_the_dangle() {
        let compacted = CompactedSpans {
            spans: vec![(2, 3), (4, 5)],
            truncated: true,
        };
        let resolution = resolve_citation(&Carriers::default(), 9, &compacted);
        match resolution {
            CitationResolution::Dangling {
                reason: DanglingReason::PossiblyCompacted { spans, truncated },
            } => {
                assert_eq!(spans, vec![2, 4], "the spans that fit are still named");
                assert!(truncated, "and the caller is told more exist");
            }
            other => panic!("truncation must not change the qualification: {other:?}"),
        }
    }

    /// The same property at its sharpest: a store that reports more spans than
    /// it returned, and returns none the walk can keep.
    ///
    /// An empty list is normally `NeverVisible`. Under truncation it must not
    /// be — "I am holding some back" and "there were none" are opposite claims,
    /// and only one of them is safe to guess wrong.
    #[test]
    fn an_empty_but_truncated_span_list_is_not_never_visible() {
        let compacted = CompactedSpans {
            // Everything returned is above the pin, so the walk's re-filter
            // drops it — while the store still says more qualify.
            spans: vec![(50, 60)],
            truncated: true,
        };
        let resolution = resolve_citation(&Carriers::default(), 9, &compacted);
        match resolution {
            CitationResolution::Dangling {
                reason: DanglingReason::PossiblyCompacted { spans, truncated },
            } => {
                assert!(spans.is_empty(), "there is nothing it can honestly name");
                assert!(truncated, "so the flag is all that carries the fact");
            }
            other => panic!("an empty truncated list must not read as never-recorded: {other:?}"),
        }
    }

    /// Nothing held back and nothing qualifying is still `NeverVisible`.
    ///
    /// The negative control for the two above: without it, they are equally
    /// satisfied by a rule that answered `PossiblyCompacted` unconditionally,
    /// which would destroy the distinction the whole arm exists to draw.
    #[test]
    fn no_qualifying_span_and_no_truncation_is_never_visible() {
        let compacted = CompactedSpans {
            spans: vec![(50, 60)],
            truncated: false,
        };
        let resolution = resolve_citation(&Carriers::default(), 9, &compacted);
        assert_eq!(
            resolution,
            CitationResolution::Dangling {
                reason: DanglingReason::NeverVisible
            }
        );
    }

    /// The walk caps the enumeration itself, not only its storage.
    ///
    /// The seam's ceiling is re-applied here for `carriers`' reason: an
    /// implementation may push the bound into SQL, but the bound is a judgement,
    /// so a store that ignored it must not make the walk unbounded.
    #[test]
    fn the_walk_caps_the_span_list_even_if_storage_did_not() {
        let over = MAX_COMPACTED_SPANS + 7;
        let compacted = CompactedSpans {
            spans: (0..over).map(|i| (i as i64, i as i64)).collect(),
            truncated: false,
        };
        let resolution = resolve_citation(&Carriers::default(), i64::MAX, &compacted);
        match resolution {
            CitationResolution::Dangling {
                reason: DanglingReason::PossiblyCompacted { spans, truncated },
            } => {
                assert_eq!(spans.len(), MAX_COMPACTED_SPANS, "capped by the walk");
                assert!(truncated, "and reported as capped, not as complete");
            }
            other => panic!("expected a compacted dangle: {other:?}"),
        }
    }

    /// Every node's parent must be the node that actually cited it, including
    /// the second and later branches of a node whose first branch walked
    /// deeper.
    ///
    /// The walk used to take the parent as "the last node appended", which is
    /// the citing node only while no branch has descended. Once one has, the
    /// tail is somewhere inside that branch's subtree, and every later sibling
    /// was attached to it — producing a tree whose shape disagrees with the
    /// evidence it was built from. A renderer reading that tree attributes a
    /// claim to whatever the previous branch happened to end on.
    #[test]
    fn a_later_branch_hangs_from_its_citing_node_not_the_previous_subtree() {
        // gen 10 cites obs-a and obs-b. obs-a's carrier (gen 8) cites obs-c,
        // so the FIRST branch is two nodes deep before the second is walked.
        let store = InMemoryCitations::new()
            .with_event(10, "obs-root", &["obs-a", "obs-b"])
            .with_event(8, "obs-a", &["obs-c"])
            .with_event(7, "obs-b", &[])
            .with_event(6, "obs-c", &[]);
        let tree = walk(&store, 10, 10);

        let node_of = |gen: i64| {
            tree.nodes
                .iter()
                .position(|n| n.generation == gen)
                .unwrap_or_else(|| panic!("generation {gen} was not walked"))
        };
        let (root, a, c, b) = (node_of(10), node_of(8), node_of(6), node_of(7));

        assert_eq!(tree.nodes[root].parent, None, "the root has no parent");
        assert_eq!(
            tree.nodes[a].parent,
            Some(root),
            "gen 8 was cited by gen 10"
        );
        assert_eq!(tree.nodes[c].parent, Some(a), "gen 6 was cited by gen 8");
        assert_eq!(
            tree.nodes[b].parent,
            Some(root),
            "gen 7 was cited by gen 10, NOT by the tail of the first branch"
        );
        // The ordinal is the citing node's edge order, so the second branch of
        // the root is ordinal 1 even though a whole subtree was walked between.
        assert_eq!(tree.nodes[b].via_ordinal, Some(1));
    }

    /// The structural invariant the case above is one instance of: a child is
    /// always exactly one level below its parent.
    ///
    /// Stated separately because it is the generic form — it catches a
    /// misattached node anywhere in any tree this suite builds, including
    /// shapes no one thought to write a case for. The specific bug it was
    /// written for produced a depth-1 node whose parent sat at depth 2.
    #[test]
    fn every_child_is_exactly_one_level_below_its_parent() {
        let store = InMemoryCitations::new()
            .with_event(20, "obs-root", &["obs-a", "obs-b", "obs-d"])
            .with_event(18, "obs-a", &["obs-c"])
            .with_event(16, "obs-c", &["obs-e"])
            .with_event(14, "obs-e", &[])
            .with_event(12, "obs-b", &[])
            .with_event(11, "obs-d", &[])
            .with_event(10, "obs-unrelated", &[]);
        let tree = walk(&store, 20, 20);
        assert!(tree.nodes.len() >= 6, "the fixture must actually branch");
        for (i, node) in tree.nodes.iter().enumerate() {
            match node.parent {
                None => assert_eq!(node.depth, 0, "node {i} is a root, so depth 0"),
                Some(p) => {
                    assert!(p < i, "node {i}'s parent {p} must precede it");
                    assert_eq!(
                        node.depth,
                        tree.nodes[p].depth + 1,
                        "node {i} (gen {}) sits at depth {} under parent {p} (gen {}) at depth {}",
                        node.generation,
                        node.depth,
                        tree.nodes[p].generation,
                        tree.nodes[p].depth
                    );
                }
            }
        }
    }

    #[test]
    fn a_zero_or_oversized_bound_names_its_dimension() {
        assert_eq!(
            GraphSpec::new(0, 4),
            Err(GraphSpecError::ZeroLimit {
                dimension: SpecDimension::Depth
            })
        );
        assert_eq!(
            GraphSpec::new(4, 0),
            Err(GraphSpecError::ZeroLimit {
                dimension: SpecDimension::Nodes
            })
        );
        assert_eq!(
            GraphSpec::new(4, MAX_PROVENANCE_NODES + 1),
            Err(GraphSpecError::LimitTooLarge {
                dimension: SpecDimension::Nodes,
                requested: MAX_PROVENANCE_NODES + 1,
                maximum: MAX_PROVENANCE_NODES,
            })
        );
    }

    /// The rule at its smallest, without a database: the same citation, two
    /// pins, two correct answers.
    #[test]
    fn visibility_is_the_pin_not_the_present() {
        let store = InMemoryCitations::new()
            .with_event(1, "obs-a", &["obs-x"])
            .with_event(2, "obs-x", &[]);

        assert_eq!(
            only(&walk(&store, 1, 1), 0).resolution,
            CitationResolution::Dangling {
                reason: DanglingReason::NeverVisible
            }
        );
        assert_eq!(
            only(&walk(&store, 1, 2), 0).resolution,
            CitationResolution::Resolved {
                target_generation: 2
            }
        );
    }

    /// A lookup that ignores the pin it is given, in exactly the way the
    /// [`CitationLookup::carriers`] contract forbids.
    struct UnpinnedCarriers(InMemoryCitations);

    impl CitationLookup for UnpinnedCarriers {
        type Error = std::convert::Infallible;

        fn coverage_floor(&self) -> Result<i64, Self::Error> {
            self.0.coverage_floor()
        }
        fn is_retained(&self, generation: i64) -> Result<bool, Self::Error> {
            self.0.is_retained(generation)
        }
        fn citations(&self, generation: i64) -> Result<(Vec<CitationEdge>, bool), Self::Error> {
            self.0.citations(generation)
        }
        fn carriers(&self, observation_id: &str, _at: i64) -> Result<Carriers, Self::Error> {
            // The violation: every carrier, at every coordinate.
            self.0.carriers(observation_id, i64::MAX)
        }
        fn compacted_spans(&self, at_generation: i64) -> Result<CompactedSpans, Self::Error> {
            self.0.compacted_spans(at_generation)
        }
    }

    /// **The rule's pin filter is load-bearing, and this is the only test that
    /// can show it.**
    ///
    /// Both shipped lookups filter by the pin in their query — SQLite in the
    /// `WHERE`, the in-memory one in its iterator — so every other test here
    /// passes whether or not [`resolve_citation`] re-applies the bound. That was
    /// verified the hard way: deleting the filter left the entire suite green,
    /// including the T1/T2 historical-honesty test, because the composition was
    /// still correct while the *rule* had stopped being.
    ///
    /// A rule whose central judgement is only enforced by its callers is a rule
    /// one caller away from being wrong, and this is what makes the lineage
    /// precedent — *"the SQL pre-filter is safe precisely because it is the same
    /// bound applied here"* — a fact rather than an intention.
    #[test]
    fn a_lookup_that_ignores_the_pin_cannot_make_the_rule_dishonest() {
        let store = InMemoryCitations::new()
            .with_event(1, "obs-a", &["obs-x"])
            .with_event(2, "obs-x", &[]);

        let faithful = walk(&store, 1, 1);
        let lying =
            walk_provenance(&UnpinnedCarriers(store), 1, 1, GraphSpec::widest(), V).expect("walk");

        assert_eq!(
            only(&lying, 0).resolution,
            CitationResolution::Dangling {
                reason: DanglingReason::NeverVisible
            },
            "generation 2 is above the pin, and the rule must refuse it however \
             eagerly the seam offers it"
        );
        assert_eq!(
            only(&lying, 0).resolution,
            only(&faithful, 0).resolution,
            "a contract-violating lookup narrows nothing and changes nothing"
        );
    }

    #[test]
    fn several_carriers_are_reported_in_full() {
        let store = InMemoryCitations::new()
            .with_event(1, "obs-x", &[])
            .with_event(2, "obs-x", &[])
            .with_event(3, "obs-a", &["obs-x"]);
        let tree = walk(&store, 3, 3);
        assert_eq!(
            only(&tree, 0).resolution,
            CitationResolution::Plural {
                target_generations: vec![1, 2],
                truncated: false
            }
        );
        assert_eq!(
            only(&tree, 0).continuation,
            BranchContinuation::NotWalked(NotWalkedReason::Plural),
            "an ambiguous edge is not descended into"
        );
    }

    #[test]
    fn a_dangle_under_a_compacted_span_is_qualified() {
        let store = InMemoryCitations::new()
            .with_event(5, "obs-a", &["obs-gone"])
            .with_compacted_span(1, 3);
        assert_eq!(
            only(&walk(&store, 5, 5), 0).resolution,
            CitationResolution::Dangling {
                reason: DanglingReason::PossiblyCompacted {
                    spans: vec![1],
                    truncated: false,
                }
            }
        );
    }

    /// The span filter is a judgement about the pin, and this is the case that
    /// distinguishes it from "the store has ever compacted anything".
    #[test]
    fn a_span_above_the_pin_does_not_qualify_a_dangle_below_it() {
        let store = InMemoryCitations::new()
            .with_event(2, "obs-a", &["obs-gone"])
            .with_compacted_span(7, 9);
        assert_eq!(
            only(&walk(&store, 2, 2), 0).resolution,
            CitationResolution::Dangling {
                reason: DanglingReason::NeverVisible
            },
            "a window compacted after the pin cannot have held evidence the pin saw"
        );
    }

    #[test]
    fn an_unretained_source_is_deleted_evidence_not_an_empty_citation_set() {
        // Edges present, event absent — the state 4a's compaction rule
        // forbids, constructed here precisely to prove the walk does not lean
        // on the index when the statement is gone.
        let store = InMemoryCitations::new().with_unretained_edges(4, &["obs-x"]);
        let tree = walk(&store, 4, 4);
        assert_eq!(tree.nodes[0].citations, NodeCitations::EvidenceCompacted);
        assert!(tree.outcome.is_degraded());
        assert!(
            !tree.outcome.coverage_limited,
            "nothing here is a coverage gap"
        );
    }

    #[test]
    fn a_root_below_the_floor_is_refused() {
        let store = InMemoryCitations::new()
            .with_event(1, "obs-a", &[])
            .with_floor(1);
        assert_eq!(
            walk_provenance(&store, 1, 1, GraphSpec::widest(), V),
            Err(WalkError::IndexIncomplete {
                requested: 1,
                floor: 1
            })
        );
    }

    #[test]
    fn a_cycle_is_not_truncation_and_a_diamond_is_neither() {
        let cyclic = InMemoryCitations::new()
            .with_event(1, "obs-a", &["obs-b"])
            .with_event(2, "obs-b", &["obs-a"]);
        let tree = walk(&cyclic, 1, 2);
        assert!(tree.outcome.cycle_detected);
        assert!(!tree.outcome.truncated);

        let diamond = InMemoryCitations::new()
            .with_event(1, "obs-base", &[])
            .with_event(2, "obs-left", &["obs-base"])
            .with_event(3, "obs-right", &["obs-base"])
            .with_event(4, "obs-top", &["obs-left", "obs-right"]);
        let tree = walk(&diamond, 4, 4);
        assert!(
            !tree.outcome.cycle_detected,
            "two paths to one event is a diamond"
        );
        assert!(tree.outcome.is_complete());
        assert_eq!(tree.nodes.len(), 5, "the shared base appears on both paths");
    }

    #[test]
    fn the_node_budget_stops_a_wide_graph_and_says_which_bound() {
        let mut store = InMemoryCitations::new();
        for g in 1..=5 {
            store = store.with_event(g, &format!("obs-{g}"), &[]);
        }
        let cited: Vec<String> = (1..=5).map(|g| format!("obs-{g}")).collect();
        let refs: Vec<&str> = cited.iter().map(String::as_str).collect();
        store = store.with_event(6, "obs-root", &refs);

        let spec = GraphSpec::new(MAX_PROVENANCE_DEPTH, 3).expect("spec");
        let tree = walk_provenance(&store, 6, 6, spec, V).expect("walk");
        assert_eq!(tree.nodes.len(), 3);
        assert!(tree.outcome.truncated);
        assert!(!tree.outcome.cycle_detected);
        let branches = match &tree.nodes[0].citations {
            NodeCitations::Indexed { branches, .. } => branches,
            other => panic!("{other:?}"),
        };
        assert!(branches
            .iter()
            .any(|b| b.continuation == BranchContinuation::NotWalked(NotWalkedReason::NodeLimit)));
    }
}
