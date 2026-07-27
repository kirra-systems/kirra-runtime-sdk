"""governor — prerequisites for the GOVERNED drive path (observability only).

Verifies the pieces the ADR-0033 chokepoint needs exist: the release-token
verify key, the pinned platform profile digest, the consumer FFI cdylib, and the
verifier itself. A FAIL here never gates anything — the chokepoint fails closed
on its own; this just tells the operator BEFORE a drive attempt dead-ends.

The profile-digest check here is FORMAT only, and pure env reading. Whether the
pinned value AGREES with what Taj computes for this robot needs a live query, so
it lives in the opt-in `profile_digest` module.
"""
import os

from doctor.core import detail, http_status

NAME = "governor"
DESCRIPTION = "governed-drive prerequisites (key, profile digest, FFI, verifier)"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 10

DIGEST_HEX_LEN = 64
_LOWERCASE_HEX = frozenset("0123456789abcdef")


def looks_like_pinned_digest(value):
    """True iff `value` is exactly 64 lowercase hex characters.

    The same rule the consumer applies (`kirra_ffi.parse_profile_digest_hex`)
    and the verifier applies to a release binding. Duplicated as a few lines
    rather than imported so a doctor module never depends on the consumer's
    ctypes layer — importing kirra_ffi would try to locate the cdylib.
    """
    if not isinstance(value, str) or len(value) != DIGEST_HEX_LEN:
        return False
    return all(c in _LOWERCASE_HEX for c in value)


def run(ctx):
    env, details = ctx["robot_env"], []

    vk = env.get("KIRRA_GOVERNOR_VK_HEX", "")
    if vk and "__FILLED" not in vk:
        details.append(detail("governor verify key pinned", "PASS", f"{len(vk)} hex chars"))
    else:
        details.append(detail("governor verify key pinned", "FAIL",
                              "unset or placeholder",
                              fix="install_kirra.sh --dev-key (bench) or enroll the real key"))

    # The evidence-bound V2 consumer refuses to START without a well-formed
    # pinned profile digest, so a bad value here is a hard drive blocker — FAIL,
    # not WARN. Agreement with Taj is a separate, live question (see the
    # `profile_digest` module); this only catches unset / uppercase / truncated.
    pinned = (env.get("KIRRA_PROFILE_DIGEST") or "").strip()
    if looks_like_pinned_digest(pinned):
        details.append(detail("platform profile digest pinned", "PASS",
                              f"{pinned[:16]}… (64 lowercase hex)"))
    elif not pinned or "__FILLED" in pinned:
        details.append(detail("platform profile digest pinned", "FAIL", "unset or placeholder",
                              fix="export KIRRA_PROFILE_DIGEST — obtain it from Taj "
                                  "(kirra_doctor.py --module profile_digest)"))
    else:
        details.append(detail("platform profile digest pinned", "FAIL",
                              f"malformed ({len(pinned)} chars"
                              + (", not lowercase hex)" if len(pinned) == DIGEST_HEX_LEN
                                 else f", want {DIGEST_HEX_LEN})"),
                              fix="must be exactly 64 LOWERCASE hex chars; uppercase is "
                                  "refused, never folded — the consumer aborts on this"))

    so = env.get("KIRRA_CONSUMER_LIB", "/opt/kirra/lib/libkirra_consumer_ffi.so")
    if os.path.isfile(so):
        details.append(detail("consumer FFI cdylib", "PASS", so))
    else:
        details.append(detail("consumer FFI cdylib", "FAIL", f"missing: {so}",
                              fix="robot/install/install_kirra.sh (builds + stages it)"))

    # KIRRA_VEHICLE_CLASS lives in the VERIFIER env (kirra.env, root 600) — not
    # readable here without sudo. The verifier itself fail-closes at startup if
    # it is unset, so "verifier is up" below already implies a class was set.
    verifier = env.get("KIRRA_VERIFIER_URL", "http://localhost:8090").rstrip("/")
    code = http_status(f"{verifier}/health")
    if code == 200:
        details.append(detail("verifier /health", "PASS",
                              f"{verifier} (implies KIRRA_VEHICLE_CLASS was set — "
                              "startup aborts without one)"))
    else:
        details.append(detail("verifier /health", "WARN",
                              f"{verifier} unreachable ({code})",
                              fix="start the verifier — R2_LIVE_LOOP_BRINGUP.md"))
    return {"details": details}
