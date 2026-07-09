// systemd_notify_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// #46 — systemd sd_notify / watchdog wiring.
// ---------------------------------------------------------------------------

use super::{sd_notify_to, watchdog_should_ping};
use std::os::unix::net::UnixDatagram;

#[test]
fn watchdog_pings_active_only_when_cache_fresh() {
    // Active liveness is gated on a fresh posture cache (engine-liveness).
    assert!(
        watchdog_should_ping(true, true),
        "Active + fresh cache → ping"
    );
    assert!(
        !watchdog_should_ping(true, false),
        "Active + STALE cache → withhold (fail-closed)"
    );
    // PassiveStandby has no posture engine by design → plain keepalive.
    assert!(
        watchdog_should_ping(false, false),
        "PassiveStandby → keepalive ping"
    );
    assert!(
        watchdog_should_ping(false, true),
        "PassiveStandby → keepalive ping"
    );
}

#[test]
fn sd_notify_to_delivers_the_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notify.sock");
    let listener = UnixDatagram::bind(&path).expect("bind notify socket");
    sd_notify_to(path.as_os_str(), "READY=1").expect("sd_notify_to send");
    let mut buf = [0u8; 64];
    let n = listener.recv(&mut buf).expect("recv");
    assert_eq!(
        &buf[..n],
        b"READY=1",
        "the exact sd_notify datagram must be delivered"
    );
}
