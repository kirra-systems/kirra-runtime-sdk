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
iceoryx2 = "=0.9.2"
```

> The host spike pins **0.9.2**. It was authored against `=0.9.1`, but that pin no
> longer builds on a fresh resolve: the `iceoryx2 0.9.1` umbrella crate pulls its
> `iceoryx2-*` sub-crates at `0.9.2` (newer patch in their `^` range), and `0.9.1`
> main + `0.9.2` subs do not compose (`ZeroCopySendError::NoConnectedReceiver`
> missing). Bumping the umbrella to `=0.9.2` realigns the whole set; the spike code
> compiles and runs unchanged. (`Cargo.lock` is gitignored here, so the pin is the
> only control.) Host Linux is iceoryx2 tier-1; **#274 must re-confirm the feature
> subset against whatever version targets QNX 8.0** — the methodology transfers
> even if exact flags shift between versions.

## Frozen-contract carrier (#275 / L2) — `src/frozen.rs`

The original spike (`wire.rs`/`judge.rs`) proved iceoryx2's feature subset over an
**ad-hoc** `CommandFrame`. `frozen.rs` **promotes** that to the **production
frozen contract**: it carries `kirra_contract_channel::GovernorContractView` (the
176-byte `#[repr(C)]`, by-value, freeze-pinned image) over the same real iceoryx2
zero-copy channel and validates every received owned sample with the **production**
`kirra_contract_channel::validate()` — the same transport-contract checks the QNX
harness and the hypervisor-SHM seam use, not a spike-local copy.

- A `#[repr(transparent)] WireView(GovernorContractView)` newtype carries the one
  audited `unsafe impl ZeroCopySend` (the orphan rule forbids impl-ing the foreign
  trait on the foreign view directly; `transparent` keeps the wire bytes identical
  to the frozen image). This is the ADR-0006 Clause 3 integration `unsafe`.
- `tests/frozen_fault_matrix.rs` drives all nine transport-contract fault classes
  (valid, layout-version, magic, oversize, CRC, sequence-regress, replay,
  generation-regress, deadline) through the live channel and asserts each maps to
  exactly its `validate()` `ContractFault` — over the **full** and **minimal**
  (`--no-default-features`) iceoryx2 configs.
- **Isolation holds (#275 gate).** The dependency direction is **spike →
  `kirra-contract-channel`** (a path dep on the lean, no_std, zero-dep,
  forbid-unsafe core), never the reverse — so iceoryx2 still **never** enters the
  SDK/parko dependency tree. This is the iceoryx2 path *using* the production
  contract, not an SDK adoption of iceoryx2 (that remains the #275 decision).
- **Scope:** `validate()` is the *transport-contract* checker; the kinematic
  envelope is a separate downstream governor step (the talisman
  `VehicleKinematicsContract`) and is intentionally out of scope here.

### Latency — why iceoryx2 for the doer→checker hop (`src/bin/latency_bench.rs`)

The QM-planner (doer) ↔ ASIL-governor (checker) **partition boundary is a
mandatory safety boundary** (freedom-from-interference) — you cannot delete it for
latency. The question is the *lowest-latency way to cross it*. `latency_bench`
times the 176-byte frozen `GovernorContractView` handoff three ways
(`KIRRA_LAT_ITERS=100000 cargo run --release --bin latency_bench`):

```
transport                  p50_ns     p99_ns    p999_ns     max_ns   p50 vs floor
in-process (floor)             28         37         60      69069     (floor)
socket+serde (proxy)          976       1901      13934      83820     34.9x
iceoryx2 (zero-copy)          309        424        995      46083     11.0x
```

INDICATIVE (shared host, no core isolation / FIFO) — the comparative **ratio** is
the takeaway. iceoryx2 is ~3× faster than a bare UDP+serialize hop at the median
and ~14× tighter at **p99.9 (995 ns vs 13.9 µs)** — and `socket+serde` is a
*conservative* proxy: a real DDS hop adds RTPS/discovery/typed-CDR overhead (tens
of µs–ms → 100–1000×; see the RMW refs in the #275 scope). The in-process **28 ns**
floor is what the mandatory isolation costs; iceoryx2 holds the crossing
**sub-microsecond**, where sockets/DDS do not. Certified numbers are QNX-target
under FIFO (#274); the deployment lowest-latency mode pairs iceoryx2 with
**busy-wait polling on an isolated core** → toward the published ~100 ns.

**Busy-wait / pinned-core ping-pong** (the deployment lowest-latency *mode*).
`latency_bench` also runs a two-thread ping-pong — a responder busy-polls
(`spin_loop`, no event wakeup) on one pinned core and echoes; the requester
busy-polls on another — i.e. the *real* cross-core round-trip a doer↔checker hop
pays (`KIRRA_LAT_FIFO=1 KIRRA_LAT_REQ_CPU=2 KIRRA_LAT_RESP_CPU=3 cargo run
--release --bin latency_bench`). Host-indicative result on this shared,
**non-isolated, virtualized** sandbox:

```
mode                       p50_ns     p99_ns    p999_ns     max_ns
iox2 ping-pong (RTT)        ~1400      ~1900      ~15000      ~90000
```

Two honest reads: (1) the **~1.4 µs RTT (~700 ns one-way)** is the *true*
cross-core handoff — the single-thread row above (347 ns) understated it because
publish+receive in one thread never actually waits across a boundary. (2) The
**tail (~15 µs p99.9) is host/hypervisor jitter, not iceoryx2**: these cores are
not `isolcpus`-isolated, so pinning removes migration but not preemption — and
`SCHED_FIFO` here *worsened* the tail (a busy-spinning FIFO thread fights the
system scheduler on a shared core). The published ~100 ns floor needs an
**isolated core on a low-jitter target**, which is precisely the QNX-under-FIFO
measurement (#274) — the mechanism is in place; the target supplies the number.

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
