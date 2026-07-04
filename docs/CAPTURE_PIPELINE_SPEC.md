# Capture Pipeline Spec — corrective-supervision dataset for the learning loop

Status: SPEC (2026-06-05). Builds on `LEARNING_LOOP_ARCHITECTURE.md`. Assumes the
**hybrid** capture choice (§3 there): Kirra emits a small non-blocking verdict record; a
Linux collector joins it with bus telemetry into the full triple.

> **Recorded status.** Companion spec to `LEARNING_LOOP_ARCHITECTURE.md`. **The §3
> capture-location decision is CONFIRMED — hybrid (3)** (owner 2026-07-04); this spec is the
> hybrid-(3) elaboration. Its own §6 decisions (sink, correlation key, model-version
> attribution, dataset format, pass-sampling) are **all RESOLVED** by `COLLECTOR_DESIGN.md`
> D1–D6 (owner 2026-06-06) — see §6 below. **As-built note:** both emit seams exist on `main`
> today — fast-loop command gateway (Phase 1, #191) and the slow-loop
> `crates/kirra-ros2-adapter/src/node.rs` (Phase 1.5, #192): it binds `objects`,
> `effective_perception_cap`, then `verdict = validate_trajectory_slow_capped(...)`, and the
> capture call goes immediately after `verdict`. The §0 verdict-path anchor `997fb7ae…` is
> current on `main`.

## 0. The non-negotiable constraint
The verdict path stays byte-identical: **`src/gateway/kinematics_contract.rs` =
`997fb7ae15ce3e11adec9218044c7c84b049ad3b`** (`validate_vehicle_command`,
`enforce_degraded_decel_to_stop`, `DenyCode`, `effective_max_speed_mps`, …). Capture is
**purely additive at the call site** and must never:
- change the verdict, its inputs, or its timing/WCET;
- block, allocate, or do I/O on the verdict path;
- be required for the safety domain to function (it fails closed identically with capture on
  or off).

Capture is **off by default** behind a flag (`KIRRA_CAPTURE_ENABLED`); enabling it changes
no verdict.

## 1. Where it hooks (the seam — NOT the verdict path)
The slow-loop tick in `crates/kirra-ros2-adapter/src/node.rs` already computes everything
the safety side of the triple needs, right after:

```
let verdict = validate_trajectory_slow_capped(&traj.points, slow_corridor, &objects,
                                              &slow_state.config, odom, …);
```

At that point the tick holds: `traj` (the doer's PROPOSAL), `objects` (perception),
`odom` (ego), `posture`, `effective_perception_cap`, and `verdict` (the DECISION +
correction). The capture call goes **immediately after `verdict` is bound** — in `node.rs`,
not in `kinematics_contract.rs`. (Optionally a second, lighter record at the fast-loop
`check_command_conforms` conformance site.)

## 2. What Kirra emits — the verdict record (small, safety-side)
Keep the on-tick record tiny and fixed-shape; the bulky inputs are pulled from the bus by
the collector (§3). Fields:

| field | source | why |
|---|---|---|
| `decision_seq` | node-assigned monotonic counter | join key + ordering |
| `t_mono`, `t_wall` | clock | ordering / bus join |
| `corr_objects_ms` | `objects_ms` freshness stamp | join to the perception frame |
| `corr_traj_stamp` | the proposal trajectory's stamp | join to the proposal |
| `outcome` | from `verdict` | accept / clamp / MRC / reject |
| `deny_code` | `DenyCode` | which check fired (kinematic / corridor / RSS / staleness / perception-derate) |
| `applied_cap_mps` | `effective_max_speed_mps()` / `effective_perception_cap` | the CORRECTION Kirra imposed |
| `mrc` | bool | controlled-stop substitution |
| `posture` | `posture` | nominal / degraded / locked-out context |
| `derate_enabled` | `perception_derate_enabled()` | so passes are attributable |

This is the authoritative "what Kirra decided and did" — the **correction** half of the
triple — and only Kirra knows it. Note it does NOT carry the doer's model version (Kirra
doesn't know it); that's joined on the Linux side (§3).

## 3. Emission mechanism (WCET-safe, fire-and-forget)
- The call-site pushes the fixed record into a **bounded lock-free SPSC ring** (pre-
  allocated, no alloc, no lock, no syscall on the tick). If full → **drop/overwrite oldest**
  (capture is best-effort; safety never waits).
- A **separate low-priority drain task/thread** empties the ring and ships records to the
  sink. On QNX this is a low-priority thread; on Linux (bench) the same — OS-agnostic.
- **Sink [RESOLVED — D1]:** local **JSONL files**, one per emitting process (the writers as
  built append JSONL). DDS becomes a transport later iff live fleet aggregation is wanted.

## 4. The Linux collector — assembling the triple
A Linux-only service (never in the safety domain). It:
1. **Subscribes to the bus** and buffers, by correlation key (stamp/seq): the PROPOSAL
   (`traj` / pre-governed command), the PERCEPTION (`objects` → `PredictedObjects`), the
   EGO state (`odom`), and the **doer model version** (stamped by the doer on its
   telemetry — see decision below).
2. **Ingests verdict records** (from the sink).
3. **Joins** record ↔ bus telemetry on `decision_seq` (+ `corr_objects_ms` /
   `corr_traj_stamp` as cross-checks) → one **corrective-supervision sample**.
4. **Writes** the sample to the versioned dataset store (§5).

The collector is where all heavy data engineering lives — the certified checker stays tiny.

## 5. Dataset schema + versioning
One sample = the triple + provenance:

```
sample {
  sample_id, t_mono, t_wall,
  model_version,                 # doer's version (join), partition key
  scenario_tags[],               # optional labels (sim/real, scenario id)
  is_intervention,               # outcome != accept
  inputs   { perception: PredictedObjects, ego: Odometry, trajectory_in: Trajectory },
  proposal { command_or_trajectory },         # what the doer wanted
  verdict  { outcome, deny_code, applied_cap_mps, mrc, posture, derate_enabled }  # what Kirra did
}
```
- **Format [RESOLVED — D4]:** **Parquet/Arrow** for the joined tabular records; heavy sensor
  blobs stay in the rosbag/MCAP with a URI/offset `bulk_ref` in the Parquet row (don't copy
  multi-GB frames into Parquet). Partition `dataset/doer_version=<v>/source=<s>/*.parquet`.
- **Versioning:** partition by `(model_version, date)`; append-only; rotate/bound on the
  bench (it generates a lot). Each training run slices "samples generated by model vN."
- **Selection-bias note (from the architecture doc §5):** the dataset must include
  `is_intervention == false` samples (normal driving), not just corrections — the collector
  records every decision, with optional pass-sampling to control volume.

## 6. Decisions — RESOLVED (by `COLLECTOR_DESIGN.md` D1–D6, owner 2026-06-06)
All five sub-decisions this spec left open are now bound; the collector is where they take
effect. Recorded here for traceability:

1. **Sink → D1:** local **JSONL files** (one per emitting process), not DDS. DDS is a later
   transport option for live fleet aggregation. *(The writers as built append JSONL.)*
2. **Correlation key → D2:** `(source, decision_seq)` primary — `decision_seq` is
   **per-process**, and there are two emitting processes, so it is not globally unique alone;
   bounded by a `t_wall_ms` window and cross-checked with `traj.asset_id` /
   `traj.trajectory_id` / `traj.objects_ms`. *(Follow-up: a gateway-record asset/instance id
   is needed IFF multi-asset comes into scope — `COMMAND_GATEWAY` records carry no `asset_id`.)*
3. **Model-version attribution → D3:** doer-stamped, collector joins, **Kirra stays ignorant**.
   The bench run records the doer version (a latched `/kirra/doer_version` topic or bag
   metadata); the collector partitions the dataset by it.
4. **Dataset format → D4:** **Parquet/Arrow** for the tabular join + a `bulk_ref` URI/offset
   to the heavy blobs in the rosbag/MCAP (not copied into Parquet).
5. **Pass-sampling → D5:** **stratified** — keep ALL clamp/deny/MRC records always; sample
   PASS records at a configurable rate. Bench default `pass_rate = 1.0` (keep everything).

Plus **D6 (collector placement)**: a Rust in-repo `kirra-collector` binary reusing
`CaptureRecord` from the SDK lib for a type-safe join (one authoritative schema, no drift);
it never links the verdict path.

## 7. Build phases
1. **Verdict record + ring + drain + call-site hook** (Rust, in `node.rs`/adapter — NOT
   `kinematics_contract.rs`). Tests: capture never blocks; **verdict path blob still
   `997fb7ae…`**; verdicts identical with capture on vs off.
2. **Sink** (telemetry topic or file) + a tiny reader.
3. **Linux collector** (bus tap + record ingest + join → sample).
4. **Dataset store + schema + versioning.**
5. (Downstream) training/validation consumers read the dataset.

Phase 1 is the buildable-today piece and the only one that touches the repo's safety-
adjacent code — everything after is Linux-side tooling.

> **Build-time guardrails (restating §0 against the merged code):** Phase 1 lives in
> `crates/kirra-ros2-adapter/` (and a small `kirra-runtime-sdk` record type), gated behind
> `KIRRA_CAPTURE_ENABLED` (default OFF), mirroring the existing fire-and-forget emit
> discipline (`audit_writer_tx.try_send` — wait-free, drop-on-full) and the
> `KIRRA_PERCEPTION_DERATE_ENABLED` default-off precedent. `src/gateway/kinematics_contract.rs`
> stays byte-identical (`997fb7ae…`); the on-tick push is a bounded, droppable enqueue that
> the verdict never waits on.
