#!/usr/bin/env python3
"""
Observe a fleet of ego vehicles driving a REAL CARLA city, bounded by the REAL
Kirra governor, and collect doer<->checker divergence data for tuning.

Each synchronous tick, for every ego:
  1. DOER  — a lane-following controller proposes (target speed, steering angle)
             from the CARLA map's waypoints (+ periodic over-reach to exercise
             the checker).
  2. CHECKER — the proposal is POSTed to the live verifier's
               /actuator/motion/command, which bounds BOTH axes.
  3. ACTUATE — the *enforced* command is converted back to a CARLA control and
               applied. (In --shadow it is observed only, autopilot drives.)
  4. CAPTURE — (proposed, enforced, divergence, action) is logged as JSONL.

The spectator camera follows one ego so you watch the city drive live. A
per-run scorecard (intervention rate, mean clamp per axis) prints at the end.

  # CARLA server already running (a Town map), Kirra verifier on :8090
  KIRRA_VERIFIER_URL=http://localhost:8090 KIRRA_ADMIN_TOKEN=test-token \
    python3 scripts/carla_drive_session.py --town Town03 --egos 3 --ticks 4000 --follow 0

  --shadow   autopilot drives; the governor is evaluated but NOT applied (pure
             observation + data collection, non-intrusive).
  --enforce  apply the governor-enforced control (the real doer-checker loop). [default]

SAFETY: tune the DOER from this data (propose checker-admissible commands more
often); NEVER the checker's envelope — the speed cap, the 0.40 m margin, and the
RSS bounds are safety-derived invariants, not knobs.

NOTE: requires a GPU + CARLA server; this script is the host-side harness only.
"""
import argparse
import json
import math
import os
import sys
import time
import urllib.request

import carla  # the CARLA Python egg / pip package must be importable


# --------------------------------------------------------------------------- #
# The real checker — thin client over the verifier's actuator-enforcement API.
# --------------------------------------------------------------------------- #
class Governor:
    def __init__(self, url, token):
        self.endpoint = url.rstrip("/") + "/actuator/motion/command"
        self.token = token

    def enforce(self, proposed):
        req = urllib.request.Request(self.endpoint, data=json.dumps(proposed).encode(), method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", f"Bearer {self.token}")
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.loads(resp.read())


# --------------------------------------------------------------------------- #
# The doer — a lane-following controller proposing (target_v, steer_deg).
# --------------------------------------------------------------------------- #
MAX_STEER_DEG = 35.0   # road-wheel angle that maps to CARLA steer = ±1
CRUISE_MPS = 12.0
LOOKAHEAD_M = 6.0


def speed_of(vehicle):
    v = vehicle.get_velocity()
    return math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z)


def propose(world_map, vehicle, tick):
    """Pure-pursuit-ish lane follow toward the next waypoint, + periodic over-reach."""
    tf = vehicle.get_transform()
    loc = tf.location
    wp = world_map.get_waypoint(loc, project_to_road=True, lane_type=carla.LaneType.Driving)
    nxts = wp.next(LOOKAHEAD_M) if wp else []
    steer_deg = 0.0
    if nxts:
        target = nxts[0].transform.location
        # Heading error between the vehicle's forward and the bearing to the target.
        desired = math.degrees(math.atan2(target.y - loc.y, target.x - loc.x))
        err = (desired - tf.rotation.yaw + 180.0) % 360.0 - 180.0
        steer_deg = max(-MAX_STEER_DEG, min(MAX_STEER_DEG, err))
    target_v = CRUISE_MPS
    # Edge-case injection so the checker has something to bound (the tuning signal).
    if tick % 200 == 0:
        target_v = 30.0          # over-speed burst
    elif tick % 130 == 0:
        steer_deg = 30.0         # sharp lateral snap
    return target_v, steer_deg


def enforced_to_control(target_v, steer_deg, current_v):
    """Map an enforced (target speed, steering angle) back to a CARLA control."""
    steer = max(-1.0, min(1.0, steer_deg / MAX_STEER_DEG))
    err = target_v - current_v
    if err >= 0.0:
        return carla.VehicleControl(throttle=min(1.0, 0.25 * err + 0.15), steer=steer, brake=0.0)
    return carla.VehicleControl(throttle=0.0, steer=steer, brake=min(1.0, -0.25 * err))


# --------------------------------------------------------------------------- #
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", type=int, default=2000)
    ap.add_argument("--town", default="Town03")
    ap.add_argument("--egos", type=int, default=3)
    ap.add_argument("--ticks", type=int, default=4000)
    ap.add_argument("--follow", type=int, default=0, help="ego index the spectator follows")
    ap.add_argument("--out", default="carla_drive_session.jsonl")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--enforce", dest="enforce", action="store_true", default=True)
    mode.add_argument("--shadow", dest="enforce", action="store_false")
    args = ap.parse_args()

    gov = Governor(os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090"),
                   os.environ.get("KIRRA_ADMIN_TOKEN", "test-token"))

    client = carla.Client(args.host, args.port)
    client.set_timeout(20.0)
    world = client.load_world(args.town)
    wmap = world.get_map()
    original = world.get_settings()
    spectator = world.get_spectator()
    egos = []
    dt = 0.05  # 20 Hz
    interventions = total = 0
    sum_dv = sum_ds = max_dv = 0.0

    try:
        settings = world.get_settings()
        settings.synchronous_mode = True
        settings.fixed_delta_seconds = dt
        world.apply_settings(settings)

        bp = world.get_blueprint_library().filter("vehicle.tesla.model3")[0]
        spawns = wmap.get_spawn_points()
        for i in range(min(args.egos, len(spawns))):
            v = world.try_spawn_actor(bp, spawns[i])
            if v:
                if not args.enforce:
                    v.set_autopilot(True)  # shadow: let the TM drive; we only observe
                egos.append(v)
        if not egos:
            raise RuntimeError("no egos could be spawned (all points blocked?)")
        print(f"[carla] {args.town}: spawned {len(egos)} egos, "
              f"{'ENFORCE (governor controls)' if args.enforce else 'SHADOW (autopilot drives, governor observed)'}")

        sink = open(args.out, "w")
        for tick in range(args.ticks):
            world.tick()
            snap = world.get_snapshot()
            t = snap.timestamp.elapsed_seconds

            # Follow one ego with the spectator (a chase camera).
            if 0 <= args.follow < len(egos):
                ftf = egos[args.follow].get_transform()
                back = ftf.get_forward_vector()
                spectator.set_transform(carla.Transform(
                    carla.Location(x=ftf.location.x - 8 * back.x, y=ftf.location.y - 8 * back.y, z=ftf.location.z + 5),
                    carla.Rotation(pitch=-15, yaw=ftf.rotation.yaw)))

            for idx, ego in enumerate(egos):
                cur_v = speed_of(ego)
                cur_steer = ego.get_control().steer * MAX_STEER_DEG
                tv, sd = propose(wmap, ego, tick)
                proposal = {
                    "linear_velocity_mps": tv, "current_velocity_mps": cur_v,
                    "delta_time_s": dt, "steering_angle_deg": sd,
                    "current_steering_angle_deg": cur_steer,
                }
                try:
                    resp = gov.enforce(proposal)
                except Exception as e:
                    print(f"tick {tick} ego {idx}: governor error {e}", file=sys.stderr)
                    continue
                enf_v = resp.get("enforced_linear_velocity_mps", tv)
                enf_s = resp.get("enforced_steering_angle_deg", sd)
                action = resp.get("action", "Allow")

                if args.enforce:
                    ego.apply_control(enforced_to_control(enf_v, enf_s, cur_v))

                dv, ds = abs(tv - enf_v), abs(sd - enf_s)
                total += 1
                interventions += int(action != "Allow")
                sum_dv += dv; sum_ds += ds; max_dv = max(max_dv, dv)
                sink.write(json.dumps({
                    "t": round(t, 3), "ego": idx,
                    "x": round(ego.get_transform().location.x, 2),
                    "y": round(ego.get_transform().location.y, 2),
                    "speed": round(cur_v, 2), "action": action,
                    "proposed": {"v": tv, "steer": round(sd, 2)},
                    "enforced": {"v": enf_v, "steer": round(enf_s, 2)},
                    "divergence": {"dv": round(dv, 3), "dsteer": round(ds, 3)},
                }) + "\n")
            if tick % 200 == 0:
                sink.flush()
        sink.close()
    except KeyboardInterrupt:
        print("\n[carla] interrupted by operator")
    finally:
        # Sync-mode teardown: revert settings BEFORE destroying actors (avoids a hang).
        world.apply_settings(original)
        client.apply_batch([carla.command.DestroyActor(e) for e in egos])
        n = max(total, 1)
        print("=== CARLA drive-session scorecard ===")
        print(f"  log:               {args.out}")
        print(f"  egos×ticks:        {total} decisions")
        print(f"  intervention rate: {interventions}/{total}  ({100*interventions/n:.1f}%)")
        print(f"  mean |Δv|:         {sum_dv/n:.3f} m/s   (max {max_dv:.2f})")
        print(f"  mean |Δsteer|:     {sum_ds/n:.3f} deg")
        print("  → lower intervention / smaller Δ = a better-aligned DOER (tune the doer, not the envelope)")


if __name__ == "__main__":
    main()
