# KIRRA QNX Runbook — RTM/FDIT + verdict-path WCET on a QNX SDP 8.0 target

**Status:** Active operator runbook (Phase-I bring-up).
**Scope:** the step-by-step workflow to cross-build the QNX RTM harness, deploy it
to a QNX SDP 8.0 target (an x86-64 VM is the Phase-I surrogate), and capture the
FDIT verdict-correctness gate + the per-verdict WCET CSV row.

This runbook is the **operator how-to**. It does not restate the design — for that
see, and keep authoritative:

- `docs/safety/WCET_QNX_BRINGUP.md` (#274) — the WCET cross-compile recipe + the
  Phase-I acceptance criteria + the host-vs-target invariant.
- `docs/adr/KIRRA_QNX_CROSSCOMPILE.md` — the QNX SDP 8.0 + Rust toolchain install
  recipe (written for the governor-service binary; the toolchain steps apply here).
- `tools/qnx-rtm-harness/README.md` + `QNX_MAPPING.md` — the harness internals, the
  concern split (C++ shim DRIVER vs Rust judge CHECKER), and the RTM traceability.
- `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md` — why host numbers are INDICATIVE.

---

## 0. Topology — who builds, who runs

The QNX target does **not** build anything. You **cross-compile on a dev host that
has QNX SDP 8.0 installed**, then copy the resulting `nto` binaries to the QNX
target and run them there.

| Box | Role | OS / stack |
|---|---|---|
| **Dev host** | Cross-compile (`run_qnx_fdit.sh`) | Linux + QNX SDP 8.0 (`qcc`/`q++`) + Rust |
| **QNX target** | Runs `rtm_harness` / `wcet_measure` / `kirra_demo` | QNX SDP 8.0 (x86-64 VM for Phase-I; NVIDIA DRIVE + QNX OS for Safety for Phase-II) |

For the Phase-I x86 laptop setup the QNX target is a **VM** (KVM/QEMU), which is why
hardware virtualization (Intel VT-x / AMD-V) must be enabled in BIOS — see §1.

> **Phase boundary (do not skip).** A number from an x86 VM under TCG/KVM is
> **Phase-I feasibility**, not a certified WCET. Cert-grade WCET is **Phase-II**:
> NVIDIA DRIVE + QNX OS for Safety + a Ferrocene-qualified Rust under FIFO. The
> harness encodes this — see `AOU-HW-QNX-TARGET-001` and `QNX_MAPPING.md` §7.

---

## 1. Prerequisites (one-time)

### 1a. Dev host — QNX SDP 8.0 + Rust

```sh
# QNX SDP 8.0 installed; source its environment (adjust to your install path).
# Sets QNX_HOST, QNX_TARGET and prepends qcc/q++ to PATH.
source ~/qnx800/qnxsdp-env.sh
qcc -V                                   # confirm the qcc variant your SDP ships

# The judge is built no_std (core only). The x86_64-pc-nto-qnx800 target usually
# has no prebuilt `core` in upstream rustc, so run_qnx_fdit.sh falls back to
# `cargo -Z build-std=core`, which needs nightly + rust-src:
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

(If your QNX SDP ships its own bundled Rust with the `nto-qnx800` target, the
script's first path — a direct `rustc --target` cross-build — is used instead and
nightly/rust-src are not needed. The script tries direct first, then build-std.)

### 1b. QNX target VM (Phase-I, x86 laptop)

- Enable **Intel VT-x** (and **VT-d** if you'll pass through devices) in BIOS, so
  KVM can run the QNX guest. (HP: Esc → F10; most others: F2 or Del.)
- Build/boot a QNX SDP 8.0 x86-64 guest image (`mkqnximage` or a prebuilt SDP VM
  image — this is a QNX SDP step, **outside** this harness; see the QNX SDP docs).
- Note the guest's IP (`ifconfig` inside QNX) for the `scp` in §3. The default QNX
  image logs in as **root**, which you want for `SCHED_FIFO`.

---

## 2. Cross-build the harness (dev host)

```sh
cd ~/kirra-runtime-sdk
source ~/qnx800/qnxsdp-env.sh            # if not already sourced this shell

# Cross-builds: the no_std judge (libkirra_judge.a, nto) + the C++ shim/harness/
# demo + wcet_measure, all for the QNX target. x86_64 is the default arch.
tools/qnx-rtm-harness/run_qnx_fdit.sh
```

Optional overrides (see the script header): `KIRRA_QNX_ARCH=aarch64`,
`KIRRA_RUSTC_TARGET=...`, `KIRRA_QNX_QCC_VARIANT=...`, `KIRRA_RUST_TARGET_JSON=...`.

Output lands in `tools/qnx-rtm-harness/build-qnx/`:

| Binary | Purpose |
|---|---|
| `rtm_harness` | FDIT/RTM matrix — exits **non-zero on ANY wrong verdict** (the gate) |
| `wcet_measure` | `SCHED_FIFO` per-verdict WCET row (run as root) |
| `kirra_demo`   | end-to-end demo incl. a replay rejection |

These are `nto` binaries — they will **not** run on the Linux dev host.

---

## 3. Deploy to the QNX target

```sh
cd tools/qnx-rtm-harness/build-qnx
scp rtm_harness wcet_measure kirra_demo root@<qnx-vm-ip>:/tmp/
```

(Or use a shared image / virtfs mount if the VM has no network route.)

---

## 4. Run on the QNX target (as root)

```sh
cd /tmp

# 1) The PASS gate — verdict correctness. MUST exit 0.
./rtm_harness && echo "FDIT: every verdict correct on QNX (gate PASS)"

# 2) End-to-end demo (incl. a replay that must be rejected).
./kirra_demo

# 3) The WCET row. Run as ROOT so SCHED_FIFO is granted; otherwise the row
#    self-declares INDICATIVE (see §5).
#
#    On a VM (KVM/TCG), tag the provenance but DO NOT assert certification —
#    a VM is Phase-I feasibility, never cert-grade:
KIRRA_WCET_PLATFORM=kvm ./wcet_measure
#
#    ONLY on the certified Phase-II hardware (DRIVE AGX + QNX OS for Safety +
#    Ferrocene under FIFO) does the operator assert certification:
#    KIRRA_WCET_CERTIFIED=1 KIRRA_WCET_PLATFORM=drive-agx ./wcet_measure
```

---

## 5. Interpreting the `wcet_measure` output

The CSV row is the **canonical `kirra_timing::report::CSV_HEADER` schema** — the
same columns the host `kirra-wcet-bench` emits, so host and on-target rows union
into one table joinable on `(metric, env)`:

```
metric,env,sched,n,min_ns,mean_ns,max_ns,stddev_ns,p50_ns,p99_ns,p999_ns,wcet_status
kirra_judge_assess,qnx-target-fifo,SCHED_FIFO,1000000,…,QNX-TARGET-MEASURED
```

The `env` / `wcet_status` columns map onto `kirra_timing::MeasurementEnv` and are
**gated**, so a row cannot misrepresent itself:

| Where it ran | `env` | `wcet_status` |
|---|---|---|
| Certified HW + FIFO + `KIRRA_WCET_CERTIFIED=1` | `qnx-target-fifo` | `QNX-TARGET-MEASURED` |
| QNX **VM** (KVM/TCG), FIFO, no assertion | `other` | `INDICATIVE-NOT-WCET` |
| QNX target, no FIFO (not root) | `other` | `INDICATIVE-NOT-WCET` |
| A host smoke build | `host` | `INDICATIVE-NOT-WCET` |

The certified pair is emitted **only** under the three-way conjunction
`kIsQnxTarget && fifo_granted && KIRRA_WCET_CERTIFIED=1`. The explicit operator
assertion is mandatory because the binary cannot distinguish certified DRIVE/QNX-OS
hardware from a QNX VM — so a near-native KVM run stays `INDICATIVE` unless someone
deliberately asserts certified hardware. `KIRRA_WCET_PLATFORM` (e.g. `kvm`) is a
provenance label echoed in the human banner only; it never changes the verdict.

**Phase-I acceptance** (`WCET_QNX_BRINGUP.md` §4): `rtm_harness` PASSes
byte-identically on-target, and `wcet_measure`'s **`MAX < 100 µs`**. That replaces
the `TBD-QNX-TARGET` placeholder figure with the measured row. Remember the
VM-vs-hardware caveat in §0: a VM `QNX-TARGET-MEASURED` is feasibility-grade.

**Send back** the `rtm_harness` PASS line and the two `wcet_measure` lines (the
human `WCET …` line + the CSV row); the row gets folded into `QNX_MAPPING.md`.

---

## 6. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `run_qnx_fdit.sh` aborts: `source qnxsdp-env.sh first` | `QNX_HOST`/`QNX_TARGET` unset — source the SDP env in this shell. |
| `[judge] FAILED to build the judge` | rust-src/nightly missing → `rustup component add rust-src --toolchain nightly`. If rustc doesn't know the `…-nto-qnx800` tuple at all, use the custom `target.json` path the script prints (`KIRRA_RUST_TARGET_JSON=…`). Logs: `build-qnx/rustc_direct.log`, `build-qnx/cargo_buildstd.log`. |
| `wcet_measure` row says `host` / `INDICATIVE-NOT-WCET` on the target | The binary wasn't built for `nto` (re-run §2 with the SDP env sourced), or you're not actually on the QNX target. |
| `wcet_measure` row says `other` / `INDICATIVE-NOT-WCET` | `SCHED_FIFO` not granted — run as **root**. |
| `MAX` is large (ms) but `p99.9` is single-digit µs | Emulation jitter (TCG). Prefer KVM (VT-x) and an isolated core; the methodology gates on **p99.9, not max** — but Phase-I acceptance is on `MAX < 100 µs`, so a KVM/native run is needed for the gate. |
| `scp` cannot reach the VM | Get the guest IP via `ifconfig` inside QNX; ensure the VM network (NAT/bridged) routes; or use a shared image/virtfs. |

---

## 7. What this proves (and what it doesn't)

- **Proves (Phase-I):** the judge renders **correct verdicts** on a real QNX target
  (the FDIT gate), and the verdict path is **bounded and fast** (feasibility-grade
  WCET). This is the deferred `max < 100 µs` feasibility check.
- **Does not prove (Phase-II):** a *certified* WCET. That requires DRIVE hardware +
  QNX OS for Safety + Ferrocene-qualified Rust under FIFO on the frozen partition
  config — the number that backs the FTTI claim (`verdict_WCET + actuation_latency
  < control_cycle < 0.5 s`). See `docs/safety/ASIL_DECOMPOSITION.md` §7 and
  `WCET_MEASUREMENT_METHODOLOGY.md`.

The PASS gate is, and remains, **verdict correctness** — the WCET row is supporting
timing evidence, staged Phase-I (indicative) → Phase-II (certified).
