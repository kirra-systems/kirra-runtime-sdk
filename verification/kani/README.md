# verification/kani — EP-15 machine-checked proofs on the checker cores

[Kani](https://model-checking.github.io/kani/) (CBMC-based model checking for
Rust) PROVES each property for **all** inputs in its stated domain — a
categorically stronger claim than any sampled test. This crate holds the
proof harnesses for the actuation-path checker cores:

| Module | Source under proof (`#[path]`-included VERBATIM) | Properties |
|---|---|---|
| `proofs_lease` | `src/lease.rs` (root crate) | L1 `from_ttl` totality + split-brain invariant, all u64 · L2 promotion only after holder expiry + positive guard margin · L3 clock-skew fails safe · L4 on-cadence renewal never expires |
| `proofs_kinematics` | `crates/kirra-core/src/kinematics_contract.rs` — the FROZEN talisman (git blob `ed00f4da…`), proved **unmodified** | K1 NaN/Inf fail-closed totality (every f64 bit pattern, every field) · K2 non-positive dt denied · K3 P2 speed-ceiling clamp exact · K4/K5 Degraded re-initiation / speed-increase denials (#70) |
| `proofs_rss` | `parko/crates/parko-core/src/rss.rs` | R1 `longitudinal_safe_distance` totality (finite ∧ ≥ 0) over the FULL f64 domain · R2 closing-speed monotonicity on the integer-scaled grid (the `occlusion_limited_speed` bisection precondition) · R3 invalid brake → exactly `RSS_FAILSAFE_DISTANCE_M` |

**Honest scope** (per the EP-15 plan): the P6 bicycle-model path uses
`tan`/`atan` — transcendentals CBMC cannot decide — so the kinematics proofs
cover the decidable prefix (P0/P1/P2 + the Degraded gates); P6 stays covered by
the MC/DC + property-test suites. RSS monotonicity is quantified over
integer-scaled operational grids (0.01 m/s speed steps, 0.1-unit parameter
steps), not the full real line.

## Running

```bash
cd verification/kani
cargo test          # BLOCKING tier: compiles the verbatim includes, runs their
                    # own unit suites + a concrete mirror of every proof property
cargo kani          # the proofs (requires `cargo install kani-verifier && cargo kani setup`)
cargo kani --harness r2_longitudinal_monotone_in_closing_speed_on_grid   # one property
```

CI: the `kani-proofs` lane runs both tiers; the Kani tier is **non-blocking**
until the lane has a stable history (then it flips to blocking). Cited from
`docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2.

Like `fuzz/` and `crates/kirra-loom-models`, this crate is workspace-detached:
it never enters the root `Cargo.lock`, the MSRV lane, or a normal
`cargo build --workspace`.
