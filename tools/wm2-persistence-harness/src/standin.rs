//! The stand-in store: a concrete-enough instance of ADR-0041's Option A to
//! benchmark, and nothing more.
//!
//! # This schema is a stand-in and the harness says so in every record
//!
//! ADR-0041 fixes the *shape* — an append-only event log, materialized
//! projections, checkpoints, a hash chain, `PRAGMA user_version` migrations —
//! and states that "column-level schemas are deliberately **not** fixed here".
//! That leaves a gap the harness cannot avoid: you cannot measure an
//! unspecified schema, so measuring at all means inventing one.
//!
//! The risk that creates is specific. A number produced against this schema
//! reads exactly like a number produced against the real one, and six months
//! later nobody remembers which. So [`schema_digest`] hashes the DDL and every
//! emitted result carries it. If the ratified schema differs by so much as an
//! index, the digest differs, and an old measurement is visibly about something
//! else rather than quietly authoritative about this.
//!
//! # What the stand-in does and does not model
//!
//! Modelled, because ADR-0041's decision depends on them:
//!
//! - the append-only log as the only writable table, with a total replay order;
//! - bitemporality — valid time and transaction time as separate columns, since
//!   the whole time-travel query family collapses without both;
//! - a hash chain over events (tamper evidence, blueprint P3);
//! - projections as a *pure fold*, so "rebuild from zero equals the incremental
//!   state" is a testable property rather than an aspiration;
//! - checkpoints, because the checkpointed rebuild is separately measured;
//! - `PRAGMA user_version` migrations that fail closed on a future schema.
//!
//! Not modelled, deliberately:
//!
//! - the four trust axes, provenance DAG, identity adjudication, compaction.
//!   Each is a domain decision that ADR-0040/0041 leave open, and inventing one
//!   here to benchmark it would be manufacturing the appearance of a decision.
//!   Their storage cost is a row-width question the growth measurement already
//!   parameterizes.
//!
//! The consequence is honest and should be stated when citing any result: this
//! measures the *substrate under a representative load*, not the finished World
//! Model. It is enough to decide SQLite-versus-graph-database, which is what
//! ADR-0041 is actually blocked on.

use crate::sha256;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// The stand-in schema. Its digest is stamped on every result record.
pub const SCHEMA_V1: &str = r#"
CREATE TABLE world_events (
    generation      INTEGER PRIMARY KEY,
    event_id        TEXT    NOT NULL UNIQUE,
    txn_time_ms     INTEGER NOT NULL,
    valid_from_ms   INTEGER NOT NULL,
    valid_to_ms     INTEGER,
    source          TEXT    NOT NULL,
    source_version  TEXT    NOT NULL,
    payload_schema  INTEGER NOT NULL,
    kind            TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    predicate       TEXT,
    object          TEXT,
    payload         TEXT    NOT NULL,
    payload_digest  TEXT    NOT NULL,
    chain_digest    TEXT    NOT NULL,
    retention_class TEXT    NOT NULL DEFAULT 'raw',
    redacted        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_events_subject_valid ON world_events (subject, valid_from_ms);
CREATE INDEX idx_events_txn           ON world_events (txn_time_ms);
CREATE INDEX idx_events_kind          ON world_events (kind, generation);

CREATE TABLE entities_projection (
    entity_id     TEXT PRIMARY KEY,
    kind          TEXT    NOT NULL,
    place_ref     TEXT    NOT NULL,
    valid_from_ms INTEGER NOT NULL,
    generation    INTEGER NOT NULL
);
CREATE INDEX idx_entities_place ON entities_projection (place_ref, kind);

CREATE TABLE relationships_projection (
    subject    TEXT    NOT NULL,
    predicate  TEXT    NOT NULL,
    object     TEXT    NOT NULL,
    generation INTEGER NOT NULL,
    PRIMARY KEY (subject, predicate, object)
);
CREATE INDEX idx_rel_object ON relationships_projection (object, predicate);

CREATE TABLE projection_checkpoints (
    name         TEXT PRIMARY KEY,
    generation   INTEGER NOT NULL,
    state_digest TEXT    NOT NULL
);

-- The citation half of compaction-with-citation (ADR-0041 §11.3). A compacted
-- window leaves this row behind: what was summarized, how much of it, the
-- digest of the removed span, and the chain digests on either side so the log
-- stays verifiable ACROSS the hole rather than merely up to it.
CREATE TABLE compaction_citations (
    summary_generation  INTEGER PRIMARY KEY,
    lo_generation       INTEGER NOT NULL,
    hi_generation       INTEGER NOT NULL,
    event_count         INTEGER NOT NULL,
    range_digest        TEXT    NOT NULL,
    chain_before        TEXT    NOT NULL,
    chain_after         TEXT    NOT NULL,
    compacted_at_ms     INTEGER NOT NULL
);
"#;

/// The v2 step used by the migration proof of concept: a new nullable column
/// plus a backfill, which is the realistic shape of a schema change against a
/// populated store (adding an unpopulated column is not a migration, it is a
/// DDL statement).
///
/// # This form is QUADRATIC, and it is kept deliberately
///
/// The backfill is a correlated scalar subquery, and SQLite plans it as
/// `SEARCH world_events USING INDEX idx_events_kind (kind=?)` — keyed on the
/// kind alone, so **every projection row walks every observation event**.
/// `idx_events_subject_valid` goes unused. Cost is therefore
/// `O(events × entities)`, not `O(events)`.
///
/// ADR-0041 D-6 measured this statement and extrapolated it linearly in events;
/// D-13 shows the model is wrong and the number describes this SQL rather than
/// the store. It is retained so that measurement stays reproducible — deleting
/// it would orphan an archived target result — and as the negative control for
/// [`SCHEMA_V2_STEP_GROUPED`], which is what a migration should actually look
/// like.
///
/// **Do not copy this shape into a real migration.** The gate that keeps it
/// from coming back is `the_backfill_must_not_rescan_the_log_per_entity` in
/// this module's test submodule — named in plain text rather than linked,
/// because `tests` is `#[cfg(test)]` and an intra-doc link to it does not
/// resolve in a normal doc build.
pub const SCHEMA_V2_STEP: &str = r#"
ALTER TABLE entities_projection ADD COLUMN observed_count INTEGER NOT NULL DEFAULT 0;
UPDATE entities_projection SET observed_count = (
    SELECT COUNT(*) FROM world_events
     WHERE world_events.subject = entities_projection.entity_id
       AND world_events.kind = 'observation'
);
"#;

/// The same v2 migration, as a single grouped pass over the log.
///
/// `UPDATE … FROM` a materialized `GROUP BY` plans as one scan of the log
/// followed by an indexed lookup per projection row — `O(events + entities)`.
/// Entities with no observations keep the column's `DEFAULT 0`, which is what
/// the correlated form's `COUNT(*)` of an empty set produces, so the two are
/// equivalent by construction as well as by test.
///
/// On the same database this is ~3 100× faster than [`SCHEMA_V2_STEP`]
/// (93 132 ms → 30 ms, ADR-0041 D-13). The difference is entirely the query
/// plan; the schemas and the results are identical.
pub const SCHEMA_V2_STEP_GROUPED: &str = r#"
ALTER TABLE entities_projection ADD COLUMN observed_count INTEGER NOT NULL DEFAULT 0;
UPDATE entities_projection SET observed_count = g.c
  FROM (SELECT subject, COUNT(*) AS c
          FROM world_events
         WHERE kind = 'observation'
         GROUP BY subject) AS g
 WHERE g.subject = entities_projection.entity_id;
"#;

/// The schema version this binary understands. A database stamped higher than
/// this is refused, never opened optimistically.
pub const KNOWN_SCHEMA_VERSION: i64 = 1;

pub fn schema_digest() -> String {
    sha256::hex(SCHEMA_V1.as_bytes())
}

/// `PRAGMA synchronous` policy under measurement.
///
/// ADR-0041 open question 1 asks for a `synchronous` policy *per source class*
/// and explicitly forbids inheriting the verifier's choice. That is not a
/// preference to settle in review — it is the trade the harness exists to
/// quantify, so all three settings are measurable rather than one being
/// hardcoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// `synchronous=FULL` — fsync per commit; survives hard power loss.
    Full,
    /// `synchronous=NORMAL` — survives process death, may lose the un-fsynced
    /// WAL tail on power loss.
    Normal,
    /// `synchronous=OFF` — measured only to bound the fsync term. Never a
    /// candidate for evidence storage, and the report labels it as such.
    Off,
}

impl Durability {
    pub fn pragma(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Normal => "NORMAL",
            Self::Off => "OFF",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "normal" => Some(Self::Normal),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn all() -> [Durability; 3] {
        [Self::Full, Self::Normal, Self::Off]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Observation,
    Relationship,
    /// The citation left behind by a compacted window. Written by
    /// [`crate::compact`], never by a producer.
    Summary,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Relationship => "relationship",
            Self::Summary => "summary",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "observation" => Self::Observation,
            "summary" => Self::Summary,
            _ => Self::Relationship,
        }
    }
}

/// Retention class, deciding what compaction is allowed to touch.
///
/// ADR-0041 §11.3 names the protected set explicitly: safety-relevant events,
/// incident windows, calibration, identity adjudications, and operator-confirmed
/// events must be retained longer than raw observation traffic. Only `Raw` is
/// compactable, and the harness proves the distinction is enforced rather than
/// documented -- the compaction pass refuses a window containing anything else,
/// which is the property that decides whether "compaction loses something
/// needed for an incident" (risk R4) is mitigated or merely hoped for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionClass {
    Raw,
    Safety,
    Incident,
    Calibration,
    Adjudication,
    Operator,
}

impl RetentionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Safety => "safety",
            Self::Incident => "incident",
            Self::Calibration => "calibration",
            Self::Adjudication => "adjudication",
            Self::Operator => "operator",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "raw" => Self::Raw,
            "safety" => Self::Safety,
            "incident" => Self::Incident,
            "calibration" => Self::Calibration,
            "adjudication" => Self::Adjudication,
            "operator" => Self::Operator,
            // An UNRECOGNISED class is treated as protected, not as raw. A
            // typo in a retention policy must never make an event eligible for
            // deletion.
            _ => Self::Safety,
        }
    }

    pub fn is_protected(self) -> bool {
        !matches!(self, Self::Raw)
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub generation: u64,
    pub event_id: String,
    pub txn_time_ms: i64,
    pub valid_from_ms: i64,
    pub valid_to_ms: Option<i64>,
    pub source: String,
    pub source_version: String,
    pub kind: EventKind,
    pub subject: String,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub payload: String,
    pub retention: RetentionClass,
}

impl Event {
    /// Canonical bytes hashed into `payload_digest`.
    ///
    /// Field-separated with a byte that cannot occur in any field, so two
    /// different events cannot serialize to the same bytes by concatenation —
    /// the same boundary-confusion problem the chain link's separator solves.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.payload.len() + 128);
        let mut push = |s: &str| {
            buf.extend_from_slice(s.as_bytes());
            buf.push(0x1f);
        };
        push(&self.event_id);
        push(&self.generation.to_string());
        push(&self.txn_time_ms.to_string());
        push(&self.valid_from_ms.to_string());
        push(&self.valid_to_ms.map(|v| v.to_string()).unwrap_or_default());
        push(&self.source);
        push(&self.source_version);
        push(self.kind.as_str());
        push(&self.subject);
        push(self.predicate.as_deref().unwrap_or(""));
        push(self.object.as_deref().unwrap_or(""));
        push(&self.payload);
        push(self.retention.as_str());
        buf
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    /// Every link recomputes, and the log is a contiguous generation run —
    /// contiguous *modulo* cited compaction windows, which are counted rather
    /// than hidden. A verifier reporting a plain "intact" over a compacted log
    /// would be concealing exactly the degradation compaction causes, and
    /// `redacted` does the same job for tombstones: both are states a reader
    /// must be able to see without going looking.
    Intact {
        entries: u64,
        head: String,
        compacted_windows: u64,
        redacted: u64,
    },
    /// A link does not recompute — tampering or corruption.
    Broken {
        at_generation: u64,
    },
    /// Generations skip. A gap is not a broken chain (the links still verify),
    /// but it is not an intact log either, and collapsing the two would let a
    /// truncated middle read as healthy.
    Gap {
        after_generation: u64,
    },
    Empty,
}

/// Refusal to open, kept separate from a plain SQLite error because the
/// fail-closed-on-future-schema case is a *result* the migration PoC asserts,
/// not an incident.
#[derive(Debug)]
pub enum OpenError {
    FutureSchema { found: i64, known: i64 },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FutureSchema { found, known } => write!(
                f,
                "database schema version {found} is newer than this binary understands ({known}); \
                 refusing to open — a newer writer's rows may not mean what this code assumes"
            ),
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
        }
    }
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

pub struct Store {
    pub conn: Connection,
    pub durability: Durability,
}

impl Store {
    /// Open (creating if needed), apply pragmas, and migrate to
    /// [`KNOWN_SCHEMA_VERSION`].
    ///
    /// Fail-closed on a future schema, mirroring `kirra-persistence`'s
    /// `assert_schema_not_future`. This is the one behaviour the harness copies
    /// from the production migration framework rather than inventing, because
    /// the migration PoC is asserting that the *policy* survives contact with a
    /// populated store.
    pub fn open(path: &Path, durability: Durability) -> Result<Self, OpenError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous={};",
            durability.pragma()
        ))?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > KNOWN_SCHEMA_VERSION {
            return Err(OpenError::FutureSchema {
                found: version,
                known: KNOWN_SCHEMA_VERSION,
            });
        }
        if version == 0 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.execute_batch(&format!("PRAGMA user_version={KNOWN_SCHEMA_VERSION}"))?;
        }
        Ok(Self { conn, durability })
    }

    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))
    }

    pub fn head_chain(&self) -> rusqlite::Result<String> {
        Ok(self
            .conn
            .query_row(
                "SELECT chain_digest FROM world_events ORDER BY generation DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| "genesis".to_string()))
    }

    pub fn count_events(&self) -> rusqlite::Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM world_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
    }

    /// Append events inside ONE transaction.
    ///
    /// `batch = 1` is ADR-0041's proposed "one event append per transaction";
    /// larger batches are measured too, because the difference between them is
    /// the fsync amortization that decides whether the proposal is viable at
    /// sensor rates.
    pub fn append_batch(&mut self, events: &[Event]) -> rusqlite::Result<()> {
        let mut prev = self.head_chain()?;
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO world_events (generation, event_id, txn_time_ms, valid_from_ms, \
                 valid_to_ms, source, source_version, payload_schema, kind, subject, predicate, \
                 object, payload, payload_digest, chain_digest, retention_class) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            )?;
            for ev in events {
                let payload_digest = sha256::hex(&ev.canonical_bytes());
                let chain = sha256::chain_link(&prev, payload_digest.as_bytes());
                stmt.execute(params![
                    ev.generation as i64,
                    ev.event_id,
                    ev.txn_time_ms,
                    ev.valid_from_ms,
                    ev.valid_to_ms,
                    ev.source,
                    ev.source_version,
                    1i64,
                    ev.kind.as_str(),
                    ev.subject,
                    ev.predicate,
                    ev.object,
                    ev.payload,
                    payload_digest,
                    chain,
                    ev.retention.as_str(),
                ])?;
                prev = chain;
            }
        }
        tx.commit()
    }

    /// Recompute every link in generation order, across compaction and
    /// redaction.
    ///
    /// # What compaction does to tamper evidence
    ///
    /// Deleting events from a hash-chained log breaks it. The resolution here
    /// is the "citation" half of ADR-0041's compaction-with-citation: a
    /// compacted window leaves a `Summary` event plus a
    /// `compaction_citations` row recording the chain digest on BOTH sides of
    /// the removed span. Verification links the summary from `chain_before`,
    /// then resumes from `chain_after` — so the events written after the window
    /// still verify against the links they were originally computed from, and
    /// nothing downstream has to be re-chained.
    ///
    /// The honest consequence, which the caller must not lose: after
    /// compaction you can no longer verify the *contents* of the removed span,
    /// only that a span was removed, how large it was, and what it hashed to.
    /// Full tamper evidence degrades to a tamper-evident citation OF a removed
    /// span. That is a real loss, and it is why `Intact` carries
    /// `compacted_windows` — a verifier that flattened this to a plain "intact"
    /// would be concealing the one thing about the log that changed.
    ///
    /// A redacted event keeps its row and its ORIGINAL stored `payload_digest`,
    /// so the chain stays continuous while the content is gone. That is
    /// ADR-0041's rule that "a redaction must leave a tombstone, or the chain
    /// breaks", made mechanical: absence is never how a deletion is
    /// represented.
    pub fn verify_chain(&self) -> rusqlite::Result<ChainStatus> {
        let mut stmt = self.conn.prepare(
            "SELECT generation, event_id, txn_time_ms, valid_from_ms, valid_to_ms, source, \
             source_version, kind, subject, predicate, object, payload, chain_digest, \
             retention_class, redacted, payload_digest \
             FROM world_events ORDER BY generation ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut prev = "genesis".to_string();
        let mut count: u64 = 0;
        let mut compacted_windows: u64 = 0;
        let mut redacted_count: u64 = 0;
        let mut expect_generation: Option<u64> = None;

        while let Some(row) = rows.next()? {
            let generation = row.get::<_, i64>(0)? as u64;
            if let Some(expected) = expect_generation {
                if generation != expected {
                    return Ok(ChainStatus::Gap {
                        after_generation: expected - 1,
                    });
                }
            }
            let kind = EventKind::parse(&row.get::<_, String>(7)?);
            let redacted: i64 = row.get(14)?;
            let stored_chain: String = row.get(12)?;
            let stored_payload_digest: String = row.get(15)?;

            let ev = Event {
                generation,
                event_id: row.get(1)?,
                txn_time_ms: row.get(2)?,
                valid_from_ms: row.get(3)?,
                valid_to_ms: row.get(4)?,
                source: row.get(5)?,
                source_version: row.get(6)?,
                kind,
                subject: row.get(8)?,
                predicate: row.get(9)?,
                object: row.get(10)?,
                payload: row.get(11)?,
                retention: RetentionClass::parse(&row.get::<_, String>(13)?),
            };

            // A tombstone's content is gone by design, so its digest cannot be
            // recomputed — the stored one is what the chain was built on and
            // remains the thing being attested.
            let payload_digest = if redacted != 0 {
                redacted_count += 1;
                stored_payload_digest
            } else {
                sha256::hex(&ev.canonical_bytes())
            };

            if sha256::chain_link(&prev, payload_digest.as_bytes()) != stored_chain {
                return Ok(ChainStatus::Broken {
                    at_generation: generation,
                });
            }
            count += 1;

            if kind == EventKind::Summary {
                // Resume from the far side of the cited window.
                let cited: Option<(i64, i64, String, String)> = self
                    .conn
                    .query_row(
                        "SELECT lo_generation, hi_generation, chain_before, chain_after \
                         FROM compaction_citations WHERE summary_generation = ?1",
                        params![generation as i64],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let Some((_lo, hi, chain_before, chain_after)) = cited else {
                    // A summary with no citation is an uncited hole: exactly
                    // the "compaction silently rewriting history" this design
                    // forbids.
                    return Ok(ChainStatus::Broken {
                        at_generation: generation,
                    });
                };
                if chain_before != prev {
                    return Ok(ChainStatus::Broken {
                        at_generation: generation,
                    });
                }
                compacted_windows += 1;
                prev = chain_after;
                expect_generation = Some(hi as u64 + 1);
                continue;
            }

            prev = stored_chain;
            expect_generation = Some(generation + 1);
        }

        if count == 0 {
            Ok(ChainStatus::Empty)
        } else {
            Ok(ChainStatus::Intact {
                entries: count,
                head: prev,
                compacted_windows,
                redacted: redacted_count,
            })
        }
    }

    /// Fold events into the projections.
    ///
    /// Pure in the sense ADR-0041 requires: the result is a function of
    /// `(prior projection state, event sequence)` alone. No clock is read — the
    /// event's own `valid_from_ms` decides supersession — which is what makes
    /// the rebuild-equals-incremental property checkable at all.
    ///
    /// Supersession rule: a later `valid_from_ms` wins; ties break on
    /// `generation`, which is total, so the fold is deterministic even when two
    /// sources report the same instant.
    pub fn fold_from(&mut self, from_generation: u64) -> rusqlite::Result<u64> {
        let tx = self.conn.transaction()?;
        let applied: i64 = {
            tx.execute(
                // `substr(payload, 1, 10)` lifts the `place-NNNN` reference out
                // of the payload. The projection stores the reference, not the
                // whole payload: a projection that keeps the raw event body is
                // a second copy of the log, which is how the two
                // representations drift apart.
                "INSERT INTO entities_projection (entity_id, kind, place_ref, valid_from_ms, generation)
                 SELECT subject, 'entity', substr(payload, 1, 10), valid_from_ms, generation FROM world_events
                  WHERE kind = 'observation' AND generation >= ?1
                  ORDER BY valid_from_ms ASC, generation ASC
                 ON CONFLICT(entity_id) DO UPDATE SET
                    place_ref     = excluded.place_ref,
                    valid_from_ms = excluded.valid_from_ms,
                    generation    = excluded.generation
                  WHERE (excluded.valid_from_ms, excluded.generation)
                        > (entities_projection.valid_from_ms, entities_projection.generation)",
                params![from_generation as i64],
            )?;
            tx.execute(
                "INSERT INTO relationships_projection (subject, predicate, object, generation)
                 SELECT subject, predicate, object, generation FROM world_events
                  WHERE kind = 'relationship' AND generation >= ?1
                    AND predicate IS NOT NULL AND object IS NOT NULL
                  ORDER BY generation ASC
                 ON CONFLICT(subject, predicate, object) DO UPDATE SET
                    generation = excluded.generation
                  WHERE excluded.generation > relationships_projection.generation",
                params![from_generation as i64],
            )?;
            tx.query_row(
                "SELECT COUNT(*) FROM world_events WHERE generation >= ?1",
                params![from_generation as i64],
                |r| r.get(0),
            )?
        };
        tx.commit()?;
        Ok(applied as u64)
    }

    pub fn clear_projections(&mut self) -> rusqlite::Result<()> {
        self.conn
            .execute_batch("DELETE FROM entities_projection; DELETE FROM relationships_projection;")
    }

    /// A digest over the projection state, used to assert that a full rebuild
    /// equals the incrementally maintained state (ADR-0041 "replay
    /// validation"). Ordered explicitly — an unordered scan would produce a
    /// digest that depends on SQLite's storage order and compare unequal for
    /// reasons that have nothing to do with the fold.
    pub fn projection_digest(&self) -> rusqlite::Result<String> {
        let mut buf = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT entity_id, kind, place_ref, valid_from_ms, generation \
             FROM entities_projection ORDER BY entity_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            buf.extend_from_slice(r.get::<_, String>(0)?.as_bytes());
            buf.push(0x1f);
            buf.extend_from_slice(r.get::<_, String>(1)?.as_bytes());
            buf.push(0x1f);
            buf.extend_from_slice(r.get::<_, String>(2)?.as_bytes());
            buf.push(0x1f);
            buf.extend_from_slice(r.get::<_, i64>(3)?.to_string().as_bytes());
            buf.push(0x1e);
        }
        let mut stmt = self.conn.prepare(
            "SELECT subject, predicate, object FROM relationships_projection \
             ORDER BY subject, predicate, object",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            for i in 0..3 {
                buf.extend_from_slice(r.get::<_, String>(i)?.as_bytes());
                buf.push(0x1f);
            }
            buf.push(0x1e);
        }
        Ok(sha256::hex(&buf))
    }

    pub fn write_checkpoint(&mut self, name: &str) -> rusqlite::Result<u64> {
        let generation: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(generation), -1) FROM world_events",
            [],
            |r| r.get(0),
        )?;
        let digest = self.projection_digest()?;
        self.conn.execute(
            "INSERT INTO projection_checkpoints (name, generation, state_digest) VALUES (?1,?2,?3)
             ON CONFLICT(name) DO UPDATE SET generation = excluded.generation,
                                             state_digest = excluded.state_digest",
            params![name, generation, digest],
        )?;
        Ok((generation + 1) as u64)
    }

    pub fn read_checkpoint(&self, name: &str) -> rusqlite::Result<Option<(u64, String)>> {
        self.conn
            .query_row(
                "SELECT generation, state_digest FROM projection_checkpoints WHERE name = ?1",
                params![name],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map(|o| o.map(|(g, d)| ((g + 1) as u64, d)))
    }

    /// fsync the WAL into the main database file. Used by the power-loss tier
    /// to establish exactly where the durable prefix ends.
    pub fn durable_checkpoint(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
    }

    /// Apply the v2 migration step to a populated store.
    /// Apply the v2 migration using the historical (quadratic) backfill.
    ///
    /// Retained so the archived D-6 measurement stays reproducible. The CLI
    /// selects between this and [`SCHEMA_V2_STEP_GROUPED`] via
    /// [`Store::migrate_to_v2_using`].
    #[cfg(test)]
    pub fn migrate_to_v2(&mut self) -> rusqlite::Result<()> {
        self.migrate_to_v2_using(SCHEMA_V2_STEP)
    }

    pub fn migrate_to_v2_using(&mut self, step: &str) -> rusqlite::Result<()> {
        self.conn.execute_batch("BEGIN EXCLUSIVE;")?;
        match self.conn.execute_batch(step) {
            Ok(()) => {
                self.conn.execute_batch("PRAGMA user_version=2; COMMIT;")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Corrupt one stored link, for the tamper-detection control.
    ///
    /// Compiled only under `cfg(test)`: a harness that ships a
    /// tamper-the-ledger method is a worse thing than the warning it silences.
    ///
    /// Without this the "chain intact" assertions are unfalsifiable: a verifier
    /// that always returns `Intact` would pass every positive test in this
    /// harness. The audit-chain drill in the main repository anchors its
    /// non-vacuousness the same way.
    #[cfg(test)]
    pub fn tamper_for_test(&mut self, generation: u64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE world_events SET payload = payload || 'x' WHERE generation = ?1",
            params![generation as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wm2-standin-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn store(name: &str) -> (Store, std::path::PathBuf) {
        let path = temp(name);
        let s = Store::open(&path, Durability::Normal).expect("open");
        (s, path)
    }

    #[test]
    fn chain_verifies_and_tamper_is_detected() {
        let (mut s, path) = store("chain");
        s.append_batch(&gen::events(0, 50, 7, 4)).unwrap();
        match s.verify_chain().unwrap() {
            ChainStatus::Intact { entries, .. } => assert_eq!(entries, 50),
            other => panic!("expected intact, got {other:?}"),
        }
        // The control: if this passed too, the positive assertion above would
        // mean nothing.
        s.tamper_for_test(20).unwrap();
        assert_eq!(
            s.verify_chain().unwrap(),
            ChainStatus::Broken { at_generation: 20 }
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_generation_gap_is_not_reported_as_intact() {
        let (mut s, path) = store("gap");
        s.append_batch(&gen::events(0, 20, 5, 3)).unwrap();
        s.conn
            .execute("DELETE FROM world_events WHERE generation = 10", [])
            .unwrap();
        // Every surviving link still recomputes, so a naive verifier says
        // "intact" on a log with a hole punched in the middle.
        assert_eq!(
            s.verify_chain().unwrap(),
            ChainStatus::Gap {
                after_generation: 9
            }
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_log_is_empty_not_intact() {
        let (s, path) = store("empty");
        assert_eq!(s.verify_chain().unwrap(), ChainStatus::Empty);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn full_rebuild_equals_incremental_fold() {
        // ADR-0041's "replay validation" requirement, which is the property
        // that makes the projections trustworthy at all.
        let (mut s, path) = store("rebuild");
        let events = gen::events(0, 400, 40, 11);
        for chunk in events.chunks(37) {
            s.append_batch(chunk).unwrap();
            s.fold_from(chunk[0].generation).unwrap();
        }
        let incremental = s.projection_digest().unwrap();

        s.clear_projections().unwrap();
        s.fold_from(0).unwrap();
        let rebuilt = s.projection_digest().unwrap();

        assert_eq!(incremental, rebuilt, "the fold is not deterministic");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_fold_is_order_independent_across_batch_boundaries() {
        // Same events, different batching. If supersession depended on arrival
        // order rather than (valid_from_ms, generation), these would diverge —
        // and the divergence would only show up under a different batch size in
        // production, which is the worst possible place to find it.
        let events = gen::events(0, 300, 25, 5);

        let (mut a, pa) = store("order-a");
        a.append_batch(&events).unwrap();
        a.fold_from(0).unwrap();

        let (mut b, pb) = store("order-b");
        for chunk in events.chunks(13) {
            b.append_batch(chunk).unwrap();
            b.fold_from(chunk[0].generation).unwrap();
        }

        assert_eq!(
            a.projection_digest().unwrap(),
            b.projection_digest().unwrap()
        );
        let _ = std::fs::remove_file(pa);
        let _ = std::fs::remove_file(pb);
    }

    #[test]
    fn projection_digest_is_sensitive_to_a_single_row() {
        let (mut s, path) = store("digest-sensitive");
        s.append_batch(&gen::events(0, 30, 6, 2)).unwrap();
        s.fold_from(0).unwrap();
        let before = s.projection_digest().unwrap();
        s.conn
            .execute(
                "UPDATE entities_projection SET place_ref = 'moved' WHERE rowid = 1",
                [],
            )
            .unwrap();
        assert_ne!(before, s.projection_digest().unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_future_schema_is_refused_rather_than_opened() {
        let path = temp("future");
        {
            let s = Store::open(&path, Durability::Normal).unwrap();
            s.conn.execute_batch("PRAGMA user_version=99;").unwrap();
        }
        match Store::open(&path, Durability::Normal) {
            Err(OpenError::FutureSchema { found, known }) => {
                assert_eq!((found, known), (99, KNOWN_SCHEMA_VERSION));
            }
            Err(other) => panic!("expected fail-closed refusal, got {other:?}"),
            Ok(_) => panic!("a v99 database was opened by a v1 binary — fail-closed did not hold"),
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_backfills_a_populated_store() {
        let path = temp("migrate");
        let mut s = Store::open(&path, Durability::Normal).unwrap();
        s.append_batch(&gen::events(0, 120, 10, 3)).unwrap();
        s.fold_from(0).unwrap();
        s.migrate_to_v2().unwrap();

        assert_eq!(s.schema_version().unwrap(), 2);
        let zero: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entities_projection WHERE observed_count = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            zero, 0,
            "backfill left rows at the default — the migration did nothing"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn checkpoint_round_trips_the_resume_point() {
        let (mut s, path) = store("checkpoint");
        s.append_batch(&gen::events(0, 64, 8, 1)).unwrap();
        s.fold_from(0).unwrap();
        let next = s.write_checkpoint("main").unwrap();
        assert_eq!(
            next, 64,
            "resume point is the generation AFTER the last folded event"
        );
        let (resume, digest) = s.read_checkpoint("main").unwrap().unwrap();
        assert_eq!(resume, 64);
        assert_eq!(digest, s.projection_digest().unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn schema_digest_changes_when_the_schema_does() {
        // The mechanism that stops a stand-in measurement being re-read as
        // evidence for a different schema.
        let a = sha256::hex(SCHEMA_V1.as_bytes());
        let b = sha256::hex(
            format!("{SCHEMA_V1}\nCREATE INDEX x ON world_events (source);").as_bytes(),
        );
        assert_ne!(a, b);
        assert_eq!(a, schema_digest());
    }

    #[test]
    fn durability_parses_and_round_trips() {
        for d in Durability::all() {
            assert_eq!(Durability::parse(d.pragma()), Some(d));
        }
        assert_eq!(Durability::parse("sometimes"), None);
    }

    #[test]
    fn the_synchronous_pragma_is_actually_applied() {
        // Asserting the setting reached SQLite, not just the format string:
        // the whole durability comparison is void if every run is NORMAL.
        for (d, expected) in [
            (Durability::Full, 2i64),
            (Durability::Normal, 1),
            (Durability::Off, 0),
        ] {
            let path = temp(&format!("pragma-{}", d.pragma()));
            let s = Store::open(&path, d).unwrap();
            let actual: i64 = s
                .conn
                .query_row("PRAGMA synchronous", [], |r| r.get(0))
                .unwrap();
            assert_eq!(actual, expected, "{d:?} did not take effect");
            let _ = std::fs::remove_file(path);
        }
    }

    // -----------------------------------------------------------------------
    // Migration cost-shape gate (ADR-0041 D-13)
    // -----------------------------------------------------------------------
    //
    // A migration whose backfill rescans the whole event log once per entity
    // costs O(events x entities). On the measured ladder that was ~3 100x the
    // grouped equivalent on identical data, and it is the kind of defect a
    // later "clearer" rewrite reintroduces silently, because the SQL reads
    // perfectly well and the result is correct. Only the plan is wrong.
    //
    // These tests gate the shape, not a wall-clock number: the plan assertion
    // is deterministic, and the timing check compares the two statements inside
    // one process so machine speed cancels out.

    /// Every `EXPLAIN QUERY PLAN` line for the backfill in `step`.
    fn backfill_plan(step: &str, path: &std::path::Path) -> Vec<String> {
        let backfill = step
            .split(';')
            .map(str::trim)
            .find(|s| s.starts_with("UPDATE"))
            .expect("the step must contain an UPDATE backfill");
        let conn = rusqlite::Connection::open(path).expect("open");
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {backfill}"))
            .expect("plan");
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        rows
    }

    fn migrated(name: &str, step: &str, events: u64, entities: u64) -> std::path::PathBuf {
        let (mut s, path) = store(name);
        s.append_batch(&gen::events(0, events, entities, 11))
            .unwrap();
        s.fold_from(0).unwrap();
        s.migrate_to_v2_using(step).expect("migrate");
        drop(s);
        path
    }

    #[test]
    fn the_backfill_must_not_rescan_the_log_per_entity() {
        // The gate. A correlated scalar subquery over the log is the shape that
        // made the measured migration quadratic; the grouped form must not have
        // one.
        let path = migrated("plan-grouped", SCHEMA_V2_STEP_GROUPED, 400, 40);
        let plan = backfill_plan(SCHEMA_V2_STEP_GROUPED, &path).join(" | ");
        assert!(
            !plan.to_uppercase().contains("CORRELATED"),
            "the grouped backfill regressed to a per-entity rescan of the log.\nplan: {plan}"
        );
        assert!(
            plan.to_uppercase().contains("GROUP BY") || plan.to_uppercase().contains("MATERIALIZE"),
            "the grouped backfill no longer makes a single grouped pass.\nplan: {plan}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_plan_gate_can_actually_tell_the_two_shapes_apart() {
        // Non-vacuity. Without this the assertion above would pass against a
        // plan-detection method that never fires, which is the failure mode
        // that makes a regression gate decorative.
        let path = migrated("plan-legacy", SCHEMA_V2_STEP, 400, 40);
        let plan = backfill_plan(SCHEMA_V2_STEP, &path).join(" | ");
        assert!(
            plan.to_uppercase().contains("CORRELATED"),
            "the legacy backfill is supposed to BE the bad shape; if it no longer \
             reads as correlated, this gate is no longer detecting anything.\nplan: {plan}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn both_backfills_compute_the_same_counts_including_unobserved_entities() {
        // Deterministic equivalence, which is what makes swapping the statement
        // legitimate rather than merely faster. The zero-observation case is
        // called out because the two forms reach it differently: the correlated
        // subquery COUNTs an empty set, the grouped form never matches and
        // leaves the column's DEFAULT.
        let read = |path: &std::path::Path| -> Vec<(String, i64)> {
            let conn = rusqlite::Connection::open(path).expect("open");
            let mut stmt = conn
                .prepare(
                    "SELECT entity_id, observed_count FROM entities_projection ORDER BY entity_id",
                )
                .expect("prepare");
            let v: Vec<(String, i64)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect();
            v
        };

        let a = migrated("equiv-legacy", SCHEMA_V2_STEP, 600, 50);
        let b = migrated("equiv-grouped", SCHEMA_V2_STEP_GROUPED, 600, 50);
        let (ra, rb) = (read(&a), read(&b));
        assert!(!ra.is_empty(), "no projection rows to compare");
        assert_eq!(ra, rb, "the two backfills disagree");

        // And prove the unobserved-entity path was actually exercised, rather
        // than assumed: insert an entity the log never mentions, re-run both
        // backfills against it, and require both to read 0.
        for (name, step) in [
            ("zero-legacy", SCHEMA_V2_STEP),
            ("zero-grouped", SCHEMA_V2_STEP_GROUPED),
        ] {
            let (mut s, path) = store(name);
            s.append_batch(&gen::events(0, 200, 20, 3)).unwrap();
            s.fold_from(0).unwrap();
            s.conn
                .execute(
                    "INSERT INTO entities_projection \
                       (entity_id, kind, place_ref, valid_from_ms, generation) \
                     VALUES ('never-observed', 'thing', 'nowhere', 0, 0)",
                    [],
                )
                .expect("insert unobserved entity");
            s.migrate_to_v2_using(step).expect("migrate");
            let n: i64 = s
                .conn
                .query_row(
                    "SELECT observed_count FROM entities_projection WHERE entity_id='never-observed'",
                    [],
                    |r| r.get(0),
                )
                .expect("read back");
            assert_eq!(n, 0, "{name}: an unobserved entity must count 0");
            drop(s);
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn the_grouped_backfill_is_flat_in_entities_where_the_legacy_one_is_not() {
        // Coarse by design. The legacy form doubles when the entity count
        // doubles at a fixed log size; the grouped form should barely move.
        // Both ratios are measured in one process against the same machine, so
        // this asserts a relationship rather than a wall-clock budget.
        let time_it = |name: &str, step: &str, entities: u64| -> std::time::Duration {
            let (mut s, path) = store(name);
            s.append_batch(&gen::events(0, 8_000, entities, 5)).unwrap();
            s.fold_from(0).unwrap();
            let t = std::time::Instant::now();
            s.migrate_to_v2_using(step).expect("migrate");
            let d = t.elapsed();
            drop(s);
            let _ = std::fs::remove_file(&path);
            d
        };

        let legacy_lo = time_it("scale-l-lo", SCHEMA_V2_STEP, 100);
        let legacy_hi = time_it("scale-l-hi", SCHEMA_V2_STEP, 400);
        let grouped_lo = time_it("scale-g-lo", SCHEMA_V2_STEP_GROUPED, 100);
        let grouped_hi = time_it("scale-g-hi", SCHEMA_V2_STEP_GROUPED, 400);

        let ratio = |lo: std::time::Duration, hi: std::time::Duration| -> f64 {
            hi.as_secs_f64() / lo.as_secs_f64().max(1e-6)
        };
        let (lr, gr) = (ratio(legacy_lo, legacy_hi), ratio(grouped_lo, grouped_hi));

        // 4x the entities. Linear-in-entities predicts ~4.0; flat predicts ~1.0.
        // The thresholds are wide because CI machines are noisy and this test is
        // worth more as a shape check that never flakes than as a tight bound.
        assert!(
            lr > 2.0,
            "the legacy backfill no longer grows with entity count (ratio {lr:.2}); \
             either the planner changed or this test has stopped measuring the \
             thing it was written for"
        );
        assert!(
            gr < 2.0,
            "the grouped backfill now grows with entity count (ratio {gr:.2}) — it has \
             regressed toward a per-entity rescan of the log"
        );
        assert!(
            gr < lr,
            "grouped ({gr:.2}) must scale better than legacy ({lr:.2})"
        );
    }
}
