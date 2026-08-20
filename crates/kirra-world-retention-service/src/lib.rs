//! **The World-side retention process — Tier 5 box 5b.**
//!
//! ADR-0040 made retention a **Tier 1 exit criterion**, and `WM_SCOPE.md` §4
//! recorded it done on 2026-08-06 with the sentence *"something now empties the
//! store without being asked."*
//!
//! That sentence was not true. The deciding half
//! ([`kirra_world::retention`]), the acting half
//! ([`kirra_world_store::retention_driver`]) and the scheduling half
//! ([`kirra_world_store::retention_sweeper`]) were all written — and **nothing
//! called any of them.** The orphan gate said so plainly: both store-side
//! modules sat in `ci/orphan_cores_baseline.json`, referenced only by their own
//! `pub mod` lines.
//!
//! This crate is the missing caller. It exists so that sentence becomes true.
//!
//! # Why the claim survived
//!
//! Because "the code exists" and "the code runs" look identical in a scope
//! document, and until #1458–#1460 the detector that should have distinguished
//! them could be satisfied by a textual mention. That is the same defect class
//! Tier 4's residual 2 closed for the explanation path, one level up: an
//! integration gate crediting reachability is not proving integration.
//!
//! # Why its own process
//!
//! `kirra-world-explain-service` is the only other World-side process, and it
//! declines this job in its own documentation: it owns a process boundary, is
//! read-only, and "must not change what it is describing in the course of
//! describing it". Retention DELETES. Hosting a compaction thread there would
//! hand a read-only surface the one irreversible capability in the subsystem.
//!
//! And it is emphatically not a verifier monitor. `campaign_monitor` and
//! `cert_expiry_monitor` are the obvious precedent and the wrong location: they
//! live inside the safety closure, so spawning the sweeper there would pull
//! `kirra-world` into it and breach ADR-0039's **Fence B**. The precedent
//! copied is the SHAPE — an interval, an explicit start, fail-closed on
//! anything unestablished — never the address.
//!
//! # What it does not do
//!
//! It does not decide. Every horizon, blocker and eligibility judgement belongs
//! to [`kirra_world::retention::decide`], which is exhaustively tested without a
//! database. This process supplies a clock and a schedule, and reports what
//! happened.

use std::path::Path;
use std::time::Duration;

use kirra_world::retention::RetentionPolicy;
use kirra_world_store::retention_sweeper::{RetentionSweeper, RETENTION_SWEEP_MS};
use kirra_world_store::StoreError;

/// Where the store to sweep lives.
pub const WORLD_DB_ENV: &str = "KIRRA_WORLD_DB";

/// Optional sweep interval override, in milliseconds.
pub const SWEEP_INTERVAL_ENV: &str = "KIRRA_WORLD_RETENTION_SWEEP_MS";

/// Why the process refused to start.
///
/// Fail-closed at startup rather than a thread rediscovering the problem every
/// hour — and each variant names something an operator can fix, because
/// "retention did not run" is otherwise indistinguishable from "retention ran
/// and found nothing".
#[derive(Debug, PartialEq, Eq)]
pub enum StartupRefusal {
    /// No store path configured. There is deliberately no default: a default
    /// would point this process at a store nobody meant to compact, and
    /// compaction is the one irreversible operation in the subsystem.
    NoDatabasePath,
    /// The interval override was set but unusable.
    BadInterval(String),
}

impl std::fmt::Display for StartupRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDatabasePath => write!(
                f,
                "{WORLD_DB_ENV} is required — refusing to start without a store \
                 to apply retention to. There is no default: compaction is \
                 irreversible, and a defaulted path is a store nobody chose."
            ),
            Self::BadInterval(v) => write!(
                f,
                "{SWEEP_INTERVAL_ENV}=`{v}` is not a positive number of \
                 milliseconds. Refusing rather than falling back to the default: \
                 an operator who set this meant something by it."
            ),
        }
    }
}

/// Resolve the sweep interval from an optional override.
///
/// Pure over its input so the policy is testable without env mutation
/// (INV-13). Unset → [`RETENTION_SWEEP_MS`]. Set-but-unusable → refuse; zero is
/// refused too, because a zero interval is a busy loop holding a write lock.
///
/// # Errors
///
/// [`StartupRefusal::BadInterval`] on a non-numeric or zero value.
pub fn resolve_interval(raw: Option<&str>) -> Result<Duration, StartupRefusal> {
    match raw.map(str::trim) {
        None | Some("") => Ok(Duration::from_millis(RETENTION_SWEEP_MS)),
        Some(v) => match v.parse::<u64>() {
            Ok(ms) if ms > 0 => Ok(Duration::from_millis(ms)),
            _ => Err(StartupRefusal::BadInterval(v.to_string())),
        },
    }
}

/// Start sweeping `path`, with the horizons OQ2 ruled.
///
/// The returned [`RetentionSweeper`] owns the thread: dropping it stops the
/// sweep, so a caller must hold it for as long as retention should run.
///
/// # Errors
///
/// [`StoreError`] if the store cannot be opened — proven on this thread before
/// any promise to sweep is made.
pub fn start_retention(path: &Path, interval: Duration) -> Result<RetentionSweeper, StoreError> {
    RetentionSweeper::start(path, RetentionPolicy::oq2(), interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_interval_uses_the_housekeeping_default() {
        assert_eq!(
            resolve_interval(None).expect("default"),
            Duration::from_millis(RETENTION_SWEEP_MS)
        );
        assert_eq!(
            resolve_interval(Some("   ")).expect("blank is unset"),
            Duration::from_millis(RETENTION_SWEEP_MS)
        );
    }

    /// A deliberate override is honoured — the positive control, so a resolver
    /// that refused everything could not pass the refusal cases below.
    #[test]
    fn a_deliberate_override_is_honoured() {
        assert_eq!(
            resolve_interval(Some("250")).expect("override"),
            Duration::from_millis(250)
        );
    }

    /// Refused, not silently defaulted. An operator who sets this meant
    /// something, and a sweeper running hourly when they asked for minutes is a
    /// disk that fills while the configuration says it should not.
    #[test]
    fn an_unusable_interval_is_refused_rather_than_defaulted() {
        for bad in ["0", "-5", "soon", "1.5", "9999999999999999999999"] {
            assert!(
                matches!(
                    resolve_interval(Some(bad)),
                    Err(StartupRefusal::BadInterval(_))
                ),
                "`{bad}` must be refused"
            );
        }
    }
}
