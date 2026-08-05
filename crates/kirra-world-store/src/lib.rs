//! **Kirra World storage — the append-only evidence log.**
//!
//! Implements `KIRRA-WM2-SCHEMA-001` (SD-1…SD-4, ruled 2026-08-05) over SQLite,
//! chained with [`kirra_audit_hash`]. Unblocked by ADR-0042 **Decision 5**
//! (*safety-related, non-authoritative*, recorded 2026-08-05), which released
//! the domain-logic gate.
//!
//! # What to call this, and what not to
//!
//! Canonical name: **Kirra World**. Accurate prose gloss: **evidence ledger** —
//! an append-only, hash-chained, bitemporal record of what was claimed and on
//! whose authority.
//!
//! **Not "the world model."** ADR-0042 Decision 1 ruled that off a measured
//! collision: the term already means *redundant perception channels* in
//! `kirra-trajectory`'s `perception_redundancy.rs` and in the ros2 adapter —
//! **both inside the safety closure** — and a TTL'd operator-facing read
//! projection in `robot/world_model.py`. The reason is safety communication:
//! *"the world model was wrong"* must not be able to mean a perception fault
//! and a knowledge fault at once. To an outside reader it also suggests a
//! learned predictive model, which this is not — nothing here predicts
//! anything. It records.
//!
//! # This crate is AHEAD of the domain core it adapts
//!
//! Deliberate, and worth knowing before it looks like a mistake in the other
//! direction: `kirra-world`, the domain core, is still unconstructible
//! placeholders while this adapter is a working implementation. ADR-0042
//! Decision 5 released the *storage* gate specifically and left the
//! domain-logic gate closed, so the store could proceed and the core could not.
//! See that crate's docs for the full reasoning.
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
//! Implemented: the schema, the write path, chain verification, the
//! current-state projection and its queries, and compaction-with-citation
//! including the per-key summaries a compacted window leaves behind.
//!
//! **Not** implemented: a retention *policy driver*, or any service.
//! [`WorldStore::compact_range`] enforces what compaction is allowed to do;
//! nothing yet decides which spans are past ADR-0041 OQ2's horizons and calls
//! it. `retention_class` is stored, constrained, and honoured by the protected
//! -class refusal — but the horizons themselves are still applied by hand.
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

pub mod compaction;
pub mod projection;
pub mod schema;

pub use compaction::{Citation, CompactionOutcome, DegradedSummary, Resolution, TemporalAnswer};
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
    /// A compaction window contained a protected-class event. §11.3 refuses
    /// such a window **whole** rather than compacting around it.
    ProtectedClassInRange {
        /// The offending generation.
        generation: i64,
        /// Its retention class.
        retention_class: String,
    },
    /// A compaction window contained an event that defines a projection's
    /// current claim. Removing it would make `rebuild_projections` stop
    /// reproducing the incremental state.
    ProjectionHeadInRange {
        /// The offending generation.
        generation: i64,
        /// The subject whose current claim it defines.
        subject: String,
    },
    /// Generations are missing with no citation explaining them. This is what
    /// stops a span being deleted quietly.
    UncitedGap {
        /// The first missing generation.
        generation: i64,
    },
    /// A citation exists but does not join the chain it claims to bridge.
    CitationBroken {
        /// The citation's first generation.
        lo_generation: i64,
        /// What did not line up.
        detail: String,
    },
    /// A compaction range was empty or inverted.
    EmptyRange {
        /// The requested low bound.
        lo: i64,
        /// The requested high bound.
        hi: i64,
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
            Self::ProtectedClassInRange {
                generation,
                retention_class,
            } => write!(
                f,
                "generation {generation} is retention_class={retention_class}, which is \
                 protected from compaction; the window is refused whole \
                 (ADR-0041 §11.3, OQ2)"
            ),
            Self::ProjectionHeadInRange {
                generation,
                subject,
            } => write!(
                f,
                "generation {generation} defines the current claim for `{subject}`; \
                 compacting it would make a projection rebuild diverge from the \
                 incremental fold"
            ),
            Self::UncitedGap { generation } => write!(
                f,
                "generation {generation} is missing with no compaction citation \
                 accounting for it"
            ),
            Self::CitationBroken {
                lo_generation,
                detail,
            } => write!(f, "citation at generation {lo_generation}: {detail}"),
            Self::EmptyRange { lo, hi } => {
                write!(f, "compaction range {lo}..={hi} is empty or inverted")
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

        // Compaction citations, keyed by the generation each span starts at.
        // Loaded up front so a hole can be crossed the moment it is reached,
        // rather than discovered as a missing row and reported as corruption.
        let citations = self.load_citations()?;
        let mut expected_generation: i64 = 1;
        // Which citations the walk actually consumed. Checked at the end, so
        // an unreachable one cannot sit unread.
        let mut visited: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();

        while let Some(r) = rows.next()? {
            let generation: i64 = r.get(0)?;

            // Cross any compacted spans standing between the last verified row
            // and this one. Each must be cited, and each citation must join the
            // chain where it claims to — that pairing is what makes a silent
            // deletion impossible: remove rows and the gap is uncited; forge a
            // citation and `chain_before` will not match what we computed.
            while expected_generation < generation {
                let c = citations.get(&expected_generation).ok_or({
                    StoreError::UncitedGap {
                        generation: expected_generation,
                    }
                })?;
                if c.chain_before != prev {
                    return Err(StoreError::CitationBroken {
                        lo_generation: c.lo_generation,
                        detail: format!(
                            "chain_before does not match the digest at generation {}",
                            expected_generation - 1
                        ),
                    });
                }
                visited.insert(c.lo_generation);
                prev = c.chain_after.clone();
                expected_generation = c.hi_generation + 1;
            }
            if expected_generation != generation {
                // A citation overshot into a surviving row: its span claims
                // generations that are still present.
                return Err(StoreError::CitationBroken {
                    lo_generation: expected_generation,
                    detail: format!(
                        "a citation covers generation {generation}, which is still present"
                    ),
                });
            }

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
            expected_generation = generation + 1;
        }

        // A span compacted off the END of the log leaves no following row to
        // trigger the crossing above, so it is followed here.
        while let Some(c) = citations.get(&expected_generation) {
            if c.chain_before != prev {
                return Err(StoreError::CitationBroken {
                    lo_generation: c.lo_generation,
                    detail: "trailing citation does not join the chain".to_string(),
                });
            }
            visited.insert(c.lo_generation);
            prev = c.chain_after.clone();
            expected_generation = c.hi_generation + 1;
        }

        // EVERY citation must have been walked. The loops above only follow
        // citations reachable by contiguity from generation 1, so one sitting
        // at an unreachable `lo_generation` — past the end of the log, or
        // behind a gap nothing accounts for — would otherwise be ignored and
        // the whole verification still return Ok.
        //
        // That is a hole in the property this design exists to provide: a
        // citation is an *admission* that a span was removed, so an admission
        // no verifier ever reads is not one. Refusing here means every
        // citation is either walked and checked, or the chain fails.
        if let Some(orphan) = citations.keys().find(|k| !visited.contains(k)) {
            return Err(StoreError::CitationBroken {
                lo_generation: *orphan,
                detail: "citation is unreachable from the verified sequence — it \
                         accounts for generations that were never reached"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Test-only: the digest recorded in the checkpoint, as committed.
    ///
    /// Exists so a test can assert the checkpoint and the state it describes
    /// were committed together, rather than trusting that they were.
    #[cfg(any(test, feature = "test-support"))]
    pub fn checkpoint_state_digest_for_test(&self) -> Result<Option<String>, StoreError> {
        if !self.has_projections()? {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT state_digest FROM projection_checkpoint WHERE name = ?1",
                params![projection::CURRENT_PROJECTION],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every citation, keyed by the generation its span starts at.
    fn load_citations(&self) -> Result<std::collections::BTreeMap<i64, Citation>, StoreError> {
        // Guards on the TABLE, not on [`WorldStore::has_citations`] — this only
        // needs to avoid querying a table that was never installed.
        if !self.has_table("compaction_citations")? {
            return Ok(std::collections::BTreeMap::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT lo_generation, hi_generation, event_count, range_digest,
                    chain_before, chain_after, compacted_at_ms
             FROM compaction_citations ORDER BY lo_generation ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Citation {
                lo_generation: r.get(0)?,
                hi_generation: r.get(1)?,
                event_count: r.get(2)?,
                range_digest: r.get(3)?,
                chain_before: r.get(4)?,
                chain_after: r.get(5)?,
                compacted_at_ms: r.get(6)?,
            })
        })?;
        let mut out = std::collections::BTreeMap::new();
        for c in rows {
            let c = c?;
            out.insert(c.lo_generation, c);
        }
        Ok(out)
    }

    /// Whether a table exists. Used for the lazily-installed tables, whose
    /// absence is the normal state of a store that has never folded or
    /// compacted rather than an error.
    fn has_table(&self, name: &str) -> Result<bool, StoreError> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        Ok(n.is_some())
    }

    /// Whether any compaction has actually run against this store.
    ///
    /// Deliberately a question about **recorded citations**, not about the
    /// table. [`WorldStore::compact_range`] installs the DDL before it runs its
    /// refusals, so a store that only ever *attempted* a compaction and was
    /// refused has the table and no rows — and answering "yes, a compaction has
    /// run" there would be false for the one caller that matters: a reader
    /// deciding whether an answer can be trusted as complete.
    pub fn has_citations(&self) -> Result<bool, StoreError> {
        if !self.has_table("compaction_citations")? {
            return Ok(false);
        }
        let any: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM compaction_citations)",
            [],
            |r| r.get(0),
        )?;
        Ok(any != 0)
    }

    /// Whether this store's compactions retain per-key summaries at all.
    ///
    /// A question about the **table**, unlike [`WorldStore::has_citations`],
    /// and the difference is load-bearing. This asks "does summary retention
    /// apply to this store" — false only for one compacted by a build that
    /// predates the feature, which
    /// [`WorldStore::resolution_for`] then treats pessimistically. An *empty*
    /// summaries table is a different thing entirely: it means the compacted
    /// window held nothing summarizable (only candidates, say), which is an
    /// answer, not a missing feature.
    pub fn has_summaries(&self) -> Result<bool, StoreError> {
        self.has_table("compaction_summaries")
    }

    /// Every recorded compaction, oldest span first.
    pub fn citations(&self) -> Result<Vec<Citation>, StoreError> {
        Ok(self.load_citations()?.into_values().collect())
    }

    /// Test-only: insert a citation for a span that was never removed.
    ///
    /// Lets a test plant an **unreachable** citation — one the verification
    /// walk never arrives at — which is the case that must be refused rather
    /// than quietly ignored.
    #[cfg(any(test, feature = "test-support"))]
    pub fn forge_citation_for_test(
        &self,
        lo_generation: i64,
        hi_generation: i64,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute_batch(compaction::COMPACTION_V1)?;
        self.conn.execute(
            "INSERT INTO compaction_citations (
                lo_generation, hi_generation, event_count, range_digest,
                chain_before, chain_after, compacted_at_ms
             ) VALUES (?1,?2,1,'forged','forged','forged',?3)",
            params![lo_generation, hi_generation, now_ms],
        )?;
        Ok(())
    }

    /// Test-only: remove a citation, leaving its gap unexplained.
    ///
    /// This is how a test proves the chain refuses an **uncited** gap — the
    /// property that stops compaction being a licence to delete evidence. A
    /// test that only compacted correctly could never show it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn delete_citation_for_test(&self, lo_generation: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM compaction_citations WHERE lo_generation = ?1",
            params![lo_generation],
        )?;
        Ok(())
    }

    /// Test-only: drop the summaries table, leaving citations behind.
    ///
    /// Reproduces a store compacted by a build that predates summary retention.
    /// That state is only reachable by *being* such a store, so without this a
    /// test could not show what the read path does with one — and the answer
    /// (degraded with nothing to show, never full) is the whole reason the
    /// upgrade path is handled explicitly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn drop_summaries_for_test(&self) -> Result<(), StoreError> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS compaction_summaries")?;
        Ok(())
    }

    /// Test-only: rewrite one column of a citation, to forge one.
    ///
    /// Allowlisted rather than interpolated, for the same reason
    /// [`WorldStore::tamper_for_test`] is: the `test-support` feature makes
    /// this public API.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tamper_citation_for_test(
        &self,
        lo_generation: i64,
        column: &str,
        value: &str,
    ) -> Result<(), StoreError> {
        const TAMPERABLE: &[&str] = &[
            "chain_before",
            "chain_after",
            "range_digest",
            "event_count",
            "hi_generation",
        ];
        if !TAMPERABLE.contains(&column) {
            return Err(StoreError::CitationBroken {
                lo_generation,
                detail: format!("`{column}` is not a tamperable column"),
            });
        }
        let sql = format!("UPDATE compaction_citations SET {column} = ?1 WHERE lo_generation = ?2");
        self.conn.execute(&sql, params![value, lo_generation])?;
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

/// The projected-state digest, over any connection.
///
/// Free-standing so the fold can compute it **inside** its own transaction
/// (a `Transaction` derefs to `Connection`), rather than after commit.
fn state_digest_of(conn: &Connection) -> Result<String, StoreError> {
    let mut stmt = conn.prepare(&format!(
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
        // The digest is computed and stored INSIDE the transaction. Doing it
        // after `commit` would mean a failure here returned `Err` from a fold
        // whose rows and checkpoint were already durable — the caller told the
        // fold did not happen, the store disagreeing. That is exactly the
        // all-or-nothing property this transaction exists to provide, so it
        // has to cover the digest too.
        let digest = state_digest_of(&tx)?;
        tx.execute(
            "UPDATE projection_checkpoint SET state_digest = ?1 WHERE name = ?2",
            params![digest, projection::CURRENT_PROJECTION],
        )?;
        tx.commit()?;
        Ok(head)
    }

    /// How many rows the projection holds, unfiltered by time.
    ///
    /// Deliberately NOT `current_all(..).len()`: that applies `holds_at`, so a
    /// bounded claim whose `valid_to_ms` has passed would be excluded from the
    /// count while still occupying a row and its bytes. For a size question the
    /// table is the answer; for a state question `current_all` is.
    pub fn projected_row_count(&self) -> Result<i64, StoreError> {
        if !self.has_projections()? {
            return Ok(0);
        }
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM world_current", [], |r| r.get(0))?)
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
        state_digest_of(&self.conn)
    }

    /// Every confirmed claim currently held about `subject` at `now_ms`.
    ///
    /// `now_ms` is a parameter rather than a clock read: a query whose answer
    /// depends on an ambient clock cannot be replayed, and incident
    /// reconstruction is the reason this store exists.
    ///
    /// # Why this one returns claims and not a [`TemporalAnswer`]
    ///
    /// Because **compaction can never degrade it**, and that is a proved
    /// property rather than an optimistic one: [`WorldStore::compact_range`]
    /// refuses any window containing a projection head, and the current claim
    /// for a key *is* a projection head. So the events this method reads are
    /// exactly the events compaction is forbidden to remove. Returning a
    /// resolution that is structurally always `Full` would be noise, and worse,
    /// it would suggest the check is doing something.
    ///
    /// The head refusal was adopted to keep rebuild-equals-incremental true.
    /// This is the second thing it buys, and it is worth naming: **current
    /// state survives retention**, so a device that has compacted its way back
    /// under budget still knows where everything is.
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
    ///
    /// Returns a [`TemporalAnswer`] rather than the claims directly, because
    /// this is the query compaction can silently truncate: the log is where the
    /// removed events used to be. If a compacted span could have borne on the
    /// answer, [`TemporalAnswer::resolution`] is [`Resolution::Degraded`] and
    /// carries the summaries retained in their place.
    pub fn as_of(
        &self,
        subject: &str,
        valid_at_ms: i64,
        as_known_at_ms: i64,
    ) -> Result<TemporalAnswer, StoreError> {
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
        Ok(TemporalAnswer {
            claims: projection::fold_all(candidates).into_values().collect(),
            resolution: self.resolution_for(subject, Some(valid_at_ms), Some(as_known_at_ms))?,
        })
    }

    /// Every confirmed event about `subject`, oldest first — the audit view.
    ///
    /// The query most exposed to compaction, and therefore the one where a
    /// bare `Vec` would be most misleading: history is *precisely* the
    /// trajectory compaction removes. No time bounds are applied to the
    /// resolution, because none are applied to the query — any compacted event
    /// about this subject is one this answer is missing.
    pub fn history(&self, subject: &str) -> Result<TemporalAnswer, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_events
             WHERE subject = ?1 AND claim_status = 'confirmed'
             ORDER BY generation ASC"
        ))?;
        let rows = stmt.query_map(params![subject], claim_from_row)?;
        Ok(TemporalAnswer {
            claims: rows.collect::<rusqlite::Result<Vec<_>>>()?,
            resolution: self.resolution_for(subject, None, None)?,
        })
    }

    /// Proposed-but-unconfirmed claims about `subject`.
    ///
    /// Deliberately a **separate method** rather than a flag on
    /// [`WorldStore::current`]. ADR-0040 lets an LLM write only candidates; if
    /// a caller could opt into mixing them with confirmed facts through a
    /// boolean, the safe reading would stop being the default one and SD-2's
    /// guarantee would end at the API boundary. Asking for candidates means
    /// naming them.
    ///
    /// **Known gap, recorded rather than hidden:** compaction removes candidate
    /// events with the rest of a window, and no summary is retained for them
    /// (see [`DegradedSummary`] — a candidate may not stand in for deleted
    /// evidence). So this returns a plain `Vec` and carries no resolution: an
    /// LLM's superseded proposals can vanish from a compacted window without
    /// this saying so. That is deliberate — a proposal is not evidence — but it
    /// means this method must not be used to reconstruct what was proposed
    /// during an incident window that has since been compacted.
    pub fn candidates(&self, subject: &str) -> Result<Vec<ProjectedClaim>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLAIM_COLUMNS} FROM world_events
             WHERE subject = ?1 AND claim_status = 'candidate'
             ORDER BY generation ASC"
        ))?;
        let rows = stmt.query_map(params![subject], claim_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The largest `lo..=n` that [`WorldStore::compact_range`] would accept,
    /// or `None` if nothing in the window is compactable.
    ///
    /// Both refusals name their first blocking generation, so a caller can
    /// always retry on the sub-range below it. This makes that a **designed**
    /// recovery rather than an accident of the error type: a refusal is a
    /// redirect, not a dead end.
    ///
    /// Note the asymmetry, which is deliberate. The two refusals are refusals
    /// of different kinds:
    ///
    /// * **Protected classes** are a *policy* unit. §11.3 refuses such a window
    ///   whole, and this helper honours that by stopping at the protected
    ///   event rather than stepping over it — compacting around it would make
    ///   how much of a span survives a question about interleaving.
    /// * **Projection heads** are a *correctness* constraint. Nothing says a
    ///   head's neighbours must survive, only the head itself.
    ///
    /// So for heads, stopping short is a conservative simplification rather
    /// than a requirement. If measurement ever shows head refusals blocking a
    /// material fraction of reclaimable space, the escalation is to compact
    /// *around* them — the citation model already handles disjoint spans, so
    /// it is a loop over sub-ranges, not a redesign. Recorded here and in
    /// ADR-0041 §11.3 so that is a pre-agreed trigger, not a later argument.
    pub fn largest_compactable_prefix(&self, lo: i64, hi: i64) -> Result<Option<i64>, StoreError> {
        if lo < 1 || hi < lo {
            return Ok(None);
        }
        let mut limit = hi;

        let protected: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(generation) FROM world_events
                 WHERE generation BETWEEN ?1 AND ?2 AND retention_class <> 'raw'",
                params![lo, hi],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(g) = protected {
            limit = limit.min(g - 1);
        }

        if self.has_projections()? {
            let head: Option<i64> = self
                .conn
                .query_row(
                    "SELECT MIN(e.generation) FROM world_events e
                     JOIN world_current c ON c.generation = e.generation
                     WHERE e.generation BETWEEN ?1 AND ?2",
                    params![lo, hi],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if let Some(g) = head {
                limit = limit.min(g - 1);
            }
        }

        if limit < lo {
            return Ok(None);
        }
        // A prefix with no rows in it is not compactable either — otherwise
        // this would hand back a range `compact_range` refuses as empty.
        let present: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM world_events WHERE generation BETWEEN ?1 AND ?2",
            params![lo, limit],
            |r| r.get(0),
        )?;
        Ok(if present > 0 { Some(limit) } else { None })
    }

    /// Compact `lo..=hi` out of the log, leaving a citation in its place.
    ///
    /// Refuses, whole, if the window contains:
    ///
    /// * a **protected-class** event (§11.3, OQ2's 365-day classes), or
    /// * an event that is a **projection head** — removing it would make
    ///   [`WorldStore::rebuild_projections`] stop reproducing the incremental
    ///   fold, quietly falsifying the property ADR-0041 names.
    ///
    /// Both refusals are all-or-nothing. Compacting "around" the offending
    /// events would make how much of a span survives a question about arrival
    /// order rather than about policy.
    ///
    /// This does **not** reclaim disk. Deleted pages return to SQLite's free
    /// list; D-3 measured what an actual `VACUUM` costs (a ~1× free-space
    /// reserve and a total write blackout), so it is the caller's decision and
    /// not a side effect of retention.
    pub fn compact_range(
        &mut self,
        lo: i64,
        hi: i64,
        now_ms: i64,
    ) -> Result<CompactionOutcome, StoreError> {
        if lo < 1 || hi < lo {
            return Err(StoreError::EmptyRange { lo, hi });
        }
        self.conn.execute_batch(compaction::COMPACTION_V1)?;

        // --- refusal 1: protected classes ---------------------------------
        let protected: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT generation, retention_class FROM world_events
                 WHERE generation BETWEEN ?1 AND ?2 AND retention_class <> 'raw'
                 ORDER BY generation ASC LIMIT 1",
                params![lo, hi],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((generation, retention_class)) = protected {
            return Err(StoreError::ProtectedClassInRange {
                generation,
                retention_class,
            });
        }

        // --- refusal 2: projection heads ----------------------------------
        if self.has_projections()? {
            let head: Option<(i64, String)> = self
                .conn
                .query_row(
                    "SELECT e.generation, e.subject FROM world_events e
                     JOIN world_current c ON c.generation = e.generation
                     WHERE e.generation BETWEEN ?1 AND ?2
                     ORDER BY e.generation ASC LIMIT 1",
                    params![lo, hi],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((generation, subject)) = head {
                return Err(StoreError::ProjectionHeadInRange {
                    generation,
                    subject,
                });
            }
        }

        // --- the span itself ----------------------------------------------
        let (event_count, range_digest, chain_after) = self.summarize_range(lo, hi)?;
        if event_count == 0 {
            return Err(StoreError::EmptyRange { lo, hi });
        }

        // `chain_before` is the digest the verifier will have computed when it
        // arrives at the hole: the last surviving row below `lo`, or GENESIS.
        // Taking the row below `lo` rather than `lo - 1` is deliberate — an
        // earlier compaction may already have removed `lo - 1`.
        let surviving_below: Option<String> = self
            .conn
            .query_row(
                "SELECT chain_digest FROM world_events
                 WHERE generation < ?1 ORDER BY generation DESC LIMIT 1",
                params![lo],
                |r| r.get(0),
            )
            .optional()?;
        let chain_before: String = match surviving_below {
            Some(d) => d,
            None => {
                // Nothing survives below the hole. If an earlier compaction
                // already covers that region, its `chain_after` is where the
                // chain stands; otherwise we are at the head of the log.
                //
                // Note the `?`: a failure to read the citations must surface,
                // not silently fall through to GENESIS and mint a citation
                // that claims the chain starts here.
                self.load_citations()?
                    .values()
                    .rfind(|c| c.hi_generation < lo)
                    .map_or_else(|| GENESIS.to_string(), |c| c.chain_after.clone())
            }
        };

        // Built BEFORE the delete, and committed in the same transaction as
        // it. A summary written afterwards could be lost to a crash between
        // the two, leaving a citation admitting a hole with nothing standing
        // in for it — which is the failure §11.3 is about, arrived at by
        // accident instead of by design.
        let summaries = self.build_summaries(lo, hi)?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM world_events WHERE generation BETWEEN ?1 AND ?2",
            params![lo, hi],
        )?;
        for s in &summaries {
            tx.execute(
                "INSERT INTO compaction_summaries (
                    lo_generation, subject, predicate_key, predicate, event_count,
                    first_valid_from_ms, last_valid_from_ms, first_txn_time_ms,
                    last_object, last_payload
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    s.lo_generation,
                    s.subject,
                    s.predicate.clone().unwrap_or_default(),
                    s.predicate,
                    s.event_count,
                    s.first_valid_from_ms,
                    s.last_valid_from_ms,
                    s.first_txn_time_ms,
                    s.last_object,
                    s.last_payload,
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO compaction_citations (
                lo_generation, hi_generation, event_count, range_digest,
                chain_before, chain_after, compacted_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                lo,
                hi,
                event_count,
                range_digest,
                chain_before,
                chain_after,
                now_ms
            ],
        )?;
        tx.commit()?;

        Ok(CompactionOutcome {
            citation: Citation {
                lo_generation: lo,
                hi_generation: hi,
                event_count,
                range_digest,
                chain_before,
                chain_after,
                compacted_at_ms: now_ms,
            },
            removed: event_count,
            summaries,
        })
    }

    /// The per-key summaries of a span, taken before it is removed.
    ///
    /// Streams the window in generation order and keeps one accumulator per
    /// `(subject, predicate)`. That is the same cost argument D-21 measured for
    /// the projection — 4 886 keys for 100 000 events on the reference stream —
    /// so memory here is bounded by the **entity** count, not by the window
    /// length. It has to be: [`WorldStore::summarize_range`] documents why
    /// buffering an 8.3 M-event `raw` window is an OOM triggered by the very
    /// operation you run because the device is full, and building summaries by
    /// collecting the rows first would reintroduce exactly that.
    ///
    /// Confirmed-only, and "latest" is by `(valid_from_ms, generation)` — see
    /// [`DegradedSummary`] for both.
    fn build_summaries(&self, lo: i64, hi: i64) -> Result<Vec<DegradedSummary>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT subject, predicate, object, payload, valid_from_ms, txn_time_ms, generation
             FROM world_events
             WHERE generation BETWEEN ?1 AND ?2 AND claim_status = 'confirmed'
             ORDER BY generation ASC",
        )?;
        let mut rows = stmt.query(params![lo, hi])?;

        // BTreeMap, not HashMap: the retained set is evidence, and its order
        // should not depend on hash seeding.
        let mut acc: std::collections::BTreeMap<(String, String), (DegradedSummary, (i64, i64))> =
            std::collections::BTreeMap::new();

        while let Some(r) = rows.next()? {
            let subject: String = r.get(0)?;
            let predicate: Option<String> = r.get(1)?;
            let object: Option<String> = r.get(2)?;
            let payload: String = r.get(3)?;
            let valid_from_ms: i64 = r.get(4)?;
            let txn_time_ms: i64 = r.get(5)?;
            let generation: i64 = r.get(6)?;

            let key = (subject.clone(), predicate.clone().unwrap_or_default());
            match acc.get_mut(&key) {
                Some((s, best)) => {
                    s.event_count += 1;
                    s.first_valid_from_ms = s.first_valid_from_ms.min(valid_from_ms);
                    s.first_txn_time_ms = s.first_txn_time_ms.min(txn_time_ms);
                    if (valid_from_ms, generation) > *best {
                        *best = (valid_from_ms, generation);
                        s.last_valid_from_ms = valid_from_ms;
                        s.last_object = object;
                        s.last_payload = payload;
                    }
                }
                None => {
                    acc.insert(
                        key,
                        (
                            DegradedSummary {
                                lo_generation: lo,
                                subject,
                                predicate,
                                event_count: 1,
                                first_valid_from_ms: valid_from_ms,
                                last_valid_from_ms: valid_from_ms,
                                first_txn_time_ms: txn_time_ms,
                                last_object: object,
                                last_payload: payload,
                            },
                            (valid_from_ms, generation),
                        ),
                    );
                }
            }
        }
        Ok(acc.into_values().map(|(s, _)| s).collect())
    }

    /// Every retained summary, oldest span first.
    pub fn summaries(&self) -> Result<Vec<DegradedSummary>, StoreError> {
        self.load_summaries(None)
    }

    /// Retained summaries, optionally narrowed to one subject.
    fn load_summaries(&self, subject: Option<&str>) -> Result<Vec<DegradedSummary>, StoreError> {
        if !self.has_summaries()? {
            return Ok(Vec::new());
        }
        let sql = "SELECT lo_generation, subject, predicate, event_count,
                          first_valid_from_ms, last_valid_from_ms, first_txn_time_ms,
                          last_object, last_payload
                   FROM compaction_summaries
                   WHERE (?1 IS NULL OR subject = ?1)
                   ORDER BY lo_generation ASC, subject ASC, predicate_key ASC";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![subject], |r| {
            Ok(DegradedSummary {
                lo_generation: r.get(0)?,
                subject: r.get(1)?,
                predicate: r.get(2)?,
                event_count: r.get(3)?,
                first_valid_from_ms: r.get(4)?,
                last_valid_from_ms: r.get(5)?,
                first_txn_time_ms: r.get(6)?,
                last_object: r.get(7)?,
                last_payload: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Whether a query about `subject` is answering across a compacted span,
    /// and what stands in for it.
    ///
    /// The bitemporal bounds are **optional filters**, and each one is a
    /// *necessary* condition rather than an exact one:
    ///
    /// * A removed event can only have appeared in an `as_of(valid_at)` answer
    ///   if its `valid_from_ms <= valid_at` — [`ProjectedClaim::holds_at`] says
    ///   so. If the window's earliest valid time is already after `valid_at`,
    ///   no event in it could have contributed, and the answer is genuinely
    ///   full.
    /// * Likewise on the transaction axis for `as_known_at`.
    ///
    /// Both bounds are minima over the whole key, so passing them does not
    /// prove a qualifying event existed — only failing them proves none did.
    /// The error therefore falls on the reporting-degraded side, which is the
    /// one that costs a caveat rather than hiding a deletion.
    ///
    /// **The upgrade path is handled explicitly.** A store compacted before
    /// summaries were retained has citations and no summaries table; that is
    /// reported as degraded with an empty summary set, never as full. Inferring
    /// "no summary, so nothing was lost" would turn a missing feature into a
    /// silent rewrite of history.
    ///
    /// **A summary with no citation is an error, not a dropped span.** The two
    /// are halves of one record — the summary says what was removed, the
    /// citation says from where and under which digest — and a `Degraded`
    /// answer carrying only the first cannot be traced back to the range it is
    /// degraded by. Compaction is defensible because the admission is
    /// *checkable*; publishing half of one is the fail-open version of saying
    /// so.
    fn resolution_for(
        &self,
        subject: &str,
        valid_at_ms: Option<i64>,
        as_known_at_ms: Option<i64>,
    ) -> Result<Resolution, StoreError> {
        // Guards on the TABLES, not on `has_citations()`. The row-counting
        // question is the wrong one here, and dangerously so: a store whose
        // citation was deleted out from under a retained summary has zero
        // citation rows, and short-circuiting to `Full` on that would report
        // the *most* damaged store as the healthiest one. The orphan check
        // below is what must see that case.
        let citations = self.load_citations()?;
        let summaries_retained = self.has_summaries()?;
        if citations.is_empty() && !summaries_retained {
            return Ok(Resolution::Full);
        }
        if !summaries_retained {
            return Ok(Resolution::Degraded {
                spans: citations.into_values().collect(),
                summaries: Vec::new(),
            });
        }

        let summaries: Vec<DegradedSummary> = self
            .load_summaries(Some(subject))?
            .into_iter()
            .filter(|s| valid_at_ms.is_none_or(|v| s.first_valid_from_ms <= v))
            .filter(|s| as_known_at_ms.is_none_or(|k| s.first_txn_time_ms <= k))
            .collect();
        if summaries.is_empty() {
            return Ok(Resolution::Full);
        }

        // Every summary must resolve to its citation, and a miss is an ERROR
        // rather than a dropped span. `filter_map` here would hand back a
        // `Degraded` answer whose spans are incomplete — degraded, but with no
        // range or digest to trace it to — and the whole reason compaction is
        // an admission rather than an erasure is that the admission is
        // *checkable*. A summary is one half of that record; silently
        // publishing it without the other half is the fail-open version of
        // saying so.
        //
        // The state is reachable: `verify_chain` refuses an uncited gap, but a
        // reader is not obliged to have verified the chain first, and this is
        // the path that reader is on.
        let mut spans: Vec<Citation> = Vec::with_capacity(summaries.len());
        for s in &summaries {
            let c = citations
                .get(&s.lo_generation)
                .ok_or_else(|| StoreError::CitationBroken {
                    lo_generation: s.lo_generation,
                    detail: "a retained summary names a compaction with no \
                             citation — the answer cannot be traced to the \
                             range and digest it is degraded by"
                        .to_string(),
                })?;
            if spans.last().map(|p: &Citation| p.lo_generation) != Some(c.lo_generation) {
                spans.push(c.clone());
            }
        }
        Ok(Resolution::Degraded { spans, summaries })
    }

    /// Count, content digest and final chain digest of a span, before removal.
    ///
    /// `range_digest` is taken over the canonical bytes of each event in
    /// generation order — the same bytes the chain covers. That is what keeps
    /// removed content *attestable*: it cannot be recovered, but anyone who
    /// later produces those events can be checked against this.
    ///
    /// The hash is **streamed**, not accumulated. Buffering the window's
    /// canonical JSON before hashing would be O(window) memory at exactly the
    /// moment memory is least available: a 30-day `raw` window at OQ2's ruled
    /// 3.20 /s is ~8.3 M events, which at ~600 B each is several GB of
    /// `String` — an OOM triggered by the operation you run *because* the
    /// device is full. Feeding `Sha256` per event keeps it bounded regardless
    /// of window size, and produces the identical digest.
    fn summarize_range(&self, lo: i64, hi: i64) -> Result<(i64, String, String), StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT generation, event_id, observation_id, txn_time_ms, valid_from_ms,
                    valid_to_ms, source, source_version, writer_class, claim_status,
                    provenance, frame_id, map_id, kind, subject, predicate, object,
                    payload, payload_schema, retention_class, chain_digest
             FROM world_events WHERE generation BETWEEN ?1 AND ?2
             ORDER BY generation ASC",
        )?;
        let mut rows = stmt.query(params![lo, hi])?;
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        let mut count = 0i64;
        let mut last_chain = String::new();

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
            last_chain = r.get(20)?;

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
            hasher.update(canonical_event_json(&rebuilt).as_bytes());
            hasher.update(b"\n");
            count += 1;
        }
        Ok((count, hex::encode(hasher.finalize()), last_chain))
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
