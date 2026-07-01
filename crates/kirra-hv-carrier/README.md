# kirra-hv-carrier — L3 cross-partition contract carrier (ADR-0030)

Binds the carrier-agnostic `ContractReader` / `ContractWriter` seam
(`kirra-contract-channel`) to a **memory-mapped shared region**, so the frozen
`GovernorContractView` + seqlock trust chain runs across a **real** address-space
boundary — not just host-in-process over `InProcessRegion`.

This is **the only crate in the L3 path that is NOT `#![forbid(unsafe_code)]`** —
the ADR-0006 **Clause 3** integration boundary. The `unsafe` budget is bounded
and enumerated (ADR-0030 Clause A): the `shm_open`/`mmap` map, the atomic field
accesses over the mapped bytes (reproducing `InProcessRegion`'s memory model
exactly — `generation` Acquire/Release, body Relaxed), and the `Send`/`Sync`
assertions. The `#[repr(C)]` overlay is byte-identical to the frozen
`GovernorContractView` (compile-time asserted against its frozen offsets).

## Bindings

- **`PosixShmRegion`** — the host **guest** handle (read-write; `create` /
  `open`). Implements `ContractReader` + `ContractWriter`.
- **`PosixShmReader`** — the host **governor** handle (read-only; `open`). The
  R-HV-1 shape: no `ContractWriter` impl (type-level) **and** a `PROT_READ`
  mapping (OS-level), so the governor cannot write the region.

Both are the testable stand-in for the QNX `HvRegion` (a hypervisor-mapped
region), which binds the *same* traits with the *same* trust chain and only swaps
the map primitive (ADR-0030 Clause B).

## Run

```sh
cargo test -p kirra-hv-carrier          # unit + two-process integration test
cargo run --release -p kirra-hv-carrier --bin shm_latency   # raw-SHM latency
```

The two-process test (`tests/two_process.rs`) has a guest process publish into a
POSIX shared region and a **separate** governor process (`shm_peer`) map it
read-only and validate+decode — the real cross-address-space proof the
in-process reference carrier cannot give.

## Latency — raw SHM is at the floor (host-INDICATIVE)

`shm_latency` times the governor read path over `MAP_SHARED` (176-byte
`GovernorContractView`, same nearest-rank percentiles as the iceoryx2
`latency_bench`). Representative host run (shared dev box, no core isolation /
FIFO — the comparative **ratio** is the takeaway, not the absolute ns):

```
path                            p50_ns    p99_ns   p999_ns
shm read_coherent_snapshot          60       187      2569
shm read+validate+decode           297       485      3797
```

Compared to the `latency_bench` rows on the same class of host:

| path | p50 |
|---|---|
| in-process floor (by-value) | ~28 ns |
| **shm read_coherent_snapshot** | **~60 ns** |
| iceoryx2 zero-copy (pub/sub) | ~309 ns |
| socket+serde (UDP proxy) | ~976 ns |

The raw-SHM seqlock read sits at the **in-process-floor class — ~5× tighter than
iceoryx2's pub/sub handoff** — because for a single latest-value slot it is the
minimal mechanism: no queue, no loan, no discovery. (And note the ~60 ns is
transport-only; the ~297 ns row already *includes* CRC + validate + decode, which
iceoryx2's 309 ns does not.) This is the measured evidence behind ADR-0006's
choice of a frozen layout over SHM — **not** iceoryx2 — for the Clause 2
cross-partition boundary. The ms-scale `max` (not shown) is host jitter; the
certified figure is a QNX-target-under-FIFO measurement (#274).
