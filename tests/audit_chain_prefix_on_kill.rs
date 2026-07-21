// tests/audit_chain_prefix_on_kill.rs (renamed from audit_powerloss.rs, EP-23)
//! Gate C criterion #2 — audit-chain **crash-consistency** + un-fsynced-tail loss.
//!
//! The SHA-256 hash-chained audit ledger lives in a WAL-mode SQLite DB; each entry
//! is appended inside an `Immediate` transaction (`audit_tx`, #685) so an append is
//! atomic. The safety property that matters is: whatever remains on disk after an
//! abrupt stop is always a valid, verifiable PREFIX — never a torn or forked chain.
//!
//! #1046 (test honesty) — this drill has TWO tiers, because they exercise DIFFERENT
//! failure modes and the difference is load-bearing:
//!
//!   1. `audit_chain_survives_sigkill_mid_append` — **crash-consistency** under
//!      process death. `SIGKILL` terminates the writer with no chance to flush or
//!      run destructors, but it leaves the OS PAGE CACHE intact, so the committed
//!      WAL survives the process even at `synchronous=NORMAL`. This proves WAL
//!      crash-consistency (no torn/forked chain across a mid-append kill) — a real,
//!      valuable property — but it does NOT exercise the fsync/hard-power-loss path,
//!      because nothing un-fsynced is ever actually dropped. (The earlier claim that
//!      SIGKILL is "the honest power-loss analogue" overstated it: a power cut also
//!      loses the page cache; SIGKILL does not.)
//!
//!   2. `audit_chain_is_valid_prefix_after_unfsynced_wal_tail_is_lost` — the
//!      **hard-power-loss** path, reproduced PORTABLY (no dm-flakey / custom VFS):
//!      a durable prefix is checkpointed into the main DB file, more entries are
//!      appended that live only in the un-fsynced WAL, and then the WAL is DROPPED —
//!      exactly the bytes a power cut loses when `synchronous=NORMAL` never fsynced
//!      them. Recovery must still yield a valid PREFIX (the durable entries), with
//!      the un-fsynced tail cleanly gone — never a torn chain. This is the tier that
//!      actually backs the "audit tail is checkpoint-bounded, never corrupt" claim
//!      in `VerifierStore::durable_checkpoint`'s #74 durability-boundary note.
//!
//! Unix-only (uses `SIGKILL` via `Child::kill`); CI runs on Linux.
#![cfg(unix)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kirra_ota_campaign::Campaign;
use kirra_verifier::verifier_store::VerifierStore;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WRITER_ENV: &str = "KIRRA_AUDIT_CRASH_DB";

/// One chained audit entry per campaign insert (deterministic, wall-clock-free).
fn mk_campaign(i: u64) -> Campaign {
    Campaign::new(
        format!("c-{i}"),
        DIGEST,
        "v1",
        vec!["c".into()],
        vec![100],
        i,
    )
    .expect("build campaign")
}

/// The crash-writer must have committed at least this many chained entries
/// before we SIGKILL it, so the kill is guaranteed to land WHILE it is actively
/// appending — regardless of how long the reexec child takes to start. Well
/// within a healthy burst (the writer commits hundreds per 100 ms when warm).
/// This replaces a fixed pre-kill `sleep`, which raced the reexec-child startup
/// and flaked to 0 committed entries on a loaded runner (#894 CI).
const READY_THRESHOLD: u64 = 50;
/// Poll cadence while waiting for that burst, and its ceiling. `5 ms × 600 ≈ 3 s`
/// — generous headroom on any real runner; exceeding it means the writer is
/// genuinely not progressing (a real regression), which fails LOUDLY rather
/// than hanging.
const READY_POLL_STEP: Duration = Duration::from_millis(5);
const MAX_READY_POLLS: u32 = 600;

static UNIQ: AtomicU64 = AtomicU64::new(0);

/// A transient SQLite BUSY/LOCKED — expected when the readiness poll's
/// `COUNT(*)` races the crash-writer's `Immediate` write transactions (rare in
/// WAL mode, but possible during a checkpoint). Treated as "retry", distinct
/// from a real query error (which must fail loudly, not be swallowed as 0).
fn is_transient_lock(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(
                err.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn temp_db(tag: &str) -> std::path::PathBuf {
    // Wall-clock-free unique name: pid + a process-local counter.
    let n = UNIQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kirra_powerloss_{}_{}_{}.sqlite",
        std::process::id(),
        tag,
        n
    ))
}

fn cleanup(db: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        std::fs::remove_file(std::path::PathBuf::from(p)).ok();
    }
}

/// The crash-writer ENTRYPOINT (reexec pattern — no product bin surface). When the
/// parent spawns this test binary with `KIRRA_AUDIT_CRASH_DB` set, this "test"
/// opens that DB and appends audit-chained entries (one `Immediate`-tx audit append
/// per `insert_campaign`) in a tight loop FOREVER, until the parent SIGKILLs it. In a
/// normal test run the env var is unset and this is an instant no-op.
#[test]
fn crash_consistency_writer_child() {
    let Ok(db) = std::env::var(WRITER_ENV) else {
        return; // normal run — not the child
    };
    let mut store = VerifierStore::new(&db).expect("child: open store");
    let mut i: u64 = 0;
    loop {
        // Each insert appends exactly one chained audit entry in one atomic tx.
        let _ = store.insert_campaign(&mk_campaign(i)); // a mid-commit SIGKILL just drops this one
        i += 1;
    }
}

/// Spawn the crash-writer, WAIT until it has committed a healthy burst (polled via
/// an observer connection — not a fixed sleep, so the kill can't race the reexec
/// child's startup), add a small per-trial jitter, SIGKILL it mid-append, then
/// reopen the DB from the file and assert the chain verifies INTACT with entries
/// that survived. Repeated across several jitter offsets so the kill lands at
/// different points relative to a commit boundary.
#[test]
fn audit_chain_survives_sigkill_mid_append() {
    let self_exe = std::env::current_exe().expect("current exe");

    for trial in 0..6u64 {
        let db = temp_db(&format!("kill{trial}"));
        cleanup(&db); // fresh

        // Pre-create the schema via an OBSERVER connection BEFORE spawning the
        // child: the child then opens an already-schema'd DB (no schema-creation
        // race), and this read connection can poll the child's commit progress.
        // WAL mode admits one writer + concurrent readers, so it sees the commits.
        let observer = VerifierStore::new(db.to_str().unwrap()).expect("open observer");

        let mut child = Command::new(&self_exe)
            .args(["--exact", "--nocapture", "crash_consistency_writer_child"])
            .env(WRITER_ENV, &db)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash-writer");

        // Wait until the child has committed a healthy burst, THEN kill — so the
        // SIGKILL is guaranteed to land mid-append INDEPENDENT of reexec-startup
        // latency (the old fixed `sleep` raced it and flaked to 0 committed under
        // load). Bounded by MAX_READY_POLLS so a genuinely stuck writer fails
        // LOUDLY rather than hanging.
        let mut committed = 0u64;
        let mut polls = 0u32;
        while committed < READY_THRESHOLD {
            assert!(
                polls < MAX_READY_POLLS,
                "trial {trial}: the crash-writer never committed {READY_THRESHOLD} entries \
                 ({committed} seen after {polls} polls) — writer not making progress"
            );
            std::thread::sleep(READY_POLL_STEP);
            // `audit_chain_len` is the light `COUNT(*)` — we only need the row
            // count, NOT the full chain re-verification (signature/keyring walk)
            // `verify_audit_chain_full` runs (review: Copilot #899). A transient
            // BUSY/LOCKED under the writer's load is expected → keep the last
            // count and retry; ANY OTHER error is a real failure and must NOT be
            // masked as "0 committed" (which would loop to the misleading
            // "writer not making progress" assertion) — surface it loudly.
            committed = match observer.audit_chain_len() {
                Ok(n) => n,
                Err(e) if is_transient_lock(&e) => committed,
                Err(e) => panic!("trial {trial}: audit_chain_len query failed unexpectedly: {e}"),
            };
            polls += 1;
        }
        // Small per-trial jitter so the kill lands at varied offsets PAST the
        // readiness point (preserving the original "different points relative to a
        // commit boundary" coverage), without depending on absolute startup time.
        std::thread::sleep(Duration::from_millis(trial * 7));
        drop(observer); // release the reader before the crash-reopen (cold-boot analogue)

        child.kill().expect("SIGKILL the writer"); // abrupt process death (page cache survives)
        let _ = child.wait();

        // Reopen from the file — SQLite's WAL recovery runs, as it would on a cold
        // boot. (The un-fsynced-tail LOSS a real power cut also causes is exercised
        // separately by the WAL-drop test below; SIGKILL keeps the page cache, so
        // here nothing un-fsynced is actually lost.)
        let store = VerifierStore::new(db.to_str().unwrap()).expect("reopen after kill");

        assert!(
            store
                .verify_audit_chain_integrity()
                .expect("integrity query"),
            "trial {trial}: audit chain must be INTACT after a SIGKILL mid-append"
        );
        let full = store
            .verify_audit_chain_full(None)
            .expect("full verify query");
        assert!(
            full.chain_intact,
            "trial {trial}: full verify must report chain_intact after crash"
        );
        assert!(
            full.total_entries >= 1,
            "trial {trial}: committed entries must survive the crash (got {})",
            full.total_entries
        );

        drop(store);
        cleanup(&db);
    }
}

/// Committed audit entries are durable across a clean reopen: append a batch, drop
/// the store (close the connection), reopen from the file, and confirm the chain
/// verifies and the entries are all present. The reopen-path companion to the crash
/// test above.
#[test]
fn committed_audit_chain_reverifies_after_reopen() {
    let db = temp_db("reopen");
    cleanup(&db);
    const N: u64 = 25;

    let before = {
        let mut store = VerifierStore::new(db.to_str().unwrap()).expect("open");
        for i in 0..N {
            let c = Campaign::new(
                format!("c-{i}"),
                DIGEST,
                "v1",
                vec!["c".into()],
                vec![100],
                i,
            )
            .expect("build campaign");
            store.insert_campaign(&c).expect("insert");
        }
        store
            .verify_audit_chain_full(None)
            .expect("verify")
            .total_entries
        // store dropped here → connection closed
    };
    assert!(before >= N, "expected at least {N} entries, got {before}");

    // Reopen from disk and re-verify — nothing was held in memory.
    let store = VerifierStore::new(db.to_str().unwrap()).expect("reopen");
    let full = store
        .verify_audit_chain_full(None)
        .expect("verify after reopen");
    assert!(full.chain_intact, "chain must verify after a clean reopen");
    assert_eq!(
        full.total_entries, before,
        "every committed entry must survive the reopen"
    );

    drop(store);
    cleanup(&db);
}

/// The HARD-POWER-LOSS tier (#1046): un-fsynced WAL frames are DROPPED, and the
/// chain that recovers must still be a valid, verifiable PREFIX — never torn.
///
/// SIGKILL (above) cannot exercise this: it leaves the OS page cache intact, so
/// the committed-but-un-fsynced WAL survives the process. A real power cut also
/// loses the page cache. We reproduce that PORTABLY (no dm-flakey / custom VFS):
///
///   1. append a durable PREFIX and force `durable_checkpoint()`
///      (`wal_checkpoint(TRUNCATE)`, `synchronous=FULL`) — those rows are now
///      fsynced into the MAIN db file, the part a power cut keeps;
///   2. append MORE entries that, under `synchronous=NORMAL`, commit only into the
///      WAL and are NEVER fsynced — the part a power cut loses;
///   3. copy ONLY the main db file (no `-wal`) — the exact on-disk state a power
///      cut freezes once the un-fsynced WAL pages evaporate;
///   4. reopen the copy: SQLite recovery sees the durable prefix alone.
///
/// The recovered chain must be INTACT and equal to the durable prefix, and the
/// original (whose tail we did NOT drop) must hold strictly more — proving the
/// drop was real, so the test can never pass vacuously.
#[test]
fn audit_chain_is_valid_prefix_after_unfsynced_wal_tail_is_lost() {
    const DURABLE: u64 = 20; // checkpointed → fsynced into main.db
    const TAIL: u64 = 30; // WAL-only, un-fsynced → lost on power cut

    let db = temp_db("waldrop");
    let db_copy = temp_db("waldrop_copy");
    cleanup(&db);
    cleanup(&db_copy);

    let durable_count = {
        let mut store = VerifierStore::new(db.to_str().unwrap()).expect("open");

        // (1) Durable prefix, fsynced into the MAIN db file by a TRUNCATE checkpoint.
        for i in 0..DURABLE {
            store
                .insert_campaign(&mk_campaign(i))
                .expect("insert durable");
        }
        store
            .durable_checkpoint()
            .expect("checkpoint the durable prefix into main.db");
        let durable_count = store
            .verify_audit_chain_full(None)
            .expect("verify durable prefix")
            .total_entries;
        assert!(durable_count >= 1, "a durable prefix must exist");

        // (2) Un-fsynced tail — commits into the WAL only (synchronous=NORMAL), so
        //     main.db is untouched (TAIL is far below the 1000-page auto-checkpoint).
        for i in DURABLE..(DURABLE + TAIL) {
            store.insert_campaign(&mk_campaign(i)).expect("insert tail");
        }

        // (3) Snapshot ONLY the main db file — NOT the `-wal`. This is the durable
        //     state a hard power cut leaves behind once the un-fsynced WAL is gone.
        std::fs::copy(&db, &db_copy).expect("snapshot main.db (without its -wal)");

        durable_count
        // store dropped here → clean close checkpoints the tail into the ORIGINAL
        // db (so the original becomes the "tail survived" control below).
    };

    // (4) Reopen the WAL-less copy: recovery yields the durable PREFIX alone.
    let recovered = VerifierStore::new(db_copy.to_str().unwrap()).expect("reopen power-loss copy");
    let recovered_full = recovered
        .verify_audit_chain_full(None)
        .expect("verify recovered prefix");
    assert!(
        recovered_full.chain_intact,
        "after an un-fsynced WAL tail is lost, the recovered chain must be an INTACT prefix — never torn/forked"
    );
    assert_eq!(
        recovered_full.total_entries, durable_count,
        "recovery must yield exactly the checkpointed durable prefix ({durable_count}); \
         the un-fsynced tail must be gone"
    );

    // Control (non-vacuity): the ORIGINAL, whose tail we did NOT drop, holds
    // strictly more — proving the WAL really carried the tail we discarded.
    let with_wal = VerifierStore::new(db.to_str().unwrap()).expect("reopen original");
    let with_wal_full = with_wal
        .verify_audit_chain_full(None)
        .expect("verify original");
    assert!(
        with_wal_full.chain_intact,
        "the original chain (tail retained) must also be intact"
    );
    assert!(
        with_wal_full.total_entries > recovered_full.total_entries,
        "the dropped WAL must have carried real entries (original {} > recovered {}) — \
         else the test proves nothing",
        with_wal_full.total_entries,
        recovered_full.total_entries
    );

    drop(recovered);
    drop(with_wal);
    cleanup(&db);
    cleanup(&db_copy);
}
