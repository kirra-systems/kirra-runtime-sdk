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
"#;

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
