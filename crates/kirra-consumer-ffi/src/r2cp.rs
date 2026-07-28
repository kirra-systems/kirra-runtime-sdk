//! C-ABI over the R2CP link — an **opaque handle**, deliberately.
//!
//! Same rule as the rest of this crate: the Python consumer never sees a key,
//! a signature or a watermark, and it must never see a COBS run, a CRC, a
//! nonce, a sequence watermark or an ACK result code either. Framing lives in
//! ONE implementation (`kirra-r2cp`, differentially tested against the
//! normative `wire.cpp`); a Python re-implementation would be a third dialect
//! that nothing tests against either of the other two.
//!
//! So this surface exposes *decisions*, not bytes. Python selects the mode,
//! passes SI-unit command fields, and reports errors.
//!
//! # The handshake is inside `open`
//!
//! [`kirra_r2cp_open`] probes the peer and returns a handle ONLY if one
//! answered acceptably. There is no way to hold a handle to an unidentified
//! device, so "command a peer we never identified" is not a mistake the caller
//! can make — it is unrepresentable rather than merely discouraged.
//!
//! On the bring-up robot that matters concretely: `/dev/myserial` is a stock
//! vendor motor board which does not speak R2CP, and writing R2CP frames into
//! a vendor command parser attached to motors is undefined behaviour.
//!
//! # Status classes exist so systemd can behave correctly
//!
//! [`kirra_r2cp_status_class`] separates DETERMINISTIC CONFIGURATION failures
//! from HARDWARE AVAILABILITY ones. This is not cosmetic: the consumer unit
//! carries `RestartPreventExitStatus=2`, so a fault reported as configuration
//! stops the unit permanently. An MCU that was merely unplugged at boot must
//! NOT land there, or reconnecting it would leave the unit dead until someone
//! runs `systemctl reset-failed`.

use std::ffi::{c_char, CStr};
use std::path::PathBuf;

use kirra_r2cp::handshake::HandshakeError;
use kirra_r2cp::link::{LinkError, LinkFault, SerialLink};
use kirra_r2cp::sim::{MotionCommand, MODE_TRACK};

/// Opaque to C. The identified peer lives INSIDE it, so a reopen starts
/// unidentified by construction.
pub struct KirraR2cpLink {
    link: SerialLink,
    /// Kept so a caller can be told which device failed without re-deriving it.
    device: PathBuf,
}

// ---------------------------------------------------------------- status codes

pub const KIRRA_R2CP_OK: i32 = 0;

// 10..19 — DETERMINISTIC CONFIGURATION. Will not fix itself; restarting only
// repeats it. These are the ones that deserve the consumer's exit 2.
pub const KIRRA_R2CP_BAD_PATH: i32 = 10;
pub const KIRRA_R2CP_DEVICE_NOT_FOUND: i32 = 11;
pub const KIRRA_R2CP_PERMISSION_DENIED: i32 = 12;
pub const KIRRA_R2CP_NOT_A_SERIAL_DEVICE: i32 = 13;

// 20..29 — HARDWARE AVAILABILITY. May resolve without a config change (someone
// plugs the MCU back in, the board finishes booting). Bounded restart is the
// right response, NOT a permanent stop.
pub const KIRRA_R2CP_NO_PEER: i32 = 20;
pub const KIRRA_R2CP_DISCONNECTED: i32 = 21;
pub const KIRRA_R2CP_FRAMING_LOST: i32 = 22;
/// The port is already held exclusively by another process. AVAILABILITY, not
/// configuration: the other holder may exit without anything about this
/// robot's config changing, and a permanent stop would then need a manual
/// `systemctl reset-failed` to undo.
pub const KIRRA_R2CP_PORT_BUSY: i32 = 23;

// 30..39 — PROTOCOL DISAGREEMENT. A peer answered and we cannot work with it.
// Deterministic for a given pair of builds, but a firmware update fixes it
// without touching this robot's configuration, so it is its own class.
pub const KIRRA_R2CP_VERSION_MISMATCH: i32 = 30;
pub const KIRRA_R2CP_FRAME_LIMIT: i32 = 31;
pub const KIRRA_R2CP_BAD_HANDSHAKE_REPLY: i32 = 32;
pub const KIRRA_R2CP_NOT_ACKNOWLEDGED: i32 = 33;
pub const KIRRA_R2CP_REFUSED: i32 = 34;

// 40+ — caller/runtime errors.
pub const KIRRA_R2CP_NULL_ARGUMENT: i32 = 40;
pub const KIRRA_R2CP_NOT_IDENTIFIED: i32 = 41;
pub const KIRRA_R2CP_IO: i32 = 42;

/// Status classes. See the module docs — the consumer maps these to exit
/// codes, and mapping availability to the configuration exit would make an
/// unplugged MCU unrecoverable.
pub const KIRRA_R2CP_CLASS_OK: i32 = 0;
pub const KIRRA_R2CP_CLASS_CONFIG: i32 = 1;
pub const KIRRA_R2CP_CLASS_AVAILABILITY: i32 = 2;
pub const KIRRA_R2CP_CLASS_PROTOCOL: i32 = 3;
pub const KIRRA_R2CP_CLASS_RUNTIME: i32 = 4;

/// Classify a status.
///
/// # Note on the one genuinely ambiguous case
///
/// [`KIRRA_R2CP_NO_PEER`] is classed as AVAILABILITY even though, on the
/// bring-up robot, "nothing answered" is *also* exactly what a mis-selected
/// mode against a vendor board looks like. The link cannot tell those apart —
/// both are silence — so the classification is a deliberate choice under
/// uncertainty:
///
/// * Calling a recoverable hardware fault permanent leaves a reconnected MCU
///   dead until someone runs `systemctl reset-failed`.
/// * Calling a wrong-mode misconfiguration recoverable costs a bounded burst
///   of restarts that fail loudly and identically, with the cause named in
///   every one, and `StartLimitBurst` stops the churn.
///
/// The second failure is visible and self-limiting; the first is silent and
/// needs an operator who knows the incantation.
#[no_mangle]
pub extern "C" fn kirra_r2cp_status_class(status: i32) -> i32 {
    match status {
        KIRRA_R2CP_OK => KIRRA_R2CP_CLASS_OK,
        10..=19 => KIRRA_R2CP_CLASS_CONFIG,
        20..=29 => KIRRA_R2CP_CLASS_AVAILABILITY,
        30..=39 => KIRRA_R2CP_CLASS_PROTOCOL,
        _ => KIRRA_R2CP_CLASS_RUNTIME,
    }
}

/// A static, operator-facing sentence for `status`. Never NULL.
#[no_mangle]
pub extern "C" fn kirra_r2cp_status_message(status: i32) -> *const c_char {
    let s: &CStr = match status {
        KIRRA_R2CP_OK => c"ok",
        KIRRA_R2CP_BAD_PATH => c"device path is empty or not valid UTF-8",
        KIRRA_R2CP_DEVICE_NOT_FOUND => c"device does not exist",
        KIRRA_R2CP_PERMISSION_DENIED => c"permission denied opening the device",
        KIRRA_R2CP_NOT_A_SERIAL_DEVICE => {
            c"device is not a serial device (raw mode could not be set)"
        }
        KIRRA_R2CP_NO_PEER => {
            c"no R2CP peer answered. Either the MCU is not connected/powered, or this \
              device is a VENDOR motor board, which does not speak R2CP — refusing to \
              send it frames"
        }
        KIRRA_R2CP_DISCONNECTED => c"the device went away (cable, power or MCU reset)",
        KIRRA_R2CP_PORT_BUSY => {
            c"the serial port is already held exclusively by another process — \
              refusing to share the path to the motors (ADR-0033 sole writer)"
        }
        KIRRA_R2CP_FRAMING_LOST => {
            c"sustained framing failure: the two ends disagree about frame boundaries"
        }
        KIRRA_R2CP_VERSION_MISMATCH => c"no overlapping protocol major version with the peer",
        KIRRA_R2CP_FRAME_LIMIT => c"the peer's advertised maximum frame size is unusable",
        KIRRA_R2CP_BAD_HANDSHAKE_REPLY => {
            c"the peer's handshake reply did not match the probe (stale, replayed or malformed)"
        }
        KIRRA_R2CP_NOT_ACKNOWLEDGED => c"the peer did not acknowledge a control frame",
        KIRRA_R2CP_REFUSED => c"the peer refused the command",
        KIRRA_R2CP_NULL_ARGUMENT => c"a required argument was NULL",
        KIRRA_R2CP_NOT_IDENTIFIED => {
            c"the peer is no longer identified; a fresh handshake is required"
        }
        _ => c"link I/O error",
    };
    s.as_ptr()
}

fn classify_open_error(e: &std::io::Error) -> i32 {
    match e.kind() {
        std::io::ErrorKind::NotFound => KIRRA_R2CP_DEVICE_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => KIRRA_R2CP_PERMISSION_DENIED,
        // EBUSY: someone else holds TIOCEXCL on this port. Availability, not
        // configuration — see KIRRA_R2CP_PORT_BUSY.
        std::io::ErrorKind::ResourceBusy => KIRRA_R2CP_PORT_BUSY,
        // ENOTTY from tcgetattr/tcsetattr: the path exists and opened, but it
        // is not a terminal — a regular file or a socket someone pointed the
        // config at. That is configuration, not availability.
        _ if e.raw_os_error() == Some(25) => KIRRA_R2CP_NOT_A_SERIAL_DEVICE,
        _ => KIRRA_R2CP_IO,
    }
}

fn classify_link_error(e: &LinkError) -> i32 {
    match e {
        LinkError::Handshake(HandshakeError::NoResponse) => KIRRA_R2CP_NO_PEER,
        LinkError::Handshake(HandshakeError::VersionMismatch { .. }) => KIRRA_R2CP_VERSION_MISMATCH,
        LinkError::Handshake(
            HandshakeError::FrameTooSmall { .. }
            | HandshakeError::FrameBelowProtocolMinimum { .. }
            | HandshakeError::FrameAboveProtocolMaximum { .. },
        ) => KIRRA_R2CP_FRAME_LIMIT,
        LinkError::Handshake(_) => KIRRA_R2CP_BAD_HANDSHAKE_REPLY,
        LinkError::NotHandshaken => KIRRA_R2CP_NOT_IDENTIFIED,
        LinkError::NotAcknowledged(_) => KIRRA_R2CP_NOT_ACKNOWLEDGED,
        LinkError::Io(io) => classify_open_error(io),
    }
}

/// Peer metadata for the caller's log. No wire details.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct KirraR2cpPeerInfo {
    pub implementation_id: u32,
    pub firmware_semver: u32,
    pub maximum_frame: u16,
    pub agreed_major: u8,
    /// 1 while the peer is identified; 0 after a disconnect, sustained framing
    /// failure, or an explicit reset. A caller seeing 0 must re-open.
    pub identified: u8,
    /// 1 when the KERNEL CONFIRMS this descriptor holds the tty exclusively
    /// (read back with TIOCGEXCL, not inferred from the claim returning 0).
    /// A caller may only tell an operator the port is exclusive when this is 1.
    pub exclusive: u8,
}

/// The outcome of one command. `result` is the peer's COMMAND_ACK result code,
/// bound normatively in `PROTOCOL.md` §COMMAND_ACK.
///
/// `accepted` is still the field to branch on: it already folds in `clamped`
/// (honoured, tightened against the calibrated envelope), which a caller must
/// not mistake for a refusal.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct KirraR2cpAck {
    /// 1 if the peer acknowledged with ACCEPTED. This is the field to branch on.
    pub accepted: u8,
    /// `safety_manager.hpp` discriminants.
    pub safety_state: u8,
    pub result: u16,
    /// 1 if no acknowledgement arrived within the timeout.
    pub timed_out: u8,
}

/// Open `device`, put it in raw mode, and complete the HELLO handshake.
///
/// On success `*out` receives an owned handle the caller must later pass to
/// [`kirra_r2cp_close`]. On failure `*out` is left NULL and the return value
/// names the reason — **there is no handle to an unidentified device.**
///
/// The handshake nonce is drawn from the OS entropy pool INSIDE this call. It
/// is deliberately not a parameter: the caller is a Python node that must not
/// hold nonce logic, and a predictable nonce silently converts the gate into
/// theatre with no visible symptom. One audited entropy source beats a
/// contract every caller has to honour.
///
/// # Safety
/// - `device` is a NUL-terminated C string.
/// - `out` points to a writable pointer slot.
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_open(
    device: *const c_char,
    timeout_ms: u16,
    out: *mut *mut KirraR2cpLink,
) -> i32 {
    if device.is_null() || out.is_null() {
        return KIRRA_R2CP_NULL_ARGUMENT;
    }
    // SAFETY: caller guarantees a writable slot.
    unsafe { *out = std::ptr::null_mut() };

    // SAFETY: caller guarantees a NUL-terminated string.
    let path = match unsafe { CStr::from_ptr(device) }.to_str() {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => return KIRRA_R2CP_BAD_PATH,
    };
    let mut link = match SerialLink::open(&path, None) {
        Ok(l) => l,
        Err(e) => return classify_open_error(&e),
    };
    if let Err(e) = link.handshake_fresh(timeout_ms) {
        return classify_link_error(&e);
    }
    let boxed = Box::new(KirraR2cpLink { link, device: path });
    // SAFETY: caller guarantees a writable slot.
    unsafe { *out = Box::into_raw(boxed) };
    KIRRA_R2CP_OK
}

/// ARM then ACTIVATE, each requiring an acknowledgement.
///
/// # Safety
/// `h` is a live handle from [`kirra_r2cp_open`].
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_activate(h: *mut KirraR2cpLink, timeout_ms: u16) -> i32 {
    let Some(link) = (unsafe { h.as_mut() }) else {
        return KIRRA_R2CP_NULL_ARGUMENT;
    };
    match link.link.arm_and_activate(timeout_ms) {
        Ok(()) => KIRRA_R2CP_OK,
        Err(e) => classify_link_error(&e),
    }
}

/// Peer metadata, including whether it is still identified.
///
/// # Safety
/// `h` is a live handle; `out` points to a writable [`KirraR2cpPeerInfo`].
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_peer_info(
    h: *mut KirraR2cpLink,
    out: *mut KirraR2cpPeerInfo,
) -> i32 {
    let (Some(link), false) = (unsafe { h.as_mut() }, out.is_null()) else {
        return KIRRA_R2CP_NULL_ARGUMENT;
    };
    let info = match link.link.peer() {
        Some(p) => KirraR2cpPeerInfo {
            implementation_id: p.implementation_id,
            firmware_semver: p.firmware_semver,
            maximum_frame: p.maximum_frame,
            agreed_major: p.agreed_major,
            identified: 1,
            exclusive: u8::from(link.link.is_exclusive()),
        },
        None => KirraR2cpPeerInfo {
            // Exclusivity is a property of the DESCRIPTOR, not of
            // identification: a de-identified link still holds the port, and
            // reporting otherwise would understate what is held.
            exclusive: u8::from(link.link.is_exclusive()),
            ..KirraR2cpPeerInfo::default()
        },
    };
    // SAFETY: caller guarantees a writable struct.
    unsafe { *out = info };
    if info.identified == 1 {
        KIRRA_R2CP_OK
    } else {
        match link.link.fault() {
            Some(LinkFault::Disconnected) => KIRRA_R2CP_DISCONNECTED,
            Some(LinkFault::FramingLost { .. }) => KIRRA_R2CP_FRAMING_LOST,
            _ => KIRRA_R2CP_NOT_IDENTIFIED,
        }
    }
}

/// Send one motion command and wait for its acknowledgement.
///
/// The values are the ones the governor already released — this transports
/// them, it does not decide them. Nothing here clamps, and nothing here is a
/// safety authority.
///
/// # Safety
/// `h` is a live handle; `out` points to a writable [`KirraR2cpAck`].
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_command(
    h: *mut KirraR2cpLink,
    velocity_mps: f32,
    curvature_per_m: f32,
    acceleration_limit_mps2: f32,
    jerk_limit_mps3: f32,
    valid_for_us: u32,
    command_id: u32,
    source_time_us: u64,
    timeout_ms: u16,
    out: *mut KirraR2cpAck,
) -> i32 {
    let (Some(link), false) = (unsafe { h.as_mut() }, out.is_null()) else {
        return KIRRA_R2CP_NULL_ARGUMENT;
    };
    // SAFETY: caller guarantees a writable struct.
    unsafe { *out = KirraR2cpAck::default() };

    let cmd = MotionCommand {
        command_id,
        valid_for_us,
        velocity_mps,
        curvature_per_m,
        acceleration_limit_mps2,
        jerk_limit_mps3,
        mode: MODE_TRACK,
    };
    let seq = match link.link.send_motion(&cmd, source_time_us) {
        Ok(s) => s,
        Err(e) => return classify_link_error(&e),
    };
    let received = match link.link.receive(timeout_ms) {
        Ok(r) => r,
        Err(e) => return classify_open_error(&e),
    };
    let Some(ack) = received
        .frames
        .iter()
        .find(|f| kirra_r2cp::link::ack_received_sequence(f) == Some(seq))
    else {
        // SAFETY: caller guarantees a writable struct.
        unsafe { (*out).timed_out = 1 };
        return KIRRA_R2CP_NOT_ACKNOWLEDGED;
    };
    let result = kirra_r2cp::link::ack_result(ack).unwrap_or(u16::MAX);
    let accepted = u8::from(result == kirra_r2cp::command_ack::AckResult::Accepted.as_u16());
    // SAFETY: caller guarantees a writable struct.
    unsafe {
        *out = KirraR2cpAck {
            accepted,
            safety_state: kirra_r2cp::link::ack_safety_state(ack).unwrap_or(0),
            result,
            timed_out: 0,
        }
    };
    if accepted == 1 {
        KIRRA_R2CP_OK
    } else {
        KIRRA_R2CP_REFUSED
    }
}

/// Send a stop. Works even after the peer has been de-identified.
///
/// 🔴 **`KIRRA_R2CP_OK` here means the bytes were written — NOT that anything
/// stopped.** Nothing is read back, and if the peer was de-identified this may
/// be writing to a device that is not an R2CP peer at all. The thing that
/// actually stops a governed robot is the peer's own watchdog: commands cease,
/// the window lapses, the MCU enters ControlledStop by itself. This makes that
/// immediate when the peer is real; it is not a substitute for it, and a caller
/// must log it as "stop sent", never "stopped".
///
/// # Safety
/// `h` is a live handle from [`kirra_r2cp_open`].
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_stop(
    h: *mut KirraR2cpLink,
    valid_for_us: u32,
    source_time_us: u64,
) -> i32 {
    let Some(link) = (unsafe { h.as_mut() }) else {
        return KIRRA_R2CP_NULL_ARGUMENT;
    };
    match link.link.send_stop(valid_for_us, source_time_us) {
        Ok(_) => KIRRA_R2CP_OK,
        Err(e) => classify_open_error(&e),
    }
}

/// Drop the identified peer, requiring a fresh open before further commands.
/// The hook for a watchdog-triggered stop.
///
/// # Safety
/// `h` is a live handle from [`kirra_r2cp_open`].
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_reset(h: *mut KirraR2cpLink) -> i32 {
    let Some(link) = (unsafe { h.as_mut() }) else {
        return KIRRA_R2CP_NULL_ARGUMENT;
    };
    link.link.reset();
    KIRRA_R2CP_OK
}

/// The device path this handle was opened on. Valid until [`kirra_r2cp_close`].
///
/// # Safety
/// `h` is a live handle; the returned pointer must not outlive it.
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_device_len(h: *mut KirraR2cpLink) -> usize {
    match unsafe { h.as_ref() } {
        Some(l) => l.device.as_os_str().len(),
        None => 0,
    }
}

/// Free a handle. Safe with NULL (no-op).
///
/// # Safety
/// `h` must come from [`kirra_r2cp_open`] and not already be freed.
#[no_mangle]
pub unsafe extern "C" fn kirra_r2cp_close(h: *mut KirraR2cpLink) {
    if !h.is_null() {
        // SAFETY: caller guarantees provenance and single free.
        drop(unsafe { Box::from_raw(h) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_and_configuration_are_different_classes() {
        // The distinction the consumer's exit code rests on. If these ever
        // collapse, an MCU that was merely unplugged at boot becomes
        // permanently un-restartable under RestartPreventExitStatus=2.
        assert_eq!(
            kirra_r2cp_status_class(KIRRA_R2CP_NO_PEER),
            KIRRA_R2CP_CLASS_AVAILABILITY
        );
        assert_eq!(
            kirra_r2cp_status_class(KIRRA_R2CP_DISCONNECTED),
            KIRRA_R2CP_CLASS_AVAILABILITY
        );
        // Another process holding the port may release it without any config
        // change, so a permanent stop would be wrong here too.
        assert_eq!(
            kirra_r2cp_status_class(KIRRA_R2CP_PORT_BUSY),
            KIRRA_R2CP_CLASS_AVAILABILITY
        );
        assert_eq!(
            kirra_r2cp_status_class(KIRRA_R2CP_DEVICE_NOT_FOUND),
            KIRRA_R2CP_CLASS_CONFIG
        );
        assert_eq!(
            kirra_r2cp_status_class(KIRRA_R2CP_PERMISSION_DENIED),
            KIRRA_R2CP_CLASS_CONFIG
        );
        assert_ne!(
            kirra_r2cp_status_class(KIRRA_R2CP_NO_PEER),
            kirra_r2cp_status_class(KIRRA_R2CP_DEVICE_NOT_FOUND),
            "a silent peer must not be classed with a missing device"
        );
    }

    #[test]
    fn every_status_has_a_message_and_a_class() {
        for status in [
            KIRRA_R2CP_OK,
            KIRRA_R2CP_BAD_PATH,
            KIRRA_R2CP_DEVICE_NOT_FOUND,
            KIRRA_R2CP_PERMISSION_DENIED,
            KIRRA_R2CP_NOT_A_SERIAL_DEVICE,
            KIRRA_R2CP_NO_PEER,
            KIRRA_R2CP_DISCONNECTED,
            KIRRA_R2CP_PORT_BUSY,
            KIRRA_R2CP_FRAMING_LOST,
            KIRRA_R2CP_VERSION_MISMATCH,
            KIRRA_R2CP_FRAME_LIMIT,
            KIRRA_R2CP_BAD_HANDSHAKE_REPLY,
            KIRRA_R2CP_NOT_ACKNOWLEDGED,
            KIRRA_R2CP_REFUSED,
            KIRRA_R2CP_NULL_ARGUMENT,
            KIRRA_R2CP_NOT_IDENTIFIED,
            KIRRA_R2CP_IO,
        ] {
            let msg = unsafe { CStr::from_ptr(kirra_r2cp_status_message(status)) };
            assert!(
                !msg.to_bytes().is_empty(),
                "status {status} has no operator message"
            );
            let class = kirra_r2cp_status_class(status);
            assert!((0..=4).contains(&class), "status {status} class {class}");
        }
    }

    #[test]
    fn the_no_peer_message_names_the_vendor_board() {
        // "no response" alone sends an operator hunting a cable fault; on this
        // robot the likeliest cause is a mode pointed at the vendor board.
        let msg = unsafe { CStr::from_ptr(kirra_r2cp_status_message(KIRRA_R2CP_NO_PEER)) }
            .to_str()
            .unwrap();
        assert!(msg.contains("VENDOR"), "got: {msg}");
        assert!(msg.contains("not connected"), "got: {msg}");
    }

    #[test]
    fn null_arguments_are_refused_not_dereferenced() {
        let mut out: *mut KirraR2cpLink = std::ptr::null_mut();
        assert_eq!(
            unsafe { kirra_r2cp_open(std::ptr::null(), 10, &mut out) },
            KIRRA_R2CP_NULL_ARGUMENT
        );
        assert_eq!(
            unsafe { kirra_r2cp_activate(std::ptr::null_mut(), 10) },
            KIRRA_R2CP_NULL_ARGUMENT
        );
        assert_eq!(
            unsafe { kirra_r2cp_stop(std::ptr::null_mut(), 100_000, 0) },
            KIRRA_R2CP_NULL_ARGUMENT
        );
        // close(NULL) is a documented no-op, not a crash.
        unsafe { kirra_r2cp_close(std::ptr::null_mut()) };
    }

    #[test]
    fn a_missing_device_is_configuration_not_availability() {
        let mut out: *mut KirraR2cpLink = std::ptr::null_mut();
        let path = c"/dev/definitely-not-a-real-device-kirra";
        let status = unsafe { kirra_r2cp_open(path.as_ptr(), 10, &mut out) };
        assert_eq!(status, KIRRA_R2CP_DEVICE_NOT_FOUND);
        assert_eq!(kirra_r2cp_status_class(status), KIRRA_R2CP_CLASS_CONFIG);
        assert!(out.is_null(), "no handle on failure");
    }
}
