# kirra-governor-service

Minimal over-the-wire (UDP) KIRRA governor for the two-box governed-car
prototype — see `docs/adr/KIRRA_BRINGUP_RUNBOOK.md` (Prompt A), `ADR-0001`, and
`KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md`.

It wraps the **existing** verdict core (`kirra_core::kinematics_contract`) —
not forked, not reimplemented. The talisman is amended ONLY under review + a
re-pin; the stop-gate H1/M1 amendment (ClampBoth + direction-aware accel/brake)
re-pinned it to logic blob `33b47b564caee20313cfeeffd2c2a0dcc42fb891`
(superseding the historical `997fb7ae…`; see `docs/CAPTURE_PIPELINE_SPEC.md` §0).
Because that core imports only `serde` + `std`, this binary's whole dependency
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

The full step-by-step is the authoritative recipe in
**`docs/adr/KIRRA_QNX_CROSSCOMPILE.md`**. Summary:

- **Prototype target: `x86_64-pc-nto-qnx800`** (QNX SDP 8.0 = QNX OS 8.0), built
  on an x86-64 Ubuntu dev host (not in CI — no QNX SDP there). The AArch64
  sibling for DRIVE Orin later is `aarch64-unknown-nto-qnx800`.
- **Use the Rust toolchain bundled with / documented by QNX SDP 8.0**, which
  ships a working `std` for `qnx800`. Do **not** rely on
  `rustup target add x86_64-pc-nto-qnx800`: upstream ships **no prebuilt `std`**
  for the `nto` targets (you'd need nightly `-Z build-std`, slow and unsupported
  for `qnx800`).
- Source the SDP env (`source ~/qnx800/qnxsdp-env.sh`), set the `qcc` linker via
  a `[target.x86_64-pc-nto-qnx800]` cargo stanza
  (`linker = "qcc"`, `rustflags = ["-C", "link-args=-Vgcc_ntox86_64"]` — match
  the variant `qcc -V` prints), then:

  ```sh
  cargo build --release --target x86_64-pc-nto-qnx800 -p kirra-governor-service
  ```

- Deploy and run on the QNX target, then validate from the PC by pointing
  `kirra-proposal-bench` at it
  (`KIRRA_GOVERNOR_ADDR=<qnx-ip>:9760 cargo run -p kirra-proposal-bench`) — the
  same verdict table as localhost closes M2-on-QNX.

This works because the crate's dependency tree is serde + bincode + std only
(no C deps, no ROS/async): the only QNX-specific machinery is the `qcc` linker
and the target's `std`.

> **Cert-stage caveat (later, not the prototype):** Ferrocene's *qualified* QNX
> target is **`x86_64-pc-nto-qnx710` (QNX 7.1.0)**, not `qnx800`. The ASIL-D Rust
> build is therefore a separate decision — QNX 7.1.0 + Ferrocene (qualified
> today) or a future Ferrocene `qnx800` qualification once QNX OS for Safety 8.0
> is the locked cert target. The prototype here is SDP 8.0 / `qnx800` with QM
> Rust: fine for the demo, **not** the certified artifact. See ADR-0001 and
> `docs/adr/KIRRA_QNX_CROSSCOMPILE.md`.

## What this is not

A prototype over UDP — not the certified build and not a tight real-time control
transport. Stale/missing verdicts are handled by safe-stating (governor watchdog
M6 + car-side deadline), which is the correct fail direction. The cert target
(single-SoC QNX Hypervisor, shared-memory mailbox, Ferrocene `no_std` core)
follows once the logic is proven — see ADR-0001.
