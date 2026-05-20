#!/usr/bin/env python3
"""Hardware-in-loop replay proof: validates Aegis proxy behaviour against reference vectors.

Three phases are tested in sequence against a running Aegis gateway on localhost:5502.
Each phase sends a single Modbus TCP FC06 frame and checks the exact byte response.

PHASE_1: 1201.8 GPM — within envelope, passes through unchanged.
PHASE_2: 4593.6 GPM — exceeds 3000.0 GPM ceiling, clamped and forwarded as 3000.0.
PHASE_3: register 11 (0x000B) — not the monitored asset_identifier 10, triggers exception 0x02.

Usage:
    AEGIS_SUPERVISOR_RESET_KEY=<key> cargo run -- config/asset_profile.json &
    python3 tests/hil_replay_proof.py
"""

import socket
import sys
import time

PROXY_HOST = "127.0.0.1"
PROXY_PORT = 5502


def send_and_recv(sock: socket.socket, frame: bytes) -> bytes:
    sock.sendall(frame)
    response = b""
    while len(response) < 6:
        chunk = sock.recv(512)
        if not chunk:
            break
        response += chunk
    if len(response) < 6:
        return response
    pdu_len = int.from_bytes(response[4:6], "big")
    while len(response) < 6 + pdu_len:
        chunk = sock.recv(512)
        if not chunk:
            break
        response += chunk
    return response


def main() -> None:
    # PHASE_1: raw value 0x2EF2 = 12018 counts / scale 10 = 1201.8 GPM → passthrough
    phase1_request  = bytes.fromhex("000100000006010600 0A2EF2".replace(" ", ""))
    phase1_expected = phase1_request

    # PHASE_2: raw value 0xB3B0 = 45984 counts / scale 10 = 4598.4 GPM → clamped to 3000.0
    #          3000.0 * scale 10 = 30000 = 0x7530
    phase2_request  = bytes.fromhex("000200000006010600 0AB3B0".replace(" ", ""))
    phase2_expected = bytes.fromhex("000200000006010600 0A7530".replace(" ", ""))

    # PHASE_3: register 0x000B (11) — not monitored, exception code 0x02 returned
    phase3_request  = bytes.fromhex("000300000006010600 0B2EF2".replace(" ", ""))
    phase3_expected = bytes.fromhex("00030000000301860 2".replace(" ", ""))

    phases = [
        ("PHASE_1: passthrough 1201.8 GPM", phase1_request, phase1_expected),
        ("PHASE_2: envelope clamp 4598.4→3000.0 GPM", phase2_request, phase2_expected),
        ("PHASE_3: unmonitored register exception 0x02", phase3_request, phase3_expected),
    ]

    failures = 0
    for label, request, expected in phases:
        try:
            with socket.create_connection((PROXY_HOST, PROXY_PORT), timeout=5.0) as sock:
                sock.settimeout(5.0)
                response = send_and_recv(sock, request)
                if response == expected:
                    print(f"[PASS] {label}")
                else:
                    print(f"[FAIL] {label}")
                    print(f"       expected: {expected.hex(' ')}")
                    print(f"       got:      {response.hex(' ')}")
                    failures += 1
        except Exception as exc:
            print(f"[ERROR] {label}: {exc}")
            failures += 1
        time.sleep(0.1)

    if failures:
        print(f"\n{failures} phase(s) FAILED")
        sys.exit(1)
    print("\nAll HIL replay phases PASSED")


if __name__ == "__main__":
    main()
