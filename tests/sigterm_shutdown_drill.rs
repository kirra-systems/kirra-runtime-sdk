//! #1127 — the SIGTERM clean-shutdown drill.
//!
//! The regression this pins: the verifier binary used to HANG FOREVER on
//! SIGTERM — the graceful-shutdown future ran to completion (the durable WAL
//! checkpoint logged), but the process never exited, so systemd deployments
//! only ever stopped via the `TimeoutStopSec` SIGKILL escalation. Two causes,
//! both fixed and both exercised here end-to-end:
//!
//!   * the audit writer's blocking task held `Arc<AppState>` while its Sender
//!     lived INSIDE that state — a self-referential keep-alive that kept the
//!     channel open forever, so `blocking_recv` never returned and tokio's
//!     runtime teardown (which joins in-flight blocking tasks) never finished
//!     (now `Weak`, `src/audit_writer.rs`);
//!   * `#[tokio::main]`'s implicit runtime `Drop` has no deadline — any future
//!     wedged blocking task would reintroduce the hang (now an explicitly
//!     owned runtime + `shutdown_timeout`, the #794 F7 / #1119 precedent).
//!
//! Shape: spawn the REAL binary (with the #1123 ops listener enabled, so the
//! richest two-listener configuration is the one proven), wait for `/ready`,
//! send SIGTERM, and assert the process exits CLEANLY within a bound that is
//! generous for CI but far below any systemd kill escalation. The bound is
//! the discriminator: the pre-fix binary never exits at all.
//!
//! `#[ignore]` by default (process orchestration + real timers); CI runs it
//! in the serial two-process-drill job. Locally:
//!
//! ```sh
//! cargo test --test sigterm_shutdown_drill -- --ignored --nocapture
//! ```
#![cfg(unix)]

use std::io::Read as _;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Boot budget (compile-cold runners, fsync-heavy init) — same as the
/// generation restart drill.
const BOOT_BUDGET: Duration = Duration::from_secs(60);
/// Exit bound after SIGTERM. The clean path measures ~milliseconds; the
/// pre-#1127 binary never exits. 20 s absorbs a slow CI checkpoint while
/// staying far below any systemd TimeoutStopSec escalation.
const EXIT_BUDGET: Duration = Duration::from_secs(20);

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
        .no_proxy() // hermetic: never route loopback probes through a proxy
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

fn log_tail(dir: &Path, name: &str) -> String {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(dir.join(format!("{name}.log"))) {
        let _ = f.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

fn read_log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default()
}

/// Send SIGTERM (Child::kill is SIGKILL — useless here; the whole point is
/// the graceful path). `kill(1)` keeps the test dependency-free.
fn sigterm(child: &Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("run kill(1)");
    assert!(status.success(), "kill -TERM must be delivered");
}

#[test]
#[ignore = "two-process drill; run serially: cargo test --test sigterm_shutdown_drill -- --ignored"]
fn sigterm_exits_cleanly_within_the_bound() {
    let dir = std::env::temp_dir().join(format!("kirra_sigterm_drill_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("drill dir");
    let db = dir.join("drill.sqlite");
    let port = free_port();
    let metrics_port = free_port();

    let log = std::fs::File::create(dir.join("svc.log")).expect("log file");
    let log_err = log.try_clone().expect("clone log handle");
    // env_clear: hermetic — an ambient KIRRA_* var must not skew the drill.
    // KIRRA_METRICS_ADDR on: the two-listener configuration is the richest
    // shutdown topology, so it is the one this drill proves.
    let mut child = Command::new(env!("CARGO_BIN_EXE_kirra_verifier_service"))
        .env_clear()
        .env("KIRRA_DB_PATH", &db)
        .env("KIRRA_VERIFIER_ADDR", format!("127.0.0.1:{port}"))
        .env("KIRRA_METRICS_ADDR", format!("127.0.0.1:{metrics_port}"))
        .env("KIRRA_VERIFIER_MODE", "active")
        .env("KIRRA_INSTANCE_ID", "sigterm-drill")
        .env("KIRRA_ADMIN_TOKEN", "drill-admin-token")
        .env("KIRRA_VEHICLE_CLASS", "courier")
        .env("RUST_LOG", "info")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .expect("spawn verifier");

    assert!(
        wait_until(BOOT_BUDGET, Duration::from_millis(100), || ready(port)),
        "service must become ready; log tail:\n{}",
        log_tail(&dir, "svc")
    );

    sigterm(&child);

    let deadline = Instant::now();
    let exited = wait_until(EXIT_BUDGET, Duration::from_millis(100), || {
        matches!(child.try_wait(), Ok(Some(_)))
    });
    if !exited {
        let _ = child.kill(); // don't leak the wedged process on failure
        let _ = child.wait();
        panic!(
            "REGRESSION (#1127): the binary did not exit within {EXIT_BUDGET:?} of SIGTERM \
             — the shutdown hang is back; log tail:\n{}",
            log_tail(&dir, "svc")
        );
    }
    let elapsed = deadline.elapsed();
    let status = child.wait().expect("reap");
    assert!(
        status.success(),
        "SIGTERM must be a CLEAN exit (code 0), got {status:?}; log tail:\n{}",
        log_tail(&dir, "svc")
    );

    // The exit must be the ORDERED path, not a fluke: the durable checkpoint
    // flushed and the audit writer saw its channel close (the #1127 fix's
    // documented teardown sequence).
    let log_text = read_log(&dir, "svc");
    assert!(
        log_text.contains("durable checkpoint flushed on shutdown"),
        "shutdown must run the durable checkpoint; log tail:\n{}",
        log_tail(&dir, "svc")
    );
    assert!(
        log_text.contains("audit writer task exiting"),
        "the audit writer must exit via its channel-closed path (the Weak fix); log tail:\n{}",
        log_tail(&dir, "svc")
    );

    println!("sigterm drill: clean exit in {elapsed:?} (bound {EXIT_BUDGET:?})");
    let _ = std::fs::remove_dir_all(&dir);
}
