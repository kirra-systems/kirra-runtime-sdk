# R2 field diagnostics — recipes from the bench

Reusable "why isn't it doing the thing" recipes for the R2 (Jetson Orin NX),
distilled from real bench sessions. Each is copy-pasteable and tells you what a
PASS vs FAIL looks like. Companion to `R2_VOICE_AUDIO_SETUP.md` (voice) and
`PLATFORM_R2_PENDING.md` (the r2 class gate).

**Robot won't move? Start at section 4** — the doer names the reason it is
holding, which is faster than any probe here.

---

## 1. "It won't creep / it needs too much space" — is the lidar seeing the robot?

> **Read section 4 first.** The doer now names the reason it is holding, in its
> own log. This section is for the case where it holds with *no* warning and no
> obvious fault — i.e. the perception really is pinched and the planner is
> correctly refusing.

**Symptom:** occy accepts a goal (`new goal: (x, y)`) but proposes zero
(`/cmd_vel_raw` stays `linear.x: 0`), so the wheels never turn — even in an open
room. Everything else looks healthy (odom, `/scan` at rate, both sidecars up).

**Root cause we hit (2026-07):** the depth **camera / lidar adapter was sitting
in the lidar's horizontal scan plane**, ~0.35 m in front. The lidar read its own
mount as a wall, Taj's corridor pinched shut, and the planner correctly
`safe_stop`-ed. It was never the governor or the vehicle-class envelope.

### 1a. QoS gotcha (read this first)
`/scan` is published **BEST_EFFORT** (sensor-data QoS). A probe subscriber that
defaults to RELIABLE receives **nothing** and you'll misread "no data" as "clear
ahead". Always subscribe with `qos_profile_sensor_data`.

### 1b. Closest-obstacle-ahead probe
```bash
source /opt/ros/humble/setup.bash
source ~/kirra-runtime-sdk/ros2_ws/install/setup.bash
export ROS_DOMAIN_ID=28
python3 - <<'EOF'
import rclpy, math, time
from sensor_msgs.msg import LaserScan
from rclpy.qos import qos_profile_sensor_data
rclpy.init(); n=rclpy.create_node('probe'); got={}
def cb(m):
    fwd=[]; a=m.angle_min
    for r in m.ranges:
        if abs(math.atan2(math.sin(a),math.cos(a)))<math.radians(15) and m.range_min<r<m.range_max: fwd.append(r)
        a+=m.angle_increment
    got['min']=min(fwd) if fwd else None
n.create_subscription(LaserScan,'/scan',cb,qos_profile_sensor_data)
t=time.time()
while 'min' not in got and time.time()-t<5: rclpy.spin_once(n,timeout_sec=0.5)
print('FORWARD closest (m):', got.get('min'))
rclpy.shutdown()
EOF
```

### 1c. The self-occlusion test (the decisive one)
**Move the robot.** If the closest-forward range **doesn't change**, the return
is *on the robot* — a mount, cable, or the camera in the scan plane. Confirm the
bearing (swap the callback body for this to print the 10 nearest hits):
```python
    pts=[]; a=m.angle_min
    for r in m.ranges:
        deg=math.degrees(math.atan2(math.sin(a),math.cos(a)))
        if -40<deg<40 and m.range_min<r<m.range_max: pts.append((deg,r))
        a+=m.angle_increment
    pts.sort(key=lambda p:p[1]); got['near']=pts[:10]   # then: print(got['near'])
```
A tight cluster at a **fixed bearing and range** (ours: +9°→+12°, 0.347–0.359 m,
a 12 mm spread) = a rigid on-robot surface. Environmental returns scatter and
move when the robot does.

### 1d. Fixes (in order of preference)
1. **Physical (best):** raise the lidar above the camera, or move the
   camera/adapter out of the horizontal scan plane. Re-run 1b — the number
   should jump to the real room distance.
2. **Software stopgap:** mask just the offending angular wedge on `/scan` before
   perception consumes it. Keep it **narrow** (only the degrees the mount
   occupies) — that sector is physically blind anyway, so masking loses no real
   perception, but a *wide* mask blinds the robot. This is a workaround, not a
   substitute for remounting; treat a masked wedge as "occluded", never "known
   clear".

> Rule of thumb: the r2 envelope wants ~0.5 m of standoff, so occy needs
> **≳0.8 m of genuinely clear runway ahead** before it will commit to a creep.
> A cluttered bench with anything inside ~0.6 m will (correctly) hold.

---

## 2. r2 vehicle-class switch — the as-applied procedure + what "healthy" looks like

The three flip sites (gated — see `PLATFORM_R2_PENDING.md` "THE FLIP"):

| # | File | Change |
|---|------|--------|
| 1 | `installer/platform_map.toml` | `[platform.r2] profile_class` `"courier"` → `"r2"` (runtime-inert; installer-verify metadata) |
| 2 | `ros2_ws/src/kirra_safety/config/kirra_params.yaml` | interceptor `wheelbase_m` → `0.229` (**runtime-critical**) |
| 3 | `/etc/kirra/kirra.env` | `KIRRA_VEHICLE_CLASS=courier` → `r2` (**runtime-critical**; verifier reads it) |

Apply → restart **verifier first**, confirm, then the ros-stack:
```bash
sudo systemctl restart kirra-verifier.service
sudo journalctl -u kirra-verifier.service -n 40 --no-pager | grep -i 'vehicle_class'
#   PASS: … vehicle class selected … vehicle_class="r2"
sudo systemctl restart kirra-ros-stack.service     # reloads the interceptor's 0.229
```

**Expected, not-a-bug:** in the seconds between the two restarts the *old*
interceptor (still 0.229 vs a just-restarted verifier, or vice-versa) may log a
`🔴 WHEELBASE MISMATCH … LATCHED to stop`. That's the fast-loop refusing to
convert Twist→steering with a wheelbase the verifier didn't sign — it clears the
instant the fresh interceptor comes up matched. A **persistent** mismatch after
both are up = sites 2 and 3 disagree; make both `0.229`/`r2`.

**Config-with-symlink-install note:** if `ros2_ws/install/.../kirra_params.yaml`
is a symlink chain back to `src/` (check with `ls -l`), your edit is already live
— just restart, no `colcon` rebuild. If it's a real copy, rebuild the one
package: `colcon build --symlink-install --packages-select kirra_safety`.

> These edits are **uncommitted working-tree changes on the robot** and the r2
> flip is gated (4 bench measurements + dynamic-limit benching + review still
> owed). Do **not** `git stash`/`checkout` on the robot — you'll revert to
> courier. `/etc/kirra/kirra.env` is a system file, unaffected by git.

---

## 3. Networking — reach the robot without the Ethernet cable

> Scope: this is the **connectivity/SSH** recipe (how *you* reach the box).
> `R2_UNTETHERED_BRINGUP.md` is the complementary **driving-off-network**
> architecture — and its load-bearing point still holds here: the WiFi/SSH link
> is *not* in the control loop, so none of this touches how the robot drives.

**The reality on this unit:** the WiFi radio (`wlP1p1s0`) runs in **AP mode** —
it *hosts* the `ROSMASTER` hotspot (`iw dev wlP1p1s0 info` → `type AP`, ch 1,
2.4 GHz). A single radio in AP mode **cannot scan for or join another WiFi**, and
`ROSMASTER` has **no internet uplink**. The robot's internet (and therefore
Tailscale) rides the **Ethernet** cable.

Confirm the picture:
```bash
iw dev wlP1p1s0 info | grep -iE 'type|ssid|channel'   # type AP  ssid ROSMASTER
ip route | grep default                               # default via …dev eno1 = internet is on Ethernet
tailscale status                                      # robot 100.x + your phone as a peer
```

Two ways off the cable:

- **A — LAN-direct over `ROSMASTER` (works immediately, no changes):** join your
  **phone** to the `ROSMASTER` WiFi (its PSK: `sudo nmcli connection show
  ROSMASTER | grep -i psk`), then `ssh jetson@192.168.1.11`. No internet, only
  reachable when you're near the robot. **This is the one that needs a WiFi
  password (on the phone).**
- **B — Tailscale from anywhere (needs internet on the robot):** the robot must
  be a WiFi **client** of a router with internet, which means switching the radio
  **AP → client mode first** (tear down the `ROSMASTER` AP), then
  `sudo nmcli device wifi connect "<home-ssid>" password "<psk>"`. Then
  `ssh` to the robot's Tailscale IP (stable, e.g. `100.x.y.z`) from anywhere —
  **no WiFi password on the phone side.** Bigger reconfigure; do it wired so a
  failed join is recoverable.

**SSH never uses the WiFi password** — that authenticates with the `jetson`
login. The WiFi PSK only gets a *device onto a network*.

---

## 4. "It isn't moving" — read the doer's own warnings first

The doer now **tells you why it is holding**. Before probing anything, read its
log:

```bash
sudo journalctl -u kirra-ros-stack.service -n 200 --no-pager | grep -iE 'doer|frame health|object goal|plan job'
```

Every hold is fail-closed and therefore **safe** — the doer publishes an empty
envelope, the interceptor refuses it, the robot stops. Safe is not the same as
visible, which is what these warnings are for: a wedged sidecar and a robot
standing at a delivered goal produce *identical* motion.

### 4a. The sustained-hold warning

```
doer holding on <STATE> (<tag>) for <N> consecutive ticks —
no motion is being proposed and the robot will remain stopped
```

Fires **once** per episode, after ~10 consecutive holds (~1 s at 10 Hz), and
again if the *cause* changes. Recovery logs `doer recovered from <STATE> after
<N> consecutive holds — proposing motion again`.

| `<STATE>` | What it means | What to do |
|---|---|---|
| `NO_REQUESTS` | `python3-requests` is not installed | `sudo apt install python3-requests`, restart the stack |
| `AWAITING_POSE` | no `/odom` has ever arrived | `ros2 topic hz /odom`; check the base driver and `ROS_DOMAIN_ID` |
| `STALE_SCAN` | newest `/scan` is older than `scan_stale_s` | `ros2 topic hz /scan` — **with sensor-data QoS** (see 1a). Dead lidar, wrong QoS, or a driver that died |
| `ABSENT` | no plan job has completed — a sidecar is not answering | `curl -s localhost:8101/health; curl -s localhost:8100/health`; see 4b if it never clears |
| `FAULT` | the job failed, or Taj and the planner disagree | read the tag: `service-error:<Exc>` = the sidecar errored/timed out; `evidence mismatch: taj=… plan=…` = the planner planned against a different Taj frame; `*-response-not-an-object` = a sidecar returned malformed JSON |
| `SUPERSEDED` | replies keep arriving about scans already acted on | usually transient. Persistent = round trips are slower than the tick; see 4c |
| `STALE` | plans complete, but the perception behind them has aged out | see 4c — the pipeline does not fit the staleness budget |
| `UNBOUND` | a usable plan carrying no bindable evidence identity | Taj returned its invalid-frame sentinel, or the planner answered with no `proposal_digest`. Check Taj is getting real rays (section 1) |
| `UNENCODABLE` | the envelope could not be encoded | non-finite velocity out of the planner; capture the plan response and file it |

### 4b. `plan job overdue`

```
plan job overdue by <X> ms past its <Y> ms budget — a sidecar is not closing
the connection (an HTTP timeout bounds each socket read, not the whole
request). No further plan jobs will start until it returns; the doer stays held.
```

This is the nastiest sidecar failure and the one worth recognising on sight. An
HTTP timeout bounds each **socket read**, not the whole request, so a sidecar
that dribbles bytes holds the doer's one worker open **forever** without ever
tripping its own timeout. Planning stops permanently.

The doer deliberately does **not** start a replacement job — piling work onto a
service that is already failing is not a fix, and the one-job bound is what
keeps thread creation bounded. It stays held and says so. **You** have to
restart the offending sidecar:

```bash
curl -m 2 -s -o /dev/null -w '%{http_code} %{time_total}s\n' localhost:8101/health   # Taj
curl -m 2 -s -o /dev/null -w '%{http_code} %{time_total}s\n' localhost:8100/health   # planner
sudo systemctl restart kirra-taj.service      # or kirra-planner.service
```

A hang shows as `curl` timing out at 2 s while the process is still alive — the
port answers but never finishes. That is the signature.

### 4c. `plan pipeline budget … exceeds scan_stale_s` (startup)

```
plan pipeline budget <X> ms (1/plan_hz=<A> ms + 2x http_timeout_ms)
exceeds scan_stale_s (<B> ms): plans will age out and the doer will hold
intermittently. …
```

The Taj + planner calls run in a background job, so a tick publishes the
**previous** tick's result. Perception is therefore one tick period plus one
round trip old when it is used. If that does not fit inside `scan_stale_s`, the
doer holds intermittently on a *perfectly healthy* lidar — which reads as a
flaky sensor and is not one.

Fix by raising `plan_hz` (shipped default `10.0`, matching the bench-verified
TG30 rate) or lowering `http_timeout_ms` in `kirra_params.yaml`. **Do not widen
`scan_stale_s` to silence it** — that is the safety bound on how blind the robot
may be while still moving, and it is set per deployment from the lidar rate.
Raising `plan_hz` above the lidar rate costs nothing: a job is only issued when a
new scan has actually arrived.

### 4d. Holds that are silent by design

Not every hold warns, and that is deliberate — a warning that fires during
normal operation stops meaning anything.

- **Never announced** (the robot is idle, not broken): waiting for a goal, goal
  reached, object goal reached. **No doer warning + no motion + a goal that was
  already reached = working as intended.** Send a new goal.
- **Announced in their own words** (they name a more specific cause than the
  generic warning could, so the generic one stays quiet): `frame health <REASON>:
  <detail> — holding …` and `object goal '<phrase>': <refusal>`. Grep for
  `frame health` and `object goal` as well as `doer holding`, which is why the
  command at the top of this section covers all of them.

> Rule of thumb: **one** hold cause fires per tick, so the log reads as a single
> story. "Stale scan for 40 ticks, then `ABSENT`" means the lidar came back and
> a sidecar then went down — two faults, in order, not one confusing one.
