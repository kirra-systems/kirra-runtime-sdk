//! Non-frozen extensions to [`DenyCode`] (#793 F6).
//!
//! `DenyCode` is DEFINED in the byte-frozen kinematics-contract talisman
//! (`kinematics_contract.rs`, git blob `ed00f4da…`), which must never change —
//! it is pinned by the safety-case talisman gate and the Kani proof. These
//! auxiliary items — a bounded [`ALL`](DenyCode::ALL) array and an
//! [`index`](DenyCode::index) slot map, used only by the `/metrics`
//! `kirra_actuator_denials_total{code=…}` family — therefore live HERE, in a
//! separate compilation unit, so the observability path can enumerate the codes
//! without touching the talisman. Rust permits a second inherent `impl` block on
//! a type declared in another module of the same crate; and because `ALL` has a
//! FIXED length, adding a `DenyCode` variant is a compile error here until it is
//! listed — the same "bounded label set, no free-form strings" discipline the
//! `GateDenialReason` family follows.

use crate::kinematics_contract::DenyCode;

impl DenyCode {
    /// Every `DenyCode`, in declaration (and bincode-wire-tag) order. A single
    /// source of truth for bounded, allocation-free iteration — the `/metrics`
    /// `kirra_actuator_denials_total{code=…}` family (#793 F6) emits one line
    /// per entry so a code that has never fired still exposes a `0` series, and
    /// [`index`](Self::index) maps a code to its slot in a fixed-size counter
    /// array.
    pub const ALL: [DenyCode; 12] = [
        Self::NanInfLinearVelocity,
        Self::NanInfCurrentVelocity,
        Self::NanInfSteeringAngle,
        Self::NanInfCurrentSteering,
        Self::NanInfDeltaTime,
        Self::InvalidTimeDelta,
        Self::AssetLockedOut,
        Self::DrivableSpaceDeparture,
        Self::DegradedReinitiationDenied,
        Self::DegradedSpeedIncreaseDenied,
        Self::FrameIntegrityUntrusted,
        Self::TrajectoryHorizonExceeded,
    ];

    /// This code's stable slot in [`ALL`](Self::ALL) (equivalently its bincode
    /// wire tag) — the index into a `[_; DenyCode::ALL.len()]` counter array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::NanInfLinearVelocity => 0,
            Self::NanInfCurrentVelocity => 1,
            Self::NanInfSteeringAngle => 2,
            Self::NanInfCurrentSteering => 3,
            Self::NanInfDeltaTime => 4,
            Self::InvalidTimeDelta => 5,
            Self::AssetLockedOut => 6,
            Self::DrivableSpaceDeparture => 7,
            Self::DegradedReinitiationDenied => 8,
            Self::DegradedSpeedIncreaseDenied => 9,
            Self::FrameIntegrityUntrusted => 10,
            Self::TrajectoryHorizonExceeded => 11,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DenyCode;

    /// `ALL` and `index` agree: every entry maps to its own position, and the
    /// set is exactly the 12 codes (a new variant must be added here).
    #[test]
    fn all_and_index_are_consistent() {
        assert_eq!(DenyCode::ALL.len(), 12);
        for (i, code) in DenyCode::ALL.iter().enumerate() {
            assert_eq!(code.index(), i, "{} at wrong slot", code.reason());
        }
        // Reasons are distinct, so no two codes share a slot.
        let mut reasons: Vec<&str> = DenyCode::ALL.iter().map(|c| c.reason()).collect();
        reasons.sort_unstable();
        reasons.dedup();
        assert_eq!(reasons.len(), 12, "DenyCode reasons must be distinct");
    }
}
