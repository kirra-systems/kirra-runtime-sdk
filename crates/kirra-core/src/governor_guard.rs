//! Shared governor input guards — the convergence point for #410.
//!
//! The governor enforces "reject non-finite command input" at several independent
//! enforcement points, and the copies had drifted: the invariant was rigorous on
//! the AV vehicle path but MISSING on the scalar kernel + C FFI (#404) and the
//! parko differential-drive channel (#407). This module gives that invariant ONE
//! implementation every point calls, so the class cannot silently regress at a
//! NEW enforcement point.
//!
//! **Shared = the PREDICATE only.** Each call site keeps its own fail-closed
//! ACTION (a verdict, `0.0`, an `EnforcementAction::Deny`, an MRC fallback) and
//! its own diagnostics — only the finite-ness test is centralised here.
//!
//! **Call sites (keep this list current — a new governor entry MUST call this):**
//!   - `kirra_core::KirraKernelGovernor::evaluate` (scalar kernel, #404)
//!   - `ffi::kirra_filter_rotate_velocity` (C ABI, #404)
//!   - `parko_kirra::diverse::DiverseKirraGovernor::gate` (separate workspace —
//!     depends on this crate and imports this predicate, #407)
//!
//! The AV reference model `gateway::kinematics_contract::validate_vehicle_command`
//! intentionally keeps PER-FIELD checks (a distinct `DenyCode` per offending
//! field); it is the model the peripheral paths converge toward, not a caller of
//! this combined predicate. The fabric `AssetGovernor` path was reviewed clean in
//! the #410 sweep and is not an actuator-command ingress that needs this guard.

/// Returns `true` iff every value is IEEE-754 finite (no `NaN`, no `±Inf`).
///
/// The shared non-finite-rejection predicate (#410). A `NaN` compares `false`
/// against every envelope/rate threshold, so an unguarded `NaN` would fall
/// straight through a governor's comparisons as a "safe" command; every entry
/// point must reject non-finite input BEFORE any comparison and fail closed.
/// An empty slice is vacuously finite (`true`).
#[must_use]
#[inline]
pub fn all_finite(values: &[f64]) -> bool {
    values.iter().all(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::all_finite;

    #[test]
    fn accepts_all_finite_rejects_any_nonfinite() {
        // Vacuously true on empty; true when every value is finite.
        assert!(all_finite(&[]));
        assert!(all_finite(&[0.0, -1.5, 1.0e6, f64::MIN, f64::MAX]));

        // False if ANY value is non-finite (NaN or either infinity), in any slot.
        assert!(!all_finite(&[f64::NAN]));
        assert!(!all_finite(&[1.0, f64::INFINITY]));
        assert!(!all_finite(&[f64::NEG_INFINITY, 2.0]));
        assert!(!all_finite(&[0.0, f64::NAN, 3.0]));
    }
}
