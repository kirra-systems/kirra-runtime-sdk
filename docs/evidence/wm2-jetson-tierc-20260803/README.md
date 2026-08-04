# WM-2 Jetson tier C — physical power-cut durability — 2026-08-03

Five physical power cuts on the target device, closing the last open gate in
[ADR-0041](../../adr/0041-world-model-persistence-architecture.md)'s ratification
checklist. Produced by
[`tools/wm2-persistence-harness`](../../../tools/wm2-persistence-harness/)
following [the drill](../../hardware/JETSON_WM2_PERSISTENCE_DRILL.md) §8.

This is the one tier no software can produce. `SIGKILL` (tier A) leaves the page
cache intact, and tier B discards the whole WAL — neither distinguishes a
filesystem that honoured `fsync` from a device cache that acknowledged the write
and buffered it. The only instrument is a power switch, and this bundle is the
record of using one five times.

## Target and commit

| | |
|---|---|
| Device | NVIDIA Jetson Orin NX Engineering Reference Developer Kit Super |
| Kernel | `5.15.148-tegra`, `aarch64` |
| Storage | `ext4` on `/dev/nvme0n1p1` (NVMe) |
| Database | `/var/lib/kirra/wm2/powercut-v2.sqlite` — **fresh**, created for this series |
| Harness commit | `6eaeb643e3f8` — the corrected harness from [#1322](https://github.com/kirra-systems/kirra-runtime-sdk/pull/1322) |
| `PRAGMA integrity_check` | `ok` |

The commit is load-bearing. `6eaeb643` is the merge of #1322, which made a trial
mean an *arming* rather than a ledger row. A series recorded by any earlier
build cannot be counted — see *The corrected series* below.

## Result

**5 distinct armings · 5 physical cuts · 5 `PASS` · chain intact every time.**

| Trial | Arm id | Fsynced prefix | Recovered | Chain |
|---:|---|---:|---:|---|
| 1 | `08c190deb2aff047` | 400 | 3 562 064 | intact |
| 2 | `87c2f1a5fe1fd2b9` | 3 562 464 | 4 086 048 | intact |
| 3 | `ea795da4698e94b7` | 4 086 448 | 4 463 024 | intact |
| 4 | `b28ce3b1844f3856` | 4 463 424 | 4 632 000 | intact |
| 5 | `f565d7f947376e1a` | 4 632 400 | 4 748 880 | intact |

Final harness output:

```
tier C after 5 arming(s) across 5 row(s): PASS — 5 distinct power-cut arming(s)
recorded across 5 row(s), all preserving the fsynced prefix with an intact chain
```

**No acknowledged write was ever lost.** In every trial the recovered log was at
least as long as the fsynced prefix the marker promised, and the hash chain
verified end to end. Losing un-fsynced tail events would also have been correct;
in fact some tail survived each cut, which is why `recovered` exceeds the prefix.

### The ledger proves its own independence, arithmetically

Each arming appends a 400-event prefix starting at `MAX(generation) + 1` of
whatever survived the previous cut. So for consecutive rows:

```
trial n+1 fsynced prefix  ==  trial n recovered  +  400
```

Which holds exactly, four times over:

| | recovered | + | = next prefix |
|---|---:|---:|---:|
| 1 → 2 | 3 562 064 | 400 | 3 562 464 |
| 2 → 3 | 4 086 048 | 400 | 4 086 448 |
| 3 → 4 | 4 463 024 | 400 | 4 463 424 |
| 4 → 5 | 4 632 000 | 400 | 4 632 400 |

This is what makes the series checkable rather than merely asserted. Five
independent cuts on a store that carries forward produce a strictly increasing,
gap-free chain of boundaries; **the defect corrected by #1322 produced identical
`durable`/`recovered` values on every row instead**, because no second arming
ever ran. The arithmetic above is inconsistent with a replayed marker.

## The corrected series

An earlier attempt on `/var/lib/kirra/wm2/powercut.sqlite` recorded **three
`PASS` rows from one physical power cut**. `powercut arm` restarted at generation
0, so on a populated store the second arming died on `UNIQUE constraint failed:
world_events.generation` while the first marker survived — and each later
`verify` re-read the same surviving store and appended another pass. Trial 4
surfaced it by failing outright.

**That attempt counts as 1 valid independent cut, not 3.** It remains on the
device as defect evidence and **must not be combined with this series.** Its rows
predate arm ids, so the corrected harness refuses them as unattributable rather
than counting them — the exclusion is enforced by the instrument, not by
recollection.

This bundle is a fresh database and a fresh ledger, on the corrected harness.

## Limitations

- **Stand-in schema.** Durability was exercised against the harness's stand-in
  schema and hash chain, not a ratified Kirra World schema. The property
  established — this device does not acknowledge writes it has not persisted —
  is a property of the *medium and the fsync path*, so it carries further than a
  latency figure would. But the figures describing what was written do not.
- **One device, one medium.** A single Jetson Orin NX on one NVMe. Durability is
  a property of *that* storage. It does not transfer to eMMC or microSD, which
  is where lying write caches are most common.
- **Five cuts, not a distribution.** Five is the drill's floor, chosen so a pass
  is not a coincidence. It is not enough to bound a failure *rate*: device-cache
  loss depends where in the erase-block cycle the cut lands, and five samples
  cannot distinguish "never" from "rarely".
- **No harness `run` record.** `powercut verify` writes only to the trials
  ledger and exits; it never emits the `run` record that carries
  `evidence_status` and `source_digest`. Provenance rests on `GIT_COMMIT`
  instead. This is how the subcommand is built, not an omission by the operator.
- **Cut timing was not instrumented.** The operator cut power at an arbitrary
  point in each tail. Nothing records how far into a write each cut landed, so
  the five samples cannot be said to cover any particular part of the write
  cycle.

## Files

| File | |
|---|---|
| `trials.jsonl` | the 5 trial records, one per line — the evidence |
| `integrity.txt` | `PRAGMA integrity_check` on the store after the final cut |
| `ENVIRONMENT.txt` | kernel, device model, and the mount the database sat on |
| `GIT_COMMIT` | harness commit the cuts were performed with |
| `SHA256SUMS` | relative digests over every file above |

Verify from this directory:

```sh
sha256sum -c SHA256SUMS
```

The tier C verdict is a pure function of `trials.jsonl`, so it can be recomputed
rather than taken on trust — point the corrected harness at a throwaway database
with this ledger beside it:

```sh
cp trials.jsonl /tmp/replay.sqlite-tierc-trials.jsonl
wm2-persistence-harness crash --db /tmp/replay.sqlite --out /tmp/out.jsonl
```

The `C_power_cut_durability` record reads `PASS`, `5 of 5`.
