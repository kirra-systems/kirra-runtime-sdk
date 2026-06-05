# `autoware_planning_msgs/msg` — populated by the extract script, never hand-edited

This directory is intentionally **empty in the repo scaffold (Phase 1)**. It is
populated in **Phase 2 (laptop step)** by:

```sh
bash scripts/curated_interface/extract_closures.sh   # reference = /opt/ros/jazzy/share
```

which copies the **verbatim, byte-identical** `Trajectory` message closure from
a reference Autoware install. Expected closure (confirm against the actual
reference — do not trust this list):

- `Trajectory.msg`
- `TrajectoryPoint.msg`

The full `autoware_planning_msgs` additionally carries route messages
(`LaneletPrimitive`, the `ClearRoute` service) that r2r 0.9.5 cannot codegen on
Jazzy; the curated subset deliberately contains **only** the Trajectory closure,
which is what the adapter binds.

**Never hand-edit a `.msg` here.** Any edit silently changes the RIHS type hash
and breaks wire compatibility with the deployed Autoware. Change these only by
re-running the extract against a new reference and re-passing
`scripts/curated_interface/verify_hashes.sh`, then bump the pinned reference in
`docs/safety/MSG_INTERFACE_VERSION_SYNC.md` (KIRRA-OCCY-MSGSYNC-001).
