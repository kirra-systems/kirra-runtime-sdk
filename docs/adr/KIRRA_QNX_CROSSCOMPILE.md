# KIRRA QNX Cross-Compile Recipe — Governor Service on QNX x86-64 (M1 → M2-on-QNX)

**Date:** 2026-06-08
**Status:** Reference procedure
**Scope:** Cross-compile `kirra-governor-service` (PR #216, now on `main`) for the QNX x86-64 target, deploy it, and validate it with the `kirra-proposal-bench` tool (PR #218). This is the **prototype** stage (QM, regular Rust) — the cert toolchain is a separate decision, flagged below. Companion to `KIRRA_BRINGUP_RUNBOOK.md` (milestones M1 → M2).

This procedure changes nothing in the repo — it's a build/deploy flow. The talisman `src/gateway/kinematics_contract.rs` (`ed00f4da…`) is untouched.

---

## 0. Target & toolchain choice

- **Target triple:** `x86_64-pc-nto-qnx800` (QNX SDP 8.0 = QNX OS 8.0). The AArch64 sibling, for DRIVE Orin later, is `aarch64-unknown-nto-qnx800`.
- **Toolchain:** use the **Rust toolchain bundled with / documented by QNX SDP 8.0**, which provides a working `std` for `qnx800`. Do **not** rely on `rustup target add x86_64-pc-nto-qnx800` — upstream ships no prebuilt `std` for the nto targets (you'd need nightly `-Z build-std`), and upstream `qnx800` support is still in progress. QNX's SDP 8.0 Rust support is the prototype path.
- **Why this works for the governor:** `kirra-governor-service` is pure `serde + bincode + std` with no C dependencies, so the only QNX-specific machinery that matters is the **linker** (`qcc`) and the target's `std`. UDP works because QNX 8.0's default network stack is io-sock (the `qnx800` default target variant).

---

## 1. Install QNX SDP 8.0 + Rust support (the PC dev host)

Via QNX Software Center, install QNX SDP 8.0 (8.0.3 is current) and its Rust support package onto the x86-64 Ubuntu dev host. (QNX SDP host tools require x86-64 Linux or Windows — not ARM, not macOS — so the PC is the build host; the QNX target is where the binary runs.)

---

## 2. Environment

```bash
# Source the SDP environment (adjust the path to your install).
# Sets QNX_HOST, QNX_TARGET, and prepends the QNX tools to PATH.
source ~/qnx800/qnxsdp-env.sh

# Confirm the exact qcc compiler variant available in YOUR install:
qcc -V            # e.g. lists gcc_ntox86_64 (release) — use whatever it prints

# Toolchain env for the qnx800 target (form mirrors the rustc nto recipe,
# adapted 710 -> 800; the _cxx variant pulls the C++ runtime Rust links against).
export CC_x86_64-pc-nto-qnx800=qcc
export CXX_x86_64-pc-nto-qnx800=qcc
export CFLAGS_x86_64-pc-nto-qnx800=-Vgcc_ntox86_64_cxx
export CXXFLAGS_x86_64-pc-nto-qnx800=-Vgcc_ntox86_64_cxx
export AR_x86_64_pc_nto_qnx800=ntox86_64-ar
```

---

## 3. Cargo config

Add a target stanza (workspace `.cargo/config.toml`, or a throwaway one for the build):

```toml
[target.x86_64-pc-nto-qnx800]
linker = "qcc"
# Tell qcc which variant to link for (match `qcc -V`):
rustflags = ["-C", "link-args=-Vgcc_ntox86_64"]
```

---

## 4. Build

```bash
# Using QNX SDP 8.0's bundled cargo/rustc (which has qnx800 std):
cargo build --release --target x86_64-pc-nto-qnx800 -p kirra-governor-service

# Output:
#   target/x86_64-pc-nto-qnx800/release/kirra-governor-service
```

*(Only if you must use upstream nightly instead of QNX's toolchain:*
`cargo +nightly build -Z build-std=std,panic_abort --release --target x86_64-pc-nto-qnx800 -p kirra-governor-service` *— slower and unsupported for qnx800; prefer QNX's toolchain.)*

---

## 5. Deploy & run on the QNX target

```bash
# Copy to the QNX box/partition:
scp target/x86_64-pc-nto-qnx800/release/kirra-governor-service qnxtarget:/tmp/

# On the QNX target:
KIRRA_GOVERNOR_ADDR=0.0.0.0:9760 /tmp/kirra-governor-service
# expect: "kirra-governor-service: listening on 0.0.0.0:9760 (UDP), contract = nominal_reference_profile, effective_max_speed = 35.00 m/s"
```

---

## 6. Validate — M2-on-QNX

From the Linux PC, point the existing bench at the QNX target:

```bash
KIRRA_GOVERNOR_ADDR=<qnx-target-ip>:9760 cargo run -p kirra-proposal-bench
```

**Pass criterion:** the same CASE / REASON / VERDICT table as the localhost run — Allow / ClampLinear (both signs) / ClampSteering / DenyBreach codes — now served by the governor running on its real target OS, over the real wire. That closes M2 on QNX.

---

## Caveats & cert-stage note

- **`qcc -V` variant string** varies by install; confirm and substitute it everywhere above (`gcc_ntox86_64` shown).
- **`rustup` won't give you a working `std`** for `qnx800`; use QNX's bundled toolchain (or nightly `build-std`, not recommended).
- **Cert-stage alignment — important for later, not the prototype:** Ferrocene's *qualified* QNX target is **`qnx710` (QNX 7.1.0)**, not `qnx800`. So the ASIL-D Rust build is a separate decision: either target **QNX 7.1.0 + Ferrocene** (the qualified combination today) or wait for / commission a Ferrocene `qnx800` qualification once QNX OS for Safety 8.0 is the locked cert target. The prototype here runs on SDP 8.0 / `qnx800` with QM Rust — fine for the demo, not the certified artifact. See ADR-0001.
- **Talisman:** this is build/deploy only; no source changes, blob unchanged.

---

## Sources

- Rust `*-nto-qnx-*` platform support (qcc env, no prebuilt std, QNX 8.0 WIP) — rustc book: https://doc.rust-lang.org/nightly/rustc/platform-support/nto-qnx.html
- `x86_64-pc-nto-qnx800` / `aarch64-unknown-nto-qnx800` target names — QNX docs: https://www.qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.utilities/topic/r/rust-host.html
- Ferrocene qualified QNX target = `x86_64-pc-nto-qnx710` (QNX 7.1.0), qnxsdp-env.sh — Ferrocene user manual: https://public-docs.ferrocene.dev/main/user-manual/targets/x86_64-pc-nto-qnx710.html

---

*Companion documents: `KIRRA_BRINGUP_RUNBOOK.md`, `ADR-0001-governor-deployment-platform.md`, `KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md`.*
