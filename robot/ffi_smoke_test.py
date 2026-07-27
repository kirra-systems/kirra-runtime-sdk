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
from kirra_ffi import (  # noqa: E402
    BOUND_PAYLOAD_LEN,
    BOUND_REFUSAL_NAMES,
    BoundKirraConsumer,
    KirraConsumer,
    bound_refusal_name,
    split_bound_frame,
    split_frame,
)

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
    """The strict wire parse the motor consumer uses (kirra_ffi.split_frame);
    minted frames are always well-formed, so None here is a test bug."""
    parsed = split_frame(frame)
    assert parsed is not None, f"minted frame has malformed length {len(frame)}"
    return parsed


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

    # (g) Malformed wire lengths are rejected by the strict parse the motor
    # consumer uses (Copilot #901): never sliced into an oversized token, never
    # a crash — the frame is simply not a frame.
    for n in (0, 31, 33, 127, 129, 256):
        check(split_frame(b"\x00" * n) is None, f"(g) length {n} must parse as malformed")
    check(split_frame(b"\x00" * 32) == (b"\x00" * 32, None), "(g) exact 32 → unsigned")
    p128 = split_frame(b"\x01" * 128)
    check(p128 is not None and len(p128[1]) == 96, "(g) exact 128 → token of exactly 96")
    print("(g) malformed lengths (0/31/33/127/129/256) → rejected by the strict parse")

    # ------------------------------------------------------------------
    # (h) The evidence-bound V2 boundary, against the REAL .so, driven by
    # real governor-signed frames from `kirra_ros_release_mint frame-v2`.
    #
    # Same shape as the V1 half above: a positive release proves the marshalling
    # is non-vacuous, and each refusal arm proves the core is actually policing
    # the evidence binding rather than waving bytes through.
    # ------------------------------------------------------------------
    PROFILE_HEX = "ab" * 32
    profile_digest = bytes.fromhex(PROFILE_HEX)

    def v2(seq: str, *extra: str) -> tuple[bytes, bytes]:
        """A signed 272-byte V2 frame, split ready for the FFI.

        Defaults describe a releasable command; `extra` overrides whichever
        field a negative case needs to break. `extra` is passed BEFORE the
        defaults because the minter takes the FIRST occurrence of a flag
        (`arg_val` uses `position`), so an earlier value wins.
        """
        frame = mint("frame-v2", "--seq", seq, *extra,
                     "--issued-ms", "10000", "--linear", "0.1", "--angular", "0.0",
                     "--profile-digest", PROFILE_HEX)
        parsed = split_bound_frame(frame)
        assert parsed is not None, f"minted V2 frame has bad length {len(frame)}"
        return parsed

    def new_bound(vk_bytes=vk, digest=profile_digest, **over):
        kwargs = dict(
            maximum_token_lifetime_ms=200, control_period_ms=100, missed_periods=3,
            stop_decel_mps2=1.0, vx_max=VX_MAX, vz_max=VZ_MAX,
        )
        kwargs.update(over)
        return BoundKirraConsumer(vk_bytes, digest, **kwargs)

    # (h1) A well-formed configuration constructs.
    bc = new_bound()
    check(bc is not None, "(h1) a valid V2 configuration must construct")

    # (h2) Fail-closed configurations must raise, never yield a usable handle.
    for label, over in (
        ("all-zero profile digest", {"digest": b"\x00" * 32}),
        ("zero maximum lifetime", {"maximum_token_lifetime_ms": 0}),
        ("non-finite vx_max", {"vx_max": float("inf")}),
        ("zero vx_max", {"vx_max": 0.0}),
    ):
        try:
            new_bound(**over)
        except ValueError:
            pass
        else:
            check(False, f"(h2) {label} must be refused (fail-closed NULL handle)")
    print("(h2) bad V2 configs (zero digest / zero lifetime / bad envelope) → refused")

    # (h3) A tokenless V2 frame → NO_TOKEN, no write.
    bc = new_bound()
    r = bc.on_frame(b"\x00" * BOUND_PAYLOAD_LEN, None, 10_000)
    check(r.write == 0, "(h3) a tokenless V2 frame must not actuate")
    check(r.refusal_code == 100,
          f"(h3) tokenless must be NO_TOKEN(100), got {r.refusal_code}")
    print(f"(h3) V2 tokenless → refused code=NO_TOKEN write={r.write}")

    # (h4) A V2 frame this core never signed → refused, no write.
    bc = new_bound()
    forged = bytes(range(256))[:BOUND_PAYLOAD_LEN]
    r = bc.on_frame(forged, b"\x5a" * 96, 10_000)
    check(r.write == 0, "(h4) an unsigned-by-us V2 frame must not actuate")
    check(r.refusal_code in BOUND_REFUSAL_NAMES,
          f"(h4) refusal must be a known V2 code, got {r.refusal_code}")
    print(f"(h4) V2 forged → refused code={bound_refusal_name(r.refusal_code)} write={r.write}")

    # (h5) The strict V2 parse: only 272, and never a V1 downgrade.
    check(split_bound_frame(b"\x00" * 272) is not None, "(h5) exact 272 must parse")
    for n in (0, 32, 96, 128, 176, 271, 273):
        check(split_bound_frame(b"\x00" * n) is None, f"(h5) length {n} must be refused")
    print("(h5) V2 parse: 272 only (0/32/96/128/176/271/273 refused, incl. V1 128)")

    # (h6) Health starts clean and counts the refusals we just caused.
    h = bc.health()
    check(h.releases == 0, "(h6) no releases can have occurred")
    check(h.refusals_total >= 1, "(h6) the forged frame must be counted")
    print(f"(h6) V2 health: releases={h.releases} refusals_total={h.refusals_total}")

    # (h7) close() is idempotent and the handle is not reusable.
    bc.close()
    bc.close()
    try:
        bc.on_tick(10_000)
    except ValueError:
        print("(h7) V2 close is idempotent; a closed handle refuses reuse")
    else:
        check(False, "(h7) a closed V2 consumer must refuse reuse")

    # (h8) A real governor-signed V2 command ABOVE the demo envelope →
    # Released + clamped, with the bound evidence identity surfaced.
    bc = new_bound()
    payload, token = v2("1", "--linear", "0.5", "--angular", "2.0",
                        "--scan-seq", "813", "--tracker-gen", "7",
                        "--camera-seq", "41")
    r = bc.on_frame(payload, token, 10_050)
    check(r.kind == 0, f"(h8) a valid V2 frame must Release, got kind={r.kind}")
    check(r.write == 1, "(h8) a V2 release must actuate")
    check(abs(r.linear - VX_MAX) < 1e-9, f"(h8) linear must clamp to {VX_MAX}, got {r.linear}")
    check(abs(r.angular - VZ_MAX) < 1e-9, f"(h8) angular must clamp to {VZ_MAX}, got {r.angular}")
    check(r.scan_sequence == 813 and r.tracker_generation == 7,
          f"(h8) bound evidence must survive the FFI, got scan={r.scan_sequence} "
          f"gen={r.tracker_generation}")
    print(f"(h8) V2 valid → Released write={r.write} twist=({r.linear:.3f},{r.angular:.3f}) "
          f"[clamped] seq={r.sequence} nonce={r.nonce} scan={r.scan_sequence} gen={r.tracker_generation}")

    # (h9) Replay of an already-released V2 frame → SEQUENCE_NOT_ADVANCED.
    r = bc.on_frame(payload, token, 10_060)
    check(r.write == 0 and r.refusal_code == 107,
          f"(h9) V2 replay must be SEQUENCE_NOT_ADVANCED(107), got {r.refusal_code}")
    print(f"(h9) V2 replay → refused code={bound_refusal_name(r.refusal_code)}")

    # (h10) The evidence gates, each reached by breaking exactly one field.
    # This is what makes the binding non-decorative: the SAME governor key
    # signs every one of these, so only the bound evidence can refuse them.
    for label, code, consumer_digest, extra in (
        ("wrong platform profile", 109, b"\xcd" * 32, ()),
        ("expired token", 105, profile_digest, ("--lifetime-ms", "50")),
        ("over-long lifetime", 106, profile_digest, ("--lifetime-ms", "999999")),
        ("corrupt signature", 102, profile_digest, ("--corrupt-sig",)),
    ):
        c2 = new_bound(digest=consumer_digest)
        p2, t2 = v2("1", *extra)
        # `expired token` needs a clock past the 50 ms lifetime; the rest are
        # judged at a nominal in-window time.
        at = 10_500 if code == 105 else 10_050
        rr = c2.on_frame(p2, t2, at)
        check(rr.write == 0, f"(h10) {label} must not actuate")
        check(rr.refusal_code == code,
              f"(h10) {label} must be code {code} "
              f"({bound_refusal_name(code)}), got {rr.refusal_code} "
              f"({bound_refusal_name(rr.refusal_code)})")
        print(f"(h10) V2 {label} → refused code={bound_refusal_name(rr.refusal_code)}")

    # (h11) A superseded perception frame is refused even though the command
    # sequence and nonce both advanced — evidence age is its own gate.
    c2 = new_bound()
    c2.on_frame(*v2("1", "--scan-seq", "9"), 10_050)
    r = c2.on_frame(*v2("2", "--scan-seq", "5"), 10_060)
    check(r.write == 0 and r.refusal_code == 110,
          f"(h11) a stale perception frame must be PERCEPTION_FRAME_SUPERSEDED(110), "
          f"got {r.refusal_code}")
    print(f"(h11) V2 stale evidence → refused code={bound_refusal_name(r.refusal_code)}")

    # (h12) Starvation on the V2 core → the SS-002 decel ramp, then silence.
    c2 = new_bound()
    c2.on_frame(*v2("1", "--linear", "0.15"), 10_000)
    ramp = [t.linear for t in
            (c2.on_tick(10_000 + 300 + k * 100) for k in range(1, 21)) if t.write == 1]
    check(len(ramp) >= 1, "(h12) V2 starvation must produce an active stop ramp")
    check(ramp[-1] == 0.0, f"(h12) V2 ramp must reach zero, got {ramp}")
    check(all(a >= b - 1e-12 for a, b in zip(ramp, ramp[1:])),
          f"(h12) V2 ramp must not increase: {ramp}")
    check(c2.health().silent == 1, "(h12) after the ramp the V2 consumer must be silent")
    print(f"(h12) V2 starvation → ramp={[round(x, 3) for x in ramp]} silent=True")

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
