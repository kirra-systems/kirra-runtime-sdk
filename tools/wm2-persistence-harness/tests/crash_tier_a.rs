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

/// `powercut arm` never returns — the operator ends it with the power switch —
/// so it is driven as a child, given time to print `ARMED`, and killed. That is
/// as close to a power cut as software gets, and it is enough to exercise the
/// marker lifecycle, which is the part under test here.
fn arm_and_kill(db: &PathBuf) -> bool {
    let mut child = Command::new(BIN)
        .args(["powercut", "arm", "--db"])
        .arg(db)
        .args(["--events", "64", "--entities", "8"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("arm did not start");

    // Poll for the marker rather than sleeping a fixed period: on a slow
    // machine a fixed sleep flakes, and on a fast one it wastes the wall clock
    // of every run.
    let marker = {
        let mut p = db.as_os_str().to_os_string();
        p.push("-tierc-armed");
        PathBuf::from(p)
    };
    for _ in 0..300 {
        if marker.exists() {
            break;
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Exited without arming — the refusal path.
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let armed = marker.exists();
    let _ = child.kill();
    let _ = child.wait();
    armed
}

#[test]
fn each_power_cut_trial_needs_its_own_arming() {
    // The defect this test exists for: `arm` restarted at generation 0, so the
    // second arming hit a primary-key collision and died while the FIRST
    // marker survived. `verify` then re-read the same surviving store and
    // recorded another PASS — one real power cut produced a tier C row per
    // invocation, and five "trials" were satisfiable by one cut and four
    // no-ops.
    let dir = scratch("powercut-independence");
    let db = dir.join("pc.sqlite");
    let ledger = dir.join("pc.sqlite-tierc-trials.jsonl");

    // Trial 1: arm, "cut", verify.
    assert!(
        arm_and_kill(&db),
        "the first arming did not produce a marker"
    );
    let status = Command::new(BIN)
        .args(["powercut", "verify", "--db"])
        .arg(&db)
        .args(["--trial", "1"])
        .status()
        .expect("verify did not run");
    // A legitimate verify still exits 1 here, and must: one arming is short of
    // the five tier C requires, so the aggregate is INCONCLUSIVE and an
    // incomplete drill is not allowed to exit 0. Exit 2 is the distinct code
    // for a *refused* verification, which is what the rest of this test turns
    // on — the two must not be conflated.
    assert_eq!(
        status.code(),
        Some(1),
        "a recorded-but-insufficient trial must exit 1, not be refused"
    );
    assert_eq!(read_records(&ledger).len(), 1);

    // Verifying again without re-arming must be refused, not recorded.
    let status = Command::new(BIN)
        .args(["powercut", "verify", "--db"])
        .arg(&db)
        .args(["--trial", "2"])
        .status()
        .expect("verify did not run");
    assert_eq!(
        status.code(),
        Some(2),
        "re-verifying without re-arming must be a refusal — this is how one power cut \
         became three PASS rows"
    );
    assert_eq!(
        read_records(&ledger).len(),
        1,
        "a refused verify appended to the ledger anyway"
    );

    // A second arming must succeed on the now-populated store: it starts at
    // MAX(generation)+1 rather than colliding at 0.
    assert!(
        arm_and_kill(&db),
        "the second arming failed on a populated store — it restarted at generation 0"
    );

    // ...and the two rows carry different arm ids, so they count as two.
    let status = Command::new(BIN)
        .args(["powercut", "verify", "--db"])
        .arg(&db)
        .args(["--trial", "2"])
        .status()
        .expect("verify did not run");
    assert_eq!(
        status.code(),
        Some(1),
        "the second verify was refused rather than recorded"
    );

    let rows = read_records(&ledger);
    assert_eq!(rows.len(), 2);
    let ids: Vec<&str> = rows.iter().filter_map(|r| field(r, "arm_id")).collect();
    assert_eq!(ids.len(), 2, "a ledger row carried no arm id");
    assert_ne!(
        ids[0], ids[1],
        "two separate armings minted the same id, so they would count as one trial"
    );

    // Arming over an outstanding marker is refused: the previous trial would be
    // orphaned and its marker would describe a store state that no longer
    // exists.
    assert!(arm_and_kill(&db), "third arming");
    // Bounded, because a regression here does not fail — it arms, and `arm`
    // never returns. A plain `.status()` would hang the suite instead of
    // reporting the bug.
    let mut child = Command::new(BIN)
        .args(["powercut", "arm", "--db"])
        .arg(&db)
        .args(["--events", "64", "--entities", "8"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("arm did not start");
    let mut code = None;
    for _ in 0..300 {
        if let Ok(Some(s)) = child.try_wait() {
            code = s.code();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if code.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert_eq!(
        code,
        Some(2),
        "arming over an unused marker must be refused; it armed again instead"
    );

    let _ = std::fs::remove_dir_all(&dir);
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
