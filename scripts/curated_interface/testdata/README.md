# closure_diff self-test fixtures (M3, #1042)

Two SYNTHETIC reference `share/` trees (`humble/`, `jazzy/`) — NOT real ROS
installs. They exist only to drive `closure_diff_selftest.sh`, which proves
`closure_diff.py` catches a nested base-message drift that the leaf-only
step-3 of `crossdistro_hash_check.sh` misses.

The two trees are constructed so that:

- `autoware_demo_msgs/msg/Leaf.msg` — the curated LEAF — is **byte-identical**
  across `humble/` and `jazzy/`.
- `std_msgs/msg/Header.msg` and `builtin_interfaces/msg/Time.msg` (nested,
  reached transitively) are identical too.
- `geometry_msgs/msg/Point.msg` (nested, reached via the leaf) **differs**:
  `jazzy/` drops the `z` field.

So a leaf-only comparison PASSES while the true RIHS closure has drifted — the
exact silent-drift hazard #1042 describes. The self-test asserts the closure
comparator FAILS on this pair (and PASSES a tree against itself).

Never point `closure_diff.py`'s real invocation (in `crossdistro_hash_check.sh`)
at this directory — the production gate runs against real
`/opt/ros/{humble,jazzy}/share` trees.
