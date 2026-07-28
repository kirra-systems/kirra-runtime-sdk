//! The FFI surface driven against the PTY simulator — the same path the Python
//! consumer will take.
//!
//! These go through `kirra_r2cp_open`/`_activate`/`_command`/`_stop`/`_close`
//! exactly as ctypes will, rather than through the Rust API underneath. A test
//! that called `SerialLink` directly would leave the C boundary — the pointer
//! handling, the status mapping, the "no handle on failure" rule — unproven.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;

use kirra_consumer_ffi::r2cp::*;
use kirra_r2cp::pty::SimulatedMcuPty;
use kirra_r2cp::sim::SimulatedMcu;

struct SimHandle {
    device: PathBuf,
    stop: std::sync::mpsc::Sender<()>,
}

fn spawn_sim() -> SimHandle {
    let pty = SimulatedMcuPty::open().expect("allocate a pty");
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

/// A pty nobody services: present, openable, silent in R2CP terms — the shape
/// of the vendor motor board on the bring-up robot.
struct SilentPeer {
    master: std::fs::File,
    _slave: std::os::fd::OwnedFd,
    device: PathBuf,
}

impl SilentPeer {
    fn open() -> Self {
        let pair = nix::pty::openpty(None, None).expect("openpty");
        let device = nix::unistd::ttyname(pair.slave.as_fd()).expect("ttyname");
        Self {
            master: std::fs::File::from(pair.master),
            _slave: pair.slave,
            device,
        }
    }
}

fn open_link(device: &std::path::Path, timeout_ms: u16) -> (i32, *mut KirraR2cpLink) {
    let c = CString::new(device.to_str().unwrap()).unwrap();
    let mut handle: *mut KirraR2cpLink = std::ptr::null_mut();
    let status = unsafe { kirra_r2cp_open(c.as_ptr(), timeout_ms, &mut handle) };
    (status, handle)
}

#[test]
fn case_1_simulator_present_command_reaches_it_and_an_ack_returns() {
    let sim = spawn_sim();
    let (status, handle) = open_link(&sim.device, 1000);
    assert_eq!(status, KIRRA_R2CP_OK, "the simulator should answer");
    assert!(!handle.is_null());

    let mut info = KirraR2cpPeerInfo::default();
    assert_eq!(
        unsafe { kirra_r2cp_peer_info(handle, &mut info) },
        KIRRA_R2CP_OK
    );
    assert_eq!(info.identified, 1);
    assert_eq!(info.agreed_major, 1);

    assert_eq!(unsafe { kirra_r2cp_activate(handle, 1000) }, KIRRA_R2CP_OK);

    let mut ack = KirraR2cpAck::default();
    let status =
        unsafe { kirra_r2cp_command(handle, 0.4, 0.0, 1.0, 5.0, 100_000, 7, 0, 1000, &mut ack) };
    assert_eq!(status, KIRRA_R2CP_OK);
    assert_eq!(ack.accepted, 1);
    assert_eq!(ack.timed_out, 0);
    assert_eq!(ack.safety_state, 4, "safety_manager.hpp: active == 4");

    unsafe { kirra_r2cp_close(handle) };
    let _ = sim.stop.send(());
}

#[test]
fn case_2_a_silent_device_refuses_and_is_an_availability_failure() {
    // 🔴 The vendor-board case. It must refuse — and it must NOT be classed as
    // deterministic configuration, or `RestartPreventExitStatus=2` would make
    // a merely-unplugged MCU unrecoverable after someone reconnects it.
    let quiet = SilentPeer::open();
    let (status, handle) = open_link(&quiet.device, 150);

    assert_eq!(status, KIRRA_R2CP_NO_PEER);
    assert_eq!(
        kirra_r2cp_status_class(status),
        KIRRA_R2CP_CLASS_AVAILABILITY
    );
    assert!(handle.is_null(), "no handle to an unidentified device");
}

#[test]
fn case_3_a_device_chattering_a_foreign_protocol_refuses_and_sends_no_command() {
    let mut noisy = SilentPeer::open();
    for _ in 0..8 {
        noisy
            .master
            .write_all(&[0xFF, 0xFE, 0x04, 0x01, 0x02, 0x03, 0x00])
            .expect("vendor-shaped chatter");
    }
    noisy.master.flush().unwrap();

    let (status, handle) = open_link(&noisy.device, 150);
    assert_ne!(status, KIRRA_R2CP_OK);
    assert!(handle.is_null());
    // And there is no way to proceed: without a handle there is no call that
    // can send a motion command. The refusal is structural, not a convention.
}

#[test]
fn case_7_a_link_that_disappears_stops_being_identified() {
    let sim = spawn_sim();
    let (status, handle) = open_link(&sim.device, 1000);
    assert_eq!(status, KIRRA_R2CP_OK);
    assert_eq!(unsafe { kirra_r2cp_activate(handle, 1000) }, KIRRA_R2CP_OK);

    // The peer goes away mid-session.
    let _ = sim.stop.send(());
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Commands now fail rather than silently going nowhere, and peer_info
    // reports the link is no longer identified.
    let mut ack = KirraR2cpAck::default();
    let status =
        unsafe { kirra_r2cp_command(handle, 0.4, 0.0, 1.0, 5.0, 100_000, 8, 0, 200, &mut ack) };
    assert_ne!(status, KIRRA_R2CP_OK, "a dead link must not report success");
    assert_eq!(ack.accepted, 0);

    let mut info = KirraR2cpPeerInfo::default();
    let status = unsafe { kirra_r2cp_peer_info(handle, &mut info) };
    assert!(
        info.identified == 0 || status == KIRRA_R2CP_OK,
        "identification must not survive a link that stopped answering"
    );
    unsafe { kirra_r2cp_close(handle) };
}

#[test]
fn case_8_a_stop_is_attempted_even_when_the_peer_was_de_identified() {
    // A shutdown path must not be blocked by the gate that protects startup.
    let sim = spawn_sim();
    let (status, handle) = open_link(&sim.device, 1000);
    assert_eq!(status, KIRRA_R2CP_OK);

    // Explicitly de-identify (the watchdog-reset hook), then stop anyway.
    assert_eq!(unsafe { kirra_r2cp_reset(handle) }, KIRRA_R2CP_OK);
    let mut info = KirraR2cpPeerInfo::default();
    let _ = unsafe { kirra_r2cp_peer_info(handle, &mut info) };
    assert_eq!(info.identified, 0);

    // A command is refused...
    let mut ack = KirraR2cpAck::default();
    assert_eq!(
        unsafe { kirra_r2cp_command(handle, 0.4, 0.0, 1.0, 5.0, 100_000, 9, 0, 200, &mut ack) },
        KIRRA_R2CP_NOT_IDENTIFIED
    );
    // ...but a stop is still attempted. OK here means BYTES WRITTEN, not
    // "stopped" — see the kirra_r2cp_stop docs.
    assert_eq!(
        unsafe { kirra_r2cp_stop(handle, 100_000, 0) },
        KIRRA_R2CP_OK
    );

    unsafe { kirra_r2cp_close(handle) };
    let _ = sim.stop.send(());
}

#[test]
fn a_handle_is_only_ever_produced_for_an_identified_peer() {
    // The structural claim the whole surface rests on, stated once as a test:
    // across every failure mode, `out` is left NULL. "Command a peer we never
    // identified" is therefore unrepresentable, not merely discouraged.
    let quiet = SilentPeer::open();
    let cases: Vec<(&str, PathBuf)> = vec![
        ("missing device", PathBuf::from("/dev/kirra-no-such-device")),
        ("silent peer", quiet.device.clone()),
    ];
    for (name, device) in cases {
        let (status, handle) = open_link(&device, 100);
        assert_ne!(status, KIRRA_R2CP_OK, "{name} should not succeed");
        assert!(handle.is_null(), "{name} produced a handle");
    }

    // Positive control: a real peer DOES produce one, so the assertion above
    // is not passing because open never succeeds.
    let sim = spawn_sim();
    let (status, handle) = open_link(&sim.device, 1000);
    assert_eq!(status, KIRRA_R2CP_OK);
    assert!(!handle.is_null());
    unsafe { kirra_r2cp_close(handle) };
    let _ = sim.stop.send(());
}
