//! Tier A end-to-end, against the real binary.
//!
//! This lives in an integration test rather than a unit test for a structural
//! reason worth recording: `crash::tier_a_sigkill` re-execs
//! `std::env::current_exe()` as its appending child, and under `cargo test`
//! that is the *test* binary — which has no `crash-child` subcommand and never
//! runs `main`. A unit test would therefore exercise the spawn failure path and
//! report `INCONCLUSIVE` forever, which is worse than no test: it would look
//! like coverage.
//!
//! `CARGO_BIN_EXE_<name>` is set by cargo for integration tests and points at
//! the freshly built binary, so this drives the same code path an operator does.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_wm2-persistence-harness");

fn scratch(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wm2-it-{}-{}", name, std::process::id()));
    let _ = std::fs::create_dir_all(&p);
    p
}

fn read_records(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Extract a string field from a JSONL record without a JSON dependency. Crude
/// on purpose — the harness's own writer is unit-tested, so this only has to be
/// good enough to assert on.
fn field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let start = record.find(&needle)? + needle.len();
    let rest = &record[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[test]
fn crash_tier_a_kills_a_real_child_and_finds_an_intact_chain() {
    let dir = scratch("tier-a");
    let db = dir.join("crash.sqlite");
    let out = dir.join("results.jsonl");

    let status = Command::new(BIN)
        .args(["crash", "--db"])
        .arg(&db)
        .args(["--out"])
        .arg(&out)
        .args(["--durability", "normal", "--crash-run-ms", "1200"])
        .status()
        .expect("harness did not run");
    assert!(status.success(), "crash command exited {status}");

    let records = read_records(&out);
    let tiers: Vec<(&str, &str)> = records
        .iter()
        .filter(|r| field(r, "record") == Some("crash"))
        .map(|r| {
            (
                field(r, "tier").unwrap_or(""),
                field(r, "outcome").unwrap_or(""),
            )
        })
        .collect();

    assert_eq!(tiers.len(), 3, "expected three tiers, got {tiers:?}");

    let a = tiers
        .iter()
        .find(|(t, _)| t.starts_with("A_"))
        .expect("tier A missing");
    assert_eq!(
        a.1, "PASS",
        "tier A did not pass — a SIGKILL mid-append left a chain that does not verify"
    );

    let b = tiers
        .iter()
        .find(|(t, _)| t.starts_with("B_"))
        .expect("tier B missing");
    assert_eq!(b.1, "PASS", "tier B did not pass");

    // Tier C must never report anything but NOT-RUN from software. If this ever
    // changes, someone has claimed a durability result the harness cannot
    // produce.
    let c = tiers
        .iter()
        .find(|(t, _)| t.starts_with("C_"))
        .expect("tier C missing");
    assert_eq!(c.1, "NOT-RUN");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_run_is_marked_uncitable_even_when_the_operator_asserts_target() {
    let dir = scratch("assert");
    let out = dir.join("results.jsonl");

    // CI and every development machine here is x86_64, so this asserts the
    // guard that actually matters: the flag is not a rubber stamp.
    let status = Command::new(BIN)
        .args(["platform", "--db"])
        .arg(dir.join("x.sqlite"))
        .args(["--out"])
        .arg(&out)
        .arg("--assert-target")
        .status()
        .expect("harness did not run");
    assert!(status.success());

    let records = read_records(&out);
    let run = records
        .iter()
        .find(|r| field(r, "record") == Some("run"))
        .expect("no run record");

    if std::env::consts::ARCH == "aarch64" && std::path::Path::new("/etc/nv_tegra_release").exists()
    {
        // Running the suite on an actual Jetson: the assertion is legitimate.
        assert_eq!(
            field(run, "evidence_status"),
            Some("JETSON-TARGET-MEASURED")
        );
    } else {
        assert_eq!(
            field(run, "evidence_status"),
            Some("HOST-INDICATIVE-NOT-TARGET"),
            "--assert-target overrode the hardware facts"
        );
        assert!(run.contains(r#""citable":false"#));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_option_is_a_usage_error_rather_than_a_silent_default() {
    let status = Command::new(BIN)
        .args(["all", "--evnets", "10"])
        .status()
        .expect("did not run");
    assert_eq!(
        status.code(),
        Some(2),
        "a typo'd flag must not run the default experiment"
    );
}

#[test]
fn a_flag_missing_its_value_is_rejected() {
    let status = Command::new(BIN)
        .args(["all", "--events"])
        .status()
        .expect("did not run");
    assert_eq!(status.code(), Some(2));
}
