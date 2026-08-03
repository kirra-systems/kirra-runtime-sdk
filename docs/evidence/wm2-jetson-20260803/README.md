# WM-2 Jetson evidence — 2026-08-03

Target measurements for [ADR-0041](../../adr/0041-world-model-persistence-architecture.md),
produced by [`tools/wm2-persistence-harness`](../../../tools/wm2-persistence-harness/)
following [the drill](../../hardware/JETSON_WM2_PERSISTENCE_DRILL.md).

## Provenance

| | |
|---|---|
| `evidence_status` | **`JETSON-TARGET-MEASURED`** (`citable: true`, `blockers: []`) |
| Device | NVIDIA Jetson Orin NX Engineering Reference Developer Kit Super |
| Arch | `aarch64` |
| Storage | `ext4` on `/dev/nvme0n1p1` |
| Build | release, `rustc 1.94.1 (e408947bf 2026-03-25)` |
| Harness commit | `021ec82379be` (clean — no `-dirty` suffix) |
| `source_digest` | `a0c2c1c870d68f6d9951c2cc0a8744126737bfa9d7a742dba5fc41e4c6bd0b63` |
| `standin_schema_digest` | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| Volume | 100 000 events, 96-byte payload, seed `20260803` |
| SQLite | 3.45.0 |

The `standin_schema_digest` is load-bearing. These numbers describe the
harness's **stand-in** schema, not a ratified one — when the real schema lands
its digest differs, and every figure here becomes a figure about something else.

## Run inventory

`results.jsonl` — **21 records**, a complete `all` run:

| Records | Stage |
|---|---|
| 1 | `run` (the classification header) |
| 6 | `append` — 3 durability settings × 2 batch sizes |
| 1 | `replay` |
| 5 | `query` — the four §12 families plus the bitemporal point query |
| 1 | `growth` |
| 1 | `migrate` |
| 1 | `compact` |
| 1 | `pressure` |
| 1 | `reclaim` |
| 3 | `crash` — tiers A, B, C |

The count is worth recording: a stage that failed to run leaves no record, so
21 with this breakdown is the difference between "every stage completed" and
"every stage that completed passed".

## What this bundle does and does not establish

**Established on target:** replay (with `deterministic: true`), all four query
families, storage growth, migration (fail-closed on a future schema), compaction
with citation (all nine §11.3 conditions including the tamper control), disk
pressure, and crash tiers A and B.

**Not established:** **tier C — physical power-cut durability.** The run reports
`NOT-RUN` with `tier_c_trials_recorded: 0` of `tier_c_trials_required: 5`. No
durability claim is supported by this bundle. Nothing in software distinguishes
an honest `fsync` from a device cache that acknowledged and buffered it; the
only instrument is a power switch, and the procedure is drill §8
(`powercut arm` / `powercut verify`).

Consequently **ADR-0041 remains Proposed.**

## Files

| File | | Present |
|---|---|---|
| `SHA256SUMS` | digests **as taken on the device**, transcribed here | yes |
| `results.jsonl` | the run, one JSON record per measurement | **not yet** |

Run `sha256sum -c SHA256SUMS` from this directory. Today it reports:

```
../../hardware/JETSON_WM2_PERSISTENCE_DRILL.md: OK
results.jsonl: FAILED open or read
```

which is the accurate state of the bundle: **the procedure is pinned and
verified, the results file has not arrived.**

### The drill digest is a verified pin, not a copy

The runbook is *not* duplicated into this directory. Instead `SHA256SUMS`
points at the tracked copy, and that line **passes**: the digest recorded on
the device, `4ecdb939…`, is byte-identical to
`docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md` at commit `021ec82379be` — the
same commit the harness binary was built from — and to the copy at `HEAD`.

That is stronger than shipping a second copy. It proves *which tracked
revision* of the procedure was followed, and a later edit to the drill will
make this line fail rather than silently diverging from an archived duplicate.

> **`results.jsonl` has not been committed.** It exists on the device at
> `~/wm2-jetson-evidence-20260803-083703/`. It is deliberately *not*
> reconstructed from terminal output: a retyped file is a transcription, not the
> artifact, and its digest would not match. Copy the real file here and verify:
>
> ```sh
> cd docs/evidence/wm2-jetson-20260803 && sha256sum -c SHA256SUMS
> ```
>
> `SHA256SUMS` is committed first on purpose — it is the independent statement
> of what the file must hash to, recorded before the file arrives, so a
> substituted or edited copy fails the check rather than being accepted.
> The drill's own digest is listed alongside for the same reason: it pins which
> revision of the procedure was followed.

The analysis and the decisions drawn from these numbers are in ADR-0041
*Target measurements* (D-1 … D-6, O-1). Figures should be read from
`results.jsonl` rather than from prose — that separation is the whole point of
the evidence gate.
