//! Consumer verifying-key enrollment — closing the #891 key-distribution gap.
//!
//! The consumer admits only a **pinned** governor verifying key
//! (`ADR-0033`; `RosReleaseGate::new` takes it by argument). This module is
//! the ONE place that decides where that pin lives on a consumer and how it
//! changes — the consumer-side sibling of
//! `kirra_release_token::provisioning` (which owns the VERIFIER's signing
//! key) and of `kirra-ota-ctl enroll` (which enrolls a node's attestation
//! identity with the verifier).
//!
//! ## The rotation-order hazard this exists to manage
//!
//! Rotation must re-provision the consumer **before** the verifier signs
//! under the new key. Done out of order, every command becomes
//! `SignatureInvalid` → the liveness supervisor starves into the safe stop
//! and stays there — a **permanent safe stop**. That outcome is fail-closed
//! and safe; the paired defect would be muteness, which the consumer's
//! key-mismatch alarm (`MotorConsumer::health`) exists to prevent. The
//! documented order and the recovery procedure live in
//! `docs/safety/GOVERNOR_KEY_PROVISIONING.md` §7.
//!
//! ## Pin file format and integrity model
//!
//! The pin is the PUBLIC key: exactly 64 lowercase hex chars (the 32
//! Ed25519 public-key bytes) plus optional trailing newline. No secrecy is
//! required — INTEGRITY is: whoever can write the pin decides which governor
//! this consumer obeys. The file therefore belongs inside the same
//! OS-ownership boundary as the serial device (ADR-0033 layer 2: dedicated
//! user/group; the Tier-3 startup sentinel asserts the device node, and the
//! pin should live in that user's config dir). Writes are atomic
//! (temp file + rename in the same directory) so a torn write can never
//! half-pin.
//!
//! ## First-provisioning assumption (reported per the task)
//!
//! `enroll` assumes an OPERATOR-AUTHENTICATED channel already exists to the
//! consumer host (the same assumption `kirra-ota-ctl enroll` makes: you can
//! run a CLI as the right user on the right machine). It does NOT bootstrap
//! trust from nothing — a consumer with no pin refuses everything, and the
//! first pin is only as trustworthy as the channel that delivered the key
//! hex. Closing that (signed enrollment bundles / attested first-boot) is a
//! tracked follow-up, not silently assumed away.

use std::io::Write as _;
use std::path::Path;

use ed25519_dalek::VerifyingKey;
use kirra_release_token::ros_twist::verifying_key_id_hex;

/// What an enrollment attempt did. Every variant is explicit — there is no
/// silent overwrite path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollOutcome {
    /// No pin existed; the key is now pinned.
    Enrolled { key_id: String },
    /// The exact same key was already pinned — idempotent success, file
    /// untouched.
    AlreadyEnrolled { key_id: String },
    /// A DIFFERENT key was pinned and rotation was explicitly requested:
    /// the pin now names the new key. `previous_key_id` is logged for the
    /// audit trail.
    Rotated {
        previous_key_id: String,
        key_id: String,
    },
}

/// Why enrollment refused. Fail-closed: on any error the existing pin (if
/// any) is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollError {
    /// The offered key hex is malformed (not 64 hex chars / not a valid
    /// Ed25519 point).
    MalformedKey,
    /// A DIFFERENT key is already pinned and rotation was NOT requested.
    /// This is the refuses-to-overwrite-silently arm: changing which
    /// governor a consumer obeys must be an explicit act.
    DifferentKeyPinned {
        pinned_key_id: String,
        offered_key_id: String,
    },
    /// A pin file exists but does not parse as a key. Refused without
    /// `rotate` — an unreadable pin is evidence of tampering or corruption,
    /// and silently replacing evidence is wrong. `rotate` may overwrite it.
    ExistingPinUnreadable,
    /// Filesystem failure (io error text attached for the operator).
    Io(String),
}

fn parse_key_hex(key_hex: &str) -> Result<VerifyingKey, EnrollError> {
    let t = key_hex.trim();
    // LOWERCASE hex only, exactly as the module docs pin the format: a
    // manually edited pin (e.g. uppercase) is treated as tamper/corruption
    // evidence rather than silently normalized (Copilot review on #892).
    if t.len() != 64
        || !t
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(EnrollError::MalformedKey);
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in t.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or(EnrollError::MalformedKey)?;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or(EnrollError::MalformedKey)?;
        bytes[i] = ((hi << 4) | lo) as u8;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| EnrollError::MalformedKey)
}

fn key_to_hex(vk: &VerifyingKey) -> String {
    let mut out = String::with_capacity(64);
    for b in vk.as_bytes() {
        use core::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Load the pinned governor verifying key a consumer starts from.
/// Fail-closed: missing or malformed pin → error → the consumer must refuse
/// to start (no pin means no fence).
pub fn load_pinned_verifying_key(path: &Path) -> Result<VerifyingKey, EnrollError> {
    let content = std::fs::read_to_string(path).map_err(|e| EnrollError::Io(e.to_string()))?;
    parse_key_hex(&content)
}

/// Pin `key_hex` at `path`. Idempotent (same key → `AlreadyEnrolled`);
/// refuses to overwrite a different or unreadable pin unless `rotate` is
/// explicitly set. The write is atomic (temp + rename, same directory).
pub fn enroll_verifying_key(
    path: &Path,
    key_hex: &str,
    rotate: bool,
) -> Result<EnrollOutcome, EnrollError> {
    let offered = parse_key_hex(key_hex)?;
    let offered_id = verifying_key_id_hex(&offered);

    let previous = match std::fs::read_to_string(path) {
        Ok(existing) => match parse_key_hex(&existing) {
            Ok(pinned) => {
                if pinned == offered {
                    return Ok(EnrollOutcome::AlreadyEnrolled { key_id: offered_id });
                }
                if !rotate {
                    return Err(EnrollError::DifferentKeyPinned {
                        pinned_key_id: verifying_key_id_hex(&pinned),
                        offered_key_id: offered_id,
                    });
                }
                Some(verifying_key_id_hex(&pinned))
            }
            Err(_) if !rotate => return Err(EnrollError::ExistingPinUnreadable),
            Err(_) => None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(EnrollError::Io(e.to_string())),
    };

    // Atomic pin: write a sibling temp file (unique name — concurrent
    // enrollments or a leftover tmp must not collide), 0600 (the pin is
    // integrity-critical; do not let a permissive umask make it
    // group-writable), fsync the file, rename over the target, then fsync
    // the DIRECTORY so the rename itself is durable (Copilot review, #892).
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let unique = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{}-{nanos}", std::process::id())
    };
    let tmp = dir.join(format!(
        ".{}.enroll-tmp.{unique}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "governor_vk".to_string())
    ));
    let write = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(key_to_hex(&offered).as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        // Durability of the rename: fsync the containing directory (Unix).
        #[cfg(unix)]
        {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(EnrollError::Io(e.to_string()));
    }

    Ok(match previous {
        Some(previous_key_id) => EnrollOutcome::Rotated {
            previous_key_id,
            key_id: offered_id,
        },
        None => EnrollOutcome::Enrolled { key_id: offered_id },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn key_hex(seed: u8) -> String {
        key_to_hex(&SigningKey::from_bytes(&[seed; 32]).verifying_key())
    }

    /// Per-test unique path under the system temp dir (the provisioning-test
    /// pattern: no tempfile dev-dep, no env mutation).
    fn pin_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kirra-enroll-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn first_enroll_then_idempotent_then_loadable() {
        let p = pin_path("first");
        let _ = std::fs::remove_file(&p);
        let out = enroll_verifying_key(&p, &key_hex(1), false).unwrap();
        assert!(matches!(out, EnrollOutcome::Enrolled { .. }));
        // Idempotent: same key again is success, not an overwrite refusal.
        let again = enroll_verifying_key(&p, &key_hex(1), false).unwrap();
        assert!(matches!(again, EnrollOutcome::AlreadyEnrolled { .. }));
        // The consumer can start from it.
        let loaded = load_pinned_verifying_key(&p).unwrap();
        assert_eq!(loaded, SigningKey::from_bytes(&[1; 32]).verifying_key());
        let _ = std::fs::remove_file(&p);
    }

    /// The refuses-to-overwrite-silently arm: a different key needs `rotate`.
    #[test]
    fn different_key_is_refused_without_explicit_rotate() {
        let p = pin_path("rotate");
        let _ = std::fs::remove_file(&p);
        enroll_verifying_key(&p, &key_hex(1), false).unwrap();
        let refused = enroll_verifying_key(&p, &key_hex(2), false);
        assert!(matches!(
            refused,
            Err(EnrollError::DifferentKeyPinned { .. })
        ));
        // Pin unchanged after the refusal.
        assert_eq!(
            load_pinned_verifying_key(&p).unwrap(),
            SigningKey::from_bytes(&[1; 32]).verifying_key()
        );
        // Explicit rotation succeeds and reports the previous key id.
        let rotated = enroll_verifying_key(&p, &key_hex(2), true).unwrap();
        match rotated {
            EnrollOutcome::Rotated {
                previous_key_id, ..
            } => assert_eq!(
                previous_key_id,
                verifying_key_id_hex(&SigningKey::from_bytes(&[1; 32]).verifying_key())
            ),
            other => panic!("expected Rotated, got {other:?}"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn malformed_and_corrupt_pins_fail_closed() {
        let p = pin_path("corrupt");
        let _ = std::fs::remove_file(&p);
        // Malformed offered key.
        assert_eq!(
            enroll_verifying_key(&p, "not-hex", false),
            Err(EnrollError::MalformedKey)
        );
        assert_eq!(
            enroll_verifying_key(&p, &"ab".repeat(31), false),
            Err(EnrollError::MalformedKey)
        );
        // Corrupt existing pin: load refuses; enroll refuses without rotate.
        std::fs::write(&p, "garbage").unwrap();
        assert!(load_pinned_verifying_key(&p).is_err());
        assert_eq!(
            enroll_verifying_key(&p, &key_hex(1), false),
            Err(EnrollError::ExistingPinUnreadable)
        );
        // rotate may replace the corrupt pin.
        assert!(matches!(
            enroll_verifying_key(&p, &key_hex(1), true),
            Ok(EnrollOutcome::Enrolled { .. })
        ));
        let _ = std::fs::remove_file(&p);
    }

    /// Copilot (#892): uppercase hex is refused (the documented format is
    /// lowercase; a hand-edited pin reads as tamper evidence, not silently
    /// normalized), and the written pin is 0600 regardless of umask.
    #[test]
    fn uppercase_hex_is_refused_and_pin_mode_is_0600() {
        let p = pin_path("case-mode");
        let _ = std::fs::remove_file(&p);
        let upper = key_hex(1).to_uppercase();
        assert_eq!(
            enroll_verifying_key(&p, &upper, false),
            Err(EnrollError::MalformedKey)
        );
        enroll_verifying_key(&p, &key_hex(1), false).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "pin must be 0600, got {mode:o}");
        }
        // An uppercase pin on disk is unreadable = tamper evidence.
        std::fs::write(&p, key_hex(1).to_uppercase()).unwrap();
        assert!(load_pinned_verifying_key(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_pin_fails_closed_on_load() {
        let p = pin_path("missing");
        let _ = std::fs::remove_file(&p);
        assert!(load_pinned_verifying_key(&p).is_err());
    }
}
