//! The read path: a materialized current-state projection, folded from the log.
//!
//! ADR-0041's Option A is *an append-only log plus materialized projections*.
//! The log half shipped in #1350; this is the other half.
//!
//! # The decision that dominates this module
//!
//! `SD-2` and ADR-0040 make it impossible for an LLM to *write* a confirmed
//! fact: the schema refuses the pairing and the hash covers the columns, so a
//! stored row cannot be relabelled either. That is a guarantee about the
//! **write** path.
//!
//! A projection that folded candidates into "current state" would undo it at
//! the **read** path. Once `current(subject)` returns a row, the caller sees a
//! claim about the world; if candidates were mixed in, an LLM's proposal would
//! be indistinguishable from a sensor's confirmed observation at exactly the
//! moment someone acts on it. The write-side guarantee would survive in the
//! table and die at the API.
//!
//! **So the fold is `confirmed`-only, and it is not a filter the caller can
//! turn off.** Candidates are reachable — [`crate::WorldStore::candidates`] —
//! but only by asking for them under that name. This is the same shape as
//! SD-2's own reasoning: the safe reading is the one you get by default, and
//! the other one you have to spell.
//!
//! # Why the projection tables are NOT created at `open`
//!
//! `schema::SCHEMA_V1`'s digest is recorded in every ADR-0041 **D-20** record,
//! and D-20's `log_only_bytes` is the on-disk size of a store holding only the
//! event log. Creating projection tables at `open` would add their root pages
//! to *every* store, including one that never projects — silently moving the
//! log-only figure and invalidating the comparison against D-2 that the whole
//! measurement rests on.
//!
//! So [`PROJECTIONS_V1`] is installed lazily, by the first fold. A store that
//! has never folded is byte-identical to one written before this module
//! existed, and `open_leaves_no_projection_tables` asserts exactly that.
//!
//! # Purity, and why it is testable rather than asserted
//!
//! [`fold_step`] is a pure function of (accumulator, row). Folding the whole
//! log from zero must therefore produce the same state as folding incrementally
//! from a checkpoint — ADR-0041 calls that out as a property to test rather
//! than hope for, and [`crate::WorldStore::projection_state_digest`] makes it a
//! one-line assertion.
//!
//! # ADR-0042 condition (1)
//!
//! Making Kirra World *readable* is the point at which someone might be
//! tempted to feed it to the checker. Nothing here may become a required
//! safety input: no `CorridorSource`, no actuator path, no release token. Gate
//! test t24 checks that by contents, and it checks this file too.

/// The projection schema, installed lazily by the first fold.
///
/// Separate DDL from `SCHEMA_V1` on purpose — see the module docs. Keying is
/// `(subject, predicate_key)` where `predicate_key` is the predicate or `''`,
/// because SQLite permits multiple NULLs in a non-INTEGER primary key and a
/// NULL-keyed row would silently duplicate rather than supersede.
pub const PROJECTIONS_V1: &str = r#"
CREATE TABLE IF NOT EXISTS world_current (
    subject        TEXT    NOT NULL,
    predicate_key  TEXT    NOT NULL,
    predicate      TEXT,
    object         TEXT,

    kind           TEXT    NOT NULL,
    payload        TEXT    NOT NULL,
    frame_id       TEXT,
    map_id         TEXT,
    source         TEXT    NOT NULL,

    valid_from_ms  INTEGER NOT NULL,
    valid_to_ms    INTEGER,
    txn_time_ms    INTEGER NOT NULL,

    generation     INTEGER NOT NULL,
    event_id       TEXT    NOT NULL,

    -- The claim's own chain position, so an answer can be CITED rather than
    -- merely read. Same argument as the subject summary's provenance_head.
    chain_digest   TEXT    NOT NULL DEFAULT '',

    -- The three STORED trust axes (schema v2), carried through so a reader
    -- gets the claim's trust with the claim. Nullable together, exactly as in
    -- world_events: a v1-shaped row is unlabelled, not wrongly labelled.
    --
    -- Validity is deliberately absent. Transition rule 6 computes it at READ
    -- time from a caller-supplied clock, so a column here would be the one
    -- place the rule could be broken.
    origin           TEXT,
    corroboration    TEXT,
    corroboration_n  INTEGER,
    adjudication     TEXT,

    PRIMARY KEY (subject, predicate_key)
);

CREATE INDEX IF NOT EXISTS idx_current_kind    ON world_current (kind, subject);
CREATE INDEX IF NOT EXISTS idx_current_valid   ON world_current (valid_from_ms);
CREATE INDEX IF NOT EXISTS idx_current_object  ON world_current (object);

-- How far the fold has consumed, so an incremental fold is possible at all.
-- `state_digest` is what makes rebuild-equals-incremental checkable rather
-- than merely believed.
"#;

/// **The checkpoint table, shared by every projection.**
///
/// Its own constant rather than a block inside [`PROJECTIONS_V1`], because more
/// than one fold installs it and they must agree about its shape. They did not:
/// the entity fold shipped a two-column `CREATE TABLE IF NOT EXISTS` of its own,
/// which is a no-op when this three-column table already exists — and *wins*
/// when it does not, so whichever fold ran second failed on the missing
/// `state_digest`. One definition makes that disagreement unrepresentable.
pub const PROJECTION_CHECKPOINT_V1: &str = r#"
CREATE TABLE IF NOT EXISTS projection_checkpoint (
    name         TEXT    PRIMARY KEY,
    generation   INTEGER NOT NULL,
    state_digest TEXT    NOT NULL
);
"#;

/// The single projection this module maintains.
pub const CURRENT_PROJECTION: &str = "world_current";

/// One folded claim: the newest `confirmed` claim for a `(subject, predicate)`.
///
/// `valid_to_ms` is carried rather than resolved, and expiry is evaluated at
/// query time against a caller-supplied `now_ms`. Baking a clock into the fold
/// would make the projection's contents depend on when it ran, which would
/// break both determinism and the rebuild-equals-incremental property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedClaim {
    /// Claim subject.
    pub subject: String,
    /// Claim predicate, `None` for predicate-less claims.
    pub predicate: Option<String>,
    /// Claim object.
    pub object: Option<String>,
    /// Claim kind.
    pub kind: String,
    /// Opaque body.
    pub payload: String,
    /// Coordinate frame, when the claim is spatial (SD-4).
    pub frame_id: Option<String>,
    /// Map the claim is relative to.
    pub map_id: Option<String>,
    /// Producer identity.
    pub source: String,
    /// When the fact began holding.
    pub valid_from_ms: i64,
    /// When it stops holding, if bounded.
    pub valid_to_ms: Option<i64>,
    /// When the store learned it.
    pub txn_time_ms: i64,
    /// Generation of the event this claim came from.
    pub generation: i64,
    /// Identity of that event.
    pub event_id: String,
    /// The claim's chain digest — its position in the tamper-evident log.
    ///
    /// This is the **provenance handle**: it lets an answer be cited and
    /// independently verified rather than merely trusted because the API
    /// returned it. Empty only for a projection folded before this column
    /// existed, which the rebuild-on-shape-change in `ensure_projections`
    /// removes.
    pub chain_digest: String,
    /// The three **stored** trust axes, or `None` for an unlabelled claim.
    ///
    /// `Option` rather than a default, for the reason `NewEvent::trust` is:
    /// inventing an origin and an adjudication for a claim that carries none is
    /// the "mush after eighteen months" the four-axis model exists to prevent.
    pub trust: Option<crate::TrustAxes>,
}

impl ProjectedClaim {
    /// The fourth axis, **computed at read time** rather than stored.
    ///
    /// Transition rule 6: validity is not a state the system enters, it is a
    /// question asked with a clock passed in. There is no `validity` column
    /// anywhere — not in `world_events`, not in this projection — which is what
    /// makes the rule unbreakable rather than documented.
    ///
    /// `staleness_budget_ms` comes from the **caller**, not from storage,
    /// because how long a claim stays fresh is a policy of the asking consumer:
    /// a floor plan and a person's location go stale at wildly different rates,
    /// and baking one budget into the row would answer for every reader at once.
    /// `None` means the claim does not go stale — [`Validity::Timeless`].
    #[must_use]
    pub fn validity_at(&self, now_ms: u64, staleness_budget_ms: Option<u64>) -> crate::Validity {
        let window = crate::ValidityWindow {
            valid_from_ms: self.valid_from_ms.max(0).unsigned_abs(),
            valid_to_ms: self.valid_to_ms.map(|v| v.max(0).unsigned_abs()),
            staleness_budget_ms,
        };
        crate::validity_at(&window, now_ms)
    }

    /// Collapse trust to a single grade — **only here, at the query boundary**.
    ///
    /// The blueprint is explicit that the axes are stored separately and
    /// collapsed to a grade only for consumers that ask for one. This is that
    /// boundary, and it is a method rather than a field so the collapse is
    /// something a caller *does*, never something they receive by default.
    ///
    /// `None` when the claim carries no axes: an unlabelled claim has no trust
    /// to grade, and returning a default grade would manufacture one.
    #[must_use]
    pub fn grade_at(
        &self,
        now_ms: u64,
        staleness_budget_ms: Option<u64>,
    ) -> Option<crate::TrustGrade> {
        let axes = self.trust.as_ref()?;
        Some(crate::trust_grade(
            axes,
            self.validity_at(now_ms, staleness_budget_ms),
        ))
    }

    /// The projection key: subject plus predicate-or-empty.
    #[must_use]
    pub fn key(&self) -> (String, String) {
        (
            self.subject.clone(),
            self.predicate.clone().unwrap_or_default(),
        )
    }

    /// Whether this claim still holds at `now_ms`.
    ///
    /// Half-open: a claim with `valid_to_ms == t` does NOT hold at `t`. The
    /// alternative makes two adjacent claims both hold at the instant one ends
    /// and the next begins, which is precisely the ambiguity bitemporality
    /// exists to remove.
    #[must_use]
    pub fn holds_at(&self, now_ms: i64) -> bool {
        self.valid_from_ms <= now_ms && self.valid_to_ms.is_none_or(|end| now_ms < end)
    }
}

/// Does `incoming` supersede `held` for the same key?
///
/// Ordered by `(valid_from_ms, generation)`. Valid time leads because the
/// question a projection answers is about the world, not about the store's
/// filing order; `generation` breaks ties so the rule is **total** — two events
/// asserting different values from the same instant must resolve the same way
/// on every replay, or rebuild-equals-incremental is false.
#[must_use]
pub fn supersedes(incoming: &ProjectedClaim, held: &ProjectedClaim) -> bool {
    (incoming.valid_from_ms, incoming.generation) > (held.valid_from_ms, held.generation)
}

/// The pure fold step.
///
/// Takes the accumulator and one claim, returns whether the claim was adopted.
/// Callers apply it in generation order. Kept free of I/O so the property
/// tests can exercise the *rule* rather than a database.
pub fn fold_step(
    acc: &mut std::collections::BTreeMap<(String, String), ProjectedClaim>,
    incoming: ProjectedClaim,
) -> bool {
    let key = incoming.key();
    match acc.get(&key) {
        Some(held) if !supersedes(&incoming, held) => false,
        _ => {
            acc.insert(key, incoming);
            true
        }
    }
}

/// Fold a whole sequence. `BTreeMap` rather than `HashMap` so iteration order
/// is deterministic — the state digest is taken over this order, and a digest
/// that depends on hash seeding would compare unequal to itself.
#[must_use]
pub fn fold_all<I: IntoIterator<Item = ProjectedClaim>>(
    claims: I,
) -> std::collections::BTreeMap<(String, String), ProjectedClaim> {
    let mut acc = std::collections::BTreeMap::new();
    for c in claims {
        fold_step(&mut acc, c);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(subject: &str, pred: Option<&str>, valid_from: i64, gen: i64) -> ProjectedClaim {
        ProjectedClaim {
            subject: subject.to_string(),
            predicate: pred.map(str::to_string),
            object: None,
            kind: "observation".to_string(),
            payload: "{}".to_string(),
            frame_id: None,
            map_id: None,
            source: "s".to_string(),
            valid_from_ms: valid_from,
            valid_to_ms: None,
            txn_time_ms: valid_from,
            generation: gen,
            chain_digest: format!("digest-{gen}"),
            trust: None,
            event_id: format!("e{gen}"),
        }
    }

    #[test]
    fn later_valid_time_supersedes() {
        let a = claim("cup", Some("colour"), 100, 1);
        let b = claim("cup", Some("colour"), 200, 2);
        assert!(supersedes(&b, &a));
        assert!(!supersedes(&a, &b));
    }

    /// The rule must be TOTAL. Equal valid times with no tiebreak would make
    /// the fold order-dependent, and rebuild-equals-incremental false.
    #[test]
    fn equal_valid_time_is_broken_by_generation() {
        let a = claim("cup", Some("colour"), 100, 1);
        let b = claim("cup", Some("colour"), 100, 2);
        assert!(supersedes(&b, &a));
        assert!(!supersedes(&a, &b));
    }

    /// A late-arriving event about an EARLIER instant must not overwrite a
    /// newer fact just because the store learned it later. This is the whole
    /// reason valid time leads generation.
    #[test]
    fn a_late_arrival_about_the_past_does_not_overwrite_the_present() {
        let now = claim("cup", Some("colour"), 500, 1);
        let backfill = claim("cup", Some("colour"), 100, 2);
        assert!(!supersedes(&backfill, &now));

        let folded = fold_all([now.clone(), backfill]);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded.values().next().unwrap().generation, 1);
    }

    #[test]
    fn distinct_predicates_do_not_collide() {
        let folded = fold_all([
            claim("cup", Some("colour"), 100, 1),
            claim("cup", Some("position"), 100, 2),
        ]);
        assert_eq!(folded.len(), 2);
    }

    /// A predicate-less claim keys on `''`, and must not collide with a claim
    /// whose predicate is literally the empty string only because both map to
    /// the same key — they DO collide, and that is correct, but a `None`
    /// predicate must not collide with a DIFFERENT named predicate.
    #[test]
    fn a_predicate_less_claim_has_its_own_slot() {
        let folded = fold_all([
            claim("cup", None, 100, 1),
            claim("cup", Some("colour"), 100, 2),
        ]);
        assert_eq!(folded.len(), 2);
        assert!(folded.contains_key(&("cup".to_string(), String::new())));
    }

    #[test]
    fn holds_at_is_half_open() {
        let mut c = claim("cup", None, 100, 1);
        c.valid_to_ms = Some(200);
        assert!(!c.holds_at(99));
        assert!(c.holds_at(100));
        assert!(c.holds_at(199));
        assert!(!c.holds_at(200), "valid_to is exclusive");
        assert!(!c.holds_at(201));
    }

    #[test]
    fn an_unbounded_claim_holds_forever_forward() {
        let c = claim("cup", None, 100, 1);
        assert!(c.holds_at(i64::MAX));
        assert!(!c.holds_at(99));
    }

    /// The property ADR-0041 names: folding in any arrival order yields the
    /// same state, because the rule is total and order-independent.
    #[test]
    fn the_fold_is_order_independent() {
        let a = claim("cup", Some("colour"), 100, 1);
        let b = claim("cup", Some("colour"), 300, 2);
        let c = claim("cup", Some("colour"), 200, 3);

        let forward = fold_all([a.clone(), b.clone(), c.clone()]);
        let reversed = fold_all([c, b, a]);
        assert_eq!(forward, reversed);
        assert_eq!(forward.values().next().unwrap().valid_from_ms, 300);
    }
}
