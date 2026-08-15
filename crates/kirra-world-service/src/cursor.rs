//! **Opaque continuation cursors** — Tier 3 §6 cross-cutting.
//!
//! The rule:
//!
//! > A cursor names a continuation of ONE query contract under ONE
//! > semantic-version set, not merely a position in SQLite.
//!
//! # What this replaces, and why it was a leak
//!
//! Pagination shipped with the cursor as a bare `i64` — the SQLite `generation`
//! — handed out by `PageBoundary::More { next_after_generation }` and taken back
//! by `LineagePage::after_generation`. Every page of every family passed a raw
//! log position across the domain boundary.
//!
//! That is not a naming complaint. A bare integer is a value a caller can do
//! *arithmetic* on, persist for a month, replay after a compaction removed the
//! row it names, or hand to a different query family — and every one of those
//! returns a page rather than an error. The answer looks correct: right shape,
//! right subject, plausible contents. It is the same defect class box 3d spent
//! two PRs on, one level up — the ANSWER is well-formed and the QUESTION it
//! answers is not the one that was asked.
//!
//! # Opaque means capability, not wrapping
//!
//! Wrapping the integer in a newtype and calling it opaque would fix nothing:
//! the hazard is not that callers can *read* the generation, it is that a cursor
//! carries no evidence of what it continues. So a [`PageCursor`] binds three
//! things and validates all three before a page is served:
//!
//! | Bound | Refusal when it does not match |
//! |---|---|
//! | the query FAMILY | [`CursorError::WrongFamily`] |
//! | the SEMANTIC VERSIONS in force when it was minted | [`CursorError::SemanticsChanged`] |
//! | a generation this store still RETAINS | [`CursorError::Unreproducible`] |
//!
//! plus [`CursorError::BeyondHead`] and [`CursorError::ImpossibleGeneration`]
//! for a coordinate that cannot have come from this log at all.
//!
//! The family binding is the one that proves "opaque" means capability, and its
//! test had to be built carefully to show it. `SubjectHistory` and
//! `SubjectLineage` declare DIFFERENT rule sets today, so simply swapping two
//! minted cursors is refused by the VERSION check and never exercises the family
//! at all — deleting the family check entirely still refuses the swap. The real
//! control re-stamps the cursor with the target family's live semantics first,
//! so the coordinate and the versions match and the family is the only
//! difference. All three bindings are then independently necessary: dropping any
//! one reds exactly one test.
//!
//! That the two families' version sets differ today is not a defence. It is a
//! coincidence of which rules each depends on, and a future family sharing a
//! rule set would collapse it.
//!
//! # Every failure is a REFUSAL
//!
//! Never a reset to page 1, and never a jump to the next surviving generation.
//! Both are available, both look like recovery, and both silently answer a
//! different question than the caller asked — re-serving rows already seen, or
//! skipping the ones that vanished. A caller who is told *"this cursor no longer
//! names a continuation"* can start over deliberately; a caller silently handed
//! page 1 cannot tell it happened.

use kirra_world_store::WorldStore;

use crate::answer_ref::QueryKind;
use crate::semantics::{SemanticVersions, VersionDifference};

/// Which query family a cursor continues.
///
/// Separate from [`QueryKind`] on purpose: only families that PAGINATE can mint
/// a cursor, and a type that could name `CurrentSubject` would imply a
/// continuation that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorFamily {
    /// [`crate::query::History`] — a subject's recorded claims, in order.
    History,
    /// [`crate::query::Lineage`] — the evidence behind an answer.
    Lineage,
}

impl CursorFamily {
    /// The query whose semantic-version set governs this family.
    #[must_use]
    pub fn query_kind(self) -> QueryKind {
        match self {
            Self::History => QueryKind::SubjectHistory,
            Self::Lineage => QueryKind::SubjectLineage,
        }
    }

    /// The name used in refusals.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Lineage => "lineage",
        }
    }
}

/// **Where a page stopped, and how to continue it.**
///
/// The boundary's replacement for `PageBoundary`, and the same shape for the
/// same reason: an enum rather than a `truncated: bool` beside an
/// `Option<cursor>`, because those two fields can disagree silently. A value
/// that could say *"complete, and here is where to continue"* has already lost
/// the visibility the ruling asks for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Continuation {
    /// Everything the query selects is in this page.
    Complete,
    /// More follows. Present this cursor to continue.
    More(PageCursor),
}

impl Continuation {
    /// The cursor to continue with, or `None` when the page is complete.
    #[must_use]
    pub fn cursor(&self) -> Option<&PageCursor> {
        match self {
            Self::Complete => None,
            Self::More(c) => Some(c),
        }
    }

    /// Whether more follows this page.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        matches!(self, Self::More(_))
    }
}

/// **An opaque continuation token.**
///
/// Carries no public accessor for its generation. That is deliberate rather
/// than fastidious: a caller who can read the coordinate can compute a
/// neighbouring one, and a computed cursor is exactly what the family and
/// version bindings cannot detect — it would be well-formed, in-family,
/// in-version, and name a position the caller invented.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageCursor {
    family: CursorFamily,
    semantics: SemanticVersions,
    /// The exclusive lower bound.
    ///
    /// Private to this MODULE, not merely to the crate: the only code that
    /// reads it is [`resolve_cursor`], which reads it in order to validate it.
    /// Nothing else in the boundary can see a raw coordinate, so nothing else
    /// can accidentally pass one on.
    generation: i64,
}

impl PageCursor {
    /// Mint a cursor for the page that follows `generation`.
    ///
    /// `pub(crate)`: only the boundary may mint, and it stamps the versions from
    /// the live declarations rather than accepting them. A caller able to mint
    /// could claim any semantics — the same reasoning
    /// [`crate::answer_ref::AnswerRef`] states for its own version stamp.
    pub(crate) fn mint(family: CursorFamily, generation: i64) -> Self {
        Self {
            family,
            semantics: SemanticVersions::for_query(family.query_kind()),
            generation,
        }
    }

    /// Which family this continues.
    ///
    /// Readable because it is not exploitable: knowing the family does not let a
    /// caller construct one, and an error naming the family is more useful than
    /// one that will not say.
    #[must_use]
    pub fn family(&self) -> CursorFamily {
        self.family
    }

    /// The semantics in force when this was minted.
    #[must_use]
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }

    /// Rebuild a cursor recorded under a different semantic-version set.
    ///
    /// The deliberately-explicit escape hatch [`crate::answer_ref::AnswerRef`]
    /// and [`crate::lineage::LineageRef`] both provide, for tests and for a
    /// reader decoding a persisted value. It cannot be used to FORGE a valid
    /// cursor: whatever it stamps is what the version check compares against, so
    /// a wrong set produces a refusal rather than an accepted page.
    #[must_use]
    pub fn recorded_under(mut self, semantics: SemanticVersions) -> Self {
        self.semantics = semantics;
        self
    }
}

/// Why a presented cursor cannot be continued.
///
/// Every variant is a REFUSAL. None of them has a recovery arm that serves a
/// page anyway — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    /// Presented to a family other than the one that minted it.
    WrongFamily {
        /// The family the cursor carries.
        presented: CursorFamily,
        /// The family it was presented to.
        expected: CursorFamily,
    },
    /// The rules that produced the earlier page have changed.
    ///
    /// Continuing would splice pages from two different query contracts into one
    /// sequence, which is the failure box 3b's versioning exists to make
    /// visible. The differences are carried so an operator can see WHICH rule
    /// moved rather than only that something did.
    SemanticsChanged {
        /// Per-rule differences between the cursor's set and the live one.
        differences: Vec<VersionDifference>,
    },
    /// The generation is no longer retained — compaction removed it.
    ///
    /// Refused rather than continued from the nearest survivor: the cursor names
    /// a position in a sequence, and the sequence it named no longer exists.
    Unreproducible {
        /// The coordinate the cursor names.
        generation: i64,
    },
    /// The generation is past this log's head.
    ///
    /// A cursor from a different store, or a computed one.
    BeyondHead {
        /// The coordinate the cursor names.
        generation: i64,
        /// The highest generation this log holds.
        head: i64,
    },
    /// The generation is not a position any log could hold.
    ImpossibleGeneration {
        /// The coordinate the cursor names.
        generation: i64,
    },
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongFamily {
                presented,
                expected,
            } => write!(
                f,
                "cursor continues the {} query, presented to {}",
                presented.as_str(),
                expected.as_str()
            ),
            Self::SemanticsChanged { differences } => write!(
                f,
                "query semantics changed since this cursor was minted ({} rule(s) differ)",
                differences.len()
            ),
            Self::Unreproducible { generation } => write!(
                f,
                "generation {generation} is no longer retained; this continuation cannot be reproduced"
            ),
            Self::BeyondHead { generation, head } => {
                write!(f, "generation {generation} is past the log head {head}")
            }
            Self::ImpossibleGeneration { generation } => {
                write!(f, "generation {generation} is not a valid log position")
            }
        }
    }
}

impl std::error::Error for CursorError {}

/// **Validate a presented cursor and yield its coordinate** — or refuse.
///
/// # Order of refusals
///
/// Family and semantics are checked BEFORE the store is touched, for the reason
/// [`crate::lineage::LineageRef::resolve`] gives about its own ordering: a
/// cursor that cannot be continued should say so without a read, and the cheap
/// structural refusals should not be reachable only when a read happens to
/// succeed.
///
/// # Errors
///
/// [`CursorError`] — every variant, and never a fallback page.
pub(crate) fn resolve_cursor(
    store: &WorldStore,
    cursor: &PageCursor,
    expected: CursorFamily,
) -> Result<i64, CursorError> {
    if cursor.family != expected {
        return Err(CursorError::WrongFamily {
            presented: cursor.family,
            expected,
        });
    }

    let live = SemanticVersions::for_query(expected.query_kind());
    let differences = cursor.semantics.differences(&live);
    if !differences.is_empty() {
        return Err(CursorError::SemanticsChanged { differences });
    }

    let generation = cursor.generation;
    if generation < 1 {
        return Err(CursorError::ImpossibleGeneration { generation });
    }

    // The store read is LAST, and its failure is treated as unreproducible
    // rather than propagated: a cursor whose anchor cannot be read is a cursor
    // that cannot be shown to name a real position, which is the same refusal
    // by a different route. Serving the page instead would be the fall-forward
    // this module exists to forbid.
    let anchor = store
        .cursor_anchor(generation)
        .map_err(|_| CursorError::Unreproducible { generation })?;

    if generation > anchor.head {
        return Err(CursorError::BeyondHead {
            generation,
            head: anchor.head,
        });
    }
    if !anchor.retained {
        return Err(CursorError::Unreproducible { generation });
    }
    Ok(generation)
}
