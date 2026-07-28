//! ADR-0033 Tier-3 sole-writer, for the R2CP path.
//!
//! The boot sentinel proves nobody held the port *before* the consumer
//! started. `TIOCEXCL` is what stops anyone taking it *after* — and on this
//! path the descriptor lives inside `SerialLink`, so the claim has to happen
//! there or not at all.
//!
//! Ordering is load-bearing: open, configure, CLAIM, then handshake. A second
//! writer appearing between the open and the claim would be invisible, and the
//! entire reason this link exists is that exactly one process may reach the
//! motors.
//!
//! ## What these tests can and cannot observe
//!
//! `tty_ioctl(4)`: `TIOCEXCL` makes further `open(2)` calls fail `EBUSY`
//! "except for a process with the `CAP_SYS_ADMIN` capability". CI runs as
//! root, so a second open here SUCCEEDS even though the claim is held — I
//! confirmed that empirically before writing these, rather than assuming the
//! refusal.
//!
//! So the assertions split:
//!
//! * The claim's STATE is checked with `TIOCGEXCL`, which is readable at any
//!   privilege and is what the link reports through `is_exclusive()`. This
//!   runs everywhere and is the primary evidence.
//! * The EBUSY REFUSAL is checked only when the suite runs unprivileged, and
//!   says so loudly when it skips. On the robot the consumer runs as a
//!   rendered non-root `User=`, which is where the refusal actually bites.
//!
//! A test that asserted the refusal unconditionally would have failed here and
//! been "fixed" by deleting it. One that quietly skipped would have implied
//! coverage it does not have.

#![cfg(feature = "pty")]

use std::os::fd::AsFd;
use std::path::PathBuf;

use kirra_r2cp::link::SerialLink;
use kirra_r2cp::pty::SimulatedMcuPty;
use kirra_r2cp::sim::SimulatedMcu;

const NONCE: [u8; 16] = [0x11; 16];

struct SimHandle {
    device: PathBuf,
    stop: std::sync::mpsc::Sender<()>,
}

fn spawn_sim() -> SimHandle {
    let pty = SimulatedMcuPty::open().expect("pty");
    let device = pty.device_path().to_path_buf();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut pty = pty;
        let mut mcu = SimulatedMcu::new();
        let started = std::time::Instant::now();
        loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            let now_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            if pty.pump(&mut mcu, now_us, 20).is_err() {
                return;
            }
        }
    });
    SimHandle {
        device,
        stop: stop_tx,
    }
}

/// A plain second opener, exactly as another process would do it.
fn second_open(device: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
}

/// Is this process exempt from the EBUSY that TIOCEXCL imposes?
fn privileged() -> bool {
    // `nix::unistd::geteuid` is behind the `user` feature this crate does not
    // enable; libc is already in the dev graph via nix.
    // SAFETY: geteuid() takes no arguments, cannot fail, and touches no memory.
    (unsafe { nix::libc::geteuid() }) == 0
}

/// Assert a second opener is refused — or say clearly why it was not checked.
fn assert_second_open_refused(device: &std::path::Path, context: &str) {
    if privileged() {
        eprintln!(
            "SKIP [{context}]: running as root, which tty_ioctl(4) exempts from \
             TIOCEXCL's EBUSY. The claim STATE is asserted instead; the refusal \
             bites on the robot, where the consumer runs unprivileged."
        );
        return;
    }
    let err = second_open(device).expect_err("a second opener must be refused");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::ResourceBusy,
        "{context}: expected EBUSY, got {err:?}"
    );
}

#[test]
fn case_1_a_live_link_opens_and_holds_exclusivity() {
    let sim = spawn_sim();
    let mut link = SerialLink::open(&sim.device, None).expect("open");
    link.handshake(NONCE, 1000).expect("handshake");
    assert!(link.peer().is_some());

    // The primary evidence, readable at any privilege.
    assert!(
        link.is_exclusive(),
        "the kernel must confirm exclusive mode after a successful open"
    );
    assert_second_open_refused(&sim.device, "live link");
    let _ = sim.stop.send(());
}

#[test]
fn case_2_a_second_opener_is_refused_while_the_handle_exists() {
    let sim = spawn_sim();
    let link = SerialLink::open(&sim.device, None).expect("open");

    // Repeatedly, not once: the claim is a property of the descriptor's
    // lifetime, not a one-shot that a retry could slip past.
    for attempt in 0..5 {
        assert!(link.is_exclusive(), "attempt {attempt}: claim must persist");
        assert_second_open_refused(&sim.device, &format!("attempt {attempt}"));
    }
    drop(link);
    let _ = sim.stop.send(());
}

#[test]
fn case_3_closing_the_handle_releases_the_port() {
    // Non-vacuity for cases 1 and 2: if the port were unopenable for some
    // other reason, those refusals would prove nothing.
    let sim = spawn_sim();
    let link = SerialLink::open(&sim.device, None).expect("open");
    assert!(link.is_exclusive(), "held while the link lives");

    drop(link);
    // Exclusive mode is a property of the DESCRIPTOR, so closing it clears the
    // tty's flag — observable at any privilege, and the reopen must also work.
    let reopened = second_open(&sim.device).expect("closing the link releases the port");
    assert!(
        !crate_read_exclusive(&reopened),
        "the tty must no longer be in exclusive mode once the link is dropped"
    );
    let _ = sim.stop.send(());
}

#[test]
fn case_4_a_failed_handshake_leaves_no_lock_and_no_descriptor() {
    // A silent device: the open and the claim both succeed, then the
    // handshake refuses. The descriptor must not leak — otherwise a robot
    // that failed to identify its MCU would hold the port against its own
    // next restart, turning a transient fault into a permanent one.
    let pair = nix::pty::openpty(None, None).expect("openpty");
    let device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
    let _keep_open = (pair.master, pair.slave);

    {
        let mut link = SerialLink::open(&device, None).expect("open");
        assert!(
            link.is_exclusive(),
            "the claim should be held during the attempt"
        );
        link.handshake(NONCE, 150)
            .expect_err("a silent device must not identify");
        // `link` is dropped here.
    }

    let after = second_open(&device).expect("the port must be reopenable");
    assert!(
        !crate_read_exclusive(&after),
        "a failed handshake must not leave the tty in exclusive mode"
    );
}

#[test]
fn case_5_a_reopen_claims_exclusivity_again() {
    // Exclusivity is a property of the descriptor, so a fresh link must make
    // a fresh claim — not inherit one, and not skip it because a previous
    // link in this process already had it.
    let sim = spawn_sim();

    for round in 0..3 {
        let mut link = SerialLink::open(&sim.device, None).expect("open");
        link.handshake([round as u8; 16], 1000)
            .unwrap_or_else(|e| panic!("round {round} handshake: {e}"));
        assert!(
            link.is_exclusive(),
            "round {round}: the reopened link must hold the port"
        );
        assert_second_open_refused(&sim.device, &format!("round {round}"));
        drop(link);
        let free = second_open(&sim.device).expect("port free after drop");
        assert!(
            !crate_read_exclusive(&free),
            "round {round}: the port must be free after the drop"
        );
    }
    let _ = sim.stop.send(());
}

#[test]
fn the_claim_happens_before_any_handshake_byte_goes_out() {
    // The ordering requirement, made observable: a peer that never receives a
    // probe cannot have been written to, and the port is already exclusive
    // before `handshake` is even called.
    let sim = spawn_sim();
    let link = SerialLink::open(&sim.device, None).expect("open");
    assert!(link.peer().is_none(), "no handshake has run yet");
    assert!(
        link.is_exclusive(),
        "exclusivity must already be held before the first probe"
    );
    drop(link);
    let _ = sim.stop.send(());
}

/// Read `TIOCGEXCL` on an arbitrary descriptor, so a test can check the tty's
/// state through a handle the link does not own.
fn crate_read_exclusive(f: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    let mut set: i32 = 0;
    // SAFETY: `f` is an open descriptor borrowed for this call; TIOCGEXCL
    // writes exactly one `int` through the supplied pointer.
    let rc = unsafe {
        nix::libc::ioctl(
            f.as_raw_fd(),
            nix::libc::TIOCGEXCL,
            std::ptr::addr_of_mut!(set),
        )
    };
    assert_eq!(
        rc,
        0,
        "TIOCGEXCL failed: {:?}",
        std::io::Error::last_os_error()
    );
    set != 0
}

#[test]
fn the_exclusivity_probe_itself_is_not_vacuous() {
    // If `crate_read_exclusive` always returned false, half the assertions
    // above would pass for the wrong reason. A plain open is NOT exclusive; a
    // link's open IS.
    let sim = spawn_sim();
    let plain = second_open(&sim.device).expect("plain open");
    assert!(
        !crate_read_exclusive(&plain),
        "a plain open is not exclusive"
    );
    drop(plain);

    let link = SerialLink::open(&sim.device, None).expect("open");
    assert!(link.is_exclusive(), "the link's open IS exclusive");
    drop(link);
    let _ = sim.stop.send(());
}

#[test]
fn dropping_a_link_whose_peer_vanished_does_not_panic() {
    // Review point: the release ioctl must not panic or block cleanup. Drop
    // runs on paths where there is nobody left to report to, so it ignores the
    // ioctl result — this proves it stays quiet when the far end is gone,
    // which is the shape of a cable pull or an MCU reset during shutdown.
    let sim = spawn_sim();
    let mut link = SerialLink::open(&sim.device, None).expect("open");
    link.handshake(NONCE, 1000).expect("handshake");

    // The peer disappears, then we tear down.
    let _ = sim.stop.send(());
    std::thread::sleep(std::time::Duration::from_millis(80));
    let _ = link.receive(50); // notice the disconnect
    drop(link); // must not panic
}

#[test]
fn drop_covers_the_link_from_the_moment_it_exists() {
    // Review point: exclusivity must not leak on a partially-initialised open.
    //
    // `SerialLink::open` constructs Self BEFORE claiming, so every fallible
    // step is inside Drop's coverage. This pins the ordering behaviourally:
    // after a successful open the tty is exclusive, and after the drop it is
    // not — with the tty kept alive throughout by a separate descriptor, which
    // is precisely the case where relying on "last close" would fail.
    let pair = nix::pty::openpty(None, None).expect("openpty");
    let device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
    let keepalive = second_open(&device).expect("hold the tty open independently");

    {
        let link = SerialLink::open(&device, None).expect("open");
        assert!(link.is_exclusive());
        assert!(
            crate_read_exclusive(&keepalive),
            "the flag is on the TTY, so an independent fd sees it too"
        );
    }

    assert!(
        !crate_read_exclusive(&keepalive),
        "Drop must clear exclusive mode even though another descriptor is \
         still holding the tty open — this is the case where waiting for the \
         last close would leave the port permanently locked"
    );
    drop(keepalive);
    drop(pair);
}
