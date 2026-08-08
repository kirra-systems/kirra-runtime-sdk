//! **Per-subject observation summary** — the fold behind §6's `first_observed`,
//! `last_observed` and `provenance_head`.
//!
//! # Why this is a projection and not a table
//!
//! `WM2_EVENT_SCHEMA.md` §7 settles it, under *what this ruling does not
//! decide*:
//!
//! > **Projection schemas.** `entities_projection` / `relationships_projection`
//! > are rebuildable views and follow from the fold, not from this table.
//!
//! So none of this is new DDL inside `KIRRA-WM2-SCHEMA-001`'s ratified surface,
//! and no schema version bump is involved. The three fields §6 leaves open are
//! **derived by folding the event log**, which is what keeps them from drifting
//! away from the evidence — the same argument that leaves validity without a
//! column.
//!
//! # Installed lazily, and that rule is load-bearing
//!
//! Like [`crate::projection::PROJECTIONS_V1`], this DDL is installed by the
//! first fold rather than at `open`. ADR-0041 **D-20**'s `log_only_bytes` is
//! the on-disk size of a store holding only the event log; creating projection
//! tables at `open` would add their root pages to *every* store, including one
//! that never projects, silently moving that figure and invalidating the
//! comparison against D-2 that the retention horizons rest on.
//!
//! # What this is keyed on
//!
//! **Keyed on `(subject_kind, subject)`** since 2026-08-08. It was keyed on the
//! subject string alone, and the module docs recorded why that was wrong:
//!
//! > The storage layer flattens all four into one `subject TEXT NOT NULL`
//! > column and keeps no discriminant, so a fold here **cannot** restrict
//! > itself to resolved entities. Every distinct subject string gets a row.
//!
//! `KIRRA-WM-CANDIDATE-ID-001` and the `subject_kind` column closed that. A
//! fold can now restrict itself to resolved entities — see
//! [`SummaryKind::Entity`] and `WorldStore::resolved_entities`.
//!
//! ## Why the kind is part of the KEY and not merely a column
//!
//! An entity id and a frame id live in different namespaces but land in one
//! `TEXT` column, so `base_link` as a frame and `base_link` as an entity are the
//! same string. Keyed on the string alone they fold into ONE row whose counts
//! and time bounds are summed across two different things, and whose kind would
//! flip-flop with whichever event was seen last. That is not a summary of
//! either of them.
//!
//! ## The sentinel, and the SQLite behaviour that forces it
//!
//! [`SummaryKind::Unlabelled`] is a **projection-only** token. It never appears
//! in `world_events.subject_kind`, which is `NULL` for an unlabelled row — the
//! evidence vocabulary is `entity`/`frame` and nothing else.
//!
//! It exists because **`NULL` never conflicts with `NULL` in SQLite**, so
//! `ON CONFLICT (subject_kind, subject)` against a nullable column does not
//! match an existing row: each unlabelled event INSERTs a new one. Measured
//! before it was designed around, not recalled — two unlabelled events for one
//! subject produced two rows of `observation_count = 1` rather than one row of
//! 2. Every row in a store today is unlabelled, so that fold would have been
//! wrong for all of them while looking entirely plausible.
//!
//! # Minting is not here either
//!
//! `WM_SCOPE.md` lists `entity_id` generation alongside these three fields, but
//! it does not belong with them. Minting an id is deciding that something is a
//! distinct thing, which is adjudication — Tier 2 — whereas these three are
//! arithmetic over evidence that already exists.
//!
//! # ADR-0042 condition (1)
//!
//! Nothing here may become a required safety input: no `CorridorSource`, no
//! actuator path, no release token. Gate test t24 checks this file by contents.

use std::collections::BTreeMap;

/// Which kind of subject a summary row is about.
///
/// A three-token closed vocabulary, where the *evidence* has only two — see the
/// module docs on the sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SummaryKind {
    /// A resolved entity. The rows entity resolution matches against.
    Entity,
    /// A spatial frame.
    Frame,
    /// No discriminant was stored on the event.
    ///
    /// Not a fourth evidence kind: `world_events.subject_kind` is `NULL` here.
    /// The token exists so the projection can key on a NOT NULL column.
    Unlabelled,
}

impl SummaryKind {
    /// The token stored in the projection.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Frame => "frame",
            Self::Unlabelled => "unlabelled",
        }
    }

    /// Resolve the kind of an event from its stored `subject_kind` column.
    ///
    /// `None` — a `NULL` column — is [`Self::Unlabelled`]. That mapping is the
    /// one place the sentinel is introduced.
    ///
    /// # Errors
    ///
    /// [`SummaryKindError`] for a token the evidence vocabulary does not
    /// contain. **Fail-closed rather than folded into `Unlabelled`**: a row
    /// whose kind cannot be read is a corrupt row, and summarising it as
    /// "nothing said what this was" would file a corruption under a legitimate
    /// case and hide it. `unlabelled` itself is refused from this direction too
    /// — finding the projection's own sentinel in evidence means something
    /// wrote a token that column may not hold.
    pub fn from_event_column(token: Option<&str>) -> Result<Self, SummaryKindError> {
        match token {
            None => Ok(Self::Unlabelled),
            Some("entity") => Ok(Self::Entity),
            Some("frame") => Ok(Self::Frame),
            Some(other) => Err(SummaryKindError::UnknownToken {
                token: other.to_owned(),
            }),
        }
    }

    /// Re-admit a kind from a stored **projection** row.
    ///
    /// Unlike [`Self::from_event_column`] this accepts the sentinel, because
    /// the projection is where the sentinel legitimately lives.
    ///
    /// # Errors
    ///
    /// [`SummaryKindError`] for anything outside the three tokens.
    pub fn from_projection_column(token: &str) -> Result<Self, SummaryKindError> {
        match token {
            "entity" => Ok(Self::Entity),
            "frame" => Ok(Self::Frame),
            "unlabelled" => Ok(Self::Unlabelled),
            other => Err(SummaryKindError::UnknownToken {
                token: other.to_owned(),
            }),
        }
    }
}

/// A kind token that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryKindError {
    /// The token is outside the vocabulary for the column it came from.
    UnknownToken {
        /// The token as stored.
        token: String,
    },
}

impl core::fmt::Display for SummaryKindError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownToken { token } => {
                write!(f, "unreadable subject kind {token:?}")
            }
        }
    }
}

impl std::error::Error for SummaryKindError {}

/// The projection this module maintains.
///
/// Named for what it computes. Calling it `entities_projection` would claim the
/// rows are entities, and until the store carries `SubjectRef` they are
/// subjects — which includes candidates and frames.
pub const SUBJECT_SUMMARY_PROJECTION: &str = "subject_summary";

/// The projection schema, installed lazily by the first fold.
///
/// Separate DDL from `SCHEMA_V1` on purpose — see the module docs.
pub const SUBJECT_PROJECTION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS subject_summary (
    -- NOT NULL, and the sentinel is why -- see the module docs. A nullable
    -- column here makes ON CONFLICT stop matching and the fold stop folding.
    subject_kind       TEXT    NOT NULL
        CHECK (subject_kind IN ('entity','frame','unlabelled')),
    subject            TEXT    NOT NULL,

    -- §6's three open fields, all folded rather than written.
    first_observed_ms  INTEGER NOT NULL,
    last_observed_ms   INTEGER NOT NULL,
    provenance_head    TEXT    NOT NULL,

    -- How many events contributed. Not one of §6's fields, but the fold has it
    -- for free and a summary that cannot distinguish "seen once" from "seen a
    -- thousand times" answers almost nothing about a subject.
    observation_count  INTEGER NOT NULL,

    -- Where provenance_head points, so a reader can locate it in the log
    -- without scanning for a matching digest.
    last_generation    INTEGER NOT NULL,
    last_event_id      TEXT    NOT NULL,

    CHECK (first_observed_ms <= last_observed_ms),
    CHECK (observation_count >= 1),

    PRIMARY KEY (subject_kind, subject)
);

CREATE INDEX IF NOT EXISTS idx_subject_summary_last
    ON subject_summary (last_observed_ms);

-- Resolved entities are the row set entity resolution matches against, so the
-- one query this projection now exists to answer should not be a table scan.
CREATE INDEX IF NOT EXISTS idx_subject_summary_kind
    ON subject_summary (subject_kind, subject);

-- The SAME checkpoint table `PROJECTIONS_V1` declares, repeated because either
-- projection may be folded without the other and each must be able to install
-- what it needs. `IF NOT EXISTS` makes the duplication idempotent rather than a
-- conflict, and the table is keyed by projection name so the two checkpoints
-- coexist as separate rows.
CREATE TABLE IF NOT EXISTS projection_checkpoint (
    name         TEXT    PRIMARY KEY,
    generation   INTEGER NOT NULL,
    state_digest TEXT    NOT NULL
);
"#;

/// One event's contribution to a subject's summary.
///
/// Deliberately narrow: the fold needs identity, a time, and a chain position,
/// and nothing else. Passing the whole row would let a future edit make the
/// summary depend on the payload, which is where a projection stops being
/// summarisable and starts being a second copy of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectObservation {
    /// The event's subject, verbatim.
    pub subject: String,
    /// The event's `subject_kind` column, verbatim — `None` when `NULL`.
    ///
    /// Deliberately the raw column rather than a [`SummaryKind`]: this struct
    /// is what a row *said*, and resolving it is a step that can fail. Doing
    /// that conversion at the boundary keeps an unreadable token a reported
    /// error rather than something the fold has already swallowed.
    pub subject_kind: Option<String>,
    /// When the store learned it.
    ///
    /// **Transaction time, not valid time.** `first_observed` and
    /// `last_observed` are statements about *this store's* encounters with a
    /// subject, so they age on when it learned things — the same choice, for
    /// the same reason, as the retention driver. Valid time would let a
    /// backdated import move a subject's `first_observed` into the past, when
    /// nothing was observed then.
    pub txn_time_ms: i64,
    /// The event's generation.
    pub generation: i64,
    /// The event's identity.
    pub event_id: String,
    /// The event's chain digest — what `provenance_head` points at.
    pub chain_digest: String,
}

/// A subject's folded summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedSubject {
    /// Which kind of subject this summarises — half of the key.
    pub subject_kind: SummaryKind,
    /// The subject this summarises.
    pub subject: String,
    /// Transaction time of the earliest contributing event.
    pub first_observed_ms: i64,
    /// Transaction time of the latest contributing event.
    pub last_observed_ms: i64,
    /// Chain digest of the latest contributing event — §6's `provenance_head`.
    ///
    /// A hash-chain head rather than a count or a timestamp, so a subject's
    /// summary can be *cited*: it names a position in the tamper-evident log
    /// that a reader can verify independently.
    pub provenance_head: String,
    /// How many events contributed.
    pub observation_count: i64,
    /// Generation of the latest contributing event.
    pub last_generation: i64,
    /// Identity of the latest contributing event.
    pub last_event_id: String,
}

/// The accumulator key: a summary is identified by its kind AND its subject.
///
/// An alias rather than a bare tuple at every call site, so the ORDER of the two
/// cannot be swapped silently — both halves are strings once flattened, and
/// `(subject, kind)` would compile everywhere `(kind, subject)` does.
pub type SubjectKey = (SummaryKind, String);

/// Fold one observation into the accumulator. Returns whether it changed.
///
/// # Ordering is not assumed
///
/// The log is walked in generation order, so `last` is almost always the
/// incoming row — but the fold does not rely on that. It compares
/// `last_generation` explicitly and takes the max, because a fold whose
/// correctness depends on its input order is one reordering away from being
/// silently wrong, and generation order is a property of the *caller*, not of
/// this function.
///
/// `first_observed_ms` takes the min for the same reason, and the two are
/// tracked independently: transaction time is monotonic per store today, but
/// deriving one bound from the other would bake that assumption into the data.
pub fn subject_fold_step(
    acc: &mut BTreeMap<SubjectKey, ProjectedSubject>,
    incoming: &SubjectObservation,
) -> Result<bool, SummaryKindError> {
    let kind = SummaryKind::from_event_column(incoming.subject_kind.as_deref())?;
    let key = (kind, incoming.subject.clone());
    match acc.get_mut(&key) {
        None => {
            acc.insert(
                key,
                ProjectedSubject {
                    subject_kind: kind,
                    subject: incoming.subject.clone(),
                    first_observed_ms: incoming.txn_time_ms,
                    last_observed_ms: incoming.txn_time_ms,
                    provenance_head: incoming.chain_digest.clone(),
                    observation_count: 1,
                    last_generation: incoming.generation,
                    last_event_id: incoming.event_id.clone(),
                },
            );
            Ok(true)
        }
        Some(held) => {
            held.observation_count += 1;

            // The TIME BOUNDS are a min and a max over every contributing
            // event, independently of which one is the head. Keeping these two
            // updates separate is what makes the fold order-independent: an
            // earlier version tied `last_observed_ms` to the head, so a later
            // generation carrying an earlier timestamp REGRESSED the maximum
            // and the summary depended on arrival order. Caught by
            // `the_fold_is_order_independent`, which is why that test is worth
            // more than the two it looks like it duplicates.
            held.first_observed_ms = held.first_observed_ms.min(incoming.txn_time_ms);
            held.last_observed_ms = held.last_observed_ms.max(incoming.txn_time_ms);

            // The HEAD follows generation, not time. Generation is the chain's
            // own order and is unique; transaction time can tie, and a head
            // chosen by a tie-break on time would not be reproducible.
            if incoming.generation > held.last_generation {
                held.last_generation = incoming.generation;
                held.provenance_head = incoming.chain_digest.clone();
                held.last_event_id = incoming.event_id.clone();
            }
            Ok(true)
        }
    }
}

/// Fold a whole sequence.
///
/// `BTreeMap` rather than `HashMap` so iteration order is deterministic — the
/// state digest is taken over this order, and a digest that depended on hash
/// seeding would compare unequal to itself.
///
/// # Errors
///
/// [`SummaryKindError`] if any observation carries a kind token the evidence
/// vocabulary does not contain. The fold stops there rather than skipping the
/// row: a projection that silently omitted rows it could not classify would
/// under-report, and under-reporting is indistinguishable from "that subject
/// was never observed".
pub fn subject_fold_all<'a, I: IntoIterator<Item = &'a SubjectObservation>>(
    observations: I,
) -> Result<BTreeMap<SubjectKey, ProjectedSubject>, SummaryKindError> {
    let mut acc = BTreeMap::new();
    for o in observations {
        subject_fold_step(&mut acc, o)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_event_token_is_refused_rather_than_folded_as_unlabelled() {
        // Reading it as Unlabelled would file a corruption under a legitimate
        // case -- "nothing decided what this was" -- and hide it. The two send
        // an investigator to different places.
        assert_eq!(
            SummaryKind::from_event_column(Some("nonsense")),
            Err(SummaryKindError::UnknownToken {
                token: "nonsense".to_string()
            })
        );
        // The projection's OWN sentinel is refused from the evidence direction:
        // finding it there means something wrote a token that column may not
        // hold.
        assert!(SummaryKind::from_event_column(Some("unlabelled")).is_err());
        // ...while the projection direction accepts it, because that is where
        // it legitimately lives.
        assert_eq!(
            SummaryKind::from_projection_column("unlabelled"),
            Ok(SummaryKind::Unlabelled)
        );
    }

    #[test]
    fn a_null_event_column_is_the_sentinel() {
        assert_eq!(
            SummaryKind::from_event_column(None),
            Ok(SummaryKind::Unlabelled)
        );
        assert_eq!(SummaryKind::Unlabelled.as_str(), "unlabelled");
    }

    #[test]
    fn the_three_kinds_have_three_distinct_tokens() {
        let t = [
            SummaryKind::Entity.as_str(),
            SummaryKind::Frame.as_str(),
            SummaryKind::Unlabelled.as_str(),
        ];
        let mut seen: Vec<&str> = Vec::new();
        for x in t {
            assert!(!seen.contains(&x), "token {x:?} used twice");
            seen.push(x);
        }
    }

    /// The accumulator key for an UNLABELLED subject — what every observation
    /// in these tests is, since `obs` builds them without a kind.
    fn ukey(subject: &str) -> SubjectKey {
        (SummaryKind::Unlabelled, subject.to_string())
    }

    fn obs(subject: &str, txn: i64, generation: i64) -> SubjectObservation {
        SubjectObservation {
            subject_kind: None,
            subject: subject.to_string(),
            txn_time_ms: txn,
            generation,
            event_id: format!("evt-{generation}"),
            chain_digest: format!("digest-{generation}"),
        }
    }

    #[test]
    fn a_single_observation_bounds_itself() {
        let got = subject_fold_all(&[obs("cup-1", 100, 1)]).expect("readable kinds");
        let s = &got[&ukey("cup-1")];
        assert_eq!(s.first_observed_ms, 100);
        assert_eq!(s.last_observed_ms, 100);
        assert_eq!(s.observation_count, 1);
        assert_eq!(s.provenance_head, "digest-1");
    }

    #[test]
    fn repeated_observations_widen_the_bounds_and_advance_the_head() {
        let got = subject_fold_all(&[obs("cup-1", 100, 1), obs("cup-1", 300, 2)])
            .expect("readable kinds");
        let s = &got[&ukey("cup-1")];
        assert_eq!(s.first_observed_ms, 100);
        assert_eq!(s.last_observed_ms, 300);
        assert_eq!(s.observation_count, 2);
        assert_eq!(s.provenance_head, "digest-2");
        assert_eq!(s.last_generation, 2);
    }

    #[test]
    fn subjects_are_summarised_independently() {
        let got = subject_fold_all(&[obs("cup-1", 100, 1), obs("cup-2", 200, 2)])
            .expect("readable kinds");
        assert_eq!(got.len(), 2);
        assert_eq!(got[&ukey("cup-1")].observation_count, 1);
        assert_eq!(got[&ukey("cup-2")].first_observed_ms, 200);
    }

    /// The fold must not depend on its input order. Generation order is the
    /// caller's property, not this function's.
    #[test]
    fn the_fold_is_order_independent() {
        let a = [
            obs("cup-1", 100, 1),
            obs("cup-1", 300, 2),
            obs("cup-1", 200, 3),
        ];
        let forward = subject_fold_all(&a).expect("readable kinds");

        let mut reversed: Vec<&SubjectObservation> = a.iter().collect();
        reversed.reverse();
        let backward = subject_fold_all(reversed).expect("readable kinds");

        assert_eq!(forward, backward, "reordering must not change the summary");
    }

    /// The head follows the chain's own order, not the clock. Generation is
    /// unique; transaction time can tie, and a head chosen by a tie-break on
    /// time would not be reproducible.
    #[test]
    fn the_head_follows_generation_not_time() {
        // Generation 3 carries an EARLIER timestamp than generation 2.
        let got = subject_fold_all(&[
            obs("cup-1", 100, 1),
            obs("cup-1", 500, 2),
            obs("cup-1", 200, 3),
        ])
        .expect("readable kinds");
        let s = &got[&ukey("cup-1")];
        assert_eq!(
            s.provenance_head, "digest-3",
            "the head is the newest chain position"
        );
        assert_eq!(s.last_generation, 3);
        assert_eq!(
            s.last_observed_ms, 500,
            "but the observed bound is still the latest time seen"
        );
    }

    /// Out-of-order arrival must still widen `first_observed` downward.
    #[test]
    fn an_earlier_time_arriving_later_moves_first_observed_back() {
        let got = subject_fold_all(&[obs("cup-1", 300, 1), obs("cup-1", 100, 2)])
            .expect("readable kinds");
        assert_eq!(got[&ukey("cup-1")].first_observed_ms, 100);
    }

    /// The bounds are tracked independently rather than one derived from the
    /// other — a single observation makes them equal, and nothing may collapse
    /// them into one field on that basis.
    #[test]
    fn the_two_bounds_are_independent() {
        let mut acc = BTreeMap::new();
        subject_fold_step(&mut acc, &obs("cup-1", 100, 1)).expect("readable kinds");
        assert_eq!(
            acc[&ukey("cup-1")].first_observed_ms,
            acc[&ukey("cup-1")].last_observed_ms
        );
        subject_fold_step(&mut acc, &obs("cup-1", 900, 2)).expect("readable kinds");
        assert_ne!(
            acc[&ukey("cup-1")].first_observed_ms,
            acc[&ukey("cup-1")].last_observed_ms
        );
        assert_eq!(acc[&ukey("cup-1")].first_observed_ms, 100);
    }

    /// Folding from zero must equal folding incrementally from a partial
    /// accumulator — the purity property ADR-0041 calls out as something to
    /// test rather than hope for.
    #[test]
    fn incremental_equals_full_rebuild() {
        let all = [
            obs("cup-1", 100, 1),
            obs("cup-2", 150, 2),
            obs("cup-1", 300, 3),
            obs("cup-3", 400, 4),
            obs("cup-2", 500, 5),
        ];

        let full = subject_fold_all(&all).expect("readable kinds");

        let mut incremental = subject_fold_all(&all[..2]).expect("readable kinds");
        for o in &all[2..] {
            subject_fold_step(&mut incremental, o).expect("readable kinds");
        }

        assert_eq!(full, incremental);
    }

    /// An empty log folds to an empty summary rather than erroring — the same
    /// answer the retention driver gives for a store never written to.
    #[test]
    fn an_empty_log_folds_to_nothing() {
        assert!(subject_fold_all(&[]).expect("readable kinds").is_empty());
    }
}
