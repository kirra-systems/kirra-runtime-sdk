# WM-2 — R2 alongside-rebuild cost on target

| | |
|---|---|
| **Captured** | 2026-08-04, Jetson Orin NX (`yahboom`), aarch64, L4T 5.15.148-tegra |
| **Evidence status** | `JETSON-TARGET-MEASURED` — citable against ADR-0041 |
| **Harness commit** | `5eec0f0d` (PR #1336, the `rebuild` command) |
| **Stand-in schema** | `630eb690aaef3df32690c39c283f6c3b30c60ac04264a84d95051cfaf29c3292` |
| **Written up as** | ADR-0041 **D-16a** |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

## What this run answers

ADR-0041's acceptance record carries one outstanding obligation: prototype R2
far enough to know what alongside-rebuild-and-swap costs *"in code, in peak
disk, in write amplification and in cutover latency"*. The protocol answered
**code**; the host sweep (D-16) measured the other three; this run repeats that
sweep on target at the **same parameters**, so the only variable is hardware.

It settles one of the three, shows a second was never a hardware question, and
establishes that the third **cannot be measured on this platform at all**.

## What it establishes

**Cutover holds.** 2.33–2.48 ms across a 16× range of fold-chunk counts, against
the host's 2.06–2.33 ms. The target's spread across that entire range (1.06×) is
*smaller than the host's run-to-run spread at a single configuration* (1.21×),
so the two platforms are not distinguishable on this measure. R2 rests on the
robot continuing to serve throughout, with the swap as the only blackout — that
now has a target number rather than an inference.

**The protocol ran clean on target.** All five configurations reached `Active`
with `completed` true: catch-up converged, equivalence was proven at a pinned
generation, and the cutover guard accepted only at a matching head. Nothing in
`docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md` needed target-specific
handling.

**No NVMe timeouts occurred during the sweep**, against D-15's five in 120
repetitions. This is therefore not a run in which the device's known
lost-completion defect was active.

## What it does NOT establish

**Write amplification is unmeasurable on this hardware.** `/proc/self/io` does
not exist — the Tegra kernel ships without `CONFIG_TASK_IO_ACCOUNTING` (there is
no `/proc/config.gz` either). Both the control and rebuild arms returned `None`,
and the harness recorded `None` rather than `0`, which is what
`rebuild_cost::process_write_bytes`'s contract demands: *a missing counter must
not arrive as zero, which would render as a flatteringly efficient rebuild*.

`ENVIRONMENT.txt` captures the `ls: cannot access '/proc/self/io'` line
deliberately. **That absence is the finding** — an evidence set showing only
`null` counters could not distinguish a missing kernel counter from a harness
that failed to read one.

The host's **2.8×–35.8× does not transfer** and must not be quoted as though it
does. This is the dimension carrying the flash-wear argument, so the R2
obligation stays open on the part that matters most — open **by ruling**, with
the declined alternatives recorded in D-16a.

A prediction was on record *in advance* that amplification would **move** on
target. It did not move; it proved unmeasurable. The prediction is therefore
**untested**, not confirmed or refuted.

**Peak disk and projection size are deterministic, not hardware measurements.**
They came back identical to every printed digit against the host — projection
fraction `2.58 / 2.61 / 2.59 / 2.54 / 2.57 %`, peak-overhead ratio
`0.0111 / 0.0127 / 0.0390 / 0.0251 / 0.0259` — because both are functions of the
seeded data and the schema. Host↔target agreement on them is a **reproducibility
check**, not evidence about the device, and neither may be entered against the
ratification checklist as a target result.

**Single sweep.** The target has no run-to-run spread of its own; the 1.21×
bound quoted above is the *host's*, and applying it to the target is an
assumption rather than a measurement. Repeating the sweep would close that.

**Stand-in schema**, as with every other WM-2 measurement: the constants
describe that schema, not a ratified one.

**Platform state.** The filesystem carried `clean with errors` with an
outstanding `e2fsck` (see `ENVIRONMENT.txt`), under `Errors behavior: Remount
read-only`. That configuration is fail-closed — a corrupt region would have
aborted the run rather than silently altering a number — but it is recorded
rather than omitted, because it is part of what the platform was when these
figures were taken.

## Files

| File | What it is |
|---|---|
| `wm2-r2-target.jsonl` | The raw result stream: one `run` record and one `rebuild` record per configuration, 10 lines |
| `ENVIRONMENT.txt` | Kernel, NVMe controller identity, filesystem state, the `/proc/self/io` absence, harness commit |
| `RUN_PARAMETERS.txt` | The exact command, and why the parameters are pinned rather than defaulted |
| `GIT_COMMIT` | Harness commit the binary was built from |
| `SHA256SUMS` | Checksums of every file above. It does not cover itself — verify with `sha256sum -c SHA256SUMS` |

## Reproducing

```bash
cd tools/wm2-persistence-harness
cargo build --release
for r in 2 4 8 16 32; do
  ./target/release/wm2-persistence-harness rebuild \
    --db /path/on/target/wm2-rebuild-$r.sqlite \
    --events 40000 --entities 2000 \
    --rebuild-rounds $r --rebuild-ingest $((16000/r)) \
    --assert-target --out results.jsonl
done
```

`--assert-target` is the operator's assertion about the physical setup; the
harness independently corroborates aarch64 and Tegra evidence and stamps
`JETSON-TARGET-MEASURED` only when both hold. A debug build is refused. On a
host, the same commands produce `HOST-INDICATIVE-NOT-TARGET` and are not
citable.
