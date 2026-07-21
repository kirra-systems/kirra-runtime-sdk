//! #791 F3 — the TWO-PROCESS generation-restart drill.
//!
//! The lib-level generation tests prove `init_generation_from_store` WORKS;
//! the SG-008 `generation_initialized` boot fact proves the binary CALLED it.
//! This drill closes the loop end-to-end the way the review asked: spawn the
//! REAL `kirra_verifier_service` binary twice against one SQLite file and
//! prove, over live `/metrics` scrapes, that the posture generation does NOT
//! time-reverse across a restart — the exact WS-0.2 wiring omission that
//! silently regressed emitted generations (breaking the ordering federation
//! peers and SSE consumers rely on) until #G10 fixed it.
//!
//! Shape:
//! 1. Run 1: boot Active on a fresh DB; drive the generation up by
//!    registering nodes one at a time, each WAITED to its own observed
//!    recalc (the engine coalesces bursts, so sequencing — not counting —
//!    is what guarantees a floor on the generation).
//! 2. SIGKILL run 1 (no graceful shutdown — the counter must survive via the
//!    persisted high-water alone).
//! 3. Run 2: boot on the SAME DB, trigger ONE recalc, scrape.
//! 4. Assert run 2's generation is STRICTLY ABOVE run 1's peak. A binary
//!    that lost the seeding wiring restarts its counter at 1 and — with only
//!    one registration plus a couple of boot recalcs — cannot reach run 1's
//!    peak (≥ [`RUN1_REGISTRATIONS`] sequenced advances), so the assert is a
//!    real discriminator, not a tautology.
//!
//! `#[ignore]` by default (process orchestration + real timers); CI runs it
//! in the serial two-process-drill job. Locally:
//!
//! ```sh
//! cargo test --test generation_restart_drill -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Sequenced registrations in run 1 — each waited to an OBSERVED generation
/// advance, so run 1's peak is at least this many recalcs deep. Chosen to sit
/// far above what an unseeded run 2 can reach (a couple of boot recalcs plus
/// one registration), so the cross-restart assert discriminates.
const RUN1_REGISTRATIONS: usize = 8;

/// Generous budget for process boot (compile-cold runners, fsync-heavy init).
const BOOT_BUDGET: Duration = Duration::from_secs(60);
/// Budget for one observed generation advance after a registration.
const ADVANCE_BUDGET: Duration = Duration::from_secs(10);

const ADMIN_TOKEN: &str = "drill-admin-token";

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
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(900))
        // Hermetic: never route loopback probes through an ambient proxy.
        .no_proxy()
        .build()
        .expect("client")
}

fn ready(port: u16) -> bool {
    client()
        .get(format!("http://127.0.0.1:{port}/ready"))
        .send()
        .map(|r| r.status().as_u16() == 200)
        .unwrap_or(false)
}

/// Scrape `kirra_posture_generation` (the CACHE generation; 0 while cold).
fn posture_generation(port: u16) -> Option<u64> {
    let body = client()
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .ok()?
        .text()
        .ok()?;
    let line = body
        .lines()
        .find(|l| l.starts_with("kirra_posture_generation") && !l.starts_with('#'))?;
    line.rsplit(' ').next()?.parse().ok()
}

/// Register a node (posture-exempt trust bootstrap — reachable under the
/// empty-fleet LockedOut) to trigger a posture recalculation.
fn register_node(port: u16, node_id: &str) -> bool {
    client()
        .post(format!("http://127.0.0.1:{port}/attestation/register"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&serde_json::json!({ "node_id": node_id }))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

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

fn spawn_instance(name: &'static str, dir: &Path, db: &Path, port: u16) -> ChildGuard {
    let log = std::fs::File::create(dir.join(format!("{name}.log"))).expect("log file");
    let log_err = log.try_clone().expect("clone log handle");
    // env_clear: hermetic — an ambient KIRRA_* var must not skew the drill.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kirra_verifier_service"));
    cmd.env_clear()
        .env("KIRRA_DB_PATH", db)
        .env("KIRRA_VERIFIER_ADDR", format!("127.0.0.1:{port}"))
        .env("KIRRA_VERIFIER_MODE", "active")
        .env("KIRRA_INSTANCE_ID", name)
        .env("KIRRA_ADMIN_TOKEN", ADMIN_TOKEN)
        .env("KIRRA_VEHICLE_CLASS", "courier")
        .env("RUST_LOG", "info");
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

fn drill_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kirra_gen_drill_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create drill dir");
    dir
}

/// Drive one observed generation advance: register `node_id`, then wait for
/// the scraped cache generation to move strictly past `floor`.
fn advance_generation(port: u16, node_id: &str, floor: u64, dir: &Path, name: &str) -> u64 {
    assert!(
        register_node(port, node_id),
        "registration of {node_id} must succeed; log tail:\n{}",
        log_tail(dir, name)
    );
    let mut seen = floor;
    let advanced = wait_until(ADVANCE_BUDGET, Duration::from_millis(50), || {
        if let Some(g) = posture_generation(port) {
            seen = seen.max(g);
        }
        seen > floor
    });
    assert!(
        advanced,
        "generation must advance past {floor} after registering {node_id}; \
         last seen {seen}; log tail:\n{}",
        log_tail(dir, name)
    );
    seen
}

/// #791 F3 (end-to-end): the posture generation NEVER time-reverses across a
/// real binary restart — the persisted high-water seeds the fresh process
/// before its first recalc.
#[test]
#[ignore = "two-process drill; run serially: cargo test --test generation_restart_drill -- --ignored"]
fn generation_survives_a_real_binary_restart() {
    let dir = drill_dir();
    let db = dir.join("shared.sqlite");

    // ---- Run 1: raise the generation floor, sequenced --------------------
    let port1 = free_port();
    let mut run1 = spawn_instance("run1", &dir, &db, port1);
    assert!(
        wait_until(BOOT_BUDGET, Duration::from_millis(100), || ready(port1)),
        "run 1 must become ready; log tail:\n{}",
        log_tail(&dir, "run1")
    );
    let mut g1 = 0u64;
    for i in 0..RUN1_REGISTRATIONS {
        g1 = advance_generation(port1, &format!("gen-drill-node-{i}"), g1, &dir, "run1");
    }
    assert!(
        g1 >= RUN1_REGISTRATIONS as u64,
        "run 1 peak generation {g1} must be at least the sequenced advances"
    );

    // ---- Kill: no graceful shutdown; the high-water alone must carry ------
    run1.kill_now();

    // ---- Run 2: same DB, one recalc, scrape -------------------------------
    let port2 = free_port();
    let _run2 = spawn_instance("run2", &dir, &db, port2);
    assert!(
        wait_until(BOOT_BUDGET, Duration::from_millis(100), || ready(port2)),
        "run 2 must become ready; log tail:\n{}",
        log_tail(&dir, "run2")
    );
    let g2 = advance_generation(port2, "gen-drill-node-after-restart", g1, &dir, "run2");

    // THE assertion: strictly newer across the restart. An unseeded counter
    // restarts at 1 and cannot reach run 1's sequenced peak with one
    // registration plus boot recalcs — so this discriminates, it doesn't
    // tautologize.
    assert!(
        g2 > g1,
        "post-restart generation {g2} must exceed the pre-restart peak {g1} \
         (time-reversal ⇒ the binary lost the init_generation_from_store wiring); \
         run 2 log tail:\n{}",
        log_tail(&dir, "run2")
    );
    println!(
        "generation restart drill: run1 peak {g1} → run2 {g2} (monotone across SIGKILL restart)"
    );
}
