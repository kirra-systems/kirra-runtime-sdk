//! Rootfs-level A/B [`BootController`] backed by the Jetson bootloader's NATIVE
//! slot machinery via `nvbootctrl` (WS-4 / Track 3 — the "generalize from the
//! governor app to the whole node image" step).
//!
//! Where the app-level scheme (two governor install dirs + a systemd unit + a JSON
//! boot record) trials a governor binary with a `systemctl restart`, the rootfs
//! scheme trials the WHOLE slot (bootloader + kernel + rootfs) with a REBOOT, and the
//! one-shot / auto-fallback is the bootloader's own retry-counter mechanism rather
//! than our `plan_run` record. The safety contract is identical — an unproven slot
//! never becomes permanently active until health is confirmed — so it plugs into the
//! same [`Installer`](crate::Installer) state machine through the same
//! [`BootController`] trait.
//!
//! ## The one hardware seam is the COMMAND RUNNER, not the logic
//!
//! `nvbootctrl` is a device utility that cannot run in CI or the sandbox, so this
//! module puts a [`NvbootctrlRunner`] trait between the controller and the process.
//! The real [`SystemNvbootctrl`] shells out to the binary; tests inject a scriptable
//! mock. That keeps the slot-mapping + command-sequence logic — the part that has to
//! be correct — fully unit-tested WITHOUT a Jetson, exactly like the rest of the
//! installer.
//!
//! ## Reboot-spanning caveat
//!
//! A rootfs trial spans a reboot, so an in-memory [`Installer`](crate::Installer)
//! cannot be held across it — the DURABLE trial state lives in the bootloader's slot
//! metadata (which slot is active, per-slot retry counters, the boot-successful bit),
//! queried via `nvbootctrl`. A real driver is therefore invoked in two phases:
//! pre-reboot (flash + verify the inactive slot, then [`set_try_boot`] to arm it) and
//! post-boot (health-probe, then [`commit`] or [`rollback`]), reconstructing "am I in
//! a trial?" from `nvbootctrl` each time. That two-phase driver is the on-device
//! follow-up; this module is the reusable controller it drives. See
//! `docs/ota/ROOTFS_AB_DESIGN.md`.
//!
//! [`set_try_boot`]: crate::BootController::set_try_boot
//! [`commit`]: crate::BootController::commit
//! [`rollback`]: crate::BootController::rollback

use std::io;

use crate::{BootController, Slot};

/// Runs an `nvbootctrl` invocation and returns its trimmed stdout on success. The one
/// hardware-coupled seam: the real impl shells to the binary, tests inject a mock.
pub trait NvbootctrlRunner {
    /// Run `nvbootctrl <args...>`; `Ok(stdout)` on exit 0, `Err` on a non-zero exit
    /// or a spawn failure. Fail-closed: the controller treats ANY error as "unknown
    /// slot state" and never proceeds on a guess.
    fn run(&self, args: &[&str]) -> io::Result<String>;
}

/// The real runner: shells out to the `nvbootctrl` binary. `target` selects the
/// `-t {bootloader,rootfs}` chain when a platform separates them (unset = the
/// unified default, which moves the rootfs with the slot).
pub struct SystemNvbootctrl {
    bin: String,
    target: Option<String>,
}

impl Default for SystemNvbootctrl {
    fn default() -> Self {
        Self {
            bin: "nvbootctrl".to_string(),
            target: None,
        }
    }
}

impl SystemNvbootctrl {
    /// Use a non-default binary path (e.g. an absolute `/usr/sbin/nvbootctrl`).
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self {
            bin: bin.into(),
            target: None,
        }
    }

    /// Select the `-t <target>` chain (`"rootfs"` / `"bootloader"`).
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

impl NvbootctrlRunner for SystemNvbootctrl {
    fn run(&self, args: &[&str]) -> io::Result<String> {
        let mut cmd = std::process::Command::new(&self.bin);
        if let Some(t) = &self.target {
            cmd.arg("-t").arg(t);
        }
        cmd.args(args);
        let out = cmd.output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "nvbootctrl {args:?} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// A rootfs-level [`BootController`] over a [`NvbootctrlRunner`]. Maps the trait's
/// four primitives onto `nvbootctrl` subcommands:
///
/// | trait method        | nvbootctrl command                    | effect |
/// |---------------------|---------------------------------------|--------|
/// | `active_slot`       | `get-current-slot`                    | the slot we booted |
/// | `set_try_boot(s)`   | `set-active-boot-slot <s>`            | arm next boot into `s`; the bootloader's retry counter auto-falls-back if the slot is never marked successful |
/// | `commit(_)`         | `mark-boot-successful`                | make the CURRENT (trial) slot permanent — health confirmed |
/// | `rollback(s)`       | `set-active-boot-slot <s>`            | point the next boot back at the previous good slot |
///
/// Slots map `A ⇒ 0`, `B ⇒ 1`. The bootloader's retry mechanism supplies the same
/// fail-safe as the app-level one-shot: a trial slot that fails to boot (or is never
/// marked successful) is abandoned automatically.
pub struct NvbootctrlBootController<R: NvbootctrlRunner> {
    runner: R,
}

impl<R: NvbootctrlRunner> NvbootctrlBootController<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Borrow the runner (e.g. to inspect recorded calls in a test).
    pub fn runner(&self) -> &R {
        &self.runner
    }

    /// `nvbootctrl` slot index for a [`Slot`]: `A ⇒ "0"`, `B ⇒ "1"`.
    fn index(slot: Slot) -> &'static str {
        match slot {
            Slot::A => "0",
            Slot::B => "1",
        }
    }

    /// Parse an `nvbootctrl` slot token into a [`Slot`]. Accepts the numeric `0`/`1`
    /// and the letter `A`/`B` (case-insensitive) forms different `nvbootctrl`
    /// versions print. FAIL-CLOSED: anything else is an error — never a default slot,
    /// because guessing the active slot could commit or roll back the wrong image.
    fn parse_slot(s: &str) -> io::Result<Slot> {
        match s.trim() {
            "0" => Ok(Slot::A),
            "1" => Ok(Slot::B),
            other => match other.to_ascii_lowercase().as_str() {
                "a" => Ok(Slot::A),
                "b" => Ok(Slot::B),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("nvbootctrl returned an unparseable slot: {other:?}"),
                )),
            },
        }
    }
}

impl<R: NvbootctrlRunner> BootController for NvbootctrlBootController<R> {
    fn active_slot(&self) -> io::Result<Slot> {
        Self::parse_slot(&self.runner.run(&["get-current-slot"])?)
    }

    fn set_try_boot(&mut self, slot: Slot) -> io::Result<()> {
        self.runner
            .run(&["set-active-boot-slot", Self::index(slot)])
            .map(|_| ())
    }

    fn commit(&mut self, _slot: Slot) -> io::Result<()> {
        // Marks the CURRENTLY-RUNNING slot boot-successful. The caller only reaches
        // here while running the trial slot, so `_slot` is that current slot; the
        // command needs no argument.
        self.runner.run(&["mark-boot-successful"]).map(|_| ())
    }

    fn rollback(&mut self, slot: Slot) -> io::Result<()> {
        // Point the next boot back at the previous good slot. (A reboot then returns
        // to it; the bootloader would also auto-fall-back on its own once the trial's
        // retry counter is exhausted — this makes the revert immediate/explicit.)
        self.runner
            .run(&["set-active-boot-slot", Self::index(slot)])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HealthOutcome, InstallState, Installer};
    use std::cell::RefCell;

    /// A scriptable `nvbootctrl` mock: returns a fixed `get-current-slot` and records
    /// every invocation's argv so a test can assert the exact command sequence.
    struct MockNv {
        current_slot: String,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl MockNv {
        fn new(current_slot: &str) -> Self {
            Self {
                current_slot: current_slot.to_string(),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }
    impl NvbootctrlRunner for MockNv {
        fn run(&self, args: &[&str]) -> io::Result<String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            match args.first().copied() {
                Some("get-current-slot") => Ok(self.current_slot.clone()),
                _ => Ok(String::new()),
            }
        }
    }

    #[test]
    fn active_slot_parses_numeric_and_letter_forms() {
        assert_eq!(
            NvbootctrlBootController::new(MockNv::new("0"))
                .active_slot()
                .unwrap(),
            Slot::A
        );
        assert_eq!(
            NvbootctrlBootController::new(MockNv::new("1"))
                .active_slot()
                .unwrap(),
            Slot::B
        );
        // Letter forms (case-insensitive) that some nvbootctrl versions print.
        assert_eq!(
            NvbootctrlBootController::new(MockNv::new("A"))
                .active_slot()
                .unwrap(),
            Slot::A
        );
        assert_eq!(
            NvbootctrlBootController::new(MockNv::new("b"))
                .active_slot()
                .unwrap(),
            Slot::B
        );
    }

    #[test]
    fn active_slot_fails_closed_on_garbage() {
        // A slot we can't parse must ERROR, never default — guessing could commit or
        // roll back the wrong image.
        let err = NvbootctrlBootController::new(MockNv::new("banana"))
            .active_slot()
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn primitives_map_to_the_right_commands() {
        let mut c = NvbootctrlBootController::new(MockNv::new("0"));
        c.set_try_boot(Slot::B).unwrap();
        c.commit(Slot::B).unwrap();
        c.rollback(Slot::A).unwrap();
        assert_eq!(
            c.runner().calls(),
            vec![
                vec!["set-active-boot-slot", "1"],
                vec!["mark-boot-successful"],
                vec!["set-active-boot-slot", "0"],
            ]
        );
    }

    #[test]
    fn installer_drives_a_full_commit_cycle_over_nvbootctrl() {
        // The device-agnostic Installer state machine drives the nvbootctrl
        // controller: begin_trial arms slot B, a healthy report marks it successful.
        // This is the command-sequence integration proof without a Jetson.
        let boot = NvbootctrlBootController::new(MockNv::new("0")); // running slot A
        let mut inst = Installer::new(boot, 3).unwrap();
        assert_eq!(inst.state(), InstallState::Idle { active: Slot::A });

        // stage() only verifies the artifact file — reuse a known-good tiny file.
        let p = std::env::temp_dir().join(format!("kirra_nv_{}.img", std::process::id()));
        std::fs::write(&p, b"hello").unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        inst.stage(&p, digest).unwrap();
        assert_eq!(inst.begin_trial().unwrap(), Slot::B);
        assert_eq!(
            inst.report_health(true).unwrap(),
            HealthOutcome::Committed { active: Slot::B }
        );
        assert_eq!(
            inst.boot_controller().runner().calls(),
            vec![
                vec!["get-current-slot"],           // Installer::new seeds active
                vec!["set-active-boot-slot", "1"],   // begin_trial arms B
                vec!["mark-boot-successful"],        // healthy → commit
            ]
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn installer_rolls_back_over_nvbootctrl_when_unhealthy() {
        let boot = NvbootctrlBootController::new(MockNv::new("0")); // running slot A
        let mut inst = Installer::new(boot, 1).unwrap(); // single attempt
        let p = std::env::temp_dir().join(format!("kirra_nv_rb_{}.img", std::process::id()));
        std::fs::write(&p, b"hello").unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        inst.stage(&p, digest).unwrap();
        inst.begin_trial().unwrap();
        assert_eq!(
            inst.report_health(false).unwrap(),
            HealthOutcome::RolledBack { active: Slot::A }
        );
        // Unhealthy → the next boot is pointed back at the good slot A.
        assert_eq!(
            inst.boot_controller().runner().calls(),
            vec![
                vec!["get-current-slot"],
                vec!["set-active-boot-slot", "1"], // begin_trial arms B
                vec!["set-active-boot-slot", "0"], // rollback to A
            ]
        );
        std::fs::remove_file(&p).ok();
    }
}
