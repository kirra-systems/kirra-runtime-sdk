# Kirra Development Environment

## Host
- OS: Ubuntu 24.04 LTS
- Architecture: x86_64
- QEMU: 8.2.2 installed (x86_64 + aarch64)

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
- Install path (when ready): `/opt/qnx800`
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
