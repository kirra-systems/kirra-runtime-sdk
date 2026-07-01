# Performance Build Tuning — control-plane binaries

Cheap, **safe** per-chipset performance for the host/edge Linux control-plane
binaries (`kirra_verifier_service`, `kirra_carla_client`) via **build flags, not
source forks**. Same code everywhere; only codegen is tuned.

## The one hard rule (read first)

**None of this touches the safety element.** The WCET-critical governor / checker
evidence is produced by the QNX cross-build (`tools/qnx-rtm-harness/run_qnx_fdit.sh`),
which builds the `no_std` judge with its own **fixed** flags
(`-C opt-level=2 -C panic=abort`, **no** LTO / PGO / `target-cpu`). That build must
stay deterministic and reproducible so the FDIT/WCET row is trustworthy. Do **not**
route the cert build through the `dist` profile or apply any tuning below to it.
Build-time tuning is for the *untrusted* control plane and the *untrusted* doer —
never the trusted kernel.

Three tiers, cheapest/safest first:

| Tier | What | Portable? | Fragments source? | Where |
|---|---|---|---|---|
| **A. `dist` profile** | thin-LTO + `codegen-units=1` + `strip` | ✅ yes | ✅ no | committed (`Cargo.toml`) |
| **B. `target-cpu`** | pin the ISA baseline / `native` | ⚠️ per-target | ✅ no | opt-in at invocation |
| **C. PGO** | profile-guided optimization | ⚠️ workload-specific | ✅ no | opt-in (`scripts/pgo-build.sh`) |

---

## Tier A — the `dist` profile (always on for shipped artifacts)

`Cargo.toml` defines `[profile.dist]` inheriting `release` (so `panic = "abort"`
and its fail-closed rationale still hold) and turning up codegen quality only:

```toml
[profile.dist]
inherits = "release"
lto = "thin"          # ~most of fat-LTO's win, a fraction of the build time
codegen-units = 1     # better cross-function optimization
strip = true          # smaller binary
```

Build:

```bash
cargo build --profile dist --bin kirra_verifier_service
# artifact: target/dist/kirra_verifier_service
```

`scripts/build_release.sh` already uses `--profile dist` for the portable musl
release artifacts. This tier is chipset-agnostic — keep it in the portable build.

**Want fat LTO?** For a final shipped artifact where build time doesn't matter,
`lto = "fat"` squeezes a little more. Thin is the default because it keeps CI and
local dist builds fast.

---

## Tier B — `target-cpu` (opt-in, per deployment)

`target-cpu` lets the compiler use instructions your deployment CPU actually has
(AVX2/AVX-512/BMI2 on x86, the right feature level on aarch64). It is **not**
committed globally because it would break the portable musl release and cross-arch
builds. Set it at the build invocation for a build you know the target of:

```bash
# Portable-but-modern x86-64 baseline (AVX2/BMI2; ~any server since ~2015).
# Big win for ed25519 (MULX/ADX) and sha2 (SHA-NI is runtime-detected anyway).
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --profile dist --bin kirra_verifier_service

# Newer server baseline (AVX-512): Intel Ice Lake+/Sapphire Rapids, AMD Zen4+.
RUSTFLAGS="-C target-cpu=x86-64-v4" cargo build --profile dist --bin kirra_verifier_service

# Exact host (only when the build machine == the deploy machine; NON-portable):
RUSTFLAGS="-C target-cpu=native"    cargo build --profile dist --bin kirra_verifier_service

# aarch64 edge SoC — pin the actual core (example; use your SoC's value):
RUSTFLAGS="-C target-cpu=neoverse-n1" \
  cargo build --profile dist --target aarch64-unknown-linux-gnu --bin kirra_verifier_service
```

Choosing a baseline:
- `x86-64-v2` — universally safe on server hardware (~2010+). Minimal win.
- `x86-64-v3` — **recommended default** for a modern-server deployment (AVX2 + BMI2/`MULX`/`ADX`). This is where the Ed25519 field arithmetic gets faster.
- `x86-64-v4` — AVX-512; only if every node has it (Ice Lake+/Zen4+).
- `native` — fastest, **not portable**; only for build-on-target or a pinned SKU.

Note: `sha2` (SHA-NI) and `curve25519-dalek` already do **runtime** CPU-feature
detection, so a chunk of the crypto win is free even without `target-cpu`. Tier B
mainly helps the non-dispatched integer/vector paths.

Prefer setting it per-invocation (as above) or in a **per-target** stanza of a
build-host-local `.cargo/config.toml` — do **not** add a global `[build] rustflags`
`target-cpu` (it would poison cross-arch and the portable release).

---

## Tier C — PGO (opt-in, per deployment)

Profile-Guided Optimization compiles an instrumented binary, runs a
**representative workload** against it, then rebuilds using the collected profile
so the compiler lays out hot paths optimally. Worth it for a long-lived server
binary with a stable traffic shape.

```bash
# 1) provide a workload driver that exercises the RUNNING service with
#    representative traffic (register/attest/posture/federation routes), then:
scripts/pgo-build.sh ./scripts/pgo-workload.sh
# → target/dist/kirra_verifier_service, PGO-optimized
```

`scripts/pgo-build.sh` does: instrument → run your workload → `llvm-profdata
merge` → rebuild with `-C profile-use`. It uses the **rustc-bundled**
`llvm-profdata` (via `rustup component add llvm-tools-preview`) to avoid an
LLVM-version mismatch with a system `llvm-profdata`.

PGO composes with Tier B — pass `EXTRA_RUSTFLAGS="-C target-cpu=x86-64-v3"` to
`pgo-build.sh` to stack them.

The workload MUST be representative: PGO optimizes for what it sees, so a bad
workload can pessimize real traffic. Capture a realistic mix, not a microbench.

---

## What NOT to do

- ❌ A global `target-cpu` / `[build] rustflags` in the committed `.cargo/config.toml`
  (breaks cross-arch + the portable release).
- ❌ Any of A/B/C on the QNX judge / safety-checker cert build.
- ❌ Source-level SIMD intrinsics or vendor forks in the control plane to chase the
  last few percent — that fragments source and is not worth the maintenance. If a
  hot loop truly needs it, put it in the *doer*, not here (and never in the checker).
- ❌ Shipping a `native` build as the portable release artifact.
