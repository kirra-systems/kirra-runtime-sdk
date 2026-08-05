//! The WM-2 event schema, as ruled.
//!
//! Every constraint below is here rather than in the write path on purpose.
//! `KIRRA-WM2-SCHEMA-001` §8 makes that explicit: the `CHECK`s are *schema*, not
//! validation the writer performs. A writer that forgets a rule is a bug; a
//! writer that cannot express a violation is a design.
//!
//! Two of the four rulings are enforced here and nowhere else:
//!
//! * **D-4** — `CHECK (kind <> 'spatial' OR frame_id IS NOT NULL)`. A spatial
//!   claim with no frame is the exact state ADR-0042 Decision 2 needs excluded,
//!   because a frameless spatial claim cannot later be shown *not* to have
//!   become checker geometry.
//! * **D-2** — `CHECK (writer_class <> 'llm_candidate' OR claim_status =
//!   'candidate')`. ADR-0040 fixes that an LLM may never write a confirmed
//!   fact. This makes the rule unrepresentable rather than merely unwritten.
//!
//! D-2's other half is not here and cannot be: the columns are also inside the
//! canonically-hashed bytes (see [`crate::canonical_event_json`]), so
//! relabelling a stored row does not fail a `CHECK` — it breaks the chain. The
//! `CHECK` stops the bad write; the hash stops the bad *edit*. Neither
//! substitutes for the other.
//!
//! **D-1** — `valid_to_ms` is write-once. There is no `UPDATE` anywhere in this
//! crate, so the rule holds structurally: the column is set at insert or stays
//! NULL, and a fact's end is derived from a superseding event. This is asserted
//! by test rather than by comment.

/// The ruled schema. Its digest is recorded in the store's metadata table so a
/// database can prove which schema it was written under.
pub const SCHEMA_V1: &str = r#"
CREATE TABLE world_events (
    generation      INTEGER PRIMARY KEY,
    event_id        TEXT    NOT NULL UNIQUE,
    observation_id  TEXT    NOT NULL,

    txn_time_ms     INTEGER NOT NULL,
    valid_from_ms   INTEGER NOT NULL,
    valid_to_ms     INTEGER,

    source          TEXT    NOT NULL,
    source_version  TEXT    NOT NULL,
    writer_class    TEXT    NOT NULL,
    claim_status    TEXT    NOT NULL,
    provenance      TEXT    NOT NULL,

    frame_id        TEXT,
    map_id          TEXT,

    kind            TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    predicate       TEXT,
    object          TEXT,

    payload         TEXT    NOT NULL,
    payload_schema  INTEGER NOT NULL,
    payload_digest  TEXT    NOT NULL,

    retention_class TEXT    NOT NULL DEFAULT 'raw',
    redacted        INTEGER NOT NULL DEFAULT 0,
    chain_digest    TEXT    NOT NULL,

    -- D-4. The Decision 2 boundary, at the storage layer.
    CHECK (kind <> 'spatial' OR frame_id IS NOT NULL),

    -- D-2. An LLM may never write a confirmed fact (ADR-0040).
    CHECK (writer_class <> 'llm_candidate' OR claim_status = 'candidate'),

    -- Closed vocabularies. An unknown value is a typo or an unreviewed class,
    -- and both should fail at the write rather than survive into the chain.
    CHECK (writer_class IN ('sensor','operator','derivation','llm_candidate')),
    CHECK (claim_status IN ('candidate','confirmed')),
    CHECK (retention_class IN
        ('raw','safety','incident','calibration','adjudication','operator'))
);

CREATE INDEX idx_events_subject_valid ON world_events (subject, valid_from_ms);
CREATE INDEX idx_events_txn           ON world_events (txn_time_ms);
CREATE INDEX idx_events_kind          ON world_events (kind, generation);
CREATE INDEX idx_events_observation   ON world_events (observation_id);

-- Not the event log: a single row recording what this database is, so a reader
-- can tell which schema and which chain algorithm produced it. ADR-0041 fixes
-- world_events as the only writable *evidence* table; this is metadata written
-- once at creation.
CREATE TABLE world_store_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Recorded in `world_store_meta` so a store written under the harness's
/// deliberately-different local SHA-256 can be told apart from one written
/// under the real primitive. There is no migration between them
/// (`KIRRA-WM2-SCHEMA-001` §4) and this is what makes that detectable rather
/// than a surprise.
pub const CHAIN_ALGORITHM: &str = "kirra-audit-hash/compute_record_hash_v2";

/// Schema version stamped into `world_store_meta`.
pub const SCHEMA_VERSION: i64 = 1;
