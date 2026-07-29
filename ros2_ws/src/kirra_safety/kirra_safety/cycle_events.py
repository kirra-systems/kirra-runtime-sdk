#!/usr/bin/env python3
"""#1215 cycle-record stage-event emission — the pure half.

No ROS, no requests, no clocks: every function takes explicit values and
returns a dict shaped exactly like `kirra-cycle-record`'s `StageEvent` wire
format (the Rust crate's serde JSONL is the contract; its tests own the shape,
these mirror it).

THE PROPERTY THIS MODULE EXISTS TO KEEP TRUE
--------------------------------------------
Each process emits only facts it can directly attest, using identifiers
already carried by the authority chain, and disabling emission does not alter
control behavior.

Concretely:
  * Builders return None rather than guessing when a response lacks the fields
    an event needs. An event with an invented digest is worse than no event —
    the joiner would happily chain it, and the artifact would lie.
  * `CycleEventWriter` is FAIL-SOFT AND LATCHING: any I/O error disables
    further emission for the process lifetime and the control path proceeds
    untouched. The recorder degrades, never the loop. This is the exact
    opposite polarity from every safety module in this package, and it is
    correct here for the same reason those are fail-closed: authority and
    observability must fail in opposite directions.
  * Emission is OFF unless a path is configured. The default deployment is
    byte-identical.

THE RELEASE PAYLOAD DECODER
---------------------------
`release_event_from_payload_hex` decodes the 176-byte V2 payload the
interceptor already relays (`payload(176) || token(96)`, see
`enforcement_decision.release_frame`). This is READ-ONLY observability over
bytes the node already carries verbatim — the trust path remains the motor
consumer's Ed25519 verify over exactly these bytes, and nothing decoded here
feeds back into any decision. Layout mirrors
`kirra-release-token/src/ros_bound_command.rs::encode` (little-endian):

    [0..8)    sequence u64        [8..16)   nonce u64
    [16..24)  issued_at_ms u64    [24..32)  expires_at_ms u64
    [32..40)  linear_mps f64      [40..48)  angular_rad_s f64
    [48..56)  scan_sequence u64   [56]      camera flag (1 -> [64..72) u64)
    [72..80)  tracker_generation  [80..112) profile_digest
    [112..144) evidence_digest    [144..176) proposal_digest
"""

import json
import struct

RELEASE_PAYLOAD_LEN = 176


def perception_event(taj, scan_sequence, t_wall_ms, stage_duration_ms):
    """Build a perception stage event from a Taj `/perception` response.

    Returns None when the response lacks the identity or cap fields — a
    partial event would join as evidence the witness cannot actually attest.
    """
    if not isinstance(taj, dict):
        return None
    frame = taj.get('frame_id')
    if not isinstance(frame, dict):
        return None
    evidence = frame.get('evidence_digest')
    tracker = frame.get('tracker_generation')
    raw_cap = taj.get('raw_speed_cap_mps')
    stab_cap = taj.get('stabilized_speed_cap_mps')
    if not isinstance(evidence, str) or not isinstance(tracker, int):
        return None
    if not isinstance(raw_cap, (int, float)) or not isinstance(stab_cap, (int, float)):
        return None
    objects = taj.get('objects')
    return {
        'stage': 'perception',
        'scan_sequence': int(scan_sequence),
        'camera_sequence': frame.get('camera_sequence'),
        'tracker_generation': tracker,
        'evidence_digest': evidence,
        'raw_speed_cap_mps': float(raw_cap),
        'stabilized_speed_cap_mps': float(stab_cap),
        'object_count': len(objects) if isinstance(objects, list) else 0,
        'camera_healthy': taj.get('camera_healthy'),
        't_wall_ms': int(t_wall_ms),
        'stage_duration_ms': int(stage_duration_ms),
    }


def proposal_event(plan, t_wall_ms, stage_duration_ms):
    """Build a proposal stage event from a planner `/plan` response.

    The scan sequence is taken from the planner's ECHOED frame id — the echo
    is what the planner actually bound, and emitting the request's value
    instead would attest something this witness did not observe the planner
    accept.
    """
    if not isinstance(plan, dict):
        return None
    frame = plan.get('perception_frame_id')
    digest = plan.get('proposal_digest')
    verdict = plan.get('verdict')
    admitted = plan.get('admitted')
    if not isinstance(frame, dict) or not isinstance(digest, str):
        return None
    if not isinstance(verdict, str) or not isinstance(admitted, bool):
        return None
    scan = frame.get('scan_sequence')
    evidence = frame.get('evidence_digest')
    if not isinstance(scan, int) or not isinstance(evidence, str):
        return None
    return {
        'stage': 'proposal',
        'scan_sequence': scan,
        'proposal_digest': digest,
        'evidence_digest': evidence,
        'verdict': verdict,
        'admitted': admitted,
        'reason_code': plan.get('reason_code'),
        't_wall_ms': int(t_wall_ms),
        'stage_duration_ms': int(stage_duration_ms),
    }


def release_event_from_payload_hex(payload_hex, t_wall_ms, stage_duration_ms):
    """Decode a release stage event from the V2 payload hex the verifier's 200
    carries (the same bytes `release_frame` relays verbatim).

    Read-only observability; None on anything other than exactly 176 bytes of
    valid hex — a truncated payload decoded "best effort" would attest fields
    that were never signed.
    """
    if not isinstance(payload_hex, str):
        return None
    try:
        payload = bytes.fromhex(payload_hex)
    except ValueError:
        return None
    if len(payload) != RELEASE_PAYLOAD_LEN:
        return None
    (sequence, nonce, _issued_at_ms, expires_at_ms) = struct.unpack_from('<4Q', payload, 0)
    (linear_mps, angular_rad_s) = struct.unpack_from('<2d', payload, 32)
    (scan_sequence,) = struct.unpack_from('<Q', payload, 48)
    evidence_digest = payload[112:144].hex()
    proposal_digest = payload[144:176].hex()
    return {
        'stage': 'release',
        'scan_sequence': scan_sequence,
        'proposal_digest': proposal_digest,
        'evidence_digest': evidence_digest,
        'release_sequence': sequence,
        'release_nonce': nonce,
        'linear_mps': linear_mps,
        'angular_rad_s': angular_rad_s,
        'expires_at_ms': expires_at_ms,
        't_wall_ms': int(t_wall_ms),
        'stage_duration_ms': int(stage_duration_ms),
    }


class CycleEventWriter:
    """Append-only JSONL emitter. FAIL-SOFT AND LATCHING.

    Disabled when constructed with an empty/None path (the default
    deployment). Any I/O or serialization error PERMANENTLY disables further
    emission and is reported once via the supplied `warn` callable — the
    control loop must be structurally incapable of being disturbed by its own
    flight recorder, and a per-cycle retry against a full disk would trade
    that for a warn storm.
    """

    def __init__(self, path, warn=None):
        self._path = path or None
        self._warn = warn or (lambda msg: None)
        self._failed = False

    @property
    def enabled(self):
        return self._path is not None and not self._failed

    def emit(self, event):
        """Append one event. `None` events (builder refusals) are counted as
        no-ops — the builder already decided there was nothing attestable."""
        if event is None or not self.enabled:
            return
        try:
            line = json.dumps(event, separators=(',', ':'))
            with open(self._path, 'a', encoding='utf-8') as f:
                f.write(line + '\n')
        except Exception as e:  # noqa: BLE001 — fail-soft is the contract here
            self._failed = True
            self._warn(
                f'cycle-record emission disabled after error: {type(e).__name__}: {e} '
                f'(control behavior unaffected; recording stops for this process)'
            )
