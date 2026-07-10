//! kirra-consumer-ctl — operator CLI for the motor consumer's pinned
//! governor verifying key (ADR-0033; the #891 key-distribution gap).
//!
//!   kirra-consumer-ctl enroll --key-hex <64-hex> --path <pin-file> [--rotate]
//!   kirra-consumer-ctl show   --path <pin-file>
//!
//! `enroll` is idempotent (re-enrolling the same key succeeds without
//! touching the file) and REFUSES to overwrite a different pin unless
//! `--rotate` is explicit — changing which governor a consumer obeys must
//! never happen silently. Rotation ORDER matters: re-enroll the consumer
//! BEFORE the verifier signs under the new key, or the platform enters a
//! permanent safe stop (loud, via the key-mismatch alarm) until re-enrolled.
//! See docs/safety/GOVERNOR_KEY_PROVISIONING.md §7.

use std::path::PathBuf;
use std::process::ExitCode;

use kirra_actuation_consumer::enrollment::{
    enroll_verifying_key, load_pinned_verifying_key, EnrollError, EnrollOutcome,
};
use kirra_release_token::ros_twist::verifying_key_id_hex;

fn usage() -> &'static str {
    "usage:\n  kirra-consumer-ctl enroll --key-hex <64-hex> --path <pin-file> [--rotate]\n  kirra-consumer-ctl show   --path <pin-file>"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("enroll") => cmd_enroll(&args[1..]),
        Some("show") => cmd_show(&args[1..]),
        _ => Err(usage().to_string()),
    };
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn cmd_enroll(args: &[String]) -> Result<String, String> {
    let key_hex = flag_value(args, "--key-hex").ok_or_else(|| usage().to_string())?;
    let path = PathBuf::from(flag_value(args, "--path").ok_or_else(|| usage().to_string())?);
    let rotate = args.iter().any(|a| a == "--rotate");

    match enroll_verifying_key(&path, &key_hex, rotate) {
        Ok(EnrollOutcome::Enrolled { key_id }) => {
            Ok(format!("ENROLLED key_id={key_id} path={}", path.display()))
        }
        Ok(EnrollOutcome::AlreadyEnrolled { key_id }) => Ok(format!(
            "ALREADY-ENROLLED key_id={key_id} (idempotent; pin untouched)"
        )),
        Ok(EnrollOutcome::Rotated {
            previous_key_id,
            key_id,
        }) => Ok(format!(
            "ROTATED previous_key_id={previous_key_id} -> key_id={key_id}\n\
             REMINDER: the verifier must not sign under the OLD key from here on; if the \
             verifier is still on the old key, this consumer will refuse everything \
             (loudly) until the verifier rotates too."
        )),
        Err(EnrollError::DifferentKeyPinned {
            pinned_key_id,
            offered_key_id,
        }) => Err(format!(
            "a DIFFERENT key is already pinned (pinned key_id={pinned_key_id}, offered \
             key_id={offered_key_id}). Refusing to overwrite silently — if this is an \
             intentional rotation, re-run with --rotate AND follow the rotation order in \
             GOVERNOR_KEY_PROVISIONING.md §7 (consumer first, verifier second)."
        )),
        Err(EnrollError::ExistingPinUnreadable) => Err(
            "the existing pin file does not parse as a key (tampering or corruption?). \
             Investigate before replacing it; --rotate may overwrite it once you have."
                .to_string(),
        ),
        Err(EnrollError::MalformedKey) => Err(
            "--key-hex must be exactly 64 hex chars (the 32 Ed25519 public-key bytes) \
             and a valid curve point"
                .to_string(),
        ),
        Err(EnrollError::Io(e)) => Err(format!("filesystem: {e}")),
    }
}

fn cmd_show(args: &[String]) -> Result<String, String> {
    let path = PathBuf::from(flag_value(args, "--path").ok_or_else(|| usage().to_string())?);
    match load_pinned_verifying_key(&path) {
        Ok(vk) => Ok(format!(
            "PINNED key_id={} path={}",
            verifying_key_id_hex(&vk),
            path.display()
        )),
        Err(EnrollError::Io(e)) => Err(format!(
            "no readable pin at {} ({e}) — an unpinned consumer refuses every command",
            path.display()
        )),
        Err(_) => Err(format!(
            "pin at {} does not parse as a key — the consumer will refuse to start",
            path.display()
        )),
    }
}
