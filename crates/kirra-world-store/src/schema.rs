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

/// The ruled schema.
///
/// Its SHA-256 is recorded in `world_store_meta` as `schema_digest` at
/// creation (see `crate::schema_digest`), alongside `schema_version` and
/// `chain_algorithm`. The digest is the one of the three that cannot be
/// claimed falsely: a version number records what someone said the schema was,
/// the digest records what it actually is.
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
///
/// Bumped to **2** by the trust-axes migration below.
pub const SCHEMA_VERSION: i64 = 2;

/// **v2 — the four orthogonal trust axes, added additively.**
///
/// `KIRRA-WM2-SCHEMA-001` ratified v1 with a recorded digest, so growing it is a
/// ruling rather than a refactor. The ruling taken on 2026-08-07 was **additive,
/// with `writer_class` kept permanently**, and the reason is worth stating
/// because it inverts an assumption that had been carried for a while:
///
/// # `writer_class` is not the origin axis in disguise
///
/// It looks like one. It is not, and neither derives the other:
///
/// * **`writer_class` records who held the pen** — `sensor`, `operator`,
///   `derivation`, `llm_candidate`. It is what **D-2** keys on, and
///   `llm_candidate` is not an origin at all: an LLM can propose a claim of any
///   origin, and the rule that constrains it is about the *writer's authority*,
///   not the claim's provenance.
/// * **`origin` records where the claim came from** — `observed`, `derived`,
///   `imported`, `asserted`. It carries `imported`, which no writer class
///   expresses, and it has no way to say "an LLM wrote this".
///
/// Replacing `writer_class` with `origin` would therefore have deleted D-2's
/// enforcement basis. It stays, permanently, as a first-class column.
///
/// # `claim_status` becomes derived, and the derivation is a `CHECK`
///
/// `claim_status` is the two-value adjudication proxy — `candidate` /
/// `confirmed` against the axis's four states, so `rejected` and `ambiguous`
/// were both unrepresentable. Rule 3 requires `Ambiguous` to be a stable,
/// reportable state (*"I have conflicting information about that"*), which the
/// proxy could not express.
///
/// It is retained for read compatibility, and its agreement with `adjudication`
/// is enforced rather than remembered: the `CHECK` below makes
/// `claim_status = 'confirmed'` hold exactly when `adjudication = 'confirmed'`.
/// A row that disagrees cannot be written or updated into existence.
///
/// # Why `validity` has no column
///
/// Because transition rule 6 says it is computed at read time, never stored.
/// Three axes are stored; the fourth is `trust::validity_at`. A column would be
/// the one place the rule could be broken, so there isn't one — the same shape
/// as `TrustAxes` itself, which holds three fields for four axes.
///
/// # Why `predicted` is absent from the origin vocabulary
///
/// Blueprint §20: it **never appears in the evidence store**.
/// `TrustAxes::new` refuses it. The `CHECK` refuses it too, so the rule holds
/// against raw SQL and not only against callers who went through the
/// constructor.
///
/// # Enforced, not merely declared
///
/// Every constraint here — including the cross-column ones — was verified to
/// fire on both `INSERT` and `UPDATE`. SQLite's `ALTER TABLE ADD COLUMN`
/// accepts a `CHECK` that references other columns and enforces it, which is
/// what makes an additive migration able to carry the same weight as the
/// original table-level `CHECK`s rather than degrading to writer-side
/// validation. That mattered: `KIRRA-WM2-SCHEMA-001` §8 rejects
/// "the caller promises not to" as a substitute for a constraint.
pub const SCHEMA_V2_MIGRATION: &str = r#"
ALTER TABLE world_events ADD COLUMN origin TEXT
    CHECK (origin IS NULL OR origin IN
        ('observed','derived','imported','asserted'));

ALTER TABLE world_events ADD COLUMN corroboration TEXT
    CHECK (corroboration IS NULL OR corroboration IN
        ('uncorroborated','corroborated','contradicted'));

ALTER TABLE world_events ADD COLUMN corroboration_n INTEGER
    CHECK (
        (corroboration_n IS NULL OR corroboration_n >= 1)
        AND (corroboration IS NULL
             OR (corroboration = 'uncorroborated') = (corroboration_n IS NULL))
    );

ALTER TABLE world_events ADD COLUMN adjudication TEXT
    CHECK (
        (adjudication IS NULL OR adjudication IN
            ('pending','confirmed','rejected','ambiguous'))

        -- The three stored axes travel together. A row carrying one axis and
        -- not the others is a partial trust record, and a reader cannot tell
        -- it from an unlabelled one.
        AND (adjudication IS NULL) = (origin IS NULL)
        AND (adjudication IS NULL) = (corroboration IS NULL)

        -- D-2, restated against the axis so it survives claim_status. An LLM
        -- may never write a confirmed fact; if claim_status is ever dropped,
        -- this is what keeps the rule.
        AND (adjudication IS NULL
             OR writer_class <> 'llm_candidate'
             OR adjudication <> 'confirmed')

        -- claim_status is DERIVED from adjudication. Enforced, so the proxy
        -- cannot drift from the axis it now stands for.
        AND (adjudication IS NULL
             OR (claim_status = 'confirmed') = (adjudication = 'confirmed'))
    );

CREATE INDEX idx_events_adjudication ON world_events (adjudication, generation);
"#;
