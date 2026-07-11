//! `kirra_ros_release_mint` — the GOVERNOR STAND-IN minter for the ROS release
//! path. Emits a signed `payload(32) || token(96)` frame (or an unsigned
//! payload) for demos, the elevated first-run test, and the FFI smoke test.
//!
//! Why this exists: the Python consumer must NEVER hold a signing key or
//! re-implement the domain-separated signing (two-sources-of-truth). In
//! production the verifier/governor signs. For a bench demo we still need a
//! signer — so this tiny CLI wraps the SAME `issue_ros_release` the governor
//! uses (`kirra-release-token::ros_twist`). No crypto is re-implemented here or
//! in Python; the Python side only ever consumes these bytes.
//!
//! ⚠️ DEV/DEMO ONLY. The `--seed` is a well-known test key, not a provisioned
//! governor key. A real deployment mints frames from the verifier's provisioned
//! signing key (see docs/safety/GOVERNOR_KEY_PROVISIONING.md), never from a CLI
//! seed on the robot.
//!
//! Usage:
//!   kirra_ros_release_mint --seed <hex32> pubkey
//!       → prints the 64-hex Ed25519 public key to pin in the consumer.
//!   kirra_ros_release_mint --seed <hex32> frame \
//!       --seq N --issued-ms M --linear X --angular Y [--corrupt-sig | --no-token]
//!       → prints the frame as one hex line to stdout
//!         (128 bytes signed, or 32 bytes with --no-token).

use ed25519_dalek::SigningKey;
use kirra_release_token::ros_twist::{issue_ros_release, RosTwistPayload};

fn die(msg: &str) -> ! {
    eprintln!("kirra_ros_release_mint: {msg}");
    std::process::exit(2);
}

fn arg_val(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn hex_to_32(s: &str) -> [u8; 32] {
    let s = s.trim();
    if s.len() != 64 {
        die("--seed must be 64 hex chars (32 bytes)");
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .unwrap_or_else(|_| die("--seed is not valid hex"));
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed = arg_val(&args, "--seed").unwrap_or_else(|| die("--seed <hex32> is required"));
    let sk = SigningKey::from_bytes(&hex_to_32(&seed));

    match args
        .iter()
        .find(|a| !a.starts_with("--") && a.as_str() != seed)
        .map(String::as_str)
    {
        Some("pubkey") => {
            let pk = sk.verifying_key().to_bytes();
            let mut s = String::with_capacity(64);
            for b in pk {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
            }
            println!("{s}");
        }
        Some("frame") => {
            let parse = |flag: &str| -> String {
                arg_val(&args, flag).unwrap_or_else(|| die(&format!("{flag} is required")))
            };
            let seq: u64 = parse("--seq")
                .parse()
                .unwrap_or_else(|_| die("--seq must be u64"));
            let issued_ms: u64 = parse("--issued-ms")
                .parse()
                .unwrap_or_else(|_| die("--issued-ms must be u64"));
            let linear: f64 = parse("--linear")
                .parse()
                .unwrap_or_else(|_| die("--linear must be f64"));
            let angular: f64 = parse("--angular")
                .parse()
                .unwrap_or_else(|_| die("--angular must be f64"));

            let payload = RosTwistPayload {
                sequence: seq,
                issued_at_ms: issued_ms,
                linear_mps: linear,
                angular_rad_s: angular,
            };
            let payload_bytes = payload.encode();

            let mut frame: Vec<u8> = payload_bytes.to_vec();
            if !has_flag(&args, "--no-token") {
                let mut token = issue_ros_release(&payload, &sk).to_bytes();
                if has_flag(&args, "--corrupt-sig") {
                    // Flip a signature bit → a token that fails Ed25519 verify
                    // (the wrong-key / tamper case, without a second key).
                    token[64] ^= 0x01;
                }
                frame.extend_from_slice(&token);
            }

            let mut s = String::with_capacity(frame.len() * 2);
            for b in &frame {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
            }
            println!("{s}");
        }
        _ => die("expected a subcommand: `pubkey` or `frame`"),
    }
}
