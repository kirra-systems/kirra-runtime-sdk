# kirra-governor-service

Minimal over-the-wire (UDP) KIRRA governor for the two-box governed-car
prototype — see `docs/adr/KIRRA_BRINGUP_RUNBOOK.md` (Prompt A), `ADR-0001`, and
`KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md`.

It wraps the **existing** verdict core (`src/gateway/kinematics_contract.rs`),
compiled in **verbatim** via `#[path]` — not forked, not moved (the talisman
file stays byte-identical, blob `997fb7ae15ce3e11adec9218044c7c84b049ad3b`).
Because that file imports only `serde` + `std`, this binary's whole dependency
tree is **serde + bincode + std** — no `tokio`, ROS 2, `r2r`, or DDS, matching
the minimal-governor thesis of ADR-0001 (and the QNX cert target has none of
those anyway).

## Wire contract (single source of truth)

Two fixed-schema `bincode` structs over UDP, 1:1 request/response:

- **Proposal** (car → governor): `{ seq: u64, ts_nanos: u128, command: ProposedVehicleCommand }`
- **Verdict** (governor → car): `{ seq: u64, action: EnforceAction, reason_code: u32 }`

`command` is the verdict core's real input type (`ProposedVehicleCommand`); no
kinematic fields are invented. The safety envelope
(`VehicleKinematicsContract`) is the **governor's** policy
(`nominal_reference_profile()`) and is *not* carried in the proposal — the doer
proposes a command; it does not choose its own limits.

`reason_code`: `0` = no breach (Accept / Clamp); `1..=10` map 1:1 to `DenyCode`
(`NanInfLinearVelocity`=1 … `DegradedSpeedIncreaseDenied`=10), so a car can
branch on a stable numeric without deserializing `EnforceAction` (which is
`Serialize`-only in the core).

## Run (host / prototype)

```sh
cargo run -p kirra-governor-service
# listens on 0.0.0.0:9760 — override with KIRRA_GOVERNOR_ADDR=host:port
```

## Host build & test (CI / dev — pure Rust, builds anywhere)

```sh
cargo build -p kirra-governor-service
cargo test  -p kirra-governor-service     # service logic + the verdict core's own unit tests
```

## QNX x86-64 cross-compile (dev host only — NOT run in CI)

The cert target is QNX Neutrino; this build runs on the QNX SDP dev host, not in
CI (no QNX SDP toolchain there). The **prototype** uses standard Rust for the
QNX target — the Ferrocene / `no_std` / ASIL-D factoring is a later stage per
ADR-0001 and does not block the demo.

1. Install QNX SDP 8.0 and source its environment (puts `qcc`/`q++` and the
   target sysroot on `PATH` / `QNX_TARGET`):

   ```sh
   source ~/qnx800/qnxsdp-env.sh
   ```

2. Provide a Rust QNX target — either rustup's QNX target or the **Ferrocene**
   toolchain, whose qualified targets include QNX Neutrino 7.1.0 (x86-64 +
   Armv8-A):

   ```sh
   rustup target add x86_64-pc-nto-qnx710        # rustup's QNX Neutrino 7.1 target
   ```

3. Point Cargo's linker at the QNX compiler driver and build **only this crate**
   (its dep tree is serde + bincode, so nothing ROS/async obstructs the QNX
   build):

   ```sh
   export CARGO_TARGET_X86_64_PC_NTO_QNX710_LINKER=qcc
   export CARGO_TARGET_X86_64_PC_NTO_QNX710_RUSTFLAGS="-Clink-arg=-Vgcc_ntox86_64"
   cargo build -p kirra-governor-service --target x86_64-pc-nto-qnx710 --release
   ```

4. Copy the binary to the QNX target and run it; point the car's bridge node
   (runbook Prompt B) at `host:port`.

> The exact target triple and `qcc -V<variant>` argument depend on the installed
> QNX SDP and Rust/Ferrocene release — confirm against the QNX SDP and Ferrocene
> QNX-target docs at build time. What this crate **guarantees** is the part that
> matters for portability: its dependency tree is serde + bincode only, so there
> is nothing ROS/async to stand in the way of the QNX build.

## What this is not

A prototype over UDP — not the certified build and not a tight real-time control
transport. Stale/missing verdicts are handled by safe-stating (governor watchdog
M6 + car-side deadline), which is the correct fail direction. The cert target
(single-SoC QNX Hypervisor, shared-memory mailbox, Ferrocene `no_std` core)
follows once the logic is proven — see ADR-0001.
