# kirra-inline-governor — the in-line SHM enforcement path (EP-01 / G-1)

The governor verdict runs **in-line** between the planner's proposal and the
actuator's release, over shared memory. No HTTP anywhere on the enforced path.

```text
  writer (guest process)                 governor                    actuator
  ─────────────────────                  ────────                    ────────
  publish(proposal)  ──► SHM region ──►  GovernorStation.cycle       ActuatorStation.release
  (frozen contract,      (seqlock)       ├ coherent seqlock read     ├ token exists?
   kirra-contract-                       ├ transport validate        ├ Ed25519 verify over
   channel)                              │ (magic/bounds/CRC/seq/    │  EXACTLY these bytes
                                         │  generation/deadline)     ├ sequence strictly
                                         ├ decode (fail-closed)      │  advances (<= = replay)
                                         ├ kinematic bound           └ decode → RELEASE
                                         └ sign the ENFORCED bytes       (else: MRC hold)
                                           (view_to_sign → token)
```

## What is proven here (host)

- **The assembled loop refuses end to end** — the FDIT fault matrix
  (`src/lib.rs::fdit_matrix`, 12 rows) runs writer → region → governor →
  actuator over the deterministic in-process carrier: CRC corruption, bounds
  violation, torn (odd-generation) write, sequence replay and regress, expired
  deadline, forged token, token/view substitution, release replay at the
  actuator — every class ends in *no release*, with both watermarks clean
  (a rejected snapshot never burns its sequence; a refused release never
  advances the release watermark).
- **The clamp is what gets signed** — an over-envelope proposal releases the
  *clamped* bytes; the same token refuses the raw proposal (digest mismatch).
- **It crosses a real process boundary** — `tests/cross_process.rs` runs the
  same loop over `kirra-hv-carrier`'s POSIX-SHM mapping with the guest as a
  separate OS process, including post-publish corruption through the raw RW
  mapping and a wrong-governor-key refusal.
- **Latency is gated** — the root crate's WCET CI gate
  (`src/wcet_gate.rs::regression_inline_loop_full_step`) times one full
  `govern_and_release` step (read → validate → bound → sign → verify →
  release-decision) and asserts p99.9 under the 1 ms CI threshold in the
  release-build `wcet-gate` lane. Measured here: ~65 µs p50 / ~116 µs p99.9
  (host, release) — Ed25519 dominates. **Host-INDICATIVE, never a WCET claim.**

Run the demo (two processes, scripted faults, latency summary):

```sh
cargo run --release -p kirra-inline-governor --bin inline_demo
```

## What remains for the QNX target

This crate is the software half of G-1. The remaining step is running the
same loop on the QNX safety partition (EPIC #270 / ADR-0030 Clause B):

1. bind the frozen contract to the **hypervisor-mapped region** (`HvRegion`,
   swapping only the map primitive — the seqlock trust chain and every
   consumer above it are carrier-agnostic and unchanged);
2. re-measure under **SCHED_FIFO on the QNX target** — those numbers (not the
   host figures above) feed the FTTI claim, per
   `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`;
3. provision the governor signing key from the platform root of trust
   (`KIRRA_GOVERNOR_SIGNING_KEY_SOURCE`, ADR-0031 Clause E — the demo/test
   keys here are fixtures, never production identity).

The on-target end-to-end harness that carries this to the QNX VM/hardware is
`crates/kirra-l3-e2e` (self-contained, cross-compiles for
`x86_64-pc-nto-qnx800`).
