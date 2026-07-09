// inline.rs — WP-21b: the ASSEMBLED EP-01 governor loop over the iceoryx2
// carrier (production adoption of the zero-copy transport).
//
// `frozen.rs` (#275 / L2) proved the TRANSPORT half: the frozen
// `GovernorContractView` rides a real iceoryx2 channel and the production
// `validate()` reaches the right verdict on every fault class. THIS module
// closes the remaining WP-21b step: the full EP-01 assembly — `GovernorStation`
// (read → validate → decode → kinematic bound → Ed25519 release token) feeding
// `ActuatorStation` (verify-before-release → strictly-advancing release
// sequence → decode) — consumes commands RECEIVED over that same channel. The
// enforced path over iceoryx2 is now the identical code the POSIX-SHM carrier
// runs; only the carrier differs.
//
// ISOLATION (the #275 gate, unchanged): this crate remains its own workspace;
// the dependency direction is spike → SDK crates (kirra-contract-channel,
// kirra-inline-governor), never the reverse — iceoryx2 stays out of the
// SDK/parko dependency tree. Adoption is crate-level opt-in: a deployment that
// wants the iceoryx2 carrier builds THIS tree; the default SDK build is
// byte-identical without it.
//
// COHERENCE MODEL: iceoryx2 delivers OWNED samples — its zero-copy ownership
// protocol is the coherence mechanism, where the SHM carrier uses the seqlock.
// An owned sample's `generation` is stable across `load/copy/load`, so the
// seqlock re-check inside `decide_cycle` trivially holds; every OTHER §4
// contract check (layout / magic / bounds / CRC / generation-evenness /
// sequence watermark / deadline) still runs in the production pipeline,
// unchanged. Nothing is bypassed — one mechanism is simply already satisfied
// by the transport's ownership semantics.
//
// HONESTY BANNER: the PASS gate is VERDICT + RELEASE-CHAIN CORRECTNESS. Host
// timing is INDICATIVE ONLY (QNX-target-under-FIFO numbers are the certified
// path, #274).

use kirra_contract_channel::{ContractReader, GovernorContractView};
use kirra_inline_governor::{
    govern_and_release, ActuatorStation, GovernorStation, ReleaseRefusal, ReleasedCommand,
};

use crate::frozen::FrozenChannel;

/// An OWNED received iceoryx2 sample presented as the governor's contract
/// region. Immutable by construction, so a coherent snapshot is guaranteed;
/// see the module-level COHERENCE MODEL note.
pub struct SampleRegion(GovernorContractView);

impl SampleRegion {
    #[must_use]
    pub fn new(view: GovernorContractView) -> Self {
        Self(view)
    }
}

impl ContractReader for SampleRegion {
    fn load_generation(&self) -> u64 {
        self.0.generation
    }
    fn copy_view(&self) -> GovernorContractView {
        self.0
    }
}

/// Publish `view` over the channel, receive it as an owned sample, and run the
/// ASSEMBLED loop on the received bytes: governor cycle (validate + kinematic
/// bound + token) then actuator verify-before-release. `now_nanos` is boundary
/// clock domain (HVCHAN-001 §5).
pub fn govern_over_channel(
    channel: &FrozenChannel,
    governor: &mut GovernorStation,
    actuator: &mut ActuatorStation,
    view: GovernorContractView,
    now_nanos: u64,
) -> Result<Result<ReleasedCommand, ReleaseRefusal>, Box<dyn core::error::Error>> {
    let received = channel.round_trip(view)?;
    let region = SampleRegion::new(received);
    Ok(govern_and_release(governor, actuator, &region, now_nanos))
}

#[cfg(test)]
mod inline_over_iceoryx2 {
    use super::*;
    use ed25519_dalek::SigningKey;
    use kirra_contract_channel::VehicleCommandPayload;
    use kirra_core::kinematics_contract::VehicleKinematicsContract;

    const FUTURE_DEADLINE: u64 = u64::MAX / 2;
    /// Committed (even) generation for published views.
    const GEN: u64 = 2;

    fn stations() -> (GovernorStation, ActuatorStation) {
        let gov = GovernorStation::new(
            VehicleKinematicsContract::nominal_reference_profile(),
            SigningKey::from_bytes(&[11u8; 32]),
        );
        let act = ActuatorStation::new(gov.verifying_key());
        (gov, act)
    }

    fn payload(linear: f64) -> VehicleCommandPayload {
        VehicleCommandPayload {
            linear_velocity_mps: linear,
            current_velocity_mps: linear,
            delta_time_s: 0.1,
            steering_angle_deg: 1.0,
            current_steering_angle_deg: 1.0,
        }
    }

    fn service(tag: &str) -> String {
        // Per-test service names so parallel tests never share a channel.
        format!("kirra/iceoryx2-inline/{tag}/{}", std::process::id())
    }

    // WP21B-01: an in-envelope proposal published over REAL iceoryx2 shared
    // memory releases at the actuator with the full verified chain intact.
    #[test]
    fn valid_proposal_releases_over_iceoryx2() {
        let ch = FrozenChannel::create(&service("valid")).expect("channel");
        let (mut gov, mut act) = stations();
        let released = govern_over_channel(
            &ch,
            &mut gov,
            &mut act,
            payload(10.0).to_view(GEN, 1, 0, FUTURE_DEADLINE),
            0,
        )
        .expect("transport")
        .expect("an in-envelope proposal must release");
        assert_eq!(released.sequence, 1);
        assert_eq!(released.command.linear_velocity_mps, 10.0);
        assert_eq!(act.last_released(), Some(1));
    }

    // WP21B-02: an over-envelope proposal releases the CLAMPED bytes — the
    // token binds the enforced command, exactly as on the SHM carrier.
    #[test]
    fn over_envelope_releases_clamped_bytes_over_iceoryx2() {
        let ch = FrozenChannel::create(&service("clamp")).expect("channel");
        let (mut gov, mut act) = stations();
        let ceiling =
            VehicleKinematicsContract::nominal_reference_profile().effective_max_speed_mps();
        let released = govern_over_channel(
            &ch,
            &mut gov,
            &mut act,
            payload(50.0).to_view(GEN, 1, 0, FUTURE_DEADLINE),
            0,
        )
        .expect("transport")
        .expect("a clampable proposal is actuatable");
        assert!(released.command.linear_velocity_mps <= ceiling);
        assert!(released.command.linear_velocity_mps > 0.0);
    }

    // WP21B-03: a replayed sequence received over the channel is refused —
    // the governor watermark burned it, so no token exists the second time.
    #[test]
    fn replayed_sequence_is_refused_over_iceoryx2() {
        let ch = FrozenChannel::create(&service("replay")).expect("channel");
        let (mut gov, mut act) = stations();
        let view = payload(10.0).to_view(GEN, 7, 0, FUTURE_DEADLINE);
        govern_over_channel(&ch, &mut gov, &mut act, view, 0)
            .expect("transport")
            .expect("first release");
        let second = govern_over_channel(&ch, &mut gov, &mut act, view, 0).expect("transport");
        assert_eq!(
            second,
            Err(ReleaseRefusal::NoToken),
            "replay must not re-release"
        );
        assert_eq!(
            act.last_released(),
            Some(7),
            "watermark untouched by the replay"
        );
    }

    // WP21B-04: a corrupted frame (CRC broken in flight-shape) received over
    // the channel never releases — the production validate refuses it before
    // any kinematics run.
    #[test]
    fn corrupted_crc_never_releases_over_iceoryx2() {
        let ch = FrozenChannel::create(&service("crc")).expect("channel");
        let (mut gov, mut act) = stations();
        let mut view = payload(10.0).to_view(GEN, 1, 0, FUTURE_DEADLINE);
        view.crc32 ^= 0xDEAD_BEEF;
        let outcome = govern_over_channel(&ch, &mut gov, &mut act, view, 0).expect("transport");
        assert_eq!(outcome, Err(ReleaseRefusal::NoToken));
        assert_eq!(act.last_released(), None, "no release ever happened");
    }

    // WP21B-05: a NaN command received over the channel is denied by the
    // talisman (SG9 fail-closed) — transport-valid but kinematically poisoned.
    #[test]
    fn nan_command_is_denied_over_iceoryx2() {
        let ch = FrozenChannel::create(&service("nan")).expect("channel");
        let (mut gov, mut act) = stations();
        let outcome = govern_over_channel(
            &ch,
            &mut gov,
            &mut act,
            payload(f64::NAN).to_view(GEN, 1, 0, FUTURE_DEADLINE),
            0,
        )
        .expect("transport");
        assert_eq!(outcome, Err(ReleaseRefusal::NoToken));
        assert_eq!(act.last_released(), None);
    }
}
