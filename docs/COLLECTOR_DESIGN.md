# Learning-Loop Collector — Design (signed off)

The collector is the **offline** Linux tool that turns the two dark capture
streams + a recording of the bus into a versioned training dataset. It closes the
gap between "Kirra emits corrective-supervision records" (Phase 1 + 1.5 on `main`)
and "the doer's models get retrained" (downstream).

It is the first place the capture-spec §6 decisions actually bind, plus one the
spec didn't list (where the collector lives / what language).

> **Status (2026-06-06):** D1–D6 below are **CONFIRMED** (owner sign-off). This
> doc is the design of record the build prompt elaborates from.

> **Source-stream note:** both emit points exist on `main`. **Phase 1**
> (`COMMAND_GATEWAY` records) merged via #191; **Phase 1.5**
> (`SLOW_LOOP_TRAJECTORY` records — the ROS 2 adapter slow-loop emit) merged via
> #192 (`from_trajectory_verdict` + `CaptureSource::SlowLoopTrajectory` live in
> `src/capture.rs`). The collector can safely assume both streams exist on `main`.

---

## 0. Safety boundary (non-negotiable, state it up front)

The collector is **offline, out-of-vehicle, and not safety-critical.** It never
links or imports the verdict path, never runs in the live loop, and produces only
a dataset. It has zero ability to affect a live Kirra verdict. This is what keeps
the doer/checker independence intact: Kirra (fixed checker) emits dark records; a
separate tool assembles data; training happens elsewhere; a **human-gated,
AWSIM-validated** model is what ever reaches the doer. The collector touching
nothing in the governor is an invariant, not a goal.

Note on [D6]: "reuse `CaptureRecord` from the SDK lib" is a *library* dependency
on the record type only — it does **not** link the verdict path
(`kinematics_contract.rs`) and does not weaken this boundary.

---

## 1. What it consumes

- **Capture JSONL**, two per-process streams, distinguished by the `source` field
  already on every record:
  - `COMMAND_GATEWAY` records — carry `proposed` (ProposedCommandSnapshot).
  - `SLOW_LOOP_TRAJECTORY` records — carry `traj` (TrajectoryCaptureExt).
  - Common fields: `decision_seq`, `t_mono_ns`, `t_wall_ms`, `outcome`,
    `deny_code`, `safe_value`, `mrc`, `posture`, `derate_enabled`.
  - These are **bounded summaries + join keys**, by design — not the bulk data.
- **A bus recording** (rosbag2) of the same bench run: the doer's proposal topics,
  perception (`objects`), odom, and a doer-version stamp. This is where the
  **bulk** lives — full trajectory points, full object lists, sensor frames.

The collector's whole job is to **join** the tiny safety-side records to the bulk
on the bus, attach the model version, and write a training-ready dataset.

---

## 2. Decisions (§6 + the new one) — CONFIRMED 2026-06-06

**[D1] Sink — files. CONFIRMED.** The spec floated DDS; the writers as built
append **per-process JSONL**. For the bench/offline loop that's correct and
simplest — the collector reads files. DDS becomes a transport later if/when we
want live fleet aggregation. → *Collector input = JSONL files + a rosbag2 of the
run.*

**[D2] Correlation key — `(source, decision_seq)` primary, stamps + ids as
cross-check. CONFIRMED.** Caveat the spec glossed: `decision_seq` is
**per-process**, and there are two emitting processes, so `decision_seq` alone is
not globally unique. The join key is `(source, decision_seq)` against the bus,
bounded by a `t_wall_ms` window and cross-checked with `traj.asset_id` /
`traj.trajectory_id` / `traj.objects_ms`.

> **Multi-asset sharpening (build-prompt input):** `source` distinguishes the two
> emit *kinds*, not multiple *instances* of the same kind. Today's single-asset
> bench is fine. But the `COMMAND_GATEWAY` record carries **no `asset_id`** (only
> `traj` records do), so two adapter instances at fleet scale would collide on
> `(source, decision_seq)`. If/when multi-asset is in scope, add an
> asset/instance id to the gateway record before relying on the join there.

**[D3] Model version — doer-stamped, collector joins, Kirra stays ignorant.
CONFIRMED.** On the bench, record Autoware's model/config version into the bag (a
latched `/kirra/doer_version` topic, or bag metadata). The collector reads it from
the bus side and stamps each joined record, then **partitions the dataset by it.**
Kirra never sees it. → *Bench run must publish/record a doer version (see §5).*

**[D4] Dataset format — Parquet for the join, references for the bulk.
CONFIRMED.** Write the joined **tabular** records (the triple summaries + outcome
label + posture + model-version partition + join keys) as **Parquet/Arrow** —
columnar, schema-stable, native to the training stack. Do **not** copy multi-GB
sensor frames into Parquet; keep heavy blobs in the rosbag/MCAP and store a
**URI/offset reference** in the Parquet row. Partition layout:
`dataset/doer_version=<v>/source=<s>/*.parquet`.

**[D5] Pass-sampling — stratified, keep every intervention. CONFIRMED.** Selection
bias is the trap (training only on clamps teaches the wrong thing). Keep **all**
clamp/deny/MRC records always; **sample PASS records** with a configurable rate.
Bench default `pass_rate = 1.0` (keep everything); the knob exists to downsample
abundant passes later without ever dropping the rare corrective signal.

**[D6 — new] Language & placement — Rust binary, in-repo, outside the governor.
CONFIRMED (Rust).** A separate **`kirra-collector`** binary (its own crate, or
`tools/`) that **reuses `CaptureRecord` from the SDK lib** for deserialization —
one authoritative schema, type-safe join, no drift. Parquet via the
`arrow`/`parquet` crates; rosbag2 read via its SQLite/MCAP store. It compiles and
ships separately from the safety crate and never links the verdict path (supports
[D0]).
- *Rejected alternative:* a Python collector (pyarrow, rosbag2_py, pandas) sits
  closer to the PyTorch training stack but re-declares the schema (drift risk).
  Not chosen. If a downstream Python consumer ever needs the schema, generate a
  JSON Schema from the Rust `CaptureRecord` type rather than hand-maintaining it.

---

## 3. Output dataset schema (one row per captured decision, post-join)

`decision_seq, source, t_wall_ms, t_mono_ns, doer_version (partition), outcome,
deny_code, mrc, posture, derate_enabled, safe_value,` then source-specific:
gateway → the `proposed` command fields + the actuated/safe command; trajectory →
`asset_id, trajectory_id, objects_ms, point_count, object_count, first/last pose,
target_speed`, plus `bulk_ref` (URI/offset into the bag for the full trajectory +
objects + frames). The label for training is the **(state, doer proposal, Kirra
correction)** triple; the collector's join is what reunites the proposal/correction
summary with the full state from the bus.

---

## 4. Phase plan

- **Phase 1 (first build):** read both JSONL streams + one rosbag → join on
  [D2] → write Parquet partitioned by `doer_version` [D4] with stratified
  pass-sampling [D5] and `bulk_ref` links. Validate against one recorded bench
  session (counts reconcile: every clamp/deny/MRC record present; pass count
  matches the sampling rate; join hit-rate reported; orphans flagged). No
  training, no live anything.
- **Phase 2:** schema/versioning hardening, dataset manifest + lineage stamp
  (so a trained model can point back at the exact dataset + capture commit), and
  join-quality metrics.
- **Phase 3 (separate track):** the training + AWSIM-validation + human-release
  gate. Out of scope here.

---

## 5. Prerequisite (bench procedure, not collector code)

The join only has bulk to attach to if the bench run **records the right topics**:
the doer's proposal/trajectory topics, perception `objects`, odom, and the
`doer_version` stamp [D3]. That recording step is part of the AWSIM/Autoware
bring-up on the GPU box — flagging it so it's captured from the first run.

---

## Sign-off

D1 files ✓  D2 `(source, decision_seq)` + window ✓  D3 doer-stamped version ✓
D4 Parquet + bulk-ref ✓  D5 stratified, bench `pass_rate=1.0` ✓
D6 **Rust, in-repo** ✓  (owner sign-off 2026-06-06)

Open follow-ups, tracked but non-blocking:
- Gateway-record asset/instance id, *iff* multi-asset comes into scope (D2 sharpening).
