#!/usr/bin/env python3
"""attest_bench_node.py — bring a fresh single-robot verifier out of the M-9
empty-fleet LockedOut by registering + ATTESTING one Trusted node.

WHY THIS EXISTS
---------------
A freshly-booted verifier with an empty node registry reports fleet posture
`LockedOut` (code 2) — BY DESIGN, not a bug. See src/posture_engine.rs (M-9):

    An EMPTY live-node set on an Active verifier is "no positive trust
    evidence", NOT "the fleet is healthy". Caching Nominal there would be
    fail-OPEN (should_route_command reads Nominal as "admit everything"), so
    the engine overrides the pure aggregate and forces LockedOut.

The empty-set LockedOut is NOT sticky: it auto-recovers to Nominal the moment
ONE Trusted node is registered (test: empty-set LockedOut must auto-recover).
A merely-*registered* node is `Unknown` → the DAG derives Degraded, and under
Degraded the actuator gate DENIES re-initiation from a standstill
(DegradedReinitiationDenied) — so the robot still can't start driving. Only a
`Trusted` node yields Nominal. Trusted requires the real Ed25519 challenge/
response handshake (INVARIANT #3 — trust is never mocked).

This script runs that handshake honestly for a bench node:

  1. POST /attestation/register        (admin-gated)  -> node Unknown
  2. POST /attestation/challenge/<id>  (open)         -> nonce (u64)
  3. sign attestation_signing_payload(node_id, nonce) with the node's AK
  4. POST /attestation/verify          (open)         -> node Trusted
  5. GET  /fleet/posture                              -> expect Nominal

The AK private seed is persisted (default /etc/kirra/bench_ak.seed) so re-runs
reuse the same key and are idempotent across verifier restarts.

  🔴 BENCH/DEMO ONLY. A production node proves possession of a hardware-rooted
  AK (measured boot + TPM quote, kirra-ota-ctl enroll). This generates a
  software AK on the box — never provision it on a golden unit.

USAGE
-----
  set -a; source /etc/kirra/kirra.env; set +a   # for KIRRA_ADMIN_TOKEN
  python3 robot/attest_bench_node.py

ENV (all optional except the admin token):
  KIRRA_URL         verifier base URL         (default http://localhost:8090)
  KIRRA_ADMIN_TOKEN admin bearer token        (REQUIRED; register is admin-gated)
  KIRRA_BENCH_NODE_ID  node id to attest      (default "r2-bench")
  KIRRA_BENCH_AK_SEED_FILE  AK seed path      (default /etc/kirra/bench_ak.seed,
                            falls back to ~/.kirra_bench_ak.seed if unwritable)
"""
import os
import struct
import sys

# Domain-separation tag — MUST byte-match ATTESTATION_DOMAIN in
# crates/kirra-safety-authority/src/attestation.rs.
ATTESTATION_DOMAIN = b"kirra-attestation-challenge-v1"


def die(msg, code=1):
    print(f"attest_bench_node: {msg}", file=sys.stderr)
    sys.exit(code)


try:
    import requests
except ImportError:
    die("python3 'requests' not found — pip3 install requests")

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization
except ImportError:
    die("python3 'cryptography' not found — pip3 install cryptography")


def signing_payload(node_id: str, nonce: int) -> bytes:
    """Byte-identical to attestation_signing_payload(node_id, nonce):
        DOMAIN || u64_le(len(node_id)) || node_id || u64_le(nonce)
    """
    idb = node_id.encode("utf-8")
    return (
        ATTESTATION_DOMAIN
        + struct.pack("<Q", len(idb))
        + idb
        + struct.pack("<Q", nonce)
    )


def load_or_make_ak(seed_file: str) -> Ed25519PrivateKey:
    """Persist a raw 32-byte Ed25519 seed so re-runs reuse the same AK."""
    if os.path.exists(seed_file):
        with open(seed_file, "rb") as f:
            seed = f.read(32)
        if len(seed) != 32:
            die(f"{seed_file} is not a 32-byte seed (len={len(seed)})")
        return Ed25519PrivateKey.from_private_bytes(seed)
    sk = Ed25519PrivateKey.generate()
    seed = sk.private_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PrivateFormat.Raw,
        encryption_algorithm=serialization.NoEncryption(),
    )
    try:
        # Try the requested (privileged) location first, mode 600.
        fd = os.open(seed_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(seed)
        print(f"  generated a fresh bench AK, saved -> {seed_file}")
    except (PermissionError, FileExistsError, OSError) as e:
        fallback = os.path.expanduser("~/.kirra_bench_ak.seed")
        if seed_file != fallback:
            print(f"  ⚠ could not write {seed_file} ({e}); using {fallback}")
            # Recurse ONCE against the fallback path.
            return load_or_make_ak(fallback)
        raise
    return sk


def public_pem(sk: Ed25519PrivateKey) -> str:
    return (
        sk.public_key()
        .public_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PublicFormat.SubjectPublicKeyInfo,
        )
        .decode("ascii")
    )


def main():
    base = os.environ.get("KIRRA_URL", "http://localhost:8090").rstrip("/")
    token = os.environ.get("KIRRA_ADMIN_TOKEN", "").strip()
    node_id = os.environ.get("KIRRA_BENCH_NODE_ID", "r2-bench").strip()
    seed_file = os.environ.get("KIRRA_BENCH_AK_SEED_FILE", "/etc/kirra/bench_ak.seed")
    if not token:
        die("KIRRA_ADMIN_TOKEN is empty — register is admin-gated. "
            "Source the verifier env: set -a; source /etc/kirra/kirra.env; set +a")

    print(f"== bench-attest node '{node_id}' at {base} ==")
    sk = load_or_make_ak(seed_file)
    pem = public_pem(sk)
    auth = {"Authorization": f"Bearer {token}"}

    # 1. register (idempotent: a re-register just refreshes the Unknown record).
    r = requests.post(
        f"{base}/attestation/register",
        json={"node_id": node_id, "ak_public_pem": pem},
        headers=auth,
        timeout=5,
    )
    if r.status_code not in (200, 201):
        die(f"register failed: HTTP {r.status_code} {r.text}")
    print(f"  1. register     -> {r.status_code}")

    # 2. challenge.
    r = requests.post(f"{base}/attestation/challenge/{node_id}", timeout=5)
    if r.status_code != 200:
        die(f"challenge failed: HTTP {r.status_code} {r.text}")
    nonce = int(r.json()["nonce"])
    print(f"  2. challenge    -> nonce={nonce}")

    # 3. sign the domain-separated challenge payload with the node's AK.
    sig = sk.sign(signing_payload(node_id, nonce))
    proof_hex = sig.hex()

    # 4. verify -> Trusted.
    r = requests.post(
        f"{base}/attestation/verify",
        json={"node_id": node_id, "nonce": nonce, "proof_hex": proof_hex},
        timeout=5,
    )
    if r.status_code != 200:
        die(f"verify failed: HTTP {r.status_code} {r.text}")
    print(f"  3. verify       -> {r.status_code} {r.json()}")

    # 5. confirm the fleet recovered to Nominal.
    r = requests.get(f"{base}/fleet/posture", timeout=5)
    print(f"  4. fleet/posture-> {r.status_code} {r.text}")
    print()
    print("✔ node attested Trusted. If fleet posture is now Nominal the actuator "
          "gate will admit /actuator/motion/command; if still LockedOut, give the "
          "posture engine a moment to recalc and re-GET /fleet/posture.")


if __name__ == "__main__":
    main()
