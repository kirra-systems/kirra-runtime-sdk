# verification/kani — EP-15 machine-checked proofs on the checker cores

[Kani](https://model-checking.github.io/kani/) (CBMC-based model checking for
Rust) PROVES each property for **all** inputs in its stated domain — a
categorically stronger claim than any sampled test. This crate holds the
proof harnesses for the actuation-path checker cores:

| Module | Source under proof (`#[path]`-included VERBATIM) | Properties |
|---|---|---|
| `proofs_lease` | `src/lease.rs` (root crate) | L1 `from_ttl` totality + split-brain invariant, all u64 · L2 promotion only after holder expiry + positive guard margin · L3 clock-skew fails safe · L4 on-cadence renewal never expires |
| `proofs_kinematics` | `crates/kirra-core/src/kinematics_contract.rs` — the FROZEN talisman (git blob `851f3f44…`), proved **unmodified** | K1 NaN/Inf fail-closed totality (every f64 bit pattern, every field) · K2 non-positive dt denied · K3 P2 speed ceiling — never exceeded, and exact when it alone binds (restated #1243) · K3b composed enforcement reports both axes · K4/K5 Degraded re-initiation / speed-increase denials (#70) · K6 (P-CAP) every executable return respects the speed ceiling · K7 (P-RACK) every executable return respects the rack limit (`deep-proofs`) · K8 every executable return respects the accel/brake rate bound, in velocity space (#1243, `deep-proofs`) |
| `proofs_rss` | `parko/crates/parko-core/src/rss.rs` | R1 `longitudinal_safe_distance` totality (finite ∧ ≥ 0) over the FULL f64 domain · R2 closing-speed monotonicity on the integer-scaled grid (the `occlusion_limited_speed` bisection precondition) · R3 invalid brake → exactly `RSS_FAILSAFE_DISTANCE_M` |

**Honest scope** (per the EP-15 plan). The P6 bicycle-model path uses
`tan`/`atan`, which CBMC cannot execute. Until #1242 the kinematics proofs
simply never reached it — Priority 2 returned early, so the exclusion was free.
Making P2 accumulate put P6 on those paths and broke four harnesses, K3 among
them, with `call to foreign "C" function 'tan' is not currently supported`
rather than any counterexample.

The transcendentals are now **modelled** rather than avoided, in this crate
only: `f64::tan` and `f64::atan` become nondeterministic values constrained to
postconditions that are theorems about the real functions (finiteness, the
principal-branch range, and the inverse-monotone relation that couples them),
and `f64::powi` — which CBMC treats as an uninterpreted builtin, enough on its
own to make the P6 entry guard undecidable — becomes exact squaring. A proof
discharged under nondeterministic stubs holds for every implementation meeting
those postconditions, so this is stronger than a proof about one libm, not
weaker. `-Z stubbing` is declared in `Cargo.toml`, so plain `cargo kani` picks
it up. Details and the axiom list: the "Solver model" block in
`src/proofs_kinematics.rs`.

What is still excluded is narrower and worth stating exactly: the **numeric
value** the P6 bicycle model computes is not proved here. Under the model the
proofs never evaluate a real `tan`, so they cannot see whether that arithmetic
is right; it stays discharged by the concrete grid
(`k6_k7_mirror_exhaustive_grid_over_the_executable_return`, 306,180 points) and
the MC/DC + property-test suites. The grid and the proofs fail in opposite
directions — the grid can miss an unsampled branch, the proofs cannot see the
arithmetic — so neither substitutes for the other.

RSS monotonicity is quantified over integer-scaled operational grids
(0.01 m/s speed steps, 0.1-unit parameter steps), not the full real line.

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
