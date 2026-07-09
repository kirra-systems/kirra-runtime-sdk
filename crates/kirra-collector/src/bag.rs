// crates/kirra-collector/src/bag.rs
//
// The bus side of the join. The collector matches each capture record to a bus
// message (the doer's proposal/trajectory + perception + a doer-version stamp)
// recorded during the same bench run, then references the heavy frames by
// `bulk_ref` rather than copying them (docs/COLLECTOR_DESIGN.md [D2]/[D4]).
//
// [C2] The real bag backend (rosbag2 sqlite3 `.db3` vs MCAP) is DEFERRED until
// the first AWSIM/Autoware session is recorded — the GPU bench isn't up. So bag
// access sits behind the `BagReader` trait; Phase 1 ships an in-memory /
// JSON-backed synthetic impl that exercises the whole join → Parquet →
// reconciliation pipeline with no real bag. A `Db3BagReader` / `McapBagReader`
// slots in later without touching the join or dataset code.

use serde::{Deserialize, Serialize};

/// One bus message the collector can join against. The heavy payload (full
/// trajectory points, object lists, sensor frames) is NOT here — it stays in the
/// bag and is referenced by `bulk_ref`. These are the fields the join needs:
/// the time stamp, the doer version [D3], the cross-check keys [D2], and the
/// reference back into the bag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    /// Wall-clock ms of the bus message (the join axis against `t_wall_ms`).
    pub t_wall_ms: u64,
    /// The doer's model/config version, stamped on the bus side [D3]. Kirra is
    /// ignorant of this; the collector attaches it here.
    pub doer_version: String,
    /// Cross-check key [D2] — the asset id, where the bus message carries one.
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Cross-check key [D2] — the trajectory id, where present.
    #[serde(default)]
    pub trajectory_id: Option<u64>,
    /// Cross-check key [D2] — the objects-snapshot freshness stamp, where present.
    #[serde(default)]
    pub objects_ms: Option<u64>,
    /// Reference into the bag for the heavy frames (uri#offset / topic@stamp).
    /// This is the `bulk_ref` written into the Parquet row — never the frames.
    pub bulk_ref: String,
}

/// The result of a successful join: what the collector attaches to the record.
#[derive(Debug, Clone, PartialEq)]
pub struct BusMatch {
    pub doer_version: String,
    pub bulk_ref: String,
    pub matched_t_wall_ms: u64,
}

/// The bus-recording abstraction. The real implementations (db3/MCAP) are a
/// Phase-1 follow-up [C2]; everything upstream depends only on this trait.
pub trait BagReader {
    /// Provenance — the bag's uri (recorded into `bulk_ref` provenance / logs).
    fn bag_uri(&self) -> &str;
    /// All bus messages whose stamp falls within `±window_ms` of `t_wall_ms`.
    /// Order is not guaranteed; the join picks the best candidate.
    fn messages_in_window(&self, t_wall_ms: u64, window_ms: u64) -> Vec<BusMessage>;
}

/// In-memory synthetic bag — Phase 1 tests + offline runs. Loadable from a JSON
/// array of `BusMessage` (the `--bag-json` path) so the binary is runnable
/// end-to-end with no real recording.
pub struct InMemoryBag {
    uri: String,
    messages: Vec<BusMessage>,
}

impl InMemoryBag {
    #[must_use]
    pub fn new(uri: impl Into<String>, messages: Vec<BusMessage>) -> Self {
        Self {
            uri: uri.into(),
            messages,
        }
    }

    /// Load from a JSON file containing an array of `BusMessage`.
    pub fn from_json_file(path: &std::path::Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let messages: Vec<BusMessage> = serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self::new(path.display().to_string(), messages))
    }
}

impl BagReader for InMemoryBag {
    fn bag_uri(&self) -> &str {
        &self.uri
    }

    fn messages_in_window(&self, t_wall_ms: u64, window_ms: u64) -> Vec<BusMessage> {
        let lo = t_wall_ms.saturating_sub(window_ms);
        let hi = t_wall_ms.saturating_add(window_ms);
        self.messages
            .iter()
            .filter(|m| m.t_wall_ms >= lo && m.t_wall_ms <= hi)
            .cloned()
            .collect()
    }
}

/// Errors from opening a real rosbag2 bag.
#[derive(Debug)]
pub enum BagError {
    /// Failed to open the sqlite file or run the topic-resolution query (db3).
    Open(rusqlite::Error),
    /// Failed to open or read an MCAP bag.
    Mcap(String),
    /// The requested join topic is not present in the bag.
    TopicNotFound(String),
}

impl std::fmt::Display for BagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BagError::Open(e) => write!(f, "opening rosbag2 db3: {e}"),
            BagError::Mcap(e) => write!(f, "opening mcap: {e}"),
            BagError::TopicNotFound(t) => write!(f, "join topic not found in bag: {t}"),
        }
    }
}

impl std::error::Error for BagError {}

/// Real rosbag2 sqlite3 (`.db3`) backend [C2].
///
/// This reads the bag's `topics`/`messages` tables and indexes the join topic
/// by message timestamp — it does NOT deserialize the CDR payloads
/// (docs/COLLECTOR_DESIGN.md [D4]: reference the heavy frames, never copy or
/// decode them). Consequently it populates `t_wall_ms`, the run's `doer_version`
/// [D3], and a `bulk_ref` (`uri#topic@stamp_ns`); the [D2] cross-check keys
/// (`asset_id`/`trajectory_id`/`objects_ms`) are left `None`, which the join
/// treats as "not asserted" (it never vetoes — see `join::keys_agree`). Richer
/// cross-check stamping requires the doer to emit a structured bus-index topic,
/// deferred to [C2b]. MCAP is a separate follow-up.
pub struct Db3BagReader {
    uri: String,
    topic: String,
    conn: rusqlite::Connection,
    topic_id: i64,
    doer_version: String,
}

impl Db3BagReader {
    /// Open a rosbag2 `.db3` read-only and resolve the join `topic` to its id.
    /// `doer_version` is supplied out-of-band (one recording == one doer build);
    /// it can't be read without decoding the payload, which we deliberately don't.
    pub fn open(
        path: &std::path::Path,
        topic: &str,
        doer_version: impl Into<String>,
    ) -> Result<Self, BagError> {
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(BagError::Open)?;
        let topic_id: i64 = conn
            .query_row("SELECT id FROM topics WHERE name = ?1", [topic], |r| {
                r.get(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => BagError::TopicNotFound(topic.to_string()),
                other => BagError::Open(other),
            })?;
        Ok(Self {
            uri: path.display().to_string(),
            topic: topic.to_string(),
            conn,
            topic_id,
            doer_version: doer_version.into(),
        })
    }
}

impl BagReader for Db3BagReader {
    fn bag_uri(&self) -> &str {
        &self.uri
    }

    fn messages_in_window(&self, t_wall_ms: u64, window_ms: u64) -> Vec<BusMessage> {
        // rosbag2 stamps are nanoseconds; the join window is milliseconds. Mirror
        // InMemoryBag's inclusive [t-window, t+window] ms window (the +999_999
        // covers the whole upper millisecond).
        let lo_ns = t_wall_ms
            .saturating_sub(window_ms)
            .saturating_mul(1_000_000);
        let hi_ns = t_wall_ms
            .saturating_add(window_ms)
            .saturating_mul(1_000_000)
            .saturating_add(999_999);
        let mut stmt = match self.conn.prepare(
            "SELECT timestamp FROM messages WHERE topic_id = ?1 AND timestamp BETWEEN ?2 AND ?3",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        // The trait signature is infallible (Vec); a query error degrades to an
        // empty window -> the record becomes an ORPHAN, surfaced by reconciliation
        // rather than silently lost. `open()` already validated topic + handle.
        let rows = stmt.query_map(
            rusqlite::params![self.topic_id, lo_ns as i64, hi_ns as i64],
            |r| r.get::<_, i64>(0),
        );
        let Ok(rows) = rows else { return Vec::new() };
        rows.flatten()
            .map(|ts| {
                let ts_ns = ts as u64;
                BusMessage {
                    t_wall_ms: ts_ns / 1_000_000,
                    doer_version: self.doer_version.clone(),
                    asset_id: None,
                    trajectory_id: None,
                    objects_ms: None,
                    bulk_ref: format!("{}#{}@{}", self.uri, self.topic, ts_ns),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod db3_tests {
    use super::*;

    // Build a faithful synthetic rosbag2 db3 (core schema + the extra Jazzy
    // `type_description_hash` column, to prove the reader doesn't depend on it).
    fn make_bag(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE topics(
                id INTEGER PRIMARY KEY, name TEXT NOT NULL, type TEXT NOT NULL,
                serialization_format TEXT NOT NULL, offered_qos_profiles TEXT NOT NULL,
                type_description_hash TEXT NOT NULL);
             CREATE TABLE messages(
                id INTEGER PRIMARY KEY, topic_id INTEGER NOT NULL,
                timestamp INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE INDEX timestamp_idx ON messages(timestamp ASC);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (1,'/planning/trajectory','autoware_planning_msgs/msg/Trajectory','cdr','','h1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (2,'/perception/objects','autoware_perception_msgs/msg/PredictedObjects','cdr','','h2')",
            [],
        ).unwrap();
        let payload: &[u8] = &[0xAA, 0xBB]; // opaque CDR — the reader must never decode it
        for (id, ts) in [
            (1i64, 1_000_000_000i64),
            (2, 1_010_000_000),
            (3, 1_050_000_000),
            (4, 2_000_000_000),
        ] {
            conn.execute(
                "INSERT INTO messages (id, topic_id, timestamp, data) VALUES (?1, 1, ?2, ?3)",
                rusqlite::params![id, ts, payload],
            )
            .unwrap();
        }
        // a message on the OTHER topic must never leak into the trajectory query
        conn.execute(
            "INSERT INTO messages (id, topic_id, timestamp, data) VALUES (99, 2, 1005000000, ?1)",
            rusqlite::params![payload],
        )
        .unwrap();
    }

    fn tmp_db() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("kirra_db3_{}_{}.db3", std::process::id(), nanos))
    }

    #[test]
    fn window_math_matches_inmemory_semantics() {
        let db = tmp_db();
        make_bag(&db);
        let r = Db3BagReader::open(&db, "/planning/trajectory", "doer-v1.2.3").unwrap();

        // ±100ms around 1000ms == [900,1100] -> 1000,1010,1050 (NOT 2000)
        let mut ms: Vec<u64> = r
            .messages_in_window(1000, 100)
            .iter()
            .map(|m| m.t_wall_ms)
            .collect();
        ms.sort_unstable();
        assert_eq!(ms, vec![1000, 1010, 1050]);

        // ±30ms around 1050 -> just 1050
        let ms2: Vec<u64> = r
            .messages_in_window(1050, 30)
            .iter()
            .map(|m| m.t_wall_ms)
            .collect();
        assert_eq!(ms2, vec![1050]);

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn stamps_version_refs_topic_and_isolates_topic() {
        let db = tmp_db();
        make_bag(&db);
        let r = Db3BagReader::open(&db, "/planning/trajectory", "doer-v1.2.3").unwrap();

        let all = r.messages_in_window(1005, 1000); // huge window
        assert_eq!(
            all.len(),
            4,
            "only the 4 trajectory msgs; the perception topic is excluded"
        );
        assert!(
            all.iter().all(|m| m.doer_version == "doer-v1.2.3"),
            "doer_version stamped [D3]"
        );
        assert!(
            all.iter().all(|m| m.asset_id.is_none() && m.trajectory_id.is_none() && m.objects_ms.is_none()),
            "cross-check keys left None (no CDR decode); join treats None as not-asserted [D2]/[D4]"
        );
        assert!(all
            .iter()
            .all(|m| m.bulk_ref.contains("#/planning/trajectory@")));

        let exact = r.messages_in_window(1000, 5);
        assert_eq!(exact.len(), 1);
        assert_eq!(
            exact[0].bulk_ref,
            format!("{}#/planning/trajectory@1000000000", db.display())
        );
        assert_eq!(r.bag_uri(), db.display().to_string());

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn topic_not_found_is_a_clean_error() {
        let db = tmp_db();
        make_bag(&db);
        assert!(matches!(
            Db3BagReader::open(&db, "/does/not/exist", "v"),
            Err(BagError::TopicNotFound(_))
        ));
        let _ = std::fs::remove_file(&db);
    }

    // The real reason this backend is valid as a first cut: a record joins
    // against it through the same join path, using time + version + bulk_ref.
    #[test]
    fn joins_through_the_real_join_path() {
        use crate::join::{join_record, JoinOutcome};
        use kirra_capture_schema::{CaptureOutcome, CaptureRecord, CaptureSource};

        let db = tmp_db();
        make_bag(&db);
        let r = Db3BagReader::open(&db, "/planning/trajectory", "doer-v1.2.3").unwrap();

        let rec = CaptureRecord {
            decision_seq: 0,
            t_mono_ns: 0,
            t_wall_ms: 1008, // within ±100ms of the 1000/1010 stamps
            source: CaptureSource::CommandGateway,
            proposed: None,
            traj: None,
            outcome: CaptureOutcome::Allow,
            deny_code: None,
            safe_value: None,
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        };
        let JoinOutcome::Joined(m) = join_record(&rec, &r, 100) else {
            panic!("expected a join against the db3 bag");
        };
        assert_eq!(m.doer_version, "doer-v1.2.3");
        assert_eq!(
            m.matched_t_wall_ms, 1010,
            "nearest-in-time to 1008 is the 1010 stamp"
        );
        assert!(m.bulk_ref.ends_with("@1010000000"));

        let _ = std::fs::remove_file(&db);
    }
}

/// Real rosbag2 MCAP (`.mcap`) backend [C2] — the sibling of [`Db3BagReader`].
///
/// rosbag2's other storage format (increasingly the default). Like the db3
/// reader it does NOT decode CDR payloads ([D4]); on open it pre-indexes the
/// join topic's message `log_time`s (sorted) and references heavy frames via
/// `bulk_ref` (`uri#topic@log_time_ns`). It populates `t_wall_ms` +
/// `doer_version` [D3] + `bulk_ref`; the [D2] cross-check keys are left `None`
/// (the join treats `None` as not-asserted). Reading compressed chunks relies on
/// the `mcap` crate's lz4/zstd features (enabled in Cargo.toml); the reader code
/// is identical whether or not chunks are compressed.
pub struct McapBagReader {
    uri: String,
    topic: String,
    doer_version: String,
    stamps_ns: Vec<u64>,
}

impl McapBagReader {
    /// Open an `.mcap`, validate the join `topic`, and pre-index its message
    /// `log_time`s. `doer_version` is supplied out-of-band (see `Db3BagReader`).
    pub fn open(
        path: &std::path::Path,
        topic: &str,
        doer_version: impl Into<String>,
    ) -> Result<Self, BagError> {
        let bytes = std::fs::read(path).map_err(|e| BagError::Mcap(e.to_string()))?;

        // Validate the topic via the summary's channel list when present.
        let mut topic_known = false;
        if let Ok(Some(summary)) = mcap::Summary::read(&bytes) {
            topic_known = summary.channels.values().any(|c| c.topic == topic);
            if !summary.channels.is_empty() && !topic_known {
                return Err(BagError::TopicNotFound(topic.to_string()));
            }
        }

        let mut stamps_ns = Vec::new();
        for msg in mcap::MessageStream::new(&bytes).map_err(|e| BagError::Mcap(e.to_string()))? {
            let m = msg.map_err(|e| BagError::Mcap(e.to_string()))?;
            if m.channel.topic == topic {
                topic_known = true;
                stamps_ns.push(m.log_time);
            }
        }
        if !topic_known {
            return Err(BagError::TopicNotFound(topic.to_string()));
        }
        stamps_ns.sort_unstable();

        Ok(Self {
            uri: path.display().to_string(),
            topic: topic.to_string(),
            doer_version: doer_version.into(),
            stamps_ns,
        })
    }
}

impl BagReader for McapBagReader {
    fn bag_uri(&self) -> &str {
        &self.uri
    }

    fn messages_in_window(&self, t_wall_ms: u64, window_ms: u64) -> Vec<BusMessage> {
        let lo_ns = t_wall_ms
            .saturating_sub(window_ms)
            .saturating_mul(1_000_000);
        let hi_ns = t_wall_ms
            .saturating_add(window_ms)
            .saturating_mul(1_000_000)
            .saturating_add(999_999);
        // stamps_ns is sorted: binary-search the lower bound, take the run.
        let start = self.stamps_ns.partition_point(|&ts| ts < lo_ns);
        self.stamps_ns[start..]
            .iter()
            .take_while(|&&ts| ts <= hi_ns)
            .map(|&ts_ns| BusMessage {
                t_wall_ms: ts_ns / 1_000_000,
                doer_version: self.doer_version.clone(),
                asset_id: None,
                trajectory_id: None,
                objects_ms: None,
                bulk_ref: format!("{}#{}@{}", self.uri, self.topic, ts_ns),
            })
            .collect()
    }
}

#[cfg(test)]
mod mcap_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn write_msg(w: &mut mcap::Writer<std::io::BufWriter<std::fs::File>>, chan_id: u16, ts: u64) {
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: chan_id,
                sequence: 0,
                log_time: ts,
                publish_time: ts,
            },
            &[0xAA, 0xBB], // opaque payload — the reader must never decode it
        )
        .unwrap();
    }

    fn make_bag(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut w = mcap::Writer::new(f).unwrap();
        let traj = mcap::Channel {
            topic: "/planning/trajectory".to_string(),
            schema: None,
            message_encoding: "cdr".to_string(),
            metadata: BTreeMap::new(),
        };
        let perc = mcap::Channel {
            topic: "/perception/objects".to_string(),
            schema: None,
            message_encoding: "cdr".to_string(),
            metadata: BTreeMap::new(),
        };
        let traj_id = w.add_channel(&traj).unwrap();
        let perc_id = w.add_channel(&perc).unwrap();
        for ts in [
            1_000_000_000u64,
            1_010_000_000,
            1_050_000_000,
            2_000_000_000,
        ] {
            write_msg(&mut w, traj_id, ts);
        }
        write_msg(&mut w, perc_id, 1_005_000_000); // other topic — must not leak
        w.finish().unwrap();
    }

    fn tmp_bag() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("kirra_mcap_{}_{}.mcap", std::process::id(), nanos))
    }

    #[test]
    fn window_math_matches_inmemory_semantics() {
        let p = tmp_bag();
        make_bag(&p);
        let r = McapBagReader::open(&p, "/planning/trajectory", "doer-v1.2.3").unwrap();
        let mut ms: Vec<u64> = r
            .messages_in_window(1000, 100)
            .iter()
            .map(|m| m.t_wall_ms)
            .collect();
        ms.sort_unstable();
        assert_eq!(ms, vec![1000, 1010, 1050]);
        let ms2: Vec<u64> = r
            .messages_in_window(1050, 30)
            .iter()
            .map(|m| m.t_wall_ms)
            .collect();
        assert_eq!(ms2, vec![1050]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn stamps_version_refs_topic_and_isolates_topic() {
        let p = tmp_bag();
        make_bag(&p);
        let r = McapBagReader::open(&p, "/planning/trajectory", "doer-v1.2.3").unwrap();
        let all = r.messages_in_window(1005, 1000);
        assert_eq!(
            all.len(),
            4,
            "only the 4 trajectory msgs; the perception topic is excluded"
        );
        assert!(all.iter().all(|m| m.doer_version == "doer-v1.2.3"));
        assert!(all
            .iter()
            .all(|m| m.asset_id.is_none() && m.trajectory_id.is_none() && m.objects_ms.is_none()));
        assert!(all
            .iter()
            .all(|m| m.bulk_ref.contains("#/planning/trajectory@")));
        let exact = r.messages_in_window(1000, 5);
        assert_eq!(exact.len(), 1);
        assert_eq!(
            exact[0].bulk_ref,
            format!("{}#/planning/trajectory@1000000000", p.display())
        );
        assert_eq!(r.bag_uri(), p.display().to_string());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn topic_not_found_is_a_clean_error() {
        let p = tmp_bag();
        make_bag(&p);
        assert!(matches!(
            McapBagReader::open(&p, "/does/not/exist", "v"),
            Err(BagError::TopicNotFound(_))
        ));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn joins_through_the_real_join_path() {
        use crate::join::{join_record, JoinOutcome};
        use kirra_capture_schema::{CaptureOutcome, CaptureRecord, CaptureSource};
        let p = tmp_bag();
        make_bag(&p);
        let r = McapBagReader::open(&p, "/planning/trajectory", "doer-v1.2.3").unwrap();
        let rec = CaptureRecord {
            decision_seq: 0,
            t_mono_ns: 0,
            t_wall_ms: 1008,
            source: CaptureSource::CommandGateway,
            proposed: None,
            traj: None,
            outcome: CaptureOutcome::Allow,
            deny_code: None,
            safe_value: None,
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        };
        let JoinOutcome::Joined(m) = join_record(&rec, &r, 100) else {
            panic!("expected a join against the mcap bag");
        };
        assert_eq!(m.doer_version, "doer-v1.2.3");
        assert_eq!(m.matched_t_wall_ms, 1010);
        assert!(m.bulk_ref.ends_with("@1010000000"));
        let _ = std::fs::remove_file(&p);
    }
}
