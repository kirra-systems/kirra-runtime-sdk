//! `kirra_ros_release_mint` — the GOVERNOR STAND-IN minter for the ROS release
//! path, for demos, the elevated first-run test, and the FFI smoke test. It
//! emits either wire format:
//!   `frame`    — V1, signed `payload(32) || token(96)` (128 bytes).
//!   `frame-v2` — evidence-bound V2, signed `payload(176) || token(96)`
//!                (272 bytes) — the format the live R2 motor consumer accepts.
//! Both can also emit the bare unsigned payload with `--no-token`.
//!
//! Why this exists: the Python consumer must NEVER hold a signing key or
//! re-implement the domain-separated signing (two-sources-of-truth). In
//! production the verifier/governor signs. For a bench demo we still need a
//! signer — so this tiny CLI wraps the SAME `issue_ros_release` /
//! `issue_ros_bound_command_release` the governor uses
//! (`kirra-release-token::{ros_twist, ros_bound_command}`). No crypto is
//! re-implemented here or in Python; the Python side only consumes these bytes.
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
//!       → prints the V1 frame as one hex line to stdout
//!         (128 bytes signed, or 32 bytes with --no-token).
//!   kirra_ros_release_mint --seed <hex32> frame-v2 \
//!       --seq N --issued-ms M --linear X --angular Y \
//!       --profile-digest <hex32> \
//!       [--nonce N] [--expires-ms M | --lifetime-ms L] \
//!       [--scan-seq N] [--camera-seq N] [--tracker-gen N] \
//!       [--evidence-digest <hex32>] [--proposal-digest <hex32>] \
//!       [--corrupt-sig | --no-token]
//!       → prints the evidence-bound V2 frame as one hex line
//!         (272 bytes signed, or 176 bytes with --no-token).
//!
//! `frame-v2` mints the SAME `RosBoundCommandPayload` the verifier's governor
//! mints (`kirra-release-token::ros_bound_command`), so the bench publisher and
//! the FFI smoke test can drive the evidence-bound motor consumer. Everything is
//! overridable and NOTHING is range-checked here beyond hex/number parsing:
//! minting a deliberately-bad payload (a zero sequence, an over-long lifetime, a
//! stale perception frame) is how the negative tests reach each refusal arm. The
//! consumer's gate is the authority on what is acceptable — this tool only signs.
//!
//! `--profile-digest` is REQUIRED and has no default: it must equal the
//! consumer's pinned `KIRRA_PROFILE_DIGEST`, and a defaulted value would mint
//! frames that silently refuse with ProfileDigestMismatch.

use ed25519_dalek::SigningKey;
use kirra_release_token::ros_bound_command::{
    issue_ros_bound_command_release, RosBoundCommandPayload,
};
use kirra_release_token::ros_twist::{issue_ros_release, RosTwistPayload};

/// Bench-default evidence/proposal identity. These are IDENTITY-only from the
/// motor gate's point of view (it pins the profile digest, and treats these as
/// opaque bound fields), so a fixed non-zero default keeps the CLI short. They
/// must be non-zero: the V2 decoder refuses an all-zero digest.
const DEFAULT_EVIDENCE_DIGEST_HEX: &str =
    "be11be11be11be11be11be11be11be11be11be11be11be11be11be11be11be11";
const DEFAULT_PROPOSAL_DIGEST_HEX: &str =
    "d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5d0e5";

/// Default signed lifetime (ms) when neither `--expires-ms` nor `--lifetime-ms`
/// is given. Matches the verifier's ROS_BOUND_RELEASE_LIFETIME_MS.
const DEFAULT_LIFETIME_MS: u64 = 200;

const SUBCOMMANDS: [&str; 3] = ["pubkey", "frame", "frame-v2"];

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

fn hex_to_32(s: &str, what: &str) -> [u8; 32] {
    let s = s.trim();
    if s.len() != 64 {
        die(&format!("{what} must be 64 hex chars (32 bytes)"));
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .unwrap_or_else(|_| die(&format!("{what} is not valid hex")));
    }
    out
}

fn hex_line(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A required flag's value.
fn req(args: &[String], flag: &str) -> String {
    arg_val(args, flag).unwrap_or_else(|| die(&format!("{flag} is required")))
}

fn req_u64(args: &[String], flag: &str) -> u64 {
    req(args, flag)
        .parse()
        .unwrap_or_else(|_| die(&format!("{flag} must be u64")))
}

fn req_f64(args: &[String], flag: &str) -> f64 {
    req(args, flag)
        .parse()
        .unwrap_or_else(|_| die(&format!("{flag} must be f64")))
}

fn opt_u64(args: &[String], flag: &str) -> Option<u64> {
    arg_val(args, flag).map(|v| {
        v.parse()
            .unwrap_or_else(|_| die(&format!("{flag} must be u64")))
    })
}

fn opt_hex_32(args: &[String], flag: &str, default_hex: &str) -> [u8; 32] {
    match arg_val(args, flag) {
        Some(v) => hex_to_32(&v, flag),
        None => hex_to_32(default_hex, flag),
    }
}

/// Append the signed token (or nothing, with `--no-token`), honouring
/// `--corrupt-sig`. Shared by both frame formats so the two cannot drift.
fn append_token(frame: &mut Vec<u8>, args: &[String], mut token: [u8; 96]) {
    if has_flag(args, "--no-token") {
        return;
    }
    if has_flag(args, "--corrupt-sig") {
        // Flip a signature bit → a token that fails Ed25519 verify (the
        // wrong-key / tamper case, without needing a second key).
        token[64] ^= 0x01;
    }
    frame.extend_from_slice(&token);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed = arg_val(&args, "--seed").unwrap_or_else(|| die("--seed <hex32> is required"));
    let sk = SigningKey::from_bytes(&hex_to_32(&seed, "--seed"));

    // Dispatch on a KNOWN subcommand name rather than "the first non-flag
    // argument": frame-v2 takes enough flags that a flag VALUE (a bare number,
    // a hex digest) would otherwise be mistaken for the subcommand whenever the
    // subcommand is not written first.
    match args
        .iter()
        .find(|a| SUBCOMMANDS.contains(&a.as_str()))
        .map(String::as_str)
    {
        Some("pubkey") => {
            println!("{}", hex_line(&sk.verifying_key().to_bytes()));
        }
        Some("frame") => {
            let payload = RosTwistPayload {
                sequence: req_u64(&args, "--seq"),
                issued_at_ms: req_u64(&args, "--issued-ms"),
                linear_mps: req_f64(&args, "--linear"),
                angular_rad_s: req_f64(&args, "--angular"),
            };

            let mut frame: Vec<u8> = payload.encode().to_vec();
            append_token(
                &mut frame,
                &args,
                issue_ros_release(&payload, &sk).to_bytes(),
            );
            println!("{}", hex_line(&frame));
        }
        Some("frame-v2") => {
            let sequence = req_u64(&args, "--seq");
            let issued_at_ms = req_u64(&args, "--issued-ms");

            // Expiry: --expires-ms is absolute and wins; --lifetime-ms is
            // relative to issued_at_ms; neither → the verifier's default.
            // Both together is a contradiction, not a precedence puzzle.
            let expires_at_ms = match (
                opt_u64(&args, "--expires-ms"),
                opt_u64(&args, "--lifetime-ms"),
            ) {
                (Some(_), Some(_)) => die("pass --expires-ms OR --lifetime-ms, not both"),
                (Some(absolute), None) => absolute,
                (None, Some(lifetime)) => issued_at_ms.saturating_add(lifetime),
                (None, None) => issued_at_ms.saturating_add(DEFAULT_LIFETIME_MS),
            };

            let payload = RosBoundCommandPayload {
                sequence,
                // Sequence and nonce must BOTH strictly advance, so tying the
                // nonce to the sequence is the natural bench behaviour; an
                // explicit --nonce covers the nonce-replay negative test.
                nonce: opt_u64(&args, "--nonce").unwrap_or(sequence),
                issued_at_ms,
                expires_at_ms,
                linear_mps: req_f64(&args, "--linear"),
                angular_rad_s: req_f64(&args, "--angular"),
                scan_sequence: opt_u64(&args, "--scan-seq").unwrap_or(1),
                // Absent → None (the "no camera contribution" encoding).
                camera_sequence: opt_u64(&args, "--camera-seq"),
                tracker_generation: opt_u64(&args, "--tracker-gen").unwrap_or(1),
                // Required: must equal the consumer's pinned digest.
                profile_digest: hex_to_32(&req(&args, "--profile-digest"), "--profile-digest"),
                evidence_digest: opt_hex_32(
                    &args,
                    "--evidence-digest",
                    DEFAULT_EVIDENCE_DIGEST_HEX,
                ),
                proposal_digest: opt_hex_32(
                    &args,
                    "--proposal-digest",
                    DEFAULT_PROPOSAL_DIGEST_HEX,
                ),
            };

            let mut frame: Vec<u8> = payload.encode().to_vec();
            append_token(
                &mut frame,
                &args,
                issue_ros_bound_command_release(&payload, &sk).to_bytes(),
            );
            println!("{}", hex_line(&frame));
        }
        _ => die("expected a subcommand: `pubkey`, `frame` or `frame-v2`"),
    }
}
