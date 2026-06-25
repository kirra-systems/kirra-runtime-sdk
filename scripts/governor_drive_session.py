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

Besides the human-readable divergence log (`out.jsonl`), it ALSO emits the two
files `kirra-collector` joins into a supervised dataset:
  - `<out>.capture.jsonl` — one `kirra_capture_schema::CaptureRecord` per tick
    (the governor's correction: ALLOW / CLAMP_LINEAR / CLAMP_STEERING / DENY).
  - `<out>.bag.json`       — the matching bus recording (a JSON array of
    `BusMessage`, the doer-side proposal stamp the record joins against).
Feed them straight into the collector:
  cargo run -p kirra-collector -- \
    --capture <out>.capture.jsonl --bag-json <out>.bag.json \
    --out dataset/ --window-ms 100
The capture crate (`kirra-capture-schema`) is the ONLY shared dependency — the
collector never links the verifier, so it cannot reach the verdict path.

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


def capture_record(tick, t_wall_ms, proposed, action, enf_v, enf_steer, resp):
    """Map one governor decision onto the `kirra_capture_schema::CaptureRecord`
    wire shape (SCREAMING_SNAKE outcome; `safe_value` carries the clamped value)."""
    if action == "ClampLinear":
        outcome, safe_value, deny_code = "CLAMP_LINEAR", enf_v, None
    elif action == "ClampSteering":
        outcome, safe_value, deny_code = "CLAMP_STEERING", enf_steer, None
    elif action == "DenyBreach":
        outcome, safe_value, deny_code = "DENY", None, resp.get("deny_code") or resp.get("reason")
    else:  # "Allow"
        outcome, safe_value, deny_code = "ALLOW", None, None
    return {
        "decision_seq": tick,
        "t_mono_ns": t_wall_ms * 1_000_000,
        "t_wall_ms": t_wall_ms,
        "source": "COMMAND_GATEWAY",
        "proposed": {
            "linear_velocity_mps": proposed["linear_velocity_mps"],
            "current_velocity_mps": proposed["current_velocity_mps"],
            "steering_angle_deg": proposed["steering_angle_deg"],
            "current_steering_angle_deg": proposed["current_steering_angle_deg"],
            "delta_time_s": proposed["delta_time_s"],
        },
        "outcome": outcome,
        "deny_code": deny_code,
        "safe_value": safe_value,
        "mrc": bool(resp.get("mrc", False)),
        "posture": "NOMINAL",
        "derate_enabled": False,
    }


def main():
    url = os.environ.get("KIRRA_VERIFIER_URL", "http://localhost:8090")
    token = os.environ.get("KIRRA_ADMIN_TOKEN", "test-token")
    ticks = int(sys.argv[1]) if len(sys.argv) > 1 else 120
    out_path = sys.argv[2] if len(sys.argv) > 2 else "drive_session.jsonl"
    base = out_path[:-6] if out_path.endswith(".jsonl") else out_path
    capture_path, bag_path = base + ".capture.jsonl", base + ".bag.json"
    dt = 0.05  # 20 Hz
    doer_version = "occy-drive-demo"

    gov, ego, doer = Governor(url, token), KinematicEgo(), Doer()
    interventions = 0
    sum_dv = sum_dsteer = max_dv = 0.0
    bus = []  # the matching bus recording (one BusMessage per emitted record)

    with open(out_path, "w") as sink, open(capture_path, "w") as cap:
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

            # The supervised-dataset pair: the CaptureRecord (what KIRRA corrected)
            # + the BusMessage it joins against (the doer-side proposal, same stamp).
            t_wall_ms = tick * int(dt * 1000)
            cap.write(json.dumps(capture_record(tick, t_wall_ms, proposed, action, enf_v, enf_steer, resp)) + "\n")
            bus.append({
                "t_wall_ms": t_wall_ms,
                "doer_version": doer_version,
                "asset_id": "ego",
                "trajectory_id": tick,
                "objects_ms": t_wall_ms,
                "bulk_ref": f"mem://drive#{tick}",
            })

    with open(bag_path, "w") as bag:
        json.dump(bus, bag)

    n = max(ticks, 1)
    print(f"=== Governor drive-session scorecard ({ticks} ticks @ {1/dt:.0f} Hz) ===")
    print(f"  log:                 {out_path}")
    print(f"  capture (collector): {capture_path}")
    print(f"  bus    (collector):  {bag_path}")
    print(f"  intervention rate:   {interventions}/{ticks}  ({100*interventions/n:.1f}%)")
    print(f"  mean |Δv| (clamp):   {sum_dv/n:.3f} m/s   (max {max_dv:.2f})")
    print(f"  mean |Δsteer|:       {sum_dsteer/n:.3f} deg")
    print(f"  ego advanced to x =  {ego.x:.1f} m   (final speed {ego.speed:.1f} m/s)")
    print("  → lower intervention rate / smaller Δ = a better-aligned DOER (tune the doer, not the envelope)")
    print(f"  → feed the dataset: cargo run -p kirra-collector -- "
          f"--capture {capture_path} --bag-json {bag_path} --out dataset/ --window-ms 100")


if __name__ == "__main__":
    main()
