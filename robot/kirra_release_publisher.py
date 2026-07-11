#!/usr/bin/env python3
"""Governor stand-in publisher for the elevated first-run test (DEV/DEMO only).

Publishes release frames on KIRRA_RELEASE_TOPIC for the consumer to verify. It
mints frames with `kirra_ros_release_mint` (the SAME Rust `issue_ros_release` the
governor uses — no Python crypto), then publishes them as std_msgs/UInt8MultiArray.

⚠️ DEV/DEMO ONLY. The signing seed is a well-known test key; a real deployment's
frames come from the verifier's provisioned key, not this script.

Modes:
    --valid     publish signed, freshly-stamped, strictly-advancing frames
    --unsigned  publish the 32-byte payload with NO token (→ consumer refuses)

Env:
    KIRRA_MINT_BIN     path to kirra_ros_release_mint (else searched under target/)
    KIRRA_DEV_SEED     64-hex signing seed (default 2a*32, matching the fixtures)
    KIRRA_RELEASE_TOPIC (default /kirra/release)
    KIRRA_PUB_LINEAR   commanded linear m/s (default 0.15)
    KIRRA_PUB_ANGULAR  commanded angular rad/s (default 0.0)
    KIRRA_PUB_RATE_HZ  publish rate (default 10)
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

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
               linear: float, angular: float, unsigned: bool) -> bytes:
    args = [mint, "--seed", seed, "frame", "--seq", str(seq),
            "--issued-ms", str(issued_ms), "--linear", str(linear),
            "--angular", str(angular)]
    if unsigned:
        args.append("--no-token")
    out = subprocess.run(args, capture_output=True, text=True, check=True)
    return bytes.fromhex(out.stdout.strip())


def main() -> int:
    unsigned = "--unsigned" in sys.argv
    valid = "--valid" in sys.argv
    if unsigned == valid:
        print("usage: kirra_release_publisher.py (--valid | --unsigned)", file=sys.stderr)
        return 2

    mint = find_mint()
    seed = os.environ.get("KIRRA_DEV_SEED", DEFAULT_SEED)
    topic = os.environ.get("KIRRA_RELEASE_TOPIC", "/kirra/release")
    linear = float(os.environ.get("KIRRA_PUB_LINEAR", "0.15"))
    angular = float(os.environ.get("KIRRA_PUB_ANGULAR", "0.0"))
    rate_hz = float(os.environ.get("KIRRA_PUB_RATE_HZ", "10"))
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
    mode = "UNSIGNED (expect REFUSED)" if unsigned else "VALID governed"
    node.get_logger().info(f"publishing {mode} frames on {topic} at {rate_hz} Hz "
                           f"linear={linear} angular={angular}")

    seq = 1
    period = 1.0 / rate_hz
    try:
        while rclpy.ok():
            issued_ms = int(time.time() * 1000)  # fresh each frame
            frame = mint_frame(mint, seed, seq, issued_ms, linear, angular, unsigned)
            msg = UInt8MultiArray()
            msg.data = list(frame)
            pub.publish(msg)
            seq += 1
            time.sleep(period)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
