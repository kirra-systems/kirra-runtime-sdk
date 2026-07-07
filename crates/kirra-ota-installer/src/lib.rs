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

pub mod nvbootctrl;
pub use nvbootctrl::{NvbootctrlBootController, NvbootctrlRunner, SystemNvbootctrl};

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
    /// WP-12: a release verifying key is provisioned on this installer, so an
    /// UNSIGNED stage is refused — hash-only staging is the legacy mode and is
    /// rejected once the node knows the governor release key.
    SignatureRequired,
    /// WP-12: the artifact-release signature failed verification (malformed,
    /// wrong key, or signed over a different digest). The slot is never armed.
    ArtifactSignatureInvalid,
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
            InstallError::SignatureRequired => write!(
                f,
                "a release key is provisioned: unsigned (hash-only) staging is refused"
            ),
            InstallError::ArtifactSignatureInvalid => {
                write!(f, "artifact release signature invalid — refusing to stage")
            }
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
    /// WP-12 — the pinned governor artifact-release verifying key. `None` =
    /// legacy hash-only mode (pre-provisioning); once `Some`, EVERY stage must
    /// carry a valid signature ([`stage_verified_signed`](Self::stage_verified_signed))
    /// and the unsigned path is refused fail-closed.
    release_vk: Option<ed25519_dalek::VerifyingKey>,
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
            release_vk: None,
        })
    }

    /// WP-12 — provision the governor artifact-release verifying key. From
    /// this point EVERY stage must carry a valid release signature
    /// ([`stage_verified_signed`](Self::stage_verified_signed)); the unsigned
    /// [`stage_verified`](Self::stage_verified) path is refused
    /// (`SignatureRequired`). Provisioning is one-way by design — an
    /// installer never downgrades to hash-only once it knows the key.
    #[must_use]
    pub fn with_release_key(mut self, release_vk: ed25519_dalek::VerifyingKey) -> Self {
        self.release_vk = Some(release_vk);
        self
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
        // WP-12: once the release key is provisioned, hash-only staging is
        // the rejected legacy mode — a digest names WHICH bytes but proves
        // nothing about WHO released them.
        if self.release_vk.is_some() {
            return Err(InstallError::SignatureRequired);
        }
        verify_staged_artifact(artifact_path, expected_digest)?;
        let target = active.other();
        self.state = InstallState::Staged { active, target };
        Ok(target)
    }

    /// WP-12 — verify-then-stage with the governor artifact-release
    /// SIGNATURE: the artifact's SHA-256 must equal `expected_digest` AND
    /// `signature_b64` must verify over that digest against the provisioned
    /// release key. Any failure (no key provisioned, malformed/mismatched
    /// digest, bad signature) leaves the state machine in `Idle` — the slot
    /// is never armed by an artifact whose release provenance is unproven.
    pub fn stage_verified_signed(
        &mut self,
        artifact_path: &Path,
        expected_digest: &str,
        signature_b64: &str,
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
        // A signed stage REQUIRES the key: verifying against nothing is not a
        // pass. (An unprovisioned node uses the legacy path explicitly.)
        let vk = self.release_vk.as_ref().ok_or(InstallError::SignatureRequired)?;
        kirra_release_token::artifact_release::verify_artifact_release(
            expected_digest,
            signature_b64,
            vk,
        )
        .map_err(|_| InstallError::ArtifactSignatureInvalid)?;
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
///
/// `trying` is the persisted analogue of [`InstallState::Trying`]'s target: the
/// slot whose ONE-SHOT trial the launcher is currently in. It lets a per-invocation
/// launcher/CLI (which has no in-memory `Installer`) both (a) enforce one-shot —
/// after consuming `try_boot` the launcher records `trying`, so a crash-restart
/// runs `active` again (auto-rollback) — and (b) know which slot to make active on
/// `commit`. `#[serde(default)]` keeps older two-field records readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootRecord {
    pub active: Slot,
    pub try_boot: Option<Slot>,
    #[serde(default)]
    pub trying: Option<Slot>,
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
                trying: None,
            };
            write_record(&path, &rec)?;
        }
        Ok(Self { path })
    }

    pub fn record(&self) -> std::io::Result<BootRecord> {
        read_record(&self.path)
    }

    /// Atomically persist a full boot record. The app-level launcher/CLI uses this
    /// to apply the [`plan_run`]/[`plan_commit`]/[`plan_rollback`]/[`plan_stage`]
    /// transitions, which manage `trying` beyond the trait's active/try_boot ops.
    pub fn write(&mut self, rec: &BootRecord) -> std::io::Result<()> {
        write_record(&self.path, rec)
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
                trying: None,
            },
        )
    }
    fn rollback(&mut self, slot: Slot) -> std::io::Result<()> {
        write_record(
            &self.path,
            &BootRecord {
                active: slot,
                try_boot: None,
                trying: None,
            },
        )
    }
}

/// Which slot the launcher should run for `rec`, and the record it must persist
/// FIRST (one-shot enforcement). Pure:
/// - `try_boot = Some(t)` → the first trial boot: run `t`, and consume the flag
///   into `trying` so a crash-restart runs `active` again (auto-rollback);
/// - else `trying = Some(_)` → the trial slot already had its one shot and we're
///   back here → run `active` and clear `trying` (the rollback is complete);
/// - else → steady state, run `active`.
pub fn plan_run(rec: &BootRecord) -> (Slot, BootRecord) {
    if let Some(t) = rec.try_boot {
        (
            t,
            BootRecord {
                active: rec.active,
                try_boot: None,
                trying: Some(t),
            },
        )
    } else if rec.trying.is_some() {
        (
            rec.active,
            BootRecord {
                active: rec.active,
                try_boot: None,
                trying: None,
            },
        )
    } else {
        (rec.active, rec.clone())
    }
}

/// Stage a new artifact into the inactive slot: arm `try_boot` on it. Pure record
/// transition (the caller does the verify + copy first).
///
/// FAIL-CLOSED: refuses when a stage or trial is ALREADY in flight (`try_boot` or
/// `trying` set). Re-arming mid-trial would grant the target extra trial boots and
/// break the one-shot rollback guarantee — the caller must `commit` or `rollback`
/// the in-progress cycle first.
pub fn plan_stage(rec: &BootRecord) -> Result<BootRecord, InstallError> {
    if rec.try_boot.is_some() || rec.trying.is_some() {
        return Err(InstallError::InvalidTransition {
            state: "in-progress",
            action: "stage",
        });
    }
    Ok(BootRecord {
        active: rec.active,
        try_boot: Some(rec.active.other()),
        trying: None,
    })
}

/// Commit the in-progress trial as the new active. `Err` if no trial is in
/// progress (`trying` is `None`) — nothing to commit.
pub fn plan_commit(rec: &BootRecord) -> Result<BootRecord, InstallError> {
    match rec.trying {
        Some(t) => Ok(BootRecord {
            active: t,
            try_boot: None,
            trying: None,
        }),
        None => Err(InstallError::InvalidTransition {
            state: "no-trial",
            action: "commit",
        }),
    }
}

/// Abandon any staged/trial state and stay on the current active slot.
pub fn plan_rollback(rec: &BootRecord) -> BootRecord {
    BootRecord {
        active: rec.active,
        try_boot: None,
        trying: None,
    }
}

/// The exact byte payload a node signs (with its attestation key) to make an OTA
/// adoption report unforgeable — **byte-identical** to the verifier's
/// `kirra_verifier::attestation::adoption_report_signing_payload`. Domain-separated,
/// length-prefixed on the two string fields (u64 LE length then bytes), with
/// `reported_at_ms` appended as a fixed u64 LE. A drift from the server is caught by
/// the byte-pinned `adoption_report_payload_is_byte_stable` test.
pub fn adoption_report_payload(node_id: &str, applied_digest: &str, reported_at_ms: u64) -> Vec<u8> {
    const DOMAIN: &[u8] = b"KIRRA-ADOPTION-REPORT-v1";
    let mut p =
        Vec::with_capacity(DOMAIN.len() + 16 + node_id.len() + applied_digest.len() + 8);
    p.extend_from_slice(DOMAIN);
    for field in [node_id, applied_digest] {
        p.extend_from_slice(&(field.len() as u64).to_le_bytes());
        p.extend_from_slice(field.as_bytes());
    }
    p.extend_from_slice(&reported_at_ms.to_le_bytes());
    p
}

/// A consecutive-success health gate for the app-level probe agent (WS-4). A trial
/// slot is declared healthy — and thus committable — only after `required_streak`
/// CONSECUTIVE healthy samples: a single lucky sample never commits, and any failure
/// resets the streak to zero, so a flapping trial can never accumulate credit toward
/// a commit. This mirrors the fleet's AV recovery hysteresis (a streak of healthy
/// reports) applied to the OTA health gate.
///
/// Pure and wall-clock-free: the CALLER owns the sampling cadence and the overall
/// window deadline (commit on a healthy verdict; roll back when the window expires
/// without one). This type only folds boolean samples into the streak verdict, so it
/// unit-tests without a clock, a process, or a device — the same doer/checker
/// discipline as the rest of the installer.
#[derive(Debug, Clone)]
pub struct HealthGate {
    required_streak: u32,
    streak: u32,
}

impl HealthGate {
    /// `required_streak` is clamped to `>= 1` — a zero requirement would "commit"
    /// before any probe ran, defeating the gate.
    pub fn new(required_streak: u32) -> Self {
        Self {
            required_streak: required_streak.max(1),
            streak: 0,
        }
    }

    /// The current consecutive-healthy streak (for progress logging).
    pub fn streak(&self) -> u32 {
        self.streak
    }

    /// The streak length required to declare healthy.
    pub fn required_streak(&self) -> u32 {
        self.required_streak
    }

    /// Fold one health sample. Returns `Some(true)` once the required consecutive
    /// streak is reached (the caller should COMMIT the trial); otherwise `None`
    /// (keep probing until the window expires, then ROLL BACK). A `false` sample
    /// resets the streak to zero.
    pub fn observe(&mut self, healthy: bool) -> Option<bool> {
        if healthy {
            self.streak = self.streak.saturating_add(1);
            if self.streak >= self.required_streak {
                return Some(true);
            }
        } else {
            self.streak = 0;
        }
        None
    }
}

/// A node's campaign assignment as returned by the verifier's
/// `GET /fleet/campaigns/assignment/{node_id}` — the SUBSET the installer needs.
/// Every field defaults, and unknown fields are ignored, so a partial response (or
/// one the verifier later extends) still deserializes. The installer never depends
/// on the verifier crate; this is a decoupled wire mirror.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AssignmentView {
    /// `true` iff the node is inside a matching campaign's rolled percentage.
    #[serde(default)]
    pub rolled: bool,
    /// The signed artifact digest the node should run (`None`/absent → stay on the
    /// current/baseline artifact).
    #[serde(default)]
    pub artifact_digest: Option<String>,
    /// WP-12 — the governor artifact-release signature over `artifact_digest`
    /// (base64 Ed25519). Absent on legacy/pre-WP-12 assignments; a node with a
    /// provisioned release key refuses to stage without it.
    #[serde(default)]
    pub artifact_signature_b64: Option<String>,
    #[serde(default)]
    pub artifact_version: Option<String>,
    #[serde(default)]
    pub campaign_id: Option<String>,
}

/// What the pull agent should do after reconciling an [`AssignmentView`] against the
/// node's current on-disk state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullAction {
    /// Nothing to stage: not rolled, no assigned digest, already running the assigned
    /// digest, or a trial is already in flight.
    UpToDate,
    /// Fetch this signed artifact, verify its SHA-256, and stage it into the inactive
    /// slot. `digest` is the content-addressed identity (SHA-256 hex).
    Stage {
        digest: String,
        /// WP-12 — the release signature to verify at stage time (absent on a
        /// legacy assignment; a key-provisioned node then refuses to stage).
        signature_b64: Option<String>,
        version: Option<String>,
        campaign_id: Option<String>,
    },
}

/// Reconcile a fetched assignment against the node's current state into a
/// [`PullAction`]. Pure + idempotent + FAIL-CLOSED:
///
/// - a trial already in flight (`trial_in_flight`) → `UpToDate`: never re-stage
///   mid-cycle (matches [`plan_stage`]'s guard) — let the trial commit or roll back;
/// - not rolled, or no assigned digest → `UpToDate`: stay on the baseline artifact;
/// - the assigned digest already equals `current_digest` (the SHA-256 of the running
///   slot's artifact) → `UpToDate`: idempotent, so a periodic poll never re-stages
///   an already-applied update;
/// - otherwise → `Stage` the assigned digest.
///
/// Because the assigned digest is content-addressed and compared to a hash of the
/// running artifact, the agent needs no persisted "last applied" bookkeeping — the
/// filesystem IS the state.
pub fn decide_pull(
    assignment: &AssignmentView,
    current_digest: Option<&str>,
    trial_in_flight: bool,
) -> PullAction {
    if trial_in_flight {
        return PullAction::UpToDate;
    }
    let digest = match assignment.artifact_digest.as_deref() {
        Some(d) if assignment.rolled && !d.is_empty() => d,
        _ => return PullAction::UpToDate,
    };
    if current_digest == Some(digest) {
        return PullAction::UpToDate;
    }
    PullAction::Stage {
        digest: digest.to_string(),
        signature_b64: assignment.artifact_signature_b64.clone(),
        version: assignment.artifact_version.clone(),
        campaign_id: assignment.campaign_id.clone(),
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
                try_boot: Some(Slot::B),
                trying: None,
            }
        );
        inst.report_health(true).unwrap();
        // Committed: B active, trial cleared, and it survives a re-open.
        assert_eq!(
            inst.boot_controller().record().unwrap(),
            BootRecord {
                active: Slot::B,
                try_boot: None,
                trying: None,
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
                    try_boot: None,
                    trying: None,
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

    // --- app-level A/B launcher/CLI transitions (plan_*) -------------------

    fn rec(active: Slot, try_boot: Option<Slot>, trying: Option<Slot>) -> BootRecord {
        BootRecord {
            active,
            try_boot,
            trying,
        }
    }

    #[test]
    fn plan_run_steady_state_runs_active() {
        let (slot, new) = plan_run(&rec(Slot::A, None, None));
        assert_eq!(slot, Slot::A);
        assert_eq!(new, rec(Slot::A, None, None), "no change in steady state");
    }

    #[test]
    fn plan_run_first_trial_consumes_try_boot_into_trying() {
        // stage armed try_boot=B; the first run boots B one-shot and records the trial.
        let (slot, new) = plan_run(&rec(Slot::A, Some(Slot::B), None));
        assert_eq!(slot, Slot::B, "the trial slot runs");
        assert_eq!(
            new,
            rec(Slot::A, None, Some(Slot::B)),
            "one-shot consumed; active still A"
        );
    }

    #[test]
    fn plan_run_after_failed_trial_auto_rolls_back_to_active() {
        // The trial slot crashed/exited without commit → this run reverts to active.
        let (slot, new) = plan_run(&rec(Slot::A, None, Some(Slot::B)));
        assert_eq!(slot, Slot::A, "auto-rollback runs the previous active");
        assert_eq!(new, rec(Slot::A, None, None), "trial cleared");
    }

    #[test]
    fn full_app_level_cycle_commit() {
        // stage → run(trial) → commit.
        let staged = plan_stage(&rec(Slot::A, None, None)).unwrap();
        assert_eq!(staged, rec(Slot::A, Some(Slot::B), None));
        let (slot, trialing) = plan_run(&staged);
        assert_eq!(slot, Slot::B);
        let committed = plan_commit(&trialing).unwrap();
        assert_eq!(committed, rec(Slot::B, None, None), "B is the new active");
        // A subsequent run steadily runs B.
        assert_eq!(plan_run(&committed).0, Slot::B);
    }

    #[test]
    fn full_app_level_cycle_rollback() {
        let staged = plan_stage(&rec(Slot::A, None, None)).unwrap();
        let (_slot, trialing) = plan_run(&staged); // now trying=Some(B), active A
                                                   // Operator rolls back (or the trial crashed): stay on A.
        let rolled = plan_rollback(&trialing);
        assert_eq!(rolled, rec(Slot::A, None, None));
        assert_eq!(plan_run(&rolled).0, Slot::A);
    }

    #[test]
    fn stage_refused_while_a_trial_is_in_flight() {
        // Re-staging mid-cycle would grant extra trial boots → refused fail-closed.
        assert!(matches!(
            plan_stage(&rec(Slot::A, Some(Slot::B), None)), // already staged
            Err(InstallError::InvalidTransition { .. })
        ));
        assert!(matches!(
            plan_stage(&rec(Slot::A, None, Some(Slot::B))), // trial in progress
            Err(InstallError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn commit_without_a_trial_is_refused() {
        assert!(matches!(
            plan_commit(&rec(Slot::A, None, None)),
            Err(InstallError::InvalidTransition { .. })
        ));
    }

    // --- adoption report signing payload ---------------------------------

    #[test]
    fn adoption_report_payload_is_byte_stable() {
        // PIN the exact bytes so any drift from the server's
        // attestation::adoption_report_signing_payload is caught here. Layout:
        // domain || u64_le(len(node)) || node || u64_le(len(digest)) || digest || u64_le(ts).
        let p = adoption_report_payload("n1", "ab", 1);
        let mut want = Vec::new();
        want.extend_from_slice(b"KIRRA-ADOPTION-REPORT-v1");
        want.extend_from_slice(&2u64.to_le_bytes());
        want.extend_from_slice(b"n1");
        want.extend_from_slice(&2u64.to_le_bytes());
        want.extend_from_slice(b"ab");
        want.extend_from_slice(&1u64.to_le_bytes());
        assert_eq!(p, want);
    }

    // --- health gate (probe agent) ---------------------------------------

    #[test]
    fn health_gate_clamps_zero_requirement_to_one() {
        // A zero streak would commit before any probe — clamp to 1.
        let mut g = HealthGate::new(0);
        assert_eq!(g.required_streak(), 1);
        assert_eq!(g.observe(true), Some(true), "one healthy sample commits");
    }

    #[test]
    fn health_gate_requires_consecutive_successes() {
        let mut g = HealthGate::new(3);
        assert_eq!(g.observe(true), None, "1/3");
        assert_eq!(g.observe(true), None, "2/3");
        assert_eq!(g.observe(true), Some(true), "3/3 → healthy");
    }

    #[test]
    fn health_gate_failure_resets_the_streak() {
        let mut g = HealthGate::new(3);
        g.observe(true);
        g.observe(true);
        assert_eq!(g.streak(), 2);
        // A single failure wipes the accumulated credit.
        assert_eq!(g.observe(false), None);
        assert_eq!(g.streak(), 0, "streak reset on failure");
        // Must earn the full streak again from scratch.
        assert_eq!(g.observe(true), None);
        assert_eq!(g.observe(true), None);
        assert_eq!(g.observe(true), Some(true));
    }

    #[test]
    fn health_gate_flapping_never_commits() {
        // Alternating healthy/unhealthy never reaches a 2-streak → no false commit.
        let mut g = HealthGate::new(2);
        for _ in 0..10 {
            assert_eq!(g.observe(true), None);
            assert_eq!(g.observe(false), None);
        }
        assert_eq!(g.streak(), 0);
    }

    // --- WP-12: signature-enforced staging (tamper suite) -----------------

    fn release_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[42u8; 32])
    }

    fn signed_installer() -> Installer<InMemoryBootController> {
        Installer::new(InMemoryBootController::new(Slot::A), 3)
            .unwrap()
            .with_release_key(release_key().verifying_key())
    }

    fn sign_digest(digest: &str, key: &ed25519_dalek::SigningKey) -> String {
        kirra_release_token::artifact_release::sign_artifact_release(digest, key).unwrap()
    }

    /// Happy path: a correctly-signed artifact stages into the inactive slot.
    #[test]
    fn signed_stage_with_valid_signature_arms_the_slot() {
        let p = write_temp("signed_ok", b"hello");
        let mut inst = signed_installer();
        let sig = sign_digest(DIGEST_HELLO, &release_key());
        let target = inst.stage_verified_signed(&p, DIGEST_HELLO, &sig).unwrap();
        assert_eq!(target, Slot::B);
        assert!(matches!(inst.state(), InstallState::Staged { .. }));
    }

    /// Once the release key is provisioned, the hash-only legacy path is a
    /// rejected mode — a digest proves WHICH bytes, not WHO released them.
    #[test]
    fn provisioned_key_refuses_unsigned_stage() {
        let p = write_temp("signed_legacy", b"hello");
        let mut inst = signed_installer();
        assert!(matches!(
            inst.stage(&p, DIGEST_HELLO),
            Err(InstallError::SignatureRequired)
        ));
        assert!(matches!(inst.state(), InstallState::Idle { .. }), "slot never armed");
    }

    /// A signature from the WRONG key never arms the slot.
    #[test]
    fn wrong_key_signature_never_arms() {
        let p = write_temp("signed_wrongkey", b"hello");
        let mut inst = signed_installer();
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let sig = sign_digest(DIGEST_HELLO, &attacker);
        assert!(matches!(
            inst.stage_verified_signed(&p, DIGEST_HELLO, &sig),
            Err(InstallError::ArtifactSignatureInvalid)
        ));
        assert!(matches!(inst.state(), InstallState::Idle { .. }));
    }

    /// A VALID signature over a DIFFERENT digest never arms the slot (a
    /// replayed release for artifact X cannot bless artifact Y).
    #[test]
    fn signature_over_different_digest_never_arms() {
        let p = write_temp("signed_replay", b"hello");
        let mut inst = signed_installer();
        let other = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let sig = sign_digest(other, &release_key());
        assert!(matches!(
            inst.stage_verified_signed(&p, DIGEST_HELLO, &sig),
            Err(InstallError::ArtifactSignatureInvalid)
        ));
        assert!(matches!(inst.state(), InstallState::Idle { .. }));
    }

    /// Right signature, TAMPERED bytes: the digest check still refuses — the
    /// signature blesses a digest, the bytes must still equal it.
    #[test]
    fn valid_signature_with_tampered_bytes_never_arms() {
        let p = write_temp("signed_tampered", b"HELLO-TAMPERED");
        let mut inst = signed_installer();
        let sig = sign_digest(DIGEST_HELLO, &release_key());
        assert!(matches!(
            inst.stage_verified_signed(&p, DIGEST_HELLO, &sig),
            Err(InstallError::DigestMismatch { .. })
        ));
        assert!(matches!(inst.state(), InstallState::Idle { .. }));
    }

    /// Without a provisioned key, the SIGNED path refuses too — verifying
    /// against nothing is not a pass (an unprovisioned node stays on the
    /// explicit legacy path).
    #[test]
    fn signed_stage_without_provisioned_key_is_refused() {
        let p = write_temp("signed_nokey", b"hello");
        let mut inst = Installer::new(InMemoryBootController::new(Slot::A), 3).unwrap();
        let sig = sign_digest(DIGEST_HELLO, &release_key());
        assert!(matches!(
            inst.stage_verified_signed(&p, DIGEST_HELLO, &sig),
            Err(InstallError::SignatureRequired)
        ));
    }

    /// The pull reconciler threads the assignment's signature into the action.
    #[test]
    fn decide_pull_carries_the_release_signature() {
        let mut a = assign(true, Some(DIGEST_HELLO));
        a.artifact_signature_b64 = Some("c2ln".into());
        match decide_pull(&a, Some("something-else"), false) {
            PullAction::Stage { signature_b64, .. } => {
                assert_eq!(signature_b64.as_deref(), Some("c2ln"));
            }
            other => panic!("expected Stage, got {other:?}"),
        }
    }

    // --- pull reconciler (HTTP agent) ------------------------------------

    fn assign(rolled: bool, digest: Option<&str>) -> AssignmentView {
        AssignmentView {
            rolled,
            artifact_digest: digest.map(String::from),
            artifact_signature_b64: None,
            artifact_version: Some("v2".into()),
            campaign_id: Some("camp-1".into()),
        }
    }

    #[test]
    fn pull_not_rolled_stays_on_baseline() {
        assert_eq!(
            decide_pull(&assign(false, Some(DIGEST_HELLO)), None, false),
            PullAction::UpToDate,
            "not rolled → never stage, even with a digest present"
        );
    }

    #[test]
    fn pull_rolled_with_new_digest_stages() {
        let action = decide_pull(&assign(true, Some(DIGEST_HELLO)), Some("old"), false);
        assert_eq!(
            action,
            PullAction::Stage {
                digest: DIGEST_HELLO.to_string(),
                signature_b64: None,
                version: Some("v2".into()),
                campaign_id: Some("camp-1".into()),
            }
        );
        // Also stages when nothing is installed yet (current_digest None).
        assert!(matches!(
            decide_pull(&assign(true, Some(DIGEST_HELLO)), None, false),
            PullAction::Stage { .. }
        ));
    }

    #[test]
    fn pull_already_running_assigned_digest_is_idempotent() {
        assert_eq!(
            decide_pull(&assign(true, Some(DIGEST_HELLO)), Some(DIGEST_HELLO), false),
            PullAction::UpToDate,
            "assigned == running → no re-stage (idempotent poll)"
        );
    }

    #[test]
    fn pull_never_restages_mid_trial() {
        // Even a genuinely new digest is deferred while a cycle is in flight.
        assert_eq!(
            decide_pull(&assign(true, Some(DIGEST_HELLO)), Some("old"), true),
            PullAction::UpToDate
        );
    }

    #[test]
    fn pull_rolled_without_digest_is_noop() {
        assert_eq!(
            decide_pull(&assign(true, None), Some("old"), false),
            PullAction::UpToDate
        );
        // An empty-string digest is also refused (never a valid content address).
        assert_eq!(
            decide_pull(&assign(true, Some("")), Some("old"), false),
            PullAction::UpToDate
        );
    }

    #[test]
    fn assignment_view_deserializes_full_endpoint_json_and_ignores_extra() {
        // Mirrors the verifier's NodeAssignment JSON, plus an unknown field.
        let json = r#"{"node_id":"robot-01","rolled":true,"campaign_id":"camp-1",
            "artifact_digest":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "artifact_version":"v2","future_field":123}"#;
        let a: AssignmentView = serde_json::from_str(json).unwrap();
        assert!(a.rolled);
        assert_eq!(a.artifact_digest.as_deref(), Some(DIGEST_HELLO));
        assert_eq!(a.artifact_version.as_deref(), Some("v2"));
        // An unrolled response deserializes to a no-op assignment.
        let none: AssignmentView =
            serde_json::from_str(r#"{"node_id":"x","rolled":false}"#).unwrap();
        assert_eq!(decide_pull(&none, None, false), PullAction::UpToDate);
    }

    #[test]
    fn boot_record_deserializes_legacy_two_field_form() {
        // A pre-`trying` record (two fields) still parses — `trying` defaults None.
        let legacy = r#"{"active":"a","try_boot":"b"}"#;
        let rec: BootRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            rec,
            BootRecord {
                active: Slot::A,
                try_boot: Some(Slot::B),
                trying: None
            }
        );
    }
}
