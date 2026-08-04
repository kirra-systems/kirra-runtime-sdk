# WM-2 — OQ1: the `NORMAL` < `FULL` inversion does not reproduce

| | |
|---|---|
| **Captured** | 2026-08-04, Jetson Orin NX (`yahboom`), aarch64, L4T 5.15.148-tegra |
| **Evidence status** | `JETSON-TARGET-MEASURED` — citable against ADR-0041 |
| **Harness commit** | `83998315` (#1340) |
| **Stand-in schema** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **Written up as** | ADR-0041 **D-17** |
| **Bears on** | ADR-0041 **open question 1** |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this run answers

ADR-0041 open question 1 records an anomaly: at batch=64 the medians ran
`OFF` > `FULL` > `NORMAL`, with `NORMAL` *slower* than `FULL`. No durability
model predicts that — `synchronous=NORMAL` does strictly fewer fsyncs than
`FULL` — and the ADR states that "until it is explained a per-source-class
policy should not be fixed at batch=64". Observed on host (D-10) and reported
as reproduced on target (D-15).

**It does not reproduce.** Two instruments, same machine, same session, same
parameters.

## The measurement

`stall`, 20 repetitions at batch=64 — the exact shape D-15 used:

| batch=64 | eps D-15 → now | worst commit | dirty/writeback | stalls |
|---|---|---|---|---|
| FULL | 30 545 → 31 083 (**1.02×**) | 15.23 → 9.11 ms | 892 → 1280 kB | 0 → 1 |
| NORMAL | **19 881 → 35 924 (1.81×)** | 58.88 → 12.28 ms | 4888 → 4572 kB | 0 → 0 |
| OFF | 54 636 → 55 267 (**1.01×**) | 3.85 → 3.77 ms | 42 104 → 44 964 kB | 0 → 0 |

`append`, three repetitions, an independent instrument, agrees: FULL 31 776,
NORMAL 36 916, OFF 56 439 — within 0.97–0.99× of the `stall` figures.

**FULL and OFF reproduce D-15 within 2 %. NORMAL is 81 % away.** Those two are
the internal controls that make the third interpretable: one setting moving
while its neighbours hold is not device variance.

Today's ordering is the conventional one: `OFF` > `NORMAL` > `FULL`.

## What it does NOT establish

**Why D-15's `NORMAL` was slow. This is unexplained and is recorded as such.**
The obvious candidates are ruled out or weakened:

- **Not the instrument.** `stall` and `append` agree here (0.97–0.99×) and on a
  host control at the same settings (0.96–1.02×) — including for `NORMAL`. A
  generic `stall`-command fault would have shown on the host too.
- **Not a healthier device.** The NVMe lost-completion defect was **live during
  this run**: `FULL` took a 30 183.9 ms stall and `DMESG_NVME.txt` carries three
  `completion polled` timeouts. If anything the device was worse for `FULL`
  today, and it still reproduced.
- **Not dirty-page pressure**, which was the mechanism proposed while
  investigating. `NORMAL`'s peak dirty/writeback is 4888 → 4572 kB — essentially
  unchanged — while its throughput rose 81 %. That column is a stable property
  of `synchronous=NORMAL` and does not track throughput. **The proposed
  mechanism is refuted by this evidence**, and is recorded rather than dropped
  because a discarded hypothesis is part of what the next investigator needs.

What did change is **commit latency**: `NORMAL`'s median worst commit fell
58.88 → 12.28 ms at unchanged dirty load. The cause is not identified here.

**One machine-day against another.** 20 repetitions per setting in both eras,
so the comparison is like-for-like, but this is a second observation and not a
distribution over many days.

**Stand-in schema**, as with every WM-2 measurement.

**Platform state.** The filesystem carried `clean with errors` with an
outstanding `e2fsck`, under `Errors behavior: Remount read-only`. Persistent
journald was enabled shortly before the run, adding modest background writes to
the same device.

## What reproduces, and is the useful result

The `append` instrument emits the full commit-latency distribution, which
`stall` does not. On **both** machines, `NORMAL` is faster at the median and
worse in the tail:

| NORMAL / FULL, batch=64 | p50 | p99 | max |
|---|---:|---:|---:|
| target | 0.65× | 1.32× | 1.40× |
| host (indicative) | 0.47× | 1.51× | 4.87× |

That is the durable, reproducible property of `synchronous=NORMAL` in WAL mode:
**it buys median throughput by paying tail latency.** For a store whose
consumers care about worst-case rather than average behaviour, that is the
trade a per-source-class policy should turn on — and it is a decision that can
now be taken on evidence rather than blocked on an anomaly.

## Files

| File | What it is |
|---|---|
| `oq1-target.jsonl` | `append` results, 3 repetitions × 3 durabilities × 2 batch sizes, with full timing distributions |
| `oq1-stall-target.jsonl` | `stall` results, 20 repetitions × 3 durabilities at batch=64 — the D-15 comparison |
| `ENVIRONMENT.txt` | Kernel, filesystem state, journald usage, harness commit |
| `DMESG_NVME.txt` | Kernel NVMe lines, including the three `completion polled` timeouts during these runs |
| `RUN_PARAMETERS.txt` | The exact commands, and the `--durability` flag caveat |
| `GIT_COMMIT` | Commit the binary was built from |
| `.gitattributes` | `* -text`, so a checkout returns the bytes that were measured |
| `SHA256SUMS` | Covers every other file in this directory. It cannot cover itself |

Both `.jsonl` files were transferred base64-over-terminal and verified by
checksum on arrival. `oq1-stall-target.jsonl` **failed** its first transfer —
same byte count, different digest — and was re-sent; the figures above come
from the verified copy, not from terminal scrollback.
