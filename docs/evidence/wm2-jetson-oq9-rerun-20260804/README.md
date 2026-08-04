# WM-2 — OQ9 stall re-run on target, with the windowed instrument

| | |
|---|---|
| **Captured** | 2026-08-04, Jetson Orin NX (`yahboom`), aarch64, L4T 5.15.148-tegra |
| **Evidence status** | `JETSON-TARGET-MEASURED` — citable against ADR-0041 |
| **Harness commit** | `a27fdbf8` (the fixed windowed instrument, PR #1332) |
| **Stand-in schema** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **Supersedes** | the attribution column of D-10 only |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this run answers

ADR-0041 open question 9 — the multi-second write stall — was classified in
D-10 as "intermittent, block-device/environment-correlated, **mechanism partly
unresolved**". This run identifies the mechanism.

**The stalls are lost NVMe completion interrupts, bounded by the driver's 30 s
`io_timeout`.** They are a platform/driver defect, not a property of SQLite, the
schema, or the `synchronous` setting.

## Files

| File | What it is |
|---|---|
| `stall.jsonl` | 12 records — one `run` header and one `stall` result per configuration |
| `DMESG_NVME.txt` | kernel ring buffer, filtered to `nvme` / `timeout` / `i/o error` |
| `ENVIRONMENT.txt` | `io_timeout`, `dirty_expire_centisecs`, drive identity, filesystem state, kernel |
| `RUN_PARAMETERS.txt` | the exact invocation parameters |
| `GIT_COMMIT` / `GIT_STATUS` | harness provenance — the tree was clean apart from this bundle |
| `SHA256SUMS` | covers every other file here, including this README and `.gitattributes` |

Verify with `sha256sum -c SHA256SUMS` from inside this directory.

`SHA256SUMS` cannot list itself — a file cannot contain its own hash — so it is
the one file the check does not cover. Everything else in the bundle is, and
`sha256sum -c` reports one `OK` per listed file, so a missing line is visible as
a shorter output rather than a silent gap. If the integrity of `SHA256SUMS`
itself matters for a given use, the git object hash of the commit that added it
is the anchor.

## The measurements

Six configurations, 20 repetitions each, 100 000 events — **D-10's protocol
exactly**, so the two are comparable. The only change is how the system counters
are windowed.

| Config | Stalls | Worst commit | Median | Window | Samples | Usable | Attribution |
|---|---:|---:|---:|---:|---:|:--:|---|
| `FULL`/b1 | **1/20** | **30 019.4 ms** | 3 086 ev/s | 30 055.2 ms | 1 412 | yes | `UNATTRIBUTED` |
| `FULL`/b64 | 0/20 | 29.9 ms | 30 545 ev/s | 64.1 ms | 4 | no | `NO-STALL` |
| `NORMAL`/b1 | **4/20** | **30 182.4 ms** | 9 485 ev/s | 30 198.0 ms | 1 418 | yes | `UNATTRIBUTED` |
| `NORMAL`/b64 | 0/20 | 64.2 ms | 19 881 ev/s | 107.2 ms | 6 | no | `NO-STALL` |
| `OFF`/b1 | 0/20 | 29.9 ms | 14 901 ev/s | 43.0 ms | 3 | yes | `NO-STALL` |
| `OFF`/b64 | 0/20 | 4.3 ms | 54 636 ev/s | 23.5 ms | 2 | no | `NO-STALL` |

**5 stalls in 120 repetitions (4.2 %).** PSI unavailable on this kernel
(`psi_io_stall_us` is `None` throughout), so one of the two I/O signals is
absent regardless of windowing.

## The mechanism

### 1. The duration is a constant, not a workload property

`/sys/module/nvme_core/parameters/io_timeout` = **30**.

| Stall | Excess over 30 000 ms |
|---|---:|
| `FULL`/b1 | **+19.4 ms** |
| `NORMAL`/b1 | **+182.4 ms** |

A timeout-bounded wait cannot finish under the timeout and exceeds it only by
handler latency. Both do exactly that.

### 2. The device was idle while the host waited

The two stalls are the first in this project measured over **their own window**
rather than the whole repetition — 1 412 and 1 418 samples at 20 ms, spanning
30 055 ms and 30 198 ms against stalls of 30 019 ms and 30 182 ms.

| Config | Device busy in-window | Fraction of the stall |
|---|---:|---:|
| `FULL`/b1 | 636 ms | **2.12 %** |
| `NORMAL`/b1 | 316 ms | **1.05 %** |

The contrast is the point. On the non-stalling rows the same counter reads
**107–214 %** of the window (`/proc/diskstats` field 13 is per-I/O busy time and
sums across queues, so a saturated multi-queue device exceeds wall time). During
ordinary operation this drive is saturated; during the stalls it did essentially
nothing.

Writeback is excluded too — peak dirty+writeback of 1 220 kB and 4 216 kB
against the 262 144 kB backlog threshold (0.5 % and 1.6 %). Thermal is excluded
at 58.0–58.2 °C against an 85 °C threshold.

**`UNATTRIBUTED` here is not the instrument shrugging.** It is positive evidence
against the device-busy, writeback-backlog and thermal hypotheses: everything
the harness can observe says nothing was happening.

### 3. The kernel names the cause

Five timeouts in `DMESG_NVME.txt`, against five stalls:

```
[Tue Aug  4 09:52:54 2026] nvme nvme0: I/O 719 QID 6 timeout, completion polled
[Tue Aug  4 10:38:47 2026] nvme nvme0: I/O 510 QID 6 timeout, completion polled
[Tue Aug  4 10:42:05 2026] nvme nvme0: I/O 817 QID 5 timeout, completion polled
[Tue Aug  4 10:43:03 2026] nvme nvme0: I/O 690 QID 6 timeout, completion polled
[Tue Aug  4 10:43:35 2026] nvme nvme0: I/O  56 QID 5 timeout, completion polled
```

The count matches, and so does the grouping: one event early (`FULL`/b1, which
recorded 1 stall) and four clustered within five minutes (`NORMAL`/b1, which
recorded 4).

**"completion polled" is the finding.** The timeout handler fired at 30 s,
polled the completion queue, and found the command *already complete*. The
device had done the work; the completion interrupt never reached the host, so
the host waited out the full timeout for a result already sitting in the queue.

**Zero resets, zero I/O errors, zero aborts** in the whole log. No command ever
failed.

## What this establishes, and what it does not

**Established by measurement:**

- Five stalls coincide one-for-one with five NVMe command timeouts.
- Each timeout resolved by *polling*, so the command had completed — no data was
  lost or retried. **Durability is unaffected**; this is a latency and
  availability defect. Tier C's five power cuts (D-11) stand independently.
- Stall duration is set by `io_timeout`, not by event volume, batch size or
  `synchronous` mode.
- The block layer, the page cache and thermal are all excluded as causes.

**Established, as a consequence of the kernel message rather than an
inference:** the completion was **not delivered by the normal interrupt path
within the timeout**. The handler only runs because the command was still
outstanding at 30 s, and it then found the completion present — so the device
had produced it and the host had not acted on it.

**Inferred, and NOT discriminated by this run:** whether the completion was
*lost* and recovered only by polling, or merely *delayed* and arriving near the
timeout. The 8 496 ms stall in the mitigation follow-up is consistent with
delayed delivery, and nothing here separates the two.

**Not established:** the *root* cause of the lost interrupt.

The controller was identified after this bundle was captured, so it is **not**
in `ENVIRONMENT.txt` — recorded here with the commands that produced it, run on
the same machine and the same boot:

```
$ sudo nvme id-ctrl /dev/nvme0 | grep -iE '^(vid|ssvid|mn|sn|fr|ieee)'
vid   : 0x10ec        ssvid : 0x10ec        ieee : 00e04c
mn    : SSD NVME 256GB    sn : JRD2025120012734    fr : VC400622

$ lspci -nn | grep -i non-volatile
0004:01:00.0 Non-Volatile memory controller [0108]:
    Realtek Semiconductor Co., Ltd. Device [10ec:5765] (rev 01)
```

**Realtek `10ec:5765`, an RTS5765-class DRAM-less controller.** `DMESG_NVME.txt`
corroborates the DRAM-less part — `nvme nvme0: allocated 64 MiB host memory
buffer` — since a controller without onboard DRAM keeps its mapping tables in
host RAM over HMB and therefore sustains materially more host-side DMA than a
DRAM-equipped drive.

That is a **lead, not a conclusion**. Candidates remain controller firmware, the
HMB path, PCIe ASPM power-state transitions, and MSI-X routing on the Tegra host
controller; nothing in this bundle discriminates between them.

**Deliberately not claimed — a rate law.** The events concentrate where more I/O
is issued: all five at batch=1 and none at batch=64, and `OFF` has now produced
**0 stalls in 80 repetitions** across this run and D-10. But five events cannot
support a rate model, and the simplest version of one fails: `NORMAL` fsyncs
*less* than `FULL` in WAL mode yet recorded four stalls against one.

## Against D-10

Five of six throughput medians reproduce within 3 %, which is what makes the
comparison like-for-like:

| Config | D-10 | This run | Δ |
|---|---:|---:|---:|
| `FULL`/b1 | 3 099 | 3 086 | −0.4 % |
| `FULL`/b64 | 31 497 | 30 545 | −3.0 % |
| `NORMAL`/b1 | 5 143 | **9 485** | **+84 %** |
| `NORMAL`/b64 | 19 351 | 19 881 | +2.7 % |
| `OFF`/b1 | 15 079 | 14 901 | −1.2 % |
| `OFF`/b64 | 55 275 | 54 636 | −1.2 % |

Two things moved and are **recorded rather than explained**:

1. **`NORMAL`/b1 median nearly doubled** (5 143 → 9 485 ev/s) while recording
   *more* stalls, not fewer — the wrong direction for a stall artefact, since a
   median is robust to them anyway. Unexplained.
2. **The stall distribution shifted.** D-10: 3/120, at `FULL`/b64 (2) and
   `NORMAL`/b1 (1). This run: 5/120, at `FULL`/b1 (1) and `NORMAL`/b1 (4). Both
   runs agree only that `OFF` never stalls.

D-10's `IO-DEVICE` attribution on `NORMAL`/b1 was withdrawn in PR #1332 because
the instrument that produced it compared whole-repetition counters against a
single-commit duration. This run supplies what that attribution was reaching
for, and it points the other way: the device was **not** busy.

## Platform caveat, recorded not swept

From `ENVIRONMENT.txt`, the filesystem holding the test database:

```
Filesystem state:    clean with errors
Errors behavior:     Continue
Maximum mount count: -1
Last checked:        2025-06-26
```

`Errors behavior: Continue` means ext4 will not remount read-only when it
detects corruption, and the volume has not been checked in 13 months and 50
mounts. **This does not invalidate the results here** — the failure is in the
NVMe driver, below the filesystem, and the mechanism is established by kernel
log correlation rather than by anything ext4 reports. It is recorded because a
durability evidence platform configured to continue past filesystem errors is a
gap that should be closed before the next evidence run.

## Consequences

**For ADR-0041.** OQ9 moves from "mechanism partly unresolved" to identified.
The stall is not a persistence-architecture property.

**It does not unblock open question 1**, and an earlier draft of this README
said it did. D-10 had already removed the stall as an OQ1 obstacle by supplying
stall-robust medians; what still blocks OQ1 is the batch=64 **inversion**, and
this run **reproduces it**:

| batch=64 median | D-10 | This run | Δ |
|---|---:|---:|---:|
| `OFF` | 55 275 | 54 636 | −1.2 % |
| `FULL` | 31 497 | 30 545 | −3.0 % |
| `NORMAL` | 19 351 | 19 881 | +2.7 % |

Same ordering, all three within 3 %: `OFF` > `FULL` > **`NORMAL`**. `NORMAL`
slower than `FULL` at the same batch size is not predicted by any durability
model, and it has now been observed twice on target from configurations with
**zero stalls** in both runs. That makes it a reproducible effect rather than
noise — which strengthens OQ1's blocker rather than removing it.

**For deployment.** At 10 Hz a 30 s stall is ~300 observations that cannot be
recorded, at roughly 4 % of batch=1 runs on this hardware. That belongs in
Assumptions of Use as a platform requirement — qualify the drive, or reduce
`io_timeout` so the recovery is faster — rather than in the persistence design.
