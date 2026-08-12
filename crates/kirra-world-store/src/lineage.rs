//! **Lineage selection — Tier 3 box 3f, the store half.**
//!
//! `KIRRA-WM-EXPLAIN-TIER-001`:
//!
//! > **`Explain` stays at Tier 4. Tier 3 builds only the deterministic lineage
//! > CONTRACT that Tier 4 consumes.** […] Bounded and paginated, with truncation
//! > visible. […] Historically correct.
//!
//! This module owns the *selection*: given the evidence a store holds, which
//! events are in one subject's lineage at one coordinate, in what order, and
//! where a page ends. The boundary half — the reference, the version set, the
//! provenance handles — is `kirra_world_service::lineage`.
//!
//! # What lineage is here, and what it deliberately is not
//!
//! It is the **evidence**: the `world_events` rows bearing on a subject, oldest
//! first, up to a pinned generation. Each row carries what was recorded about
//! where it came from — source, writer class, observation id, and the
//! `provenance` array **verbatim**.
//!
//! It is **not** a traversal of that array. `WM_SCOPE.md` §7 records `Explain`
//! as depending on *"derivation edges being real structure rather than a JSON
//! array of identifiers"*, and today they are precisely that array. Following it
//! would mean inventing the structure whose absence is the reason `Explain` is
//! Tier 4 — so this reports the identifiers as recorded and stops there. The
//! stopping point is the tier boundary, not an oversight.
//!
//! # Why the rule is a pure function over rows
//!
//! The SQL that fetches candidates could express the whole thing — filter,
//! order, limit. It deliberately does not. A rule living in a query string is a
//! rule no corpus can render and no source pin can cover, which is the
//! decorative-metadata failure box 3b exists to prevent. So SQL fetches by
//! subject (an exact equality on an indexed column, which cannot disagree with
//! the same equality applied here) and [`select_lineage`] decides everything
//! that involves a judgement: the generation bound, the ordering, and where the
//! page boundary falls.

use crate::{ClaimStatus, WriterClass};

/// The largest page a caller may request.
///
/// Rule 2 — *queries are bounded* — with a number rather than a hope. D-9
/// measured 10.5 s p99 temporal queries at 100 000 entities and ADR-0041 D-12
/// bars this class of query from any control or safety deadline path; an
/// unbounded lineage walk is the shape most likely to breach both.
pub const MAX_LINEAGE_PAGE: usize = 256;

/// One evidence event in a subject's lineage.
///
/// The `world_events` row as recorded. Nothing here is derived, folded or
/// resolved — a lineage entry is a citation of what the log holds, and a field
/// this type computed would be a claim the log does not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageEvent {
    /// The log position — also this event's page cursor.
    pub generation: i64,
    /// The event's identity.
    pub event_id: String,
    /// The observation this event was written from.
    pub observation_id: String,
    /// When the store learned it.
    pub txn_time_ms: i64,
    /// When the fact it asserts became true.
    pub valid_from_ms: i64,
    /// When that fact stopped being true, if it has.
    pub valid_to_ms: Option<i64>,
    /// Who wrote it.
    pub source: String,
    /// Which version of that writer.
    pub source_version: String,
    /// The writer's class — ADR-0040's `llm_candidate` distinction lives here.
    pub writer_class: WriterClass,
    /// Whether this is a confirmed fact or an unconfirmed proposal.
    pub claim_status: ClaimStatus,
    /// The recorded provenance array, **verbatim, uninterpreted**.
    ///
    /// A JSON array of observation identifiers, passed through exactly as
    /// stored. Not parsed, because parsing it here would be the first half of
    /// treating it as the derivation structure it is not yet — see the module
    /// docs. A consumer that wants the identifiers can decode this string
    /// knowing it is doing so.
    pub provenance: String,
    /// The claim kind.
    pub kind: String,
    /// The subject asserted about.
    pub subject: String,
    /// The predicate, or `None` for a payload-only claim.
    pub predicate: Option<String>,
    /// The object, or `None` for a claim carrying no relationship.
    pub object: Option<String>,
    /// This event's position in the tamper-evident chain.
    ///
    /// The raw stored string. The boundary turns it into an
    /// `EvidenceDigest` — and refuses the entry if it is not one — for the same
    /// reason an answer refuses an unreadable handle: a lineage entry that
    /// cannot be cited is not lineage.
    pub chain_digest: String,
}

/// **Where a page of lineage stopped, and whether anything follows.**
///
/// An enum rather than a `truncated: bool` beside an `Option<cursor>`, because
/// those two fields can disagree and the disagreement is silent. The ruling asks
/// for truncation to be *visible*; a shape that can express "complete, and here
/// is where to continue" has already lost that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageBoundary {
    /// Every event in this lineage is in this page.
    Complete,
    /// The page filled. More events follow, starting after this generation.
    More {
        /// Pass as [`LineagePage::after_generation`] to continue. Exclusive.
        next_after_generation: i64,
    },
}

impl PageBoundary {
    /// Whether the page stopped short of the whole lineage.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        matches!(self, Self::More { .. })
    }

    /// The cursor to continue from, if there is more.
    #[must_use]
    pub fn next_after_generation(&self) -> Option<i64> {
        match self {
            Self::Complete => None,
            Self::More {
                next_after_generation,
            } => Some(*next_after_generation),
        }
    }
}

/// A rejected page request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageSpecError {
    /// A zero-length page. Refused rather than treated as "no bound": it would
    /// return nothing while reporting more, which is an infinite loop dressed as
    /// a paginated read.
    ZeroLimit,
    /// Over [`MAX_LINEAGE_PAGE`].
    ///
    /// **Refused, never clamped.** A silent clamp answers a different question
    /// from the one asked and reports the result as though it were the one
    /// asked — the caller believes it holds 1 000 events and holds 256, with the
    /// boundary saying `Complete` if the lineage happened to be shorter than its
    /// clamped request. Same discipline as `SelfFilterMask`: validated at
    /// construction, refused on violation.
    LimitTooLarge {
        /// What was asked for.
        requested: usize,
        /// The ceiling.
        maximum: usize,
    },
}

impl core::fmt::Display for PageSpecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroLimit => write!(f, "a lineage page limit of 0 returns nothing"),
            Self::LimitTooLarge { requested, maximum } => write!(
                f,
                "lineage page limit {requested} exceeds the maximum of {maximum}"
            ),
        }
    }
}

impl std::error::Error for PageSpecError {}

/// **A validated page request.**
///
/// Constructed through [`LineagePage::new`] or not at all, so an out-of-range
/// bound cannot reach the selection rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineagePage {
    limit: usize,
    after_generation: Option<i64>,
}

impl LineagePage {
    /// Validate a page request.
    ///
    /// # Errors
    ///
    /// [`PageSpecError`] for a zero limit or one over [`MAX_LINEAGE_PAGE`].
    pub fn new(limit: usize, after_generation: Option<i64>) -> Result<Self, PageSpecError> {
        if limit == 0 {
            return Err(PageSpecError::ZeroLimit);
        }
        if limit > MAX_LINEAGE_PAGE {
            return Err(PageSpecError::LimitTooLarge {
                requested: limit,
                maximum: MAX_LINEAGE_PAGE,
            });
        }
        Ok(Self {
            limit,
            after_generation,
        })
    }

    /// The first page at the maximum size.
    #[must_use]
    pub fn first() -> Self {
        Self {
            limit: MAX_LINEAGE_PAGE,
            after_generation: None,
        }
    }

    /// How many events this page may hold.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The exclusive lower bound on generation, if continuing.
    #[must_use]
    pub fn after_generation(&self) -> Option<i64> {
        self.after_generation
    }
}

/// One page of a subject's lineage, and whether it is the whole of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedLineage {
    /// The events, oldest first.
    pub events: Vec<LineageEvent>,
    /// Whether anything follows.
    pub boundary: PageBoundary,
}

// SEMANTICS-PIN-BEGIN: lineage_selection
//
// The versioned rule — see `crate::semantics`. Digested by
// `ci/check_world_semantics.py`; changing which events are selected, the order
// they come back in, or where a page ends moves the corpus digest, and then
// `SEMANTICS`' version must be bumped so recorded lineage references refuse
// rather than replay under the new rule.

/// **Which events are in a subject's lineage, in what order, and where the page
/// ends.**
///
/// Four decisions, each of which can change what a lineage answer says:
///
/// 1. **Subject** — events whose `subject` column equals `subject`, as written.
///    Identity is deliberately not followed; see the note below.
/// 2. **Generation bound** — at or below `at_generation`. This is the
///    historical-correctness rule: an event appended after the pinned coordinate
///    is not evidence that was visible at it, and including it would be 2d's
///    trap ("resolve current state and label it historical") one tier up.
/// 3. **Ordering** — `generation` ascending. Total and stable, since generation
///    is `world_events`' primary key, which is what makes a cursor meaningful.
/// 4. **Page boundary** — at most `page.limit()` events, starting strictly after
///    `page.after_generation()`.
///
/// # Candidates are unfiltered by contract
///
/// This takes every event the caller has and does its own filtering, so the rule
/// is complete in one place. The SQL path pre-filters by subject as an index
/// optimisation; that is safe precisely because it is the *same* exact equality
/// applied here, and could not select a different set. Anything involving a
/// judgement — the bound, the order, the boundary — is here and nowhere else.
///
/// # Candidate claims are INCLUDED, and that is the point
///
/// `world_current` folds only `claim_status = 'confirmed'`. Lineage does not
/// filter on it. A proposal that was never confirmed is part of why an answer
/// says what it says, and an investigator asking "what did the LLM propose here"
/// is asking exactly this question. ADR-0040's guarantee is that a candidate
/// never becomes a *fact*; it is not that a candidate becomes invisible.
///
/// # Identity is NOT followed, stated because it bounds the answer
///
/// An adjudication that merged `pkg-17-alias` into `package_17` is recorded
/// under whichever subject it names, so it is not in `package_17`'s lineage
/// here. That is the same limitation `WorldView::ask` states for the subject
/// side, and for the same reason: reading the whole equivalence class is a
/// different query, not a flag on this one. It is also what keeps `entity_fold`
/// honestly OUT of this family's semantic version set — the moment lineage
/// follows identity edges, that fold can change what a lineage answer says and
/// must join the set.
#[must_use]
pub fn select_lineage(
    candidates: Vec<LineageEvent>,
    subject: &str,
    at_generation: i64,
    page: LineagePage,
) -> SelectedLineage {
    let mut selected: Vec<LineageEvent> = candidates
        .into_iter()
        .filter(|e| e.subject == subject)
        .filter(|e| e.generation <= at_generation)
        .filter(|e| page.after_generation.is_none_or(|a| e.generation > a))
        .collect();
    selected.sort_by_key(|e| e.generation);

    // One over the limit is fetched conceptually here: `More` is reported only
    // when an event actually exists beyond the page, never when the page merely
    // filled exactly. A page that is exactly full and complete must not claim a
    // successor, or a caller paginating to exhaustion makes one wasted round
    // trip on every lineage whose length divides the page size.
    if selected.len() > page.limit {
        selected.truncate(page.limit);
        let last = selected
            .last()
            .expect("a limit of 0 is unrepresentable, so the page is non-empty")
            .generation;
        return SelectedLineage {
            events: selected,
            boundary: PageBoundary::More {
                next_after_generation: last,
            },
        };
    }
    SelectedLineage {
        events: selected,
        boundary: PageBoundary::Complete,
    }
}

// SEMANTICS-PIN-END: lineage_selection

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(generation: i64, subject: &str) -> LineageEvent {
        LineageEvent {
            generation,
            event_id: format!("ev-{generation}"),
            observation_id: format!("obs-{generation}"),
            txn_time_ms: 1_000 + generation,
            valid_from_ms: 1_000 + generation,
            valid_to_ms: None,
            source: "scanner".to_string(),
            source_version: "1.0.0".to_string(),
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: "[]".to_string(),
            kind: "mission".to_string(),
            subject: subject.to_string(),
            predicate: Some("last_seen_at".to_string()),
            object: None,
            chain_digest: format!("digest-{generation}"),
        }
    }

    fn gens(s: &SelectedLineage) -> Vec<i64> {
        s.events.iter().map(|e| e.generation).collect()
    }

    #[test]
    fn a_zero_limit_is_refused_and_an_oversized_one_is_not_clamped() {
        assert_eq!(LineagePage::new(0, None), Err(PageSpecError::ZeroLimit));
        assert_eq!(
            LineagePage::new(MAX_LINEAGE_PAGE + 1, None),
            Err(PageSpecError::LimitTooLarge {
                requested: MAX_LINEAGE_PAGE + 1,
                maximum: MAX_LINEAGE_PAGE,
            }),
            "an oversized limit must REFUSE — a clamp answers a smaller question \
             and reports it as the one that was asked"
        );
        assert!(LineagePage::new(MAX_LINEAGE_PAGE, None).is_ok());
    }

    #[test]
    fn events_come_back_oldest_first_regardless_of_input_order() {
        let out = select_lineage(
            vec![ev(3, "s"), ev(1, "s"), ev(2, "s")],
            "s",
            10,
            LineagePage::first(),
        );
        assert_eq!(gens(&out), vec![1, 2, 3]);
        assert_eq!(out.boundary, PageBoundary::Complete);
    }

    #[test]
    fn another_subjects_events_are_not_in_this_lineage() {
        let out = select_lineage(
            vec![ev(1, "s"), ev(2, "other"), ev(3, "s")],
            "s",
            10,
            LineagePage::first(),
        );
        assert_eq!(gens(&out), vec![1, 3]);
    }

    /// The historical-correctness rule, at its smallest.
    #[test]
    fn an_event_after_the_pinned_generation_is_not_visible_at_it() {
        let out = select_lineage(
            vec![ev(1, "s"), ev(2, "s"), ev(9, "s")],
            "s",
            2,
            LineagePage::first(),
        );
        assert_eq!(
            gens(&out),
            vec![1, 2],
            "generation 9 was appended after the pinned coordinate and was not \
             evidence visible at it"
        );
    }

    #[test]
    fn a_full_page_reports_where_to_continue_and_the_next_page_resumes_there() {
        let all: Vec<LineageEvent> = (1..=5).map(|g| ev(g, "s")).collect();
        let page = LineagePage::new(2, None).expect("valid");
        let first = select_lineage(all.clone(), "s", 10, page);
        assert_eq!(gens(&first), vec![1, 2]);
        assert_eq!(
            first.boundary,
            PageBoundary::More {
                next_after_generation: 2
            }
        );

        let second = select_lineage(
            all.clone(),
            "s",
            10,
            LineagePage::new(2, first.boundary.next_after_generation()).expect("valid"),
        );
        assert_eq!(gens(&second), vec![3, 4]);

        let third = select_lineage(
            all,
            "s",
            10,
            LineagePage::new(2, second.boundary.next_after_generation()).expect("valid"),
        );
        assert_eq!(gens(&third), vec![5]);
        assert_eq!(
            third.boundary,
            PageBoundary::Complete,
            "the last page must not advertise a successor"
        );
    }

    /// An exactly-full page that IS complete must say so.
    ///
    /// The off-by-one a limit-only check gets wrong: `len == limit` is
    /// indistinguishable from "there is more" unless something looked past the
    /// boundary. Getting this wrong costs a wasted round trip on every lineage
    /// whose length divides the page size — and reports `More` for a lineage
    /// that has no more.
    #[test]
    fn an_exactly_full_but_complete_page_does_not_advertise_a_successor() {
        let out = select_lineage(
            (1..=2).map(|g| ev(g, "s")).collect(),
            "s",
            10,
            LineagePage::new(2, None).expect("valid"),
        );
        assert_eq!(gens(&out), vec![1, 2]);
        assert_eq!(out.boundary, PageBoundary::Complete);
    }

    /// Paginating to exhaustion visits every event exactly once.
    ///
    /// The property that actually matters to a caller, asserted over the whole
    /// walk rather than inferred from the single-page cases: a cursor that
    /// skipped or repeated at a boundary would still pass each test above.
    #[test]
    fn paginating_to_exhaustion_visits_every_event_exactly_once() {
        let all: Vec<LineageEvent> = (1..=7).map(|g| ev(g, "s")).collect();
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let out = select_lineage(
                all.clone(),
                "s",
                10,
                LineagePage::new(3, cursor).expect("valid"),
            );
            seen.extend(gens(&out));
            match out.boundary.next_after_generation() {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, vec![1, 2, 3, 4, 5, 6, 7]);
    }

    /// A candidate is evidence about why an answer says what it says, even
    /// though it is never folded into one.
    #[test]
    fn unconfirmed_candidates_are_in_the_lineage() {
        let mut candidate = ev(2, "s");
        candidate.claim_status = ClaimStatus::Candidate;
        candidate.writer_class = WriterClass::LlmCandidate;
        let out = select_lineage(vec![ev(1, "s"), candidate], "s", 10, LineagePage::first());
        assert_eq!(
            gens(&out),
            vec![1, 2],
            "a proposal that was never confirmed is part of the record of what \
             was proposed"
        );
    }
}
