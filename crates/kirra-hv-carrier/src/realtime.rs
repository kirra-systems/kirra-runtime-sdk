//! W2 (#1028) — real-time configuration for the ENFORCED SHM loop.
//!
//! The in-line governor/actuator cycle (`kirra-inline-governor`) is the
//! hard-real-time path: a planner publishes into the shared region, the governor
//! bounds in-line, the actuator releases on proof. Off the QNX target, on stock
//! Linux, that cycle is by default `SCHED_OTHER` — **preemptible, core-migratable,
//! and subject to a major page fault on the first touch of a demand-paged mapping
//! inside the loop**. Every host timing number then carries an unbounded
//! scheduling/paging tail, and there is no real-time-configured path at all.
//!
//! This module wires the three primitives that remove that tail, as **opt-in**,
//! **degrade-with-warning** calls the enforced-loop entry point makes ONCE, before
//! entering the cycle:
//!
//! 1. [`configure_realtime`] — `mlockall(MCL_CURRENT|MCL_FUTURE)` (no future page
//!    fault), `sched_setscheduler(SCHED_FIFO)` (not preempted by `SCHED_OTHER`),
//!    and `sched_setaffinity` (no core migration). Each step is attempted
//!    independently; a step the process is not privileged for (the common CI /
//!    unprivileged-container case: `EPERM`) is reported [`RtStep::Degraded`] with a
//!    warning, never a panic and never a hard failure — a governor that cannot get
//!    RT scheduling must still run (fail-safe), it just runs without the guarantee.
//! 2. [`PosixShmRegion::prefault`](crate::PosixShmRegion::prefault) /
//!    [`PosixShmReader::prefault`](crate::PosixShmReader::prefault) — `madvise
//!    (MADV_WILLNEED)` + a touch of every page, so the FIRST access inside the loop
//!    cannot take a major fault (paired with `mlockall(MCL_FUTURE)` the pages then
//!    stay resident).
//!
//! Scope: this is the **enforced-loop** real-time path. The verifier's
//! `execution_manager::TASK_MANIFEST` scheduling intent is a SEPARATE, control-plane
//! concern (the supervised background monitors are not hard-real-time and must NOT
//! be `SCHED_FIFO` — that would starve the async runtime); wiring that manifest is
//! tracked independently. On the QNX target the same three primitives are the
//! partition's scheduling/locking config (ADR-0006 Clause 3).

/// What one real-time step did. `Applied` = the syscall succeeded; `Skipped` = the
/// caller did not request it; `Degraded` = it was requested but refused (e.g.
/// `EPERM` unprivileged) — the loop runs WITHOUT that guarantee, fail-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtStep {
    /// The syscall succeeded.
    Applied,
    /// The caller did not request this step (`RtConfig` had it off).
    Skipped,
    /// Requested but refused; the string is the OS reason (never a panic).
    Degraded(String),
}

impl RtStep {
    /// True unless this step was requested-and-refused — i.e. every guarantee the
    /// caller asked for actually holds.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self, RtStep::Degraded(_))
    }
}

/// The outcome of [`configure_realtime`], one [`RtStep`] per primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtStatus {
    /// `mlockall(MCL_CURRENT | MCL_FUTURE)`.
    pub memory_lock: RtStep,
    /// `sched_setscheduler(0, SCHED_FIFO, …)`.
    pub scheduler: RtStep,
    /// `sched_setaffinity(0, …)`.
    pub affinity: RtStep,
}

impl RtStatus {
    /// True iff EVERY requested step was applied (no requested step degraded).
    #[must_use]
    pub fn fully_applied(&self) -> bool {
        self.memory_lock.is_ok() && self.scheduler.is_ok() && self.affinity.is_ok()
    }
}

/// What real-time configuration to request for the calling thread. Defaults to
/// all-off (a no-op — byte-identical to not calling this at all), so a caller
/// opts in field by field.
#[derive(Debug, Clone, Default)]
pub struct RtConfig {
    /// `mlockall(MCL_CURRENT | MCL_FUTURE)` — lock all current + future pages
    /// resident so the enforced loop never takes a page fault.
    pub lock_memory: bool,
    /// `Some(prio)` → `SCHED_FIFO` at that priority (1..=99 on Linux); `None` → skip.
    pub fifo_priority: Option<i32>,
    /// `Some(cpu)` → pin the calling thread to that CPU; `None` → skip.
    pub cpu_affinity: Option<usize>,
}

impl RtConfig {
    /// The recommended enforced-loop configuration: lock memory, `SCHED_FIFO` at a
    /// high (but sub-ceiling, leaving headroom for a watchdog) priority, pinned to
    /// `cpu`. The exact priority/CPU is a deployment/integration choice; this is a
    /// sensible default for the governor thread.
    #[must_use]
    pub fn enforced_loop(cpu: usize) -> Self {
        Self {
            lock_memory: true,
            fifo_priority: Some(80),
            cpu_affinity: Some(cpu),
        }
    }
}

/// Apply `cfg` to the CALLING thread, best-effort and fail-safe. Each requested
/// step is attempted independently; a refused step is reported `Degraded(reason)`
/// with a `stderr` warning (degrade-with-warning) and the others still apply. This
/// NEVER panics and NEVER aborts the loop: a governor that cannot obtain RT
/// scheduling must still bound commands — it just does so without the timing
/// guarantee, which the returned [`RtStatus`] makes explicit for the caller to log
/// / gate on.
#[cfg(target_os = "linux")]
#[must_use]
pub fn configure_realtime(cfg: &RtConfig) -> RtStatus {
    RtStatus {
        memory_lock: if cfg.lock_memory {
            apply_mlockall()
        } else {
            RtStep::Skipped
        },
        scheduler: match cfg.fifo_priority {
            Some(prio) => apply_sched_fifo(prio),
            None => RtStep::Skipped,
        },
        affinity: match cfg.cpu_affinity {
            Some(cpu) => apply_affinity(cpu),
            None => RtStep::Skipped,
        },
    }
}

/// Non-Linux hosts (e.g. a macOS dev box building the workspace): the three
/// primitives are Linux/QNX-specific, so every requested step degrades cleanly
/// rather than failing to compile. The real target is Linux/QNX.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn configure_realtime(cfg: &RtConfig) -> RtStatus {
    let unsupported = |requested: bool| {
        if requested {
            RtStep::Degraded("real-time syscalls are Linux/QNX-only on this host".to_string())
        } else {
            RtStep::Skipped
        }
    };
    RtStatus {
        memory_lock: unsupported(cfg.lock_memory),
        scheduler: unsupported(cfg.fifo_priority.is_some()),
        affinity: unsupported(cfg.cpu_affinity.is_some()),
    }
}

#[cfg(target_os = "linux")]
fn degrade(step: &str) -> RtStep {
    let e = std::io::Error::last_os_error();
    eprintln!(
        "kirra-hv-carrier: real-time step '{step}' DEGRADED: {e} — the enforced loop \
         runs WITHOUT this guarantee (fail-safe). Grant CAP_SYS_NICE / CAP_IPC_LOCK \
         or run on the RT-configured target to enable it."
    );
    RtStep::Degraded(e.to_string())
}

#[cfg(target_os = "linux")]
fn apply_mlockall() -> RtStep {
    // SAFETY: FFI with constant flags; no memory is passed in.
    let rc = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
    if rc == 0 {
        RtStep::Applied
    } else {
        degrade("mlockall")
    }
}

#[cfg(target_os = "linux")]
fn apply_sched_fifo(priority: i32) -> RtStep {
    // Clamp to the kernel's valid SCHED_FIFO band [1, 99] so a caller typo can
    // never pass an out-of-range priority to the syscall.
    let sched_priority = priority.clamp(1, 99);
    let param = libc::sched_param { sched_priority };
    // SAFETY: FFI. `&param` is a valid, initialized `sched_param` for the call;
    // pid 0 targets the calling thread.
    let rc = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) };
    if rc == 0 {
        RtStep::Applied
    } else {
        degrade("sched_setscheduler(SCHED_FIFO)")
    }
}

#[cfg(target_os = "linux")]
fn apply_affinity(cpu: usize) -> RtStep {
    // SAFETY: `cpu_set_t` is zero-initializable; CPU_ZERO/CPU_SET operate on it in
    // place; sched_setaffinity reads exactly `size_of::<cpu_set_t>()` bytes.
    unsafe {
        let mut set: libc::cpu_set_t = core::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        let rc = libc::sched_setaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &set);
        if rc == 0 {
            RtStep::Applied
        } else {
            degrade("sched_setaffinity")
        }
    }
}

/// Pre-fault `len` bytes at `ptr`: advise the kernel the region will be needed
/// (`MADV_WILLNEED`) and touch one byte of every page so the pages are resident
/// BEFORE the enforced loop's first access — paired with `mlockall(MCL_FUTURE)`
/// they then stay resident, so the loop can never take a major page fault.
///
/// # Safety
/// `ptr` must point to a valid mapping of at least `len` bytes that stays mapped
/// for the duration of the call (the caller — a live `PosixShmRegion` — guarantees
/// this; the mapping is `CANONICAL_IMAGE_LEN` bytes and outlives the handle).
#[cfg(target_os = "linux")]
pub unsafe fn prefault_region(ptr: *mut core::ffi::c_void, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    // Best-effort hint; ignore the result (touching below is what actually faults).
    let _ = libc::madvise(ptr, len, libc::MADV_WILLNEED);
    // Touch one byte per page. A volatile READ populates the page table entry
    // (the region is already zeroed by ftruncate, so no write is needed to make it
    // resident); `read_volatile` prevents the compiler from eliding the touch.
    let page = page_size();
    let base = ptr as *const u8;
    let mut off = 0usize;
    while off < len {
        let _ = core::ptr::read_volatile(base.add(off));
        off += page;
    }
    // The final byte, in case `len` is not a page multiple.
    let _ = core::ptr::read_volatile(base.add(len - 1));
}

#[cfg(target_os = "linux")]
fn page_size() -> usize {
    // SAFETY: FFI with a constant selector.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 {
        p as usize
    } else {
        4096
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// An all-off config is a pure no-op: every step `Skipped`, `fully_applied`.
    #[test]
    fn default_config_is_a_noop() {
        let status = configure_realtime(&RtConfig::default());
        assert_eq!(status.memory_lock, RtStep::Skipped);
        assert_eq!(status.scheduler, RtStep::Skipped);
        assert_eq!(status.affinity, RtStep::Skipped);
        assert!(
            status.fully_applied(),
            "a no-op config is trivially fully applied"
        );
    }

    /// The core fail-safe contract: requesting the full enforced-loop config NEVER
    /// panics, and each step is either applied or cleanly degraded — even
    /// unprivileged (CI is typically not `CAP_SYS_NICE`, so `SCHED_FIFO` degrades).
    /// This is the degrade-with-warning path the enforced-loop entry relies on.
    #[test]
    fn enforced_loop_config_is_fail_safe_unprivileged() {
        let status = configure_realtime(&RtConfig::enforced_loop(0));
        // Every field is a well-formed RtStep (Applied or Degraded — never a panic,
        // never Skipped since all three were requested).
        for step in [&status.memory_lock, &status.scheduler, &status.affinity] {
            assert!(
                matches!(step, RtStep::Applied | RtStep::Degraded(_)),
                "a requested step must be Applied or Degraded, got {step:?}"
            );
        }
        // A degraded SCHED_FIFO (the common unprivileged case) carries a reason.
        if let RtStep::Degraded(reason) = &status.scheduler {
            assert!(!reason.is_empty(), "a degrade must carry the OS reason");
        }
    }

    /// A refused priority is reported, not applied — but the OTHER steps are
    /// independent (an affinity/mlock success is not blocked by a FIFO refusal).
    #[test]
    fn steps_are_independent() {
        // Only affinity: FIFO is not requested, so it must be Skipped regardless of
        // privilege, proving one refusal cannot forge another step's outcome.
        let status = configure_realtime(&RtConfig {
            lock_memory: false,
            fifo_priority: None,
            cpu_affinity: Some(0),
        });
        assert_eq!(status.memory_lock, RtStep::Skipped);
        assert_eq!(status.scheduler, RtStep::Skipped);
        assert!(matches!(
            status.affinity,
            RtStep::Applied | RtStep::Degraded(_)
        ));
    }

    /// `prefault_region` touches every page of a live heap mapping without a fault
    /// or panic (a stand-in for the SHM region; the SHM path is covered by the
    /// `PosixShmRegion::prefault` test).
    #[test]
    fn prefault_touches_a_region_without_panic() {
        let mut buf = vec![0u8; 3 * page_size() + 17];
        // SAFETY: buf is a valid mapping of buf.len() bytes for the call.
        unsafe {
            prefault_region(buf.as_mut_ptr() as *mut core::ffi::c_void, buf.len());
        }
        // A null / zero-length call is a clean no-op.
        unsafe {
            prefault_region(core::ptr::null_mut(), 0);
            prefault_region(buf.as_mut_ptr() as *mut core::ffi::c_void, 0);
        }
    }
}
