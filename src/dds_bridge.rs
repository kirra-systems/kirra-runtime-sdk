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
}

impl DdsQosViolation {
    pub fn as_str(&self) -> &'static str {
        match self {
            DdsQosViolation::NonVolatileActuatorTopic => "DDS_ACTUATOR_NON_VOLATILE",
            DdsQosViolation::NonLatestWinsActuatorTopic => "DDS_ACTUATOR_NON_LATEST_WINS",
            DdsQosViolation::UnboundedLifespan => "DDS_ACTUATOR_UNBOUNDED_LIFESPAN",
        }
    }
}

impl std::fmt::Display for DdsQosViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct DdsPublisherBridge;

impl DdsPublisherBridge {
    /// Prepend the 4-byte CDR (little-endian) encapsulation header to a payload.
    /// Transport-level framing only; carries no QoS guarantee on its own.
    pub fn wrap_cdr_encapsulation(payload: &[u8]) -> Vec<u8> {
        let mut wrapped = Vec::with_capacity(4 + payload.len());
        wrapped.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        wrapped.extend_from_slice(payload);
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
        // CDR header (4 bytes) + payload.
        assert_eq!(&frame[..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&frame[4..], &[0xAA, 0xBB]);
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
        let frame = DdsPublisherBridge::publish_actuator_command(&[0xDE, 0xAD], &requested).unwrap();
        writer.publish(&frame).unwrap();
        let sent = writer.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(&sent[0][..4], &[0x00, 0x01, 0x00, 0x00], "the writer received the CDR-encapsulated frame");
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
