//! Sidecar network policy: the loopback-default bind gate and the ingest
//! rate limiter (the `kirra-fleet-transport::ingress_limit` shape, sized for
//! a single local consumer).

use std::net::SocketAddr;

/// Opt-in for a non-loopback bind. Unset/false → a sidecar refuses to start
/// on a routable address (fail-closed startup abort, the half-configured-TLS
/// convention): these services carry no authentication of their own, so the
/// default deployment shape is strictly on-box (the occy_doer bridge and the
/// sidecars share the host; the verifier is the authenticated plane).
pub const ALLOW_NONLOCAL_ENV: &str = "KIRRA_SIDECAR_ALLOW_NONLOCAL";

/// Is the opt-in set (`1`/`true`)?
#[must_use]
pub fn allow_nonlocal_from_env() -> bool {
    std::env::var(ALLOW_NONLOCAL_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Enforce the bind policy on a configured listen address. Pure over
/// `(addr, allow_nonlocal)` for testability; the binaries abort startup on
/// `Err`. Fail-closed: an address we cannot classify as loopback is treated
/// as non-loopback.
pub fn enforce_bind_policy(addr: &str, allow_nonlocal: bool) -> Result<(), String> {
    if allow_nonlocal {
        return Ok(());
    }
    let is_loopback = addr
        .parse::<SocketAddr>()
        .map(|sa| sa.ip().is_loopback())
        .unwrap_or_else(|_| {
            // Accept the conventional hostname spelling; anything else is
            // unclassifiable → not loopback (fail-closed).
            addr.starts_with("localhost:")
        });
    if is_loopback {
        Ok(())
    } else {
        Err(format!(
            "refusing to bind non-loopback address {addr}: the sidecars are \
             unauthenticated on-box services. Set {ALLOW_NONLOCAL_ENV}=1 only \
             behind a trusted network boundary."
        ))
    }
}

/// A token-bucket rate limiter, pure over a caller-supplied `now_ms` clock
/// (the `IngressRateLimiter` shape, single-source). Over-rate requests are
/// shed CHEAPLY before the expensive work (an LLM call, a plan+check cycle).
pub struct RateLimiter {
    capacity: f64,
    refill_per_s: f64,
    tokens: f64,
    last_ms: u64,
}

impl RateLimiter {
    #[must_use]
    pub fn new(capacity: f64, refill_per_s: f64) -> Self {
        Self {
            capacity,
            refill_per_s,
            tokens: capacity,
            last_ms: 0,
        }
    }

    /// Admit one request at `now_ms`? Refills by elapsed wall time, then
    /// spends one token if available.
    pub fn admit(&mut self, now_ms: u64) -> bool {
        let elapsed_s = now_ms.saturating_sub(self.last_ms) as f64 / 1000.0;
        self.last_ms = self.last_ms.max(now_ms);
        self.tokens = (self.tokens + elapsed_s * self.refill_per_s).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Wall clock in ms since the UNIX epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds_are_admitted_without_the_flag() {
        for addr in ["127.0.0.1:8100", "[::1]:8101", "localhost:8102"] {
            assert!(enforce_bind_policy(addr, false).is_ok(), "{addr}");
        }
    }

    #[test]
    fn nonlocal_or_unparseable_binds_are_refused_without_the_flag() {
        for addr in ["0.0.0.0:8100", "192.168.1.7:8100", "example.com:80", "garbage"] {
            assert!(enforce_bind_policy(addr, false).is_err(), "{addr}");
        }
        // The explicit opt-in admits them (the operator owns the boundary).
        assert!(enforce_bind_policy("0.0.0.0:8100", true).is_ok());
    }

    #[test]
    fn rate_limiter_sheds_a_burst_and_recovers_by_refill() {
        let mut rl = RateLimiter::new(3.0, 1.0);
        let t0 = 10_000;
        assert!(rl.admit(t0) && rl.admit(t0) && rl.admit(t0), "burst up to capacity");
        assert!(!rl.admit(t0), "over-burst is shed");
        assert!(!rl.admit(t0 + 500), "half a second refills half a token");
        assert!(rl.admit(t0 + 1_100), "a second refills one");
        // A stale clock never panics or over-refills.
        assert!(!rl.admit(t0), "time going backwards refills nothing");
    }
}
