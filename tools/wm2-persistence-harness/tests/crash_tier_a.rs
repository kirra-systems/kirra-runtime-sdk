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

#[test]
fn a_stray_positional_word_is_rejected_outside_powercut() {
    // `powercut` takes a positional subcommand, so the parser accepts one
    // bare word. It must not accept one anywhere else, or a mistyped
    // invocation quietly runs a different experiment than was asked for.
    let status = Command::new(BIN)
        .args(["all", "verify"])
        .status()
        .expect("did not run");
    assert_eq!(
        status.code(),
        Some(2),
        "a bare word after a normal command must be a usage error"
    );
}

#[test]
fn powercut_requires_a_subcommand_and_verify_requires_a_trial_number() {
    for args in [
        vec!["powercut"],
        vec!["powercut", "sideways"],
        vec!["powercut", "verify"], // no --trial
    ] {
        let status = Command::new(BIN).args(&args).status().expect("did not run");
        assert_eq!(
            status.code(),
            Some(2),
            "`{}` must be a usage error, not a silent no-op",
            args.join(" ")
        );
    }
}

/// The full tier C loop, with `SIGKILL` standing in for the power switch.
///
/// This does **not** make tier C automatable — a signal leaves the page cache
/// intact, which is exactly why tier C needs a power supply and why the harness
/// refuses to claim it. What it proves is that the *recording* machinery works:
/// the marker survives, `verify` judges against it, the ledger accumulates, and
/// the aggregate still refuses to pass below the required trial count.
#[test]
fn the_powercut_ledger_accumulates_and_still_refuses_to_pass_early() {
    use std::io::Read;

    let mut db = scratch("powercut-loop");
    db.push("w.sqlite");
    for suffix in ["", "-wal", "-shm", "-tierc-armed", "-tierc-trials.jsonl"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }

    // Arm never returns, so it is killed the way the mains would kill it.
    let mut child = Command::new(BIN)
        .args(["powercut", "arm", "--db", &db.to_string_lossy()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn arm");

    // Wait for it to announce that the prefix is fsynced and recorded, so the
    // kill lands on the tail rather than on the prefix.
    let mut out = child.stdout.take().expect("stdout");
    let mut seen = String::new();
    let mut byte = [0u8; 1];
    while !seen.contains("ARMED:") {
        match out.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => seen.push(byte[0] as char),
        }
    }
    assert!(seen.contains("ARMED:"), "arm never armed: {seen}");
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = child.kill();
    let _ = child.wait();

    // Three trials: below the required five, so the tier must still refuse.
    for trial in 1..=3 {
        let status = Command::new(BIN)
            .args([
                "powercut",
                "verify",
                "--db",
                &db.to_string_lossy(),
                "--trial",
                &trial.to_string(),
            ])
            .status()
            .expect("did not run");
        assert_eq!(
            status.code(),
            Some(1),
            "trial {trial}: an incomplete tier C must exit non-zero"
        );
    }

    let ledger = std::fs::read_to_string(format!("{}-tierc-trials.jsonl", db.display()))
        .expect("ledger written");
    let lines: Vec<&str> = ledger.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "one row per trial: {ledger}");
    for line in &lines {
        assert!(
            line.contains("\"outcome\":\"PASS\""),
            "a SIGKILL must not lose the fsynced prefix — that would be a real \
             finding, not a test artefact: {line}"
        );
    }
}
