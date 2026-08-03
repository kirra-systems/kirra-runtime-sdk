# WM-2 Jetson scale sweep + stall matrix — 2026-08-03

Target measurements for [ADR-0041](../../adr/0041-world-model-persistence-architecture.md),
closing the **scale sweep** gate and answering **open question 9** (the ~29 s
write stall). Produced by
[`tools/wm2-persistence-harness`](../../../tools/wm2-persistence-harness/)
following [the drill](../../hardware/JETSON_WM2_PERSISTENCE_DRILL.md) §9 and §9a.

Companion to [`../wm2-jetson-20260803/`](../wm2-jetson-20260803/), the earlier
`all` run. That bundle is frozen against commit `021ec82379be`; this one is a
separate, later run and does not amend it.

## Target and commit

| | |
|---|---|
| `evidence_status` | **`JETSON-TARGET-MEASURED`** (`citable: true`, `blockers: []`) |
| Device | NVIDIA Jetson Orin NX Engineering Reference Developer Kit Super |
| Arch | `aarch64` |
| Storage | `ext4` on `/dev/nvme0n1p1` (NVMe) |
| Build | release, `rustc 1.94.1` |
| Harness commit | `ba818b0b22b3` |
| `source_digest` | `e596fc824dc5f540e7e7943f5e53e830dcacf5c747f446bc30f0ca3f9e5d4db8` |
| `standin_schema_digest` | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |

`source_digest` was verified independently: building the harness from
`ba818b0b22b3` reproduces `e596fc82…` exactly, so these runs used the merged
instruments from the recorded commit. `ENVIRONMENT.txt` derives every field
above by reading it back out of the 22 result records — a field that disagreed
between records would surface as a conflict rather than a single line. None did.

> The `standin_schema_digest` is load-bearing. These numbers describe the
> harness's **stand-in** schema, not a ratified one. When the real schema lands
> its digest differs, and every figure here becomes a figure about something
> else.

## Method — constant density

Both sweeps hold **observations per entity constant** at 100, so each rung uses
`entities × 100` events. This is the deployment-realistic shape: more entities
means more observations, not the same observations spread thinner.

The alternative — sweeping entity count at a fixed *total* event count — makes
each entity thinner, so fan-out and cost go **down**, and the ladder reports
excellent sublinear scaling while measuring density rather than scale. The
harness refuses that ladder (`INSUFFICIENT`) rather than praising it; see
ADR-0041 D-8.

## Graph — `graph_bounded_reach`

| Entities | Events | Median | p99 | Rows matched |
|---:|---:|---:|---:|---:|
| 1 000 | 100 000 | 11.46 ms | 13.56 ms | 420 983 |
| 10 000 | 1 000 000 | 50.92 ms | 71.43 ms | 2 489 685 |
| 100 000 | 10 000 000 | **91.95 ms** | **158.95 ms** | 7 169 733 |

**`SUBLINEAR`, overall log-log slope 0.45** (segments 0.65 → 0.26).
`supports_option_a: true`.

100× the entities costs 8.0× the time, and the exponent *falls* along the
ladder rather than rising. No knee.

## Temporal — `temporal_changes_since`

| Entities | Events | Median | p99 | Rows matched |
|---:|---:|---:|---:|---:|
| 1 000 | 100 000 | 28.44 ms | 61.64 ms | 197 315 |
| 10 000 | 1 000 000 | 427.76 ms | 751.68 ms | 1 993 963 |
| 100 000 | 10 000 000 | **5 536.23 ms** | **10 503.70 ms** | 19 838 752 |

**`LINEAR`, overall log-log slope 1.14** (segments 1.18 → 1.11).
`supports_option_a: true`.

### The verdict is about shape, and the shape is not the whole story

Both families pass ADR-0041's reopening condition: neither curve bends upward,
so SQLite is not falling off a cliff and Option B is not indicated. That is the
question the gate asked, and the answer is clean.

It is not a performance endorsement. At the top rung the **absolute** costs are:

| | p99 | as a multiple of a 100 ms (10 Hz) period |
|---|---:|---:|
| `graph_bounded_reach` | 159 ms | **1.6×** |
| `temporal_changes_since` | 10 504 ms | **105×** |

A ten-second p99 is linear *and* unusable interactively. 100× the entities costs
195× the time here — proportional in exponent, brutal in constant. Reading only
`verdict: LINEAR` and moving on would be the mistake this table exists to
prevent.

## Stall matrix — 20 repetitions per configuration

| Config | Stalls | Worst commit | Median worst | Median throughput | Attribution |
|---|---:|---:|---:|---:|---|
| `FULL` batch 1 | 0/20 | 18.75 ms | 12.93 ms | 3 099 ev/s | `NO-STALL` |
| `FULL` batch 64 | **2/20** | **19 644.91 ms** | 9.09 ms | 31 497 ev/s | `UNATTRIBUTED` |
| `NORMAL` batch 1 | **1/20** | **12 864.32 ms** | 45.27 ms | 5 143 ev/s | `IO-DEVICE` |
| `NORMAL` batch 64 | 0/20 | 63.16 ms | 52.96 ms | 19 351 ev/s | `NO-STALL` |
| `OFF` batch 1 | 0/20 | 29.83 ms | 1.20 ms | 15 079 ev/s | `NO-STALL` |
| `OFF` batch 64 | 0/20 | 4.00 ms | 3.79 ms | 55 275 ev/s | `NO-STALL` |

All six exited 0; `stall/status.log` carries six `exit=0` rows and `COMPLETE`.

### The original theory is rejected

The earlier `all` run recorded a **29.27 s** commit under `NORMAL`/batch 64,
which made that configuration the suspect. Twenty repetitions of *exactly* that
configuration produced **zero stalls**, worst commit **63 ms** — three orders of
magnitude below the original event.

The stall is **not** a property of `NORMAL`/batch 64. It appeared instead in
`FULL`/batch 64 and `NORMAL`/batch 1, neither of which was suspected.

### What the matrix does and does not support

- **Intermittent.** 3 stalls in 120 repetitions (2.5 %). Rare enough that a
  single run can miss it entirely — which is exactly what happened in four of
  the six configurations here.
- **Not durability-specific.** It crossed both `FULL` and `NORMAL`, and both
  batch sizes. `synchronous` is the only variable the harness controls, so a
  stall that ignores it is not explained by `synchronous`.
- **Never observed with `OFF`.** 0/40 across both `OFF` configurations versus
  3/80 across the fsyncing ones. Suggestive that fsync is involved — but three
  events is far too few to call it, and the honest reading is *not yet
  distinguishable from chance*.
- **Thermal is ruled out by measurement, not assumption.** The hottest zone
  across all six runs was **59.6 °C**, against an 85 °C threshold. No run came
  close.
- **One attribution, held loosely.** `NORMAL`/batch 1 read `IO-DEVICE`: the
  block layer was busy with no large dirty backlog, which on NVMe points at
  garbage collection or an SLC-cache cliff. `FULL`/batch 64 read
  `UNATTRIBUTED` — the counters did not discriminate, which is the expected
  outcome and not a failed measurement.

**Mechanism remains partly unresolved.** The evidence supports
*intermittent, block-device/environment-correlated*; it does not identify a
cause.

### The median throughput figures did their job

`NORMAL`/batch 64 reported **19 351 ev/s** here across 20 runs. The original
single run reported **3 123 ev/s** for the same configuration, because one
29 s stall consumed ~91 % of its wall time. That row is now known to have been
a stall artefact rather than a throughput regime, and the median across
repetitions is what separates the two.

### An inversion that survives the stalls

Reading the medians, which no single stall can move:

```
batch=1 : OFF 15 079  >  NORMAL  5 143  >  FULL  3 099     expected ordering
batch=64: OFF 55 275  >  FULL   31 497  >  NORMAL 19 351    INVERTED
```

At batch 64, `NORMAL` is measurably **slower** than `FULL`, which no durability
model predicts — and `NORMAL`/batch 64 recorded **zero** stalls, so this is not
a tail artefact. Unexplained, and recorded rather than smoothed over.

## Limitations

- **Stand-in schema.** Every figure describes the harness's schema, not a
  ratified Kirra World schema.
- **One device.** A single Jetson Orin NX on one NVMe. Durability and stall
  behaviour are properties of *that* medium and do not transfer to eMMC or
  microSD.
- **No SMART data.** `nvme smart-log` was not captured, so the `IO-DEVICE`
  attribution cannot be confirmed against device wear, media errors or the
  drive's own thermal-throttle counters. Drill §9a asks for it; it is missing
  here.
- **No `tegrastats`.** SoC clock and throttle behaviour over the run is
  unrecorded. Thermal is ruled out from `/sys/class/thermal` alone.
- **PSI unavailable.** `psi_io_stall_us` is `None` in every record — this
  kernel does not expose `/proc/pressure/io`. Attribution rested on
  `/proc/diskstats` busy-time alone, so one of the two I/O signals was simply
  absent.
- **Whole-repetition counters vs a single commit.** The attribution compares a
  counter delta taken across an entire repetition against the duration of the
  single worst commit inside it. That can over-state I/O evidence, and is a
  reason to hold the one `IO-DEVICE` reading loosely. A per-commit sampling
  window would be a harness change, deliberately out of scope for this
  evidence PR.
- **No real filesystem `ENOSPC`.** Disk-pressure behaviour was established via
  `PRAGMA max_page_count` in the earlier bundle — SQLite's full-*database*
  path, not a genuinely full partition.
- **Tier C durability remains `NOT-RUN`, `0/5`.** Nothing here supports any
  durability claim. That gate needs a power switch.

## Design implication

The temporal figures are the operative constraint: a p99 of **10.5 s** at the
top rung, from a store that a robot is also trying to write to.

**Semantic persistence must not be able to block safety or actuation.** That is
a structural requirement, not a tuning target, and it does not depend on the
stall being explained — a 10.5 s query and a 19.6 s stall are equally fatal to a
control loop that waits on them.

What that implies, for WM-2 design rather than for this bundle to decide:

- a **bounded queue** between producers and the store, so a slow write applies
  backpressure to the queue rather than to the caller;
- **explicit backpressure/shed** semantics at the queue boundary, with dropping
  observations a declared behaviour rather than an emergent one;
- a **latency watchdog** on store operations, since the stall is intermittent
  and rare enough that only continuous monitoring will catch it in the field;
- **writer isolation** — the checker and actuation path must not share a thread,
  a connection, or a lock with the world-model writer.

None of these are proposed as decided. They are recorded here because the
measurement is what makes them necessary.

## Files

| File | |
|---|---|
| `graph-sweep.jsonl` | 3 `sweep_point` + 1 `sweep_summary` |
| `temporal-sweep.jsonl` | 3 `sweep_point` + 1 `sweep_summary` |
| `stall/*.jsonl` | six configurations, one `stall` record each |
| `stall/status.log` | six `exit=0` rows + `COMPLETE` |
| `stall/nohup.out` | run transcript, six `JETSON-TARGET-MEASURED` banners |
| `ENVIRONMENT.txt` | environment derived from the records; names what is absent |
| `GIT_COMMIT`, `GIT_STATUS` | tree state on the device at run time |
| `SHA256SUMS` | relative digests over every file above |

Verify from this directory:

```sh
sha256sum -c SHA256SUMS
```

### Selection was by content, not timestamp

The device also held an earlier stall directory containing a single file with
one `run` record, no `stall` record, no `status.log` and no other
configurations — an aborted attempt. It is **excluded**. Every file archived
here was checked to carry its expected records: 3 points + 1 summary per sweep,
exactly one `stall` record per configuration, and a `status.log` with six
`exit=0` rows and `COMPLETE`.

`GIT_STATUS` shows the result files as untracked (`??`) on the device — expected,
since they were produced into the working tree before being committed here. No
tracked source file was modified, so `GIT_COMMIT` (`ba818b0b22b3…`) describes
the code that ran.
