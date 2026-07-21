// src/bin/kirra_verifier_service/startup.rs
// SG-008 (ASIL D) — process fail-closed startup sentinel.
//
// Extracted verbatim from `kirra_verifier_service.rs` to keep the binary's
// entry point lean; the predicate, its types, and the CERT-003 RTM coverage
// tests live together here. Re-exported into the binary via `use startup::*;`
// so `main` and the in-file tests reference them unqualified.

// --- SG-008: process fail-closed startup sentinel ---------------------------
//
// Verifies: SG-008 (ASIL D) — Process Fail-Closed on Startup. The service must
// refuse to bind its listener unless the safety-critical startup invariants
// hold. The checks are factored into a pure predicate so they are
// deterministically testable without `process::exit` (see sg_008_cert_tests):
// `main` builds a `StartupContext` from the real boot facts, and aborts BEFORE
// `TcpListener::bind` on any `Err` — so "the listener never binds before
// invariants pass" holds by construction (bind is strictly after the check).

/// The boot facts the startup sentinel evaluates. Built once in `main` from the
/// real environment/store/wiring; consumed by `check_startup_invariants`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StartupContext {
    /// The hardware root of trust is healthy (`StartupSentinel::verify_hardware_root()
    /// == Trusted`). Fail-closed: an unavailable/unresponsive TPM aborts startup.
    /// SS-001 lists this as a startup entry invariant; this is its enforcement
    /// point. Without the `tpm` feature the sentinel returns `Trusted` (no-op pass).
    pub hardware_root_trusted: bool,
    /// `KIRRA_ADMIN_TOKEN` is present and non-empty (CRITICAL INVARIANT #6).
    pub admin_token_present: bool,
    /// The SQLite store reports `journal_mode = wal` (CRITICAL INVARIANT #12
    /// ordering depends on the WAL-mode durable seam).
    pub sqlite_wal: bool,
    /// #791 F3 — `init_generation_from_store` completed successfully in this
    /// process (`posture_engine::generation_initialized()`), so the posture
    /// generation counter was seeded from the persisted high-water BEFORE any
    /// recalculation can claim a generation. Required in BOTH modes: the
    /// Active initial recalc and the standby PROMOTION recalc each depend on
    /// it, and a regression here silently time-reverses generations across
    /// restarts (the exact WS-0.2 wiring omission this fact pins).
    pub generation_initialized: bool,
    /// True on the Active path. PassiveStandby is read-only and intentionally
    /// runs neither the watchdog nor the posture engine, so those two
    /// invariants are evaluated ONLY when this is true.
    pub mode_active: bool,
    /// The telemetry watchdog task was spawned (Active path; SG-003 / SG9).
    pub watchdog_spawned: bool,
    /// The serialized posture-engine worker is running (`posture_engine_tx` set).
    pub posture_engine_running: bool,
}

/// The first violated startup invariant, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupInvariant {
    HardwareRootUntrusted,
    AdminTokenMissing,
    SqliteNotWal,
    GenerationNotInitialized,
    WatchdogNotSpawned,
    PostureEngineDown,
}

impl std::fmt::Display for StartupInvariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::HardwareRootUntrusted => "hardware root of trust unavailable/unresponsive (TPM)",
            Self::AdminTokenMissing => "KIRRA_ADMIN_TOKEN absent or empty",
            Self::SqliteNotWal => "SQLite store is not in WAL journal mode",
            Self::GenerationNotInitialized => {
                "posture-generation counter not seeded from the persisted high-water \
                 (init_generation_from_store did not run — generations would time-reverse)"
            }
            Self::WatchdogNotSpawned => "telemetry watchdog not spawned (Active path)",
            Self::PostureEngineDown => "posture-engine worker not running (Active path)",
        };
        write!(f, "{s}")
    }
}

/// SG-008 (ASIL D) — pure startup-invariant predicate. Returns the first
/// violated invariant, or `Ok(())` when all hold. Fail-closed and order-stable.
/// The watchdog / posture-engine invariants apply only to the Active path
/// (`mode_active`); PassiveStandby is read-only and runs neither, so requiring
/// them there would wrongly abort a valid standby.
//
// Verifies: SG-008
pub(crate) fn check_startup_invariants(ctx: &StartupContext) -> Result<(), StartupInvariant> {
    // Hardware root of trust first — it is the most fundamental precondition
    // (SS-001 entry invariant). Fail-closed before anything else is trusted.
    if !ctx.hardware_root_trusted {
        return Err(StartupInvariant::HardwareRootUntrusted);
    }
    if !ctx.admin_token_present {
        return Err(StartupInvariant::AdminTokenMissing);
    }
    if !ctx.sqlite_wal {
        return Err(StartupInvariant::SqliteNotWal);
    }
    // #791 F3: both modes — the standby's promotion recalc needs the seeded
    // counter exactly as the Active initial recalc does.
    if !ctx.generation_initialized {
        return Err(StartupInvariant::GenerationNotInitialized);
    }
    if ctx.mode_active {
        if !ctx.watchdog_spawned {
            return Err(StartupInvariant::WatchdogNotSpawned);
        }
        if !ctx.posture_engine_running {
            return Err(StartupInvariant::PostureEngineDown);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CERT-003 — SG-008 RTM coverage (ASIL D): Process Fail-Closed on Startup
//
// Verifies: SG-008 — the startup sentinel refuses to bind unless every
// safety-critical startup invariant holds. We test the PURE predicate
// (`check_startup_invariants`) for each individual violation, the all-present
// Ok case, and the mode distinction (watchdog/posture-engine are required only
// on the Active path). The abort + bind ordering is structural: `main` calls
// this predicate immediately before `TcpListener::bind` and `process::exit(1)`s
// on Err, so a failing predicate means the listener is never reached. These
// live in the bin (the predicate is `pub(crate)` and not visible to an external
// integration test).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod sg_008_cert_tests {
    use super::{check_startup_invariants, StartupContext, StartupInvariant};

    /// All invariants satisfied on the Active path.
    fn all_ok_active() -> StartupContext {
        StartupContext {
            hardware_root_trusted: true,
            admin_token_present: true,
            sqlite_wal: true,
            generation_initialized: true,
            mode_active: true,
            watchdog_spawned: true,
            posture_engine_running: true,
        }
    }

    /// #791 F3 — a binary wired without the generation seeding must fail
    /// closed before the listener binds, in EITHER mode (the standby's
    /// promotion recalc depends on the seed exactly as the Active path does).
    #[test]
    fn test_startup_aborts_when_generation_not_initialized() {
        let ctx = StartupContext {
            generation_initialized: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::GenerationNotInitialized),
            "SG-008/#791 F3: the generation-seeding wiring is a startup invariant"
        );
        let standby = StartupContext {
            mode_active: false,
            watchdog_spawned: false,
            posture_engine_running: false,
            ..ctx
        };
        assert_eq!(
            check_startup_invariants(&standby),
            Err(StartupInvariant::GenerationNotInitialized),
            "SG-008/#791 F3: required on the standby path too"
        );
    }

    #[test]
    fn test_startup_ok_when_all_invariants_hold() {
        assert_eq!(
            check_startup_invariants(&all_ok_active()),
            Ok(()),
            "SG-008: startup must succeed when all invariants hold"
        );
    }

    #[test]
    fn test_startup_aborts_when_hardware_root_untrusted() {
        // SS-001 entry invariant: an unavailable/unresponsive hardware root of
        // trust must abort startup before the listener binds (fail-closed).
        let ctx = StartupContext {
            hardware_root_trusted: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::HardwareRootUntrusted),
            "SG-008: startup must fail closed when the hardware root of trust is untrusted"
        );
    }

    #[test]
    fn test_startup_aborts_without_admin_token() {
        let ctx = StartupContext {
            admin_token_present: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: startup must fail closed when KIRRA_ADMIN_TOKEN is absent/empty"
        );
    }

    #[test]
    fn test_startup_aborts_when_sqlite_not_wal() {
        let ctx = StartupContext {
            sqlite_wal: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::SqliteNotWal),
            "SG-008: startup must fail closed when the store is not in WAL mode"
        );
    }

    #[test]
    fn test_startup_aborts_when_watchdog_not_spawned_on_active() {
        let ctx = StartupContext {
            watchdog_spawned: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::WatchdogNotSpawned),
            "SG-008: an Active node must fail closed if the telemetry watchdog is not spawned"
        );
    }

    #[test]
    fn test_startup_aborts_when_posture_engine_down_on_active() {
        let ctx = StartupContext {
            posture_engine_running: false,
            ..all_ok_active()
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::PostureEngineDown),
            "SG-008: an Active node must fail closed if the posture-engine worker is not running"
        );
    }

    /// PassiveStandby is read-only and runs neither the watchdog nor the
    /// posture engine — so their absence must NOT abort a standby, but the
    /// admin-token and WAL invariants still apply.
    #[test]
    fn test_standby_ok_without_watchdog_or_posture_engine() {
        let ctx = StartupContext {
            hardware_root_trusted: true,
            admin_token_present: true,
            sqlite_wal: true,
            generation_initialized: true,
            mode_active: false,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Ok(()),
            "SG-008: PassiveStandby must boot without watchdog/posture-engine (not required in read-only mode)"
        );
    }

    #[test]
    fn test_standby_still_requires_admin_token_and_wal() {
        let no_token = StartupContext {
            hardware_root_trusted: true,
            admin_token_present: false,
            sqlite_wal: true,
            generation_initialized: true,
            mode_active: false,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&no_token),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: admin token is required in every mode"
        );
        let no_wal = StartupContext {
            admin_token_present: true,
            sqlite_wal: false,
            ..no_token
        };
        assert_eq!(
            check_startup_invariants(&no_wal),
            Err(StartupInvariant::SqliteNotWal),
            "SG-008: WAL mode is required in every mode"
        );
    }

    /// Order stability: when multiple invariants are violated (with a trusted
    /// hardware root, which is checked first), the admin-token check wins —
    /// deterministic diagnosis.
    #[test]
    fn test_invariant_check_order_is_stable() {
        let ctx = StartupContext {
            hardware_root_trusted: true,
            admin_token_present: false,
            sqlite_wal: false,
            generation_initialized: false,
            mode_active: true,
            watchdog_spawned: false,
            posture_engine_running: false,
        };
        assert_eq!(
            check_startup_invariants(&ctx),
            Err(StartupInvariant::AdminTokenMissing),
            "SG-008: with a trusted hardware root, the admin-token invariant is reported first when several are violated"
        );
    }
}
