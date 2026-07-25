#!/usr/bin/env python3
"""
Classical-CV camera detector — the PURE core (no rclpy, no cv2, no numpy).

This is the producer behind the two camera channels documented in
`docs/hardware/CAMERA_PERCEPTION_INTEGRATION.md`:

  1. **Drivability** (tighten-only)  — depth → `static_obstacle` regions that may
     only SHORTEN the Taj Phase-A corridor, never extend it.
  2. **Where a named thing is**      — a colour blob → a `LabeledTarget`, i.e. a
     DESTINATION for `object_goal`. Carries no authority over the ground.

Everything here is pure arithmetic on scalars so it is host-testable with no ROS,
no camera and no OpenCV. The node (`camera_detect.py`) owns the pixel work
(HSV threshold, morphology, connected components) and calls into this module for
every decision that could affect motion.

🔴 HONEST SCOPE — the classical detector grounds **colour, not nouns.**
   It can confirm "there is a red thing at 2.4 m"; it cannot confirm the thing is
   a *cup*. So `colour_term("red cup") -> "red"` and the emitted target label is
   the colour term. A caller wanting the noun verified needs the ML detector tier
   (`parko-onnx` / `parko-tensorrt`). Labelling a red blob "red cup" would be a
   fabrication, so this module refuses to do it.

🔴 FAIL-CLOSED RULES (each has a test):
   - No valid depth for a blob  → no target (never a guessed range).
   - Non-finite arithmetic      → dropped, never propagated.
   - A blob behind the camera   → dropped (`x <= 0` is not a destination ahead).
   - A depth frame that is mostly INVALID → `depth_frame_usable() is False`, and
     the node must then withhold `camera_stamp_ms`, so the Taj camera channel
     resolves Faulted → MRC floor. "The detector could not see" is never "clear."
"""

import math
import re
from dataclasses import dataclass
from typing import Iterable, List, Optional, Sequence, Tuple

# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class CameraIntrinsics:
    """Pinhole intrinsics, as published on `camera_info` (`k` = fx,0,cx,0,fy,cy,...)."""

    fx: float
    fy: float
    cx: float
    cy: float

    def valid(self) -> bool:
        return (
            all(math.isfinite(v) for v in (self.fx, self.fy, self.cx, self.cy))
            and self.fx > 0.0
            and self.fy > 0.0
        )


@dataclass(frozen=True)
class CameraExtrinsics:
    """Where the camera sits on the robot, in the EGO frame (+X fwd, +Y left, +Z up).

    `pitch_rad` is the DOWNWARD tilt of the optical axis (0 = level, positive =
    nose down, which is the usual mounting for a robot that must see the floor
    just ahead of itself).
    """

    x_m: float = 0.0
    y_m: float = 0.0
    z_m: float = 0.0
    pitch_rad: float = 0.0

    def valid(self) -> bool:
        return all(
            math.isfinite(v) for v in (self.x_m, self.y_m, self.z_m, self.pitch_rad)
        )


def pixel_to_ego(
    u: float,
    v: float,
    depth_m: float,
    intr: CameraIntrinsics,
    extr: CameraExtrinsics,
    max_range_m: float = 10.0,
) -> Optional[Tuple[float, float, float]]:
    """Project one pixel + its depth into the ego frame. `None` = unusable.

    The camera's OPTICAL frame is the ROS convention (+X right, +Y down,
    +Z along the optical axis), which is what a depth image is expressed in.
    The ego frame is REP-103 (+X forward, +Y left, +Z up).

    Returns `(x_m, y_m, z_m)`; `z_m` lets the caller reject the floor (a return
    at ground height is not an obstacle).
    """
    if not (intr.valid() and extr.valid()):
        return None
    if not math.isfinite(u) or not math.isfinite(v):
        return None
    if not math.isfinite(depth_m) or depth_m <= 0.0 or depth_m > max_range_m:
        return None

    # Optical frame.
    right = (u - intr.cx) / intr.fx * depth_m
    down = (v - intr.cy) / intr.fy * depth_m
    along = depth_m

    # Un-pitch into the ego frame. With a downward tilt theta, the optical axis
    # is (cos t)*F - (sin t)*U and the image-down axis is (sin t)*F - (cos t)*U.
    s, c = math.sin(extr.pitch_rad), math.cos(extr.pitch_rad)
    fwd = along * c + down * s
    up = -along * s - down * c
    left = -right

    x_m = extr.x_m + fwd
    y_m = extr.y_m + left
    z_m = extr.z_m + up
    if not all(math.isfinite(q) for q in (x_m, y_m, z_m)):
        return None
    return (x_m, y_m, z_m)


# ---------------------------------------------------------------------------
# Colour vocabulary
# ---------------------------------------------------------------------------

# OpenCV HSV: H in [0,179], S/V in [0,255]. Red wraps the hue origin, so it
# carries TWO bands — a detector that only takes the upper band misses half of
# every red object. Each entry is a tuple of (lo_hsv, hi_hsv) inclusive bands.
COLOUR_BANDS = {
    "red": (((0, 110, 70), (8, 255, 255)), ((172, 110, 70), (179, 255, 255))),
    "orange": (((9, 130, 90), (21, 255, 255)),),
    "yellow": (((22, 110, 110), (33, 255, 255)),),
    "green": (((36, 80, 60), (85, 255, 255)),),
    "blue": (((90, 90, 60), (128, 255, 255)),),
    "purple": (((129, 70, 60), (158, 255, 255)),),
}

# Ordered longest-first so a two-word colour would win over its prefix; also the
# stable order used when reporting which colours a phrase mentioned.
_COLOUR_WORDS = tuple(sorted(COLOUR_BANDS, key=lambda w: (-len(w), w)))

_WORD_RE = re.compile(r"[a-z]+")


def known_colours() -> Tuple[str, ...]:
    """The colour terms this detector can ground, alphabetically."""
    return tuple(sorted(COLOUR_BANDS))


def colour_term(phrase: str) -> Optional[str]:
    """Reduce an object phrase to the ONE colour term this detector can ground.

    ``"red cup"`` / ``"the RED cup"`` / ``"red"`` -> ``"red"``.

    `None` (refuse) when the phrase names no known colour (``"cup"``) or names
    more than one (``"red blue thing"``) — an ambiguous request must not silently
    pick a colour. The noun is deliberately discarded: see the module docstring.
    """
    if not isinstance(phrase, str):
        return None
    words = set(_WORD_RE.findall(phrase.lower()))
    found = [c for c in _COLOUR_WORDS if c in words]
    if len(found) != 1:
        return None
    return found[0]


def colour_bands(colour: str) -> Optional[Tuple[Tuple[Sequence[int], Sequence[int]], ...]]:
    """The HSV bands for a colour term, or `None` for an unknown one (fail-closed)."""
    if not isinstance(colour, str):
        return None
    return COLOUR_BANDS.get(colour.strip().lower())


# ---------------------------------------------------------------------------
# Blobs -> labelled targets (the goal channel)
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Blob:
    """One connected colour region, already reduced to scalars by the node."""

    colour: str
    u: float  # centroid pixel, horizontal
    v: float  # centroid pixel, vertical
    pixel_count: int
    depth_m: float  # ROBUST (median) depth over the region, metres
    depth_valid_fraction: float  # share of region pixels with a usable depth


@dataclass(frozen=True)
class DetectConfig:
    """Detector thresholds. All safety-relevant knobs live here, not in the node."""

    min_pixels: int = 200
    min_depth_valid_fraction: float = 0.30
    max_range_m: float = 6.0
    # A return at or below this height above the floor is ground, not an object.
    ground_z_m: float = 0.03
    # Depth frames below this valid share mean "could not see" -> withhold the
    # stamp so the Taj camera channel faults to the MRC floor.
    min_frame_valid_fraction: float = 0.20
    # Lateral bin width for the drivability channel.
    obstacle_bin_m: float = 0.20


def blob_confidence(blob: Blob, cfg: DetectConfig) -> float:
    """A calibrated-ish [0,1] score: region size x depth agreement.

    Deliberately conservative — the size term saturates at 4x the minimum area,
    so a huge blob does not buy unlimited confidence, and a region whose depth is
    mostly invalid can never score high no matter how many pixels it has.
    """
    if blob.pixel_count < cfg.min_pixels or cfg.min_pixels <= 0:
        return 0.0
    frac = blob.depth_valid_fraction
    if not math.isfinite(frac) or frac < cfg.min_depth_valid_fraction:
        return 0.0
    size_term = min(1.0, blob.pixel_count / float(4 * cfg.min_pixels))
    return max(0.0, min(1.0, size_term * min(1.0, frac)))


def blob_to_target(
    blob: Blob,
    intr: CameraIntrinsics,
    extr: CameraExtrinsics,
    cfg: DetectConfig,
) -> Optional[dict]:
    """One blob -> the `targets[]` wire row for `POST /plan`, or `None` to drop it.

    The emitted `label` is the COLOUR TERM (see the module docstring) — never a
    noun the detector did not verify.
    """
    conf = blob_confidence(blob, cfg)
    if conf <= 0.0:
        return None
    if colour_bands(blob.colour) is None:
        return None  # unknown colour token -> refuse, never a target
    ego = pixel_to_ego(blob.u, blob.v, blob.depth_m, intr, extr, cfg.max_range_m)
    if ego is None:
        return None
    x_m, y_m, _z_m = ego
    if x_m <= 0.0:
        return None  # not ahead of the ego -> not a destination
    return {
        "label": blob.colour.strip().lower(),
        "x": x_m,
        "y": y_m,
        "confidence": conf,
    }


def blobs_to_targets(
    blobs: Iterable[Blob],
    intr: CameraIntrinsics,
    extr: CameraExtrinsics,
    cfg: DetectConfig,
) -> List[dict]:
    """Every usable blob as a target row, nearest first (stable, deterministic)."""
    rows = [t for t in (blob_to_target(b, intr, extr, cfg) for b in blobs) if t]
    rows.sort(key=lambda t: (t["x"], t["y"], t["label"]))
    return rows


# ---------------------------------------------------------------------------
# Depth -> drivability detections (the tighten-only channel)
# ---------------------------------------------------------------------------


def depth_frame_usable(valid_fraction: float, cfg: DetectConfig) -> bool:
    """Did the depth camera actually SEE this frame?

    Load-bearing: a frame that is mostly invalid (dark, saturated, out of the
    sensor's range, driver hiccup) must NOT be published as "fresh, zero
    detections" — that reads to Taj as *"I looked, it's clear"*. The node
    withholds `camera_stamp_ms` when this is `False`, which makes
    `resolve_camera_channel` return Faulted and floors the speed cap.
    """
    if not math.isfinite(valid_fraction):
        return False
    return valid_fraction >= cfg.min_frame_valid_fraction


def points_to_obstacle_detections(
    points: Iterable[Tuple[float, float, float]],
    cfg: DetectConfig,
) -> List[dict]:
    """Ego-frame points -> `detections[]` rows for `POST /perception`.

    Points are binned laterally; each occupied bin reports its NEAREST return as
    a `static_obstacle`. Because the Taj fusion is tighten-only, a spurious bin
    can only slow the robot down — the failure direction is availability, never
    safety. Ground returns (`z <= ground_z_m`) and out-of-range points are
    dropped, and bins are emitted nearest-first.
    """
    if cfg.obstacle_bin_m <= 0.0:
        return []
    nearest: dict = {}
    for p in points:
        try:
            x, y, z = float(p[0]), float(p[1]), float(p[2])
        except (TypeError, ValueError, IndexError):
            continue
        if not all(math.isfinite(q) for q in (x, y, z)):
            continue
        if x <= 0.0 or x > cfg.max_range_m:
            continue
        if z <= cfg.ground_z_m:
            continue  # floor, not an obstacle
        b = math.floor(y / cfg.obstacle_bin_m)
        if b not in nearest or x < nearest[b]:
            nearest[b] = x
    rows = [
        {
            "class": "static_obstacle",
            "near_x_m": x,
            "lateral_min_m": b * cfg.obstacle_bin_m,
            "lateral_max_m": (b + 1) * cfg.obstacle_bin_m,
        }
        for b, x in nearest.items()
    ]
    rows.sort(key=lambda r: (r["near_x_m"], r["lateral_min_m"]))
    return rows


# ---------------------------------------------------------------------------
# The published frame
# ---------------------------------------------------------------------------


def build_detection_frame(
    blobs: Iterable[Blob],
    points: Iterable[Tuple[float, float, float]],
    frame_valid_fraction: float,
    stamp_ms: int,
    intr: CameraIntrinsics,
    extr: CameraExtrinsics,
    cfg: DetectConfig,
) -> dict:
    """Assemble the JSON the node publishes and `occy_doer` forwards.

    `stamp_ms` is **withheld (`None`)** — and `usable` is `False` — whenever the
    depth frame was not actually seen, so a consumer that arms the camera channel
    faults to the MRC floor instead of believing an empty detection list.
    """
    usable = depth_frame_usable(frame_valid_fraction, cfg)
    return {
        "usable": usable,
        "stamp_ms": int(stamp_ms) if usable else None,
        "frame_valid_fraction": (
            float(frame_valid_fraction) if math.isfinite(frame_valid_fraction) else 0.0
        ),
        "detections": points_to_obstacle_detections(points, cfg) if usable else [],
        "targets": blobs_to_targets(blobs, intr, extr, cfg) if usable else [],
    }


# ---------------------------------------------------------------------------
# Consumer side: relaying a frame into the sidecar requests
# ---------------------------------------------------------------------------

# Mirrors `kirra_sidecars::taj::DEFAULT_CAMERA_MAX_AGE_MS`.
DEFAULT_CAMERA_MAX_AGE_MS = 500


def camera_perception_fields(
    armed: bool,
    frame: Optional[dict],
    max_age_ms: int = DEFAULT_CAMERA_MAX_AGE_MS,
) -> dict:
    """The camera fields to merge into Taj `POST /perception`.

    A FAITHFUL RELAY, not a second opinion: freshness is Taj's call (it compares
    `camera_stamp_ms` against the request's `stamp_ms`, so both must come from
    the SAME clock — per AOU-TIMESYNC-001 the caller sends the ROS header stamps
    of the scan and the depth frame, never a wall clock mixed with a ROS one).

    - Not armed → `{"camera_armed": false}`: Phase-B never runs, byte-identical
      to the lidar-only endpoint.
    - Armed with a usable frame → relay its stamp and detections.
    - Armed with NO usable frame (detector down, unregistered depth, blind
      frame) → armed but **no stamp**, which resolves Faulted at Taj and floors
      the speed cap. Silence must never be relayed as an empty detection list.
    """
    if not armed:
        return {"camera_armed": False}
    out = {"camera_armed": True, "camera_max_age_ms": int(max_age_ms)}
    if isinstance(frame, dict) and frame.get("usable") is True:
        stamp = frame.get("stamp_ms")
        if isinstance(stamp, int) and not isinstance(stamp, bool) and stamp >= 0:
            out["camera_stamp_ms"] = stamp
            out["detections"] = [
                d for d in (frame.get("detections") or []) if isinstance(d, dict)
            ]
    return out


def plan_object_goal_fields(
    phrase: Optional[str],
    frame: Optional[dict],
    now_ms: int,
) -> Tuple[Optional[dict], Optional[str]]:
    """The object-goal fields for Occy `POST /plan`, or a refusal reason.

    Returns `(fields, None)` when the request can be made, else `(None, why)` —
    and a refusal means the caller HOLDS. It must never fall back to some other
    goal: the operator asked for a specific thing, and quietly driving somewhere
    else would be worse than not moving.

    `phrase` is the operator's words ("drive to the red cup"); it is reduced to
    the one colour term this detector can ground. The planner does the rest
    (label match, confidence floor, ambiguity, staleness) and fails closed.
    """
    if phrase is None or not str(phrase).strip():
        return (None, None)  # no object goal requested — not a refusal
    colour = colour_term(phrase)
    if colour is None:
        return (None, f'cannot ground "{phrase}" — name one known colour '
                      f'({", ".join(known_colours())})')
    if not (isinstance(frame, dict) and frame.get("usable") is True):
        return (None, "no usable camera frame")
    stamp = frame.get("stamp_ms")
    if not (isinstance(stamp, int) and not isinstance(stamp, bool) and stamp >= 0):
        return (None, "camera frame carries no stamp")
    return ({
        "object_goal": colour,
        "targets": [t for t in (frame.get("targets") or []) if isinstance(t, dict)],
        "targets_stamp_ms": stamp,
        "now_ms": int(now_ms),
    }, None)


def object_goal_reached(phrase: Optional[str], frame: Optional[dict], tol_m: float) -> bool:
    """Has the ego arrived at the requested thing?

    Without this the loop would oscillate at close range: a target inside the
    goal tolerance keeps being planned toward, then drops out of the detector's
    usable band (`x <= 0`, or too near for depth), the planner refuses, the doer
    holds, the target reappears, and the robot nudges forward again. Latching
    "arrived" on the same range test the positional goal uses makes the stop
    deliberate instead of emergent.

    `False` whenever the answer is not knowable (no phrase, no frame, no matching
    target) — arrival is a positive claim, and not-seen must fall through to the
    planner's own fail-closed refusal, not be reported as "we are there".
    """
    colour = colour_term(phrase) if phrase else None
    if colour is None or not isinstance(frame, dict) or frame.get("usable") is not True:
        return False
    if not math.isfinite(tol_m) or tol_m <= 0.0:
        return False
    for t in frame.get("targets") or []:
        if not isinstance(t, dict) or str(t.get("label", "")).strip().lower() != colour:
            continue
        try:
            x, y = float(t["x"]), float(t["y"])
        except (KeyError, TypeError, ValueError):
            continue
        if math.isfinite(x) and math.isfinite(y) and math.hypot(x, y) <= tol_m:
            return True
    return False
