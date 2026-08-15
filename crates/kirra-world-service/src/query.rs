//! **The one sanctioned way to ask Kirra World a domain question** — box 3d.
//!
//! Box 3d's boundedness half made it impossible for a query to hide an
//! unbounded store read. This half makes it impossible to ask *without going
//! through a query at all*.
//!
//! Those are different properties, and the first does not imply the second. A
//! consumer holding a `&WorldStore` could build its own [`WorldView`], call a
//! family method directly, and be perfectly bounded while bypassing every other
//! contract the boundary carries — semantics versions, freshness classification,
//! the no-bare-values rule. `mission_context` did exactly that until this
//! module existed, and it was the sanctioned route at the time.
//!
//! # The property this adds
//!
//! > There is ONE mechanically enforced way for application code to ask Kirra
//! > World a domain question.
//!
//! Not *"a typed engine exists"* — that is satisfied by an engine nobody has to
//! use. The enforcement is primarily **visibility**: [`WorldView`], and the
//! `resolve` methods on [`LineageRef`] and [`AnswerRef`], are `pub(crate)`. A
//! consumer that reaches past the engine does not produce a gate finding after
//! the fact; it produces a compile error, at the point of the mistake, naming
//! the item it cannot see.
//!
//! `ci/check_query_boundedness.py` rule 5 is defence in depth behind that, for
//! the cases visibility cannot reach: a *new* family method added as `pub`
//! without being wrapped, or a consumer reaching into the store directly rather
//! than into this crate.
//!
//! # Typed requests, not one enum
//!
//! Five families with five genuinely different result shapes — different
//! payload outcomes, completeness types, pagination state, historical
//! coordinates and refusal semantics. A single `WorldQuery` enum returning a
//! single `WorldAnswer` would flatten all five axes back together and force
//! every caller to `match` over families it did not ask about, undoing the work
//! that kept those axes honest. So the surface is one entry point with
//! **compile-time output types**:
//!
//! ```ignore
//! engine.execute(Ask { subject, now_ms })?;          // -> ComposedLookup
//! engine.execute(History { subject, page })?;        // -> HistoryLookup
//! ```
//!
//! # The trait is sealed
//!
//! [`WorldQuery`] cannot be implemented outside this crate. Without that, the
//! engine would be a suggestion: a consumer could write its own implementation,
//! close over a `&WorldStore` or an arbitrary predicate, and route it through
//! `execute` — arriving at the ad-hoc queries the whole box exists to prevent,
//! while looking like sanctioned use.
//!
//! Sealing also preserves the [`AnswerRef`] ruling. A public domain query stays
//! a **serializable, named value** rather than a closure: it can be recorded,
//! shipped, replayed and compared. An open trait would admit query types that
//! are none of those things.
//!
//! # This is orchestration, and deliberately nothing more
//!
//! Every request dispatches into the already-proven family implementation. No
//! request re-derives a bound, re-implements a fold, or re-decides freshness.
//! If a boundedness argument or a completeness rule lived in here, there would
//! be a second query engine hiding inside the query engine, and the proofs
//! behind the first would stop covering what callers actually run.
//!
//! The controls for that claim are the family test suites themselves, which now
//! drive their assertions through [`QueryEngine::execute`]. They pass unchanged,
//! which is what "wrapped without changing semantics" has to mean.

use kirra_world_store::lineage::LineagePage;
use kirra_world_store::WorldStore;

use crate::answer_ref::{AnswerRef, RefResolution};
use crate::cursor::{resolve_cursor, CursorFamily, PageCursor};
use crate::freshness::FreshnessSource;
use crate::lineage::{LineageRef, LineageResolution};
use crate::read_view::{
    AskError, ComposedLookup, HistoryLookup, SummaryLookup, TemporalLookup, WorldView,
};

mod sealed {
    /// Closes [`super::WorldQuery`] to this crate.
    ///
    /// A consumer cannot name this trait, so it cannot implement the public one
    /// either — the standard sealing idiom, used here for the reason in the
    /// module docs rather than for API-stability hygiene.
    pub trait Sealed {}
}

/// **A domain question, as a value.**
///
/// Implemented only by the request types in this module (the trait is sealed).
/// The associated [`Output`](WorldQuery::Output) is what keeps five families
/// from collapsing into one union: `Ask` yields a [`ComposedLookup`] and
/// `History` a [`HistoryLookup`], checked at compile time, with no runtime arm
/// a caller could get wrong.
pub trait WorldQuery: sealed::Sealed {
    /// What answering this question produces.
    type Output;

    /// Run this question against `engine`.
    ///
    /// Prefer [`QueryEngine::execute`] at call sites — it reads in the order the
    /// work happens and keeps the engine the subject of the sentence. This is
    /// the dispatch target, not the intended spelling.
    ///
    /// # Errors
    ///
    /// Whatever the underlying family raises; see [`AskError`].
    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError>;
}

/// **The entry point.**
///
/// Holds a `&WorldStore` and a [`FreshnessSource`], and offers no mutation:
/// read-only is structural here for the same reason it is on [`WorldView`] —
/// Kirra World is evidence, and an answer boundary serves it rather than
/// adjudicating it.
///
/// The freshness source is bound to the ENGINE, not to each request, and that
/// placement is deliberate. It is a property of the asking context — who is
/// asking and under what recency contract — not of the individual question, and
/// putting it on every request would invite two questions in one context to
/// disagree about whether a fact had expired.
pub struct QueryEngine<'a> {
    view: WorldView<'a>,
    store: &'a WorldStore,
}

impl<'a> QueryEngine<'a> {
    /// Bind an engine to a store and a freshness contract.
    ///
    /// [`FreshnessSource`] has no variant meaning "nothing supplied" — see box
    /// 3e. Either the ruled table decides, or the caller states a policy.
    #[must_use]
    pub fn new(store: &'a WorldStore, freshness: FreshnessSource) -> Self {
        Self {
            view: WorldView::new(store, freshness),
            store,
        }
    }

    /// **Ask one typed question.**
    ///
    /// ```ignore
    /// let engine = QueryEngine::new(&store, FreshnessSource::Ruled);
    /// let answer = engine.execute(Ask { subject: "robot-1".into(), now_ms })?;
    /// ```
    ///
    /// # Errors
    ///
    /// Whatever the family raises; see [`AskError`].
    pub fn execute<Q: WorldQuery>(&self, query: Q) -> Result<Q::Output, AskError> {
        query.execute(self)
    }

    /// The view the requests dispatch into.
    ///
    /// `pub(crate)` and no wider: this is the thing the module exists to stop
    /// consumers from reaching.
    pub(crate) fn view(&self) -> &WorldView<'a> {
        &self.view
    }

    /// The store, for the two families that resolve a recorded reference.
    ///
    /// [`LineageRef`] and [`AnswerRef`] carry their own coordinates and resolve
    /// against the store rather than through a view, so they need it directly.
    pub(crate) fn store(&self) -> &'a WorldStore {
        self.store
    }
}

/// **What is currently known about one subject.**
///
/// Bounded by construction — one subject, never the whole projection.
#[derive(Debug, Clone)]
pub struct Ask {
    /// The subject to ask about.
    pub subject: String,
    /// The instant the question is asked at, for freshness.
    pub now_ms: i64,
}

impl sealed::Sealed for Ask {}

impl WorldQuery for Ask {
    type Output = ComposedLookup;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        engine.view().ask(&self.subject, self.now_ms)
    }
}

/// **What was known about one subject, at a past coordinate.**
///
/// Two independent axes, which is why they are two fields rather than one
/// "time": `valid_at_ms` asks *when the fact held*, `as_known_at_ms` asks *what
/// the store had recorded by then*. Collapsing them would make it impossible to
/// ask what we believed last week about last year.
#[derive(Debug, Clone)]
pub struct AskAsOf {
    /// The subject to ask about.
    pub subject: String,
    /// The valid-time instant the fact must hold at.
    pub valid_at_ms: i64,
    /// The transaction-time cut: knowledge recorded after this is not consulted.
    pub as_known_at_ms: i64,
}

impl sealed::Sealed for AskAsOf {}

impl WorldQuery for AskAsOf {
    type Output = TemporalLookup;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        engine
            .view()
            .ask_as_of(&self.subject, self.valid_at_ms, self.as_known_at_ms)
    }
}

/// **One page of a subject's recorded history.**
///
/// The bound is a FIELD of the request rather than an argument threaded past
/// it, which is the structural half of clause 2: a history question that
/// carries no bound is not constructible.
///
/// `after` is an OPAQUE [`PageCursor`], never a log position. A caller cannot
/// compute one, cannot hand a [`Lineage`] cursor to it, and cannot continue
/// across a semantics change or a compaction that removed the coordinate — each
/// of those is a refusal rather than a plausible page. See [`crate::cursor`].
#[derive(Debug, Clone)]
pub struct History {
    /// The subject whose history to read.
    pub subject: String,
    /// How many claims this page may hold.
    pub limit: usize,
    /// Where to continue from, or `None` for the first page.
    pub after: Option<PageCursor>,
}

impl sealed::Sealed for History {}

impl WorldQuery for History {
    type Output = HistoryLookup;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        // Cursor FIRST, page second. A refused continuation must not depend on
        // the limit also being valid — the caller's mistake is the cursor, and
        // reporting a limit error for a stale cursor sends them to the wrong
        // fix.
        let after = match &self.after {
            None => None,
            Some(cursor) => Some(resolve_cursor(
                engine.store(),
                cursor,
                CursorFamily::History,
            )?),
        };
        let page = LineagePage::new(self.limit, after)?;
        engine.view().history(&self.subject, page)
    }
}

/// **The folded summary for one subject, with its evidence coverage.**
///
/// Carries no page because it has no growth dimension to bound: the answer is
/// one row per subject. Adding a cursor here would be ceremony implying a
/// dimension that does not exist — the same judgement the boundedness baseline
/// records for the structurally-bounded reads.
#[derive(Debug, Clone)]
pub struct SubjectSummary {
    /// The subject to summarise.
    pub subject: String,
}

impl sealed::Sealed for SubjectSummary {}

impl WorldQuery for SubjectSummary {
    type Output = SummaryLookup;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        engine.view().subject_summary(&self.subject)
    }
}

/// **The evidence behind an answer, one page at a time.**
///
/// The bound rides on the [`LineageRef`] rather than sitting beside it, and that
/// is the correct placement rather than an omission: the reference is the
/// serializable recorded value — subject, generation AND page together are what
/// gets stored, shipped and replayed. A second `page` field on the request would
/// create two sources of truth for one bound, and the recorded one would win
/// silently whenever they disagreed.
#[derive(Debug, Clone)]
pub struct Lineage {
    /// The recorded reference, carrying its own page.
    pub reference: LineageRef,
}

impl sealed::Sealed for Lineage {}

impl WorldQuery for Lineage {
    type Output = LineageResolution;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        self.reference.resolve(engine.store())
    }
}

/// **Re-execute a recorded answer at the coordinate it was taken.**
///
/// Not a sixth family: this is the [`Ask`] family's REPLAY form, pinned to a
/// generation instead of asked at now, which is why it shares `ask`'s bounded
/// composed primitive rather than owning one.
///
/// It is here rather than in a carve-out because it is unambiguously a domain
/// read — it returns the same answers `Ask` does. Leaving it out would have
/// meant exempting a domain read from rule 5, and an exemption is exactly the
/// shape the five earlier defects took.
#[derive(Debug, Clone)]
pub struct ReplayAnswer {
    /// The recorded reference to re-execute.
    pub reference: AnswerRef,
}

impl sealed::Sealed for ReplayAnswer {}

impl WorldQuery for ReplayAnswer {
    type Output = RefResolution;

    fn execute(self, engine: &QueryEngine<'_>) -> Result<Self::Output, AskError> {
        self.reference.resolve(engine.store())
    }
}
