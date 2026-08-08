//! Retention enforcement: compaction-with-citation (ADR-0041 §11.3).
//!
//! OQ2 ruled retention *durations* — 30 days `raw`, 365 days protected — and
//! D-21 measured the budget they fit in. Nothing enforced them: the classes
//! were stored and constrained, the horizons were ruled, and the store filled
//! in 15.79 days regardless. This is the half that makes the policy behaviour.
//!
//! # Deleting from an append-only hash chain
//!
//! The obvious problem: `world_events` is chained, and removing a span breaks
//! recomputation at the hole. §11.3's answer is that compaction leaves a
//! **citation** behind — what was removed, how much, the digest of the removed
//! span, and the chain digests on either side — so the log stays verifiable
//! **across** the hole rather than merely up to it.
//!
//! That makes three things true, and the third is the one that matters:
//!
//! 1. A verifier crossing a hole picks the chain back up from `chain_after`.
//! 2. The removed content stays *attestable* through `range_digest`: it cannot
//!    be recovered, but anyone later producing those events can be checked.
//! 3. **Rows cannot be deleted quietly.** A gap with no citation is a chain
//!    error, and a citation whose `chain_before` does not match what the
//!    verifier computed is a chain error. Compaction is therefore an
//!    *admission*, recorded in the same structure it modifies — not an
//!    erasure.
//!
//! # Two refusals, and why the second one is new
//!
//! **Protected classes are refused WHOLE.** A window containing any of
//! `safety`, `incident`, `calibration`, `adjudication` or `operator` is refused
//! entirely — not compacted around, not partially applied. OQ2 gave those
//! classes a 365-day horizon; silently compacting the raw traffic interleaved
//! with them would leave a window that is *partly* summarized, and "how much of
//! this span survives" would become a question about arrival order.
//!
//! **Projection heads are refused.** This one is not in §11.3, because that
//! design predates the store having a read path. If compaction removes the
//! event that *defines* a `(subject, predicate)`'s current claim, then
//! `rebuild_projections()` no longer reproduces the incremental state — the
//! property ADR-0041 names, and that
//! [`crate::WorldStore::projection_state_digest`] exists to check, becomes
//! quietly false.
//!
//! Refusing heads resolves it exactly, and provably: the fold retains only
//! heads, so a compaction that removes no head cannot change what a rebuild
//! produces. The rule also reads correctly on its own terms — the event saying
//! where a thing *is* is evidence still in use, not history to summarize.
//!
//! ## The two refusals are not alike, and the difference is pre-recorded
//!
//! It would be easy to read them as one rule applied twice. They are not:
//!
//! * A **protected class** makes the window a policy unit. Compacting around
//!   it would make how much of a span survives a question about interleaving
//!   rather than about policy, so refusing whole is the *requirement*.
//! * A **projection head** only requires that the head itself survive. Nothing
//!   says its neighbours must. Refusing the whole window there is a
//!   conservative simplification, not a requirement.
//!
//! Two things follow. First, [`crate::WorldStore::largest_compactable_prefix`]
//! exists so a refusal is a redirect rather than a dead end — both refusals
//! name their first blocking generation, and that is now a designed recovery.
//!
//! Second, the escalation is agreed in advance: **if measurement ever shows
//! head refusals blocking a material fraction of reclaimable space, compact
//! *around* heads instead.** The citation model already supports disjoint
//! spans, so that is a loop over sub-ranges rather than a redesign. Recording
//! the trigger now costs nothing and stops it becoming an argument later.
//!
//! The concern is also smaller than it first looks, because **heads age out**.
//! An event stops being a head the moment something supersedes it, so for an
//! old span to still hold one means nothing has been observed about that
//! `(subject, predicate)` since — which is rare in a sensor stream, and when it
//! happens that claim is arguably still-live evidence rather than history.
//!
//! # What compaction is NOT
//!
//! **Lossy, and only for summaries** (OQ2 rule 2). D-4 measured ~50 % recovery
//! on the stand-in; it buys retention of *summaries*, not of observations, and
//! does not relax the sampling conclusion.
//!
//! **Not reclamation.** Deleting rows returns pages to SQLite's free list; it
//! does not shrink the file. D-3 measured what reclamation actually costs — a
//! ~1× free-space reserve and a total write blackout — so a policy that assumes
//! continuous `VACUUM` assumes a maintenance window that may never open. This
//! module deliberately does not call it.

/// Retention classes OQ2 gave a 365-day horizon and §11.3 protects from
/// compaction. A window containing any of these is refused whole.
pub const PROTECTED_CLASSES: [&str; 5] = [
    "safety",
    "incident",
    "calibration",
    "adjudication",
    "operator",
];

/// Is this retention class protected from compaction?
///
/// Unknown classes are treated as **protected**. The schema constrains the
/// column to six known values, so an unknown one means the schema moved or the
/// row is corrupt — and in either case deleting it is the irreversible choice.
#[must_use]
pub fn is_protected(retention_class: &str) -> bool {
    retention_class != "raw"
}

/// The citation table, installed lazily by the first compaction.
///
/// Lazily for the same reason the projection tables are (see
/// [`crate::projection`]): ADR-0041 D-20's `log_only_bytes` is the size of a
/// store holding only the event log, and adding root pages at `open` would
/// silently move it.
pub const COMPACTION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS compaction_citations (
    lo_generation   INTEGER PRIMARY KEY,
    hi_generation   INTEGER NOT NULL,
    event_count     INTEGER NOT NULL,
    range_digest    TEXT    NOT NULL,
    chain_before    TEXT    NOT NULL,
    chain_after     TEXT    NOT NULL,
    compacted_at_ms INTEGER NOT NULL,

    CHECK (hi_generation >= lo_generation),
    CHECK (event_count > 0)
);

-- What a time-travel query into a compacted window gets INSTEAD of the
-- observations. One row per `(subject, predicate)` the window held; see
-- `DegradedSummary` for why per-key rather than per-span.
--
-- `predicate_key` is the predicate or `''`, matching `world_current`: SQLite
-- permits multiple NULLs in a non-INTEGER primary key, so a NULL-keyed row
-- would duplicate rather than collide.
CREATE TABLE IF NOT EXISTS compaction_summaries (
    lo_generation       INTEGER NOT NULL,
    subject             TEXT    NOT NULL,
    predicate_key       TEXT    NOT NULL,
    predicate           TEXT,
    event_count         INTEGER NOT NULL,
    first_valid_from_ms INTEGER NOT NULL,
    last_valid_from_ms  INTEGER NOT NULL,
    first_txn_time_ms   INTEGER NOT NULL,
    last_object         TEXT,
    last_payload        TEXT    NOT NULL,

    PRIMARY KEY (lo_generation, subject, predicate_key),
    CHECK (event_count > 0),
    CHECK (last_valid_from_ms >= first_valid_from_ms)
);

-- The PRIMARY KEY leads with `lo_generation`, so a lookup BY SUBJECT -- which
-- is how `SummaryCoverage` is computed for every summary read -- cannot use it
-- and would scan the whole table once per subject.
CREATE INDEX IF NOT EXISTS idx_compaction_summaries_subject
    ON compaction_summaries (subject);
"#;

/// The per-key summary retained when a span is compacted.
///
/// §11.3 requires the retained summary to **cite the source event range and its
/// digest** — that is the [`Citation`] — and to be what a time-travel query
/// into the window returns *instead of* the observations. This is that second
/// half.
///
/// # Why per `(subject, predicate)` rather than per span
///
/// A span-level aggregate ("N events between T1 and T2") cannot answer the
/// question an incident reconstruction actually asks, which is *what did we
/// know about **this thing** during the window*. Per-key can.
///
/// The cost argument is the same one that makes projections affordable and was
/// measured in D-21: keys are bounded by the **entity** count, not the log
/// length — 4 886 for 100 000 events on the reference stream. A summary set is
/// therefore bounded like a projection, not like a log.
///
/// # What "degraded" means precisely
///
/// The trajectory is gone; the endpoints survive. You learn that a key was
/// observed `event_count` times between `first_valid_from_ms` and
/// `last_valid_from_ms`, and what the **last** value in the window was. You
/// cannot recover the individual observations, and nothing here pretends
/// otherwise — which is the difference §11.3 draws between degraded resolution
/// and silent fabrication.
///
/// "Last" is by `(valid_from_ms, generation)` — the *same* order
/// [`crate::projection::supersedes`] uses. Deliberately not "highest
/// generation": the summary has to answer *what was true at the end of the
/// window*, and a late-arriving backfill about an earlier instant does not
/// change that. Two orders here would mean the summary and the projection
/// could disagree about which observation was the latest one.
///
/// # Confirmed-only
///
/// Summaries are built from `confirmed` events alone, for the reason the fold
/// is confirmed-only: this is what a degraded answer returns *in place of*
/// evidence, and a candidate standing in for a deleted observation would put an
/// LLM proposal exactly where SD-2 says it may never appear. The consequence is
/// arithmetic and testable — the per-key counts sum to **at most** the
/// citation's `event_count`, and the difference is the candidates the window
/// held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedSummary {
    /// The citation this summary belongs to.
    pub lo_generation: i64,
    /// Claim subject.
    pub subject: String,
    /// Claim predicate, `None` for predicate-less claims.
    pub predicate: Option<String>,
    /// How many confirmed events about this key the window held.
    pub event_count: i64,
    /// Valid time of the earliest.
    pub first_valid_from_ms: i64,
    /// Valid time of the latest, by the supersession order.
    pub last_valid_from_ms: i64,
    /// Transaction time of the earliest-known. Carried so a bitemporal query
    /// can rule a window out on the *transaction* axis too — without it,
    /// `as_of` could only filter soundly on valid time.
    pub first_txn_time_ms: i64,
    /// Object of the latest observation in the window.
    pub last_object: Option<String>,
    /// Payload of the latest observation in the window.
    pub last_payload: String,
}

/// Whether an answer is the whole truth, or what remains of it.
///
/// §11.3: *"compaction must never silently rewrite history. A time-travel query
/// into a compacted window returns the summary **and says so** — degraded
/// resolution, never silent fabrication."*
///
/// This is carried **in the return type** rather than as a flag a caller can
/// forget to read, an out-parameter, or a separate method. The reasoning is the
/// same one that made the projection fold confirmed-only: a guarantee that
/// depends on the caller remembering to check is not a guarantee, and the
/// failure it prevents here is specific — an incident reconstruction unable to
/// tell *"nothing was known"* from *"we deleted it"*, with the answer looking
/// complete either way.
///
/// # Which way it is allowed to be wrong
///
/// The test for degradation is a **necessary condition** on the removed events,
/// not an exact one: a window is reported degraded if it *could* have held an
/// observation bearing on the query. So this can say `Degraded` where a full
/// answer would in fact have been identical. It cannot say `Full` where
/// something was lost, and that asymmetry is the point — over-reporting costs a
/// caveat, under-reporting is the silent rewrite §11.3 forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No compacted span could have borne on the queried range.
    Full,
    /// Part of the queried range was compacted. The claims returned alongside
    /// are what survives; these summaries are what stands for the rest.
    Degraded {
        /// The spans that were compacted away.
        spans: Vec<Citation>,
        /// Per-key summaries covering the query's subject. **May be empty** —
        /// a span compacted before summaries were retained still degrades the
        /// answer, and reports so with nothing to show for it.
        summaries: Vec<DegradedSummary>,
    },
}

impl Resolution {
    /// Whether anything was lost to compaction.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// The summaries standing in for what was removed; empty when [`Self::Full`].
    #[must_use]
    pub fn summaries(&self) -> &[DegradedSummary] {
        match self {
            Self::Full => &[],
            Self::Degraded { summaries, .. } => summaries,
        }
    }

    /// The spans that were compacted away; empty when [`Self::Full`].
    #[must_use]
    pub fn spans(&self) -> &[Citation] {
        match self {
            Self::Full => &[],
            Self::Degraded { spans, .. } => spans,
        }
    }
}

/// A temporal answer: the surviving claims, and whether that is all of them.
///
/// Returning `Vec<ProjectedClaim>` directly would make the degradation
/// invisible at the call site. This type exists so a caller cannot obtain the
/// claims without the answer's resolution being in front of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalAnswer {
    /// The claims that survive in the log.
    pub claims: Vec<crate::ProjectedClaim>,
    /// Whether a compacted span intersects the queried range.
    pub resolution: Resolution,
}

impl TemporalAnswer {
    /// Shorthand for [`Resolution::is_degraded`].
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.resolution.is_degraded()
    }

    /// How many claims survive. Shorthand, so a caller counting an answer does
    /// not have to reach past the resolution to do it.
    #[must_use]
    pub fn len(&self) -> usize {
        self.claims.len()
    }

    /// Whether no claims survive. Note this is **not** the same question as
    /// "nothing was known": an empty `Degraded` answer means the opposite.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }
}

/// One recorded compaction: the citation left where a span used to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// First generation removed.
    pub lo_generation: i64,
    /// Last generation removed.
    pub hi_generation: i64,
    /// How many events were actually removed.
    pub event_count: i64,
    /// Digest over the removed events' canonical bytes, in generation order.
    /// The removed content is gone; this keeps it *attestable*.
    pub range_digest: String,
    /// Chain digest immediately BEFORE the removed span.
    pub chain_before: String,
    /// Chain digest of the LAST removed event — where a verifier resumes.
    pub chain_after: String,
    /// When the compaction ran.
    pub compacted_at_ms: i64,
}

/// What a compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// The citation now standing in for the removed span.
    pub citation: Citation,
    /// Events removed.
    pub removed: i64,
    /// The per-key summaries retained in their place — OQ2 rule 2's "compaction
    /// buys retention of summaries, not of observations", returned rather than
    /// merely asserted. Bounded by the window's **key** count, not its length.
    pub summaries: Vec<DegradedSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_raw_is_compactable() {
        assert!(!is_protected("raw"));
        for c in PROTECTED_CLASSES {
            assert!(is_protected(c), "{c} must be protected");
        }
    }

    /// An unknown class must be treated as protected. The schema constrains
    /// the column, so an unknown value means the schema moved or the row is
    /// corrupt — and deletion is the irreversible way to be wrong.
    #[test]
    fn an_unknown_class_is_protected() {
        assert!(is_protected("some-future-class"));
        assert!(is_protected(""));
        assert!(
            is_protected("RAW"),
            "case matters; only exact `raw` is open"
        );
    }
}
