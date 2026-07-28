"""profile_digest — does the PINNED platform profile digest match what Taj
actually computes for this robot? (opt-in; read-only observability)

WHY THIS EXISTS
---------------
The evidence-bound V2 release binds a platform profile digest into the signed
payload, and the motor consumer refuses any release whose digest differs from
its pinned `KIRRA_PROFILE_DIGEST`. That is the correct fail-closed behaviour,
but its symptom is indistinguishable from a broken signer: EVERY frame is
refused and the robot simply never moves. Nothing else in the stack compares
the two values before a drive, so this module does.

The digest is a pure function of the SAFETY CONFIGURATION Taj was asked to
interpret a scan under (`compute_perception_profile_digest`, taj.rs). It
deliberately excludes observations and timestamps — proven here by probing with
a one-ray scan, which yields the same digest as a full one. So the comparison
needs only the configuration the doer would send:

  * the lidar geometry (angle_min / angle_increment / range_min / range_max),
    read from a LIVE `/scan` — it is a property of the sensor, not a config file;
  * `forward_extent_m`, `camera_armed`, `camera_max_age_ms` — occy_doer's
    parameters, with occy_doer's own defaults;
  * everything else — Taj's serde defaults, because occy_doer never sends them.

OPT-IN, and why
---------------
Unlike the other modules this one shells to `ros2` and POSTs to a sidecar, so it
is DEFAULT=False: run it deliberately (`--module profile_digest`, or `--all`)
during bringup, before a drive. The POST is a query — it cannot actuate and its
result is discarded — but it does advance Taj's internal scan counter, so it
stays out of the always-on read-only set. Advancing that counter is harmless to
the live path (the consumer's supersession watermark only ever refuses OLDER
evidence, and a probe moves it forward, never back).

Every unresolvable input degrades to UNKNOWN, never FAIL: "I could not check
this" must never read like "these disagree".
"""
import json
import urllib.error
import urllib.request

from doctor.core import detail, run_cmd

NAME = "profile_digest"
DESCRIPTION = "pinned KIRRA_PROFILE_DIGEST vs the digest Taj computes"
DEFAULT, HEAVY, TIMEOUT_S = False, False, 25

DIGEST_HEX_LEN = 64
_LOWERCASE_HEX = frozenset("0123456789abcdef")

# occy_doer's own parameter defaults, mirrored so the probe reproduces what the
# doer would send. Keep in step with occy_doer.py's declare_parameter calls and
# camera_detect_core.DEFAULT_CAMERA_MAX_AGE_MS.
#
# NOTE forward_extent_m: occy_doer ALWAYS sends it and defaults to 8.0, which is
# NOT Taj's own default (20.0). Probing without it would compute a digest no
# real frame ever carries.
DOER_FORWARD_EXTENT_M = 8.0
DOER_CAMERA_ARMED = False
DOER_CAMERA_MAX_AGE_MS = 500

# The four sensor-geometry fields the digest covers, as they appear in a
# sensor_msgs/LaserScan and in Taj's request.
SCAN_FIELDS = {
    "angle_min": "angle_min_rad",
    "angle_increment": "angle_increment_rad",
    "range_min": "range_min_m",
    "range_max": "range_max_m",
}


def looks_like_digest(value):
    """True iff `value` is exactly 64 lowercase hex characters."""
    if not isinstance(value, str) or len(value) != DIGEST_HEX_LEN:
        return False
    return all(c in _LOWERCASE_HEX for c in value)


def parse_scan_echo(text):
    """Pull the four geometry fields out of `ros2 topic echo --no-arr` output.

    The output is YAML-ish; with `--no-arr` the big `ranges`/`intensities`
    arrays are replaced by a placeholder string, so only scalars remain. Only
    TOP-LEVEL keys are accepted (no leading whitespace) — `header:` nests its
    own `stamp`/`frame_id`, and matching those would silently pick up the wrong
    number.

    Returns a dict of Taj request field -> float, or None if any of the four is
    missing or unparseable. Pure, so it is testable without ROS.
    """
    if not isinstance(text, str):
        return None
    found = {}
    for line in text.splitlines():
        if not line or line[0].isspace() or ":" not in line:
            continue
        key, _, raw = line.partition(":")
        field = SCAN_FIELDS.get(key.strip())
        if field is None or field in found:
            continue  # first message wins; echo may print several
        try:
            found[field] = float(raw.strip())
        except ValueError:
            continue
    return found if len(found) == len(SCAN_FIELDS) else None


def build_probe(geometry, forward_extent_m, camera_armed, camera_max_age_ms):
    """The Taj request whose profile digest the doer's requests would carry.

    `ranges` carries exactly one ray: the digest excludes observations (the
    module docstring's proven property), and an EMPTY list is not equivalent —
    Taj answers an empty scan with its invalid-frame sentinel (zero sequences,
    empty digests), which would look like a mismatch.

    The camera fields mirror `camera_detect_core.camera_perception_fields`
    EXACTLY, including its omission: disarmed sends only `camera_armed: false`
    and never `camera_max_age_ms`, so Taj's own default is hashed. Sending the
    field anyway would agree only while the two defaults happen to coincide,
    and would silently produce a false mismatch the moment an operator set a
    non-default age with the camera off.
    """
    probe = dict(geometry)
    probe.update({
        "ranges": [1.0],
        "stamp_ms": 0,
        "forward_extent_m": forward_extent_m,
        "camera_armed": camera_armed,
        # #1211: this probe POSTs identical bytes at a fixed stamp on every run,
        # which is exactly what a frozen lidar driver looks like. Flag it so the
        # scan-liveness watchdog does not observe it — a diagnostic must not be
        # able to cause the fault it is diagnosing, nor leave the live stream
        # owing a recovery streak afterwards.
        "diagnostic_probe": True,
    })
    if camera_armed:
        probe["camera_max_age_ms"] = camera_max_age_ms
    return probe


def taj_profile_digest(url, probe, timeout_s=5.0):
    """POST the probe to Taj and return (digest, error). Never raises."""
    try:
        req = urllib.request.Request(
            f"{url}/perception",
            data=json.dumps(probe).encode("utf-8"),
            headers={"content-type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=timeout_s) as r:
            body = json.load(r)
    except urllib.error.HTTPError as e:
        return None, f"HTTP {e.code}"
    except Exception as e:  # noqa: BLE001 — unreachable / bad JSON / timeout
        return None, str(e)

    frame = body.get("frame_id") if isinstance(body, dict) else None
    if not isinstance(frame, dict):
        return None, "response carried no frame_id"
    digest = frame.get("profile_digest")
    if not looks_like_digest(digest):
        # Taj's invalid-frame sentinel is an EMPTY digest — report it as
        # unusable rather than comparing against it.
        return None, f"frame_id.profile_digest not usable ({digest!r})"
    return digest, None


def _doer_settings(env):
    """occy_doer's digest-affecting parameters, from robot.env or its defaults."""
    def num(key, default, cast):
        raw = (env.get(key) or "").strip()
        if not raw:
            return default
        try:
            return cast(raw)
        except ValueError:
            return default

    armed = (env.get("KIRRA_DOER_CAMERA_ARMED") or "").strip().lower()
    return (
        num("KIRRA_DOER_FORWARD_EXTENT_M", DOER_FORWARD_EXTENT_M, float),
        armed in ("1", "true", "yes", "on") if armed else DOER_CAMERA_ARMED,
        num("KIRRA_DOER_CAMERA_MAX_AGE_MS", DOER_CAMERA_MAX_AGE_MS, int),
    )


def run(ctx):
    env, details = ctx["robot_env"], []

    pinned = (env.get("KIRRA_PROFILE_DIGEST") or "").strip()
    if not looks_like_digest(pinned):
        # The `governor` module already FAILs on the format; here it just means
        # there is nothing to compare against.
        details.append(detail(
            "pinned digest available to compare", "UNKNOWN",
            "KIRRA_PROFILE_DIGEST unset or malformed",
            fix="fix the pin first — kirra_doctor.py --module governor"))
        return {"details": details}

    scan_topic = (env.get("KIRRA_SCAN_TOPIC") or "/scan").strip()
    rc, out, err = run_cmd(
        ["ros2", "topic", "echo", "--once", "--no-arr", scan_topic], timeout_s=12)
    geometry = parse_scan_echo(out) if rc == 0 else None
    if geometry is None:
        reason = (f"no {scan_topic} message in 12 s" if rc == 0
                  else (err.strip() or f"ros2 topic echo failed (rc={rc})"))
        details.append(detail(
            "live scan geometry", "UNKNOWN", reason,
            fix=f"start the lidar and re-run; the digest covers {scan_topic}'s "
                "angle/range geometry, so it cannot be checked without a live scan"))
        return {"details": details}
    details.append(detail(
        "live scan geometry", "PASS",
        " ".join(f"{k}={v:g}" for k, v in sorted(geometry.items()))))

    forward_extent_m, camera_armed, camera_max_age_ms = _doer_settings(env)
    taj_url = (env.get("KIRRA_TAJ_URL") or "http://localhost:8101").rstrip("/")
    observed, error = taj_profile_digest(
        taj_url, build_probe(geometry, forward_extent_m, camera_armed, camera_max_age_ms))

    if observed is None:
        details.append(detail(
            "Taj profile digest", "UNKNOWN", f"{taj_url}: {error}",
            fix="start the Taj sidecar (taj_service) and re-run — "
                "R2_LIVE_LOOP_BRINGUP.md"))
        return {"details": details}

    if observed == pinned:
        details.append(detail(
            "pinned digest matches Taj", "PASS", f"{pinned[:16]}…"))
    else:
        details.append(detail(
            "pinned digest matches Taj", "FAIL",
            f"pinned {pinned[:16]}… but Taj computes {observed[:16]}… — every "
            "release would be refused PROFILE_DIGEST_MISMATCH and the robot "
            "would never move",
            fix=f"export KIRRA_PROFILE_DIGEST={observed} (and put it in "
                "/etc/kirra/robot.env) if this robot's configuration is correct; "
                "otherwise fix the doer/lidar config that changed the profile"))

    details.append(detail(
        "probe configuration", "PASS",
        f"forward_extent_m={forward_extent_m:g} camera_armed={camera_armed} "
        f"camera_max_age_ms={camera_max_age_ms} (occy_doer defaults unless "
        "overridden in robot.env)"))
    return {"details": details}
