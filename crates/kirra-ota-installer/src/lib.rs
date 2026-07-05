//! Node-side dual-slot (A/B) governor-artifact installer with health-gated
//! automatic rollback (WS-4 / Track 3 — the doer side of the Fleet Plane).
//!
//! This is the **device-agnostic core**. A node runs two governor-artifact slots;
//! one is active, the other receives an update. The install cycle is:
//!
//! 1. **stage** the new signed artifact into the inactive slot and VERIFY its
//!    SHA-256 against the campaign's signed digest — a mismatch fail-closes here,
//!    the slot is never armed;
//! 2. **begin_trial** — arm a one-shot trial boot into the target slot;
//! 3. **report_health** — after the target boots, its self-health is reported:
//!    healthy → **commit** (the target becomes the permanent active); unhealthy →
//!    retry up to a bounded number of boots, then **roll back** to the previous
//!    known-good slot automatically.
//!
//! The safety spine is the same doer/checker split as the rest of the stack: the
//! [`Installer`] state machine is the invariant (an unverified artifact never
//! boots; an unhealthy target never commits; exhausted retries always roll back),
//! and the one hardware-coupled seam is the [`BootController`] trait — the real
//! bootloader impl (Jetson `nvbootctrl` / EFI / a systemd active-slot flip) plugs
//! in behind it, so all of this tests without a device. Wall-clock-free.
//!
//! Out of scope here (follow-ups): the HTTP agent that polls the verifier's
//! `/fleet/campaigns/assignment/{node_id}` endpoint for the assigned digest, and
//! the concrete on-device `BootController` + the real reboot/rollback drill.

#![forbid(unsafe_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One of the two governor-artifact slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    A,
    B,
}

impl Slot {
    /// The other slot — the install target when `self` is active.
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }
}

/// The lifecycle state of an A/B install cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstallState {
    /// Steady state — `active` is running and committed; no install in flight.
    Idle { active: Slot },
    /// A verified artifact is staged into `target` (the inactive slot); not booted.
    Staged { active: Slot, target: Slot },
    /// Trial-booting `target`; awaiting a health report. `attempts` counts boots so
    /// far; `previous` is the slot rolled back to on failure.
    Trying {
        previous: Slot,
        target: Slot,
        attempts: u8,
    },
}

impl InstallState {
    /// The slot currently serving traffic: the active in `Idle`, the still-active
    /// (not-yet-committed) slot in `Staged`, and the `previous` (fallback) during a
    /// `Trying` trial — a trial that has not yet committed is still backed by
    /// `previous`.
    pub fn serving_slot(&self) -> Slot {
        match *self {
            InstallState::Idle { active } => active,
            InstallState::Staged { active, .. } => active,
            InstallState::Trying { previous, .. } => previous,
        }
    }
}

/// The result of a [`Installer::report_health`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthOutcome {
    /// The target reported healthy and was committed as the new active.
    Committed { active: Slot },
    /// The target was unhealthy but attempts remain — a further trial boot is armed.
    Retrying { attempts: u8 },
    /// Retries exhausted (or an unhealthy final attempt) — rolled back to `active`.
    RolledBack { active: Slot },
}

/// Installer errors. Fail-closed: an invalid transition or a digest mismatch is
/// refused, never coerced into a boot.
#[derive(Debug)]
pub enum InstallError {
    /// The staged artifact's SHA-256 did not match the expected signed digest.
    DigestMismatch { expected: String, actual: String },
    /// The expected digest was not a 64-char lowercase hex SHA-256.
    InvalidExpectedDigest,
    /// The requested action is not legal from the current state.
    InvalidTransition {
        state: &'static str,
        action: &'static str,
    },
    /// An I/O failure reading the artifact or driving the boot controller.
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::DigestMismatch { expected, actual } => write!(
                f,
                "staged artifact digest mismatch: expected {expected}, got {actual}"
            ),
            InstallError::InvalidExpectedDigest => {
                write!(f, "expected digest is not a 64-char lowercase hex sha256")
            }
            InstallError::InvalidTransition { state, action } => {
                write!(f, "cannot {action} from state {state}")
            }
            InstallError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// The one hardware-coupled seam. A `BootController` owns the platform's notion of
/// "which slot is active" and the one-shot trial-boot / commit / rollback
/// primitives the bootloader exposes. The real impl (Jetson `nvbootctrl`, EFI boot
/// entries, or an app-level systemd active-slot flip) implements this; the
/// [`Installer`] state machine drives it, so the safety logic is device-independent.
///
/// Contract: `set_try_boot(slot)` arms a ONE-SHOT boot into `slot` that the
/// bootloader must AUTOMATICALLY fall back from (to the other slot) if `commit` is
/// not called — this is what makes rollback safe against a target that fails to
/// boot at all, not just one that boots unhealthy.
pub trait BootController {
    /// The slot the platform currently considers active/committed.
    fn active_slot(&self) -> std::io::Result<Slot>;
    /// Arm a one-shot trial boot into `slot` (auto-fallback if not committed).
    fn set_try_boot(&mut self, slot: Slot) -> std::io::Result<()>;
    /// Make `slot` the permanent active and clear the trial flag (health confirmed).
    fn commit(&mut self, slot: Slot) -> std::io::Result<()>;
    /// Abandon the trial and return to `slot` (the previous known-good active).
    fn rollback(&mut self, slot: Slot) -> std::io::Result<()>;
}

/// The dual-slot installer state machine over a [`BootController`].
pub struct Installer<B: BootController> {
    boot: B,
    state: InstallState,
    max_boot_attempts: u8,
}

impl<B: BootController> Installer<B> {
    /// Build an installer seeded from the controller's current active slot (steady
    /// `Idle` state). `max_boot_attempts` (>= 1) bounds how many trial boots a
    /// target gets before an automatic rollback.
    pub fn new(boot: B, max_boot_attempts: u8) -> std::io::Result<Self> {
        let active = boot.active_slot()?;
        Ok(Self {
            boot,
            state: InstallState::Idle { active },
            max_boot_attempts: max_boot_attempts.max(1),
        })
    }

    pub fn state(&self) -> InstallState {
        self.state
    }

    pub fn boot_controller(&self) -> &B {
        &self.boot
    }

    /// Stage + verify a new artifact into the inactive slot. FAIL-CLOSED: the
    /// artifact's SHA-256 must equal `expected_digest` (the campaign's signed
    /// digest) or the slot is NOT armed and the state is unchanged. Legal only
    /// from `Idle`.
    pub fn stage(
        &mut self,
        artifact_path: &Path,
        expected_digest: &str,
    ) -> Result<Slot, InstallError> {
        let active = match self.state {
            InstallState::Idle { active } => active,
            InstallState::Staged { .. } => {
                return Err(InstallError::InvalidTransition {
                    state: "staged",
                    action: "stage",
                })
            }
            InstallState::Trying { .. } => {
                return Err(InstallError::InvalidTransition {
                    state: "trying",
                    action: "stage",
                })
            }
        };
        verify_staged_artifact(artifact_path, expected_digest)?;
        let target = active.other();
        self.state = InstallState::Staged { active, target };
        Ok(target)
    }

    /// Arm the trial boot into the staged target. The bootloader will boot the
    /// target next and auto-fall-back unless `report_health(true)` commits it.
    /// Legal only from `Staged`.
    pub fn begin_trial(&mut self) -> Result<Slot, InstallError> {
        match self.state {
            InstallState::Staged { active, target } => {
                self.boot.set_try_boot(target)?;
                self.state = InstallState::Trying {
                    previous: active,
                    target,
                    attempts: 1,
                };
                Ok(target)
            }
            InstallState::Idle { .. } => Err(InstallError::InvalidTransition {
                state: "idle",
                action: "begin_trial",
            }),
            InstallState::Trying { .. } => Err(InstallError::InvalidTransition {
                state: "trying",
                action: "begin_trial",
            }),
        }
    }

    /// Report the trial target's post-boot health. `true` → commit the target as
    /// the new active. `false` → arm another trial while attempts remain, else roll
    /// back to the previous known-good slot. Legal only from `Trying`.
    pub fn report_health(&mut self, healthy: bool) -> Result<HealthOutcome, InstallError> {
        match self.state {
            InstallState::Trying {
                previous,
                target,
                attempts,
            } => {
                if healthy {
                    self.boot.commit(target)?;
                    self.state = InstallState::Idle { active: target };
                    Ok(HealthOutcome::Committed { active: target })
                } else if attempts < self.max_boot_attempts {
                    // Re-arm the one-shot trial for another boot.
                    self.boot.set_try_boot(target)?;
                    let attempts = attempts + 1;
                    self.state = InstallState::Trying {
                        previous,
                        target,
                        attempts,
                    };
                    Ok(HealthOutcome::Retrying { attempts })
                } else {
                    // Retries exhausted → automatic rollback to the known-good slot.
                    self.boot.rollback(previous)?;
                    self.state = InstallState::Idle { active: previous };
                    Ok(HealthOutcome::RolledBack { active: previous })
                }
            }
            InstallState::Idle { .. } => Err(InstallError::InvalidTransition {
                state: "idle",
                action: "report_health",
            }),
            InstallState::Staged { .. } => Err(InstallError::InvalidTransition {
                state: "staged",
                action: "report_health",
            }),
        }
    }
}

/// A 64-char lowercase hex SHA-256.
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Compute the SHA-256 (lowercase hex) of a file's contents, streaming so a large
/// artifact is not held in memory.
pub fn artifact_sha256_hex(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verify a staged artifact file's SHA-256 equals `expected_digest`. Fail-closed:
/// a malformed expected digest, a read error, or ANY mismatch is an `Err` — the
/// caller must never treat an unverified artifact as installable.
pub fn verify_staged_artifact(path: &Path, expected_digest: &str) -> Result<(), InstallError> {
    if !is_sha256_hex(expected_digest) {
        return Err(InstallError::InvalidExpectedDigest);
    }
    let actual = artifact_sha256_hex(path)?;
    if actual != expected_digest {
        return Err(InstallError::DigestMismatch {
            expected: expected_digest.to_string(),
            actual,
        });
    }
    Ok(())
}

/// An in-memory [`BootController`] for tests — records the active slot, the armed
/// trial (if any), and the ordered history of operations so a test can assert the
/// exact bootloader calls the state machine made.
#[derive(Debug, Clone)]
pub struct InMemoryBootController {
    active: Slot,
    try_boot: Option<Slot>,
    pub ops: Vec<String>,
}

impl InMemoryBootController {
    pub fn new(active: Slot) -> Self {
        Self {
            active,
            try_boot: None,
            ops: Vec::new(),
        }
    }
    pub fn try_boot(&self) -> Option<Slot> {
        self.try_boot
    }
}

impl BootController for InMemoryBootController {
    fn active_slot(&self) -> std::io::Result<Slot> {
        Ok(self.active)
    }
    fn set_try_boot(&mut self, slot: Slot) -> std::io::Result<()> {
        self.try_boot = Some(slot);
        self.ops.push(format!("try_boot:{}", slot.as_str()));
        Ok(())
    }
    fn commit(&mut self, slot: Slot) -> std::io::Result<()> {
        self.active = slot;
        self.try_boot = None;
        self.ops.push(format!("commit:{}", slot.as_str()));
        Ok(())
    }
    fn rollback(&mut self, slot: Slot) -> std::io::Result<()> {
        self.active = slot;
        self.try_boot = None;
        self.ops.push(format!("rollback:{}", slot.as_str()));
        Ok(())
    }
}

/// The persisted record a [`FileBootController`] keeps — the app-level A/B boot
/// state a systemd unit / launcher reads to pick which governor slot to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootRecord {
    pub active: Slot,
    pub try_boot: Option<Slot>,
}

/// A file-backed [`BootController`] — persists the A/B boot record to a JSON file.
/// This is a real, platform-neutral controller for an **app-level** A/B scheme
/// (two governor install dirs + a launcher/systemd unit that reads the record and
/// runs `active`). It PERSISTS the intent (`active` + an armed `try_boot`); it does
/// NOT itself implement the one-shot semantics. The CONSUMER that reads the record
/// (the launcher / systemd / bootloader) MUST honour `set_try_boot`'s one-shot
/// contract: on seeing an armed `try_boot`, boot that slot but CLEAR the flag
/// before/at that boot so a target that fails to boot at all falls back to `active`
/// on the next attempt — exactly as a real bootloader's boot-count does. This
/// controller only records + durably persists the decision; it never self-clears.
/// The Jetson `nvbootctrl` / EFI rootfs-level controller is the hardware follow-up
/// (the bootloader enforces one-shot natively); it implements the same trait.
pub struct FileBootController {
    path: std::path::PathBuf,
}

impl FileBootController {
    /// Open (or initialize at `default_active`) a boot record at `path`.
    pub fn open(
        path: impl Into<std::path::PathBuf>,
        default_active: Slot,
    ) -> std::io::Result<Self> {
        let path = path.into();
        if !path.exists() {
            let rec = BootRecord {
                active: default_active,
                try_boot: None,
            };
            write_record(&path, &rec)?;
        }
        Ok(Self { path })
    }

    pub fn record(&self) -> std::io::Result<BootRecord> {
        read_record(&self.path)
    }
}

impl BootController for FileBootController {
    fn active_slot(&self) -> std::io::Result<Slot> {
        Ok(self.record()?.active)
    }
    fn set_try_boot(&mut self, slot: Slot) -> std::io::Result<()> {
        let mut rec = self.record()?;
        rec.try_boot = Some(slot);
        write_record(&self.path, &rec)
    }
    fn commit(&mut self, slot: Slot) -> std::io::Result<()> {
        write_record(
            &self.path,
            &BootRecord {
                active: slot,
                try_boot: None,
            },
        )
    }
    fn rollback(&mut self, slot: Slot) -> std::io::Result<()> {
        write_record(
            &self.path,
            &BootRecord {
                active: slot,
                try_boot: None,
            },
        )
    }
}

fn read_record(path: &Path) -> std::io::Result<BootRecord> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn write_record(path: &Path, rec: &BootRecord) -> std::io::Result<()> {
    use std::io::Write as _;
    let text = serde_json::to_string(rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // ATOMIC replace, not truncate-in-place: a launcher/bootloader reading the boot
    // record mid-write (or after a power loss during it) must NEVER observe a
    // torn/empty file — that would leave the node unable to decide which slot to
    // run. Write a sibling temp, fsync it, then `rename` over the target (atomic on
    // POSIX same-filesystem), then fsync the parent directory so the rename itself
    // is durable across power loss. The reader only ever sees the old record or the
    // complete new one.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);

    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;

    // Best-effort directory fsync so the rename is durable (Unix); harmless where a
    // directory handle can't be synced.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(dirf) = std::fs::File::open(dir) {
            let _ = dirf.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 64-char lowercase hex digests.
    const DIGEST_HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"; // sha256("hello")

    fn write_temp(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kirra_ota_{}_{}", std::process::id(), name));
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn slot_other_is_involutive() {
        assert_eq!(Slot::A.other(), Slot::B);
        assert_eq!(Slot::B.other(), Slot::A);
        assert_eq!(Slot::A.other().other(), Slot::A);
    }

    #[test]
    fn verify_accepts_matching_digest_and_rejects_mismatch() {
        let p = write_temp("hello", b"hello");
        assert!(verify_staged_artifact(&p, DIGEST_HELLO).is_ok());
        // Wrong digest → DigestMismatch.
        let wrong = "0".repeat(64);
        assert!(matches!(
            verify_staged_artifact(&p, &wrong),
            Err(InstallError::DigestMismatch { .. })
        ));
        // Malformed expected digest → InvalidExpectedDigest.
        assert!(matches!(
            verify_staged_artifact(&p, "nothex"),
            Err(InstallError::InvalidExpectedDigest)
        ));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn happy_path_stage_trial_commit() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap();
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::A });

        let p = write_temp("commit", b"hello");
        let target = inst.stage(&p, DIGEST_HELLO).unwrap();
        assert_eq!(target, Slot::B);
        assert_eq!(
            inst.state(),
            InstallState::Staged {
                active: Slot::A,
                target: Slot::B
            }
        );

        assert_eq!(inst.begin_trial().unwrap(), Slot::B);
        assert!(matches!(
            inst.state(),
            InstallState::Trying {
                target: Slot::B,
                attempts: 1,
                ..
            }
        ));

        // Healthy → commit; B becomes active.
        assert_eq!(
            inst.report_health(true).unwrap(),
            HealthOutcome::Committed { active: Slot::B }
        );
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::B });
        assert_eq!(inst.boot_controller().ops, vec!["try_boot:b", "commit:b"]);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn digest_mismatch_never_arms_the_slot() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap();
        let p = write_temp("bad", b"not-hello");
        assert!(inst.stage(&p, DIGEST_HELLO).is_err());
        // State unchanged; the boot controller was never touched.
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::A });
        assert!(inst.boot_controller().ops.is_empty());
        assert!(inst.boot_controller().try_boot().is_none());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unhealthy_retries_then_rolls_back() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap(); // 3 boot attempts
        let p = write_temp("rb", b"hello");
        inst.stage(&p, DIGEST_HELLO).unwrap();
        inst.begin_trial().unwrap(); // attempts = 1

        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::Retrying { attempts: 2 }
        );
        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::Retrying { attempts: 3 }
        );
        // Third unhealthy report exhausts attempts → rollback to A.
        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::RolledBack { active: Slot::A }
        );
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::A });
        assert_eq!(
            inst.boot_controller().ops,
            vec!["try_boot:b", "try_boot:b", "try_boot:b", "rollback:a"]
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn recovery_after_retry_still_commits() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap();
        let p = write_temp("recover", b"hello");
        inst.stage(&p, DIGEST_HELLO).unwrap();
        inst.begin_trial().unwrap();
        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::Retrying { attempts: 2 }
        );
        // Second boot is healthy → commit B (no rollback).
        assert_eq!(
            inst.report_health(true).unwrap(),
            HealthOutcome::Committed { active: Slot::B }
        );
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::B });
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn max_attempts_one_rolls_back_on_first_failure() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 1).unwrap(); // single attempt
        let p = write_temp("one", b"hello");
        inst.stage(&p, DIGEST_HELLO).unwrap();
        inst.begin_trial().unwrap();
        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::RolledBack { active: Slot::A }
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn invalid_transitions_are_refused() {
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap();
        // Can't begin_trial or report_health from Idle.
        assert!(matches!(
            inst.begin_trial(),
            Err(InstallError::InvalidTransition { .. })
        ));
        assert!(matches!(
            inst.report_health(true),
            Err(InstallError::InvalidTransition { .. })
        ));
        // After staging, can't stage again.
        let p = write_temp("inv", b"hello");
        inst.stage(&p, DIGEST_HELLO).unwrap();
        assert!(matches!(
            inst.stage(&p, DIGEST_HELLO),
            Err(InstallError::InvalidTransition { .. })
        ));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn serving_slot_is_never_the_unproven_target() {
        // During a trial, the serving slot stays the previous known-good one — the
        // target is not "serving" until it commits.
        let boot = InMemoryBootController::new(Slot::A);
        let mut inst = Installer::new(boot, 3).unwrap();
        let p = write_temp("serve", b"hello");
        inst.stage(&p, DIGEST_HELLO).unwrap();
        assert_eq!(inst.state().serving_slot(), Slot::A);
        inst.begin_trial().unwrap();
        assert_eq!(
            inst.state().serving_slot(),
            Slot::A,
            "trial target is not yet serving"
        );
        inst.report_health(true).unwrap();
        assert_eq!(
            inst.state().serving_slot(),
            Slot::B,
            "committed target now serves"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn file_boot_controller_persists_and_drives_a_full_cycle() {
        let path =
            std::env::temp_dir().join(format!("kirra_ota_bootrec_{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        let boot = FileBootController::open(&path, Slot::A).unwrap();
        let mut inst = Installer::new(boot, 2).unwrap();
        let p = write_temp("fbc", b"hello");

        inst.stage(&p, DIGEST_HELLO).unwrap();
        inst.begin_trial().unwrap();
        // The persisted record now arms a trial boot into B.
        assert_eq!(
            inst.boot_controller().record().unwrap(),
            BootRecord {
                active: Slot::A,
                try_boot: Some(Slot::B)
            }
        );
        inst.report_health(true).unwrap();
        // Committed: B active, trial cleared, and it survives a re-open.
        assert_eq!(
            inst.boot_controller().record().unwrap(),
            BootRecord {
                active: Slot::B,
                try_boot: None
            }
        );
        let reopened = FileBootController::open(&path, Slot::A).unwrap();
        assert_eq!(
            reopened.active_slot().unwrap(),
            Slot::B,
            "committed slot persisted"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn atomic_write_leaves_no_temp_and_record_stays_valid_across_rewrites() {
        // Each write is a temp+rename; after any number of writes the record is a
        // single complete file with no lingering ".tmp" sibling (the reader can
        // never see a torn/empty boot record).
        let path =
            std::env::temp_dir().join(format!("kirra_ota_atomic_{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        let mut boot = FileBootController::open(&path, Slot::A).unwrap();
        for _ in 0..5 {
            boot.set_try_boot(Slot::B).unwrap();
            boot.commit(Slot::B).unwrap();
            boot.rollback(Slot::A).unwrap();
            // The record parses to a complete, valid BootRecord every time.
            assert_eq!(
                boot.record().unwrap(),
                BootRecord {
                    active: Slot::A,
                    try_boot: None
                }
            );
        }
        let tmp = {
            let mut t = path.as_os_str().to_owned();
            t.push(".tmp");
            std::path::PathBuf::from(t)
        };
        assert!(
            !tmp.exists(),
            "the temp file must be consumed by the rename"
        );
        std::fs::remove_file(&path).ok();
    }
}
