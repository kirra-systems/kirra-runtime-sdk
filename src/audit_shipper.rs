//! WORM / off-box audit shipping (WS-4 / Track 3 — Fleet Plane).
//!
//! The local audit ledger (`audit_log_chain`) is SHA-256 hash-chained and
//! Ed25519 anchor-signed, so it is tamper-EVIDENT — but it lives on the box. If
//! the box is lost (disk failure, seizure, a wipe), the evidence goes with it.
//! This module ships each new audit record to an append-only (Write-Once-
//! Read-Many) off-box sink and provides an INDEPENDENT re-verifier that re-checks
//! the hash chain on the shipped records alone — no source DB required.
//!
//! **The WORM guarantee.** [`verify_shipped_chain`] recomputes every record hash
//! from the record's own content (the same domain-separated construction the
//! store uses — [`AuditChainLinker::compute_record_hash_v2`]) and checks
//! (a) the recomputed hash matches, (b) each record's `previous_hash` links to
//! the prior record's hash, and (c) sequences are contiguous ascending. Any
//! single-field mutation breaks the hash; a dropped record breaks contiguity; a
//! reorder breaks linkage. So a verifier holding only the shipped stream can
//! prove the off-box copy is intact even after the origin is gone.
//!
//! This module is the shipping CORE (records + sinks + shipper + re-verifier);
//! the background scheduler that drives it on an interval is a thin follow-up.

use std::io::Write as _;

use serde::{Deserialize, Serialize};

use crate::audit_chain::AuditChainLinker;

/// One audit-chain record in its off-box shipped form — the raw chain fields
/// needed to INDEPENDENTLY re-verify the hash chain without the source database.
/// (Deliberately distinct from `AuditExportEntry`, which is the DESC-paginated
/// human/API export view and omits `sequence`/`hash_version`.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShippedAuditRecord {
    pub sequence: u64,
    pub event_type: String,
    pub event_json: String,
    pub previous_hash_hex: String,
    pub record_hash_hex: String,
    pub created_at_ms: i64,
    pub hash_version: i64,
    pub signature_b64: Option<String>,
    pub key_id: Option<String>,
}

impl ShippedAuditRecord {
    /// Recompute this record's hash from its own content, dispatching on
    /// `hash_version` exactly as the store does (v2 binds `event_type` + `sequence`
    /// and is domain-separated; v1 is the legacy `prev || json || ts` form).
    fn recompute_hash(&self) -> String {
        match self.hash_version {
            2 => AuditChainLinker::compute_record_hash_v2(
                &self.previous_hash_hex,
                &self.event_type,
                &self.event_json,
                self.created_at_ms,
                self.sequence,
            ),
            // hash_version 1 (and any pre-v2 legacy row): the original
            // `prev || canonical_json || created_at_ms` construction.
            _ => AuditChainLinker::compute_record_hash_v1(
                &self.previous_hash_hex,
                &self.event_json,
                self.created_at_ms,
            ),
        }
    }
}

/// The verdict of an off-box re-verification of a shipped chain segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShippedChainVerdict {
    /// The whole segment is internally consistent: every hash recomputes, prev
    /// linkage holds, sequences are contiguous ascending.
    Ok { records: u64, last_sequence: u64 },
    /// No records were supplied.
    Empty,
    /// A record's recomputed hash does not match its stored `record_hash_hex`
    /// (content tamper).
    HashMismatch { sequence: u64 },
    /// A record's `previous_hash_hex` does not link to the prior record's hash
    /// (reorder / splice).
    PrevLinkageBroken { sequence: u64 },
    /// A gap in the ascending sequence (a dropped/missing record).
    SequenceGap { expected: u64, found: u64 },
}

impl ShippedChainVerdict {
    /// `true` only for [`ShippedChainVerdict::Ok`].
    pub fn is_ok(&self) -> bool {
        matches!(self, ShippedChainVerdict::Ok { .. })
    }
}

/// Independently re-verify a CONTIGUOUS shipped chain segment (in ascending
/// sequence order) using only the records themselves — the WORM guarantee. The
/// segment need not start at sequence 1 (incremental shipping starts wherever the
/// last batch ended), but it must be internally contiguous: each record's
/// sequence is the previous + 1 and its `previous_hash` is the previous record's
/// hash. The first record's `previous_hash` is taken as given (its predecessor is
/// outside the segment); every subsequent link and every hash is checked.
pub fn verify_shipped_chain(records: &[ShippedAuditRecord]) -> ShippedChainVerdict {
    if records.is_empty() {
        return ShippedChainVerdict::Empty;
    }
    let mut prev_hash: Option<&str> = None;
    let mut expected_seq: Option<u64> = None;

    for r in records {
        if let Some(exp) = expected_seq {
            if r.sequence != exp {
                return ShippedChainVerdict::SequenceGap {
                    expected: exp,
                    found: r.sequence,
                };
            }
        }
        if let Some(ph) = prev_hash {
            if r.previous_hash_hex != ph {
                return ShippedChainVerdict::PrevLinkageBroken {
                    sequence: r.sequence,
                };
            }
        }
        if r.recompute_hash() != r.record_hash_hex {
            return ShippedChainVerdict::HashMismatch {
                sequence: r.sequence,
            };
        }
        prev_hash = Some(&r.record_hash_hex);
        expected_seq = Some(r.sequence + 1);
    }

    ShippedChainVerdict::Ok {
        records: records.len() as u64,
        // Safe: non-empty checked above.
        last_sequence: records.last().map(|r| r.sequence).unwrap_or(0),
    }
}

/// An append-only (WORM) destination for shipped audit records. Implementations
/// must only ever APPEND — never rewrite or truncate — so the off-box copy is
/// write-once. Appends are all-or-nothing from the shipper's view (a partial
/// append surfaces as `Err` and the high-water mark is not advanced).
pub trait AuditSink {
    fn append(&mut self, records: &[ShippedAuditRecord]) -> std::io::Result<()>;
}

/// In-memory sink for tests and for composing a sink in front of another
/// transport. Accumulates the shipped records in order.
#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
    pub records: Vec<ShippedAuditRecord>,
}

impl AuditSink for InMemoryAuditSink {
    fn append(&mut self, records: &[ShippedAuditRecord]) -> std::io::Result<()> {
        self.records.extend_from_slice(records);
        Ok(())
    }
}

/// Append-only JSON-Lines file sink: one JSON object per line, opened in append
/// mode. A WORM-mounted volume or a log-shipping agent tailing the file carries
/// it off-box. Each `append` is written then `sync_all`-fsync'd before returning,
/// so a shipped batch is durable on stable storage before the cursor advances.
pub struct JsonlFileAuditSink {
    path: std::path::PathBuf,
}

impl JsonlFileAuditSink {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AuditSink for JsonlFileAuditSink {
    fn append(&mut self, records: &[ShippedAuditRecord]) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // Build the whole batch in memory then write once, so a serialization
        // error cannot leave a half-written line on the WORM file.
        let mut buf = String::new();
        for r in records {
            let line = serde_json::to_string(r)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            buf.push_str(&line);
            buf.push('\n');
        }
        file.write_all(buf.as_bytes())?;
        // `flush` only drains userspace buffers; `sync_all` (fsync) forces the
        // batch to stable storage. The cursor advances only after this returns, so
        // ship-then-advance must be durable HERE — a power loss must not lose a
        // batch the cursor already considers shipped.
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }
}

/// `posture_engine_state` key holding the shipping CURSOR — the inclusive next
/// sequence to ship (one past the last sequence durably appended to the sink).
/// Persisted so shipping resumes across restarts without re-shipping the ledger.
pub const AUDIT_SHIP_CURSOR_KEY: &str = "audit_ship_cursor";

/// Default per-cycle ship batch cap — bounds the work (and the memory) of one
/// shipping cycle; the next cycle picks up the remainder.
pub const DEFAULT_SHIP_BATCH_LIMIT: u64 = 512;

/// The result of a shipping cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipOutcome {
    pub shipped: usize,
    /// The cursor after this cycle — the inclusive next sequence to ship
    /// (`last_shipped + 1`, or the input cursor unchanged when nothing shipped).
    pub next_cursor: u64,
}

/// A shipping-cycle error — either the store read or the sink append failed. The
/// high-water mark is NOT advanced on either, so the records re-ship next cycle
/// (at-least-once: audit evidence is never dropped; the off-box consumer dedupes
/// by `sequence`).
#[derive(Debug)]
pub enum ShipError {
    Store(rusqlite::Error),
    Sink(std::io::Error),
}

impl std::fmt::Display for ShipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShipError::Store(e) => write!(f, "audit ship store read: {e}"),
            ShipError::Sink(e) => write!(f, "audit ship sink append: {e}"),
        }
    }
}

impl std::error::Error for ShipError {}

/// Ship the audit records with `sequence >= from_cursor` to `sink` (ascending, up
/// to `batch_limit`). Returns how many shipped and the new cursor
/// (`last_shipped + 1`, or `from_cursor` unchanged when there was nothing new).
/// Does NOT persist the cursor — the caller advances it only after the sink append
/// succeeds (see [`ship_and_advance`]).
pub fn ship_new_records(
    store: &crate::verifier_store::VerifierStore,
    sink: &mut dyn AuditSink,
    from_cursor: u64,
    batch_limit: u64,
) -> Result<ShipOutcome, ShipError> {
    let records = store
        .load_shippable_audit_records(from_cursor, batch_limit)
        .map_err(ShipError::Store)?;
    if records.is_empty() {
        return Ok(ShipOutcome {
            shipped: 0,
            next_cursor: from_cursor,
        });
    }
    sink.append(&records).map_err(ShipError::Sink)?;
    let next_cursor = records
        .last()
        .map(|r| r.sequence + 1)
        .unwrap_or(from_cursor);
    Ok(ShipOutcome {
        shipped: records.len(),
        next_cursor,
    })
}

/// The persisted shipping cursor — the inclusive next sequence to ship (0 if never
/// shipped / unparseable, i.e. start from the genesis row).
pub fn load_ship_cursor(
    store: &crate::verifier_store::VerifierStore,
) -> Result<u64, rusqlite::Error> {
    Ok(store
        .load_engine_state(AUDIT_SHIP_CURSOR_KEY)?
        .and_then(|s| s.parse::<u64>().ok())
        // The cursor is later bound as a SQLite INTEGER (i64). A persisted value
        // beyond i64::MAX is corruption — treat it as "unset" and restart from the
        // genesis row (at-least-once re-ship, never a wrapped bind) rather than
        // trusting a poisoned cursor.
        .filter(|&v| v <= i64::MAX as u64)
        .unwrap_or(0))
}

/// One full shipping cycle: load the persisted cursor, ship everything from it,
/// and — ONLY after the sink append succeeds — advance the persisted cursor.
/// Ordering is deliberate (ship-then-advance): a crash between the append and the
/// advance re-ships the last batch next cycle (at-least-once), so audit evidence
/// is never lost; the off-box consumer dedupes by `sequence`. The alternative
/// (advance-then-ship) could LOSE records on a crash — unacceptable for a
/// tamper-evidence log.
pub fn ship_and_advance(
    store: &crate::verifier_store::VerifierStore,
    sink: &mut dyn AuditSink,
    batch_limit: u64,
) -> Result<ShipOutcome, ShipError> {
    let from = load_ship_cursor(store).map_err(ShipError::Store)?;
    let outcome = ship_new_records(store, sink, from, batch_limit)?;
    if outcome.next_cursor > from {
        store
            .save_engine_state(AUDIT_SHIP_CURSOR_KEY, &outcome.next_cursor.to_string())
            .map_err(ShipError::Store)?;
    }
    Ok(outcome)
}

/// Interval between audit-shipping cycles. Shipping is a durability enhancement,
/// not a latency-critical path, so a relaxed cadence bounds the off-box lag while
/// keeping the fsync-per-cycle cost low.
pub const AUDIT_SHIP_INTERVAL_MS: u64 = 5_000;

/// Env var naming the append-only off-box sink FILE. Set → the background shipper
/// runs, appending new audit records there each cycle (a WORM volume / log-shipping
/// agent carries the file off-box). Unset/empty → shipping is OFF (default;
/// byte-identical prior behaviour).
pub const AUDIT_SHIP_PATH_ENV: &str = "KIRRA_AUDIT_SHIP_PATH";

/// Spawn the background audit shipper IF `KIRRA_AUDIT_SHIP_PATH` names a sink file;
/// otherwise a no-op (shipping is opt-in, default OFF). Supervised **non-critical**
/// on a `AUDIT_SHIP_INTERVAL_MS` loop: a wedged shipper cannot make anything unsafe
/// (the LOCAL audit chain is intact and independently verifiable; off-box shipping
/// is a survivability enhancement), so a panic restarts it but never forces
/// `LockedOut`. Each cycle runs `ship_and_advance` off the async thread via
/// `StoreHandle::call`; the `JsonlFileAuditSink` is cheap (a path; it opens +
/// fsyncs per append), so a fresh one per cycle is fine.
///
/// Returns `true` if the shipper was spawned, `false` if shipping is disabled.
pub fn spawn_audit_shipper(app: std::sync::Arc<crate::verifier::AppState>) -> bool {
    let path = match std::env::var(AUDIT_SHIP_PATH_ENV) {
        Ok(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => return false, // shipping off (default)
    };

    crate::supervisor::spawn_supervised(
        "audit_shipper",
        /* critical    */ false,
        /* run-forever  */ true,
        /* escalate     */ None,
        move || {
            let app = std::sync::Arc::clone(&app);
            let path = path.clone();
            async move {
                let mut tick =
                    tokio::time::interval(std::time::Duration::from_millis(AUDIT_SHIP_INTERVAL_MS));
                // Coalesce missed windows (each cycle is idempotent — the cursor
                // makes re-runs safe); no catch-up burst after runtime starvation.
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let path = path.clone();
                    // The whole read-ship-advance runs under one store call so the
                    // cursor read/write and the ledger read see a consistent store.
                    match app
                        .store
                        .call(move |store| {
                            let mut sink = JsonlFileAuditSink::new(path);
                            ship_and_advance(store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT)
                        })
                        .await
                    {
                        Ok(Ok(outcome)) if outcome.shipped > 0 => tracing::info!(
                            shipped = outcome.shipped,
                            next_cursor = outcome.next_cursor,
                            "audit shipper: appended records to the off-box sink"
                        ),
                        Ok(Ok(_)) => {} // nothing new this cycle
                        Ok(Err(e)) => tracing::error!(error = %e, "audit shipper cycle failed"),
                        Err(_) => tracing::error!("audit shipper store task failed"),
                    }
                }
            }
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a v2 record chained onto `prev_hash` at `sequence`, computing its
    /// real record hash (a faithful shipped record).
    fn rec(
        sequence: u64,
        prev_hash: &str,
        event_type: &str,
        json: &str,
        ts: i64,
    ) -> ShippedAuditRecord {
        let record_hash =
            AuditChainLinker::compute_record_hash_v2(prev_hash, event_type, json, ts, sequence);
        ShippedAuditRecord {
            sequence,
            event_type: event_type.to_string(),
            event_json: json.to_string(),
            previous_hash_hex: prev_hash.to_string(),
            record_hash_hex: record_hash,
            created_at_ms: ts,
            hash_version: 2,
            signature_b64: None,
            key_id: None,
        }
    }

    /// A short, well-formed chain of `n` records starting at sequence `start`.
    fn chain(start: u64, n: u64) -> Vec<ShippedAuditRecord> {
        let mut out = Vec::new();
        let mut prev = "GENESIS".to_string();
        for i in 0..n {
            let seq = start + i;
            let r = rec(
                seq,
                &prev,
                "Evt",
                &format!("{{\"i\":{seq}}}"),
                1_000 + seq as i64,
            );
            prev = r.record_hash_hex.clone();
            out.push(r);
        }
        out
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(verify_shipped_chain(&[]), ShippedChainVerdict::Empty);
    }

    #[test]
    fn well_formed_chain_verifies() {
        let c = chain(1, 5);
        assert_eq!(
            verify_shipped_chain(&c),
            ShippedChainVerdict::Ok {
                records: 5,
                last_sequence: 5
            }
        );
    }

    #[test]
    fn segment_not_starting_at_one_verifies() {
        // Incremental shipping resumes mid-chain; internal consistency is what's
        // checked, so a segment starting at 100 is fine.
        let c = chain(100, 3);
        assert!(verify_shipped_chain(&c).is_ok());
    }

    #[test]
    fn content_tamper_breaks_the_hash() {
        let mut c = chain(1, 4);
        // Mutate the event payload of the 3rd record WITHOUT recomputing its hash.
        c[2].event_json = "{\"i\":999}".to_string();
        assert_eq!(
            verify_shipped_chain(&c),
            ShippedChainVerdict::HashMismatch { sequence: 3 }
        );
    }

    #[test]
    fn dropping_a_record_breaks_contiguity() {
        let mut c = chain(1, 5);
        c.remove(2); // drop sequence 3
        assert_eq!(
            verify_shipped_chain(&c),
            ShippedChainVerdict::SequenceGap {
                expected: 3,
                found: 4
            }
        );
    }

    #[test]
    fn reordering_breaks_prev_linkage() {
        let mut c = chain(1, 5);
        c.swap(2, 3); // sequences now 1,2,4,3,5 → seq check fires first
                      // The swap makes sequence 4 appear where 3 was expected.
        assert_eq!(
            verify_shipped_chain(&c),
            ShippedChainVerdict::SequenceGap {
                expected: 3,
                found: 4
            }
        );
    }

    #[test]
    fn splicing_a_foreign_record_breaks_linkage() {
        // Keep sequences contiguous but replace a record's body with a different
        // one that re-hashes correctly for ITS content — its prev_hash no longer
        // matches the real prior record's hash.
        let mut c = chain(1, 4);
        let foreign = rec(3, "NOT-THE-REAL-PREV", "Evt", "{\"i\":3}", 1_003);
        c[2] = foreign;
        assert_eq!(
            verify_shipped_chain(&c),
            ShippedChainVerdict::PrevLinkageBroken { sequence: 3 }
        );
    }

    #[test]
    fn in_memory_sink_accumulates_in_order() {
        let mut sink = InMemoryAuditSink::default();
        let c = chain(1, 3);
        sink.append(&c[..2]).unwrap();
        sink.append(&c[2..]).unwrap();
        assert_eq!(sink.records, c);
        assert!(verify_shipped_chain(&sink.records).is_ok());
    }

    #[test]
    fn jsonl_file_sink_roundtrips_and_verifies() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kirra_audit_ship_test_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut sink = JsonlFileAuditSink::new(&path);
        let c = chain(1, 4);
        sink.append(&c[..2]).unwrap();
        sink.append(&c[2..]).unwrap(); // append-only: second batch adds, not rewrites

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<ShippedAuditRecord> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            parsed, c,
            "file is the appended union of both batches, in order"
        );
        assert!(verify_shipped_chain(&parsed).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    // --- store-backed shipping (real audit chain) -------------------------

    use crate::verifier_store::VerifierStore;

    fn store_with_events(n: usize) -> VerifierStore {
        let mut s = VerifierStore::new(":memory:").expect("in-memory store");
        for i in 0..n {
            // A standalone chained audit append (the same primitive campaign /
            // clearance mutations use) — real hash-linked, sequenced rows.
            s.append_clearance_audit_event(
                "ShipTestEvent",
                &format!("{{\"i\":{i}}}"),
                1_000 + i as u64,
            )
            .expect("append audit event");
        }
        s
    }

    #[test]
    fn ships_real_audit_records_and_reverifies_off_box() {
        let store = store_with_events(4);
        let mut sink = InMemoryAuditSink::default();
        let outcome = ship_and_advance(&store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT).unwrap();
        assert_eq!(outcome.shipped, 4);
        assert_eq!(sink.records.len(), 4);
        // The off-box copy re-verifies with NO reference to the source DB.
        assert!(
            verify_shipped_chain(&sink.records).is_ok(),
            "shipped real chain must re-verify independently"
        );
        // The genesis row (sequence 0) shipped, so the cursor advanced past it.
        assert_eq!(sink.records[0].sequence, 0, "chain is 0-based");
        assert_eq!(load_ship_cursor(&store).unwrap(), outcome.next_cursor);
        assert_eq!(outcome.next_cursor, 4, "next cursor is last_shipped(3) + 1");
    }

    #[test]
    fn shipping_resumes_from_high_water() {
        let mut store = store_with_events(3);
        let mut sink = InMemoryAuditSink::default();
        let first = ship_and_advance(&store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT).unwrap();
        assert_eq!(first.shipped, 3);

        // A second cycle with no new events ships nothing.
        let none = ship_and_advance(&store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT).unwrap();
        assert_eq!(none.shipped, 0);
        assert_eq!(sink.records.len(), 3);

        // Append two more events, then ship again — only the new ones move.
        store
            .append_clearance_audit_event("ShipTestEvent", "{\"i\":3}", 2_000)
            .unwrap();
        store
            .append_clearance_audit_event("ShipTestEvent", "{\"i\":4}", 2_001)
            .unwrap();
        let more = ship_and_advance(&store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT).unwrap();
        assert_eq!(more.shipped, 2, "only the two new records ship");
        assert_eq!(sink.records.len(), 5);
        // The accumulated off-box copy is a single contiguous, verifiable chain.
        assert!(verify_shipped_chain(&sink.records).is_ok());
    }

    #[test]
    fn tampering_the_shipped_copy_is_detected() {
        let store = store_with_events(4);
        let mut sink = InMemoryAuditSink::default();
        ship_and_advance(&store, &mut sink, DEFAULT_SHIP_BATCH_LIMIT).unwrap();
        // An adversary mutates the off-box copy after shipping.
        sink.records[1].event_json = "{\"i\":666}".to_string();
        assert!(
            !verify_shipped_chain(&sink.records).is_ok(),
            "a mutated off-box record must fail re-verification"
        );
    }

    #[test]
    fn corrupt_persisted_cursor_restarts_from_genesis() {
        // A poisoned cursor beyond the SQLite INTEGER domain must not be trusted
        // (it would wrap to a negative bind and re-ship everything from a wrong
        // point / desync). It is treated as unset → restart from 0.
        let store = store_with_events(2);
        store
            .save_engine_state(AUDIT_SHIP_CURSOR_KEY, &(i64::MAX as u64 + 1).to_string())
            .unwrap();
        assert_eq!(
            load_ship_cursor(&store).unwrap(),
            0,
            "out-of-domain cursor → 0"
        );
        // And an in-domain cursor round-trips.
        store.save_engine_state(AUDIT_SHIP_CURSOR_KEY, "1").unwrap();
        assert_eq!(load_ship_cursor(&store).unwrap(), 1);
    }

    #[test]
    fn out_of_range_from_sequence_ships_nothing() {
        // A `from_sequence` beyond i64::MAX saturates the bind rather than wrapping
        // negative — it must return no rows (fail-closed), never the whole ledger.
        let store = store_with_events(3);
        let recs = store
            .load_shippable_audit_records(u64::MAX, DEFAULT_SHIP_BATCH_LIMIT)
            .unwrap();
        assert!(
            recs.is_empty(),
            "a beyond-range cursor must ship nothing, not wrap to match all"
        );
    }

    #[test]
    fn batch_limit_ships_in_chunks() {
        let store = store_with_events(5);
        let mut sink = InMemoryAuditSink::default();
        // Ship two at a time via the low-level fn, advancing the cursor manually.
        let a = ship_new_records(&store, &mut sink, 0, 2).unwrap();
        assert_eq!(a.shipped, 2);
        let b = ship_new_records(&store, &mut sink, a.next_cursor, 2).unwrap();
        assert_eq!(b.shipped, 2);
        let c = ship_new_records(&store, &mut sink, b.next_cursor, 2).unwrap();
        assert_eq!(c.shipped, 1);
        assert_eq!(sink.records.len(), 5);
        assert!(verify_shipped_chain(&sink.records).is_ok());
    }
}
