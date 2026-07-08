//! EP-01 — the in-line SHM enforcement path, assembled (G-1's software half).
//!
//! The two stations of the in-line loop:
//!
//! - [`GovernorStation`] — the CHECKER's per-cycle step: coherent seqlock read →
//!   the fail-closed transport/codec/kinematic pipeline
//!   (`kirra_core::contract_consumer::decide_cycle`) → on an actuatable verdict,
//!   mint the release token over the ENFORCED bytes (`view_to_sign` guarantees a
//!   denied command is never signable, and a clamped one is signed post-clamp).
//! - [`ActuatorStation`] — the verify-before-release gate (ADR-0031): no token →
//!   no release; a token that does not verify over EXACTLY the presented bytes →
//!   no release; a sequence that does not strictly advance past the last
//!   released one → no release (the kernel `<=`-is-replay rule, enforced again
//!   at the final authority boundary); an undecodable payload → no release.
//!   Only a fully released command advances the actuator's watermark — a
//!   refusal never poisons it.
//!
//! No HTTP anywhere: the planner publishes into the shared region, the governor
//! bounds in-line, the actuator releases on proof. The carrier is any
//! [`ContractReader`] — `InProcessRegion` for deterministic tests,
//! `kirra-hv-carrier`'s `PosixShmRegion` across real process boundaries (see
//! `tests/cross_process.rs` and the `inline_demo` bin), and the QNX
//! hypervisor-mapped region on target (the remaining step; see README).

#![forbid(unsafe_code)]

use ed25519_dalek::{SigningKey, VerifyingKey};

use kirra_contract_channel::{
    CommandCodecError, ContractReader, GovernorContractView, VehicleCommandPayload,
    MAX_SNAPSHOT_RETRIES,
};
use kirra_core::contract_consumer::{decide_cycle, GovernorCycle, GovernorOutcome};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_release_token::{issue_release_token, verify_release, ReleaseDenied, ReleaseToken};

/// One governor cycle's output: the full [`GovernorCycle`] (outcome + view) plus
/// the release token — minted **iff** the cycle produced a signable view
/// (`view_to_sign`, i.e. an actuatable verdict over the enforced bytes).
#[derive(Clone, Debug, PartialEq)]
pub struct InlineCycle {
    pub cycle: GovernorCycle,
    pub token: Option<ReleaseToken>,
}

impl InlineCycle {
    /// The (view, token) pair the actuator may be offered — `Some` iff the
    /// verdict was actuatable. The view is the ENFORCED view (post-clamp).
    #[must_use]
    pub fn releasable(&self) -> Option<(&GovernorContractView, &ReleaseToken)> {
        match (self.cycle.view_to_sign(), &self.token) {
            (Some(view), Some(token)) => Some((view, token)),
            _ => None,
        }
    }
}

/// The governor's side of the in-line loop: the monotonic accepted-watermark,
/// the per-class kinematic contract (the talisman), the signing identity, and
/// the seqlock retry budget. One station per contract region.
pub struct GovernorStation {
    watermark: kirra_contract_channel::AcceptedWatermark,
    contract: VehicleKinematicsContract,
    signing_key: SigningKey,
    max_retries: u32,
}

impl GovernorStation {
    #[must_use]
    pub fn new(contract: VehicleKinematicsContract, signing_key: SigningKey) -> Self {
        Self {
            watermark: kirra_contract_channel::AcceptedWatermark::new(),
            contract,
            signing_key,
            max_retries: MAX_SNAPSHOT_RETRIES,
        }
    }

    /// The verifying half of the governor identity — provisioned to the
    /// actuator out-of-band (constructor input for [`ActuatorStation`]).
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// One in-line cycle: read → validate → decode → bound → (sign iff
    /// actuatable). `now_nanos` MUST be in the boundary clock domain
    /// (HVCHAN-001 §5). The watermark advances exactly as in `decide_cycle` —
    /// a transport/codec fault never poisons it; a validly-received command
    /// burns its sequence even when kinematically denied.
    #[must_use]
    pub fn cycle<R: ContractReader>(&mut self, reader: &R, now_nanos: u64) -> InlineCycle {
        let cycle = decide_cycle(
            reader,
            &mut self.watermark,
            now_nanos,
            &self.contract,
            self.max_retries,
        );
        let token = cycle
            .view_to_sign()
            .map(|view| issue_release_token(view, &self.signing_key));
        InlineCycle { cycle, token }
    }
}

/// Why the actuator refused to release. Every variant is fail-closed: the
/// actuator holds its MRC (safe stop) and its release watermark is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseRefusal {
    /// The governor produced no token this cycle (a fault or a denied verdict)
    /// — there is nothing to prove, so there is nothing to release.
    NoToken,
    /// The token failed verification over the presented bytes (substitution /
    /// forgery / tamper — see [`ReleaseDenied`]).
    Denied(ReleaseDenied),
    /// The presented sequence does not STRICTLY advance past the last released
    /// one — a replayed or reordered release (the kernel `<=` rule, re-enforced
    /// at the final authority boundary).
    SequenceNotAdvanced { presented: u64, last_released: u64 },
    /// The (verified) payload failed to decode — fail-closed, never actuate
    /// bytes that don't parse to a command.
    Codec(CommandCodecError),
}

/// A command the actuator RELEASED: proof verified, sequence advanced, payload
/// decoded. This is the only type the drive layer accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReleasedCommand {
    pub sequence: u64,
    pub command: VehicleCommandPayload,
}

/// The actuator's side of the in-line loop: the trusted governor verifying key
/// plus the released-sequence watermark.
pub struct ActuatorStation {
    governor_vk: VerifyingKey,
    last_released: Option<u64>,
}

impl ActuatorStation {
    #[must_use]
    pub fn new(governor_vk: VerifyingKey) -> Self {
        Self { governor_vk, last_released: None }
    }

    /// The verify-before-release gate. Order matters and every step is
    /// fail-closed:
    /// 1. a token must exist (no proof → no release);
    /// 2. the token must verify over EXACTLY the presented view's canonical
    ///    bytes against the governor key (`verify_release` — digest match +
    ///    strict Ed25519);
    /// 3. the sequence must strictly advance past the last RELEASED sequence
    ///    (`<=` is a replay — mirrored from the kernel rule);
    /// 4. the payload must decode.
    ///
    /// Only after all four does the release watermark advance — a refusal at
    /// any step leaves it untouched, so a later legitimate release is never
    /// blocked by a rejected attempt.
    pub fn release(
        &mut self,
        view: &GovernorContractView,
        token: Option<&ReleaseToken>,
    ) -> Result<ReleasedCommand, ReleaseRefusal> {
        let token = token.ok_or(ReleaseRefusal::NoToken)?;
        verify_release(token, view, &self.governor_vk).map_err(ReleaseRefusal::Denied)?;
        if let Some(last) = self.last_released {
            if view.sequence <= last {
                return Err(ReleaseRefusal::SequenceNotAdvanced {
                    presented: view.sequence,
                    last_released: last,
                });
            }
        }
        let command =
            VehicleCommandPayload::from_validated_view(view).map_err(ReleaseRefusal::Codec)?;
        self.last_released = Some(view.sequence);
        Ok(ReleasedCommand { sequence: view.sequence, command })
    }

    /// The last released sequence (observability/tests).
    #[must_use]
    pub fn last_released(&self) -> Option<u64> {
        self.last_released
    }
}

/// The full in-line step, one call: govern the region's current proposal and —
/// on an actuatable, provable verdict — release it. Any refusal means the
/// actuator holds its MRC this cycle.
pub fn govern_and_release<R: ContractReader>(
    governor: &mut GovernorStation,
    actuator: &mut ActuatorStation,
    reader: &R,
    now_nanos: u64,
) -> Result<ReleasedCommand, ReleaseRefusal> {
    let inline = governor.cycle(reader, now_nanos);
    match inline.releasable() {
        Some((view, token)) => actuator.release(view, Some(token)),
        None => Err(ReleaseRefusal::NoToken),
    }
}

/// Convenience re-export: whether a cycle outcome is the safe stop (for demo /
/// telemetry call-sites that only need the branch).
#[must_use]
pub fn is_safe_stop(cycle: &InlineCycle) -> bool {
    matches!(cycle.cycle.outcome, GovernorOutcome::SafeStop)
}

// ---------------------------------------------------------------------------
// The FDIT fault matrix, run against the ASSEMBLED loop (writer → region →
// governor → actuator) over the deterministic in-process carrier. Each row
// injects one fault class and asserts NO RELEASE (and, for the happy/clamp
// rows, exactly the right release). Mirrors the qnx-rtm-harness matrix at the
// assembly level: memory faults were proven at the shim, contract faults at
// the consumer — these rows prove the COMPOSITION refuses end to end.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fdit_matrix {
    use super::*;
    use kirra_contract_channel::reference::InProcessRegion;
    use kirra_contract_channel::{publish, ContractWriter};

    const FUTURE_DEADLINE: u64 = u64::MAX / 2;

    fn governor_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }

    fn stations() -> (GovernorStation, ActuatorStation) {
        let gov = GovernorStation::new(
            VehicleKinematicsContract::nominal_reference_profile(),
            governor_key(),
        );
        let act = ActuatorStation::new(gov.verifying_key());
        (gov, act)
    }

    fn payload(linear: f64) -> VehicleCommandPayload {
        VehicleCommandPayload {
            linear_velocity_mps: linear,
            current_velocity_mps: linear, // accel 0 → only the speed bound decides
            delta_time_s: 0.1,
            steering_angle_deg: 1.0,
            current_steering_angle_deg: 1.0,
        }
    }

    /// Publish `view` on top of the region's committed generation.
    fn publish_view(region: &InProcessRegion, view: &GovernorContractView) {
        publish(region, region.load_generation(), view);
    }

    fn publish_payload(region: &InProcessRegion, p: &VehicleCommandPayload, seq: u64) {
        let gen = region.load_generation();
        publish_view(region, &p.to_view(gen, seq, 0, FUTURE_DEADLINE));
    }

    // ---- FDIT-01: valid proposal → released, decoded intact ----------------
    #[test]
    fn valid_proposal_is_released_with_verified_token() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 1);

        let released = govern_and_release(&mut gov, &mut act, &region, 0)
            .expect("an in-envelope proposal must release");
        assert_eq!(released.sequence, 1);
        assert_eq!(released.command.linear_velocity_mps, 10.0);
        assert_eq!(act.last_released(), Some(1));
    }

    // ---- FDIT-02: over-envelope → clamped; the token binds the ENFORCED bytes
    #[test]
    fn over_envelope_releases_the_clamped_bytes_only() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        let raw = payload(50.0); // over the 35 m/s nominal ceiling
        publish_payload(&region, &raw, 1);
        let raw_view = raw.to_view(2, 1, 0, FUTURE_DEADLINE); // what the guest proposed

        let inline = gov.cycle(&region, 0);
        let (view, token) = inline.releasable().expect("a clampable proposal is actuatable");
        let ceiling = VehicleKinematicsContract::nominal_reference_profile()
            .effective_max_speed_mps();

        // The signed view is the ENFORCED (clamped) command…
        let released = act.release(view, Some(token)).expect("clamped release");
        assert!(
            released.command.linear_velocity_mps <= ceiling,
            "released speed {} must be inside the envelope {ceiling}",
            released.command.linear_velocity_mps
        );
        // …and the SAME token must NOT release the guest's raw proposal.
        let mut fresh_actuator = ActuatorStation::new(gov.verifying_key());
        assert_eq!(
            fresh_actuator.release(&raw_view, Some(token)),
            Err(ReleaseRefusal::Denied(ReleaseDenied::DigestMismatch)),
            "a token over the clamped bytes must never release the raw proposal"
        );
    }

    // ---- FDIT-03: CRC corruption → no release ------------------------------
    #[test]
    fn crc_corruption_never_releases() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        let gen = region.load_generation();
        let mut view = payload(10.0).to_view(gen, 1, 0, FUTURE_DEADLINE);
        view.crc32 ^= 0x1; // flip one CRC bit after encoding
        publish_view(&region, &view);

        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 0),
            Err(ReleaseRefusal::NoToken),
            "a CRC-corrupted snapshot must produce no token and no release"
        );
        assert_eq!(act.last_released(), None);
    }

    // ---- FDIT-04: bounds violation (command_len over MAX) → no release ------
    #[test]
    fn oversized_command_len_never_releases() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        let gen = region.load_generation();
        let mut view = payload(10.0).to_view(gen, 1, 0, FUTURE_DEADLINE);
        view.command_len = (kirra_contract_channel::MAX_COMMAND_BYTES as u32) + 1;
        publish_view(&region, &view);

        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 0),
            Err(ReleaseRefusal::NoToken),
            "an out-of-bounds command_len must fail closed"
        );
    }

    // ---- FDIT-05: header tear (odd generation) → no release -----------------
    #[test]
    fn torn_write_never_releases() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 1);
        // Simulate a writer dying mid-publish: the seqlock generation is left ODD.
        region.store_generation(region.load_generation() + 1);

        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 0),
            Err(ReleaseRefusal::NoToken),
            "a torn (odd-generation) region must fail closed"
        );
    }

    // ---- FDIT-06: replay (same sequence) → second cycle refuses -------------
    #[test]
    fn replayed_sequence_never_releases_twice() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 7);
        govern_and_release(&mut gov, &mut act, &region, 0).expect("first release");

        // The guest re-publishes the SAME sequence (equal = replay).
        publish_payload(&region, &payload(10.0), 7);
        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 0),
            Err(ReleaseRefusal::NoToken),
            "an equal sequence is a replay — the governor must refuse it"
        );
        assert_eq!(act.last_released(), Some(7), "the replay must not advance the watermark");
    }

    // ---- FDIT-07: sequence regress → no release ------------------------------
    #[test]
    fn regressed_sequence_never_releases() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 9);
        govern_and_release(&mut gov, &mut act, &region, 0).expect("first release");

        publish_payload(&region, &payload(10.0), 3); // strictly below the watermark
        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 0),
            Err(ReleaseRefusal::NoToken)
        );
    }

    // ---- FDIT-08: expired deadline → no release ------------------------------
    #[test]
    fn expired_deadline_never_releases() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        let gen = region.load_generation();
        publish_view(&region, &payload(10.0).to_view(gen, 1, 0, 1_000));

        assert_eq!(
            govern_and_release(&mut gov, &mut act, &region, 5_000),
            Err(ReleaseRefusal::NoToken),
            "a stale proposal must fail closed at the boundary clock"
        );
    }

    // ---- FDIT-09: forged token (wrong key) → actuator refuses ----------------
    #[test]
    fn forged_token_is_refused_at_the_actuator() {
        let region = InProcessRegion::new();
        let (mut gov, _) = stations();
        publish_payload(&region, &payload(10.0), 1);
        let inline = gov.cycle(&region, 0);
        let (view, _) = inline.releasable().expect("actuatable");

        // An attacker signs the same bytes with THEIR key.
        let forged = issue_release_token(view, &SigningKey::from_bytes(&[99u8; 32]));
        let mut act = ActuatorStation::new(gov.verifying_key());
        assert_eq!(
            act.release(view, Some(&forged)),
            Err(ReleaseRefusal::Denied(ReleaseDenied::SignatureInvalid)),
            "a token signed by any key but the governor's must be refused"
        );
        assert_eq!(act.last_released(), None);
    }

    // ---- FDIT-10: token/view substitution → refused ---------------------------
    #[test]
    fn token_for_one_command_never_releases_another() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 1);
        let first = gov.cycle(&region, 0);
        let (_, token_one) = first.releasable().expect("actuatable");
        let token_one = *token_one;

        publish_payload(&region, &payload(20.0), 2);
        let second = gov.cycle(&region, 0);
        let (view_two, _) = second.releasable().expect("actuatable");

        assert_eq!(
            act.release(view_two, Some(&token_one)),
            Err(ReleaseRefusal::Denied(ReleaseDenied::DigestMismatch)),
            "cycle 1's approval must never release cycle 2's bytes"
        );
    }

    // ---- FDIT-11: release replay at the ACTUATOR → refused --------------------
    #[test]
    fn a_released_command_cannot_be_released_again() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();
        publish_payload(&region, &payload(10.0), 4);
        let inline = gov.cycle(&region, 0);
        let (view, token) = inline.releasable().expect("actuatable");

        act.release(view, Some(token)).expect("first release");
        assert_eq!(
            act.release(view, Some(token)),
            Err(ReleaseRefusal::SequenceNotAdvanced { presented: 4, last_released: 4 }),
            "re-presenting an already-released (view, token) is a replay at the release point"
        );
    }

    // ---- FDIT-12: a fault cycle never poisons either watermark ----------------
    #[test]
    fn fault_cycles_leave_both_watermarks_clean() {
        let region = InProcessRegion::new();
        let (mut gov, mut act) = stations();

        // A CRC fault first…
        let gen = region.load_generation();
        let mut bad = payload(10.0).to_view(gen, 5, 0, FUTURE_DEADLINE);
        bad.crc32 ^= 0xFF;
        publish_view(&region, &bad);
        assert!(govern_and_release(&mut gov, &mut act, &region, 0).is_err());

        // …must not block the SAME sequence arriving validly afterwards.
        publish_payload(&region, &payload(10.0), 5);
        let released = govern_and_release(&mut gov, &mut act, &region, 0)
            .expect("a rejected snapshot must not burn its sequence");
        assert_eq!(released.sequence, 5);
    }
}
