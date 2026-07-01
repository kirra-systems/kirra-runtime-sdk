// crates/kirra-ros2-adapter/src/contract_producer.rs
//
// L3.2 — the GUEST-side producer binding: turn the doer's proposed control
// command into published `GovernorContractView`s over the frozen Clause-2
// contract channel (ADR-0006 / HVCHAN-001).
//
// In the ROS2/QNX guest split the ROS adapter (the untrusted DOER side) does
// NOT decide safety; it PROPOSES. This module maps the fast-loop ingress command
// (`IngressControlCommand`, from `control_ingress.rs`) to the frozen wire payload
// (`kirra_contract_channel::VehicleCommandPayload`, L3.1) and publishes it via a
// `ContractWriter` using the odd/even seqlock. The CHECKER (governor, ASIL, QNX
// partition) reads the region, `validate()`s the contract, `decode()`s the
// payload, and bounds it with `validate_vehicle_command` (L3.3). Nothing here is
// trusted for safety — the producer faithfully forwards what the doer proposed,
// including a fail-closed sentinel; the governor is the sole authority.
//
// Kept OUTSIDE `node.rs` (like `control_ingress.rs`) so the producer contract is
// unit-tested on host — over `kirra_contract_channel::reference::InProcessRegion`
// — without a sourced ROS 2 / r2r toolchain. The ros2-gated `node.rs` fast-loop
// call-site and the hypervisor-mapped `ContractWriter` binding land in L3.3.

#![cfg_attr(not(feature = "ros2"), allow(dead_code))]

use kirra_contract_channel::{publish, ContractWriter, VehicleCommandPayload};

use crate::control_ingress::IngressControlCommand;

/// Map a proposed fast-loop control command to the frozen wire payload.
///
/// The ingress command carries only the DOER's desired `linear_velocity_mps`
/// (m/s) and `steering_angle_rad` (**radians**); the checker's command form
/// (`VehicleCommandPayload`, mirroring `ProposedVehicleCommand`) also needs the
/// step's *actual* start state and duration. Those come from the governor-side
/// context the caller holds (ego odometry + the fast-loop period), passed in:
///
/// - `current_velocity_mps` — actual forward velocity at start of the step (m/s)
/// - `current_steering_angle_deg` — actual steering at start of the step (deg)
/// - `delta_time_s` — planning step duration (s)
///
/// Steering is converted **rad → deg** here (the wire schema is degrees, ISO
/// 8855, +left). No clamping or validation: the producer forwards the proposal
/// faithfully; the governor bounds it (`validate_vehicle_command`). A finite
/// ingress command (guaranteed by `parse_control_command_json`) plus finite
/// context yields a finite payload; any non-finite value is still caught by the
/// consumer's fail-closed `decode` (defense in depth).
pub fn proposal_payload(
    cmd: &IngressControlCommand,
    current_velocity_mps: f64,
    current_steering_angle_deg: f64,
    delta_time_s: f64,
) -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: cmd.linear_velocity_mps,
        current_velocity_mps,
        delta_time_s,
        steering_angle_deg: cmd.steering_angle_rad.to_degrees(),
        current_steering_angle_deg,
    }
}

/// The guest-side publish counters: the last committed (even) seqlock
/// `generation` and the next monotonic command `sequence`. Owns no region — the
/// [`ContractWriter`] (an `InProcessRegion` in tests, the hypervisor-mapped
/// region on target) is passed to [`publish_to`](Self::publish_to) — so a single
/// shared region can be borrowed by both this producer and the governor reader.
///
/// Sequence and generation both advance only on a successful publish, so the
/// governor's `AcceptedWatermark` (`<= last_accepted ⇒ reject`) sees a strictly
/// increasing stream and rejects any replay/regress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalSequencer {
    committed_generation: u64,
    next_sequence: u64,
}

impl ProposalSequencer {
    /// A fresh producer: committed generation 0 (the region's quiescent even
    /// value) and the first command sequence 1.
    pub fn new() -> Self {
        Self { committed_generation: 0, next_sequence: 1 }
    }

    /// The last committed (even) seqlock generation.
    pub fn committed_generation(&self) -> u64 {
        self.committed_generation
    }

    /// The sequence the next [`publish_to`](Self::publish_to) will stamp.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Publish one proposal into `writer` via the odd/even seqlock, stamping the
    /// next monotonic sequence and current committed generation. Returns the new
    /// committed (even) generation and advances both counters.
    ///
    /// `publication_nanos` / `deadline_nanos` are in the **boundary clock domain**
    /// (HVCHAN-001 §5): the guest must convert from its own clock before calling
    /// (`AOU-TIMESYNC-001`). This never fails — the payload always fits
    /// `MAX_COMMAND_BYTES` (freeze-asserted in `kirra-contract-channel`).
    pub fn publish_to<W: ContractWriter>(
        &mut self,
        writer: &W,
        payload: &VehicleCommandPayload,
        publication_nanos: u64,
        deadline_nanos: u64,
    ) -> u64 {
        let body = payload.to_view(
            self.committed_generation,
            self.next_sequence,
            publication_nanos,
            deadline_nanos,
        );
        let committed = publish(writer, self.committed_generation, &body);
        self.committed_generation = committed;
        self.next_sequence += 1;
        committed
    }
}

impl Default for ProposalSequencer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_contract_channel::reference::InProcessRegion;
    use kirra_contract_channel::{
        read_coherent_snapshot, validate, AcceptedWatermark, ContractFault, MAX_SNAPSHOT_RETRIES,
    };

    fn ingress(linear_velocity_mps: f64, steering_angle_rad: f64) -> IngressControlCommand {
        IngressControlCommand {
            asset_id: "ego".to_string(),
            linear_velocity_mps,
            steering_angle_rad,
            stamp_ms: 0,
        }
    }

    #[test]
    fn proposal_payload_maps_fields_and_converts_steering_to_degrees() {
        let cmd = ingress(4.5, 0.125);
        let p = proposal_payload(&cmd, 4.0, 7.0, 0.1);
        assert_eq!(p.linear_velocity_mps, 4.5);
        assert_eq!(p.current_velocity_mps, 4.0);
        assert_eq!(p.delta_time_s, 0.1);
        // radians -> degrees (the wire schema is degrees).
        assert_eq!(p.steering_angle_deg, 0.125_f64.to_degrees());
        assert_eq!(p.current_steering_angle_deg, 7.0);
    }

    #[test]
    fn publish_then_governor_reads_back_the_exact_command() {
        // The full L3 producer->consumer path over the in-process region.
        let region = InProcessRegion::new();
        let mut seq = ProposalSequencer::new();
        let payload = proposal_payload(&ingress(3.0, -0.25), 2.5, -14.0, 0.05);

        let committed = seq.publish_to(&region, &payload, 1_000, 10_000);
        assert_eq!(committed, 2); // 0 -> odd 1 -> even 2
        assert_eq!(seq.committed_generation(), 2);
        assert_eq!(seq.next_sequence(), 2);

        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        let mut wm = AcceptedWatermark::new();
        validate(&snap, 5_000, &wm).expect("fresh proposal validates");
        wm.record(&snap);
        assert_eq!(snap.sequence, 1);
        assert_eq!(VehicleCommandPayload::from_validated_view(&snap), Ok(payload));
    }

    #[test]
    fn successive_publishes_are_monotonic_and_accepted_in_order() {
        let region = InProcessRegion::new();
        let mut seq = ProposalSequencer::new();
        let mut wm = AcceptedWatermark::new();

        for i in 1..=3u64 {
            let payload = proposal_payload(&ingress(i as f64, 0.0), 0.0, 0.0, 0.1);
            let committed = seq.publish_to(&region, &payload, 0, 10_000);
            assert_eq!(committed, 2 * i); // even, strictly increasing
            let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
            assert_eq!(snap.sequence, i);
            validate(&snap, 5_000, &wm).expect("each proposal validates in order");
            wm.record(&snap);
            assert_eq!(VehicleCommandPayload::from_validated_view(&snap), Ok(payload));
        }
    }

    #[test]
    fn a_stale_snapshot_is_rejected_by_the_watermark() {
        // Capture a snapshot, advance the producer, then re-validating the OLD
        // snapshot against the advanced watermark must reject (replay/regress).
        let region = InProcessRegion::new();
        let mut seq = ProposalSequencer::new();
        let mut wm = AcceptedWatermark::new();

        let p1 = proposal_payload(&ingress(1.0, 0.0), 0.0, 0.0, 0.1);
        seq.publish_to(&region, &p1, 0, 10_000);
        let first = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        validate(&first, 5_000, &wm).expect("first accepts");
        wm.record(&first);

        let p2 = proposal_payload(&ingress(2.0, 0.0), 0.0, 0.0, 0.1);
        seq.publish_to(&region, &p2, 0, 10_000);
        let _second = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();

        // Re-presenting `first` (sequence 1) after sequence 1 was accepted is a replay.
        assert_eq!(
            validate(&first, 5_000, &wm),
            Err(ContractFault::SequenceRegressOrReplay { found: 1, last_accepted: 1 })
        );
    }

    #[test]
    fn over_envelope_but_finite_command_is_forwarded_for_the_governor_to_reject() {
        // The producer does NOT bound: an over-envelope but FINITE proposal is
        // forwarded and decodes fine; the ENVELOPE rejection is the governor's
        // job (validate_vehicle_command), not the transport codec's.
        let region = InProcessRegion::new();
        let mut seq = ProposalSequencer::new();

        // 999 m/s and 30° steering: absurd, far over any class envelope, finite.
        let payload = proposal_payload(&ingress(999.0, 30.0_f64.to_radians()), 0.0, 0.0, 0.1);

        seq.publish_to(&region, &payload, 0, 10_000);
        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        let wm = AcceptedWatermark::new();
        validate(&snap, 5_000, &wm).expect("transport contract is valid regardless of envelope");
        // decode succeeds (finite); the huge values are the governor's to reject.
        let decoded = VehicleCommandPayload::from_validated_view(&snap).expect("decodes");
        assert_eq!(decoded.linear_velocity_mps, 999.0);
        assert!((decoded.steering_angle_deg - 30.0).abs() < 1e-9);
    }

    #[test]
    fn a_conversion_that_overflows_to_inf_is_refused_by_decode() {
        // The fast loop's fail-closed sentinel sets steering to f64::MAX RADIANS;
        // rad->deg (`* 180/pi`) overflows to +Inf. The producer does not guard
        // this — the codec's fail-closed finiteness check is the backstop, so the
        // governor never receives a non-finite command (defense in depth).
        use crate::control_ingress::fail_closed_control_command;
        let sentinel = fail_closed_control_command("ego", 0);
        let payload = proposal_payload(&sentinel, 0.0, 0.0, 0.1);
        assert!(payload.steering_angle_deg.is_infinite()); // f64::MAX.to_degrees() overflows

        let region = InProcessRegion::new();
        let mut seq = ProposalSequencer::new();
        seq.publish_to(&region, &payload, 0, 10_000);
        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
        let wm = AcceptedWatermark::new();
        // The transport contract itself is intact (CRC/bounds/etc. all pass)...
        validate(&snap, 5_000, &wm).expect("transport contract valid");
        // ...but decode fails closed on the non-finite steering field.
        assert_eq!(
            VehicleCommandPayload::from_validated_view(&snap),
            Err(kirra_contract_channel::CommandCodecError::NonFinite {
                field: kirra_contract_channel::CommandField::SteeringAngleDeg,
            })
        );
    }
}
