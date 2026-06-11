# iceoryx2 host-side feature-subset + fault-matrix spike (#273)

Part of EPIC #270 (iceoryx2 transport adoption for the QNX governor lane).
**This is a spike, not an adoption** — adoption is the #275 ADR's decision. It
produces the evidence that informs **#274** (QNX 8.0 feature-subset check) and
**#275** (the transport ADR).

## Isolation (hard rule)

A **standalone crate** with its own `[workspace]` table (→ its own `Cargo.lock`).
It is **NOT** a member of the `kirra-runtime-sdk` / `parko` workspace, and
**iceoryx2 never enters any existing crate's dependency tree.** The certified
kinematic envelope lives in the untouched talisman
`src/gateway/kinematics_contract.rs`; this spike imports **nothing** from it and
labels its own envelope numbers as **PROXIES**.

## Pinned version

```
iceoryx2 = "=0.9.1"
```

> The host spike pins **0.9.1** (latest at authoring; host Linux is iceoryx2
> tier-1). The EPIC notes QNX 7.1 landed as tier-3 in v0.7.0. **#274 must
> re-confirm the feature subset against whatever version targets QNX 8.0** — the
> methodology transfers even if exact flags shift between versions.

## How to run

```sh
cd tools/iceoryx2-spike

# FULL config (iceoryx2 default features: std + console)
cargo test                          # asserted fault matrix over real iceoryx2
cargo run                           # print the matrix + per-class timing

# MINIMAL config (iceoryx2 --no-default-features → NO features; QNX-8.0 mirror)
cargo test --no-default-features
cargo run  --no-default-features

# Production-shape demo: subscriber + publisher as two OS processes
cargo run -- --two-process
```

The asserted matrix (`cargo test`) runs publisher + subscriber in one process
over the `ipc` service — **genuine zero-copy shared memory**, just deterministic
for assertion. The `--two-process` mode exercises the *same* channel across two
OS processes (the production shape).

## THE FEATURE-SUBSET RESULT (the deliverable for #274)

| Config | iceoryx2 features | Builds | Runs | Matrix |
|--------|-------------------|--------|------|--------|
| **full** (default) | `std`, `console` | ✅ | ✅ | PASS |
| **minimal** (`--no-default-features`) | **(none)** | ✅ | ✅ | PASS |

**The minimal feature subset for KIRRA's governor edge (pub/sub + subscriber
lifecycle + judge) is EMPTY.** On host, iceoryx2 0.9.1 compiles **and** runs the
zero-copy publish/subscribe path with `default-features = false` and nothing
added back — both `std` and `console` are droppable.

* `console` = iceoryx2-bb-loggers console output — pure convenience, droppable.
* `std` = std support across the `iceoryx2-bb-*` crates. **Not required** for the
  pub/sub + subscriber-lifecycle API this spike uses on host. (The spike's own
  binary still links `std`; the QNX-8.0 `--no-default-features` constraint is
  about *iceoryx2's* `std` feature, not the application's.)
* No feature had to be re-added. **Input to #274:** start from
  `--no-default-features` on the QNX 8.0 target and verify this empty set
  survives the 8.0 build + runtime (the host cannot prove the *target* runtime —
  that is #274's job).

> Note on `cargo build --no-default-features`: the CLI flag gates **this crate's**
> features, not the dependency's. The two configs are therefore driven through
> this crate's `[features]` (`ix-full = ["iceoryx2/std", "iceoryx2/console"]`,
> default-off), with iceoryx2 declared `default-features = false`.

## Fault matrix (verdict-correctness gate)

Eight harness fault classes + an `IntegrityFlag` class, each injected
publisher-side, detected subscriber-side, verdict asserted. **PASS gate =
verdict correctness only.** Both feature configs are all-PASS.

```
fault class            samples     ok    p50(us)    p99(us)    max(us)
----------------------------------------------------------------------
Valid                     2000   PASS      6.821     16.642     40.570
BadMagic/StaleHeader      2000   PASS      6.667     15.229     91.615
SequenceRegress           2000   PASS      6.671     21.574     99.067
Replay (equal seq)        2000   PASS      6.678     15.588     39.458
DeadlineMissed            2000   PASS      6.671     21.596     54.754
IntegrityFlag             2000   PASS      6.655     16.230     43.433
PayloadCorrupt (CRC)      2000   PASS      6.666     15.563     55.823
PayloadOversize           2000   PASS      5.713     13.626     43.062
KinematicLimit            2000   PASS      6.801     15.319     56.222
```

(Above: full config. The minimal config produces the same all-PASS matrix; both
are reproduced by the commands above.)

### Subscriber-edge defense-in-depth → judge

Each received frame passes the subscriber **edge** (bounds/oversize, then payload
CRC) **before** the judge runs the contract checks (magic → sequence → deadline →
integrity flag → kinematic envelope).

### The Replay row (corrected `<=`)

The judge's sequence rule is **`sequence <= last_accepted ⇒ reject`** — an *equal*
sequence is a replay and rejects; only a *strictly newer* sequence passes. The
strict-`<` form was a known replay hole and is **not** reintroduced. Proven
red/green by `replay_equal_sequence_rejects` (equal → `Reject(Replay)`, newer →
`Accept`) and the `Replay (equal seq)` matrix row.

## TornHeader — ELIMINATED BY TRANSPORT (a finding, not a skipped test)

iceoryx2's managed sample lifecycle prevents a torn read **by construction**: the
publisher writes into an **exclusively-loaned** slot, and `send()` publishes an
**immutable** sample; the subscriber's `receive()` returns an **owned `Sample`**
over a stable slot that is **not recycled while held**. The application never
double-reads a live, concurrently-mutating buffer — so a torn header is not
reachable without `unsafe` raw-memory access, which this spike forbids.
**`transport-eliminates-X` is first-class ADR evidence for #275.** (A variable-
length `[u8]` service would additionally bound the received slice length; the
fixed `#[repr(C)]` frame here makes the application-level `declared_len` bound
the explicit oversize check.)

## FFI-elimination (the architectural point)

In the classic-iceoryx **C++** shim the judge was reached across an `extern "C"`
boundary — a raw pointer dereference inside `unsafe`. Over a **Rust iceoryx2**
subscriber edge the judge is an **ordinary function call** on a typed
`&CommandFrame`: **no `extern "C"`, no `unsafe`, no FFI on the command path**
(`grep -rn 'unsafe\|extern "C"\|no_mangle' src/` finds only doc-comments). The
governor hot path can be Rust end-to-end — coherent with the QNX + Ferrocene
certification stack. **This is the input to the #275 ADR** ("FFI demoted to the
documented C/C++ integration boundary").

## Honesty banner

The PASS gate is **verdict correctness**. The host timing above is **indicative
only** — warmup-primed p50/p99/max over the full
publish→zero-copy→validate→judge path on a shared dev host. **Certified WCET
comes from the QNX 8.0 target under FIFO scheduling (#274).** Host numbers are
never presented as WCET.

## Informs

* **#274** — QNX 8.0 `--no-default-features` build + feature-subset verification +
  target-measured FDIT/WCET. This spike's empty minimal feature list is its input.
* **#275** — the transport ADR (iceoryx2 over classic; Rust end-to-end; FFI
  demoted). This spike supplies the no-FFI demonstration and the
  transport-eliminates-TornHeader finding.
