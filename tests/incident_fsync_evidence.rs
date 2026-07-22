//! #792 (roadmap) — EVIDENCE-GRADE fsync verification for the incident-class
//! durable posture write, via a COUNTING VFS shim.
//!
//! The WS-0.3 durability claim rests on "the `synchronous=FULL` connection's
//! COMMIT itself fsyncs the WAL". Until now that was asserted from SQLite's
//! documented semantics plus the SIGKILL/WAL-drop crash drills
//! (`audit_chain_prefix_on_kill.rs`) — behavioral evidence, but never a
//! direct observation of the sync call. This test registers a wrapper VFS
//! around SQLite's default that COUNTS `xSync` invocations per file kind and
//! asserts, on the real `VerifierStore`:
//!
//!   1. the INCIDENT-CLASS durable write (`save_posture_event_chained_
//!      with_generation_durable`) performs at least one `xSync` on the
//!      `-wal` file — the fsync IS there, observed, not inferred; and
//!   2. the 20 Hz-class NORMAL write performs NO WAL `xSync` — the negative
//!      control that keeps (1) non-vacuous and pins the INV-12 throughput
//!      contract (NORMAL commits are checkpoint-bounded, not per-commit
//!      fsynced).
//!
//! This doubles as evidence for the #74 epoch/nonce durability claims — they
//! ride the same `synchronous=FULL` durable connection.
//!
//! ## Safety of the `unsafe` here
//!
//! This file contains the repo's only VFS-level unsafe code, and it is
//! TEST-ONLY (never compiled into any shipped artifact). The shim follows
//! SQLite's documented shim pattern (copy the default `sqlite3_vfs`, replace
//! `xOpen`, grow `szOsFile` by a fixed header; give each opened file an
//! io-methods table that delegates every call to the real methods stored
//! behind the header). All pointers dereferenced are either provided by
//! SQLite for exactly this purpose (`sqlite3_file` buffers of our declared
//! `szOsFile`) or captured from `sqlite3_vfs_find` at registration and never
//! freed (registered VFS structs are required to live forever).
//!
//! Registration makes the shim the process-wide DEFAULT VFS, so this file
//! holds a SINGLE #[test] (its own process under the integration-test
//! harness) — everything it opens routes through the shim.

#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use rusqlite::ffi;

/// xSync calls observed on `-wal` files / on all files.
static WAL_SYNCS: AtomicU64 = AtomicU64::new(0);
static TOTAL_SYNCS: AtomicU64 = AtomicU64::new(0);

/// The real (default) VFS captured at registration.
static REAL_VFS: OnceLock<usize> = OnceLock::new();

fn real_vfs() -> *mut ffi::sqlite3_vfs {
    *REAL_VFS.get().expect("shim registered") as *mut ffi::sqlite3_vfs
}

/// Header our xOpen prepends to every file object. The REAL `sqlite3_file`
/// (of the real VFS's `szOsFile`) lives immediately after this header inside
/// the buffer SQLite hands us (we declared `szOsFile = header + real`).
/// `repr(C)` with a trailing u64 keeps the real file 8-aligned.
#[repr(C)]
struct ShimFile {
    base: ffi::sqlite3_file,
    /// 1 when the opened path ends in "-wal" (the write-ahead log).
    is_wal: u64,
}

const HEADER: usize = std::mem::size_of::<ShimFile>();

unsafe fn shim_of(file: *mut ffi::sqlite3_file) -> *mut ShimFile {
    file.cast::<ShimFile>()
}

unsafe fn real_of(file: *mut ffi::sqlite3_file) -> *mut ffi::sqlite3_file {
    file.cast::<u8>().add(HEADER).cast::<ffi::sqlite3_file>()
}

/// Delegate a real io-method by name; the real methods pointer lives on the
/// embedded real file object.
macro_rules! delegate {
    ($file:expr, $method:ident $(, $arg:expr)*) => {{
        let real = real_of($file);
        ((*(*real).pMethods).$method.expect(concat!("real ", stringify!($method))))(real $(, $arg)*)
    }};
}

unsafe extern "C" fn x_close(f: *mut ffi::sqlite3_file) -> c_int {
    delegate!(f, xClose)
}
unsafe extern "C" fn x_read(
    f: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    n: c_int,
    off: i64,
) -> c_int {
    delegate!(f, xRead, buf, n, off)
}
unsafe extern "C" fn x_write(
    f: *mut ffi::sqlite3_file,
    buf: *const c_void,
    n: c_int,
    off: i64,
) -> c_int {
    delegate!(f, xWrite, buf, n, off)
}
unsafe extern "C" fn x_truncate(f: *mut ffi::sqlite3_file, size: i64) -> c_int {
    delegate!(f, xTruncate, size)
}
unsafe extern "C" fn x_sync(f: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    // THE observation point: count, attribute to the WAL when applicable,
    // then delegate to the real fsync.
    TOTAL_SYNCS.fetch_add(1, Ordering::SeqCst);
    if (*shim_of(f)).is_wal == 1 {
        WAL_SYNCS.fetch_add(1, Ordering::SeqCst);
    }
    delegate!(f, xSync, flags)
}
unsafe extern "C" fn x_file_size(f: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    delegate!(f, xFileSize, out)
}
unsafe extern "C" fn x_lock(f: *mut ffi::sqlite3_file, l: c_int) -> c_int {
    delegate!(f, xLock, l)
}
unsafe extern "C" fn x_unlock(f: *mut ffi::sqlite3_file, l: c_int) -> c_int {
    delegate!(f, xUnlock, l)
}
unsafe extern "C" fn x_check_reserved_lock(f: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    delegate!(f, xCheckReservedLock, out)
}
unsafe extern "C" fn x_file_control(
    f: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    delegate!(f, xFileControl, op, arg)
}
unsafe extern "C" fn x_sector_size(f: *mut ffi::sqlite3_file) -> c_int {
    delegate!(f, xSectorSize)
}
unsafe extern "C" fn x_device_characteristics(f: *mut ffi::sqlite3_file) -> c_int {
    delegate!(f, xDeviceCharacteristics)
}
unsafe extern "C" fn x_shm_map(
    f: *mut ffi::sqlite3_file,
    pg: c_int,
    pgsz: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    delegate!(f, xShmMap, pg, pgsz, extend, out)
}
unsafe extern "C" fn x_shm_lock(
    f: *mut ffi::sqlite3_file,
    off: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    delegate!(f, xShmLock, off, n, flags)
}
unsafe extern "C" fn x_shm_barrier(f: *mut ffi::sqlite3_file) {
    let real = real_of(f);
    if let Some(m) = (*(*real).pMethods).xShmBarrier {
        m(real);
    }
}
unsafe extern "C" fn x_shm_unmap(f: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    delegate!(f, xShmUnmap, delete)
}

/// iVersion 2: everything WAL needs (the shm methods live here). We
/// deliberately do NOT advertise v3 (xFetch/xUnfetch mmap fast-path) — SQLite
/// then simply reads through xRead, which is a perf nuance, never a
/// correctness one, and keeps the shim smaller.
static SHIM_IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 2,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: Some(x_shm_map),
    xShmLock: Some(x_shm_lock),
    xShmBarrier: Some(x_shm_barrier),
    xShmUnmap: Some(x_shm_unmap),
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn shim_open(
    _vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let shim = shim_of(file);
    (*shim).is_wal = 0;
    if !z_name.is_null() {
        let name = std::ffi::CStr::from_ptr(z_name).to_string_lossy();
        if name.ends_with("-wal") {
            (*shim).is_wal = 1;
        }
    }
    let real = real_vfs();
    let real_file = real_of(file);
    let rc = ((*real).xOpen.expect("real xOpen"))(real, z_name, real_file, flags, out_flags);
    if rc == ffi::SQLITE_OK && !(*real_file).pMethods.is_null() {
        (*shim).base.pMethods = &SHIM_IO_METHODS;
    } else {
        // Failed open: leave the outer file method-less so SQLite won't
        // call xClose on it (its contract for xOpen failures).
        (*shim).base.pMethods = std::ptr::null();
    }
    rc
}

/// Register the counting shim as the process-default VFS (idempotent).
fn register_counting_vfs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let real = ffi::sqlite3_vfs_find(std::ptr::null());
        assert!(!real.is_null(), "default VFS present");
        REAL_VFS.set(real as usize).expect("set once");
        // Copy the default VFS wholesale (pAppData and the non-open method
        // pointers keep working on a byte-identical struct), then override
        // identity, xOpen, and the grown per-file buffer size.
        let mut shim: ffi::sqlite3_vfs = *real;
        shim.zName = c"kirra-fsync-counting".as_ptr();
        shim.xOpen = Some(shim_open);
        shim.szOsFile = (*real).szOsFile + HEADER as c_int;
        let shim: &'static mut ffi::sqlite3_vfs = Box::leak(Box::new(shim));
        let rc = ffi::sqlite3_vfs_register(shim, 1 /* make default */);
        assert_eq!(rc, ffi::SQLITE_OK, "register counting VFS");
    });
}

/// #792 — the durable incident write is OBSERVED to fsync the WAL; the
/// NORMAL-class write is observed NOT to (the negative control).
#[test]
fn incident_durable_write_syncs_the_wal_and_the_normal_write_does_not() {
    register_counting_vfs();

    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("fsync_evidence.sqlite");
    let mut store = kirra_persistence::VerifierStore::new(path.to_str().unwrap()).expect("store");
    // The shim is live: store creation (DDL, WAL setup) must have routed
    // through it. If this is 0 the whole test would be vacuous — fail loudly.
    assert!(
        TOTAL_SYNCS.load(Ordering::SeqCst) > 0,
        "the counting VFS must be on the path (store creation syncs at least once)"
    );

    // --- Negative control: the 20 Hz-class NORMAL write ------------------
    let wal_before_normal = WAL_SYNCS.load(Ordering::SeqCst);
    store
        .save_posture_event_chained_with_generation(
            "posture_engine",
            "POSTURE_CACHE_REFRESHED",
            "{\"posture\":\"Nominal\"}",
            Some("fsync evidence drill — NORMAL class"),
            1_000,
            1,
            1,
        )
        .expect("normal write");
    let wal_after_normal = WAL_SYNCS.load(Ordering::SeqCst);
    assert_eq!(
        wal_after_normal, wal_before_normal,
        "a NORMAL (synchronous=NORMAL) commit must NOT fsync the WAL — \
         checkpoint-bounded durability is the INV-12 throughput contract"
    );

    // --- The claim: the INCIDENT-CLASS durable write fsyncs the WAL ------
    let wal_before_durable = WAL_SYNCS.load(Ordering::SeqCst);
    store
        .save_posture_event_chained_with_generation_durable(
            "posture_engine",
            "SYSTEM_POSTURE_TRANSITION",
            "{\"posture\":\"Degraded\"}",
            Some("fsync evidence drill — INCIDENT class"),
            2_000,
            2,
            1,
        )
        .expect("durable write");
    let wal_after_durable = WAL_SYNCS.load(Ordering::SeqCst);
    assert!(
        wal_after_durable > wal_before_durable,
        "the synchronous=FULL incident write must perform an OBSERVED xSync \
         on the -wal file (got {wal_before_durable} -> {wal_after_durable}); \
         the WS-0.3 durability claim is measured here, not inferred"
    );

    println!(
        "fsync evidence: normal write WAL syncs +0; durable write WAL syncs +{}; total syncs {}",
        wal_after_durable - wal_before_durable,
        TOTAL_SYNCS.load(Ordering::SeqCst)
    );
}
