#!/usr/bin/env python3
"""Governor stand-in publisher for the elevated first-run test (DEV/DEMO only).

Publishes evidence-bound V2 release frames on KIRRA_RELEASE_TOPIC for the
consumer to verify. It mints them with `kirra_ros_release_mint frame-v2` (the
SAME Rust `issue_ros_bound_command_release` the governor uses — no Python
crypto), then publishes them as std_msgs/UInt8MultiArray.

⚠️ DEV/DEMO ONLY. The signing seed is a well-known test key; a real deployment's
frames come from the verifier's provisioned key, not this script.

V2 (272 bytes = payload 176 + token 96) is what the live motor consumer accepts;
it refuses the old 128-byte V1 frame outright. Every field the consumer's gate
checks is therefore minted here: the pinned profile digest, an explicit signed
expiry, and a perception-frame identity.

Modes (exactly one):
    --valid       publish signed, freshly-stamped, strictly-advancing frames
    --unsigned    publish the bare 176-byte payload with NO token. V2 has no
                  unsigned wire form, so the consumer refuses this at its strict
                  272-byte parse — logged as REFUSED (MALFORMED_FRAME_176B), and
                  the bytes never reach the verify core. (Under V1 this reached
                  the core and came back NO_TOKEN; that arm is now unreachable
                  from the topic, which is the stricter outcome.)
    --corrupt-sig publish a well-formed 272-byte frame whose signature bit is
                  flipped. This DOES reach the core, so it exercises the Ed25519
                  gate itself → REFUSED (SIGNATURE_INVALID). Use this when the
                  point is to prove the crypto check rather than the wire parse.

Env:
    KIRRA_MINT_BIN     path to kirra_ros_release_mint (else searched under target/)
    KIRRA_DEV_SEED     64-hex signing seed (default 2a*32, matching the fixtures)
    KIRRA_PROFILE_DIGEST  REQUIRED, 64 LOWERCASE hex — the platform profile
                       digest to bind into each frame. It must equal the value
                       the consumer pins, or every frame is refused with
                       PROFILE_DIGEST_MISMATCH. Deliberately the SAME variable
                       the consumer reads, so one exported value keeps a bench
                       run consistent; there is no default, because a guessed
                       digest would produce a bench that refuses everything.
    KIRRA_RELEASE_TOPIC (default /kirra/release)
    KIRRA_PUB_LINEAR   commanded linear m/s (default 0.15)
    KIRRA_PUB_ANGULAR  commanded angular rad/s (default 0.0)
    KIRRA_PUB_RATE_HZ  publish rate (default 10)
    KIRRA_PUB_LIFETIME_MS  signed token lifetime (default: KIRRA_FRESHNESS_WINDOW_MS
                       if set, else 200). Must be <= the consumer's configured
                       maximum or the gate refuses LIFETIME_TOO_LONG; defaulting
                       from the consumer's own knob keeps the two in step.
    KIRRA_PUB_TRACKER_GEN  bound tracker generation (default 1)
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kirra_ffi import parse_profile_digest_hex  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
DEFAULT_SEED = "2a" * 32


def find_mint() -> str:
    env = os.environ.get("KIRRA_MINT_BIN")
    if env:
        return env
    for prof in ("release", "debug"):
        cand = REPO / "target" / prof / "kirra_ros_release_mint"
        if cand.is_file():
            return str(cand)
    print("FATAL: kirra_ros_release_mint not found "
          "(cargo build -p kirra-release-token --bin kirra_ros_release_mint) "
          "or set KIRRA_MINT_BIN", file=sys.stderr)
    sys.exit(2)


def mint_frame(mint: str, seed: str, seq: int, issued_ms: int,
               linear: float, angular: float, unsigned: bool,
               *, profile_digest: str, lifetime_ms: int,
               scan_seq: int, tracker_gen: int, corrupt_sig: bool = False) -> bytes:
    """Mint one evidence-bound V2 frame (272 bytes; 176 with `unsigned`).

    The command sequence doubles as the nonce (the minter's default), which is
    what the gate wants: both must strictly advance, and this publisher
    advances `seq` once per frame.
    """
    args = [mint, "--seed", seed, "frame-v2", "--seq", str(seq),
            "--issued-ms", str(issued_ms), "--linear", str(linear),
            "--angular", str(angular),
            "--profile-digest", profile_digest,
            "--lifetime-ms", str(lifetime_ms),
            "--scan-seq", str(scan_seq),
            "--tracker-gen", str(tracker_gen)]
    if unsigned:
        args.append("--no-token")
    if corrupt_sig:
        args.append("--corrupt-sig")
    out = subprocess.run(args, capture_output=True, text=True, check=True)
    return bytes.fromhex(out.stdout.strip())


def main() -> int:
    MODES = ("--valid", "--unsigned", "--corrupt-sig")
    selected = [m for m in MODES if m in sys.argv]
    if len(selected) != 1:
        print("usage: kirra_release_publisher.py (--valid | --unsigned | --corrupt-sig)",
              file=sys.stderr)
        return 2
    unsigned = selected[0] == "--unsigned"
    corrupt_sig = selected[0] == "--corrupt-sig"

    mint = find_mint()
    seed = os.environ.get("KIRRA_DEV_SEED", DEFAULT_SEED)
    topic = os.environ.get("KIRRA_RELEASE_TOPIC", "/kirra/release")
    linear = float(os.environ.get("KIRRA_PUB_LINEAR", "0.15"))
    angular = float(os.environ.get("KIRRA_PUB_ANGULAR", "0.0"))
    rate_hz = float(os.environ.get("KIRRA_PUB_RATE_HZ", "10"))

    # The profile digest bound into every frame. No default: it must equal what
    # the consumer pins, and a guessed value would mint a bench that refuses
    # every frame with PROFILE_DIGEST_MISMATCH — a confusing way to fail a
    # guided first-run test. Validated here with the same rule the consumer
    # applies, so a typo is caught before any frame is published.
    profile_digest = (os.environ.get("KIRRA_PROFILE_DIGEST") or "").strip()
    if parse_profile_digest_hex(profile_digest) is None:
        print("FATAL: KIRRA_PROFILE_DIGEST must be exactly 64 LOWERCASE hexadecimal "
              "characters — the evidence-bound V2 frame binds it, and it must match "
              "the digest the consumer pins. Export the same value for both "
              f"processes. Got {os.environ.get('KIRRA_PROFILE_DIGEST')!r}.",
              file=sys.stderr)
        return 2

    # Signed lifetime. Default from the consumer's own maximum knob so the two
    # stay in step: a lifetime above the consumer's maximum is refused
    # LIFETIME_TOO_LONG, which would look like a signing failure but is a
    # config mismatch.
    lifetime_ms = int(os.environ.get(
        "KIRRA_PUB_LIFETIME_MS",
        os.environ.get("KIRRA_FRESHNESS_WINDOW_MS", "200"),
    ))
    if lifetime_ms <= 0:
        print(f"FATAL: token lifetime must be > 0 ms, got {lifetime_ms}", file=sys.stderr)
        return 2
    tracker_gen = int(os.environ.get("KIRRA_PUB_TRACKER_GEN", "1"))
    # Fail fast on a nonsensical rate (Copilot #901): 0 divides, NaN/Inf breaks
    # sleep(). This script drives the guided first-run test — a bad knob must be
    # a clear error, not a crash mid-procedure.
    import math
    if not (math.isfinite(rate_hz) and rate_hz > 0):
        print(f"FATAL: KIRRA_PUB_RATE_HZ must be finite and > 0, got {rate_hz}", file=sys.stderr)
        return 2

    import rclpy
    from rclpy.node import Node
    from std_msgs.msg import UInt8MultiArray

    rclpy.init()
    node = Node("kirra_release_publisher")
    pub = node.create_publisher(UInt8MultiArray, topic, 10)
    mode = ("UNSIGNED (expect REFUSED: MALFORMED_FRAME_176B)" if unsigned
            else "CORRUPT-SIG (expect REFUSED: SIGNATURE_INVALID)" if corrupt_sig
            else "VALID governed")
    node.get_logger().info(
        f"publishing evidence-bound V2 {mode} frames on {topic} at {rate_hz} Hz "
        f"linear={linear} angular={angular} lifetime={lifetime_ms}ms "
        f"profile={profile_digest[:16]}…"
    )

    # Seed the sequence from the wall clock, NOT a fixed 1. The consumer's
    # accepted-sequence watermark persists for its whole lifetime and the rule is
    # `sequence <= last_accepted => reject`; a fresh publisher process hardcoding
    # seq=1 would be fully refused (SEQUENCE_NOT_ADVANCED) after the FIRST --valid
    # window, since the consumer already accepted a higher seq. A wall-clock seed
    # makes each new publisher run start above any recent prior run, so multi-
    # window guided tests (straight, then turn, then re-establish) all advance.
    # NANOSECONDS (u64, the minter's seq width) — not ms — so back-to-back
    # restarts within the same millisecond don't collide back to a refused seq.
    # Honest caveat: the wall clock is NOT guaranteed monotonic across restarts
    # (NTP/manual steps can move it back) — this is restart-unique in practice for
    # a DEV harness, not a hard guarantee; if a backward step ever re-triggers
    # SEQUENCE_NOT_ADVANCED, restart the consumer to reset its watermark. Per-frame
    # +1 keeps it strictly increasing within the run.
    seq = time.time_ns()
    # The bound perception-frame identity. The consumer's supersession
    # watermark persists for its lifetime and refuses evidence OLDER than the
    # newest already released, so this needs the same restart-uniqueness
    # argument as `seq` above — hence the same wall-clock seed. It is held
    # CONSTANT for the run: the gate explicitly allows several control commands
    # derived from one perception frame, provided sequence and nonce advance
    # (which they do, once per frame). A per-frame increment would buy nothing
    # and would make a mid-run clock step able to strand the publisher.
    scan_seq = time.time_ns()
    period = 1.0 / rate_hz
    try:
        while rclpy.ok():
            issued_ms = int(time.time() * 1000)  # fresh each frame
            frame = mint_frame(
                mint, seed, seq, issued_ms, linear, angular, unsigned,
                profile_digest=profile_digest, lifetime_ms=lifetime_ms,
                scan_seq=scan_seq, tracker_gen=tracker_gen,
                corrupt_sig=corrupt_sig,
            )
            msg = UInt8MultiArray()
            msg.data = list(frame)
            pub.publish(msg)
            seq += 1
            time.sleep(period)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        # GUARDED shutdown (hardware first-run finding): rclpy.init() installs
        # its own SIGINT/SIGTERM handlers, so on Ctrl-C / `timeout`'s SIGTERM
        # the context is ALREADY shut down by the time this finally runs — an
        # unconditional shutdown() here was a guaranteed second call
        # (RCLError: rcl_shutdown already called) after every publish window.
        # Exactly-once teardown: shutdown only if the context is still up.
        if rclpy.ok():
            rclpy.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
