//! **Kirra World storage — the append-only evidence log.**
//!
//! Implements `KIRRA-WM2-SCHEMA-001` (SD-1…SD-4, ruled 2026-08-05) over SQLite,
//! chained with [`kirra_audit_hash`]. Unblocked by ADR-0042 **Decision 5**
//! (*safety-related, non-authoritative*, recorded 2026-08-05), which released
//! the domain-logic gate.
//!
//! # The boundary this crate must not cross
//!
//! Decision 5's classification holds only while Kirra World has **no authority
//! over actuation, release, safety decisions, or required safety inputs** —
//! condition (1) of *Conditions that reopen the decision*. Crossing it does not
//! merely break a convention: it reopens the ruling and re-closes the gate.
//! Nothing here is read by the checker, and nothing here may implement
//! `CorridorSource`. Gate test t24 enforces that by contents, because the
//! dependency would be inverted and the bidirectional fence would not see it.
//!
//! # What is implemented, and what deliberately is not
//!
//! Implemented: the schema, the write path, and chain verification.
//! **Not** implemented: projections, queries, compaction, retention
//! enforcement, or any service. `retention_class` is *stored and constrained*
//! here, but nothing yet applies ADR-0041 OQ2's durations to it.
//!
//! # Durability
//!
//! `synchronous=FULL`, universally, per ADR-0041 open question 1 (P-1, ruled
//! 2026-08-05). Tier C's five physical power cuts (D-11) ran at that setting,
//! so it is also the only configuration the closed durability gate covers.
//! Per-class differentiation is commit *grouping* (P-2/P-3) and belongs to the
//! writer, not here.
//!
//! # Bytes per event
//!
//! OQ2's retention horizons derive from ADR-0041 D-2's 458.51 B/event, measured
//! on the harness's *stand-in* schema. Every column this schema adds is
//! additive, so that figure is stale and the horizons are provisional until
//! re-measured against this table (`KIRRA-WM2-SCHEMA-001` §5, §8.4).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

pub mod projection;
pub mod schema;

pub use projection::{ProjectedClaim, CURRENT_PROJECTION};

// Re-exported so the dependency edge is real rather than declared-and-unused.
pub use kirra_world::{EntityId, ObservationId};
pub use schema::{CHAIN_ALGORITHM, SCHEMA_VERSION};

/// Who or what produced an observation.
///
/// ADR-0040 fixes the writer classes, and fixes that an LLM may create only a
/// candidate — never a confirmed fact. That rule is enforced by the schema
/// *and* carried inside the hashed bytes, so it cannot be edited back out of a
/// stored row without breaking the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriterClass {
    /// A sensor or perception producer.
    Sensor,
    /// A human operator.
    Operator,
    /// Computed from other recorded evidence.
    Derivation,
    /// An LLM. May only ever produce [`ClaimStatus::Candidate`].
    LlmCandidate,
}

impl WriterClass {
    /// The stored token. Stable — it is inside the hashed bytes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sensor => "sensor",
            Self::Operator => "operator",
            Self::Derivation => "derivation",
            Self::LlmCandidate => "llm_candidate",
        }
    }

    /// Parse a stored token. Unknown tokens map to the most restricted class so
    /// that a corrupt value can never be re-read as a more privileged one.
    #[must_use]
    pub fn from_stored(s: &str) -> Self {
        match s {
            "sensor" => Self::Sensor,
            "operator" => Self::Operator,
            "derivation" => Self::Derivation,
            _ => Self::LlmCandidate,
        }
    }
}

/// Whether a claim is asserted or merely proposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimStatus {
    /// Proposed. The only status a [`WriterClass::LlmCandidate`] may use.
    Candidate,
    /// Asserted.
    Confirmed,
}

impl ClaimStatus {
    /// The stored token. Stable — it is inside the hashed bytes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Confirmed => "confirmed",
        }
    }

    /// Parse a stored token. Anything but `confirmed` reads as the weaker
    /// claim, for the same reason as [`WriterClass::from_stored`].
    #[must_use]
    pub fn from_stored(s: &str) -> Self {
        if s == "confirmed" {
            Self::Confirmed
        } else {
            Self::Candidate
        }
    }
}

/// What the store refused, and why.
#[derive(Debug)]
pub enum StoreError {
    /// SQLite said no — including the schema `CHECK` violations, which is
    /// deliberate. See [`schema`].
    Sqlite(rusqlite::Error),
    /// SD-2, refused before the statement is built so the message names the
    /// rule rather than a constraint index.
    LlmCannotConfirm,
    /// SD-4, refused early for the same reason.
    SpatialClaimNeedsFrame,
    /// Recomputation diverged from a stored digest.
    ChainBroken {
        /// The generation at which it diverged.
        generation: i64,
    },
    /// A stored row could not be read back faithfully. Distinct from
    /// [`StoreError::ChainBroken`] on purpose: "this row is unreadable" and
    /// "this row was edited" are different diagnoses, and collapsing them
    /// costs an investigator the difference.
    CorruptRow {
        /// The generation whose row could not be read.
        generation: i64,
        /// What was wrong with it.
        detail: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
            Self::LlmCannotConfirm => write!(
                f,
                "writer_class=llm_candidate may not write claim_status=confirmed \
                 (ADR-0040; KIRRA-WM2-SCHEMA-001 SD-2)"
            ),
            Self::SpatialClaimNeedsFrame => write!(
                f,
                "a spatial claim requires frame_id (ADR-0042 Decision 2; \
                 KIRRA-WM2-SCHEMA-001 SD-4)"
            ),
            Self::ChainBroken { generation } => {
                write!(f, "chain broken at generation {generation}")
            }
            Self::CorruptRow { generation, detail } => {
                write!(f, "corrupt row at generation {generation}: {detail}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

/// One event to append.
///
/// `valid_to_ms` is **write-once** (SD-1): set it here when the observation is
/// inherently bounded, or leave it `None` and let a superseding event define
/// the end. No method in this crate updates it, because in an append-only log
/// there is no honest `UPDATE`.
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    /// Identity of this record.
    pub event_id: &'a str,
    /// Identity of the observation. Distinct from `event_id` so re-attribution
    /// does not rewrite the observation it re-attributes.
    pub observation_id: &'a str,
    /// When the store learned it.
    pub txn_time_ms: i64,
    /// When the fact began holding in the world.
    pub valid_from_ms: i64,
    /// When it stopped, if inherently bounded. Write-once.
    pub valid_to_ms: Option<i64>,
    /// Producer identity.
    pub source: &'a str,
    /// Producer version. With `source` and `kind`, carries the derivation
    /// method, which is why SD-3 needs no separate field for it.
    pub source_version: &'a str,
    /// See [`WriterClass`].
    pub writer_class: WriterClass,
    /// See [`ClaimStatus`].
    pub claim_status: ClaimStatus,
    /// Observation ids this claim rests on (SD-3).
    pub provenance: &'a [&'a str],
    /// Coordinate frame. Required when `kind == "spatial"` (SD-4).
    pub frame_id: Option<&'a str>,
    /// Map the claim is relative to.
    pub map_id: Option<&'a str>,
    /// Claim kind. `"spatial"` triggers the SD-4 constraint.
    pub kind: &'a str,
    /// Claim subject.
    pub subject: &'a str,
    /// Claim predicate.
    pub predicate: Option<&'a str>,
    /// Claim object.
    pub object: Option<&'a str>,
    /// Opaque body.
    pub payload: &'a str,
    /// Version of the payload's own schema.
    pub payload_schema: i64,
    /// One of the six retention classes ruled in ADR-0041 OQ2.
    pub retention_class: &'a str,
}

fn json_str(v: &str) -> String {
    serde_json::Value::String(v.to_string()).to_string()
}

/// The provenance column's stored form: a JSON array of observation ids (SD-3).
#[must_use]
pub fn provenance_json(ids: &[&str]) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(|p| json_str(p))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// The canonical JSON that goes into the chain hash.
///
/// **This is the load-bearing function for SD-2.** `writer_class` and
/// `claim_status` appear here, so they are covered by
/// [`kirra_audit_hash::compute_record_hash_v2`]: relabelling a stored row does
/// not fail a `CHECK`, it breaks the chain. Field order is fixed and explicit
/// rather than derived from a map, because a reordering would silently change
/// every digest ever computed.
#[must_use]
pub fn canonical_event_json(e: &NewEvent<'_>) -> String {
    fn opt(v: Option<&str>) -> String {
        v.map_or_else(|| "null".to_string(), json_str)
    }
    fn opt_i(v: Option<i64>) -> String {
        v.map_or_else(|| "null".to_string(), |x| x.to_string())
    }
    format!(
        concat!(
            "{{\"event_id\":{},\"observation_id\":{},\"txn_time_ms\":{},",
            "\"valid_from_ms\":{},\"valid_to_ms\":{},\"source\":{},",
            "\"source_version\":{},\"writer_class\":{},\"claim_status\":{},",
            "\"provenance\":{},\"frame_id\":{},\"map_id\":{},\"kind\":{},",
            "\"subject\":{},\"predicate\":{},\"object\":{},\"payload\":{},",
            "\"payload_schema\":{},\"retention_class\":{}}}"
        ),
        json_str(e.event_id),
        json_str(e.observation_id),
        e.txn_time_ms,
        e.valid_from_ms,
        opt_i(e.valid_to_ms),
        json_str(e.source),
        json_str(e.source_version),
        json_str(e.writer_class.as_str()),
        json_str(e.claim_status.as_str()),
        provenance_json(e.provenance),
        opt(e.frame_id),
        opt(e.map_id),
        json_str(e.kind),
        json_str(e.subject),
        opt(e.predicate),
        opt(e.object),
        json_str(e.payload),
        e.payload_schema,
        json_str(e.retention_class),
    )
}

/// `generation` as the chain's sequence number, failing CLOSED.
///
/// `unwrap_or(0)` here would hash an out-of-range generation as sequence 0,
/// silently producing a digest that is not the one the row's position implies.
/// In an integrity chain a silent substitution is worse than an error.
fn sequence_of(generation: i64) -> Result<u64, StoreError> {
    u64::try_from(generation).map_err(|_| StoreError::CorruptRow {
        generation,
        detail: "generation is negative or not representable as u64".to_string(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// SHA-256 of [`schema::SCHEMA_V1`], recorded in `world_store_meta` at
/// creation so a store can prove which exact schema text produced it — not
/// merely which version number someone claimed.
#[must_use]
pub fn schema_digest() -> String {
    sha256_hex(schema::SCHEMA_V1.as_bytes())
}

/// The genesis value the first event chains from.
pub const GENESIS: &str = "kirra-world:genesis";

/// The append-only evidence log.
pub struct WorldStore {
    conn: Connection,
}

impl WorldStore {
    /// Open or create a store at `path`.
    ///
    /// WAL with `synchronous=FULL` (ADR-0041 OQ1 P-1). Not configurable, on
    /// purpose: it is the configuration tier C's power-cut gate was closed
    /// under, and a per-class knob was ruled out because `fsync` flushes the
    /// shared WAL rather than a transaction.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;

        let installed: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='world_events'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if installed.is_none() {
            conn.execute_batch(schema::SCHEMA_V1)?;
            conn.execute(
                "INSERT INTO world_store_meta (key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
            conn.execute(
                "INSERT INTO world_store_meta (key, value) VALUES ('chain_algorithm', ?1)",
                params![CHAIN_ALGORITHM],
            )?;
            conn.execute(
                "INSERT INTO world_store_meta (key, value) VALUES ('schema_digest', ?1)",
                params![schema_digest()],
            )?;
        }
        Ok(Self { conn })
    }

    /// The chain digest of the newest event, or [`GENESIS`].
    pub fn head_chain(&self) -> Result<String, StoreError> {
        let head: Option<String> = self
            .conn
            .query_row(
                "SELECT chain_digest FROM world_events ORDER BY generation DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(head.unwrap_or_else(|| GENESIS.to_string()))
    }

    /// Number of events in the log.
    pub fn count(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM world_events", [], |r| r.get(0))?)
    }

    /// The next generation: `MAX(generation) + 1`.
    ///
    /// Deliberately not `COUNT(*) + 1`. COUNT is O(n), but the reason that
    /// matters less than the second one: COUNT assumes the sequence is
    /// CONTIGUOUS. ADR-0041 §11.3 plans compaction, which removes spans and
    /// leaves gaps — and a compaction citation still references the generations
    /// on either side of the hole. COUNT would hand out a generation that had
    /// already been used and cited. MAX is index-backed and gap-safe.
    fn next_generation(&self) -> Result<i64, StoreError> {
        let max: Option<i64> =
            self.conn
                .query_row("SELECT MAX(generation) FROM world_events", [], |r| r.get(0))?;
        Ok(max.unwrap_or(0) + 1)
    }

    /// Append one event, returning its generation.
    ///
    /// The two early refusals duplicate schema `CHECK`s deliberately: the
    /// `CHECK` is the guarantee, and these give the caller a message naming the
    /// rule. Removing them would not weaken the store; removing the `CHECK`s
    /// would.
    pub fn append(&mut self, e: &NewEvent<'_>) -> Result<i64, StoreError> {
        if e.writer_class == WriterClass::LlmCandidate && e.claim_status == ClaimStatus::Confirmed {
            return Err(StoreError::LlmCannotConfirm);
        }
        if e.kind == "spatial" && e.frame_id.is_none() {
            return Err(StoreError::SpatialClaimNeedsFrame);
        }

        let generation = self.next_generation()?;
        let prev = self.head_chain()?;
        let chain = kirra_audit_hash::compute_record_hash_v2(
            &prev,
            e.kind,
            &canonical_event_json(e),
            e.txn_time_ms,
            sequence_of(generation)?,
        );

        self.conn.execute(
            "INSERT INTO world_events (
                generation, event_id, observation_id, txn_time_ms, valid_from_ms,
                valid_to_ms, source, source_version, writer_class, claim_status,
                provenance, frame_id, map_id, kind, subject, predicate, object,
                payload, payload_schema, payload_digest, retention_class, chain_digest
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                generation,
                e.event_id,
                e.observation_id,
                e.txn_time_ms,
                e.valid_from_ms,
                e.valid_to_ms,
                e.source,
                e.source_version,
                e.writer_class.as_str(),
                e.claim_status.as_str(),
                provenance_json(e.provenance),
                e.frame_id,
                e.map_id,
                e.kind,
                e.subject,
                e.predicate,
                e.object,
                e.payload,
                e.payload_schema,
                sha256_hex(e.payload.as_bytes()),
                e.retention_class,
                chain,
            ],
        )?;
        Ok(generation)
    }

    /// Recompute the chain from genesis and compare against what is stored.
    ///
    /// This is what makes SD-2 structural: an edit to `writer_class` or
    /// `claim_status` in a stored row changes the canonical JSON, so
    /// recomputation diverges here. Rows are rehashed from their **stored**
    /// values, not from any caller input, so a tampered row is hashed as it now
    /// reads.
    pub fn verify_chain(&self) -> Result<(), StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT generation, event_id, observation_id, txn_time_ms, valid_from_ms,
                    valid_to_ms, source, source_version, writer_class, claim_status,
                    provenance, frame_id, map_id, kind, subject, predicate, object,
                    payload, payload_schema, retention_class, chain_digest
             FROM world_events ORDER BY generation ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut prev = GENESIS.to_string();

        while let Some(r) = rows.next()? {
            let generation: i64 = r.get(0)?;
            let event_id: String = r.get(1)?;
            let observation_id: String = r.get(2)?;
            let txn_time_ms: i64 = r.get(3)?;
            let source: String = r.get(6)?;
            let source_version: String = r.get(7)?;
            let writer_class: String = r.get(8)?;
            let claim_status: String = r.get(9)?;
            let provenance_raw: String = r.get(10)?;
            let frame_id: Option<String> = r.get(11)?;
            let map_id: Option<String> = r.get(12)?;
            let kind: String = r.get(13)?;
            let subject: String = r.get(14)?;
            let predicate: Option<String> = r.get(15)?;
            let object: Option<String> = r.get(16)?;
            let payload: String = r.get(17)?;
            let retention_class: String = r.get(19)?;
            let stored: String = r.get(20)?;

            // Fail CLOSED. `unwrap_or_default()` here would read corrupt JSON
            // as an empty list — and a row whose provenance was legitimately
            // empty at write time would then still verify, so corruption in
            // that case passed silently. That is fail-open in an integrity
            // check, which is the one place it is least acceptable.
            let provenance: Vec<String> =
                serde_json::from_str(&provenance_raw).map_err(|err| StoreError::CorruptRow {
                    generation,
                    detail: format!("provenance is not a JSON array of strings: {err}"),
                })?;
            let prov_refs: Vec<&str> = provenance.iter().map(String::as_str).collect();

            let rebuilt = NewEvent {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms,
                valid_from_ms: r.get(4)?,
                valid_to_ms: r.get(5)?,
                source: &source,
                source_version: &source_version,
                writer_class: WriterClass::from_stored(&writer_class),
                claim_status: ClaimStatus::from_stored(&claim_status),
                provenance: &prov_refs,
                frame_id: frame_id.as_deref(),
                map_id: map_id.as_deref(),
                kind: &kind,
                subject: &subject,
                predicate: predicate.as_deref(),
                object: object.as_deref(),
                payload: &payload,
                payload_schema: r.get(18)?,
                retention_class: &retention_class,
            };
            let expect = kirra_audit_hash::compute_record_hash_v2(
                &prev,
                &kind,
                &canonical_event_json(&rebuilt),
                txn_time_ms,
                sequence_of(generation)?,
            );
            if expect != stored {
                return Err(StoreError::ChainBroken { generation });
            }
            prev = stored;
        }
        Ok(())
    }

    /// Test-only: rewrite one column of one row, bypassing the write path.
    ///
    /// This exists so the tamper tests can prove the chain **detects** an edit.
    /// Without it a test could only show the writer refuses — the weaker claim,
    /// and precisely the one SD-2 rejected as conventional.
    /// `value: None` writes SQL NULL, which the SD-4 test needs in order to
    /// clear a frame and prove the `CHECK` — not the writer — refuses it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tamper_for_test(
        &self,
        generation: i64,
        column: &str,
        value: Option<&str>,
    ) -> Result<(), StoreError> {
        // Allowlisted rather than interpolated. This is test-only, but the
        // `test-support` feature makes it public API, and an interpolated
        // identifier is an injection point regardless of who is expected to
        // call it. It also turns a typo into a refusal instead of an UPDATE
        // that matches nothing and quietly passes.
        const TAMPERABLE: &[&str] = &[
            "event_id",
            "observation_id",
            "writer_class",
            "claim_status",
            "provenance",
            "frame_id",
            "map_id",
            "kind",
            "subject",
            "payload",
            "retention_class",
            "chain_digest",
        ];
        if !TAMPERABLE.contains(&column) {
            return Err(StoreError::CorruptRow {
                generation,
                detail: format!("`{column}` is not a tamperable column"),
            });
        }
        let sql = format!("UPDATE world_events SET {column} = ?1 WHERE generation = ?2");
        self.conn.execute(&sql, params![value, generation])?;
        Ok(())
    }
}

// ===========================================================================
// The read path
// ===========================================================================

/// Columns the fold reads, in one place so the two readers cannot drift.
const CLAIM_COLUMNS: &str = "subject, predicate, object, kind, payload, frame_id, map_id, \
     source, valid_from_ms, valid_to_ms, txn_time_ms, generation, event_id";

fn claim_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectedClaim> {
    Ok(ProjectedClaim {
        subject: r.get(0)?,
        predicate: r.get(1)?,
        object: r.get(2)?,
        kind: r.get(3)?,
        payload: r.get(4)?,
        frame_id: r.get(5)?,
        map_id: r.get(6)?,
        source: r.get(7)?,
        valid_from_ms: r.get(8)?,
        valid_to_ms: r.get(9)?,
        txn_time_ms: r.get(10)?,
        generation: r.get(11)?,
        event_id: r.get(12)?,
    })
}

impl WorldStore {
    /// Install the projection tables if absent.
    ///
    /// Called by the fold rather than by `open`, so a store that never projects
    /// stays byte-identical to one written before projections existed. See
    /// [`projection`] for why that matters to ADR-0041 D-20.
    fn ensure_projections(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(projection::PROJECTIONS_V1)?;
        Ok(())
    }

    /// Whether the projection tables have been installed.
    pub fn has_projections(&self) -> Result<bool, StoreError> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='world_current'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(n.is_some())
    }

    /// The generation the projection has consumed up to, or 0.
    pub fn projection_generation(&self) -> Result<i64, StoreError> {
        if !self.has_projections()? {
            return Ok(0);
        }
        let g: Option<i64> = self
            .conn
            .query_row(
                "SELECT generation FROM projection_checkpoint WHERE name = ?1",
                params![projection::CURRENT_PROJECTION],
                |r| r.get(0),
            )
            .optional()?;
        Ok(g.unwrap_or(0))
    }

    /// Fold new events into the current-state projection, incrementally.
    ///
    /// Returns the generation now consumed. **Only `confirmed` claims are
    /// folded** — see [`projection`] for why that is structural rather than a
    /// default. Candidates remain reachable via [`WorldStore::candidates`].
    ///
    /// The whole fold runs in one transaction: a projection caught halfway
    /// through a batch would disagree with its own checkpoint, and a reader
    /// cannot tell a partial fold from a complete one.
    pub fn fold(&mut self) -> Result<i64, StoreError> {
        self.ensure_projections()?;
        let from = self.projection_generation()?;
        self.fold_range(from)
    }

    /// Discard the projection and rebuild it from generation 0.
    ///
    /// The result must equal an incremental fold — that is ADR-0041's stated
    /// property, and [`WorldStore::projection_state_digest`] is how it is
    /// checked rather than assumed.
    pub fn rebuild_projections(&mut self) -> Result<i64, StoreError> {
        self.ensure_projections()?;
        self.conn.execute("DELETE FROM world_current", [])?;
        self.conn.execute("DELETE FROM projection_checkpoint", [])?;
        self.fold_range(0)
    }

    fn fold_range(&mut self, from_generation: i64) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;

        let mut head = from_generation;
        {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CLAIM_COLUMNS} FROM world_events
                 WHERE generation > ?1 AND claim_status = 'confirmed'
                 ORDER BY generation ASC"
            ))?;
            let mut rows = stmt.query(params![from_generation])?;

            let mut upsert = tx.prepare(
                "INSERT INTO world_current (
                    subject, predicate_key, predicate, object, kind, payload,
                    frame_id, map_id, source, valid_from_ms, valid_to_ms,
                    txn_time_ms, generation, event_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                 ON CONFLICT (subject, predicate_key) DO UPDATE SET
                    predicate=excluded.predicate, object=excluded.object,
                    kind=excluded.kind, payload=excluded.payload,
                    frame_id=excluded.frame_id, map_id=excluded.map_id,
                    source=excluded.source, valid_from_ms=excluded.valid_from_ms,
                    valid_to_ms=excluded.valid_to_ms, txn_time_ms=excluded.txn_time_ms,
                    generation=excluded.generation, event_id=excluded.event_id
                 WHERE (excluded.valid_from_ms, excluded.generation)
                     > (world_current.valid_from_ms, world_current.generation)",
            )?;

            while let Some(r) = rows.next()? {
                let c = claim_from_row(r)?;
                head = head.max(c.generation);
                let (_, pkey) = c.key();
                upsert.execute(params![
                    c.subject,
                    pkey,
                    c.predicate,
                    c.object,
                    c.kind,
                    c.payload,
                    c.frame_id,
                    c.map_id,
                    c.source,
                    c.valid_from_ms,
                    c.valid_to_ms,
                    c.txn_time_ms,
                    c.generation,
                    c.event_id,
                ])?;
            }
        }

        // The checkpoint must advance past every event CONSIDERED, not merely
        // every event adopted — a batch of candidates adopts nothing, and a
        // checkpoint that stayed put would re-scan them on every fold forever.
        let considered: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation), ?1) FROM world_events",
            params![from_generation],
            |r| r.get(0),
        )?;
        head = head.max(considered);

        tx.execute(
            "INSERT INTO projection_checkpoint (name, generation, state_digest)
             VALUES (?1, ?2, '')
             ON CONFLICT (name) DO UPDATE SET generation = excluded.generation",
            params![projection::CURRENT_PROJECTION, head],
        )?;
        tx.commit()?;

        let digest = self.projection_state_digest()?;
        self.conn.execute(
            "UPDATE projection_checkpoint SET state_digest = ?1 WHERE name = ?2",
            params![digest, projection::CURRENT_PROJECTION],
        )?;
        Ok(head)
    }

    /// A digest over the whole projected state, in key order.
    ///
    /// This is the instrument for ADR-0041's rebuild-equals-incremental
    /// property: two folds agree iff their digests agree, which is a single
    /// comparison rather than a row-by-row walk that could quietly skip a
    /// column.
    pub fn projection_state_digest(&self) -> Result<String, StoreError> {
        if !self.has_projections()? {
            return Ok(sha256_hex(b""));
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_current ORDER BY subject, predicate_key"
        ))?;
        let mut rows = stmt.query([])?;
        let mut acc = String::new();
        while let Some(r) = rows.next()? {
            let c = claim_from_row(r)?;
            acc.push_str(&format!("{c:?}\n"));
        }
        Ok(sha256_hex(acc.as_bytes()))
    }

    /// Every confirmed claim currently held about `subject` at `now_ms`.
    ///
    /// `now_ms` is a parameter rather than a clock read: a query whose answer
    /// depends on an ambient clock cannot be replayed, and incident
    /// reconstruction is the reason this store exists.
    pub fn current(&self, subject: &str, now_ms: i64) -> Result<Vec<ProjectedClaim>, StoreError> {
        if !self.has_projections()? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_current WHERE subject = ?1 ORDER BY predicate_key"
        ))?;
        let rows = stmt.query_map(params![subject], claim_from_row)?;
        let mut out = Vec::new();
        for c in rows {
            let c = c?;
            if c.holds_at(now_ms) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// The whole projected state at `now_ms`, in key order.
    pub fn current_all(&self, now_ms: i64) -> Result<Vec<ProjectedClaim>, StoreError> {
        if !self.has_projections()? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_current ORDER BY subject, predicate_key"
        ))?;
        let rows = stmt.query_map([], claim_from_row)?;
        let mut out = Vec::new();
        for c in rows {
            let c = c?;
            if c.holds_at(now_ms) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// **Bitemporal query.** What the store, as of transaction time
    /// `as_known_at_ms`, held to be true at valid time `valid_at_ms`.
    ///
    /// Answered from the LOG, not the projection — the projection is a cache of
    /// one point on the two-dimensional surface (latest known, latest valid),
    /// and every other point has to be replayed. Pretending otherwise is how a
    /// bitemporal store silently becomes a unitemporal one.
    ///
    /// Confirmed-only, for the same reason [`WorldStore::fold`] is.
    pub fn as_of(
        &self,
        subject: &str,
        valid_at_ms: i64,
        as_known_at_ms: i64,
    ) -> Result<Vec<ProjectedClaim>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_events
             WHERE subject = ?1 AND claim_status = 'confirmed'
               AND txn_time_ms <= ?2 AND valid_from_ms <= ?3
             ORDER BY generation ASC"
        ))?;
        let rows = stmt.query_map(
            params![subject, as_known_at_ms, valid_at_ms],
            claim_from_row,
        )?;
        let mut candidates = Vec::new();
        for c in rows {
            let c = c?;
            if c.holds_at(valid_at_ms) {
                candidates.push(c);
            }
        }
        Ok(projection::fold_all(candidates).into_values().collect())
    }

    /// Every confirmed event about `subject`, oldest first — the audit view.
    pub fn history(&self, subject: &str) -> Result<Vec<ProjectedClaim>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_events
             WHERE subject = ?1 AND claim_status = 'confirmed'
             ORDER BY generation ASC"
        ))?;
        let rows = stmt.query_map(params![subject], claim_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Proposed-but-unconfirmed claims about `subject`.
    ///
    /// Deliberately a **separate method** rather than a flag on
    /// [`WorldStore::current`]. ADR-0040 lets an LLM write only candidates; if
    /// a caller could opt into mixing them with confirmed facts through a
    /// boolean, the safe reading would stop being the default one and SD-2's
    /// guarantee would end at the API boundary. Asking for candidates means
    /// naming them.
    pub fn candidates(&self, subject: &str) -> Result<Vec<ProjectedClaim>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_events
             WHERE subject = ?1 AND claim_status = 'candidate'
             ORDER BY generation ASC"
        ))?;
        let rows = stmt.query_map(params![subject], claim_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Subjects whose state changed after transaction time `since_ms`.
    ///
    /// Transaction time, not valid time: "what have I not seen yet" is a
    /// question about the store's knowledge, and answering it in valid time
    /// would silently skip late-arriving events about the past — the exact
    /// traffic a bitemporal store exists to handle.
    pub fn changed_since(&self, since_ms: i64) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT subject FROM world_events
             WHERE txn_time_ms > ?1 AND claim_status = 'confirmed'
             ORDER BY subject ASC",
        )?;
        let rows = stmt.query_map(params![since_ms], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
