"""governor — prerequisites for the GOVERNED drive path (observability only).

Verifies the pieces the ADR-0033 chokepoint needs exist: the release-token
verify key, the consumer FFI cdylib, and the verifier itself. A FAIL here never
gates anything — the chokepoint fails closed on its own; this just tells the
operator BEFORE a drive attempt dead-ends.
"""
import os

from doctor.core import detail, http_status

NAME = "governor"
DESCRIPTION = "governed-drive prerequisites (key, FFI, verifier)"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 10


def run(ctx):
    env, details = ctx["robot_env"], []

    vk = env.get("KIRRA_GOVERNOR_VK_HEX", "")
    if vk and "__FILLED" not in vk:
        details.append(detail("governor verify key pinned", "PASS", f"{len(vk)} hex chars"))
    else:
        details.append(detail("governor verify key pinned", "FAIL",
                              "unset or placeholder",
                              fix="install_kirra.sh --dev-key (bench) or enroll the real key"))

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
