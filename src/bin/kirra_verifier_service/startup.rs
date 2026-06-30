// Startup invariants and fail-closed boot checks (SG-008).

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
    WatchdogNotSpawned,
    PostureEngineDown,
}

impl std::fmt::Display for StartupInvariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::HardwareRootUntrusted => "hardware root of trust unavailable/unresponsive (TPM)",
            Self::AdminTokenMissing => "KIRRA_ADMIN_TOKEN absent or empty",
            Self::SqliteNotWal => "SQLite store is not in WAL journal mode",
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
