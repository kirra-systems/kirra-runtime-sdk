// src/dds_bridge.rs
//
// DDS publisher bridge for actuator command topics.
//
// SAFETY (SG-016, INV-10): an actuator topic MUST use Volatile durability. A
// TransientLocal actuator topic would let a late-joining / reconnecting
// subscriber replay the LAST-published command — a stale actuation a vehicle
// could act on after a dropout. KeepAll / KeepLast(n>1) history is the same class
// of hazard (a backed-up subscriber drains stale commands), and an unbounded
// Lifespan lets a sample outlive its deadline.
//
// The actuator QoS profile used to be DESCRIPTIVE — `critical_actuator_profile`
// had no caller, so the Volatile rule was only a source-level assertion checked by
// the SG-016 test. It is now ENFORCED at the publish seam: `publish_actuator_command`
// validates the profile (`actuator_admissibility`) and REFUSES to emit a frame
// under a non-Volatile / non-latest-wins / unbounded-lifespan profile (fail-closed),
// so a misconfigured topic cannot replay a stale command at runtime, not just in test.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsReliability {
    Reliable,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsDurability {
    Volatile,
    TransientLocal,
}

/// History QoS. For actuation the only safe policy is `KeepLast(1)` (latest-wins);
/// `KeepAll` (or `KeepLast(n>1)`) would queue commands and let a backed-up
/// subscriber drain stale ones after a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsHistory {
    KeepLast(u16),
    KeepAll,
}

/// Liveliness QoS — the publisher asserts liveliness automatically within
/// `lease_ms`; a subscriber that misses the lease treats the writer as LOST
/// (so a silent governor reads as a fault to fail-close against, never as a
/// held last-command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsLiveliness {
    Automatic { lease_ms: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdsQosProfile {
    pub reliability: DdsReliability,
    pub durability: DdsDurability,
    pub history: DdsHistory,
    pub liveliness: DdsLiveliness,
    /// Command validity horizon (ms): a sample older than this is dropped by the
    /// middleware before any subscriber sees it (kept ≈ the deadline).
    pub lifespan_ms: u32,
    pub deadline_ms: u32,
}

impl DdsQosProfile {
    /// The frozen actuator-topic profile: Reliable + Volatile + KeepLast(1),
    /// Lifespan = deadline, automatic liveliness at the deadline lease.
    pub fn critical_actuator_profile() -> Self {
        Self {
            reliability: DdsReliability::Reliable,
            durability: DdsDurability::Volatile,
            history: DdsHistory::KeepLast(1),
            liveliness: DdsLiveliness::Automatic { lease_ms: 20 },
            lifespan_ms: 20,
            deadline_ms: 20,
        }
    }

    /// Fail-closed QoS admissibility for an ACTUATOR topic. Returns the specific
    /// violation so a caller / `startup_sentinel` can log which rule failed.
    pub fn actuator_admissibility(&self) -> Result<(), DdsQosViolation> {
        // INV-10 / SG-016: Volatile only — TransientLocal would replay a stale
        // command to a reconnecting subscriber.
        if self.durability != DdsDurability::Volatile {
            return Err(DdsQosViolation::NonVolatileActuatorTopic);
        }
        // Latest-wins only — KeepAll / KeepLast(n>1) can drain stale commands.
        if self.history != DdsHistory::KeepLast(1) {
            return Err(DdsQosViolation::NonLatestWinsActuatorTopic);
        }
        // A command with no validity horizon can outlive its deadline.
        if self.lifespan_ms == 0 {
            return Err(DdsQosViolation::UnboundedLifespan);
        }
        // D2: a zero `deadline_ms` is an UNBOUNDED deadline — the subscriber can
        // never detect a missed-publish (a silent governor), so a held last
        // command reads as "fresh" forever. An actuator topic MUST carry a finite
        // deadline so a dropout fails closed. (The field was previously set but
        // never validated.)
        if self.deadline_ms == 0 {
            return Err(DdsQosViolation::UnboundedDeadline);
        }
        Ok(())
    }
}

/// A QoS misconfiguration that would make an actuator topic unsafe. Carrying the
/// specific rule (rather than a bool) lets the publish seam and startup sentinel
/// log WHICH invariant a profile violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsQosViolation {
    NonVolatileActuatorTopic,
    NonLatestWinsActuatorTopic,
    UnboundedLifespan,
    UnboundedDeadline,
}

impl DdsQosViolation {
    pub fn as_str(&self) -> &'static str {
        match self {
            DdsQosViolation::NonVolatileActuatorTopic => "DDS_ACTUATOR_NON_VOLATILE",
            DdsQosViolation::NonLatestWinsActuatorTopic => "DDS_ACTUATOR_NON_LATEST_WINS",
            DdsQosViolation::UnboundedLifespan => "DDS_ACTUATOR_UNBOUNDED_LIFESPAN",
            DdsQosViolation::UnboundedDeadline => "DDS_ACTUATOR_UNBOUNDED_DEADLINE",
        }
    }
}

impl std::fmt::Display for DdsQosViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A minimal, dependency-free **CDR little-endian** body encoder (OMG CDR /
/// DDS-RTPS §10, the `PLAIN_CDR` representation). It exists so the bridge stops
/// emitting hand-rolled byte literals as if they were a serialized sample: a real
/// DDS reader (Cyclone / Fast DDS) decodes primitives at their **natural
/// alignment relative to the start of the CDR body**, so a `u32` field that is not
/// 4-aligned, or a `string` without its `u32` length prefix and NUL terminator, is
/// an off-by-`pad` interop break — the same defect class the encapsulation-header
/// padding fix (`wrap_cdr_encapsulation`) addresses, one layer down.
///
/// Alignment is measured from the body origin (offset 0 of this buffer); the
/// 4-byte encapsulation header is prepended afterwards by `into_cdr_le` /
/// `wrap_cdr_encapsulation` and does not perturb intra-body alignment because it is
/// itself a multiple of 4. Only the primitives the actuator path needs are
/// provided; the encoder is deliberately not a general IDL backend.
#[derive(Debug, Default, Clone)]
pub struct CdrLeEncoder {
    body: Vec<u8>,
}

impl CdrLeEncoder {
    pub fn new() -> Self {
        Self { body: Vec::new() }
    }

    /// Current body length (pre-encapsulation), i.e. the next field's offset.
    pub fn len(&self) -> usize {
        self.body.len()
    }

    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    /// Pad with zero bytes until the body length is a multiple of `align`
    /// (1/2/4/8). CDR aligns each primitive to its own size relative to the body
    /// origin.
    fn align(&mut self, align: usize) {
        debug_assert!(align.is_power_of_two());
        let rem = self.body.len() % align;
        if rem != 0 {
            self.body.resize(self.body.len() + (align - rem), 0u8);
        }
    }

    pub fn put_u8(&mut self, v: u8) -> &mut Self {
        self.body.push(v);
        self
    }

    pub fn put_bool(&mut self, v: bool) -> &mut Self {
        self.put_u8(v as u8)
    }

    pub fn put_i8(&mut self, v: i8) -> &mut Self {
        self.put_u8(v as u8)
    }

    pub fn put_u16(&mut self, v: u16) -> &mut Self {
        self.align(2);
        self.body.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn put_i16(&mut self, v: i16) -> &mut Self {
        self.put_u16(v as u16)
    }

    pub fn put_u32(&mut self, v: u32) -> &mut Self {
        self.align(4);
        self.body.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn put_i32(&mut self, v: i32) -> &mut Self {
        self.put_u32(v as u32)
    }

    pub fn put_u64(&mut self, v: u64) -> &mut Self {
        self.align(8);
        self.body.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn put_i64(&mut self, v: i64) -> &mut Self {
        self.put_u64(v as u64)
    }

    pub fn put_f32(&mut self, v: f32) -> &mut Self {
        self.put_u32(v.to_bits())
    }

    pub fn put_f64(&mut self, v: f64) -> &mut Self {
        self.put_u64(v.to_bits())
    }

    /// A CDR `sequence<octet>` / opaque byte block: a 4-aligned `u32` element
    /// count, then the raw bytes (no terminator).
    pub fn put_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.put_u32(bytes.len() as u32);
        self.body.extend_from_slice(bytes);
        self
    }

    /// A CDR bounded/unbounded `string`: a 4-aligned `u32` length that **includes
    /// the NUL terminator**, the UTF-8 bytes, then a single `\0`.
    pub fn put_string(&mut self, s: &str) -> &mut Self {
        self.put_u32(s.len() as u32 + 1);
        self.body.extend_from_slice(s.as_bytes());
        self.body.push(0u8);
        self
    }

    /// Borrow the raw body (pre-encapsulation), e.g. for a CRC.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Consume the encoder and return the body bytes (pre-encapsulation).
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Finish: prepend the 4-byte RTPS `CDR_LE` encapsulation header and pad the
    /// body to a 4-byte boundary (pad count in the options low byte), producing a
    /// frame a spec reader can decode.
    pub fn into_cdr_le(self) -> Vec<u8> {
        DdsPublisherBridge::wrap_cdr_encapsulation(&self.body)
    }
}

pub struct DdsPublisherBridge;

impl DdsPublisherBridge {
    /// Wrap a CDR body in the 4-byte RTPS encapsulation header (PLAIN_CDR,
    /// little-endian) AND pad it to a 4-byte boundary (C1 — interop correctness).
    ///
    /// The prior version emitted `[0x00,0x01,0x00,0x00] ++ body` with NO trailing
    /// alignment padding and a zero options field. A spec-compliant DDS reader
    /// (Cyclone / Fast DDS) requires the serialized payload to be a multiple of 4
    /// bytes, and reads the **number of trailing pad bytes from the 2 LSBs of the
    /// options field** (DDS-RTPS §10 / DDS-XTypes §7.6.3.1.2) to recover the real
    /// body length — so the old frame was an off-by-`pad` interop break.
    ///
    /// Header bytes: `[0x00, 0x01]` = representation id `CDR_LE` (0x0001, stored
    /// big-endian); `[0x00, P]` = representation options carrying the pad count
    /// `P = (4 - body.len() % 4) % 4` (0..=3) in the low byte.
    pub fn wrap_cdr_encapsulation(payload: &[u8]) -> Vec<u8> {
        let pad = (4 - (payload.len() % 4)) % 4;
        let mut wrapped = Vec::with_capacity(4 + payload.len() + pad);
        wrapped.extend_from_slice(&[0x00, 0x01, 0x00, pad as u8]);
        wrapped.extend_from_slice(payload);
        wrapped.resize(wrapped.len() + pad, 0u8);
        wrapped
    }

    /// Publish an actuator command under `profile`, ENFORCING the actuator QoS
    /// invariant FIRST (fail-closed): a non-Volatile / non-latest-wins /
    /// unbounded-lifespan profile is REFUSED — no frame is produced — so a
    /// misconfigured topic can never replay a stale command. On success returns
    /// the CDR-encapsulated frame ready for the writer.
    pub fn publish_actuator_command(
        payload: &[u8],
        profile: &DdsQosProfile,
    ) -> Result<Vec<u8>, DdsQosViolation> {
        profile.actuator_admissibility()?;
        Ok(Self::wrap_cdr_encapsulation(payload))
    }
}

#[cfg(test)]
mod dds_qos_tests {
    use super::*;

    #[test]
    fn critical_profile_is_admissible_and_publishes() {
        let profile = DdsQosProfile::critical_actuator_profile();
        assert!(profile.actuator_admissibility().is_ok());
        let frame = DdsPublisherBridge::publish_actuator_command(&[0xAA, 0xBB], &profile)
            .expect("the frozen critical profile must be admissible");
        // CDR_LE header (4 bytes): rep id 0x0001 (big-endian), options low byte =
        // the trailing pad count. A 2-byte body needs 2 pad bytes → P = 2, and the
        // whole frame is a 4-byte multiple (4 header + 2 body + 2 pad = 8).
        assert_eq!(&frame[..4], &[0x00, 0x01, 0x00, 0x02]);
        assert_eq!(&frame[4..], &[0xAA, 0xBB, 0x00, 0x00]);
        assert_eq!(frame.len() % 4, 0);
    }

    #[test]
    fn unbounded_deadline_is_refused() {
        // D2: a zero deadline is unbounded — a missed-publish (silent governor) is
        // undetectable, so a held last command reads as fresh forever. Fail closed.
        let mut profile = DdsQosProfile::critical_actuator_profile();
        profile.deadline_ms = 0;
        assert_eq!(
            DdsPublisherBridge::publish_actuator_command(&[0x01], &profile),
            Err(DdsQosViolation::UnboundedDeadline),
            "an actuator topic with no deadline cannot detect a dropout and must be refused"
        );
        assert_eq!(
            DdsQosViolation::UnboundedDeadline.as_str(),
            "DDS_ACTUATOR_UNBOUNDED_DEADLINE"
        );
    }

    #[test]
    fn cdr_encapsulation_pads_body_to_four_and_records_pad_count() {
        // Body lengths 0..=7 → pad = (4 - len%4)%4, frame always a 4-multiple, and
        // the options low byte carries the exact pad count a reader uses to recover
        // the real body length.
        for len in 0u8..=7 {
            let body: Vec<u8> = (0..len).map(|i| 0x10 + i).collect();
            let frame = DdsPublisherBridge::wrap_cdr_encapsulation(&body);
            let expected_pad = (4 - (len as usize % 4)) % 4;
            assert_eq!(&frame[..3], &[0x00, 0x01, 0x00], "rep id CDR_LE for len {len}");
            assert_eq!(frame[3] as usize, expected_pad, "pad count for len {len}");
            assert_eq!(frame.len() % 4, 0, "frame is a 4-byte multiple for len {len}");
            assert_eq!(&frame[4..4 + len as usize], &body[..], "body preserved for len {len}");
            // The pad bytes are zero.
            assert!(frame[4 + len as usize..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn cdr_le_encoder_aligns_primitives_to_natural_boundaries() {
        // A u8 then a u32: the u32 must start at a 4-aligned offset, so 3 pad bytes
        // sit between them (offsets: u8@0, pad@1..=3, u32@4..=7).
        let mut enc = CdrLeEncoder::new();
        enc.put_u8(0xAB).put_u32(0x11223344);
        let body = enc.into_body();
        assert_eq!(body.len(), 8);
        assert_eq!(body[0], 0xAB);
        assert_eq!(&body[1..4], &[0x00, 0x00, 0x00], "3 pad bytes align the u32");
        assert_eq!(&body[4..8], &0x11223344u32.to_le_bytes(), "little-endian u32");
    }

    #[test]
    fn cdr_le_encoder_u64_and_f64_align_to_eight() {
        let mut enc = CdrLeEncoder::new();
        enc.put_u8(0x01).put_u64(0x0102030405060708);
        let body = enc.into_body();
        // u8@0, pad@1..=7, u64@8..=15.
        assert_eq!(body.len(), 16);
        assert_eq!(&body[1..8], &[0; 7], "7 pad bytes align the u64");
        assert_eq!(&body[8..16], &0x0102030405060708u64.to_le_bytes());
    }

    #[test]
    fn cdr_le_encoder_string_is_length_prefixed_and_nul_terminated() {
        // CDR string: u32 length INCLUDING the NUL, the bytes, then `\0`.
        let mut enc = CdrLeEncoder::new();
        enc.put_string("hi");
        let body = enc.into_body();
        assert_eq!(&body[0..4], &3u32.to_le_bytes(), "length includes the NUL terminator");
        assert_eq!(&body[4..6], b"hi");
        assert_eq!(body[6], 0u8, "NUL terminator");
        assert_eq!(body.len(), 7);
    }

    #[test]
    fn cdr_le_encoder_into_cdr_le_wraps_and_pads() {
        // Round-trip through the encapsulation: a 2-byte body (u16) → header + body
        // padded to 4. The encoder body is 4-multiple-padded by the wrapper.
        let mut enc = CdrLeEncoder::new();
        enc.put_u16(0xBEEF);
        let frame = enc.into_cdr_le();
        assert_eq!(&frame[..4], &[0x00, 0x01, 0x00, 0x02], "pad count 2 for a 2-byte body");
        assert_eq!(&frame[4..6], &0xBEEFu16.to_le_bytes());
        assert_eq!(&frame[6..8], &[0x00, 0x00]);
        assert_eq!(frame.len(), 8);
    }

    #[test]
    fn transient_local_actuator_topic_is_refused() {
        let mut profile = DdsQosProfile::critical_actuator_profile();
        profile.durability = DdsDurability::TransientLocal;
        assert_eq!(
            profile.actuator_admissibility(),
            Err(DdsQosViolation::NonVolatileActuatorTopic)
        );
        // The publish seam must emit NO frame for a TransientLocal actuator topic.
        assert_eq!(
            DdsPublisherBridge::publish_actuator_command(&[0x01], &profile),
            Err(DdsQosViolation::NonVolatileActuatorTopic),
            "INV-10/SG-016: a TransientLocal actuator topic could replay stale \
             commands and must be refused fail-closed"
        );
    }

    #[test]
    fn keep_all_or_deep_history_is_refused() {
        for history in [DdsHistory::KeepAll, DdsHistory::KeepLast(8)] {
            let mut profile = DdsQosProfile::critical_actuator_profile();
            profile.history = history;
            assert_eq!(
                DdsPublisherBridge::publish_actuator_command(&[0x01], &profile),
                Err(DdsQosViolation::NonLatestWinsActuatorTopic),
                "non-latest-wins history {history:?} can drain stale commands and must be refused"
            );
        }
    }

    #[test]
    fn unbounded_lifespan_is_refused() {
        let mut profile = DdsQosProfile::critical_actuator_profile();
        profile.lifespan_ms = 0;
        assert_eq!(
            DdsPublisherBridge::publish_actuator_command(&[0x01], &profile),
            Err(DdsQosViolation::UnboundedLifespan),
            "a command with no validity horizon can outlive its deadline"
        );
    }
}
