#!/usr/bin/env python3
"""
Drive an ego through the REAL Kirra governor and collect doer↔checker divergence
data for tuning — the corrected, working shape of a CARLA tuning loop.

Unlike a mock checker, every proposed command is POSTed to the live verifier's
`/actuator/motion/command`, the *enforced* result is applied to the (kinematic)
ego, and the (proposed, enforced, divergence) tuple is logged as JSONL. The
governor bounds BOTH axes; this harness only proposes and records.

  KIRRA_VERIFIER_URL=http://localhost:8090 KIRRA_ADMIN_TOKEN=test-token \
    python3 scripts/governor_drive_session.py [ticks] [out.jsonl]

To graduate to CARLA: replace `KinematicEgo` reads/writes with CARLA actor
state + `apply_control`, and replace `Doer.propose` with your planner / ROS 2
bridge. The governor call and the capture stay identical.

SAFETY NOTE: tune the DOER from this data (make it propose checker-admissible
commands more often), NEVER the checker's envelope — the speed cap, the 0.40 m
margin, and the RSS bounds are safety-derived invariants, not knobs.
"""
import json
import math
import os
import sys
import time
import urllib.request


class Governor:
    """Thin client over the real verifier's actuator-enforcement endpoint."""
    def __init__(self, url: str, token: str):
        self.endpoint = url.rstrip("/") + "/actuator/motion/command"
        self.token = token

    def enforce(self, proposed: dict) -> dict:
        body = json.dumps(proposed).encode()
        req = urllib.request.Request(self.endpoint, data=body, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", f"Bearer {self.token}")
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.loads(resp.read())


class KinematicEgo:
    """A minimal bicycle-ish ego the enforced command is integrated onto."""
    def __init__(self):
        self.x = 0.0
        self.heading = 0.0
        self.speed = 0.0
        self.wheelbase = 2.7

    def apply(self, v_mps: float, steer_deg: float, dt: float):
        self.speed = v_mps
        self.heading += (v_mps / self.wheelbase) * math.tan(math.radians(steer_deg)) * dt
        self.x += v_mps * math.cos(self.heading) * dt


class Doer:
    """The proposer. Cruises, and periodically over-reaches (an aggressive merge /
    a hallucinated over-speed) so the governor has something to bound — the
    interventions are the tuning signal."""
    def propose(self, tick: int, ego: KinematicEgo, dt: float) -> dict:
        v, steer = 12.0, 0.0
        if tick % 17 == 0:        # over-speed burst
            v = 30.0
        elif tick % 11 == 0:      # sharp lateral snap
            steer = 28.0
        return {
            "linear_velocity_mps": v,
            "current_velocity_mps": ego.speed,
            "delta_time_s": dt,
            "steering_angle_deg": steer,
            "current_steering_angle_deg": 0.0,
        }


def main():
    url = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090")
    token = os.environ.get("KIRRA_ADMIN_TOKEN", "test-token")
    ticks = int(sys.argv[1]) if len(sys.argv) > 1 else 120
    out_path = sys.argv[2] if len(sys.argv) > 2 else "drive_session.jsonl"
    dt = 0.05  # 20 Hz

    gov, ego, doer = Governor(url, token), KinematicEgo(), Doer()
    interventions = 0
    sum_dv = sum_dsteer = max_dv = 0.0

    with open(out_path, "w") as sink:
        for tick in range(ticks):
            proposed = doer.propose(tick, ego, dt)
            try:
                resp = gov.enforce(proposed)
            except Exception as e:
                print(f"tick {tick}: governor error {e}", file=sys.stderr)
                continue

            enf_v = resp.get("enforced_linear_velocity_mps", proposed["linear_velocity_mps"])
            enf_steer = resp.get("enforced_steering_angle_deg", proposed["steering_angle_deg"])
            action = resp.get("action", "Allow")
            ego.apply(enf_v, enf_steer, dt)

            dv = abs(proposed["linear_velocity_mps"] - enf_v)
            dsteer = abs(proposed["steering_angle_deg"] - enf_steer)
            intervened = action != "Allow"
            interventions += int(intervened)
            sum_dv += dv
            sum_dsteer += dsteer
            max_dv = max(max_dv, dv)

            sink.write(json.dumps({
                "t": round(tick * dt, 3),
                "ego_x": round(ego.x, 3),
                "ego_speed": round(ego.speed, 3),
                "action": action,
                "proposed": {"v": proposed["linear_velocity_mps"], "steer": proposed["steering_angle_deg"]},
                "enforced": {"v": enf_v, "steer": enf_steer},
                "divergence": {"dv": round(dv, 3), "dsteer": round(dsteer, 3)},
            }) + "\n")

    n = max(ticks, 1)
    print(f"=== Governor drive-session scorecard ({ticks} ticks @ {1/dt:.0f} Hz) ===")
    print(f"  log:                 {out_path}")
    print(f"  intervention rate:   {interventions}/{ticks}  ({100*interventions/n:.1f}%)")
    print(f"  mean |Δv| (clamp):   {sum_dv/n:.3f} m/s   (max {max_dv:.2f})")
    print(f"  mean |Δsteer|:       {sum_dsteer/n:.3f} deg")
    print(f"  ego advanced to x =  {ego.x:.1f} m   (final speed {ego.speed:.1f} m/s)")
    print("  → lower intervention rate / smaller Δ = a better-aligned DOER (tune the doer, not the envelope)")


if __name__ == "__main__":
    main()
