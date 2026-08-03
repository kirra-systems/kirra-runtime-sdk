//! Compaction-with-citation — the experiment behind ADR-0041 §11.3.
//!
//! The blueprint calls this "the one place P2 (append-only forever) is
//! knowingly bounded", and ADR-0041 ratifies the *policy* while deferring every
//! threshold until measured. What neither settles is the mechanism, and there
//! is a real problem hiding in the gap:
//!
//! **Deleting events from a hash-chained append-only log breaks the chain.**
//!
//! Two bad answers are available. Re-chaining everything after the window
//! rewrites history, which is precisely what §11.3 forbids, and costs O(n) on a
//! device that has better uses for its I/O. Leaving the hole makes the log
//! unverifiable past the first compaction, at which point the tamper evidence
//! the whole design rests on is gone.
//!
//! # The mechanism this experiment implements and measures
//!
//! A compacted window `[lo, hi]` is replaced by ONE `Summary` event at
//! generation `lo`, plus a `compaction_citations` row carrying:
//!
//! - what was summarized (`lo`, `hi`, `event_count`);
//! - `range_digest` — a hash over the canonical bytes of every removed event,
//!   in generation order, so anyone holding the originals can prove they are
//!   the ones that were removed;
//! - `chain_before` and `chain_after` — the chain digests on either side.
//!
//! Verification links the summary from `chain_before` and then resumes from
//! `chain_after`, so events written *after* the window still verify against the
//! links they were originally computed from. Nothing is re-chained and no
//! history is rewritten.
//!
//! # What is lost, stated plainly
//!
//! After compaction you can no longer verify the *contents* of the removed
//! span — only that a span was removed, how large it was, and what it hashed
//! to. Full tamper evidence degrades to **tamper-evident citation of a removed
//! span**. That is a real loss, it is the price of finite disk, and the design
//! makes it visible rather than silent: [`ChainStatus::Intact`] carries
//! `compacted_windows`, and a query into a compacted window returns
//! [`Resolution::DegradedSummary`] rather than a value or a bare absence.
//!
//! That last distinction is the sharp one. §11.3 requires a time-travel query
//! into a compacted window to return the summary "*and say so* — degraded
//! resolution, never silent fabrication". Returning `Unknown` would satisfy
//! "never fabricates" while failing the more important half: `Unknown` is what
//! you also get for a thing that was never observed, and conflating "we had
//! this and compacted it" with "we never saw it" destroys the one fact an
//! incident investigator most needs.

use crate::sha256;
use crate::standin::{ChainStatus, Event, EventKind, RetentionClass, Store};
use rusqlite::{params, OptionalExtension};

#[derive(Debug)]
pub enum CompactError {
    /// The window contains events a retention class protects. Refused whole —
    /// never partially applied, because a partial compaction is indistinguishable
    /// from a policy nobody wrote down.
    Protected {
        generation: u64,
        class: &'static str,
    },
    /// The window is empty or already compacted.
    Empty,
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protected { generation, class } => write!(
                f,
                "generation {generation} is retention class `{class}`, which compaction \
                 must retain (ADR-0041 §11.3); window refused"
            ),
            Self::Empty => write!(f, "no compactable events in the window"),
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
        }
    }
}

impl From<rusqlite::Error> for CompactError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

#[derive(Debug, Clone)]
pub struct Citation {
    pub summary_generation: u64,
    pub lo: u64,
    pub hi: u64,
    pub event_count: u64,
    pub range_digest: String,
    pub chain_before: String,
    pub chain_after: String,
}

/// The answer to a bitemporal point query over a possibly-compacted log.
///
/// Three outcomes, deliberately not two. Blueprint §12 already refuses to let
/// `Unknown` be an error; §11.3 additionally refuses to let a compacted window
/// masquerade as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A raw observation still in the log.
    Value { payload: String, generation: u64 },
    /// The window covering this instant was compacted. The summary is returned
    /// **and says so**.
    DegradedSummary {
        citation_generation: u64,
        lo: u64,
        hi: u64,
        range_digest: String,
    },
    /// Nothing was ever recorded here. Distinct from `DegradedSummary` on
    /// purpose: conflating them would erase the difference between "we never
    /// saw it" and "we had it and compacted it".
    Unknown,
}

/// Compact `[lo, hi]` into a single cited summary.
///
/// Refuses the whole window if it contains a protected retention class. That is
/// stricter than skipping the protected rows, and deliberately so: a window
/// with holes in it is no longer a window, its `range_digest` would attest to a
/// selection rather than a span, and "which events did this summary actually
/// cover" becomes a question nobody can answer later.
pub fn compact_range(
    store: &mut Store,
    lo: u64,
    hi: u64,
    now_ms: i64,
) -> Result<Citation, CompactError> {
    // Refuse before mutating anything.
    {
        let mut stmt = store.conn.prepare(
            "SELECT generation, retention_class FROM world_events \
              WHERE generation BETWEEN ?1 AND ?2 ORDER BY generation",
        )?;
        let mut rows = stmt.query(params![lo as i64, hi as i64])?;
        let mut any = false;
        while let Some(r) = rows.next()? {
            any = true;
            let class = RetentionClass::parse(&r.get::<_, String>(1)?);
            if class.is_protected() {
                return Err(CompactError::Protected {
                    generation: r.get::<_, i64>(0)? as u64,
                    class: class.as_str(),
                });
            }
        }
        if !any {
            return Err(CompactError::Empty);
        }
    }

    let chain_before: String = store
        .conn
        .query_row(
            "SELECT chain_digest FROM world_events WHERE generation < ?1 \
              ORDER BY generation DESC LIMIT 1",
            params![lo as i64],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "genesis".to_string());

    let chain_after: String = store.conn.query_row(
        "SELECT chain_digest FROM world_events WHERE generation = ?1",
        params![hi as i64],
        |r| r.get(0),
    )?;

    // The citation's substance: a hash over exactly the bytes about to be
    // destroyed, in order. Anyone holding the originals can recompute it and
    // prove these are the events that were removed.
    let (range_digest, event_count) = range_digest(store, lo, hi)?;

    let summary = Event {
        generation: lo,
        event_id: format!("summary-{lo:012}-{hi:012}"),
        txn_time_ms: now_ms,
        valid_from_ms: store.conn.query_row(
            "SELECT MIN(valid_from_ms) FROM world_events WHERE generation BETWEEN ?1 AND ?2",
            params![lo as i64, hi as i64],
            |r| r.get(0),
        )?,
        valid_to_ms: store
            .conn
            .query_row(
                "SELECT MAX(valid_from_ms) FROM world_events WHERE generation BETWEEN ?1 AND ?2",
                params![lo as i64, hi as i64],
                |r| r.get(0),
            )
            .optional()?,
        source: "compaction".to_string(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        kind: EventKind::Summary,
        subject: format!("window:{lo}..{hi}"),
        predicate: None,
        object: None,
        payload: format!("compacted {event_count} events, range_digest={range_digest}"),
        // A summary is itself an adjudicated conclusion, not raw traffic: it
        // must never become eligible for a later compaction pass, or the
        // citation chain would erode one generation at a time.
        retention: RetentionClass::Adjudication,
    };
    let summary_payload_digest = sha256::hex(&summary.canonical_bytes());
    let summary_chain = sha256::chain_link(&chain_before, summary_payload_digest.as_bytes());

    let tx = store.conn.transaction()?;
    tx.execute(
        "DELETE FROM world_events WHERE generation BETWEEN ?1 AND ?2",
        params![lo as i64, hi as i64],
    )?;
    tx.execute(
        "INSERT INTO world_events (generation, event_id, txn_time_ms, valid_from_ms, valid_to_ms, \
         source, source_version, payload_schema, kind, subject, predicate, object, payload, \
         payload_digest, chain_digest, retention_class) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,1,?8,?9,NULL,NULL,?10,?11,?12,?13)",
        params![
            summary.generation as i64,
            summary.event_id,
            summary.txn_time_ms,
            summary.valid_from_ms,
            summary.valid_to_ms,
            summary.source,
            summary.source_version,
            summary.kind.as_str(),
            summary.subject,
            summary.payload,
            summary_payload_digest,
            summary_chain,
            summary.retention.as_str(),
        ],
    )?;
    tx.execute(
        "INSERT INTO compaction_citations (summary_generation, lo_generation, hi_generation, \
         event_count, range_digest, chain_before, chain_after, compacted_at_ms) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            lo as i64,
            lo as i64,
            hi as i64,
            event_count as i64,
            range_digest,
            chain_before,
            chain_after,
            now_ms,
        ],
    )?;
    tx.commit()?;

    Ok(Citation {
        summary_generation: lo,
        lo,
        hi,
        event_count,
        range_digest,
        chain_before,
        chain_after,
    })
}

/// Hash over the canonical bytes of `[lo, hi]`, in generation order.
///
/// Public so a holder of the original events can recompute it independently —
/// which is the only thing that makes the citation more than an assertion.
pub fn range_digest(store: &Store, lo: u64, hi: u64) -> rusqlite::Result<(String, u64)> {
    let mut stmt = store.conn.prepare(
        "SELECT payload_digest FROM world_events WHERE generation BETWEEN ?1 AND ?2 \
          ORDER BY generation ASC",
    )?;
    let mut rows = stmt.query(params![lo as i64, hi as i64])?;
    let mut buf = Vec::new();
    let mut count = 0u64;
    while let Some(r) = rows.next()? {
        buf.extend_from_slice(r.get::<_, String>(0)?.as_bytes());
        buf.push(0x1e);
        count += 1;
    }
    Ok((sha256::hex(&buf), count))
}

/// Redact one event's content, leaving a tombstone.
///
/// The row and its ORIGINAL `payload_digest` stay, so the chain remains
/// continuous and the deletion is represented as a record rather than as
/// absence — ADR-0041's rule that "deletion and privacy rules are represented
/// explicitly as records, not as absence; a redaction must leave a tombstone,
/// or the chain breaks."
pub fn redact(store: &mut Store, generation: u64, reason: &str) -> rusqlite::Result<()> {
    store.conn.execute(
        "UPDATE world_events SET payload = ?2, redacted = 1 WHERE generation = ?1",
        params![generation as i64, format!("[REDACTED: {reason}]")],
    )?;
    Ok(())
}

/// Bitemporal point query that is honest about compaction.
pub fn resolve_at(
    store: &Store,
    subject: &str,
    valid_ms: i64,
    txn_ms: i64,
) -> rusqlite::Result<Resolution> {
    let hit: Option<(String, i64)> = store
        .conn
        .query_row(
            "SELECT payload, generation FROM world_events \
              WHERE subject = ?1 AND kind = 'observation' AND redacted = 0 \
                AND valid_from_ms <= ?2 AND txn_time_ms <= ?3 \
              ORDER BY valid_from_ms DESC, generation DESC LIMIT 1",
            params![subject, valid_ms, txn_ms],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((payload, generation)) = hit {
        return Ok(Resolution::Value {
            payload,
            generation: generation as u64,
        });
    }

    // Nothing live answers. Before reporting absence, check whether a compacted
    // window covers this instant — the difference between "never observed" and
    // "observed, then compacted" is the whole point.
    let cited: Option<(i64, i64, i64, String)> = store
        .conn
        .query_row(
            "SELECT c.summary_generation, c.lo_generation, c.hi_generation, c.range_digest \
               FROM compaction_citations c \
               JOIN world_events s ON s.generation = c.summary_generation \
              WHERE s.valid_from_ms <= ?1 AND COALESCE(s.valid_to_ms, s.valid_from_ms) >= ?1 \
                AND s.txn_time_ms <= ?2 \
              ORDER BY c.hi_generation DESC LIMIT 1",
            params![valid_ms, txn_ms],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    Ok(match cited {
        Some((summary_generation, lo, hi, range_digest)) => Resolution::DegradedSummary {
            citation_generation: summary_generation as u64,
            lo: lo as u64,
            hi: hi as u64,
            range_digest,
        },
        None => Resolution::Unknown,
    })
}

/// Every maximal run of compactable (`raw`) generations inside `[lo, hi]`.
///
/// This is what a retention policy actually operates on. Protected events sit
/// scattered through the log — a safety event here, an operator confirmation
/// there — and they partition the compactable traffic into spans. Compacting
/// "a window" in the abstract measures an operation nobody performs; compacting
/// every raw span in a retention horizon is the real one, and it is the only
/// version that reclaims a realistic amount of disk.
///
/// Spans shorter than `min_len` are skipped: collapsing three events into one
/// summary plus one citation row is not a saving, and doing it anyway would
/// bury the log in citations.
pub fn raw_spans(
    store: &Store,
    lo: u64,
    hi: u64,
    min_len: u64,
) -> rusqlite::Result<Vec<(u64, u64)>> {
    let mut stmt = store.conn.prepare(
        "SELECT generation, retention_class, kind FROM world_events \
          WHERE generation BETWEEN ?1 AND ?2 ORDER BY generation ASC",
    )?;
    let mut rows = stmt.query(params![lo as i64, hi as i64])?;
    let mut spans = Vec::new();
    let mut start: Option<u64> = None;
    let mut last: u64 = 0;

    while let Some(r) = rows.next()? {
        let g = r.get::<_, i64>(0)? as u64;
        let class = RetentionClass::parse(&r.get::<_, String>(1)?);
        let kind = EventKind::parse(&r.get::<_, String>(2)?);
        let compactable = !class.is_protected() && kind != EventKind::Summary;
        match (compactable, start) {
            (true, None) => start = Some(g),
            (true, Some(_)) => {}
            (false, Some(s0)) => {
                if last >= s0 && last - s0 + 1 >= min_len {
                    spans.push((s0, last));
                }
                start = None;
            }
            (false, None) => {}
        }
        last = g;
    }
    if let Some(s0) = start {
        if last >= s0 && last - s0 + 1 >= min_len {
            spans.push((s0, last));
        }
    }
    Ok(spans)
}

// ---------------------------------------------------------------------------
// The experiment
// ---------------------------------------------------------------------------

pub struct CompactionResult {
    /// The citation itself, emitted verbatim. A holder of the original events
    /// recomputes `range_digest` over `[lo, hi]` and compares — which is the
    /// only thing that makes the citation evidence rather than an assertion, so
    /// every field of it has to reach the results file.
    pub citation: Citation,
    pub events_before: u64,
    pub events_after: u64,
    pub compacted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_after_vacuum: u64,
    pub compact_ms: f64,
    pub verify_ms: f64,
    /// Every assertion §11.3 makes, checked rather than assumed.
    pub citation_digest_reverifies: bool,
    pub protected_events_survived: bool,
    pub protected_window_refused: bool,
    pub chain_intact_across_window: bool,
    pub compacted_windows_reported: u64,
    /// Every compaction performed is reported by the verifier. A verifier that
    /// counted only some of them would understate the degradation.
    pub all_windows_reported: bool,
    pub query_into_window_is_degraded: bool,
    pub query_outside_window_is_unknown: bool,
    pub redaction_keeps_chain_intact: bool,
    pub tampered_summary_breaks_chain: bool,
}

/// Run the compaction experiment end to end.
///
/// Every boolean is a §11.3 requirement turned into a check. The last one is
/// the non-vacuity control: if tampering with a summary did *not* break the
/// chain, every other "intact" here would be worthless.
pub fn run(
    path: &std::path::Path,
    durability: crate::standin::Durability,
    events: u64,
    entities: u64,
    seed: u64,
    now_ms: i64,
) -> Result<CompactionResult, String> {
    use std::time::Instant;

    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }

    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    for chunk in crate::gen::events(0, events, entities, seed).chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }
    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let events_before = store.count_events().map_err(|e| e.to_string())?;
    let bytes_before = crate::bench::db_bytes(path);

    // The retention horizon: the older 10%-to-60% of the log, which is the
    // shape of a real policy (recent traffic stays raw, old raw traffic is
    // summarized, protected classes are never touched at any age).
    let horizon_lo = events / 10;
    let horizon_hi = (events * 6) / 10;

    // First, prove the refusal on a window that deliberately spans a protected
    // event. Without this the rest could pass on a log that happened to contain
    // no protected traffic at all.
    let protected_window_refused = match raw_spans(&store, horizon_lo, horizon_hi, 1)
        .map_err(|e| e.to_string())?
        .first()
        .copied()
    {
        // One past the end of the first raw span is, by construction, protected.
        Some((lo, hi)) if hi < horizon_hi => {
            matches!(
                compact_range(&mut store, lo, hi + 1, now_ms),
                Err(CompactError::Protected { .. })
            )
        }
        _ => false,
    };

    let protected_before: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM world_events WHERE retention_class != 'raw' AND kind != 'summary'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    let spans = raw_spans(&store, horizon_lo, horizon_hi, 8).map_err(|e| e.to_string())?;
    if spans.is_empty() {
        return Err("no compactable spans in the retention horizon".to_string());
    }

    // Digests taken while the originals still exist, so the citations can be
    // checked against something computed independently of the compaction pass.
    let mut expected: Vec<(String, u64)> = Vec::with_capacity(spans.len());
    for &(lo, hi) in &spans {
        expected.push(range_digest(&store, lo, hi).map_err(|e| e.to_string())?);
    }

    // A valid-time instant that lands inside the first compacted span.
    let window_valid_ms: i64 = store
        .conn
        .query_row(
            "SELECT valid_from_ms FROM world_events WHERE generation = ?1",
            params![spans[0].0 as i64],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    let t = Instant::now();
    let mut citations = Vec::with_capacity(spans.len());
    for &(lo, hi) in &spans {
        citations.push(
            compact_range(&mut store, lo, hi, now_ms)
                .map_err(|e| format!("compaction refused for [{lo},{hi}]: {e}"))?,
        );
    }
    let compact_ms = t.elapsed().as_secs_f64() * 1e3;

    let citation_digest_reverifies = citations
        .iter()
        .zip(&expected)
        .all(|(c, (digest, count))| c.range_digest == *digest && c.event_count == *count);
    let compacted: u64 = citations.iter().map(|c| c.event_count).sum();
    let citation = citations[0].clone();

    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_after = crate::bench::db_bytes(path);
    // VACUUM in WAL mode rewrites the whole database THROUGH the WAL, so
    // measuring immediately afterwards counts a fresh multi-megabyte WAL and
    // reports that compaction made the database bigger. Checkpoint first.
    store
        .conn
        .execute_batch("VACUUM;")
        .map_err(|e| e.to_string())?;
    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_after_vacuum = crate::bench::db_bytes(path);
    let events_after = store.count_events().map_err(|e| e.to_string())?;

    let t = Instant::now();
    let status = store.verify_chain().map_err(|e| e.to_string())?;
    let verify_ms = t.elapsed().as_secs_f64() * 1e3;
    let (chain_intact_across_window, compacted_windows_reported) = match &status {
        ChainStatus::Intact {
            compacted_windows, ..
        } => (true, *compacted_windows),
        _ => (false, 0),
    };
    let all_windows_reported = compacted_windows_reported == citations.len() as u64;

    // Excluding summaries: compaction MINTS a protected event (the citation is
    // itself an adjudicated conclusion), so a raw count would come out one
    // higher and the assertion would fail for the wrong reason — or, worse,
    // pass while one original protected event had actually been destroyed.
    let protected_after: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM world_events WHERE retention_class != 'raw' AND kind != 'summary'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    // A query landing inside the compacted window must be degraded, and one
    // landing on a subject that never existed must be Unknown. Both, because
    // either alone is satisfiable by a function that always returns the other.
    let inside =
        resolve_at(&store, "entity-999999", window_valid_ms, now_ms).map_err(|e| e.to_string())?;
    let query_into_window_is_degraded = matches!(inside, Resolution::DegradedSummary { .. });
    let outside = resolve_at(&store, "entity-999999", 1, 1).map_err(|e| e.to_string())?;
    let query_outside_window_is_unknown = matches!(outside, Resolution::Unknown);

    // Redaction: a live event's content goes, its tombstone and chain stay.
    let live: Option<i64> = store
        .conn
        .query_row(
            "SELECT generation FROM world_events WHERE kind = 'observation' \
              AND generation > ?1 ORDER BY generation LIMIT 1",
            params![citation.hi as i64],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let redaction_keeps_chain_intact = match live {
        Some(g) => {
            redact(&mut store, g as u64, "privacy request").map_err(|e| e.to_string())?;
            matches!(
                store.verify_chain().map_err(|e| e.to_string())?,
                ChainStatus::Intact { redacted, .. } if redacted >= 1
            )
        }
        None => false,
    };

    // Non-vacuity control: without this, every "intact" above is unfalsifiable.
    store
        .conn
        .execute(
            "UPDATE compaction_citations SET range_digest = 'tampered' WHERE summary_generation = ?1",
            params![citation.summary_generation as i64],
        )
        .map_err(|e| e.to_string())?;
    store
        .conn
        .execute(
            "UPDATE world_events SET payload = payload || '!' WHERE generation = ?1",
            params![citation.summary_generation as i64],
        )
        .map_err(|e| e.to_string())?;
    let tampered_summary_breaks_chain = !matches!(
        store.verify_chain().map_err(|e| e.to_string())?,
        ChainStatus::Intact { .. }
    );

    Ok(CompactionResult {
        citation_digest_reverifies,
        all_windows_reported,
        citation,
        events_before,
        events_after,
        compacted,
        bytes_before,
        bytes_after,
        bytes_after_vacuum,
        compact_ms,
        verify_ms,
        protected_events_survived: protected_before > 0 && protected_after == protected_before,
        protected_window_refused,
        chain_intact_across_window,
        compacted_windows_reported,
        query_into_window_is_degraded,
        query_outside_window_is_unknown,
        redaction_keeps_chain_intact,
        tampered_summary_breaks_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen;
    use crate::standin::Durability;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wm2-compact-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    const NOW: i64 = 1_700_000_900_000;

    /// Events with every retention class forced to `Raw`, produced at the
    /// SOURCE.
    ///
    /// The obvious shortcut — append normally, then
    /// `UPDATE world_events SET retention_class = 'raw'` — does not work, and
    /// finding that out was worth more than the test it broke: the retention
    /// class is inside the canonically-hashed bytes, so rewriting it breaks the
    /// chain at generation 0. That is exactly right. A retention class that
    /// could be quietly downgraded after the fact would let anyone make
    /// protected events compactable, and the protection would be a comment.
    /// `t::retention_downgrade_breaks_the_chain` pins the property.
    fn raw_events(start: u64, count: u64, entities: u64, seed: u64) -> Vec<crate::standin::Event> {
        let mut evs = gen::events(start, count, entities, seed);
        for e in &mut evs {
            e.retention = RetentionClass::Raw;
        }
        evs
    }

    #[test]
    fn retention_downgrade_breaks_the_chain() {
        let p = temp("downgrade");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        let mut evs = gen::events(0, 50, 5, 1);
        evs[30].retention = RetentionClass::Incident;
        s.append_batch(&evs).unwrap();
        assert!(matches!(
            s.verify_chain().unwrap(),
            ChainStatus::Intact { .. }
        ));

        // The attack this prevents: relabel a protected event as raw so a later
        // compaction pass is allowed to delete it.
        s.conn
            .execute(
                "UPDATE world_events SET retention_class = 'raw' WHERE generation = 30",
                [],
            )
            .unwrap();
        assert_eq!(
            s.verify_chain().unwrap(),
            ChainStatus::Broken { at_generation: 30 }
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_full_experiment_satisfies_every_section_11_3_requirement() {
        let p = temp("full");
        let r = run(&p, Durability::Off, 20_000, 300, 3, NOW).unwrap();

        assert!(
            r.citation_digest_reverifies,
            "the citation does not match what was removed"
        );
        assert!(
            r.protected_window_refused,
            "a window containing protected events was accepted"
        );
        assert!(
            r.protected_events_survived,
            "compaction destroyed protected retention classes"
        );
        assert!(
            r.chain_intact_across_window,
            "the chain does not verify across the compacted hole"
        );
        assert!(
            r.compacted_windows_reported >= 2,
            "expected several spans, got {}",
            r.compacted_windows_reported
        );
        assert!(
            r.all_windows_reported,
            "the verifier under-counted the compacted windows"
        );
        assert!(
            r.query_into_window_is_degraded,
            "a query into the window did not say it was degraded"
        );
        assert!(
            r.query_outside_window_is_unknown,
            "an absent subject was not reported Unknown"
        );
        assert!(
            r.redaction_keeps_chain_intact,
            "a tombstone broke the chain"
        );
        assert!(
            r.tampered_summary_breaks_chain,
            "tampering went undetected — every assertion above is vacuous"
        );
        assert!(r.events_after < r.events_before);
        assert!(
            r.bytes_after_vacuum < r.bytes_before,
            "compaction reclaimed nothing"
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_protected_event_refuses_the_whole_window_not_just_itself() {
        let p = temp("protected");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        let mut evs = gen::events(0, 100, 10, 1);
        evs[57].retention = RetentionClass::Incident;
        s.append_batch(&evs).unwrap();

        match compact_range(&mut s, 10, 90, NOW) {
            Err(CompactError::Protected { generation, class }) => {
                assert_eq!(generation, 57);
                assert_eq!(class, "incident");
            }
            other => panic!("expected refusal, got {other:?}"),
        }
        // Refused whole: nothing was mutated.
        assert_eq!(s.count_events().unwrap(), 100);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn an_unknown_retention_class_is_treated_as_protected() {
        // A typo in a retention policy must never make an event deletable.
        assert!(RetentionClass::parse("saftey").is_protected());
        assert!(!RetentionClass::parse("raw").is_protected());
    }

    #[test]
    fn a_summary_is_not_itself_compactable() {
        // Otherwise the citation chain erodes one pass at a time.
        let p = temp("nested");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&raw_events(0, 200, 20, 4)).unwrap();
        compact_range(&mut s, 10, 60, NOW).unwrap();
        assert!(matches!(
            compact_range(&mut s, 10, 80, NOW),
            Err(CompactError::Protected { .. })
        ));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn an_uncited_hole_is_reported_broken() {
        // Deleting the citation while keeping the summary is "compaction
        // silently rewriting history" — the failure §11.3 forbids by name.
        let p = temp("uncited");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&raw_events(0, 200, 20, 5)).unwrap();
        let c = compact_range(&mut s, 20, 100, NOW).unwrap();
        assert!(matches!(
            s.verify_chain().unwrap(),
            ChainStatus::Intact { .. }
        ));

        s.conn
            .execute(
                "DELETE FROM compaction_citations WHERE summary_generation = ?1",
                params![c.summary_generation as i64],
            )
            .unwrap();
        assert!(matches!(
            s.verify_chain().unwrap(),
            ChainStatus::Broken { .. }
        ));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_relinked_window_boundary_is_detected() {
        // Rewriting chain_before to something plausible must not verify: the
        // citation has to attest the ACTUAL boundary, not any boundary.
        let p = temp("relink");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&raw_events(0, 200, 20, 6)).unwrap();
        let c = compact_range(&mut s, 20, 100, NOW).unwrap();
        s.conn
            .execute(
                "UPDATE compaction_citations SET chain_before = ?2 WHERE summary_generation = ?1",
                params![c.summary_generation as i64, "genesis"],
            )
            .unwrap();
        assert!(matches!(
            s.verify_chain().unwrap(),
            ChainStatus::Broken { .. }
        ));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn compaction_is_idempotent_in_its_reported_window_count() {
        let p = temp("twice");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&raw_events(0, 400, 30, 7)).unwrap();
        compact_range(&mut s, 10, 100, NOW).unwrap();
        compact_range(&mut s, 150, 250, NOW).unwrap();
        match s.verify_chain().unwrap() {
            ChainStatus::Intact {
                compacted_windows, ..
            } => assert_eq!(compacted_windows, 2),
            other => panic!("expected intact across two windows, got {other:?}"),
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_range_digest_is_recomputable_by_a_holder_of_the_originals() {
        // The property that makes the citation evidence rather than assertion.
        let p = temp("digest");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&raw_events(0, 300, 25, 8)).unwrap();
        let (before, count) = range_digest(&s, 50, 150).unwrap();
        assert_eq!(count, 101);
        let c = compact_range(&mut s, 50, 150, NOW).unwrap();
        assert_eq!(c.range_digest, before);
        assert_eq!(c.event_count, 101);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_redacted_event_is_a_tombstone_not_an_absence() {
        let p = temp("redact");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&gen::events(0, 100, 10, 9)).unwrap();
        redact(&mut s, 42, "privacy request").unwrap();

        assert_eq!(
            s.count_events().unwrap(),
            100,
            "the row was removed rather than tombstoned"
        );
        match s.verify_chain().unwrap() {
            ChainStatus::Intact { redacted, .. } => assert_eq!(redacted, 1),
            other => panic!("a tombstone broke the chain: {other:?}"),
        }
        let payload: String = s
            .conn
            .query_row(
                "SELECT payload FROM world_events WHERE generation = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            payload.starts_with("[REDACTED:"),
            "the tombstone does not say what happened"
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn resolution_distinguishes_compacted_from_never_observed() {
        // The distinction an incident investigator most needs, and the one a
        // two-valued Option would have destroyed.
        let a = Resolution::DegradedSummary {
            citation_generation: 1,
            lo: 1,
            hi: 2,
            range_digest: "d".into(),
        };
        assert_ne!(a, Resolution::Unknown);
    }
}
