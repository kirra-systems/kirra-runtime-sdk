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

## 2c. Drive with REAL Occy as the doer (not the placeholder controller)

The harnesses above use a built-in controller as the doer. To drive egos with the
**actual planner**, run the Occy planner endpoint and point the harness at it:

```bash
cargo run -p kirra-sidecars --bin planner_service          # Occy on :8100 (POST /plan)

KIRRA_VERIFIER_URL=http://localhost:8090 \
  python3 scripts/carla_drive_session.py --town Town03 --egos 3 --occy http://localhost:8100
```

Per tick, the harness builds a corridor from the map's lane waypoints ahead of each ego,
POSTs the world snapshot to `/plan`, and drives from Occy's KIRRA-validated trajectory.
`crates/kirra-mick/examples/drive_session.rs` is the same Occy↔KIRRA loop fully in Rust
(no CARLA), with the `MickEvalSummary` scorecard — use it to sanity-check the doer offline.

## 3. Tune from the capture

Each JSONL row carries the proposal, the enforced result, and the per-axis Δ. Use it to:
- track the **intervention rate** and **mean clamp magnitude** over a run (the scorecard);
- find **where** the doer over-reaches (cluster by map x/y, maneuver, speed);
- make the doer propose checker-admissible commands more often (geometric tuning, or
  as training data for the learned doer).

## 4. Build the supervised-learning dataset (wired)

`governor_drive_session.py` already emits the two files `kirra-collector` joins —
alongside `drive_session.jsonl` it writes `drive_session.capture.jsonl` (one
`kirra-capture-schema::CaptureRecord` per tick: `ALLOW` / `CLAMP_LINEAR` /
`CLAMP_STEERING` / `DENY`, with the clamped value in `safe_value`) and
`drive_session.bag.json` (the matching `BusMessage` array — the doer-side stamp
each record joins against). Feed them straight into the collector:

```bash
cargo run -p kirra-collector -- \
  --capture drive_session.capture.jsonl \
  --bag-json drive_session.bag.json \
  --out dataset/ --window-ms 100
```

A 120-tick headless run produces a partitioned Parquet dataset + manifest:

```
reconciliation:
  records_in            = 120 (gateway 120, trajectory 0; 0 duplicate(s) dropped)
  interventions / passes= 28 / 92
  joined / orphans      = 120 / 0 (orphan_rate 0.000)
dataset/
  manifest.json
  doer_version=occy-drive-demo/source=COMMAND_GATEWAY/part-000.parquet
```

Each Parquet row carries the governor's correction joined to the doer's proposal
+ version — the supervised target for tuning the **doer** to propose
checker-admissible commands. The collector depends on `kirra-capture-schema`
**only** (never the verifier), so it is mechanically incapable of reaching the
verdict path.

### Sidewalk courier — the slow-loop trajectory dataset (ADR-0028)

The above captures the **fast-loop command gateway**. The courier's decisions live in the
**slow loop** (Mick intent → Occy trajectory → KIRRA verdict), so they capture as
`SlowLoopTrajectory` records. `cargo run -p kirra-mick --example sidewalk_capture` runs a
courier drive (yield to a pedestrian; cross when clear) and writes `sidewalk.capture.jsonl`
+ `sidewalk.bag.json`; feed them to the same collector:

```
cargo run -p kirra-collector -- --capture sidewalk.capture.jsonl \
  --bag-json sidewalk.bag.json --out dataset/ --window-ms 100
```

A 120-tick courier run produces (verified):

```
reconciliation:
  records_in            = 120 (gateway 0, trajectory 120; 0 duplicate(s) dropped)
  interventions / passes= 83 / 37
  joined / orphans      = 120 / 0 (orphan_rate 0.000)
dataset/
  manifest.json
  doer_version=occy-courier-v0/source=SLOW_LOOP_TRAJECTORY/part-000.parquet
```

Each row carries the courier's trajectory summary (point/object counts, endpoints, target
speed) joined to KIRRA's verdict — the supervised signal for tuning the courier DOER. Same
collector, same one-way dependency; only the capture **source** differs.
