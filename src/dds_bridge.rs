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
}
