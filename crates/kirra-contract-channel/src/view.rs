//! The frozen `#[repr(C)]` cross-partition layout (HVCHAN-001 §2).
//!
//! **Layout stability IS the safety claim.** The compile-time assertions at the
//! bottom of this file pin every field offset, the total size, and the alignment;
//! they fail the build if the layout drifts. Any intended change is a NEW
//! [`LAYOUT_VERSION`] with re-validation, never an in-place edit (HVCHAN-001 §2.4).

use crate::crc::crc32_ieee;

/// Command-payload capacity, fixed at the layout version (HVCHAN-001 §2.2,
/// design-intent 64–128 B). Growing it is a new [`LAYOUT_VERSION`], never an
/// in-place edit. The command rides **by value** (a fixed array): pointers do
/// not cross a partition boundary (HVCHAN-001 §2.1).
pub const MAX_COMMAND_BYTES: usize = 128;

/// Layout version held at **offset 0** so any reader can locate it regardless of
/// how later fields evolve (a versioned-prefix discipline, like a file magic). A
/// reader that does not recognize the version **rejects** — it never best-effort
/// parses a layout it was not built and certified against (HVCHAN-001 §2.2, §4).
pub const LAYOUT_VERSION: u32 = 1;

/// Fixed channel sentinel (ASCII `"KGCV"` — Kirra Governor Contract View). A
/// wrong sentinel is a gross-corruption / wrong-region signal ⇒ reject.
pub const MAGIC: u32 = 0x4B47_4356;

/// Length of [`GovernorContractView::canonical_image`] — the little-endian wire
/// image the release-token digest is computed over. Equals
/// `size_of::<GovernorContractView>()` because the layout has **no padding holes**
/// (the widest-first design); see the assertions below.
pub const CANONICAL_IMAGE_LEN: usize = 48 + MAX_COMMAND_BYTES;

/// Cross-partition governor contract view — **FROZEN once certified** (HVCHAN-001
/// §2.4). `#[repr(C)]`, fixed size, pointer-free; mapped **read-only** into the
/// governor partition. All multi-byte integers are little-endian on the wire.
///
/// Every field's writer is the **guest publisher** and its checker is the
/// **governor**; no field is trusted because the guest set it (HVCHAN-001 §2.3).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GovernorContractView {
    // --- version-discovery prefix (STABLE across ALL layout versions) ---
    /// Layout version (offset 0). Checked FIRST, before any other field is
    /// interpreted — a mismatch ⇒ reject.
    pub layout_version: u32,
    /// Channel sentinel. Pairs with `layout_version` to fill 8 bytes (no hole).
    pub magic: u32,

    // --- widest-first body (natural alignment; NOTHING is packed) ---
    /// Monotonic write generation; the seqlock counter (odd while writing, even
    /// on commit). Read before and after the snapshot copy.
    pub generation: u64,
    /// Monotonic command sequence — `sequence <= last_accepted ⇒ reject`.
    pub sequence: u64,
    /// Publication timestamp (nanoseconds, **boundary clock domain** — §5).
    pub publication_nanos: u64,
    /// Absolute deadline (nanoseconds, same domain) — `now > deadline ⇒ reject`.
    pub deadline_nanos: u64,

    /// CRC-32 (IEEE) over `command[..command_len]`. Checked after the snapshot is
    /// stable, before the judge.
    pub crc32: u32,
    /// Valid command length in bytes; `> MAX_COMMAND_BYTES ⇒ reject`. Pairs with
    /// `crc32` to 8 bytes.
    pub command_len: u32,

    /// The command payload, BY VALUE. The command occupies the first
    /// `command_len` bytes; the remainder is unspecified and never read.
    pub command: [u8; MAX_COMMAND_BYTES],
}

impl GovernorContractView {
    /// Build a committed view carrying `command`, with the fixed
    /// [`LAYOUT_VERSION`]/[`MAGIC`] and a freshly computed [`crc32`]. Returns
    /// `None` if the command exceeds [`MAX_COMMAND_BYTES`] (the publisher cannot
    /// fabricate an oversize frame). The `generation` is the caller's committed
    /// (even) value; [`publish`](crate::publish) drives the odd/even transitions.
    pub fn new_command(
        generation: u64,
        sequence: u64,
        publication_nanos: u64,
        deadline_nanos: u64,
        command: &[u8],
    ) -> Option<Self> {
        if command.len() > MAX_COMMAND_BYTES {
            return None;
        }
        let mut buf = [0u8; MAX_COMMAND_BYTES];
        buf[..command.len()].copy_from_slice(command);
        Some(Self {
            layout_version: LAYOUT_VERSION,
            magic: MAGIC,
            generation,
            sequence,
            publication_nanos,
            deadline_nanos,
            crc32: crc32_ieee(command),
            command_len: command.len() as u32,
            command: buf,
        })
    }

    /// The valid command bytes, `command[..command_len]`, or `None` if
    /// `command_len` is out of range. Only call after [`validate`](crate::validate)
    /// has passed — this is a convenience accessor, not a validation step.
    pub fn validated_command(&self) -> Option<&[u8]> {
        let len = self.command_len as usize;
        if len > MAX_COMMAND_BYTES {
            return None;
        }
        Some(&self.command[..len])
    }

    /// The canonical **little-endian** byte image (length [`CANONICAL_IMAGE_LEN`]).
    /// This is the exact, endian-explicit, padding-free serialization the
    /// release-token digest is signed over (HVCHAN-001 §3 step 5). On a
    /// little-endian target it is byte-identical to the `#[repr(C)]` image; the
    /// manual serialization keeps it deterministic and endian-independent without
    /// any `unsafe` byte reinterpretation.
    pub fn canonical_image(&self) -> [u8; CANONICAL_IMAGE_LEN] {
        let mut out = [0u8; CANONICAL_IMAGE_LEN];
        out[0..4].copy_from_slice(&self.layout_version.to_le_bytes());
        out[4..8].copy_from_slice(&self.magic.to_le_bytes());
        out[8..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..24].copy_from_slice(&self.sequence.to_le_bytes());
        out[24..32].copy_from_slice(&self.publication_nanos.to_le_bytes());
        out[32..40].copy_from_slice(&self.deadline_nanos.to_le_bytes());
        out[40..44].copy_from_slice(&self.crc32.to_le_bytes());
        out[44..48].copy_from_slice(&self.command_len.to_le_bytes());
        out[48..48 + MAX_COMMAND_BYTES].copy_from_slice(&self.command);
        out
    }

    /// Parse a canonical little-endian image back into a view. Returns `None` if
    /// the slice is shorter than [`CANONICAL_IMAGE_LEN`]. The inverse of
    /// [`canonical_image`](Self::canonical_image); it performs **no validation**
    /// (that is [`validate`](crate::validate)'s job).
    pub fn from_canonical_image(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CANONICAL_IMAGE_LEN {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let u64_at = |o: usize| {
            u64::from_le_bytes([
                bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3], bytes[o + 4], bytes[o + 5],
                bytes[o + 6], bytes[o + 7],
            ])
        };
        let mut command = [0u8; MAX_COMMAND_BYTES];
        command.copy_from_slice(&bytes[48..48 + MAX_COMMAND_BYTES]);
        Some(Self {
            layout_version: u32_at(0),
            magic: u32_at(4),
            generation: u64_at(8),
            sequence: u64_at(16),
            publication_nanos: u64_at(24),
            deadline_nanos: u64_at(32),
            crc32: u32_at(40),
            command_len: u32_at(44),
            command,
        })
    }
}

// --------------------------------------------------------------------------
// FREEZE ASSERTIONS — layout stability IS the safety claim (HVCHAN-001 §2.4).
// A change to any field, size, order, or MAX_COMMAND_BYTES breaks the build
// here; that is the point. Fix it by minting a new LAYOUT_VERSION, not by
// editing these numbers to match a drifted struct.
// --------------------------------------------------------------------------
use core::mem::{align_of, offset_of, size_of};

const _: () = assert!(offset_of!(GovernorContractView, layout_version) == 0);
const _: () = assert!(offset_of!(GovernorContractView, magic) == 4);
const _: () = assert!(offset_of!(GovernorContractView, generation) == 8);
const _: () = assert!(offset_of!(GovernorContractView, sequence) == 16);
const _: () = assert!(offset_of!(GovernorContractView, publication_nanos) == 24);
const _: () = assert!(offset_of!(GovernorContractView, deadline_nanos) == 32);
const _: () = assert!(offset_of!(GovernorContractView, crc32) == 40);
const _: () = assert!(offset_of!(GovernorContractView, command_len) == 44);
const _: () = assert!(offset_of!(GovernorContractView, command) == 48);
// Total size has NO interior or trailing padding (widest-first, u32+u32 pairs).
const _: () = assert!(size_of::<GovernorContractView>() == CANONICAL_IMAGE_LEN);
const _: () = assert!(size_of::<GovernorContractView>() == 176);
const _: () = assert!(align_of::<GovernorContractView>() == 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_image_roundtrips() {
        let v = GovernorContractView::new_command(4, 7, 1_000, 2_000, b"steer").unwrap();
        let img = v.canonical_image();
        assert_eq!(img.len(), CANONICAL_IMAGE_LEN);
        assert_eq!(GovernorContractView::from_canonical_image(&img), Some(v));
    }

    #[test]
    fn canonical_image_is_little_endian_at_fixed_offsets() {
        let v = GovernorContractView::new_command(0x0807_0605, 0x0102, 0, 0, b"").unwrap();
        let img = v.canonical_image();
        // layout_version @0, magic @4, generation @8 — all LE.
        assert_eq!(&img[0..4], &LAYOUT_VERSION.to_le_bytes());
        assert_eq!(&img[4..8], &MAGIC.to_le_bytes());
        assert_eq!(&img[8..16], &0x0807_0605u64.to_le_bytes());
        assert_eq!(&img[16..24], &0x0102u64.to_le_bytes());
    }

    #[test]
    fn new_command_rejects_oversize_payload() {
        let too_long = [0u8; MAX_COMMAND_BYTES + 1];
        assert_eq!(
            GovernorContractView::new_command(0, 0, 0, 0, &too_long),
            None
        );
    }

    #[test]
    fn new_command_sets_version_magic_crc_and_len() {
        let v = GovernorContractView::new_command(2, 3, 0, 0, b"abc").unwrap();
        assert_eq!(v.layout_version, LAYOUT_VERSION);
        assert_eq!(v.magic, MAGIC);
        assert_eq!(v.command_len, 3);
        assert_eq!(v.crc32, crc32_ieee(b"abc"));
        assert_eq!(v.validated_command(), Some(&b"abc"[..]));
    }

    #[test]
    fn from_canonical_image_rejects_short_slice() {
        let short = [0u8; CANONICAL_IMAGE_LEN - 1];
        assert_eq!(GovernorContractView::from_canonical_image(&short), None);
    }

    #[test]
    fn validated_command_rejects_out_of_range_len() {
        let mut v = GovernorContractView::new_command(0, 0, 0, 0, b"x").unwrap();
        v.command_len = (MAX_COMMAND_BYTES + 1) as u32;
        assert_eq!(v.validated_command(), None);
    }
}
