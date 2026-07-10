//! ADR-0033 Tier-1 regression guard — the artifact that turns "the checker is
//! structurally the authority" from a claim into evidence.
//!
//! CROSS-PROCESS: a child process (this same test binary re-executed with
//! `KIRRA_TIER1_CHILD=1`, the `audit_chain_prefix_on_kill` reexec precedent)
//! plays the GOVERNOR — it mints the release tokens with the signing key and
//! emits five frames over a pipe. The parent plays the CONSUMER — it holds
//! only the VERIFYING key, feeds every received frame through the real
//! `MotorConsumer` chokepoint with a mock serial seam, and asserts:
//!
//!   **exactly one serial write occurred, and it was case (a)'s enforced
//!   bytes.**
//!
//! The five injected frames (the FDIT matrix's shape lifted to the deployment
//! boundary):
//!   (a) a valid signed command                    → the ONLY release
//!   (b) an unsigned command                       → NoToken
//!   (c) a replayed signed command ((a) verbatim)  → SequenceNotAdvanced
//!   (d) a token over DIFFERENT bytes              → DigestMismatch
//!   (e) a deny-path response — per the pinned verifier invariant the deny
//!       path never mints, so the frame carries no token; a rogue could also
//!       staple a captured token onto denied bytes, which is case (d)'s
//!       digest mismatch. Modeled here as its enforced-stop bytes, token-less
//!       → NoToken, no write.
//!
//! No ROS anywhere; CI-blocking via the workspace `Test` lane.

use std::process::{Command, Stdio};

use kirra_actuation_consumer::{
    ConsumerConfig, FrameOutcome, MotorConsumer, MotorSerial, ReleaseToken, RosReleaseRefusal,
    DEFAULT_MISSED_PERIODS, ROS_TWIST_PAYLOAD_LEN,
};

/// The governor key pair for this drill. The CHILD derives the signing key
/// from this seed; the PARENT uses only the verifying key below. A real
/// deployment provisions per `docs/safety/GOVERNOR_KEY_PROVISIONING.md`.
const DRILL_SEED: [u8; 32] = [42u8; 32];

/// Deterministic virtual clock for the drill (all frames minted at T, all
/// verified at T + 50 ms — inside the 200 ms freshness window).
const T_MINT: u64 = 10_000;
const T_VERIFY: u64 = 10_050;

/// Case (a)'s enforced twist — the ONE thing allowed to reach the serial seam.
const A_LINEAR: f64 = 0.42;
const A_ANGULAR: f64 = 0.05;

#[derive(Default)]
struct RecordingSerial {
    writes: Vec<(f64, f64)>,
}
impl MotorSerial for RecordingSerial {
    type Error = std::convert::Infallible;
    fn write_twist(&mut self, linear: f64, angular: f64) -> Result<(), Self::Error> {
        self.writes.push((linear, angular));
        Ok(())
    }
}

/// CHILD role: mint and emit the five frames as `FRAME <case> <payload_hex>
/// <token_hex|->` lines. Runs in a separate process from the consumer — the
/// signing key never exists in the parent's address space.
fn run_child_governor() {
    use ed25519_dalek::SigningKey;
    use kirra_actuation_consumer::RosTwistPayload;
    use kirra_release_token::ros_twist::issue_ros_release;

    let sk = SigningKey::from_bytes(&DRILL_SEED);
    let mut out = String::new();

    let payload = |seq: u64, linear: f64, angular: f64| RosTwistPayload {
        sequence: seq,
        issued_at_ms: T_MINT,
        linear_mps: linear,
        angular_rad_s: angular,
    };

    // (a) valid signed command.
    let a = payload(1, A_LINEAR, A_ANGULAR);
    let a_token = issue_ros_release(&a, &sk);
    out += &format!(
        "FRAME a {} {}\n",
        hex::encode(a.encode()),
        hex::encode(a_token.to_bytes())
    );

    // (b) unsigned command (rogue publisher: bytes, no proof).
    let b = payload(2, 3.0, 1.0);
    out += &format!("FRAME b {} -\n", hex::encode(b.encode()));

    // (c) replayed signed command — (a) verbatim.
    out += &format!(
        "FRAME c {} {}\n",
        hex::encode(a.encode()),
        hex::encode(a_token.to_bytes())
    );

    // (d) a genuine token stapled onto DIFFERENT bytes (substitution).
    let d_signed = payload(4, 0.10, 0.0);
    let d_token = issue_ros_release(&d_signed, &sk);
    let mut d_presented = d_signed;
    d_presented.linear_mps = 5.0; // the attacker's bytes
    out += &format!(
        "FRAME d {} {}\n",
        hex::encode(d_presented.encode()),
        hex::encode(d_token.to_bytes())
    );

    // (e) the deny-path response: the verifier's deny arm NEVER mints (pinned
    // by tests/ros_release_mint_integration.rs in the root crate), so what
    // arrives on the bus is the enforced-stop bytes with no token.
    let e = payload(5, 0.0, 0.0);
    out += &format!("FRAME e {} -\n", hex::encode(e.encode()));

    print!("{out}");
}

fn parse_hex_array<const N: usize>(s: &str) -> [u8; N] {
    let bytes = hex::decode(s).expect("valid hex from child");
    bytes.try_into().expect("exact frame width")
}

#[test]
fn tier1_chokepoint_exactly_one_write_and_it_is_the_signed_enforced_bytes() {
    // Child role dispatch (same binary, re-executed).
    if std::env::var("KIRRA_TIER1_CHILD").is_ok() {
        run_child_governor();
        return;
    }

    // Spawn the governor child.
    let exe = std::env::current_exe().expect("test binary path");
    let mut child = Command::new(exe)
        .args([
            "tier1_chokepoint_exactly_one_write_and_it_is_the_signed_enforced_bytes",
            "--exact",
            "--nocapture",
        ])
        .env("KIRRA_TIER1_CHILD", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn governor child");
    let output = {
        let out = child.stdout.take().expect("child stdout");
        let mut buf = String::new();
        use std::io::Read as _;
        let mut out = out;
        out.read_to_string(&mut buf).expect("read child frames");
        let status = child.wait().expect("child exit");
        assert!(status.success(), "governor child failed");
        buf
    };
    // Keep the harness honest: stdout mixing with libtest output is fine —
    // we parse only FRAME lines, and there must be exactly five.
    let frames: Vec<(&str, [u8; ROS_TWIST_PAYLOAD_LEN], Option<ReleaseToken>)> = output
        .lines()
        .filter(|l| l.starts_with("FRAME "))
        .map(|l| {
            let mut parts = l.split_whitespace();
            let _tag = parts.next();
            let case = parts.next().expect("case id");
            let payload: [u8; ROS_TWIST_PAYLOAD_LEN] =
                parse_hex_array(parts.next().expect("payload hex"));
            let token = match parts.next().expect("token column") {
                "-" => None,
                t => Some(ReleaseToken::from_bytes(&parse_hex_array::<96>(t))),
            };
            (case, payload, token)
        })
        .collect();
    assert_eq!(frames.len(), 5, "child must emit exactly the five cases");

    // The consumer: verifying key only (the signing key lives in the child
    // process), real gate, real liveness config, mock serial seam.
    let governor_vk = ed25519_dalek::SigningKey::from_bytes(&DRILL_SEED).verifying_key();
    let mut consumer = MotorConsumer::new(
        governor_vk,
        ConsumerConfig {
            freshness_window_ms: 200,
            control_period_ms: 100,
            missed_periods: DEFAULT_MISSED_PERIODS,
            stop_decel_mps2: 1.0, // test fixture; deployments use the class MRC decel
        },
        RecordingSerial::default(),
    )
    .expect("valid drill config");

    let mut outcomes = Vec::new();
    for (case, payload, token) in &frames {
        let outcome = consumer.on_frame(payload, token.as_ref(), T_VERIFY);
        outcomes.push((*case, outcome));
    }

    // THE assertion: the serial seam saw exactly one write, and it was (a)'s
    // enforced bytes.
    let writes = &consumer.serial().writes;
    assert_eq!(
        writes.as_slice(),
        &[(A_LINEAR, A_ANGULAR)],
        "exactly one serial write, and it must be case (a)'s enforced twist; got {writes:?}"
    );

    // And the refusal taxonomy is the right one per case (not just "refused").
    assert!(matches!(
        outcomes[0],
        ("a", FrameOutcome::Released { sequence: 1 })
    ));
    assert!(matches!(
        outcomes[1],
        ("b", FrameOutcome::Refused(RosReleaseRefusal::NoToken))
    ));
    assert!(matches!(
        outcomes[2],
        (
            "c",
            FrameOutcome::Refused(RosReleaseRefusal::SequenceNotAdvanced {
                presented: 1,
                last_released: 1
            })
        )
    ));
    assert!(matches!(
        outcomes[3],
        (
            "d",
            FrameOutcome::Refused(RosReleaseRefusal::Denied(
                kirra_release_token::ReleaseDenied::DigestMismatch
            ))
        )
    ));
    assert!(matches!(
        outcomes[4],
        ("e", FrameOutcome::Refused(RosReleaseRefusal::NoToken))
    ));

    assert_eq!(consumer.release_count(), 1);
    assert_eq!(consumer.refusal_count(), 4);
}
