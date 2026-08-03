# WM-2 persistence harness

The measurement instrument for
[ADR-0041](../../docs/adr/0041-world-model-persistence-architecture.md), which
proposes a SQLite append-only event log with materialized projections for Kirra
World and then refuses to ratify itself on argument — acceptance is
**measurement-gated on target hardware**.

Operator runbook:
[`docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md`](../../docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md).

```sh
cargo build --release
./target/release/wm2-persistence-harness platform     # would a run here be citable?
./target/release/wm2-persistence-harness all --out results.jsonl
cargo test                                            # unit + integration; no target needed
```

## This is not the Kirra World store

Copying code from here into `kirra-world-store` is a defect, not a shortcut.
Three things are deliberately wrong for production and right for a benchmark:

- **The schema is a stand-in.** ADR-0041 states that column-level schemas are
  deliberately not fixed, so anything concrete enough to measure had to be
  invented. Every result record carries the stand-in's SHA-256, so when the real
  schema lands its digest differs and an old measurement becomes visibly about
  something else rather than quietly authoritative.
- **The hash chain is a local SHA-256 with a harness-only domain tag**, not
  `kirra-audit-hash`. Those encoders *are* the production on-disk format; a
  harness emitting production-shaped bytes is one `cp -r` from becoming the
  store. The substitution's cost is measured and reported (`hash_share_percent`)
  so it can be shown not to move the decision.
- **It is workspace-detached.** Own lockfile, own target dir, never built by the
  root `cargo test --workspace`. Nothing can path-depend on it.

## One dependency, and that is the point

`rusqlite` (bundled), and nothing else.
`ci/check_kirra_world_bidirectional_fence.py` walks this manifest as an extra
Fence A root (`FENCE_A_EXTRA_PACKAGES`), so a `serialport` or `r2r` added here
reds CI exactly as one added to `kirra-world` would. A benchmark grows a
transport crate "just to publish results" far more easily than a domain crate
does, which is why the fence covers it despite the name being outside the
`kirra-world*` namespace.

## Two honesty mechanisms

**Citability.** Every record is stamped `JETSON-TARGET-MEASURED` or
`HOST-INDICATIVE-NOT-TARGET`, following the `TBD-QNX-TARGET` convention in
`tools/qnx-rtm-harness`. The first requires both machine corroboration and an
explicit `--assert-target`; neither alone. A `tmpfs` path forfeits target status
outright, because a run that never fsyncs produces the best numbers the harness
can emit while measuring none of the property being decided.

**Tier C.** The crash experiment has three tiers. `SIGKILL` crash-consistency
and WAL-loss prefix validity are automated. The actual power cut is not, because
nothing in software distinguishes an honest `fsync` from a device cache that
acknowledged and buffered it. That tier always reports `NOT-RUN` with the reason
attached, so a results file can never imply a durability test that did not
happen.

## Layout

| File | What it is |
|---|---|
| `platform.rs` | Which runs may be cited. Pure classifier + `/proc/self/mountinfo` parsing |
| `standin.rs` | The stand-in schema, hash chain, deterministic fold, migrations |
| `gen.rs` | Seeded synthetic load (splitmix64, dependency-free) |
| `bench.rs` | Append, replay, the four §12 query families, growth, migration |
| `crash.rs` | Corruption / restart tiers A, B and the C refusal |
| `json.rs`, `sha256.rs` | Minimal writer and the local hash — see above |
| `tests/crash_tier_a.rs` | Tier A end-to-end against the real binary (a unit test would exercise the spawn-failure path forever and look like coverage) |
