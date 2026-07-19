# Autoware-on-Humble isolation (ADR-0036)

Scaffold for the "**only Autoware on 22.04/Humble**" topology: the Autoware
*doer* stays on Humble in its own container; the KIRRA checker + adapter and the
rest of the stack move to 24.04/Jazzy. They meet **only** on the 5 curated
boundary topics. See `docs/adr/0036-autoware-distro-migration-occy-gap.md`.

```
 ┌─────────────────────────────┐        5 curated topics over DDS        ┌────────────────────────────┐
 │ Autoware  (doer)            │  trajectory / objects(+2nd) / map /     │ KIRRA checker + adapter    │
 │ ros:humble  (the ONLY 22.04)│  odometry  ───────────────────────────► │ ros:jazzy                  │
 │                             │  ◄─────────────  governed control_cmd   │ (Occy/Taj/robot on Jazzy)  │
 └─────────────────────────────┘                                         └────────────────────────────┘
```

The checker does **not** depend on Autoware — only on the 4 curated
`autoware_*_msgs` packages vendored in `ros2_ws/src/` (they build from `.msg` on
any distro). So the only thing pinned to Humble is Autoware itself.

## Step 1 — prove the boundary is wire-safe across distros (do this FIRST)

Direct cross-distro DDS is only safe if the 5 curated interfaces are
byte-identical on both distros (⇒ identical RIHS type hash). Verify:

```bash
bash scripts/curated_interface/crossdistro_hash_check.sh \
     /opt/ros/humble/share  /opt/ros/jazzy/share
```
- **PASS** → every curated interface is identical Humble==Jazzy → **direct DDS**, no bridge.
- **DRIFT** → the named interface(s) differ across distros → route those through
  `kirra_bridge_cpp` / `domain_bridge`; record the drift in the MSGSYNC SRAC
  (`docs/safety/MSG_INTERFACE_VERSION_SYNC.md`).

## Step 2 — bring up the two containers

```bash
# real (placeholder) Autoware on Humble + KIRRA on Jazzy:
docker compose -f deploy/autoware-isolation/docker-compose.yml up
```
Replace the `autoware` service image/command with your real Autoware-on-Humble
build. Both sides share `ROS_DOMAIN_ID` + the same `RMW_IMPLEMENTATION`
(Fast DDS is the most interop-tested Humble↔Jazzy pairing) over host networking.

## Step 3 — validate the KIRRA side WITHOUT a real Autoware (if it isn't built out)

If Autoware isn't fully implemented yet, use the **stub doer** — it publishes
minimal, valid messages on the 5 boundary topics so you can bring up and validate
the whole Jazzy checker path (topic discovery, type match, message flow, and the
governed-control round-trip) independent of Autoware:

```bash
docker compose -f deploy/autoware-isolation/docker-compose.yml \
  --profile stub up autoware-stub kirra
# or standalone, in any sourced ROS 2 container with the curated msgs built:
python3 deploy/autoware-isolation/autoware_stub_publisher.py
```
The stub logs `✓ received a governed control_cmd back from the checker` once the
adapter's bounded output returns — that confirms the seam round-trips end to end.

## Bench commands (copy-paste)

Grounded in the real node identity: the adapter is `kirra_governor` in namespace
`kirra`, so its `~/input/*` subscriptions resolve to `/kirra/kirra_governor/input/*`.

**0 — code on the bench**
```bash
cd ~/kirra-runtime-sdk
git checkout main && git pull          # ADR-0036 + this scaffold are on main
```

**1 — the wire-compat gate (the one new safety check)**
```bash
bash scripts/curated_interface/crossdistro_hash_check.sh \
     /opt/ros/humble/share  /opt/ros/jazzy/share
# PASS + no DRIFT → direct cross-distro DDS is wire-safe.
# DRIFT <pkg/Msg>  → bridge THAT interface (kirra_bridge_cpp / domain_bridge).
```
Distros on different boxes? Run the per-distro gate on each and compare:
```bash
bash scripts/curated_interface/verify_hashes.sh /opt/ros/humble/share   # Humble box
bash scripts/curated_interface/verify_hashes.sh /opt/ros/jazzy/share    # Jazzy box
```
Empty curated pkg? populate first: `bash scripts/curated_interface/extract_closures.sh /opt/ros/jazzy/share`.

**2 — bring up the split (docker)**
```bash
export ROS_DOMAIN_ID=0
export RMW_IMPLEMENTATION=rmw_fastrtps_cpp
export AUTOWARE_IMAGE=<your-autoware-humble-image>
export KIRRA_IMAGE=<your-kirra-jazzy-image>

docker compose -f deploy/autoware-isolation/docker-compose.yml up                       # real Autoware ⟷ KIRRA
docker compose -f deploy/autoware-isolation/docker-compose.yml --profile stub up autoware-stub kirra   # stub doer ⟷ KIRRA
```

**3 — standalone on a Jazzy host (no docker), to see the seam live**

Terminal A — curated msgs + stub doer (publish on the adapter's private ns):
```bash
source /opt/ros/jazzy/setup.bash
export ROS_DOMAIN_ID=0
cd ~/kirra-runtime-sdk/ros2_ws
colcon build --packages-up-to autoware_planning_msgs autoware_perception_msgs \
                              autoware_map_msgs autoware_control_msgs
source install/setup.bash
KIRRA_BOUNDARY_PREFIX=/kirra/kirra_governor/input \
  python3 ~/kirra-runtime-sdk/deploy/autoware-isolation/autoware_stub_publisher.py
```
Terminal B — build + run the checker (adapter) node:
```bash
source /opt/ros/jazzy/setup.bash
source ~/kirra-runtime-sdk/ros2_ws/install/setup.bash   # the autoware_*_msgs types r2r binds
export ROS_DOMAIN_ID=0
cd ~/kirra-runtime-sdk
cargo build -p kirra-ros2-adapter --features ros2 --release
./target/release/kirra_ros2_adapter_node --corridor-source mock
# mock = explicit 5 m straight-line TEST corridor (WARN-bannered).
# real map: --corridor-source lanelet2 --map-bin <map.osm.bin>  (needs ros-jazzy-lanelet2)
```
Terminal C — confirm the wire + the cross-distro type match:
```bash
source /opt/ros/jazzy/setup.bash; export ROS_DOMAIN_ID=0
ros2 topic list | grep kirra_governor/input
ros2 topic hz   /kirra/kirra_governor/input/trajectory      # stub is publishing
ros2 topic info -v /kirra/kirra_governor/input/objects      # type + (Jazzy) RIHS hash — compare to Humble's
```
Governed round-trip: `ros2 topic list` for the adapter's output topic, then point the
stub at it (`KIRRA_CONTROL_TOPIC=<that topic>`) — it logs `✓ received a governed
control_cmd back from the checker` on the first bounded command.

## What this scaffold is / isn't
- **Is:** the topology, the cross-distro wire-compat gate, and a doer stub so the
  Jazzy side is testable now.
- **Isn't:** a real Autoware build (image/command are placeholders), and it does
  **not** touch the safety spine — the checker is `no_std`/ROS-agnostic and
  bounds whatever crosses the boundary regardless of distro.

## Retirement
When Autoware ships stable Jazzy support, migrate the `autoware` service to
Jazzy, re-run step 1 (now Jazzy↔Jazzy, trivially PASS), and delete the Humble
container — the isolation was only ever a bridge across the EOL gap.
