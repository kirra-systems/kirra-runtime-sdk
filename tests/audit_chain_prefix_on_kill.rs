// tests/audit_chain_prefix_on_kill.rs (renamed from audit_powerloss.rs, EP-23)
//! Gate C criterion #2 — "audit chain survives a power-loss test."
//!
//! The SHA-256 hash-chained audit ledger lives in a WAL-mode SQLite DB; each entry
//! is appended inside an `Immediate` transaction (`audit_tx`, #685) so an append is
//! atomic. This drill proves the SAFETY property that matters under power loss: after
//! an ABRUPT kill mid-append, the chain that remains on disk is always a valid,
//! verifiable PREFIX — never a torn or forked chain. (Under WAL + `synchronous=NORMAL`
//! the very last uncheckpointed entries can be lost on power loss, but the chain is
//! never left broken — which is exactly the invariant a hash chain needs.)
//!
//! `SIGKILL` is the honest power-loss analogue: the process is terminated with no
//! chance to flush or run destructors, and SQLite's WAL recovery runs when the file
//! is reopened — the same recovery a cold boot after a power cut performs.
//!
//! Unix-only (uses `SIGKILL` via `Child::kill`); CI runs on Linux.
#![cfg(unix)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kirra_verifier::ota_campaign::Campaign;
use kirra_verifier::verifier_store::VerifierStore;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WRITER_ENV: &str = "KIRRA_AUDIT_POWERLOSS_DB";

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
/// parent spawns this test binary with `KIRRA_AUDIT_POWERLOSS_DB` set, this "test"
/// opens that DB and appends audit-chained entries (one `Immediate`-tx audit append
/// per `insert_campaign`) in a tight loop FOREVER, until the parent SIGKILLs it. In a
/// normal test run the env var is unset and this is an instant no-op.
#[test]
fn powerloss_writer_child() {
    let Ok(db) = std::env::var(WRITER_ENV) else {
        return; // normal run — not the child
    };
    let mut store = VerifierStore::new(&db).expect("child: open store");
    let mut i: u64 = 0;
    loop {
        // Each insert appends exactly one chained audit entry in one atomic tx.
        let c = Campaign::new(
            format!("crash-{i}"),
            DIGEST,
            "v1",
            vec!["c".into()],
            vec![100],
            i,
        )
        .expect("build campaign");
        let _ = store.insert_campaign(&c); // a mid-commit SIGKILL just drops this one
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
            .args(["--exact", "--nocapture", "powerloss_writer_child"])
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
            committed = observer
                .verify_audit_chain_full(None)
                .map(|f| f.total_entries)
                .unwrap_or(0); // a transient SQLITE_BUSY under the write load → retry
            polls += 1;
        }
        // Small per-trial jitter so the kill lands at varied offsets PAST the
        // readiness point (preserving the original "different points relative to a
        // commit boundary" coverage), without depending on absolute startup time.
        std::thread::sleep(Duration::from_millis(trial * 7));
        drop(observer); // release the reader before the crash-reopen (cold-boot analogue)

        child.kill().expect("SIGKILL the writer"); // abrupt termination == power loss
        let _ = child.wait();

        // Reopen from the file — this is where SQLite's WAL recovery runs, exactly as
        // it would on a cold boot after a power cut.
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
