# Kirra Development Environment

## Host
- OS: Ubuntu 24.04 LTS
- Architecture: x86_64
- QEMU: 8.2.2 installed (x86_64 + aarch64)

## Cursor Cloud Agents
- Repo-level environment config: `.cursor/environment.json`
- Base image: `.cursor/Dockerfile`
- Rust toolchain: `rust:1.96-bookworm`

The locked dependency graph includes crates whose manifests require
edition-2024-aware Cargo (for example `getrandom v0.4.2`). Cursor Cloud Agents
therefore use the repo-level Dockerfile instead of relying on the platform
default Rust/Cargo version. This lets plain `cargo test --locked` work without a
per-agent `rustup toolchain install stable`.

## Cross-compilation Targets

| Target | Status | Use |
|--------|--------|-----|
| x86_64-unknown-linux-gnu | Native | Development |
| aarch64-unknown-linux-gnu | ✅ Working | Jetson simulation |
| x86_64-pc-nto-qnx800 | ⏳ Blocked — QNX SDP not installed | QNX VM |
| aarch64-unknown-nto-qnx800 | ⏳ Blocked — QNX SDP not installed | QNX physical |

Linker config: `.cargo/config.toml` sets `aarch64-linux-gnu-gcc` for the
`aarch64-unknown-linux-gnu` target.

## QNX SDP 8.0
- Status: License approved, download pending
- Install path (when ready): `/opt/qnx800/sdp2`
- First task after install: PARK-024 (QNX deployment spike)
- VM test script: `scripts/test-qnx-vm.sh` (guards for missing SDP)
- Target triples:
  - `x86_64-pc-nto-qnx800` — VM testing via QEMU
  - `aarch64-unknown-nto-qnx800` — physical hardware

## Jetson (aarch64)
- Status: Hardware in transit
- aarch64 binary builds cleanly via cross-compilation (`aarch64-unknown-linux-gnu`)
- Verified: ELF 64-bit LSB pie executable, ARM aarch64
- Pending: TensorRT (PARK-020) requires physical Jetson hardware

## aarch64 Cross-compilation

Confirmed working — commit `70e7c77`:

```
cargo build --target aarch64-unknown-linux-gnu --bin kirra_verifier_service
file target/aarch64-unknown-linux-gnu/debug/kirra_verifier_service
# ELF 64-bit LSB pie executable, ARM aarch64 ...
```

Required packages: `gcc-aarch64-linux-gnu`, `g++-aarch64-linux-gnu`
Rust target: `rustup target add aarch64-unknown-linux-gnu`

## ROS2
- Distribution: Jazzy
- Installed: 2026-05-27
- Source: source /opt/ros/jazzy/setup.bash

## Inference Backends

Compile each silicon backend with its feature flag from `parko-core`. The "Status on this host" column reflects what compiles and runs on the development Ubuntu 24.04 / x86_64 environment today.

| Backend | Feature flag | Crate | Status on this host | Target hardware | Tracking |
|---|---|---|---|---|---|
| ONNX Runtime (CPU) | (default) | `parko-onnx` | ✅ Full implementation, `OrtBackend` | Any x86_64 / aarch64 | PARK-008/009 |
| OpenVINO | `backend-openvino` | `parko-core/backends/openvino_stub.rs` | ✅ Stub compiles; `new()` returns `InitializationError` (no runtime installed) | Intel CPU / iGPU / VPU / AUTO | **PARK-029** |
| TensorRT | `backend-tensorrt` | `parko-core/backends/tensorrt_stub.rs` | ⏳ Stub compiles; Jetson hardware in transit | NVIDIA Jetson / DRIVE | PARK-020/021 |
| QNN | `backend-qnn` | `parko-core/backends/qnn_stub.rs` | ⏳ Stub compiles; Qualcomm hardware not on hand | Snapdragon / QCS6490 | PARK-027 |
| TIDL | `backend-tidl` | `parko-core/backends/tidl_stub.rs` | ⏳ Stub compiles; TI hardware not on hand | TDA4VM / J7 DSP | PARK-028 |
| AMD Vitis | `backend-amd` | `parko-core/backends/amd_stub.rs` | ⏳ Stub compiles; Kria K26 not yet ordered | Xilinx Kria / Vitis AI | PARK-030 |

OpenVINO real-implementation install path: install OpenVINO runtime from <https://docs.openvino.ai/>, then replace `OpenVinoStubBackend::new()` body with an `openvino`-crate (C-API binding) backed engine. Stub's `descriptor()` and `capabilities()` remain valid as-is.
