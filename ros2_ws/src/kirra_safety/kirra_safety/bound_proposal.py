#!/usr/bin/env python3
"""
Atomic evidence-bound proposal envelope for the R2 ROS live path.

No ROS / rclpy dependency — the same pure-transform / ROS-shim split the rest
of this package uses, so the fail-closed parse is unit-testable without booting
ROS.

WHY AN ENVELOPE INSTEAD OF A BARE Twist
---------------------------------------
A `geometry_msgs/Twist` carries velocities and nothing else. It cannot say
WHICH perception frame the doer looked at when it authored them, so the
verifier could only ever sign "some robot asked for 0.4 m/s" — never "0.4 m/s
authored against scan 813, tracker generation 7, under profile <digest>".
Splitting the command from its evidence across two topics also makes them
independently stale: a fresh Twist could pair with a superseded binding.

This envelope makes command and evidence ONE atomic message. The doer
publishes canonical JSON on the proposal topic as a `std_msgs/String`; the
interceptor parses it fail-closed and forwards the exact binding to the
verifier, which binds it into the signed 176-byte V2 release payload. The
motor consumer then refuses any frame whose evidence identity does not match
what it independently expects (`RosBoundCommandRefusal::*`).

WIRE SHAPE (canonical JSON object)
----------------------------------
    {"angular_z": <float>,
     "linear_x": <float>,
     "linear_y": <float>,
     "release_binding": {
        "camera_sequence": <u64 > 0> | null,
        "evidence_digest": "<64 lowercase hex>",
        "profile_digest":  "<64 lowercase hex>",
        "proposal_digest": "<64 lowercase hex>",
        "scan_sequence":   <u64 > 0>,
        "tracker_generation": <u64 > 0>}}

VALIDATION IS A MIRROR, NOT A POLICY
------------------------------------
`parse_bound_proposal` accepts exactly what the verifier's HTTP perimeter
accepts — `ActuatorReleaseBinding::validate` in `src/gateway/policy_layer.rs`:
nonzero `scan_sequence` / `tracker_generation`, a `camera_sequence` that is
absent/null or nonzero, and digests that are EXACTLY 64 lowercase hex
characters. Anything the verifier would 400 is refused here instead, before a
request is ever made. This module is deliberately the stricter twin of that
Rust function; if one changes the other must change with it.

The parse NEVER repairs. It does not lowercase a digest, coerce "12" to 12,
round a float, or substitute a default for a missing field — a repaired
identity is a FORGED identity, and it would be signed as though the doer had
authored it. Every fault returns None, and the caller stops the robot.
"""

import json
import math
from collections import namedtuple

# One parsed, fully-validated proposal. `release_binding` is a plain dict
# carrying exactly the six canonical fields with their values UNCHANGED from
# the wire — it is forwarded to the verifier verbatim.
BoundProposal = namedtuple(
    "BoundProposal", ["linear_x", "linear_y", "angular_z", "release_binding"]
)

# The three velocity components, and the six binding fields. Named here so the
# encoder, the parser and the tests all agree on one spelling.
VELOCITY_FIELDS = ("linear_x", "linear_y", "angular_z")
DIGEST_FIELDS = ("profile_digest", "evidence_digest", "proposal_digest")
SEQUENCE_FIELDS = ("scan_sequence", "tracker_generation")
BINDING_FIELDS = SEQUENCE_FIELDS + ("camera_sequence",) + DIGEST_FIELDS

# SHA-256 as the verifier spells it on the wire: 64 characters, lowercase hex.
DIGEST_HEX_LEN = 64
_LOWERCASE_HEX = frozenset("0123456789abcdef")

# u64 domain — the payload encodes these as 8 little-endian bytes, so a value
# outside the range is not representable and the verifier would refuse it.
U64_MAX = (1 << 64) - 1


def _is_finite_number(value):
    """True only for a real, finite int/float.

    Rejects None, bool (an int subclass, never a measurement), non-numeric
    types such as a JSON string, NaN, and +/-Inf. A non-finite velocity must
    never reach the governor, let alone the motors.
    """
    if isinstance(value, bool):
        return False
    if not isinstance(value, (int, float)):
        return False
    return math.isfinite(value)


def _is_positive_u64(value):
    """True only for an integer in [1, U64_MAX].

    Rejects bool, floats (including an exact-valued 5.0 — the verifier's serde
    u64 refuses a JSON float, so accepting one here would send a request that
    is guaranteed to 400), negatives, and zero. Zero is refused because it is
    the verifier's explicit "no such frame" sentinel.
    """
    if isinstance(value, bool):
        return False
    if not isinstance(value, int):
        return False
    return 1 <= value <= U64_MAX


def is_lowercase_sha256_hex(value):
    """True iff `value` is exactly 64 lowercase hexadecimal characters.

    Mirrors `decode_lowercase_sha256` in `src/gateway/policy_layer.rs`.
    Uppercase is refused rather than folded: the digest is an identity, and
    two spellings of one identity would let the same evidence be presented
    under two different-looking bindings.
    """
    if not isinstance(value, str) or len(value) != DIGEST_HEX_LEN:
        return False
    return all(char in _LOWERCASE_HEX for char in value)


def validate_release_binding(binding):
    """Validate a release-binding mapping, returning a canonical dict or None.

    The returned dict carries exactly `BINDING_FIELDS`, with values IDENTICAL
    to the input (digest strings are the same objects' values, never
    re-cased or re-encoded). Unknown extra keys are dropped rather than
    forwarded, so a doer cannot smuggle unvalidated fields to the verifier.

    `camera_sequence` is optional: absent or null means "this frame had no
    camera contribution" and is forwarded as None (JSON null), which the
    verifier's `Option<u64>` accepts. Present-but-zero is refused, matching
    the verifier's `camera_sequence == Some(0)` rejection.
    """
    if not isinstance(binding, dict):
        return None

    for field in SEQUENCE_FIELDS:
        if not _is_positive_u64(binding.get(field)):
            return None

    camera_sequence = binding.get("camera_sequence")
    if camera_sequence is not None and not _is_positive_u64(camera_sequence):
        return None

    for field in DIGEST_FIELDS:
        if not is_lowercase_sha256_hex(binding.get(field)):
            return None

    return {
        "scan_sequence": binding["scan_sequence"],
        "camera_sequence": camera_sequence,
        "tracker_generation": binding["tracker_generation"],
        "profile_digest": binding["profile_digest"],
        "evidence_digest": binding["evidence_digest"],
        "proposal_digest": binding["proposal_digest"],
    }


def parse_bound_proposal(data):
    """Parse one canonical-JSON proposal envelope, fail-closed.

    Returns a `BoundProposal` only when the payload is a JSON object carrying
    three finite velocity components AND a fully valid `release_binding`.
    Returns None for every fault: a non-string input, empty or whitespace-only
    data (the doer's explicit hold), undecodable JSON, a non-object top level,
    a missing/non-finite velocity, or a binding that fails validation.

    A None result means the caller must stop the robot and make no request.
    There is deliberately no partial success: a velocity without its evidence
    is exactly the unbound command this envelope exists to abolish.
    """
    if not isinstance(data, str):
        return None

    try:
        # `parse_constant` fires on the bare JSON tokens NaN / Infinity /
        # -Infinity, which json.loads otherwise accepts as floats. A non-finite
        # velocity is refused at the decode, before it can reach the governor.
        decoded = json.loads(data, parse_constant=_refuse_json_constant)
    except (ValueError, TypeError, RecursionError):
        return None

    if not isinstance(decoded, dict):
        return None

    velocities = {}
    for field in VELOCITY_FIELDS:
        value = decoded.get(field)
        if not _is_finite_number(value):
            return None
        velocities[field] = float(value)

    release_binding = validate_release_binding(decoded.get("release_binding"))
    if release_binding is None:
        return None

    return BoundProposal(
        linear_x=velocities["linear_x"],
        linear_y=velocities["linear_y"],
        angular_z=velocities["angular_z"],
        release_binding=release_binding,
    )


def _refuse_json_constant(token):
    """Reject the non-finite JSON constants outright (see `parse_bound_proposal`)."""
    raise ValueError("non-finite JSON constant: {}".format(token))


def encode_bound_proposal(linear_x, linear_y, angular_z, release_binding):
    """Serialize one proposal envelope to canonical JSON, or return None.

    Canonical = sorted keys, no insignificant whitespace, and `allow_nan=False`
    so a non-finite velocity raises instead of emitting the `NaN` token that no
    strict JSON reader accepts. The binding is re-validated on the way out, so
    a doer cannot publish an envelope its own interceptor would refuse — the
    fault surfaces at the publisher, where the operator can see it.

    Returns None when the velocities are not finite or the binding is invalid.
    """
    validated = validate_release_binding(release_binding)
    if validated is None:
        return None
    if not all(_is_finite_number(v) for v in (linear_x, linear_y, angular_z)):
        return None

    return json.dumps(
        {
            "linear_x": float(linear_x),
            "linear_y": float(linear_y),
            "angular_z": float(angular_z),
            "release_binding": validated,
        },
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )


def build_release_binding(perception_frame_id, proposal_digest):
    """Assemble a release binding from a Taj evidence frame + Occy's proposal digest.

    `perception_frame_id` is the `frame_id` object of a Taj `/perception`
    response (`PerceptionFrameId` in `crates/kirra-sidecars/src/taj.rs`);
    `proposal_digest` is the `proposal_digest` of the Occy `/plan` response
    that was authored against exactly that frame.

    Returns a validated binding dict, or None if either input is missing or
    malformed — including Taj's `PerceptionFrameId::invalid()` sentinel, whose
    zero sequences and empty digests fail validation. Pairing a digest with a
    frame the plan did not actually use is the one thing this function must
    never do, so it composes only what the caller hands it and validates the
    result.
    """
    if not isinstance(perception_frame_id, dict):
        return None

    return validate_release_binding(
        {
            "scan_sequence": perception_frame_id.get("scan_sequence"),
            "camera_sequence": perception_frame_id.get("camera_sequence"),
            "tracker_generation": perception_frame_id.get("tracker_generation"),
            "profile_digest": perception_frame_id.get("profile_digest"),
            "evidence_digest": perception_frame_id.get("evidence_digest"),
            "proposal_digest": proposal_digest,
        }
    )
