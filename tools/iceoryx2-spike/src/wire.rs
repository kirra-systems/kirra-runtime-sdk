// wire.rs — the on-the-wire command frame + the subscriber-EDGE validations.
//
// The frame is a fixed-size `#[repr(C)]` POD so it rides iceoryx2's zero-copy
// publish/subscribe directly (no serialization on the hot path). The subscriber
// edge keeps two defense-in-depth checks BEFORE the judge runs: a bounds /
// oversize check and a payload CRC verify (harness design carried over from the
// QNX FFI harness). The judge (see judge.rs) runs only on a frame that has
// passed these.

use iceoryx2::prelude::ZeroCopySend;

/// Maximum valid application payload length (bytes) inside the fixed frame.
/// A `declared_len` above this is the OVERSIZE fault — rejected at the edge.
pub const MAX_PAYLOAD_LEN: usize = 32;

/// Fixed payload capacity carried by every frame. The iceoryx2 message size is
/// therefore constant (`size_of::<CommandFrame>()`); "oversize" is modelled as a
/// *declared* length beyond `MAX_PAYLOAD_LEN` (an application-level bound), since
/// a fixed type cannot transport a larger-than-capacity buffer. A variable-length
/// `[u8]` service would additionally check the received slice length; noted in
/// the README.
pub const PAYLOAD_CAPACITY: usize = 64;

/// The expected header magic. A frame whose magic differs is rejected
/// (BadMagic / StaleHeader class).
pub const FRAME_MAGIC: u32 = 0x4B49_5252; // "KIRR"

/// One command frame. `#[repr(C)]`, all-POD, naturally aligned — nothing is
/// packed; the layout is stable for zero-copy transfer.
#[repr(C)]
#[derive(Clone, Copy, Debug, ZeroCopySend)]
pub struct CommandFrame {
    /// Header magic — must equal [`FRAME_MAGIC`].
    pub magic: u32,
    /// Monotonic command sequence number. The judge rejects `seq <= last`
    /// (equal = replay, lower = regress); strictly-newer passes.
    pub sequence: u64,
    /// Absolute deadline (nanoseconds, monotonic domain) by which the command
    /// must be consumed. The judge rejects `now > deadline`.
    pub deadline_nanos: u64,
    /// Upstream integrity assertion. `1` = integrity asserted; anything else is
    /// rejected (IntegrityFlag class).
    pub integrity_flag: u8,
    /// Number of valid payload bytes. `> MAX_PAYLOAD_LEN` ⇒ OVERSIZE.
    pub declared_len: u16,
    /// CRC-32 (IEEE) over `payload[..declared_len]`. Verified at the edge.
    pub crc32: u32,
    /// Fixed-capacity payload buffer; the command occupies the first
    /// `declared_len` bytes (here: two little-endian `f64`s — linear m/s,
    /// angular rad/s).
    pub payload: [u8; PAYLOAD_CAPACITY],
}

impl CommandFrame {
    /// Build a well-formed frame carrying `(linear_mps, angular_radps)`, with a
    /// correct CRC and `declared_len = 16`. Callers mutate fields afterward to
    /// inject faults.
    pub fn well_formed(
        sequence: u64,
        deadline_nanos: u64,
        linear_mps: f64,
        angular_radps: f64,
    ) -> Self {
        let mut payload = [0u8; PAYLOAD_CAPACITY];
        payload[0..8].copy_from_slice(&linear_mps.to_le_bytes());
        payload[8..16].copy_from_slice(&angular_radps.to_le_bytes());
        let declared_len: u16 = 16;
        let crc32 = crc32_ieee(&payload[..declared_len as usize]);
        Self {
            magic: FRAME_MAGIC,
            sequence,
            deadline_nanos,
            integrity_flag: 1,
            declared_len,
            crc32,
            payload,
        }
    }

    /// Decode the kinematic command `(linear_mps, angular_radps)` from the
    /// payload. Returns `None` if there are not enough valid bytes.
    pub fn decode_command(&self) -> Option<(f64, f64)> {
        let n = self.declared_len as usize;
        if !(16..=PAYLOAD_CAPACITY).contains(&n) {
            return None;
        }
        let lin = f64::from_le_bytes(self.payload[0..8].try_into().ok()?);
        let ang = f64::from_le_bytes(self.payload[8..16].try_into().ok()?);
        Some((lin, ang))
    }
}

/// Why the subscriber EDGE rejected a frame (before the judge ran).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeReject {
    /// `declared_len` exceeds `MAX_PAYLOAD_LEN` (or the frame capacity).
    Oversize,
    /// CRC over the declared payload bytes did not match the header CRC.
    CrcMismatch,
}

/// The subscriber-edge defense-in-depth: bounds/oversize THEN CRC. Returns
/// `Ok(())` only if the frame is safe to hand to the judge. Pure, no unsafe.
pub fn edge_validate(frame: &CommandFrame) -> Result<(), EdgeReject> {
    let n = frame.declared_len as usize;
    // 1. bounds / oversize — never index past the buffer, never trust a length.
    if n > MAX_PAYLOAD_LEN || n > PAYLOAD_CAPACITY {
        return Err(EdgeReject::Oversize);
    }
    // 2. payload CRC — integrity of the bytes the judge will read.
    if crc32_ieee(&frame.payload[..n]) != frame.crc32 {
        return Err(EdgeReject::CrcMismatch);
    }
    Ok(())
}

/// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320), table-less. Kept in-crate so
/// the spike's only dependency is iceoryx2 (clean feature-subset story).
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
