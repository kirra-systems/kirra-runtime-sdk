#!/usr/bin/env python3
"""Host smoke test for the ADR-0033 Python↔Rust consumer boundary.

Runs WITHOUT ROS or a motor: it loads libkirra_consumer_ffi via ctypes, mints
frames with the governor stand-in (`kirra_ros_release_mint`), and asserts the
decisions the Rust core returns across the boundary. This is the non-vacuity
proof that the FFI marshalling matches the verify core's semantics — the same
(a)/(b)/(c) behaviours the elevated first-run test checks, minus the wheels.

Prereqs (both built by this script's CI step):
    cargo build -p kirra-consumer-ffi
    cargo build -p kirra-release-token --bin kirra_ros_release_mint

Exit 0 = boundary behaves; exit 1 = a mismatch (fail the gate).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kirra_ffi import KirraConsumer  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
# The dev/demo governor key (well-known [42u8;32]) — DEV ONLY, matches the Rust
# fixtures. A real deployment pins the verifier's provisioned key.
DEV_SEED = "2a" * 32
MINT = None  # resolved in main()

# Demo envelope (Step 3) — the SAME values the node/docs use.
VX_MAX = 0.15
VZ_MAX = 0.4


def mint(*args: str) -> bytes:
    out = subprocess.run(
        [str(MINT), "--seed", DEV_SEED, *args],
        capture_output=True, text=True, check=True,
    )
    return bytes.fromhex(out.stdout.strip())


def pubkey(seed: str = DEV_SEED) -> bytes:
    out = subprocess.run(
        [str(MINT), "--seed", seed, "pubkey"],
        capture_output=True, text=True, check=True,
    )
    return bytes.fromhex(out.stdout.strip())


def split(frame: bytes) -> tuple[bytes, bytes | None]:
    """payload(32) [|| token(96)] → (payload, token|None)."""
    payload = frame[:32]
    token = frame[32:] if len(frame) > 32 else None
    return payload, token


def new_consumer(vk: bytes) -> KirraConsumer:
    return KirraConsumer(
        vk,
        freshness_window_ms=200,
        control_period_ms=100,
        missed_periods=3,
        stop_decel_mps2=1.0,
        vx_max=VX_MAX,
        vz_max=VZ_MAX,
    )


def main() -> int:
    global MINT
    for prof in ("release", "debug"):
        cand = REPO / "target" / prof / "kirra_ros_release_mint"
        if cand.is_file():
            MINT = cand
            break
    if MINT is None:
        print("FAIL: kirra_ros_release_mint not built "
              "(cargo build -p kirra-release-token --bin kirra_ros_release_mint)")
        return 1

    vk = pubkey()
    failures: list[str] = []

    def check(cond: bool, msg: str) -> None:
        if not cond:
            failures.append(msg)

    # (a) A valid governed command ABOVE the demo envelope → Released + clamped.
    c = new_consumer(vk)
    payload, token = split(mint("frame", "--seq", "1", "--issued-ms", "10000",
                                "--linear", "0.5", "--angular", "2.0"))
    r = c.on_frame(payload, token, 10_050)
    check(r.kind == 0, f"(a) valid frame must Release, got kind={r.kind}")
    check(r.write == 1, "(a) release must actuate")
    check(abs(r.linear - VX_MAX) < 1e-9, f"(a) linear must clamp to {VX_MAX}, got {r.linear}")
    check(abs(r.angular - VZ_MAX) < 1e-9, f"(a) angular must clamp to {VZ_MAX}, got {r.angular}")
    print(f"(a) valid  → kind=Released write={r.write} twist=({r.linear:.3f},{r.angular:.3f}) [clamped]")

    # (b) An unsigned command → Refused NO_TOKEN, no write.
    c = new_consumer(vk)
    payload, token = split(mint("frame", "--seq", "1", "--issued-ms", "10000",
                                "--linear", "0.1", "--angular", "0.0", "--no-token"))
    check(token is None, "(b) --no-token frame must carry no token")
    r = c.on_frame(payload, token, 10_000)
    check(r.kind == 2 and r.refusal_code == 0, f"(b) unsigned must be NO_TOKEN, got kind={r.kind} code={r.refusal_code}")
    check(r.write == 0, "(b) refusal must NOT actuate")
    print(f"(b) unsigned → kind=Refused code=NO_TOKEN write={r.write}")

    # (c) Sustained wrong-key (corrupt signature) → SIGNATURE_INVALID + latched alarm.
    c = new_consumer(vk)
    for k in range(10):
        payload, token = split(mint("frame", "--seq", str(k + 1), "--issued-ms", str(10_000 + k),
                                    "--linear", "0.1", "--angular", "0.0", "--corrupt-sig"))
        r = c.on_frame(payload, token, 10_000 + k)
        check(r.kind == 2 and r.refusal_code == 2, f"(c) must be SIGNATURE_INVALID, got code={r.refusal_code}")
        check(r.write == 0, "(c) bad-sig must not actuate")
    h = c.health()
    check(h.key_mismatch_alarm == 1, "(c) sustained bad-sig must LATCH the key-mismatch alarm")
    check(h.signature_invalid == 10, f"(c) per-class counter must read 10, got {h.signature_invalid}")
    expl = c.alarm_explanation()
    check("KEY MISMATCH" in expl, "(c) alarm must carry the loud operator sentence")
    print(f"(c) 10× bad-sig → alarm_latched={h.key_mismatch_alarm} sig_invalid={h.signature_invalid}")
    print(f"    alarm: {expl[:72]}...")

    # (d) Replay (same sequence) → SEQUENCE_NOT_ADVANCED after the first release.
    c = new_consumer(vk)
    payload, token = split(mint("frame", "--seq", "5", "--issued-ms", "10000",
                                "--linear", "0.1", "--angular", "0.0"))
    r1 = c.on_frame(payload, token, 10_000)
    r2 = c.on_frame(payload, token, 10_010)  # exact replay
    check(r1.kind == 0, "(d) first release must pass")
    check(r2.kind == 2 and r2.refusal_code == 5, f"(d) replay must be SEQUENCE_NOT_ADVANCED, got code={r2.refusal_code}")
    print(f"(d) replay → first=Released replay=code=SEQUENCE_NOT_ADVANCED write={r2.write}")

    # (e) Starvation → active decel ramp to zero, then silence (write=0).
    c = new_consumer(vk)
    payload, token = split(mint("frame", "--seq", "1", "--issued-ms", "10000",
                                "--linear", "0.15", "--angular", "0.0"))
    c.on_frame(payload, token, 10_000)
    ramp = []
    silent = False
    for k in range(1, 21):
        t = c.on_tick(10_000 + 300 + k * 100)
        if t.write == 1:
            ramp.append(t.linear)
    check(len(ramp) >= 1, "(e) starvation must produce an active stop ramp")
    check(ramp and ramp[-1] == 0.0, f"(e) ramp must reach zero, got {ramp}")
    check(all(a >= b - 1e-12 for a, b in zip(ramp, ramp[1:])), f"(e) ramp must not increase: {ramp}")
    silent = c.health().silent == 1
    check(silent, "(e) after the ramp the consumer must be silent")
    print(f"(e) starvation → ramp={[round(x,3) for x in ramp]} silent={silent}")

    # (f) A frame signed by a DIFFERENT key entirely → SIGNATURE_INVALID.
    c = new_consumer(vk)
    other_seed = "09" * 32
    other = subprocess.run([str(MINT), "--seed", other_seed, "frame", "--seq", "1",
                            "--issued-ms", "10000", "--linear", "0.1", "--angular", "0.0"],
                           capture_output=True, text=True, check=True)
    payload, token = split(bytes.fromhex(other.stdout.strip()))
    r = c.on_frame(payload, token, 10_000)
    check(r.kind == 2 and r.refusal_code == 2, f"(f) wrong-key must be SIGNATURE_INVALID, got code={r.refusal_code}")
    print(f"(f) wrong-key → kind=Refused code=SIGNATURE_INVALID write={r.write}")

    print()
    if failures:
        for f in failures:
            print(f"FAIL {f}")
        print(f"\nffi smoke test FAILED ({len(failures)} mismatch(es)) — the Python↔Rust boundary diverges.")
        return 1
    print("ffi smoke test: OK — the Python↔Rust consumer boundary matches the verify core.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
