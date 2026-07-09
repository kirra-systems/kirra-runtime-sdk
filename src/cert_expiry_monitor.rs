//! mTLS cert-principal expiry monitor (WS-1 · G7 · Track 1.2 · WP-15 / MGA G-19).
//!
//! A pinned client cert (`cert_principals`) carries an X.509 `notAfter`. The auth
//! path already fail-closes a request under an EXPIRED cert (a lapsed cert stops
//! authorizing exactly like a revoked one — see `auth.rs`). This monitor is the
//! OBSERVABILITY arm of that lifecycle: it periodically censuses the registry and
//! WARNs (log + hash-chained audit) when a cert has lapsed or is about to, so an
//! operator renews *before* the credential silently starts 401-ing — the same
//! "warn before it bites" discipline as the telemetry watchdog.
//!
//! It NEVER mutates trust: it only reads and reports. A revoked/expired cert is
//! already dead at the auth layer regardless of whether this monitor ran, so the
//! worker is supervised **non-critical** — a panic restarts it, but it can never
//! force a posture change or make anything unsafe. The Prometheus `/metrics`
//! surface (`kirra_cert_principals{state=…}`) carries the same census at scrape
//! time; this task adds the push-style audit trail the scrape cannot.

use std::sync::Arc;

use tokio::time::{interval, Duration};

use crate::clock::{Clock, SystemClock};
use crate::verifier::AppState;
use crate::verifier_store::{CertExpirySummary, VerifierStore};

/// How often the expiry census runs. Cert expiry is a slow (days/weeks) concern,
/// so an hourly sweep is ample to surface a lapse well within an operator's
/// renewal window without touching SQLite meaningfully.
pub const CERT_EXPIRY_SWEEP_MS: u64 = 3_600_000; // 1 hour

/// "Expiring soon" horizon — a cert whose `notAfter` falls within this window of
/// the census instant is flagged for renewal. 14 days is a comfortable operator
/// lead time for an offline cert-reissue + re-pin.
pub const CERT_EXPIRY_WARN_WINDOW_MS: u64 = 14 * 24 * 3_600_000; // 14 days

/// Run one expiry census and emit warnings. Pure over the store: computes the
/// [`CertExpirySummary`] and, when anything is lapsed or lapsing, WARN-logs and
/// appends ONE hash-chained `CertPrincipalExpiryWarning` audit event carrying the
/// census (never any fingerprint/secret). Returns the summary for the caller/tests.
///
/// Silent when the registry is entirely healthy (no expired, none expiring) — the
/// audit chain records lapses, not routine all-clear sweeps.
pub fn sweep_cert_expiry_once(
    store: &mut VerifierStore,
    now_ms: u64,
    warn_window_ms: u64,
) -> rusqlite::Result<CertExpirySummary> {
    let summary = store.cert_expiry_summary(now_ms, warn_window_ms)?;
    if summary.expired > 0 || summary.expiring_soon > 0 {
        tracing::warn!(
            expired = summary.expired,
            expiring_soon = summary.expiring_soon,
            active = summary.active,
            total = summary.total,
            warn_window_ms,
            "mTLS cert principals are lapsed or lapsing — renew before they stop authorizing"
        );
        // Non-repudiable trail: an operator can prove WHEN a lapse was first
        // surfaced. Payload is counts only (no fingerprint, no secret). The audit
        // IS this sweep's product, so PROPAGATE a failure (Copilot #857) rather than
        // dropping it — the caller's `Ok(Err(e))` arm logs "sweep DB error", so a
        // broken audit trail is surfaced, not silently skipped while callers/tests
        // treat the entry as evidence.
        store.append_clearance_audit_event(
            "CertPrincipalExpiryWarning",
            &serde_json::json!({
                "expired": summary.expired,
                "expiring_soon": summary.expiring_soon,
                "active": summary.active,
                "total": summary.total,
                "warn_window_ms": warn_window_ms,
            })
            .to_string(),
            now_ms,
        )?;
    }
    Ok(summary)
}

/// Spawn the cert-expiry monitor (production entry point — real `SystemClock`).
/// Supervised non-critical, run-forever.
pub fn spawn_cert_expiry_monitor(app: Arc<AppState>) {
    spawn_cert_expiry_monitor_with_clock(app, Arc::new(SystemClock));
}

/// Same as [`spawn_cert_expiry_monitor`] but with an injected clock for
/// deterministic tests; the production path passes `SystemClock`.
pub fn spawn_cert_expiry_monitor_with_clock(app: Arc<AppState>, clock: Arc<dyn Clock>) {
    crate::supervisor::spawn_supervised(
        "cert_expiry_monitor",
        /* critical   */ false,
        /* run-forever */ true,
        /* escalate    */ None,
        move || {
            let app = Arc::clone(&app);
            let clock = Arc::clone(&clock);
            async move {
                let mut sweep = interval(Duration::from_millis(CERT_EXPIRY_SWEEP_MS));
                // A starved runtime should not fire a catch-up burst — each census is
                // idempotent, so skipping missed ticks is fine.
                sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    sweep.tick().await;
                    let now = clock.now_ms();
                    // EP-11: time this census against the manifest's deadline budget
                    // (observability; NonCritical — never escalates).
                    let sweep_start_ms = now;
                    match app
                        .store
                        .call(move |s| sweep_cert_expiry_once(s, now, CERT_EXPIRY_WARN_WINDOW_MS))
                        .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "cert-expiry monitor sweep DB error")
                        }
                        Err(_) => tracing::error!("cert-expiry monitor sweep task failed"),
                    }
                    let elapsed_ms = clock.now_ms().saturating_sub(sweep_start_ms);
                    app.deadline_registry
                        .record("cert_expiry_monitor", elapsed_ms);
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> VerifierStore {
        VerifierStore::new(":memory:").expect("in-memory store")
    }

    #[test]
    fn healthy_registry_sweeps_silently_no_audit() {
        let mut s = store();
        // One active cert whose expiry is comfortably beyond the 14-day warn window
        // from `now = 10_000` (so it is neither expired nor expiring-soon).
        s.register_cert_principal("svc", "fp", "integrator", Some(5_000_000_000), 1_000)
            .unwrap();
        let sum = sweep_cert_expiry_once(&mut s, 10_000, CERT_EXPIRY_WARN_WINDOW_MS).unwrap();
        assert_eq!(sum.active, 1);
        assert_eq!(sum.expired, 0);
        assert_eq!(sum.expiring_soon, 0);
        // No lapse → no audit event.
        assert_eq!(
            s.count_audit_events_for_test("CertPrincipalExpiryWarning"),
            0
        );
    }

    #[test]
    fn lapsed_or_lapsing_registry_audits_the_warning() {
        let mut s = store();
        // Expired (past notAfter).
        s.register_cert_principal("stale", "fp-stale", "integrator", Some(5_000), 1_000)
            .unwrap();
        // Expiring within the window.
        s.register_cert_principal("soon", "fp-soon", "integrator", Some(10_500), 1_000)
            .unwrap();
        let sum = sweep_cert_expiry_once(&mut s, 10_000, 1_000).unwrap();
        assert_eq!(sum.expired, 1);
        assert_eq!(sum.expiring_soon, 1);
        // The lapse is recorded once in the hash-chained audit log.
        assert_eq!(
            s.count_audit_events_for_test("CertPrincipalExpiryWarning"),
            1
        );
        // A second sweep with the same state records again (each census is a fresh
        // observation — the audit chain is append-only, dedup is the consumer's job).
        let _ = sweep_cert_expiry_once(&mut s, 10_000, 1_000).unwrap();
        assert_eq!(
            s.count_audit_events_for_test("CertPrincipalExpiryWarning"),
            2
        );
    }
}
