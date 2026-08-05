# WM-2 — post-repair baseline: the filesystem repair moved nothing

| | |
|---|---|
| **Captured** | 2026-08-04 21:40 EDT, Jetson Orin NX (`yahboom`), aarch64, L4T 5.15.148-tegra |
| **Evidence status** | `JETSON-TARGET-MEASURED` — citable against ADR-0041 |
| **Harness commit** | `83998315` — the **same commit** as the OQ1 run |
| **Instrument digest** | `8882f659…` — gate-checked identical to OQ1 before any measurement ran |
| **Stand-in schema** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **Written up as** | ADR-0041 **D-19** |
| **Bears on** | ADR-0041 **D-18** (the platform-state discontinuity), **D-15**, **D-17**, open question 1 |

## Why this run exists

ADR-0041 **D-18** records that the root filesystem was repaired on 2026-08-04,
after every target figure D-1…D-17 had been measured on the unrepaired one. D-18
deliberately does not claim the repair did or did not affect those figures —
nobody had measured it.

This run measures it. Same instrument, same commit, same parameters, same store
location, same device; one variable changed.

## The answer: nothing moved

`append`, median of 3 repetitions, against D-17:

| durability | b=1 post | b=1 D-17 | ratio | b=64 post | b=64 D-17 | ratio |
|---|---:|---:|---:|---:|---:|---:|
| FULL | 3 270 | 3 246 | 1.007 | 31 778 | 31 665 | 1.004 |
| NORMAL | 9 936 | 9 924 | 1.001 | 36 405 | 36 870 | 0.987 |
| OFF | 15 077 | 15 089 | 0.999 | 56 403 | 56 406 | 1.000 |

Every cell within **1.3 %**, five of six within **0.7 %**.

The latency shape reproduces as well — NORMAL/FULL at batch=64:

| | p50 | p99 | max |
|---|---:|---:|---:|
| post-repair | 0.64× | 1.30× | 1.42× |
| D-17 target | 0.65× | 1.32× | 1.40× |

**Counting unit** is events/second. **Independence unit** is one machine-day at
one filesystem state — three repetitions inside a run are repetitions, not
independent observations. **Held fixed:** instrument (digest-gated), commit,
parameters, seed, store location, device. **Changed:** the filesystem, repaired.
**The claim this supports:** the repair did not move the measured figures, so
D-1…D-17 and post-repair runs are comparable. It supports no claim that the
allocation errors were harmless in general — only that they are not visible in
these figures at this precision.

## `NORMAL` at batch=64: D-15 is now one against two

`stall`, 20 repetitions:

| batch=64 | D-15 | D-17 | post-repair |
|---|---:|---:|---:|
| FULL | 30 545 | 31 083 | **30 697** |
| NORMAL | 19 881 | 35 924 | **35 992** |
| OFF | 54 636 | 55 267 | **55 834** |

Post-repair `NORMAL` lands **0.2 %** from D-17. Two independent observations now
agree near 36 000 against D-15's 19 881.

**This does not explain D-15**, and OQ1's residual stays open exactly as ADR-0041
words it. What changes is the weight: a reading that was one of two competing
eras is now one anomalous observation against two that agree.

## The dirty-page mechanism is refuted a second time

`NORMAL` peak dirty/writeback across the three runs: **4 888 → 4 572 → 5 224 kB**.
The post-repair run carries the *highest* dirty load of the three while its
throughput matches the *fast* era.

D-17 refuted the mechanism by holding dirty essentially constant while throughput
rose 81 %. This refutes it from the opposite direction. A hypothesis I proposed
while investigating OQ1 is now wrong under two independent tests, and is recorded
rather than dropped.

## What does NOT fit, recorded rather than smoothed

`FULL` recorded **4 stalls in 20 repetitions** (D-17: 1), with durations

```
1942.5, 5926.0, 15077.1, 30091.8 ms
```

The 30 091.8 ms stall is the familiar signature — `nvme_core.io_timeout` is 30,
so it is the timeout plus handler latency, exactly as D-15 characterises.

**But `DMESG_NVME.txt` carries only ONE `completion polled` event** for this run
(`[2400.400831] nvme nvme0: I/O 13 QID 5 timeout, completion polled`). Three of
the four stalls have no corresponding NVMe timeout. The nvme lines run unbroken
from boot (7.7 s) to 2 400 s, so this is not a truncated ring buffer.

D-15's mechanism rested on **5 stalls coinciding with 5 timeouts**. Here the
correspondence is **1 of 4**. Either there is a second stall population D-15 did
not separate, or the sub-30 s stalls have a different cause. This evidence cannot
resolve it and no resolution is asserted.

Median throughput is unchanged despite four times the stalls, which is itself
D-15's point — stalls are a device property, not a persistence property — now
observed on a repaired filesystem.

## Confounders, stated

- **journald was growing during this run**, reading 227.0 M against its 200 M cap
  (194 M before). It was also growing during OQ1, newly enabled. More comparable
  than expected, not less — but it is background write load on the same device in
  both runs.
- **The NVMe defect was live**, which is what makes the comparison like-for-like.
  A quieter device would have confounded any improvement; there was none to
  confound.
- **Free space differs** by the ~0.96 GiB the repair reclaimed, plus the
  benchmark databases this run created (`df` read 107 G used / 34 G available,
  against 106 G / 35 G before).
- **Stand-in schema**, as with every WM-2 measurement.
- **One machine-day.** Three repetitions inside a run do not make three
  independent observations of the platform.

## Files

| File | What it is |
|---|---|
| `postrepair-append.jsonl` | `append` results — 9 run + 18 append records, 3 reps × 3 durabilities × 2 batch sizes, with full timing distributions |
| `postrepair-stall.jsonl` | `stall` results — 3 run + 3 stall records, 20 repetitions each at batch=64 |
| `probe.jsonl` | The instrument-identity probe that gated the run. Its `source_digest` is why the rest is comparable |
| `ENVIRONMENT.txt` | Filesystem state (`clean`, checked 20:43), free blocks, journald usage, `df` |
| `DMESG_NVME.txt` | Kernel NVMe lines, including the single `completion polled` timeout |
| `RUN_PARAMETERS.txt` | Exact commands, the identity gate, and the transfer verification |
| `GIT_COMMIT` | Commit the binary was built from — the device's checkout, identical to OQ1's |
| `.gitattributes` | `* -text`, so a checkout returns the bytes that were measured |
| `SHA256SUMS` | Covers every other file in this directory. It cannot cover itself |

Transfer was verified at two layers: the outer tarball digest matched on arrival
and the inner manifest then verified every file. The figures above come from the
verified copy, not from terminal scrollback.
