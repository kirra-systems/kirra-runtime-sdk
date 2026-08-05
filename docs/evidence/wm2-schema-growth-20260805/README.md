# WM-2 — bytes/event against the RATIFIED schema: OQ2's allocation no longer fits

| | |
|---|---|
| **Captured** | 2026-08-05, x86_64 build host (CI-class container), ext4 on `/dev/vda` |
| **Evidence status** | `HOST-PAIRED` — see *Why a host run is admissible here*, below |
| **Repo commit** | `2ea66cf2` (#1350, the ratified store) |
| **Ratified schema digest** | `502b5460331d842b8363d89c81856e2333bb54060091526f6d98a932ece66203` |
| **Stand-in schema digest** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **SQLite** | 3.45.0 (bundled), identical in both arms |
| **Instruments** | `tools/wm2-schema-growth` (new), `tools/wm2-persistence-harness growth` |
| **Written up as** | ADR-0041 **D-20** |
| **Bears on** | ADR-0041 **OQ2**; `KIRRA-WM2-SCHEMA-001` **§5, §8.4** |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this run answers

`KIRRA-WM2-SCHEMA-001` §8.4 recorded an obligation created by ratifying the
event schema: D-2's **458.51 B/event**, and OQ2's budget of **18 033 812
events** derived from it, were measured against the harness's deliberate
**stand-in** schema. The ratified schema adds six columns. Until re-measured,
OQ2's horizons stood on a number about a different table.

**Measured. The figure moves by 1.24×–1.34×, and OQ2's allocation overruns the
corrected budget.**

## The measurement

Two arms, same host, same session, same SQLite build, same event stream
(seed `20260803`, 100 000 events, 1 000 entities, 96-byte payload) — D-2's own
parameters. Log-only in both arms, because `kirra-world-store` has no
projections yet.

| Arm | Schema | B/event | Days to fill 8 GiB @ 10 Hz | Ratio vs stand-in |
|---|---|---:|---:|---:|
| control | stand-in (D-2's) | **458.50624** | 21.68 | 1.000× |
| ratified, `lean` | ratified | **566.55872** | 17.55 | **1.236×** |
| ratified, `populated` | ratified | **612.18816** | 16.24 | **1.335×** |

**Counting unit:** bytes of on-disk database per appended event, where bytes is
`len(main) + len(-wal) + len(-shm)` after a `wal_checkpoint(TRUNCATE)` — the
harness's own `db_bytes` definition, reused verbatim so the two arms measure
the same thing.

**Independence unit:** one database build. Events within a build are *not*
independent — SQLite page fill depends on insertion order — and **no per-event
variance is claimed**. Each arm was built twice; both arms reproduced
**bit-identically** (same `log_only_bytes` to the byte), so the quantity is
deterministic and a variance estimate would be measuring nothing.

**Held fixed:** platform, SQLite build, event stream, log-only condition.
**Varied:** the schema, and the fill of the columns it added.

**The claim this supports:** the multiplicative change in log-only growth
attributable to the ratified schema. It does not support any latency,
throughput, or durability claim — none was measured.

## The band, and why there are two ratified numbers

Four of the six added columns are variable-width TEXT and two are nullable, so
"bytes/event under the real schema" is a function of how much of that width
real traffic carries. Nothing measured so far constrains that, so the
instrument reports a **band** with both ends named rather than one number
resting on an unstated guess:

- **`lean`** — added columns at their lightest legal values: `provenance` `[]`,
  `frame_id` and `map_id` NULL. A real configuration, not an invented floor:
  it is what a raw non-spatial sensor observation produces, and SD-4 permits a
  NULL frame precisely for those.
- **`populated`** — `provenance` citing one upstream observation, `frame_id`
  and `map_id` set. Also real: SD-4 makes the frame **mandatory** for spatial
  claims, so this is the shape of any perception-derived spatial event.

**Horizons must be taken from `populated`.** A retention horizon says when a
disk fills; taking it from the lean end gives the longest life and the least
margin, which is the wrong direction to be wrong in.

`writer_class` and `claim_status` were pinned to the sensor path and not
varied — closed vocabularies a few bytes wide cannot move a per-event figure
meaningfully. `observation_id` is present in both fills because it is NOT NULL;
the schema offers no lean option there.

## The consequence: OQ2's allocation does not fit

OQ2's budget of 18 033 812 events is 8 GiB at D-2's **with-projections** figure
(476.32384 B/event). Against the ratified schema's **log-only** figure:

| Basis | Budget (events) | vs OQ2 |
|---|---:|---:|
| OQ2 (stand-in, with projections) | 18 033 812 | — |
| ratified, `lean`, log-only | 15 161 596 | 0.841× |
| ratified, `populated`, log-only | **14 031 527** | **0.778×** |

OQ2 allocated **11 664 000** events to `raw` (30 days at ≤4.5/s) and
**3 784 320** to the protected classes (365 days at ≤0.12/s) — **15 448 320**
together, against a stated 14 % headroom.

| Against | Headroom |
|---|---:|
| ratified `lean` | **−286 724 (−1.9 %)** |
| ratified `populated` | **−1 416 793 (−10.1 %)** |

**The headroom is gone and the allocation overruns the budget at both ends of
the band.** And the overrun is *understated*: these are log-only figures
compared against a budget that included projections. `kirra-world-store` has
not built projections yet, so the with-projections figure for the ratified
schema **cannot be measured**, only bounded — it is strictly larger, so the
real deficit is larger than shown.

### What the corrected arithmetic permits — not a ruling

OQ2 was ruled on 2026-08-05, and the input the ruling turns on — how far back
an incident reconstruction must reach — is **unchanged**. What changed is how
many events fit. The levers, with the numbers, for whoever re-rules it:

| Lever | Effect (ratified `populated`, protected held at 365 d, 14 % headroom) |
|---|---|
| Keep 30-day `raw`, coalesce harder | sustained rate **4.5 → 3.20 /s** (~3.1× from 10 Hz, was ~2×) |
| Keep ~4.5/s, shorten `raw` | 30 days → **21.3 days** |
| Raise the budget | 8 GiB → ~10.3 GiB restores the original allocation |

**This evidence set does not re-rule OQ2.** It records that the ruling's
allocation was sized against a stale figure and no longer closes, and supplies
the arithmetic for each exit. Which lever to pull is a decision about incident
reconstruction, not about bytes.

## Why a host run is admissible here

The harness labels host runs `HOST-INDICATIVE-NOT-TARGET` and refuses them
against ADR-0041's ratification checklist. That label is correct and is
**not** being argued around. Two things make this bundle usable anyway, and
both have limits worth stating:

1. **The control arm reproduced the Jetson figure exactly.** Re-running the
   stand-in `growth` on this host produced `log_only_bytes` **45 850 624** and
   `bytes_per_event` **458.50624** — byte-for-byte identical to D-2's
   `JETSON-TARGET-MEASURED` record. That is expected: the quantity is the
   *logical* length of a SQLite file, which is determined by the SQLite build,
   the page size, the schema and the data — none of which differ between the
   two machines. It is an empirical identity on one pair, **not a proof of
   platform invariance**, and it would not transfer to any timing measurement.

2. **The reported result is a ratio taken within one host.** Whatever platform
   dependence might exist, it is held fixed across the two arms and divides
   out. This is why the instrument **refuses to emit a ratio** unless a
   same-host stand-in figure is supplied (`--standin-bpe`); a ratio taken
   across two machines is a schema ratio confounded with a platform
   difference, and the tool will not fabricate one.

**Still owed:** a target run with `--assert-target` if a figure is to be
entered against ADR-0041's ratification checklist as `JETSON-TARGET-MEASURED`.
Given (1), that run is expected to confirm rather than revise, and it should be
run anyway — an expectation is not a measurement.

## Files

| File | What it is |
|---|---|
| `results.jsonl` | 6 records: the harness `run` + `growth` control, two ratified `growth` arms, two `paired_ratio` records |
| `SHA256SUMS` | Digests of the above |

Reproduce:

```
cd tools/wm2-persistence-harness && cargo run --release -- growth \
  --events 100000 --entities 1000 --payload-bytes 96 --seed 20260803

cd tools/wm2-schema-growth && cargo run --release -- \
  --events 100000 --entities 1000 --payload-bytes 96 --seed 20260803 \
  --standin-bpe <the figure from the line above>
```

## What is NOT established

- **No target-hardware status.** See above. This bundle is not citable as
  `JETSON-TARGET-MEASURED`.
- **No with-projections figure for the ratified schema.** Not measurable until
  projections exist. The budget comparison above is therefore optimistic.
- **No claim about the band's true position.** `lean` and `populated` bound a
  modelling choice; where real traffic sits between them is unmeasured, and
  will stay so until real observations are recorded.
- **Nothing about latency, throughput, durability or crash behaviour.** This
  instrument appends and measures a file length. ADR-0041 D-11's power-cut
  gate, D-15/D-17's timing work and D-19's stall population are untouched by
  it.
- **No deep-provenance case.** `populated` cites one upstream observation.
  SD-3 permits many, and a derivation-heavy workload would sit above this
  band's upper end.
