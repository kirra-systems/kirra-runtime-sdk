# `autoware_perception_msgs/msg` — populated by the extract script, never hand-edited

This directory is intentionally **empty in the repo scaffold (Phase 1)**. It is
populated in **Phase 2 (laptop step)** by:

```sh
bash scripts/curated_interface/extract_closures.sh   # reference = /opt/ros/jazzy/share
```

which copies the **verbatim, byte-identical** `PredictedObjects` message closure
from a reference Autoware install. Expected closure (confirm against the actual
reference — do not trust this list):

- `PredictedObjects.msg`
- `PredictedObject.msg`
- `PredictedObjectKinematics.msg`
- `PredictedPath.msg`
- `ObjectClassification.msg`
- `Shape.msg`

**Never hand-edit a `.msg` here.** Any edit silently changes the RIHS type hash
and breaks wire compatibility with the deployed Autoware. The only sanctioned
way to change these files is to re-run the extract against a new reference and
re-pass `scripts/curated_interface/verify_hashes.sh` (the byte-identical gate),
then bump the pinned reference in `docs/safety/MSG_INTERFACE_VERSION_SYNC.md`
(KIRRA-OCCY-MSGSYNC-001).
