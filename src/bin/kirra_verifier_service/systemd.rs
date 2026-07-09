// systemd — sd_notify plumbing + the Type=notify watchdog loop.
// Extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).

use super::*;

// ---------------------------------------------------------------------------

/// Send one sd_notify message (e.g. `"READY=1"`, `"WATCHDOG=1"`) to the socket
/// at `socket` (a filesystem path, or an abstract socket when it begins with
/// `@`). Separated from [`sd_notify`] so it is testable without mutating
/// `NOTIFY_SOCKET` in-process (INVARIANT #13).
pub(crate) fn sd_notify_to(socket: &std::ffi::OsStr, message: &str) -> std::io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    let sock = UnixDatagram::unbound()?;
    let path_str = socket.to_string_lossy();
    if let Some(name) = path_str.strip_prefix('@') {
        // Linux abstract namespace socket (leading NUL).
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;
        let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
        sock.connect_addr(&addr)?;
    } else {
        sock.connect(std::path::Path::new(socket))?;
    }
    sock.send(message.as_bytes())?;
    Ok(())
}

/// Best-effort sd_notify. No-op when `NOTIFY_SOCKET` is unset (not run under
/// `Type=notify`); a send failure is logged, never fatal.
pub(crate) fn sd_notify(message: &str) {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(e) = sd_notify_to(&socket, message) {
        tracing::warn!(error = %e, message, "sd_notify: send failed");
    }
}

/// Whether the watchdog should ping this tick. On the Active node the ping is
/// GATED on posture-engine liveness (a fresh cache — the refresh loop restamps
/// it each interval; a hung worker lets it go stale → ping withheld → systemd
/// restarts, fail-closed). PassiveStandby has no posture engine by design, so
/// it pings as a plain keepalive (its liveness is the promotion monitor).
pub(crate) fn watchdog_should_ping(is_active: bool, cache_fresh: bool) -> bool {
    if is_active {
        cache_fresh
    } else {
        true
    }
}

/// Spawn the systemd watchdog keepalive (#46). No-op unless `WATCHDOG_USEC` is
/// set (i.e. the unit declares `WatchdogSec=`). Pings `WATCHDOG=1` at half the
/// configured interval, gated by [`watchdog_should_ping`].
pub(crate) fn spawn_systemd_watchdog(svc: Arc<ServiceState>) {
    let usec: u64 = match std::env::var("WATCHDOG_USEC")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(u) if u > 0 => u,
        _ => return, // no WatchdogSec configured → nothing to feed
    };
    let period = std::time::Duration::from_micros(usec / 2); // systemd-recommended margin
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tracing::info!(watchdog_usec = usec, "systemd watchdog keepalive started");
        loop {
            tick.tick().await;
            let cache_fresh = svc
                .posture_cache
                .read()
                .ok()
                .and_then(|g| g.as_ref().map(|c| !c.is_stale(now_ms())))
                .unwrap_or(false);
            if watchdog_should_ping(svc.app.is_active(), cache_fresh) {
                sd_notify("WATCHDOG=1");
            } else {
                tracing::error!(
                    "systemd watchdog: Active posture engine appears stalled (cache stale) — \
                     withholding WATCHDOG ping; systemd will restart (SG-003 / SG9 fail-closed)"
                );
            }
        }
    });
}
