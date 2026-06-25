# Drive-session simulator — observe egos + collect doer↔checker tuning data

Two host-side harnesses drive an ego (or a fleet) through the **real Kirra governor**
and log every `(proposed, enforced, divergence)` decision as JSONL, so the planner's
performance can be **measured** against the checker and the **doer** tuned.

| Harness | Needs | What it gives |
|---|---|---|
| `scripts/governor_drive_session.py` | just the verifier | a headless kinematic ego — proves the loop, no GPU. **Runnable anywhere.** |
| `scripts/carla_drive_session.py` | CARLA server (GPU) + verifier | a fleet driving a real **CARLA city**, spectator camera, the same capture. |

The doer–checker contract is preserved end to end: the doer **proposes**, KIRRA
**bounds both axes**, the enforced command is applied, and the divergence is the
tuning signal. **Tune the doer from this data — never the checker's envelope** (the
speed cap, the 0.40 m margin, the RSS bounds are safety-derived invariants).

## 1. Start the governor

```bash
KIRRA_ADMIN_TOKEN=test-token \
KIRRA_SUPERVISOR_RESET_KEY=test-reset-key \
KIRRA_DB_PATH=/tmp/kirra.sqlite \
cargo run --bin kirra_verifier_service        # listens on :8090
```

> Run **one** instance per DB/port. Multiple instances sharing a port (`SO_REUSEPORT`)
> load-balance requests, and the HA one-writer fencing can self-demote an instance to
> PassiveStandby (admin/mutation routes then 503). One clean instance = no fencing.

## 2a. Headless (no GPU) — prove the loop / collect data anywhere

```bash
KIRRA_VERIFIER_URL=http://localhost:8090 KIRRA_ADMIN_TOKEN=test-token \
  python3 scripts/governor_drive_session.py 120 drive_session.jsonl
```

Sample scorecard (this is what it printed against a local governor):

```
intervention rate:   28/120  (23.3%)
mean |Δv| (clamp):   2.224 m/s   (max 29.88)   # over-speed bursts ClampLinear'd
mean |Δsteer|:       2.146 deg                 # sharp-steer bursts ClampSteering'd
```

## 2b. CARLA city (GPU) — watch a fleet drive + collect

```bash
# CARLA server already running (e.g. ./CarlaUE4.sh)
KIRRA_VERIFIER_URL=http://localhost:8090 KIRRA_ADMIN_TOKEN=test-token \
  python3 scripts/carla_drive_session.py --town Town03 --egos 3 --ticks 4000 --follow 0
```

- The **spectator camera** chases ego `--follow`, so you watch the city drive live.
- `--enforce` (default): the governor-enforced control is applied — the real
  doer-checker loop.
- `--shadow`: the CARLA Traffic Manager autopilot drives realistically and the
  governor is only **observed** (non-intrusive) — pure data collection.
- Requires the `carla` Python package importable (the CARLA egg / `pip install carla`).

## 3. Tune from the capture

Each JSONL row carries the proposal, the enforced result, and the per-axis Δ. Use it to:
- track the **intervention rate** and **mean clamp magnitude** over a run (the scorecard);
- find **where** the doer over-reaches (cluster by map x/y, maneuver, speed);
- make the doer propose checker-admissible commands more often (geometric tuning, or
  as training data for the learned doer).

For the heavier supervised-learning path, map each row to a `CaptureRecord`
(`kirra-capture-schema`) and run it through `kirra-collector` to build the Parquet dataset.
