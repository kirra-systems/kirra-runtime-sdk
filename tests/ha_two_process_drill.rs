//! EP-02 (M2) — the TWO-PROCESS HA failover drill.
//!
//! `tests/ha_failover.rs` proves the epoch fence at the STORE level (two
//! `VerifierStore`s in one process). This drill closes the remaining gap the
//! DD review named: two REAL `kirra_verifier_service` processes against one
//! shared SQLite file, the Active killed with SIGKILL, and the standby's
//! detection → promotion → first-served-request measured over live HTTP —
//! the actual deployment topology, process boundaries and all.
//!
//! What is asserted:
//! 1. **Exactly one Active at all times** — sampled over the steady window
//!    (`kirra_mode_active` on both instances; never both 1), and the durable
//!    `ha_state` holder polled from the shared store.
//! 2. **Promotion within the documented bound** for the drill's timings: the
//!    legacy heartbeat path promotes at `PROMOTION_TIMEOUT` after the token
//!    stops advancing (+ poll granularity + promotion work). The measured
//!    kill→Active time is printed as the drill's output.
//! 3. **The promoted standby actually serves** — it answers `/fleet/posture`
//!    with the posture gate's own verdict (a fail-closed 503 on this EMPTY
//!    drill fleet: M-9 forces LockedOut with no registered nodes — the
//!    correct live decision, not a connection failure), measured as
//!    kill→first-served; and its posture cache stays FRESH past a full
//!    `POSTURE_CACHE_TTL_MS` — the H2 on-promote freshness rewiring, without
//!    which a promoted standby goes stale one TTL after promotion and
//!    fail-closes forever.
//!
//! `#[ignore]` by default: process orchestration + real timers make this a
//! poor fit for the ordinary `cargo test` sweep. CI runs it in the dedicated
//! serial `ha-two-process-drill` job (`-- --ignored`); locally:
//!
//! ```sh
//! cargo test --test ha_two_process_drill -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Drill timings — scaled down from the defaults (2 s heartbeat / 10 s
/// timeout) so the drill runs in seconds. The #689 floor requires
/// `timeout >= (MAX_CONSECUTIVE_HEARTBEAT_FAILURES + 1) x interval` = 800 ms
/// at a 200 ms interval; 1000 ms clears it without being clamped.
const HEARTBEAT_INTERVAL_MS: u64 = 200;
const PROMOTION_TIMEOUT_MS: u64 = 1_000;
const PROMOTION_POLL_MS: u64 = 100;

/// The documented promotion bound for these timings: the token must go stale
/// for `PROMOTION_TIMEOUT_MS` (anchored at the LAST observed advance, which is
/// at most one heartbeat + one poll before the kill), plus one poll to notice,
/// plus the promotion transaction itself. ~1.4 s expected; the assert allows a
/// generous CI-scheduling margin while still being far below the default-config
/// bound (~12 s), so a regression to "never promotes" or "promotes an order of
/// magnitude late" fails loudly.
const PROMOTION_BOUND_MS: u64 = 6_000;

/// Generous budget for process boot (compile-cold runners, fsync-heavy init).
const BOOT_BUDGET: Duration = Duration::from_secs(60);

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn kill_now(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

fn free_port() -> u16 {
    // Bind :0, take the assigned port, drop the listener. A tiny race window
    // exists before the child binds; acceptable for a serial CI job.
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

fn http_get(url: &str) -> Option<(u16, String)> {
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(900))
        // Hermetic: never route loopback probes through an ambient HTTP(S)_PROXY.
        .no_proxy()
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()?;
    let code = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    Some((code, body))
}

/// Parse `kirra_mode_active{...} <0|1>` from a /metrics scrape.
/// `None` = unreachable or series absent.
fn mode_active(port: u16) -> Option<bool> {
    let (code, body) = http_get(&format!("http://127.0.0.1:{port}/metrics"))?;
    if code != 200 {
        return None;
    }
    let line = body
        .lines()
        .find(|l| l.starts_with("kirra_mode_active") && !l.starts_with('#'))?;
    match line.rsplit(' ').next()? {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Parse `kirra_posture_cache_stale{...} <0|1>` from a /metrics scrape.
fn posture_cache_stale(port: u16) -> Option<bool> {
    let (code, body) = http_get(&format!("http://127.0.0.1:{port}/metrics"))?;
    if code != 200 {
        return None;
    }
    let line = body
        .lines()
        .find(|l| l.starts_with("kirra_posture_cache_stale") && !l.starts_with('#'))?;
    match line.rsplit(' ').next()? {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn ready(port: u16) -> bool {
    matches!(http_get(&format!("http://127.0.0.1:{port}/ready")), Some((200, _)))
}

/// Poll `f` every `poll` until it returns true or `budget` elapses.
fn wait_until(budget: Duration, poll: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < budget {
        if f() {
            return true;
        }
        std::thread::sleep(poll);
    }
    false
}

fn spawn_instance(
    name: &'static str,
    dir: &Path,
    db: &Path,
    port: u16,
    mode: &str,
    extra_env: &[(&str, String)],
) -> ChildGuard {
    let log = std::fs::File::create(dir.join(format!("{name}.log"))).expect("log file");
    let log_err = log.try_clone().expect("clone log handle");
    // env_clear: the drill must be hermetic — an ambient KIRRA_* var (CI, a
    // developer shell) silently changing HA timings would invalidate the
    // measurement. Everything the service needs is set explicitly.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kirra_verifier_service"));
    cmd.env_clear()
        .env("KIRRA_DB_PATH", db)
        .env("KIRRA_VERIFIER_ADDR", format!("127.0.0.1:{port}"))
        .env("KIRRA_VERIFIER_MODE", mode)
        .env("KIRRA_INSTANCE_ID", name)
        .env("KIRRA_ADMIN_TOKEN", "drill-admin-token")
        .env("KIRRA_VEHICLE_CLASS", "courier")
        .env("RUST_LOG", "info");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = cmd
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {name}: {e}"));
    ChildGuard { child }
}

fn log_tail(dir: &Path, name: &str) -> String {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(dir.join(format!("{name}.log"))) {
        let _ = f.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines.len().saturating_sub(40);
    lines[tail..].join("\n")
}

/// The durable single-writer record: `(epoch, holder)` from the shared store.
/// Opened via the WAL READ-REPLICA (SQLITE_OPEN_READ_ONLY): `VerifierStore::new`
/// would open a writer-capable connection and run PRAGMAs/DDL against the LIVE
/// shared DB — a third writer-style connection that could contend with the two
/// service processes and skew the drill's timing measurements. The replica open
/// is the store's own non-contending read path.
fn durable_holder(db: &Path) -> (u64, Option<String>) {
    let store = kirra_verifier::verifier_store::VerifierStore::open_read_replica(
        db.to_str().expect("utf8 db path"),
    )
    .expect("open read replica from drill");
    store.current_active_holder().expect("read ha_state")
}

fn drill_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("kirra_ha_drill_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create drill dir");
    dir
}

/// The legacy-path timing env (scaled down; see the constants above).
fn legacy_env() -> Vec<(&'static str, String)> {
    vec![
        ("KIRRA_HEARTBEAT_INTERVAL", HEARTBEAT_INTERVAL_MS.to_string()),
        ("KIRRA_PROMOTION_TIMEOUT", PROMOTION_TIMEOUT_MS.to_string()),
        ("KIRRA_PROMOTION_POLL", PROMOTION_POLL_MS.to_string()),
    ]
}

/// EP-03 gate-on env: the lease trigger at its DEFAULT TTL (the ≤5 s product
/// property is defined at the default), legacy timings left at THEIR defaults
/// (10 s timeout) so a promotion inside 5 s can only have come from the lease
/// path. Poll tightened so observation granularity doesn't eat the margin.
fn lease_env() -> Vec<(&'static str, String)> {
    vec![
        ("KIRRA_HA_LEASE_ENABLED", "1".to_string()),
        ("KIRRA_PROMOTION_POLL", "100".to_string()),
    ]
}

/// One full drill run: boot A (active) + B (standby) on `env`, steady-window
/// sample, SIGKILL A, measure promotion + first-served, check the durable
/// handover, and assert `bound_ms`. Returns (kill→promotion, kill→first-served).
fn run_drill(tag: &str, env: &[(&str, String)], steady_ms: u64, bound_ms: u64) -> (u64, u64) {
    let dir = drill_dir(tag);
    let db = dir.join("ha-shared.sqlite");
    let port_a = free_port();
    let port_b = free_port();

    // --- Phase 1: boot the Active, then the standby, against ONE db file ---
    let mut a = spawn_instance("drill-a", &dir, &db, port_a, "active", env);
    assert!(
        wait_until(BOOT_BUDGET, Duration::from_millis(100), || ready(port_a)),
        "instance A never became ready; log tail:\n{}",
        log_tail(&dir, "drill-a")
    );
    assert!(
        wait_until(Duration::from_secs(10), Duration::from_millis(100), || {
            mode_active(port_a) == Some(true)
        }),
        "instance A never reported Active; log tail:\n{}",
        log_tail(&dir, "drill-a")
    );

    let b = spawn_instance("drill-b", &dir, &db, port_b, "passive_standby", env);
    assert!(
        wait_until(BOOT_BUDGET, Duration::from_millis(100), || ready(port_b)),
        "instance B never became ready; log tail:\n{}",
        log_tail(&dir, "drill-b")
    );
    assert_eq!(
        mode_active(port_b),
        Some(false),
        "instance B must boot as PassiveStandby"
    );

    // The durable holder is the Active's identity.
    let (epoch_before, holder_before) = durable_holder(&db);
    assert_eq!(
        holder_before.as_deref(),
        Some("drill-a"),
        "the durable ha_state holder must be the Active instance"
    );

    // --- Phase 2: steady window — exactly one Active, no spurious promotion ---
    // Sampled for longer than the promotion deadline so a broken freshness
    // tracker (one that ignores the advancing liveness signals) would promote
    // INSIDE this window and fail here rather than pass silently.
    let steady_deadline = Instant::now() + Duration::from_millis(steady_ms);
    while Instant::now() < steady_deadline {
        let a_active = mode_active(port_a);
        let b_active = mode_active(port_b);
        assert_ne!(
            (a_active, b_active),
            (Some(true), Some(true)),
            "TWO ACTIVES observed in the steady window (split-brain)"
        );
        assert_ne!(
            b_active,
            Some(true),
            "standby promoted while the primary heartbeat was advancing (spurious failover)"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // --- Phase 3: kill the Active (SIGKILL — no graceful teardown) ---
    let killed_at = Instant::now();
    a.kill_now();

    // --- Phase 4: measure detection → promotion (mode flip over HTTP) ---
    assert!(
        wait_until(Duration::from_secs(30), Duration::from_millis(50), || {
            mode_active(port_b) == Some(true)
        }),
        "standby never promoted after the Active was killed; log tail:\n{}",
        log_tail(&dir, "drill-b")
    );
    let promoted_ms = killed_at.elapsed().as_millis() as u64;

    // --- Phase 5: first-served-request on the new Active ---
    // The drill fleet has NO registered nodes, so the correct live posture is
    // the M-9 fail-closed LockedOut — the gate answers reads with its own 503.
    // "Served" therefore means an HTTP answer from the posture gate (any
    // status), not a transport failure: the promoted node is up, routing, and
    // deciding. (A 200 would require a trusted, telemetry-fresh node — a
    // richer fixture than a failover drill needs.)
    assert!(
        wait_until(Duration::from_secs(10), Duration::from_millis(50), || {
            http_get(&format!("http://127.0.0.1:{port_b}/fleet/posture")).is_some()
        }),
        "promoted standby never answered a posture read; log tail:\n{}",
        log_tail(&dir, "drill-b")
    );
    let first_served_ms = killed_at.elapsed().as_millis() as u64;

    // --- Phase 5b: the promoted node's posture cache stays FRESH past one
    // full TTL (review H2: the on-promote hook must rewire the posture
    // freshness tasks; without it the cache goes stale one TTL after
    // promotion, the stale gauge flips to 1, and the node fail-closes until a
    // restart — the exact regression this assertion pins).
    std::thread::sleep(Duration::from_millis(
        kirra_verifier::posture_cache::POSTURE_CACHE_TTL_MS + 2_000,
    ));
    assert_eq!(
        mode_active(port_b),
        Some(true),
        "promoted node must still be Active one TTL after promotion (no self-fence); log tail:\n{}",
        log_tail(&dir, "drill-b")
    );
    assert_eq!(
        posture_cache_stale(port_b),
        Some(false),
        "promoted node's posture cache went STALE one TTL after promotion — the \
         on-promote freshness rewiring (review H2) is broken; log tail:\n{}",
        log_tail(&dir, "drill-b")
    );

    // --- Phase 6: the durable record shows the handover ---
    let (epoch_after, holder_after) = durable_holder(&db);
    assert_eq!(
        holder_after.as_deref(),
        Some("drill-b"),
        "the durable ha_state holder must be the promoted standby"
    );
    assert!(
        epoch_after > epoch_before,
        "promotion must advance the durable epoch (fence the dead primary): \
         before={epoch_before} after={epoch_after}"
    );

    // --- The measurement (the drill's OUTPUT) + the bound ---
    println!(
        "HA-DRILL-RESULT[{tag}]: kill→promotion {promoted_ms} ms, \
         kill→first-served {first_served_ms} ms (bound {bound_ms} ms)"
    );
    assert!(
        promoted_ms <= bound_ms,
        "promotion took {promoted_ms} ms — beyond the documented bound {bound_ms} ms \
         for the [{tag}] configuration"
    );

    drop(b);
    let _ = std::fs::remove_dir_all(&dir);
    (promoted_ms, first_served_ms)
}

/// The legacy heartbeat-timeout path (scaled timings; see the constants).
#[test]
#[ignore = "two-process drill (real processes + real timers); run by the dedicated CI job with -- --ignored"]
fn two_process_failover_drill() {
    run_drill(
        "legacy",
        &legacy_env(),
        PROMOTION_TIMEOUT_MS * 2,
        PROMOTION_BOUND_MS,
    );
}

/// EP-03 — the lease path at its DEFAULT TTL: the ≤5 s failover product
/// property, measured end to end. Legacy timings stay at their defaults
/// (10 s timeout), so a promotion inside 5 s proves the LEASE trigger fired
/// (reason `LEASE_EXPIRED` in the standby's log). The steady window exceeds
/// `promote_after_ms` so a live, renewing holder is never promoted over.
#[test]
#[ignore = "two-process drill (real processes + real timers); run by the dedicated CI job with -- --ignored"]
fn two_process_failover_drill_lease_gate_on() {
    let (promoted_ms, _) = run_drill(
        "lease",
        &lease_env(),
        6_000, // > promote_after (4.5 s): a spurious lease promotion fails HERE
        5_000, // THE product property: failover ≤ 5 s with the gate on
    );
    println!("HA-DRILL-RESULT[lease]: ≤5 s product property met ({promoted_ms} ms)");
}
