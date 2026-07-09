//! # kirra-hv-carrier — the L3 last-mile cross-partition carrier (ADR-0030)
//!
//! Binds the carrier-agnostic [`ContractReader`] / [`ContractWriter`] seam
//! (`kirra-contract-channel`) to a **memory-mapped shared region**, so the same
//! frozen `GovernorContractView` + seqlock trust chain that runs host-in-process
//! over `InProcessRegion` runs across a **real** address-space boundary.
//!
//! This is **the only crate in the L3 path that is NOT `#![forbid(unsafe_code)]`**
//! — it is the ADR-0006 **Clause 3** integration boundary. The `unsafe` budget is
//! bounded and enumerated (ADR-0030 Clause A):
//!
//! 1. the **map** (`shm_open` + `mmap`, and `ftruncate` on create) producing a
//!    pointer to exactly `size_of::<GovernorContractView>()` bytes (R-HV-2);
//! 2. viewing those mapped bytes as [`AtomicView`] and doing **atomic** field
//!    accesses that reproduce `InProcessRegion`'s memory model **exactly** —
//!    `generation` Acquire/Release, body fields Relaxed; the generation counter
//!    fences the body. The trust chain's torn-read freedom holds across the
//!    boundary **only** because these orderings are preserved (ADR-0030 Clause A);
//! 3. the `Send`/`Sync` assertions (the backing is atomics over a stable mapping).
//!
//! No other `unsafe`. The `#[repr(C)]` layout the shim overlays is **byte-identical
//! to the frozen `GovernorContractView`** — pinned by the compile-time assertions
//! below against `kirra_contract_channel`'s own frozen offsets — so the mapped
//! region *is* the canonical contract image; the shim maps and atomically accesses
//! it, it never re-derives the layout.
//!
//! ## Host binding vs. target
//!
//! This crate ships the **host** binding, `PosixShmRegion` (`shm_open`/`mmap`,
//! `MAP_SHARED`) — the testable stand-in for the QNX `HvRegion` (a hypervisor-
//! mapped region), which binds the *same* traits with the same trust chain and
//! only swaps the map primitive (ADR-0030 Clause B). [`PosixShmRegion`] is the
//! read-write handle (guest); [`PosixShmReader`] is the **read-only** handle
//! (governor) — read-only is enforced both by the `PROT_READ` mapping and at the
//! type level (it has no [`ContractWriter`] impl), the R-HV-1 shape.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::ffi::CString;
use std::io;

use kirra_contract_channel::{
    ContractReader, ContractWriter, GovernorContractView, CANONICAL_IMAGE_LEN, MAX_COMMAND_BYTES,
};

/// The mapped region viewed as atomics. `#[repr(C)]`, field order + widths
/// **identical to [`GovernorContractView`]**, so the mapped bytes are the frozen
/// contract image and a peer that maps the same region interprets it identically.
/// Atomics have the same size/alignment as their underlying integers, so the
/// layout matches byte-for-byte (asserted below).
#[repr(C)]
struct AtomicView {
    layout_version: AtomicU32,
    magic: AtomicU32,
    generation: AtomicU64,
    sequence: AtomicU64,
    publication_nanos: AtomicU64,
    deadline_nanos: AtomicU64,
    crc32: AtomicU32,
    command_len: AtomicU32,
    command: [AtomicU8; MAX_COMMAND_BYTES],
}

// The mapped-region layout IS the frozen contract layout — assert every offset
// against GovernorContractView's own (themselves frozen in kirra-contract-channel).
use core::mem::{align_of, offset_of, size_of};
const _: () = assert!(
    offset_of!(AtomicView, layout_version) == offset_of!(GovernorContractView, layout_version)
);
const _: () = assert!(offset_of!(AtomicView, magic) == offset_of!(GovernorContractView, magic));
const _: () =
    assert!(offset_of!(AtomicView, generation) == offset_of!(GovernorContractView, generation));
const _: () =
    assert!(offset_of!(AtomicView, sequence) == offset_of!(GovernorContractView, sequence));
const _: () = assert!(
    offset_of!(AtomicView, publication_nanos)
        == offset_of!(GovernorContractView, publication_nanos)
);
const _: () = assert!(
    offset_of!(AtomicView, deadline_nanos) == offset_of!(GovernorContractView, deadline_nanos)
);
const _: () = assert!(offset_of!(AtomicView, crc32) == offset_of!(GovernorContractView, crc32));
const _: () =
    assert!(offset_of!(AtomicView, command_len) == offset_of!(GovernorContractView, command_len));
const _: () = assert!(offset_of!(AtomicView, command) == offset_of!(GovernorContractView, command));
const _: () = assert!(size_of::<AtomicView>() == size_of::<GovernorContractView>());
const _: () = assert!(size_of::<AtomicView>() == CANONICAL_IMAGE_LEN);
const _: () = assert!(align_of::<AtomicView>() == align_of::<GovernorContractView>());

/// Copy the region into an owned [`GovernorContractView`] — the reader side of
/// the seqlock. Body fields Relaxed; the caller's Acquire load of `generation`
/// (in `read_coherent_snapshot`) fences them. **Mirrors `InProcessRegion::copy_view`
/// exactly** — the two carriers MUST share the memory model.
fn read_view(v: &AtomicView) -> GovernorContractView {
    let mut command = [0u8; MAX_COMMAND_BYTES];
    for (i, slot) in command.iter_mut().enumerate() {
        *slot = v.command[i].load(Ordering::Relaxed);
    }
    GovernorContractView {
        layout_version: v.layout_version.load(Ordering::Relaxed),
        magic: v.magic.load(Ordering::Relaxed),
        generation: v.generation.load(Ordering::Relaxed),
        sequence: v.sequence.load(Ordering::Relaxed),
        publication_nanos: v.publication_nanos.load(Ordering::Relaxed),
        deadline_nanos: v.deadline_nanos.load(Ordering::Relaxed),
        crc32: v.crc32.load(Ordering::Relaxed),
        command_len: v.command_len.load(Ordering::Relaxed),
        command,
    }
}

/// Write every field EXCEPT `generation` (the `publish` driver owns the counter).
/// Relaxed; the Release store of `generation` publishes them. **Mirrors
/// `InProcessRegion::store_body` exactly.**
fn write_body(v: &AtomicView, view: &GovernorContractView) {
    v.layout_version
        .store(view.layout_version, Ordering::Relaxed);
    v.magic.store(view.magic, Ordering::Relaxed);
    v.sequence.store(view.sequence, Ordering::Relaxed);
    v.publication_nanos
        .store(view.publication_nanos, Ordering::Relaxed);
    v.deadline_nanos
        .store(view.deadline_nanos, Ordering::Relaxed);
    v.crc32.store(view.crc32, Ordering::Relaxed);
    v.command_len.store(view.command_len, Ordering::Relaxed);
    for (i, b) in view.command.iter().enumerate() {
        v.command[i].store(*b, Ordering::Relaxed);
    }
}

// --- the map primitive (the enumerated `unsafe`) --------------------------------

/// Build a NUL-terminated POSIX shared-memory name. Callers pass a leading-slash
/// name (e.g. `"/kirra-gov"`); an interior NUL is rejected.
fn shm_name(name: &str) -> io::Result<CString> {
    CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "shm name has interior NUL"))
}

/// `mmap` the named shared region and return the base pointer. `create` adds
/// `O_CREAT | O_EXCL` + `ftruncate` to `CANONICAL_IMAGE_LEN` (a fresh, zeroed
/// region). The fd is closed after mapping (the mapping persists).
///
/// SAFETY: pure FFI. On any failure we translate `errno` and (on create) unlink
/// the partially-created object. The returned pointer is page-aligned (satisfies
/// `AtomicView`'s 8-byte alignment) and spans exactly `CANONICAL_IMAGE_LEN` bytes.
fn map_region(name: &CString, prot: libc::c_int, create: bool) -> io::Result<*mut AtomicView> {
    let oflag = if create {
        libc::O_CREAT | libc::O_EXCL | libc::O_RDWR
    } else if prot & libc::PROT_WRITE != 0 {
        libc::O_RDWR
    } else {
        libc::O_RDONLY
    };
    // SAFETY: FFI. name is a valid NUL-terminated C string for the call's duration.
    let fd = unsafe { libc::shm_open(name.as_ptr(), oflag, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    if !create {
        // Fail-closed size guard: mapping CANONICAL_IMAGE_LEN over a stale/smaller
        // object of the same name would map fine and then SIGBUS on first access
        // (a crash, not a reject). Verify the object is large enough BEFORE mmap.
        // SAFETY: fd is a valid descriptor; fstat writes `st`.
        let mut st: libc::stat = unsafe { core::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        if (st.st_size as u64) < CANONICAL_IMAGE_LEN as u64 {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shm region smaller than the frozen contract layout",
            ));
        }
    }
    if create {
        // SAFETY: fd is a valid open descriptor from shm_open above.
        if unsafe { libc::ftruncate(fd, CANONICAL_IMAGE_LEN as libc::off_t) } < 0 {
            let e = io::Error::last_os_error();
            // SAFETY: fd valid; name valid. Best-effort cleanup of the new object.
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
            return Err(e);
        }
    }
    // SAFETY: fd valid; length is the fixed region size; MAP_SHARED so writes are
    // visible to every peer mapping the same object.
    let addr = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            CANONICAL_IMAGE_LEN,
            prot,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    // Capture the mmap errno BEFORE close() can overwrite it.
    let map_err = if addr == libc::MAP_FAILED {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: fd valid; the mapping outlives the fd, so closing it now is correct.
    unsafe { libc::close(fd) };
    if let Some(e) = map_err {
        if create {
            // SAFETY: name valid; drop the object we created but couldn't map.
            unsafe { libc::shm_unlink(name.as_ptr()) };
        }
        return Err(e);
    }
    Ok(addr as *mut AtomicView)
}

/// The read-write carrier handle (the **guest** side): maps the region
/// `PROT_READ | PROT_WRITE` and implements both [`ContractReader`] and
/// [`ContractWriter`]. The creator (`create`) owns the object and `shm_unlink`s
/// it on drop; a `open`ed handle does not.
pub struct PosixShmRegion {
    ptr: *mut AtomicView,
    name: CString,
    is_creator: bool,
}

// SAFETY: the only shared state is the `AtomicView` over the mapping, which is
// `Sync` (all fields are atomics); the pointer is stable for the handle's life
// and the mapping is `MAP_SHARED`. Moving/​sharing the handle across threads only
// ever performs atomic accesses. Same rationale for `PosixShmReader`.
unsafe impl Send for PosixShmRegion {}
unsafe impl Sync for PosixShmRegion {}

impl PosixShmRegion {
    /// Create a fresh, zeroed region (fails if `name` already exists — `O_EXCL`).
    /// The returned handle owns the object (unlinked on drop).
    pub fn create(name: &str) -> io::Result<Self> {
        let name = shm_name(name)?;
        let ptr = map_region(&name, libc::PROT_READ | libc::PROT_WRITE, true)?;
        Ok(Self {
            ptr,
            name,
            is_creator: true,
        })
    }

    /// Open an existing region read-write (a second guest-side mapping). Does not
    /// own the object.
    pub fn open(name: &str) -> io::Result<Self> {
        let name = shm_name(name)?;
        let ptr = map_region(&name, libc::PROT_READ | libc::PROT_WRITE, false)?;
        Ok(Self {
            ptr,
            name,
            is_creator: false,
        })
    }

    /// SAFETY: `ptr` is a valid, aligned mapping of `size_of::<AtomicView>()`
    /// bytes for the handle's lifetime, so the shared reference is sound.
    fn view(&self) -> &AtomicView {
        unsafe { &*self.ptr }
    }
}

impl ContractReader for PosixShmRegion {
    fn load_generation(&self) -> u64 {
        self.view().generation.load(Ordering::Acquire)
    }
    fn copy_view(&self) -> GovernorContractView {
        read_view(self.view())
    }
}

impl ContractWriter for PosixShmRegion {
    fn store_generation(&self, generation: u64) {
        self.view().generation.store(generation, Ordering::Release);
    }
    fn store_body(&self, view: &GovernorContractView) {
        write_body(self.view(), view);
    }
}

impl Drop for PosixShmRegion {
    fn drop(&mut self) {
        // SAFETY: ptr is our mapping of CANONICAL_IMAGE_LEN bytes; name is valid.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, CANONICAL_IMAGE_LEN);
            if self.is_creator {
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }
}

/// The **read-only** carrier handle (the **governor** side, R-HV-1): maps the
/// region `PROT_READ` only and implements [`ContractReader`] **only** — it has no
/// [`ContractWriter`] impl, so a governor cannot write the region at the type
/// level, and the `PROT_READ` mapping enforces it at the OS level too.
pub struct PosixShmReader {
    ptr: *mut AtomicView,
}

// SAFETY: see `PosixShmRegion` — atomics over a stable `MAP_SHARED` mapping.
unsafe impl Send for PosixShmReader {}
unsafe impl Sync for PosixShmReader {}

impl PosixShmReader {
    /// Map an existing region read-only. The creator must already have created it.
    pub fn open(name: &str) -> io::Result<Self> {
        let name = shm_name(name)?;
        let ptr = map_region(&name, libc::PROT_READ, false)?;
        Ok(Self { ptr })
    }

    /// SAFETY: as `PosixShmRegion::view`.
    fn view(&self) -> &AtomicView {
        unsafe { &*self.ptr }
    }
}

impl ContractReader for PosixShmReader {
    fn load_generation(&self) -> u64 {
        self.view().generation.load(Ordering::Acquire)
    }
    fn copy_view(&self) -> GovernorContractView {
        read_view(self.view())
    }
}

impl Drop for PosixShmReader {
    fn drop(&mut self) {
        // SAFETY: our own read-only mapping; readers never unlink the object.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, CANONICAL_IMAGE_LEN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_contract_channel::{
        publish, read_coherent_snapshot, validate, AcceptedWatermark, VehicleCommandPayload,
        MAX_SNAPSHOT_RETRIES,
    };
    use std::sync::atomic::AtomicU32 as Counter;

    // Unique shm name per test (process id + a counter) so parallel tests and
    // reruns don't collide on a leftover object.
    static SEQ: Counter = Counter::new(0);
    fn unique_name(tag: &str) -> String {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        format!("/kirra-hv-{}-{}-{}", tag, std::process::id(), n)
    }

    fn demo(seq: u64) -> VehicleCommandPayload {
        VehicleCommandPayload {
            linear_velocity_mps: 3.0 + seq as f64,
            current_velocity_mps: 2.5,
            delta_time_s: 0.1,
            steering_angle_deg: -4.0,
            current_steering_angle_deg: -3.5,
        }
    }

    #[test]
    fn two_rw_mappings_of_one_region_round_trip_a_command() {
        let name = unique_name("rw");
        let writer = PosixShmRegion::create(&name).expect("create");
        let reader = PosixShmRegion::open(&name).expect("open"); // a SECOND mmap

        let payload = demo(1);
        let body = payload.to_view(0, 1, 0, u64::MAX / 2);
        let committed = publish(&writer, 0, &body);
        assert_eq!(committed, 2);

        // Read through the independent mapping — real shared pages.
        let snap = read_coherent_snapshot(&reader, MAX_SNAPSHOT_RETRIES).unwrap();
        let mut wm = AcceptedWatermark::new();
        validate(&snap, 0, &wm).expect("valid");
        wm.record(&snap);
        assert_eq!(
            VehicleCommandPayload::from_validated_view(&snap),
            Ok(payload)
        );
    }

    #[test]
    fn read_only_mapping_sees_what_the_guest_published() {
        let name = unique_name("ro");
        let guest = PosixShmRegion::create(&name).expect("create");
        let governor = PosixShmReader::open(&name).expect("open ro"); // governor, R-HV-1

        let payload = demo(7);
        let body = payload.to_view(0, 42, 0, u64::MAX / 2);
        publish(&guest, 0, &body);

        let snap = read_coherent_snapshot(&governor, MAX_SNAPSHOT_RETRIES).unwrap();
        let wm = AcceptedWatermark::new();
        validate(&snap, 0, &wm).expect("valid");
        assert_eq!(snap.sequence, 42);
        assert_eq!(
            VehicleCommandPayload::from_validated_view(&snap),
            Ok(payload)
        );
    }

    #[test]
    fn a_monotonic_stream_is_accepted_in_order_over_shared_memory() {
        let name = unique_name("stream");
        let guest = PosixShmRegion::create(&name).expect("create");
        let governor = PosixShmReader::open(&name).expect("open ro");
        let mut wm = AcceptedWatermark::new();

        let mut committed = 0u64;
        for seq in 1..=4u64 {
            let body = demo(seq).to_view(committed, seq, 0, u64::MAX / 2);
            committed = publish(&guest, committed, &body);
            let snap = read_coherent_snapshot(&governor, MAX_SNAPSHOT_RETRIES).unwrap();
            validate(&snap, 0, &wm).expect("in order");
            wm.record(&snap);
            assert_eq!(snap.sequence, seq);
            assert_eq!(
                VehicleCommandPayload::from_validated_view(&snap),
                Ok(demo(seq))
            );
        }
        assert_eq!(wm.last(), Some((8, 4)));
    }

    #[test]
    fn open_rejects_a_too_small_region_fail_closed() {
        // A stale/malformed object one byte too small must be REFUSED before mmap
        // (else CANONICAL_IMAGE_LEN accesses would SIGBUS). Create it raw.
        let name = unique_name("small");
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: FFI; fd is checked and closed.
        unsafe {
            let fd = libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            );
            assert!(fd >= 0, "raw shm_open");
            assert_eq!(
                libc::ftruncate(fd, (CANONICAL_IMAGE_LEN - 1) as libc::off_t),
                0,
                "ftruncate too-small"
            );
            libc::close(fd);
        }
        // Both handles fail closed rather than map a SIGBUS-prone region.
        assert!(
            PosixShmRegion::open(&name).is_err(),
            "RW open must reject too-small"
        );
        assert!(
            PosixShmReader::open(&name).is_err(),
            "RO open must reject too-small"
        );
        // SAFETY: cleanup the raw object.
        unsafe { libc::shm_unlink(cname.as_ptr()) };
    }

    #[test]
    fn creator_unlinks_on_drop() {
        let name = unique_name("unlink");
        {
            let _guest = PosixShmRegion::create(&name).expect("create");
        } // dropped → shm_unlink
          // A fresh create with the same name must succeed (O_EXCL) → proves unlink.
        let _again = PosixShmRegion::create(&name).expect("recreate after unlink");
    }
}
