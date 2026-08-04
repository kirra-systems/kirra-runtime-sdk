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
acknowledged and buffered it.

The harness cannot perform that tier, but it does *record* it. `powercut arm`
fsyncs a known prefix and writes a durable marker stating what must survive;
`powercut verify`, run after the reboot, checks what actually did and appends
the verdict to a ledger beside the database. The judgement is pure and unit
tested, so the distinction it turns on is not left to the operator: losing the
un-fsynced tail is **correct**, while coming back with fewer events than were
fsynced is a device that acknowledged writes it had not persisted — the one
failure this tier can detect that A and B cannot.

With no trials recorded, tier C reports `NOT-RUN` with the reason attached, so
a results file can never imply a durability test that did not happen. Below the
required five trials the aggregate is `INCONCLUSIVE`, not a pass: device-cache
loss is probabilistic, so one survived cut is not evidence.

**A trial is an arming, not a ledger row.** The marker is single-use: `arm`
refuses to run over an unused one, continues from `MAX(generation) + 1` rather
than restarting at 0, and `verify` rejects both a repeated trial number and a
repeated arm id before writing anything. The verdict is appended and fsynced
*before* the marker is removed, so a cut in between leaves the arming
outstanding — recoverable — instead of losing the verdict. The aggregate counts
**distinct arm ids**, so three rows carrying one id are three verifications of
one power cut and count as one.

This was not true of earlier builds, and the difference is not academic. `arm`
restarted at generation 0, so on a populated store the second arming died on a
primary-key collision while the first marker survived, and each later `verify`
re-read the same surviving store and appended another `PASS`. **The R2 attempt
recorded three `PASS` rows from one real power cut; its honest status is 1 valid
independent PASS of the 5 required.** Ledgers from those builds carry no arm id,
and the aggregate now refuses them as unattributable rather than counting them —
restart tier C on a fresh database.

**Migration cost is a property of the SQL, and the harness gates that.** The
`migrate` command's backfill is selectable with `--migration-sql`:

| | Statement | Cost | Why it exists |
|---|---|---|---|
| `legacy` (default) | correlated scalar subquery | **O(events × entities)** | what ADR-0041 D-6 measured on target; kept so that result stays reproducible |
| `grouped` | `UPDATE … FROM` a materialized `GROUP BY` | O(events + entities) | what a migration should look like |

Both emit a `migration_sql` field, so a record can never be mistaken for the
other. They are hundreds of times apart and the gap **widens with entity count** —
at 30 000 events the grouped form is ~750× faster at 2 000 entities, because the
legacy plan rescans every observation event once per projection row.

Four tests in `standin.rs` keep that from coming back through a later "clearer"
rewrite, which is exactly how it would return — the SQL reads fine and the answer
is correct, only the plan is wrong:

- the grouped backfill's `EXPLAIN QUERY PLAN` must contain no correlated
  subquery (the gate, deterministic);
- the legacy one must still read as correlated (non-vacuity — otherwise the gate
  could pass while detecting nothing);
- both statements must compute identical counts, *including* for an entity the
  log never mentions, which the two forms reach differently;
- the grouped form must stay flat as entity count grows while the legacy form
  does not (a coarse shape check, both measured in one process so machine speed
  cancels).

## Layout

| File | What it is |
|---|---|
| `platform.rs` | Which runs may be cited. Pure classifier + `/proc/self/mountinfo` parsing |
| `standin.rs` | The stand-in schema, hash chain, deterministic fold, migrations |
| `gen.rs` | Seeded synthetic load (splitmix64, dependency-free) |
| `bench.rs` | Append, replay, the four §12 query families, growth, migration |
| `crash.rs` | Corruption / restart tiers A, B, and tier C's arm/verify/ledger machinery |
| `pressure.rs` | Disk-full behaviour, and what `VACUUM` costs the system while it runs |
| `sweep.rs` | The scale ladder, with a computed fail-closed verdict against ADR-0041's own reopening condition |
| `stall.rs` | The ~29 s write-stall investigation: repetition, a stall rate, and system counters sampled across each run |
| `json.rs`, `sha256.rs` | Minimal writer and the local hash — see above |
| `build.rs` | Build identity: rustc version, git commit (`-dirty` when it is), source digest |
| `tests/crash_tier_a.rs` | Tier A end-to-end against the real binary (a unit test would exercise the spawn-failure path forever and look like coverage) |
