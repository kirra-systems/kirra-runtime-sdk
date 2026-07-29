"""Host tests for cycle_events — the pure emission half of #1215.

The property under test: each builder emits only facts the witness can
directly attest (None on anything else), and the writer's failure can never
reach the control path (fail-soft, latching, off by default).

The wire shape's authority is the Rust crate (`kirra-cycle-record`); the
round-trip test here encodes a payload byte-for-byte per
`ros_bound_command.rs::encode` so the Python decoder cannot drift from the
signed layout without this failing.
"""

import json
import struct
import unittest

from kirra_safety.cycle_events import (
    CycleEventWriter,
    perception_event,
    proposal_event,
    release_event_from_payload_hex,
)


def taj_response(**overrides):
    base = {
        'frame_id': {
            'scan_sequence': 7,
            'camera_sequence': 70,
            'tracker_generation': 1,
            'evidence_digest': 'aa' * 32,
        },
        'raw_speed_cap_mps': 0.9,
        'stabilized_speed_cap_mps': 0.8,
        'objects': [{}, {}, {}],
        'camera_healthy': True,
    }
    base.update(overrides)
    return base


def plan_response(**overrides):
    base = {
        'perception_frame_id': {
            'scan_sequence': 7,
            'evidence_digest': 'aa' * 32,
        },
        'proposal_digest': 'bb' * 32,
        'verdict': 'Accept',
        'admitted': True,
        'reason_code': None,
    }
    base.update(overrides)
    return base


def encode_payload(sequence=100, nonce=700, issued=1000, expires=2007,
                   linear=0.4, angular=0.0, scan=7, camera=70,
                   tracker=1):
    """Byte-for-byte mirror of ros_bound_command.rs::encode."""
    out = bytearray(176)
    struct.pack_into('<4Q', out, 0, sequence, nonce, issued, expires)
    struct.pack_into('<2d', out, 32, linear, angular)
    struct.pack_into('<Q', out, 48, scan)
    if camera is not None:
        out[56] = 1
        struct.pack_into('<Q', out, 64, camera)
    struct.pack_into('<Q', out, 72, tracker)
    out[80:112] = bytes.fromhex('cc' * 32)   # profile digest
    out[112:144] = bytes.fromhex('aa' * 32)  # evidence digest
    out[144:176] = bytes.fromhex('bb' * 32)  # proposal digest
    return bytes(out).hex()


class PerceptionBuilder(unittest.TestCase):
    def test_a_full_response_builds_the_event(self):
        ev = perception_event(taj_response(), 7, 1007, 12)
        self.assertEqual(ev['stage'], 'perception')
        self.assertEqual(ev['scan_sequence'], 7)
        self.assertEqual(ev['evidence_digest'], 'aa' * 32)
        self.assertEqual(ev['raw_speed_cap_mps'], 0.9)
        self.assertEqual(ev['stabilized_speed_cap_mps'], 0.8)
        self.assertEqual(ev['object_count'], 3)

    def test_a_response_missing_identity_or_caps_builds_nothing(self):
        # Only facts the witness can attest: no frame id, no event; no cap
        # pair, no event. A partial event would join as false evidence.
        self.assertIsNone(perception_event({'objects': []}, 7, 0, 0))
        broken = taj_response()
        del broken['stabilized_speed_cap_mps']
        self.assertIsNone(perception_event(broken, 7, 0, 0))
        self.assertIsNone(perception_event('not a dict', 7, 0, 0))


class ProposalBuilder(unittest.TestCase):
    def test_the_scan_comes_from_the_planner_echo(self):
        # The echo is what the planner actually bound; a request-side value
        # would attest something this witness did not observe accepted.
        plan = plan_response()
        plan['perception_frame_id']['scan_sequence'] = 9
        ev = proposal_event(plan, 1027, 25)
        self.assertEqual(ev['scan_sequence'], 9)
        self.assertEqual(ev['proposal_digest'], 'bb' * 32)
        self.assertTrue(ev['admitted'])

    def test_a_refusal_carries_its_reason_code(self):
        plan = plan_response(verdict='MRCFallback', admitted=False,
                             reason_code='TRAJECTORY_CORRIDOR_BREACH')
        ev = proposal_event(plan, 0, 0)
        self.assertFalse(ev['admitted'])
        self.assertEqual(ev['reason_code'], 'TRAJECTORY_CORRIDOR_BREACH')

    def test_a_response_without_echo_or_digest_builds_nothing(self):
        plan = plan_response()
        del plan['perception_frame_id']
        self.assertIsNone(proposal_event(plan, 0, 0))
        plan = plan_response(proposal_digest=None)
        self.assertIsNone(proposal_event(plan, 0, 0))


class ReleaseDecoder(unittest.TestCase):
    def test_the_decoder_round_trips_the_rust_layout(self):
        ev = release_event_from_payload_hex(encode_payload(), 1047, 8)
        self.assertEqual(ev['stage'], 'release')
        self.assertEqual(ev['release_sequence'], 100)
        self.assertEqual(ev['release_nonce'], 700)
        self.assertEqual(ev['scan_sequence'], 7)
        self.assertEqual(ev['linear_mps'], 0.4)
        self.assertEqual(ev['expires_at_ms'], 2007)
        self.assertEqual(ev['evidence_digest'], 'aa' * 32)
        self.assertEqual(ev['proposal_digest'], 'bb' * 32)

    def test_anything_but_exactly_176_valid_hex_bytes_decodes_to_nothing(self):
        # A best-effort decode of a truncated payload would attest fields
        # that were never signed.
        full = encode_payload()
        self.assertIsNone(release_event_from_payload_hex(full[:-2], 0, 0))
        self.assertIsNone(release_event_from_payload_hex(full + 'ff', 0, 0))
        self.assertIsNone(release_event_from_payload_hex('zz' + full[2:], 0, 0))
        self.assertIsNone(release_event_from_payload_hex(None, 0, 0))


class Writer(unittest.TestCase):
    def test_off_by_default_and_none_events_are_no_ops(self):
        w = CycleEventWriter('')
        self.assertFalse(w.enabled)
        w.emit({'stage': 'perception'})  # must not raise, must not write
        w2 = CycleEventWriter(None)
        self.assertFalse(w2.enabled)

    def test_events_append_as_jsonl(self):
        import tempfile, os
        path = os.path.join(tempfile.mkdtemp(), 'cycle.jsonl')
        w = CycleEventWriter(path)
        ev = perception_event(taj_response(), 7, 1007, 12)
        w.emit(ev)
        w.emit(None)  # builder refusal: a counted no-op
        w.emit(proposal_event(plan_response(), 1027, 25))
        with open(path, encoding='utf-8') as f:
            lines = [json.loads(l) for l in f.read().splitlines()]
        self.assertEqual(len(lines), 2)
        self.assertEqual(lines[0]['stage'], 'perception')
        self.assertEqual(lines[1]['stage'], 'proposal')

    def test_an_io_failure_latches_off_and_never_raises(self):
        # A directory path cannot be opened for append: the first emit fails,
        # warns ONCE, latches off, and every later emit is a silent no-op.
        warnings = []
        w = CycleEventWriter('/', warn=warnings.append)
        self.assertTrue(w.enabled)
        w.emit({'stage': 'perception'})  # must not raise
        self.assertFalse(w.enabled)
        self.assertEqual(len(warnings), 1)
        self.assertIn('control behavior unaffected', warnings[0])
        w.emit({'stage': 'proposal'})  # latched: no second warning
        self.assertEqual(len(warnings), 1)


if __name__ == '__main__':
    unittest.main()
