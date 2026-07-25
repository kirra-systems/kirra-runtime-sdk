#!/usr/bin/env python3
"""
camera_detect — the classical-CV camera detector NODE (the thin hop).

Subscribes the colour + depth + camera_info triple, does the pixel work (HSV
threshold → morphology → connected components, plus a coarse depth grid), and
publishes ONE JSON frame on `~/detections`:

    {"usable": true, "stamp_ms": 1234, "frame_valid_fraction": 0.87,
     "detections": [ {"class":"static_obstacle","near_x_m":…,
                      "lateral_min_m":…,"lateral_max_m":…}, … ],
     "targets":    [ {"label":"red","x":2.4,"y":0.3,"confidence":0.8}, … ]}

`occy_doer` consumes it and forwards `detections` to Taj `POST /perception`
(camera_armed) and `targets` to Occy `POST /plan` (object_goal) — see
`docs/hardware/CAMERA_PERCEPTION_INTEGRATION.md`.

Every decision that could affect motion lives in the PURE core
(`camera_detect_core.py`, host-tested with no ROS/cv2/camera). This file only
turns images into the scalars that core consumes. In particular the node never
invents a stamp: when the depth frame is not actually usable the core withholds
it, and the consumer's camera channel then faults to the MRC floor.

🔴 The camera is a DOER input. `detections` can only TIGHTEN the lidar corridor
   and `targets` are destinations — neither can authorize motion. Lidar remains
   the sole authority on free space.
"""

import json
import math

import rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy, HistoryPolicy
from sensor_msgs.msg import CameraInfo, Image
from std_msgs.msg import String

from kirra_safety.camera_detect_core import (
    Blob,
    CameraExtrinsics,
    CameraIntrinsics,
    DetectConfig,
    build_detection_frame,
    colour_bands,
    known_colours,
)

try:
    import cv2
    import numpy as np
    CV_AVAILABLE = True
except ImportError:  # pragma: no cover - exercised only on a box without cv2
    CV_AVAILABLE = False

# Sensor-ingress QoS: freshness over buffering, and BestEffort matches both
# BestEffort and Reliable publishers (the house discipline — see occy_doer).
IMAGE_QOS = QoSProfile(
    reliability=ReliabilityPolicy.BEST_EFFORT,
    history=HistoryPolicy.KEEP_LAST,
    depth=1,
)


class CameraDetect(Node):
    def __init__(self):
        super().__init__('camera_detect')
        self.declare_parameter('color_topic', '/camera/color/image_raw')
        self.declare_parameter('depth_topic', '/camera/depth/image_raw')
        self.declare_parameter('info_topic', '/camera/color/camera_info')
        self.declare_parameter('detections_topic', '~/detections')
        self.declare_parameter('detect_hz', 5.0)
        # Which colour terms to look for. The classical detector grounds COLOUR,
        # not nouns (see the core's docstring) — "red cup" is reduced to "red".
        self.declare_parameter('colours', ['red', 'blue', 'green'])
        # Mounting, in the ego frame (+X fwd, +Y left, +Z up). pitch_deg is the
        # DOWNWARD tilt. These must match the physical mount or every detection
        # lands in the wrong place — validate against /scan before arming.
        self.declare_parameter('mount_x_m', 0.10)
        self.declare_parameter('mount_y_m', 0.0)
        self.declare_parameter('mount_z_m', 0.20)
        self.declare_parameter('mount_pitch_deg', 0.0)
        # Detector thresholds (the pure core's DetectConfig).
        self.declare_parameter('min_pixels', 200)
        self.declare_parameter('max_range_m', 6.0)
        self.declare_parameter('ground_z_m', 0.03)
        self.declare_parameter('min_frame_valid_fraction', 0.20)
        self.declare_parameter('obstacle_bin_m', 0.20)
        # Depth grid stride in pixels — the drivability channel samples rather
        # than projecting every pixel, so the per-tick cost stays bounded.
        self.declare_parameter('depth_stride_px', 8)

        self._colours = [c for c in self.get_parameter('colours').value
                         if colour_bands(c) is not None]
        rejected = [c for c in self.get_parameter('colours').value
                    if colour_bands(c) is None]
        if rejected:
            self.get_logger().warn(
                f'ignoring unknown colour term(s) {rejected} — known: {list(known_colours())}')
        if not self._colours:
            # Fail-closed on configuration: a detector asked to find nothing
            # would publish empty `targets` forever, which reads as "the object
            # is not there" rather than "I was never told to look".
            self.get_logger().fatal(
                'no known colour terms configured — refusing to start. '
                f'Set the `colours` parameter to a subset of {list(known_colours())}.')
            raise SystemExit(2)

        self._extr = CameraExtrinsics(
            x_m=self.get_parameter('mount_x_m').value,
            y_m=self.get_parameter('mount_y_m').value,
            z_m=self.get_parameter('mount_z_m').value,
            pitch_rad=math.radians(self.get_parameter('mount_pitch_deg').value),
        )
        self._cfg = DetectConfig(
            min_pixels=int(self.get_parameter('min_pixels').value),
            max_range_m=float(self.get_parameter('max_range_m').value),
            ground_z_m=float(self.get_parameter('ground_z_m').value),
            min_frame_valid_fraction=float(
                self.get_parameter('min_frame_valid_fraction').value),
            obstacle_bin_m=float(self.get_parameter('obstacle_bin_m').value),
        )
        self._stride = max(1, int(self.get_parameter('depth_stride_px').value))

        self._intr = None     # from camera_info; no calibration -> no projection
        self._color = None
        self._depth = None

        self._pub = self.create_publisher(
            String, self.get_parameter('detections_topic').value, 10)
        self.create_subscription(
            Image, self.get_parameter('color_topic').value, self._on_color, IMAGE_QOS)
        self.create_subscription(
            Image, self.get_parameter('depth_topic').value, self._on_depth, IMAGE_QOS)
        self.create_subscription(
            CameraInfo, self.get_parameter('info_topic').value, self._on_info, 10)
        self.create_timer(1.0 / float(self.get_parameter('detect_hz').value), self._tick)

        if not CV_AVAILABLE:
            self.get_logger().error(
                'python3-opencv / numpy missing — detector publishes UNUSABLE frames '
                '(a consumer that armed the camera channel will hold).')
        self.get_logger().info(
            f'camera_detect: colours={self._colours} mount={self._extr} -> '
            f'{self.get_parameter("detections_topic").value}')

    # --- subscriptions ------------------------------------------------------
    def _on_info(self, msg: CameraInfo):
        k = list(msg.k)
        if len(k) >= 6:
            self._intr = CameraIntrinsics(fx=k[0], fy=k[4], cx=k[2], cy=k[5])

    def _on_color(self, msg: Image):
        self._color = msg

    def _on_depth(self, msg: Image):
        self._depth = msg

    # --- image decoding -----------------------------------------------------
    def _depth_metres(self, msg: Image):
        """Depth image -> float32 metres, NaN where invalid. `None` if undecodable."""
        enc = msg.encoding.lower()
        if enc == '16uc1' or enc == 'mono16':
            raw = np.frombuffer(msg.data, dtype=np.uint16).reshape(msg.height, msg.width)
            d = raw.astype(np.float32) / 1000.0          # millimetres -> metres
            d[raw == 0] = np.nan                          # 0 is the "no return" sentinel
            return d
        if enc == '32fc1':
            d = np.frombuffer(msg.data, dtype=np.float32).reshape(
                msg.height, msg.width).astype(np.float32).copy()
            d[~np.isfinite(d)] = np.nan
            d[d <= 0.0] = np.nan
            return d
        return None   # unknown encoding -> refuse, never reinterpret bytes

    def _bgr(self, msg: Image):
        """Colour image -> BGR uint8, or `None` for an encoding we will not guess at."""
        enc = msg.encoding.lower()
        if enc in ('bgr8', 'rgb8'):
            a = np.frombuffer(msg.data, dtype=np.uint8).reshape(msg.height, msg.width, 3)
            return a if enc == 'bgr8' else a[:, :, ::-1]
        return None

    # --- the detector tick --------------------------------------------------
    def _unusable(self, why: str):
        """Publish an explicitly-unusable frame — never a silently-empty one."""
        self.get_logger().debug(f'unusable frame: {why}')
        self._pub.publish(String(data=json.dumps(build_detection_frame(
            blobs=[], points=[], frame_valid_fraction=0.0, stamp_ms=0,
            intr=self._intr or CameraIntrinsics(0, 0, 0, 0), extr=self._extr,
            cfg=self._cfg))))

    def _tick(self):
        if not CV_AVAILABLE:
            return self._unusable('no-opencv')
        if self._intr is None or not self._intr.valid():
            return self._unusable('no-camera_info')
        if self._color is None or self._depth is None:
            return self._unusable('awaiting-frames')

        color_msg, depth_msg = self._color, self._depth
        depth = self._depth_metres(depth_msg)
        bgr = self._bgr(color_msg)
        if depth is None:
            return self._unusable(f'depth-encoding:{depth_msg.encoding}')
        if bgr is None:
            return self._unusable(f'color-encoding:{color_msg.encoding}')
        if depth.shape[:2] != bgr.shape[:2]:
            # An unregistered pair would give every blob the WRONG depth, which
            # would put a target metres from the real object. Refuse.
            return self._unusable(
                f'depth {depth.shape[:2]} not registered to color {bgr.shape[:2]} '
                '(enable the driver depth_registration option)')

        stamp_ms = int(depth_msg.header.stamp.sec * 1000
                       + depth_msg.header.stamp.nanosec // 1_000_000)

        # --- drivability channel: a coarse depth grid -> ego points ---------
        sub = depth[::self._stride, ::self._stride]
        vs, us = np.nonzero(np.isfinite(sub))
        total = sub.size
        in_range = 0
        points = []
        if total:
            for gv, gu in zip(vs.tolist(), us.tolist()):
                d = float(sub[gv, gu])
                if d <= 0.0 or d > self._cfg.max_range_m:
                    continue
                in_range += 1
                p = self._project(gu * self._stride, gv * self._stride, d)
                if p is not None:
                    points.append(p)
        frame_valid_fraction = (in_range / float(total)) if total else 0.0

        # --- goal channel: colour blobs -> labelled targets -----------------
        blobs = self._find_blobs(bgr, depth)

        frame = build_detection_frame(
            blobs=blobs, points=points, frame_valid_fraction=frame_valid_fraction,
            stamp_ms=stamp_ms, intr=self._intr, extr=self._extr, cfg=self._cfg)
        self._pub.publish(String(data=json.dumps(frame)))
        self.get_logger().debug(
            f'usable={frame["usable"]} valid={frame_valid_fraction:.2f} '
            f'obstacles={len(frame["detections"])} targets={len(frame["targets"])}')

    def _project(self, u, v, d):
        from kirra_safety.camera_detect_core import pixel_to_ego
        return pixel_to_ego(u, v, d, self._intr, self._extr, self._cfg.max_range_m)

    def _find_blobs(self, bgr, depth):
        """HSV threshold -> morphology -> components -> one Blob per region."""
        hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)
        kernel = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (5, 5))
        out = []
        for colour in self._colours:
            bands = colour_bands(colour)
            mask = None
            for lo, hi in bands:
                m = cv2.inRange(hsv, np.array(lo, dtype=np.uint8),
                                np.array(hi, dtype=np.uint8))
                mask = m if mask is None else cv2.bitwise_or(mask, m)
            mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, kernel)
            mask = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, kernel)
            n, labels, stats, centroids = cv2.connectedComponentsWithStats(mask, 8)
            for i in range(1, n):   # 0 is the background component
                area = int(stats[i, cv2.CC_STAT_AREA])
                if area < self._cfg.min_pixels:
                    continue
                region = depth[labels == i]
                finite = region[np.isfinite(region)]
                in_range = finite[(finite > 0.0) & (finite <= self._cfg.max_range_m)]
                if in_range.size == 0:
                    continue        # no usable depth -> the core would drop it anyway
                out.append(Blob(
                    colour=colour,
                    u=float(centroids[i][0]),
                    v=float(centroids[i][1]),
                    pixel_count=area,
                    depth_m=float(np.median(in_range)),   # robust to blob edges
                    depth_valid_fraction=float(in_range.size) / float(area),
                ))
        return out


def main(args=None):
    rclpy.init(args=args)
    node = CameraDetect()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == '__main__':
    main()
