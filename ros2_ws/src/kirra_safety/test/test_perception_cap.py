"""
Unit tests for the pure, fail-closed perception-cap clamp.

No ROS / rclpy needed — imports the pure `perception_cap` module directly. The
load-bearing cases are the fail-closed ones: a missing/stale/malformed cap must
STOP the robot (0.0), never let the proposed command through unchanged.

Run:  pytest ros2_ws/src/kirra_safety/test/test_perception_cap.py
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "kirra_safety"))
from perception_cap import (  # noqa: E402
    apply_perception_cap,
    resolve_stabilized_cap,
    STABILIZED_OK,
    STABILIZED_MALFORMED,
    STABILIZED_UNSTABILIZED,
    DISABLED,
    STALE,
    INVALID,
    CAPPED,
    PASS,
)

STALE_S = 0.5


def test_disabled_is_a_pure_passthrough():
    # Opt-in: when off, the proposed command is untouched (byte-identical prior path).
    v, reason = apply_perception_cap(1.8, cap_mps=0.0, cap_age_s=0.0, enabled=False, stale_s=STALE_S)
    assert v == 1.8 and reason == DISABLED


def test_missing_cap_fails_closed():
    v, reason = apply_perception_cap(1.8, cap_mps=None, cap_age_s=None, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == STALE


def test_stale_cap_fails_closed():
    # A cap older than the staleness budget is a perception fault → stop.
    v, reason = apply_perception_cap(1.8, cap_mps=3.0, cap_age_s=0.9, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == STALE


def test_nonfinite_or_negative_cap_fails_closed():
    for bad in (float("nan"), float("inf"), -1.0):
        v, reason = apply_perception_cap(1.8, cap_mps=bad, cap_age_s=0.0, enabled=True, stale_s=STALE_S)
        assert v == 0.0 and reason == INVALID


def test_fresh_cap_clamps_when_proposed_exceeds_it():
    # Proposed 1.8 m/s, Taj allows only 0.9 → clamp to 0.9.
    v, reason = apply_perception_cap(1.8, cap_mps=0.9, cap_age_s=0.1, enabled=True, stale_s=STALE_S)
    assert v == 0.9 and reason == CAPPED


def test_fresh_cap_passes_when_proposed_is_under_it():
    # Proposed 0.4 m/s, Taj allows 3.0 → unchanged (Taj only tightens, never loosens).
    v, reason = apply_perception_cap(0.4, cap_mps=3.0, cap_age_s=0.1, enabled=True, stale_s=STALE_S)
    assert v == 0.4 and reason == PASS


def test_zero_cap_holds_the_robot():
    # Taj at the MRC floor (obstacle in the standoff / unhealthy corridor) → hold.
    v, reason = apply_perception_cap(1.8, cap_mps=0.0, cap_age_s=0.0, enabled=True, stale_s=STALE_S)
    assert v == 0.0 and reason == CAPPED



# ---------------------------------------------------------------------------
# #1212 relay: the node forwards the CHECKER's stabilized cap and computes none.
# ---------------------------------------------------------------------------


def _stabilized(value, reason="CAP_PASS"):
    """A well-formed stabilized response: a value AND the authorship claim."""
    return {"stabilized_speed_cap_mps": value, "cap_reason": reason}


def test_stabilized_cap_is_relayed_when_present():
    cap, reason = resolve_stabilized_cap(_stabilized(0.8), 2.0)
    assert reason == STABILIZED_OK
    assert cap == 0.8


def test_relay_is_clamped_by_the_unhealthy_raw_cap():
    # An unhealthy frame zeroes the raw cap upstream; the relay must never widen it.
    cap, reason = resolve_stabilized_cap(_stabilized(0.8), 0.0)
    assert reason == STABILIZED_OK
    assert cap == 0.0


def test_malformed_stabilized_cap_fails_closed():
    # An older Taj that does not serve the field is NOT a licence to fall back on
    # the raw cap. Falling back would silently restore pre-#1212 behaviour on
    # exactly the deployments that had not been upgraded.
    for payload in (
        None,
        "not a dict",
        {"cap_reason": "CAP_PASS"},
        {"speed_cap_mps": 1.5, "cap_reason": "CAP_PASS"},
        _stabilized(None),
        _stabilized("0.8"),
        _stabilized(True),
        _stabilized(float("nan")),
        _stabilized(float("inf")),
        _stabilized(-0.1),
    ):
        cap, reason = resolve_stabilized_cap(payload, 2.0)
        assert reason == STABILIZED_MALFORMED, payload
        assert cap == 0.0, payload


def test_a_cap_with_no_authorship_claim_is_refused():
    # The hole this closes: a sidecar that does NOT stabilize still populates
    # `stabilized_speed_cap_mps` — with the RAW cap. Accepting the number on its own
    # would relay an unstabilized cap under a stabilized name, on exactly the
    # deployments that had not been upgraded, and the value would look entirely
    # reasonable. `cap_reason` is the claim; without it there is nothing to relay.
    for missing in ({}, {"cap_reason": None}, {"cap_reason": ""}, {"cap_reason": 7}):
        payload = {"stabilized_speed_cap_mps": 1.9}
        payload.update(missing)
        cap, reason = resolve_stabilized_cap(payload, 2.0)
        assert reason == STABILIZED_UNSTABILIZED, payload
        assert cap == 0.0, payload


def test_a_replayed_held_cap_is_a_valid_relay():
    # A duplicate frame is served the cap the stabilizer is holding, stamped
    # CAP_REPLAY_HELD. It is stabilizer-authored, so it relays. Refusing it would
    # make a two-consumer deployment stutter between the held cap and zero on
    # identical perception.
    cap, reason = resolve_stabilized_cap(_stabilized(0.6, "CAP_REPLAY_HELD"), 2.0)
    assert reason == STABILIZED_OK
    assert cap == 0.6


def test_an_unusable_clamp_bound_fails_closed():
    # Clamping against NaN passes the stabilized value through untouched, so an
    # unusable bound is itself a fault rather than a reason to skip the clamp.
    for bound in (float("nan"), float("inf"), -1.0, None, "2.0", True):
        cap, reason = resolve_stabilized_cap(_stabilized(1.5), bound)
        assert reason == STABILIZED_MALFORMED, bound
        assert cap == 0.0, bound


def test_zero_stabilized_cap_is_a_valid_relay_not_a_fault():
    # Zero is what the checker serves while holding the robot. Treating it as
    # missing would report a relay fault every time the cap was legitimately down.
    cap, reason = resolve_stabilized_cap(_stabilized(0.0), 2.0)
    assert reason == STABILIZED_OK
    assert cap == 0.0


def test_no_cap_state_machine_survives_in_this_module():
    # Gate: prove the Python authority is ABSENT, not merely unused. An advisory
    # copy of a safety state machine drifts silently — its tests keep passing while
    # nothing enforces it, which is the condition that made the original placement a
    # problem. This fails if anyone reintroduces one.
    import perception_cap

    for gone in (
        "SpeedCapGovernor",
        "SpeedCapGovernorConfig",
        "SpeedCapDecision",
    ):
        assert not hasattr(perception_cap, gone), gone

    source = open(perception_cap.__file__, encoding="utf-8").read()
    for residue in ("rise_rate", "clear_confirmations", "initial_cap", "max_dt"):
        assert residue not in source, residue
