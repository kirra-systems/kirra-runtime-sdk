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
//! # What this is keyed on, and the honest limit of it
//!
//! **Keyed on `subject`, not on an entity id** — because the store cannot tell
//! the difference.
//!
//! `kirra_world::observation::SubjectRef` distinguishes four cases:
//! `Entity(id)` (resolved), `Candidate(id)` (not yet adjudicated), `Frame(id)`,
//! and `Unbound` (recorded before anything decided what it was about). The
//! storage layer flattens all four into one `subject TEXT NOT NULL` column and
//! keeps no discriminant, so a fold here **cannot** restrict itself to resolved
//! entities. Every distinct subject string gets a row.
//!
//! This is the same shape as the `writer_class`-versus-`origin` finding: the
//! core carries a type, the store carries a proxy, and the proxy cannot answer
//! the question the type was built for. It is recorded rather than papered
//! over, and the module is named for what it actually computes.
//!
//! Fixing it means carrying `SubjectRef`'s discriminant in storage, which
//! touches `subject` — a field inside the canonically-hashed bytes — so it
//! needs the same append-only-when-present treatment the trust axes got, and it
//! is a slice of its own. Entity *resolution* (deciding that two subjects are
//! one thing) is Tier 2 regardless.
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

/// The projection this module maintains.
///
/// Named for what it computes. Calling it `entities_projection` would claim the
/// rows are entities, and until the store carries `SubjectRef` they are
/// subjects — which includes candidates and frames.
pub const ENTITY_SUMMARY_PROJECTION: &str = "subject_summary";

/// The projection schema, installed lazily by the first fold.
///
/// Separate DDL from `SCHEMA_V1` on purpose — see the module docs.
pub const ENTITY_PROJECTION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS subject_summary (
    subject            TEXT    PRIMARY KEY,

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
    CHECK (observation_count >= 1)
);

CREATE INDEX IF NOT EXISTS idx_subject_summary_last
    ON subject_summary (last_observed_ms);
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
pub fn entity_fold_step(
    acc: &mut BTreeMap<String, ProjectedSubject>,
    incoming: &SubjectObservation,
) -> bool {
    match acc.get_mut(&incoming.subject) {
        None => {
            acc.insert(
                incoming.subject.clone(),
                ProjectedSubject {
                    subject: incoming.subject.clone(),
                    first_observed_ms: incoming.txn_time_ms,
                    last_observed_ms: incoming.txn_time_ms,
                    provenance_head: incoming.chain_digest.clone(),
                    observation_count: 1,
                    last_generation: incoming.generation,
                    last_event_id: incoming.event_id.clone(),
                },
            );
            true
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
            true
        }
    }
}

/// Fold a whole sequence.
///
/// `BTreeMap` rather than `HashMap` so iteration order is deterministic — the
/// state digest is taken over this order, and a digest that depended on hash
/// seeding would compare unequal to itself.
#[must_use]
pub fn entity_fold_all<'a, I: IntoIterator<Item = &'a SubjectObservation>>(
    observations: I,
) -> BTreeMap<String, ProjectedSubject> {
    let mut acc = BTreeMap::new();
    for o in observations {
        entity_fold_step(&mut acc, o);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(subject: &str, txn: i64, generation: i64) -> SubjectObservation {
        SubjectObservation {
            subject: subject.to_string(),
            txn_time_ms: txn,
            generation,
            event_id: format!("evt-{generation}"),
            chain_digest: format!("digest-{generation}"),
        }
    }

    #[test]
    fn a_single_observation_bounds_itself() {
        let got = entity_fold_all(&[obs("cup-1", 100, 1)]);
        let s = &got["cup-1"];
        assert_eq!(s.first_observed_ms, 100);
        assert_eq!(s.last_observed_ms, 100);
        assert_eq!(s.observation_count, 1);
        assert_eq!(s.provenance_head, "digest-1");
    }

    #[test]
    fn repeated_observations_widen_the_bounds_and_advance_the_head() {
        let got = entity_fold_all(&[obs("cup-1", 100, 1), obs("cup-1", 300, 2)]);
        let s = &got["cup-1"];
        assert_eq!(s.first_observed_ms, 100);
        assert_eq!(s.last_observed_ms, 300);
        assert_eq!(s.observation_count, 2);
        assert_eq!(s.provenance_head, "digest-2");
        assert_eq!(s.last_generation, 2);
    }

    #[test]
    fn subjects_are_summarised_independently() {
        let got = entity_fold_all(&[obs("cup-1", 100, 1), obs("cup-2", 200, 2)]);
        assert_eq!(got.len(), 2);
        assert_eq!(got["cup-1"].observation_count, 1);
        assert_eq!(got["cup-2"].first_observed_ms, 200);
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
        let forward = entity_fold_all(&a);

        let mut reversed: Vec<&SubjectObservation> = a.iter().collect();
        reversed.reverse();
        let backward = entity_fold_all(reversed);

        assert_eq!(forward, backward, "reordering must not change the summary");
    }

    /// The head follows the chain's own order, not the clock. Generation is
    /// unique; transaction time can tie, and a head chosen by a tie-break on
    /// time would not be reproducible.
    #[test]
    fn the_head_follows_generation_not_time() {
        // Generation 3 carries an EARLIER timestamp than generation 2.
        let got = entity_fold_all(&[
            obs("cup-1", 100, 1),
            obs("cup-1", 500, 2),
            obs("cup-1", 200, 3),
        ]);
        let s = &got["cup-1"];
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
        let got = entity_fold_all(&[obs("cup-1", 300, 1), obs("cup-1", 100, 2)]);
        assert_eq!(got["cup-1"].first_observed_ms, 100);
    }

    /// The bounds are tracked independently rather than one derived from the
    /// other — a single observation makes them equal, and nothing may collapse
    /// them into one field on that basis.
    #[test]
    fn the_two_bounds_are_independent() {
        let mut acc = BTreeMap::new();
        entity_fold_step(&mut acc, &obs("cup-1", 100, 1));
        assert_eq!(
            acc["cup-1"].first_observed_ms,
            acc["cup-1"].last_observed_ms
        );
        entity_fold_step(&mut acc, &obs("cup-1", 900, 2));
        assert_ne!(
            acc["cup-1"].first_observed_ms,
            acc["cup-1"].last_observed_ms
        );
        assert_eq!(acc["cup-1"].first_observed_ms, 100);
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

        let full = entity_fold_all(&all);

        let mut incremental = entity_fold_all(&all[..2]);
        for o in &all[2..] {
            entity_fold_step(&mut incremental, o);
        }

        assert_eq!(full, incremental);
    }

    /// An empty log folds to an empty summary rather than erroring — the same
    /// answer the retention driver gives for a store never written to.
    #[test]
    fn an_empty_log_folds_to_nothing() {
        assert!(entity_fold_all(&[]).is_empty());
    }
}
