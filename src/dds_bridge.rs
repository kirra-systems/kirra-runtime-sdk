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

// ===========================================================================
// D1 — real-writer seam + read-back QoS validation (review item 19)
// ===========================================================================
//
// `actuator_admissibility` validates the profile we ASK FOR. But DDS QoS is
// REQUESTED-OFFERED: the middleware may negotiate a writer whose effective QoS
// differs from what we requested (a misconfigured XML profile, an env override,
// a participant default, or a peer-driven downgrade). For an actuator topic a
// SILENT downgrade is exactly the INV-10/SG-016 hazard re-opened one layer down:
// a writer that came back TransientLocal, or with the deadline dropped, can
// replay or outlive a command even though the profile we handed in was clean.
//
// So the real-writer contract is: create the writer, READ ITS QoS BACK
// (`dds_get_qos` in the CycloneDDS impl), and run it through
// `validate_qos_readback` — refuse to publish on ANY actuator-relevant
// relaxation (fail-closed). This module owns that check (host-testable); the
// `cyclonedds`-gated `dds_cyclonedds` module owns the FFI that feeds it.

/// A read-back QoS mismatch: the writer the middleware actually created is
/// WEAKER than the actuator profile we requested on some safety-relevant axis.
/// Each variant names the relaxed policy so the publish seam / startup log can
/// report which guarantee the middleware failed to honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QosReadbackError {
    /// The negotiated writer is not Volatile (e.g. TransientLocal) → a
    /// reconnecting subscriber could replay the last command.
    DurabilityRelaxed { requested: DdsDurability, negotiated: DdsDurability },
    /// History is no longer KeepLast(1) → a backed-up subscriber can drain
    /// stale commands.
    HistoryRelaxed { requested: DdsHistory, negotiated: DdsHistory },
    /// Reliability dropped (Reliable → BestEffort) → commands may be silently
    /// lost without the writer noticing.
    ReliabilityRelaxed { requested: DdsReliability, negotiated: DdsReliability },
    /// The negotiated lifespan is unbounded or LONGER than requested → a sample
    /// can outlive its validity horizon.
    LifespanRelaxed { requested_ms: u32, negotiated_ms: u32 },
    /// The negotiated deadline is unbounded or LONGER than requested → a missed
    /// publish (silent governor) goes undetected for longer.
    DeadlineRelaxed { requested_ms: u32, negotiated_ms: u32 },
    /// The negotiated liveliness lease is LONGER than requested → writer loss is
    /// detected more slowly.
    LivelinessLeaseRelaxed { requested_ms: u32, negotiated_ms: u32 },
}

impl QosReadbackError {
    pub fn as_str(&self) -> &'static str {
        match self {
            QosReadbackError::DurabilityRelaxed { .. } => "DDS_READBACK_DURABILITY_RELAXED",
            QosReadbackError::HistoryRelaxed { .. } => "DDS_READBACK_HISTORY_RELAXED",
            QosReadbackError::ReliabilityRelaxed { .. } => "DDS_READBACK_RELIABILITY_RELAXED",
            QosReadbackError::LifespanRelaxed { .. } => "DDS_READBACK_LIFESPAN_RELAXED",
            QosReadbackError::DeadlineRelaxed { .. } => "DDS_READBACK_DEADLINE_RELAXED",
            QosReadbackError::LivelinessLeaseRelaxed { .. } => "DDS_READBACK_LIVELINESS_RELAXED",
        }
    }
}

impl std::fmt::Display for QosReadbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Fail-closed read-back check: the `negotiated` QoS (what the middleware
/// actually created, read back via `dds_get_qos`) must be AT LEAST AS STRICT as
/// the `requested` actuator profile on every safety axis. Returns the FIRST
/// relaxation found (checked in the same order as `actuator_admissibility`).
///
/// "At least as strict" for an actuator topic means: same Volatile durability,
/// same KeepLast(1) history, still Reliable, and a lifespan / deadline / liveliness
/// lease that is bounded (non-zero) AND not longer than requested. A negotiated
/// horizon SHORTER than requested is acceptable (stricter); LONGER or zero is a
/// relaxation and is refused.
pub fn validate_qos_readback(
    requested: &DdsQosProfile,
    negotiated: &DdsQosProfile,
) -> Result<(), QosReadbackError> {
    if negotiated.durability != requested.durability {
        return Err(QosReadbackError::DurabilityRelaxed {
            requested: requested.durability,
            negotiated: negotiated.durability,
        });
    }
    if negotiated.history != requested.history {
        return Err(QosReadbackError::HistoryRelaxed {
            requested: requested.history,
            negotiated: negotiated.history,
        });
    }
    if negotiated.reliability != requested.reliability {
        return Err(QosReadbackError::ReliabilityRelaxed {
            requested: requested.reliability,
            negotiated: negotiated.reliability,
        });
    }
    // A zero (unbounded) negotiated horizon is the worst case and is caught by
    // the `>` comparison too, since requested is non-zero for the critical
    // profile — but spell it out: 0 means "infinite", which is always relaxed.
    if negotiated.lifespan_ms == 0 || negotiated.lifespan_ms > requested.lifespan_ms {
        return Err(QosReadbackError::LifespanRelaxed {
            requested_ms: requested.lifespan_ms,
            negotiated_ms: negotiated.lifespan_ms,
        });
    }
    if negotiated.deadline_ms == 0 || negotiated.deadline_ms > requested.deadline_ms {
        return Err(QosReadbackError::DeadlineRelaxed {
            requested_ms: requested.deadline_ms,
            negotiated_ms: negotiated.deadline_ms,
        });
    }
    let DdsLiveliness::Automatic { lease_ms: req_lease } = requested.liveliness;
    let DdsLiveliness::Automatic { lease_ms: neg_lease } = negotiated.liveliness;
    if neg_lease == 0 || neg_lease > req_lease {
        return Err(QosReadbackError::LivelinessLeaseRelaxed {
            requested_ms: req_lease,
            negotiated_ms: neg_lease,
        });
    }
    Ok(())
}

/// The seam a real DDS writer plugs into. The hot path hands the writer a
/// pre-encapsulated CDR actuator frame; the writer is responsible for having
/// already validated its NEGOTIATED QoS read-back (fail-closed) at open time.
/// The `cyclonedds`-gated `CycloneDdsActuatorWriter` is the production impl; a
/// `LoopbackTestWriter` exercises the seam + read-back logic on the host.
pub trait DdsActuatorWriter {
    type Error: std::fmt::Debug;

    /// The QoS the middleware actually negotiated for this writer (read back via
    /// `dds_get_qos`), so a caller can re-assert `validate_qos_readback`.
    fn negotiated_qos(&self) -> DdsQosProfile;

    /// Publish one pre-encapsulated CDR actuator frame (e.g. from
    /// `DdsPublisherBridge::publish_actuator_command`).
    fn publish(&self, frame: &[u8]) -> Result<(), Self::Error>;
}

/// CycloneDDS QoS expressed as the raw C-API kind enums + nanosecond durations,
/// DECOUPLED from the FFI so the whole mapping is host-testable (the FFI glue in
/// the `cyclonedds`-gated `dds_cyclonedds` module is then a thin pass-through:
/// these integers go straight into `dds_qset_*` and come straight out of
/// `dds_qget_*`). Constants mirror `dds/dds.h` (stable ABI; cited per field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycloneQosParams {
    /// `dds_durability_kind_t`
    pub durability_kind: i32,
    /// `dds_history_kind_t`
    pub history_kind: i32,
    pub history_depth: i32,
    /// `dds_reliability_kind_t`
    pub reliability_kind: i32,
    /// `dds_duration_t` (ns); `INFINITY_NS` = unbounded
    pub lifespan_ns: i64,
    pub deadline_ns: i64,
    /// `dds_liveliness_kind_t`
    pub liveliness_kind: i32,
    pub liveliness_lease_ns: i64,
}

impl CycloneQosParams {
    // dds/dds.h enum values — part of the CycloneDDS ABI.
    pub const DURABILITY_VOLATILE: i32 = 0;
    pub const DURABILITY_TRANSIENT_LOCAL: i32 = 1;
    pub const HISTORY_KEEP_LAST: i32 = 0;
    pub const HISTORY_KEEP_ALL: i32 = 1;
    pub const RELIABILITY_BEST_EFFORT: i32 = 0;
    pub const RELIABILITY_RELIABLE: i32 = 1;
    pub const LIVELINESS_AUTOMATIC: i32 = 0;
    /// `DDS_INFINITY` — the max `dds_duration_t`.
    pub const INFINITY_NS: i64 = i64::MAX;

    const NS_PER_MS: i64 = 1_000_000;
}

/// Map an actuator `DdsQosProfile` to the CycloneDDS C-API parameters a writer is
/// created with. `ms → ns`; a finite, non-zero horizon stays finite. (A zero ms
/// horizon never reaches here for the critical profile — `actuator_admissibility`
/// already rejects it — but if one did, it maps to `INFINITY_NS`, which the
/// read-back validation then refuses, so the fail-closed posture is preserved.)
pub fn qos_to_cyclone_params(profile: &DdsQosProfile) -> CycloneQosParams {
    let ms_to_ns = |ms: u32| -> i64 {
        if ms == 0 {
            CycloneQosParams::INFINITY_NS
        } else {
            (ms as i64).saturating_mul(CycloneQosParams::NS_PER_MS)
        }
    };
    let DdsLiveliness::Automatic { lease_ms } = profile.liveliness;
    CycloneQosParams {
        durability_kind: match profile.durability {
            DdsDurability::Volatile => CycloneQosParams::DURABILITY_VOLATILE,
            DdsDurability::TransientLocal => CycloneQosParams::DURABILITY_TRANSIENT_LOCAL,
        },
        history_kind: match profile.history {
            DdsHistory::KeepLast(_) => CycloneQosParams::HISTORY_KEEP_LAST,
            DdsHistory::KeepAll => CycloneQosParams::HISTORY_KEEP_ALL,
        },
        history_depth: match profile.history {
            DdsHistory::KeepLast(n) => n as i32,
            DdsHistory::KeepAll => 0,
        },
        reliability_kind: match profile.reliability {
            DdsReliability::Reliable => CycloneQosParams::RELIABILITY_RELIABLE,
            DdsReliability::BestEffort => CycloneQosParams::RELIABILITY_BEST_EFFORT,
        },
        lifespan_ns: ms_to_ns(profile.lifespan_ms),
        deadline_ns: ms_to_ns(profile.deadline_ms),
        liveliness_kind: CycloneQosParams::LIVELINESS_AUTOMATIC,
        liveliness_lease_ns: ms_to_ns(lease_ms),
    }
}

/// Reconstruct a `DdsQosProfile` from the QoS read back off a live writer
/// (`dds_qget_*`). Fail-closed: an UNRECOGNISED kind (a durability/history/
/// reliability/liveliness value Kirra's model can't represent) returns `None`,
/// which the caller treats as a refusal — never a silent best-effort coercion. An
/// unbounded or non-positive duration maps to `0` ms (the "unbounded" sentinel),
/// which `validate_qos_readback` then rejects.
pub fn cyclone_params_to_qos(p: &CycloneQosParams) -> Option<DdsQosProfile> {
    // ns → ms, rounding a sub-ms positive UP to 1 ms (so a strict, tiny finite
    // horizon is never mistaken for the 0 = unbounded sentinel). Unbounded /
    // non-positive → 0 (unbounded), which fails the read-back check.
    let ns_to_ms = |ns: i64| -> u32 {
        if ns <= 0 || ns == CycloneQosParams::INFINITY_NS {
            0
        } else {
            ns.saturating_add(CycloneQosParams::NS_PER_MS - 1)
                .saturating_div(CycloneQosParams::NS_PER_MS)
                .min(u32::MAX as i64) as u32
        }
    };
    let durability = match p.durability_kind {
        CycloneQosParams::DURABILITY_VOLATILE => DdsDurability::Volatile,
        CycloneQosParams::DURABILITY_TRANSIENT_LOCAL => DdsDurability::TransientLocal,
        _ => return None, // Transient / Persistent — not in Kirra's actuator model
    };
    let history = match p.history_kind {
        CycloneQosParams::HISTORY_KEEP_LAST => {
            DdsHistory::KeepLast(p.history_depth.clamp(0, u16::MAX as i32) as u16)
        }
        CycloneQosParams::HISTORY_KEEP_ALL => DdsHistory::KeepAll,
        _ => return None,
    };
    let reliability = match p.reliability_kind {
        CycloneQosParams::RELIABILITY_RELIABLE => DdsReliability::Reliable,
        CycloneQosParams::RELIABILITY_BEST_EFFORT => DdsReliability::BestEffort,
        _ => return None,
    };
    // Kirra's model only carries AUTOMATIC liveliness; a manual kind is outside
    // the actuator contract → fail closed.
    if p.liveliness_kind != CycloneQosParams::LIVELINESS_AUTOMATIC {
        return None;
    }
    Some(DdsQosProfile {
        reliability,
        durability,
        history,
        liveliness: DdsLiveliness::Automatic { lease_ms: ns_to_ms(p.liveliness_lease_ns) },
        lifespan_ms: ns_to_ms(p.lifespan_ns),
        deadline_ms: ns_to_ms(p.deadline_ns),
    })
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

    // --- D1: read-back QoS validation + writer seam ------------------------

    /// A host writer that records published frames and reports a CONFIGURABLE
    /// negotiated QoS, so the read-back validation (the part the CycloneDDS impl
    /// can't run in CI) is exercised on every downgrade path.
    struct LoopbackTestWriter {
        negotiated: DdsQosProfile,
        sent: std::cell::RefCell<Vec<Vec<u8>>>,
    }

    impl LoopbackTestWriter {
        fn new(negotiated: DdsQosProfile) -> Self {
            Self { negotiated, sent: std::cell::RefCell::new(Vec::new()) }
        }
    }

    impl DdsActuatorWriter for LoopbackTestWriter {
        type Error = std::convert::Infallible;
        fn negotiated_qos(&self) -> DdsQosProfile { self.negotiated }
        fn publish(&self, frame: &[u8]) -> Result<(), Self::Error> {
            self.sent.borrow_mut().push(frame.to_vec());
            Ok(())
        }
    }

    #[test]
    fn readback_accepts_exact_match_and_writer_round_trips() {
        let requested = DdsQosProfile::critical_actuator_profile();
        let writer = LoopbackTestWriter::new(requested);
        assert_eq!(validate_qos_readback(&requested, &writer.negotiated_qos()), Ok(()));
        // A 2-byte body is NOT 4-byte aligned, so `wrap_cdr_encapsulation`
        // appends `pad = (4 - 2 % 4) % 4 = 2` trailing zero bytes and advertises
        // that count in the options low byte (DDS-RTPS §10 / DDS-XTypes
        // §7.6.3.1.2). The full frame is therefore the header `[0x00,0x01,0x00,
        // 0x02]` (CDR_LE id + options-pad=2), the body, then the 2 pad bytes.
        let frame = DdsPublisherBridge::publish_actuator_command(&[0xDE, 0xAD], &requested).unwrap();
        writer.publish(&frame).unwrap();
        let sent = writer.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            vec![0x00, 0x01, 0x00, 0x02, 0xDE, 0xAD, 0x00, 0x00],
            "the writer received the CDR-encapsulated frame (header + body + alignment pad)"
        );
    }

    #[test]
    fn readback_accepts_stricter_horizons() {
        // A negotiated writer with SHORTER lifespan/deadline/lease is stricter,
        // not a relaxation → admissible.
        let requested = DdsQosProfile::critical_actuator_profile();
        let mut stricter = requested;
        stricter.lifespan_ms = requested.lifespan_ms - 5;
        stricter.deadline_ms = requested.deadline_ms - 5;
        stricter.liveliness = DdsLiveliness::Automatic { lease_ms: 10 };
        assert_eq!(validate_qos_readback(&requested, &stricter), Ok(()));
    }

    #[test]
    fn readback_rejects_transient_local_downgrade() {
        let requested = DdsQosProfile::critical_actuator_profile();
        let mut neg = requested;
        neg.durability = DdsDurability::TransientLocal;
        assert_eq!(
            validate_qos_readback(&requested, &neg),
            Err(QosReadbackError::DurabilityRelaxed {
                requested: DdsDurability::Volatile,
                negotiated: DdsDurability::TransientLocal,
            }),
            "a writer the middleware created TransientLocal must be refused — INV-10 re-opened on the wire"
        );
    }

    #[test]
    fn readback_rejects_history_and_reliability_downgrades() {
        let requested = DdsQosProfile::critical_actuator_profile();
        let mut h = requested;
        h.history = DdsHistory::KeepLast(8);
        assert!(matches!(
            validate_qos_readback(&requested, &h),
            Err(QosReadbackError::HistoryRelaxed { .. })
        ));
        let mut r = requested;
        r.reliability = DdsReliability::BestEffort;
        assert!(matches!(
            validate_qos_readback(&requested, &r),
            Err(QosReadbackError::ReliabilityRelaxed { .. })
        ));
    }

    #[test]
    fn readback_rejects_unbounded_or_longer_horizons() {
        let requested = DdsQosProfile::critical_actuator_profile();

        // Zero (unbounded) lifespan.
        let mut z = requested;
        z.lifespan_ms = 0;
        assert!(matches!(
            validate_qos_readback(&requested, &z),
            Err(QosReadbackError::LifespanRelaxed { negotiated_ms: 0, .. })
        ));

        // Longer deadline than requested.
        let mut d = requested;
        d.deadline_ms = requested.deadline_ms + 50;
        assert!(matches!(
            validate_qos_readback(&requested, &d),
            Err(QosReadbackError::DeadlineRelaxed { .. })
        ));

        // Longer liveliness lease (slower loss detection).
        let mut l = requested;
        l.liveliness = DdsLiveliness::Automatic { lease_ms: 10_000 };
        assert!(matches!(
            validate_qos_readback(&requested, &l),
            Err(QosReadbackError::LivelinessLeaseRelaxed { .. })
        ));
    }

    // --- D1: CycloneDDS QoS mapping (pure, host-tested) --------------------

    #[test]
    fn cyclone_params_round_trip_the_critical_profile() {
        let p = DdsQosProfile::critical_actuator_profile();
        let params = qos_to_cyclone_params(&p);
        // Spot-check the C-API encoding.
        assert_eq!(params.durability_kind, CycloneQosParams::DURABILITY_VOLATILE);
        assert_eq!(params.history_kind, CycloneQosParams::HISTORY_KEEP_LAST);
        assert_eq!(params.history_depth, 1);
        assert_eq!(params.reliability_kind, CycloneQosParams::RELIABILITY_RELIABLE);
        assert_eq!(params.deadline_ns, 20 * 1_000_000, "20 ms → 20_000_000 ns");
        // Round-trips back to an equal profile and is read-back-admissible.
        let back = cyclone_params_to_qos(&params).expect("critical profile round-trips");
        assert_eq!(back, p);
        assert_eq!(validate_qos_readback(&p, &back), Ok(()));
    }

    #[test]
    fn cyclone_params_to_qos_maps_infinity_to_unbounded_and_fails_readback() {
        let requested = DdsQosProfile::critical_actuator_profile();
        let mut params = qos_to_cyclone_params(&requested);
        // A writer that came back with an INFINITE lifespan.
        params.lifespan_ns = CycloneQosParams::INFINITY_NS;
        let back = cyclone_params_to_qos(&params).expect("kinds still valid");
        assert_eq!(back.lifespan_ms, 0, "DDS_INFINITY → 0 ms (unbounded sentinel)");
        assert!(matches!(
            validate_qos_readback(&requested, &back),
            Err(QosReadbackError::LifespanRelaxed { negotiated_ms: 0, .. })
        ));
    }

    #[test]
    fn cyclone_params_to_qos_fails_closed_on_unknown_kind() {
        let mut params = qos_to_cyclone_params(&DdsQosProfile::critical_actuator_profile());
        // dds_durability_kind_t = 3 (PERSISTENT) — outside Kirra's actuator model.
        params.durability_kind = 3;
        assert_eq!(cyclone_params_to_qos(&params), None,
            "an unrepresentable durability kind must fail closed, not coerce");
    }

    #[test]
    fn cyclone_params_transient_local_round_trips_and_is_caught() {
        // A writer the middleware made TransientLocal reads back faithfully and is
        // then rejected by the read-back validation (defense in depth: the mapping
        // does NOT hide the downgrade).
        let requested = DdsQosProfile::critical_actuator_profile();
        let mut params = qos_to_cyclone_params(&requested);
        params.durability_kind = CycloneQosParams::DURABILITY_TRANSIENT_LOCAL;
        let back = cyclone_params_to_qos(&params).unwrap();
        assert_eq!(back.durability, DdsDurability::TransientLocal);
        assert!(matches!(
            validate_qos_readback(&requested, &back),
            Err(QosReadbackError::DurabilityRelaxed { .. })
        ));
    }
}
