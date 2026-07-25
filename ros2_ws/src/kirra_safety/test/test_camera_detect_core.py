#!/usr/bin/env python3
"""Host tests for the classical-CV camera detector core (no ROS, no cv2, no camera)."""

import math
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from kirra_safety.camera_detect_core import (  # noqa: E402
    Blob,
    CameraExtrinsics,
    CameraIntrinsics,
    DetectConfig,
    blob_confidence,
    blob_to_target,
    blobs_to_targets,
    build_detection_frame,
    camera_perception_fields,
    colour_bands,
    colour_term,
    depth_frame_usable,
    known_colours,
    object_goal_reached,
    pixel_to_ego,
    plan_object_goal_fields,
    points_to_obstacle_detections,
)

# A plausible 640x480 depth camera, mounted 0.20 m up, 0.10 m forward of the
# ego origin, tilted 15 degrees down so it sees the floor just ahead.
INTR = CameraIntrinsics(fx=520.0, fy=520.0, cx=320.0, cy=240.0)
LEVEL = CameraExtrinsics(x_m=0.10, y_m=0.0, z_m=0.20, pitch_rad=0.0)
TILTED = CameraExtrinsics(x_m=0.10, y_m=0.0, z_m=0.20, pitch_rad=math.radians(15.0))
CFG = DetectConfig()


# --- geometry ---------------------------------------------------------------

def test_principal_point_on_a_level_camera_projects_straight_ahead():
    x, y, z = pixel_to_ego(INTR.cx, INTR.cy, 2.0, INTR, LEVEL)
    assert x == 2.1              # 0.10 mount offset + 2.0 depth
    assert y == 0.0              # dead centre -> no lateral offset
    assert z == 0.2              # stays at mount height


def test_right_of_centre_is_negative_y_left_of_centre_is_positive():
    # Ego frame is REP-103: +Y is LEFT. A pixel to the RIGHT of the principal
    # point must therefore land at negative y. Getting this backwards would
    # mirror every detection.
    _, y_right, _ = pixel_to_ego(INTR.cx + 104.0, INTR.cy, 2.0, INTR, LEVEL)
    _, y_left, _ = pixel_to_ego(INTR.cx - 104.0, INTR.cy, 2.0, INTR, LEVEL)
    assert y_right < 0.0
    assert y_left > 0.0
    assert math.isclose(y_right, -0.4, abs_tol=1e-9)   # 104/520 * 2.0
    assert math.isclose(y_left, 0.4, abs_tol=1e-9)


def test_a_downward_tilt_shortens_the_forward_reach_and_lowers_the_point():
    # The same optical-axis return, once the camera is nosed down 15 deg, is
    # nearer in x and below the mount height.
    fx, _, fz = pixel_to_ego(INTR.cx, INTR.cy, 2.0, INTR, TILTED)
    assert math.isclose(fx, 0.10 + 2.0 * math.cos(math.radians(15.0)), abs_tol=1e-9)
    assert math.isclose(fz, 0.20 - 2.0 * math.sin(math.radians(15.0)), abs_tol=1e-9)
    assert fz < 0.0  # 2 m along a 15 deg down-axis is below the floor plane


def test_a_camera_looking_straight_down_reports_the_floor_beneath_it():
    down = CameraExtrinsics(x_m=0.0, y_m=0.0, z_m=0.5, pitch_rad=math.radians(90.0))
    x, y, z = pixel_to_ego(INTR.cx, INTR.cy, 0.5, INTR, down)
    assert math.isclose(x, 0.0, abs_tol=1e-9)
    assert math.isclose(y, 0.0, abs_tol=1e-9)
    assert math.isclose(z, 0.0, abs_tol=1e-9)   # exactly the ground


def test_unusable_depth_and_bad_calibration_are_refused_not_guessed():
    for bad_depth in (0.0, -1.0, float('nan'), float('inf'), 99.0):
        assert pixel_to_ego(INTR.cx, INTR.cy, bad_depth, INTR, LEVEL) is None
    assert pixel_to_ego(float('nan'), INTR.cy, 2.0, INTR, LEVEL) is None
    assert pixel_to_ego(INTR.cx, INTR.cy, 2.0, CameraIntrinsics(0.0, 520.0, 320.0, 240.0), LEVEL) is None
    bad_extr = CameraExtrinsics(pitch_rad=float('nan'))
    assert pixel_to_ego(INTR.cx, INTR.cy, 2.0, INTR, bad_extr) is None


# --- colour vocabulary ------------------------------------------------------

def test_colour_term_reduces_a_phrase_to_the_one_colour_it_can_ground():
    assert colour_term('red cup') == 'red'
    assert colour_term('the RED  cup') == 'red'
    assert colour_term('red') == 'red'
    assert colour_term('drive to the blue box') == 'blue'


def test_colour_term_refuses_no_colour_and_refuses_ambiguity():
    # No colour at all: the classical detector cannot ground a bare noun.
    assert colour_term('cup') is None
    assert colour_term('') is None
    assert colour_term(None) is None
    # Two colours: must NOT silently pick one.
    assert colour_term('red blue thing') is None


def test_colour_term_does_not_match_a_colour_inside_another_word():
    # "prepared" contains "red"; word-boundary matching must not fire.
    assert colour_term('prepared meal') is None


def test_red_carries_both_hue_bands_because_red_wraps_the_hue_origin():
    bands = colour_bands('red')
    assert len(bands) == 2, 'a single-band red misses half of every red object'
    los = sorted(b[0][0] for b in bands)
    assert los[0] == 0 and los[1] > 150
    for other in ('blue', 'green', 'yellow'):
        assert len(colour_bands(other)) == 1


def test_unknown_colour_tokens_are_refused():
    assert colour_bands('chartreuse') is None
    assert colour_bands('') is None
    assert colour_bands(None) is None
    assert 'red' in known_colours() and 'blue' in known_colours()


# --- blobs -> targets -------------------------------------------------------

def _blob(**kw):
    base = dict(colour='red', u=INTR.cx, v=INTR.cy, pixel_count=1000,
                depth_m=2.0, depth_valid_fraction=0.9)
    base.update(kw)
    return Blob(**base)


def test_a_solid_blob_becomes_a_target_labelled_with_the_colour_term_only():
    t = blob_to_target(_blob(), INTR, LEVEL, CFG)
    # The label is the COLOUR, never a noun the detector did not verify.
    assert t['label'] == 'red'
    assert math.isclose(t['x'], 2.1, abs_tol=1e-9)
    assert math.isclose(t['y'], 0.0, abs_tol=1e-9)
    assert 0.0 < t['confidence'] <= 1.0


def test_a_blob_with_mostly_invalid_depth_is_dropped_never_range_guessed():
    b = _blob(depth_valid_fraction=0.05)
    assert blob_confidence(b, CFG) == 0.0
    assert blob_to_target(b, INTR, LEVEL, CFG) is None


def test_a_speck_is_dropped_and_confidence_rises_with_size_then_saturates():
    assert blob_to_target(_blob(pixel_count=10), INTR, LEVEL, CFG) is None
    small = blob_confidence(_blob(pixel_count=CFG.min_pixels), CFG)
    mid = blob_confidence(_blob(pixel_count=2 * CFG.min_pixels), CFG)
    huge = blob_confidence(_blob(pixel_count=10_000 * CFG.min_pixels), CFG)
    assert small < mid < 1.0
    assert huge <= 1.0
    # A giant blob cannot buy confidence beyond its depth agreement.
    assert blob_confidence(_blob(pixel_count=10**6, depth_valid_fraction=0.5), CFG) <= 0.5


def test_an_unknown_colour_or_out_of_range_blob_never_becomes_a_target():
    assert blob_to_target(_blob(colour='chartreuse'), INTR, LEVEL, CFG) is None
    assert blob_to_target(_blob(depth_m=CFG.max_range_m + 1.0), INTR, LEVEL, CFG) is None
    assert blob_to_target(_blob(depth_m=float('nan')), INTR, LEVEL, CFG) is None


def test_a_blob_that_projects_behind_the_ego_is_not_a_destination():
    # Camera looking straight backwards-and-down: a near return lands at x <= 0.
    behind = CameraExtrinsics(x_m=-1.0, y_m=0.0, z_m=0.2, pitch_rad=0.0)
    assert blob_to_target(_blob(depth_m=0.5), INTR, behind, CFG) is None


def test_targets_are_returned_nearest_first_and_deterministically():
    rows = blobs_to_targets(
        [_blob(depth_m=3.0), _blob(colour='blue', depth_m=1.0), _blob(depth_m=2.0)],
        INTR, LEVEL, CFG)
    xs = [r['x'] for r in rows]
    assert xs == sorted(xs)
    assert rows[0]['label'] == 'blue'
    assert blobs_to_targets([_blob(depth_m=3.0), _blob(colour='blue', depth_m=1.0),
                             _blob(depth_m=2.0)], INTR, LEVEL, CFG) == rows


# --- depth -> drivability detections ---------------------------------------

def test_occupied_bins_become_static_obstacles_at_their_nearest_return():
    pts = [(2.0, 0.05, 0.4), (1.5, 0.07, 0.4), (3.0, -0.5, 0.4)]
    rows = points_to_obstacle_detections(pts, CFG)
    assert [r['class'] for r in rows] == ['static_obstacle', 'static_obstacle']
    # Nearest first; the 0.05/0.07 pair share a bin and collapse to x=1.5.
    assert rows[0]['near_x_m'] == 1.5
    assert rows[0]['lateral_min_m'] <= 0.05 < rows[0]['lateral_max_m']
    assert rows[1]['near_x_m'] == 3.0


def test_ground_returns_and_out_of_range_points_are_not_obstacles():
    ground = [(2.0, 0.0, 0.0), (2.0, 0.0, CFG.ground_z_m)]
    assert points_to_obstacle_detections(ground, CFG) == []
    assert points_to_obstacle_detections([(CFG.max_range_m + 1.0, 0.0, 0.5)], CFG) == []
    assert points_to_obstacle_detections([(-1.0, 0.0, 0.5)], CFG) == []


def test_malformed_and_non_finite_points_are_skipped_not_propagated():
    pts = [(float('nan'), 0.0, 0.5), (2.0, float('inf'), 0.5), ('x', 'y', 'z'),
           (1.0,), None, (2.0, 0.0, 0.5)]
    rows = points_to_obstacle_detections(pts, CFG)
    assert len(rows) == 1
    assert rows[0]['near_x_m'] == 2.0
    assert all(math.isfinite(v) for r in rows for v in
               (r['near_x_m'], r['lateral_min_m'], r['lateral_max_m']))


# --- the fail-closed frame contract ----------------------------------------

def test_a_mostly_invalid_depth_frame_is_not_usable():
    assert depth_frame_usable(0.9, CFG) is True
    assert depth_frame_usable(CFG.min_frame_valid_fraction, CFG) is True
    assert depth_frame_usable(0.01, CFG) is False
    assert depth_frame_usable(float('nan'), CFG) is False


def test_an_unseen_frame_withholds_the_stamp_so_the_consumer_faults_closed():
    # THE load-bearing rule: a frame the camera could not actually see must not
    # be published as "fresh, zero detections" (which reads as "I looked, it's
    # clear"). Withholding the stamp makes resolve_camera_channel go Faulted.
    frame = build_detection_frame(
        blobs=[_blob()], points=[(2.0, 0.0, 0.5)],
        frame_valid_fraction=0.0, stamp_ms=1234, intr=INTR, extr=LEVEL, cfg=CFG)
    assert frame['usable'] is False
    assert frame['stamp_ms'] is None
    assert frame['detections'] == []
    assert frame['targets'] == []


def test_a_seen_frame_publishes_its_stamp_detections_and_targets():
    frame = build_detection_frame(
        blobs=[_blob()], points=[(2.0, 0.0, 0.5)],
        frame_valid_fraction=0.85, stamp_ms=1234, intr=INTR, extr=LEVEL, cfg=CFG)
    assert frame['usable'] is True
    assert frame['stamp_ms'] == 1234
    assert len(frame['detections']) == 1
    assert frame['targets'][0]['label'] == 'red'


def test_a_seen_but_empty_frame_is_clear_not_faulted():
    # Distinct from silence: the camera looked and found nothing. Legitimate.
    frame = build_detection_frame(
        blobs=[], points=[], frame_valid_fraction=0.95, stamp_ms=7,
        intr=INTR, extr=LEVEL, cfg=CFG)
    assert frame['usable'] is True and frame['stamp_ms'] == 7
    assert frame['detections'] == [] and frame['targets'] == []


# --- consumer relay: /perception fields ------------------------------------

def _good_frame(**kw):
    f = build_detection_frame(
        blobs=[_blob()], points=[(2.0, 0.0, 0.5)], frame_valid_fraction=0.9,
        stamp_ms=9000, intr=INTR, extr=LEVEL, cfg=CFG)
    f.update(kw)
    return f


def test_a_disarmed_camera_relays_nothing_so_taj_is_byte_identical():
    assert camera_perception_fields(False, _good_frame()) == {'camera_armed': False}


def test_an_armed_camera_with_a_usable_frame_relays_stamp_and_detections():
    out = camera_perception_fields(True, _good_frame())
    assert out['camera_armed'] is True
    assert out['camera_stamp_ms'] == 9000
    assert len(out['detections']) == 1
    assert out['camera_max_age_ms'] > 0


def test_an_armed_camera_with_no_usable_frame_withholds_the_stamp():
    # Armed but blind: relaying `detections: []` with a fresh stamp would read
    # at Taj as "I looked, it's clear". Withholding the stamp faults the channel
    # to the MRC floor instead.
    for frame in (None, {}, _good_frame(usable=False), _good_frame(stamp_ms=None),
                  _good_frame(stamp_ms='9000'), _good_frame(stamp_ms=True),
                  'not-a-dict'):
        out = camera_perception_fields(True, frame)
        assert out['camera_armed'] is True
        assert 'camera_stamp_ms' not in out, frame
        assert 'detections' not in out, frame


def test_malformed_detection_rows_are_dropped_from_the_relay():
    out = camera_perception_fields(True, _good_frame(detections=[{'class': 'water'}, 7, None]))
    assert out['detections'] == [{'class': 'water'}]


# --- consumer relay: /plan object-goal fields -------------------------------

def test_no_object_goal_requested_is_not_a_refusal():
    for phrase in (None, '', '   '):
        fields, why = plan_object_goal_fields(phrase, _good_frame(), now_ms=9100)
        assert fields is None and why is None


def test_a_grounded_phrase_becomes_the_colour_term_plus_targets():
    fields, why = plan_object_goal_fields('drive to the red cup', _good_frame(), now_ms=9100)
    assert why is None
    assert fields['object_goal'] == 'red'      # colour only — the noun is unverified
    assert fields['targets'][0]['label'] == 'red'
    assert fields['targets_stamp_ms'] == 9000
    assert fields['now_ms'] == 9100


def test_an_ungroundable_phrase_refuses_and_never_falls_back_to_another_goal():
    fields, why = plan_object_goal_fields('drive to the cup', _good_frame(), now_ms=9100)
    assert fields is None
    assert why and 'cannot ground' in why
    fields, why = plan_object_goal_fields('the red blue thing', _good_frame(), now_ms=1)
    assert fields is None and why


def test_an_object_goal_without_a_usable_frame_refuses_rather_than_planning_blind():
    for frame in (None, {}, _good_frame(usable=False), _good_frame(stamp_ms=None)):
        fields, why = plan_object_goal_fields('red cup', frame, now_ms=9100)
        assert fields is None, frame
        assert why, frame


# --- arrival ----------------------------------------------------------------

def _targets_frame(targets):
    return _good_frame(targets=targets)


def test_arrival_latches_when_the_matching_target_is_inside_the_tolerance():
    near = _targets_frame([{'label': 'red', 'x': 0.2, 'y': 0.1, 'confidence': 0.9}])
    assert object_goal_reached('red cup', near, 0.25) is True
    far = _targets_frame([{'label': 'red', 'x': 2.0, 'y': 0.0, 'confidence': 0.9}])
    assert object_goal_reached('red cup', far, 0.25) is False


def test_arrival_only_counts_the_requested_colour():
    other = _targets_frame([{'label': 'blue', 'x': 0.1, 'y': 0.0, 'confidence': 0.9}])
    assert object_goal_reached('red cup', other, 0.25) is False


def test_arrival_is_never_claimed_when_it_cannot_be_known():
    good = _targets_frame([{'label': 'red', 'x': 0.1, 'y': 0.0, 'confidence': 0.9}])
    # Not-seen / not-knowable must fall through to the planner's refusal, never
    # be reported as "we are there".
    assert object_goal_reached(None, good, 0.25) is False
    assert object_goal_reached('cup', good, 0.25) is False          # ungroundable
    assert object_goal_reached('red cup', None, 0.25) is False
    assert object_goal_reached('red cup', _good_frame(usable=False), 0.25) is False
    assert object_goal_reached('red cup', _targets_frame([]), 0.25) is False
    assert object_goal_reached('red cup', good, 0.0) is False       # no tolerance
    assert object_goal_reached('red cup', good, float('nan')) is False


def test_arrival_skips_malformed_target_rows():
    frame = _targets_frame([
        7, None, {'label': 'red'}, {'label': 'red', 'x': 'near', 'y': 0.0},
        {'label': 'red', 'x': float('nan'), 'y': 0.0},
        {'label': 'red', 'x': 0.1, 'y': 0.0},
    ])
    assert object_goal_reached('red cup', frame, 0.25) is True
    without = _targets_frame([7, None, {'label': 'red', 'x': float('inf'), 'y': 0.0}])
    assert object_goal_reached('red cup', without, 0.25) is False
