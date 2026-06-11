# QNX-Safe Backend Selection Rules (PARK-026)

| Field | Value |
|---|---|
| Requirement | **PARK-026** (issue #38) |
| Status | **Requirements half DECIDED** + enforced; **availability half PENDING #36** |
| Enforcement | `parko-core` — `backend_permitted` / `current_platform` (`src/backend_selector.rs`) |
| Cross-refs | #36 (PARK-024 QNX deployment spike), #37 (PARK-025 QNN+QNX analysis), #34 / PR #290 (`BackendSelector`), ADR-0001, ADR-0006, EPIC #270 |

> **The split (PARK-026).** This issue was labelled *blocked until PARK-024 (#36)
> confirms which POSIX features QNX exposes*. That blocks only the **availability**
> half — which backends QNX can actually host. It does **not** block the
> **requirements** half: the rules KIRRA imposes *regardless* of what QNX offers.
> This document delivers the requirements half plus a **conservative,
> deny-by-default** enforcement; every availability row that depends on #36's
> target findings is marked **PENDING-#36** and is **denied at runtime until
> confirmed**.

---

## 1. Architecture framing — where these rules apply

The **reference architecture (ADR-0006)** runs the Autoware/ROS 2 planner — and
parko's ML inference — in an **Ubuntu guest partition**, with the **KIRRA governor
QNX-resident** on the safety partition and the planner an *isolated guest*. In that
topology **parko is not on QNX**: it is the guest doer, and the governor validates
its outputs across the frozen-layout partition boundary.

These rules therefore govern the two cases where inference *does* touch QNX:

- **(a) QNX-native single-OS deployments** — vendor-neutral robots that run parko
  directly on QNX Neutrino (no hypervisor/guest split), and the #36
  verifier/governor-on-QNX lane where a backend may be co-resident.
- **(b) Any future safety-partition inference** — should an inference path ever be
  admitted onto a QNX safety partition (not the reference path today).

**This document does not make parko-on-QNX the primary path.** Off-QNX behavior is
**unchanged** (§4): existing Linux/Jetson users see zero difference.

---

## 2. The requirements (decided now — not pending anything)

These hold on QNX **independent of** what #36 finds available. A backend that
cannot meet them is ineligible for a QNX-native / safety-partition deployment even
if its runtime *is* present.

### R-1 — No dynamic allocation on the backend hot path
Steady-state inference (`InferenceBackend::run`) shall perform **no heap
allocation**. This aligns with the existing parko contracts:
- **ADL-003**, the zero-copy hot-path contract (`run(&[f32], &mut [f32])`) noted on
  `InferenceBackend` in `parko/crates/parko-core/src/backend.rs` (the documented
  future-refactor target); and
- the **alloc-free Governor hot path (S3 / #115)** — see
  `parko/crates/parko-kirra/src/lib.rs` ("Governor hot path itself stayed
  alloc-free").

**Honest current state:** the live `run()` returns an **owned** `TensorBatch`
(output allocation), which ADL-003 will refactor to the zero-copy form. Until ADL-003
lands, a QNX-native backend MUST pre-allocate its output buffers at `load_model`
time and reuse them per `run()` — it must not allocate per inference. Input
zero-copy (`TensorStorage::Borrowed`) is already supported and shall be used.

### R-2 — Restricted POSIX API surface
A QNX-native backend (and the loader it pulls in) shall **not** depend on:
- **`fork` / `exec`** — no process spawning; the single-process model (R-3) forbids it.
- **`dlopen` beyond the documented loader path** — dynamic loading is permitted
  *only* via the platform's sanctioned, fixed loader path (the one #36 confirms);
  arbitrary/relative `dlopen` of vendor blobs at runtime is forbidden.
- **signals for control flow** — signals may not be used as a control or
  data-path mechanism (handlers for fault containment are a separate concern);
  inference progress shall not depend on signal delivery.
- **unbounded threading** — no unbounded or per-request thread spawning; thread
  counts shall be **fixed and bounded**, set at init. (Default inference is
  **single-threaded** for bitwise reproducibility — `InferenceThreads::default()`,
  `num_threads = 1`; raising it is a logged, deliberate act, never per-request.)

### R-3 — Single-process model
The backend shall operate **in-process**: no helper processes, no IPC to a
co-process daemon, no shared-memory inference broker spawned by the backend. The
governor's trust boundary assumes one process; a backend that forks an inference
server breaks that assumption and is ineligible.

---

## 3. The availability table (PENDING #36)

Per-backend QNX status. **Conservative rule — stated normatively:** `PENDING-#36`
means **FORBIDDEN at runtime** until #36 confirms the required facilities on the
target. This is **deny-by-default**, never allow-until-denied. A row moves to
`CONFIRMED` only with #36 evidence (and, for QNN, the #37 analysis).

| Backend (`BackendDescriptor`) | Required platform facilities | QNX status | Rationale |
|---|---|---|---|
| **Cpu** (parko-onnx CPU EP) | ONNX Runtime CPU build; `libonnxruntime` via the sanctioned loader path (R-2); bounded threads | **PENDING-#36** | CPU inference is the most portable, but an ORT QNX build + the fixed loader path are **unconfirmed** on target. Plausible to confirm first — but denied until #36. |
| **Cuda** (parko-onnx CUDA EP) | NVIDIA driver + `libcuda` + CUDA EP | **FORBIDDEN** | No CUDA stack on standard QNX Neutrino. Structurally impossible — not a #36 question. |
| **TensorRT** (parko-tensorrt) | CUDA runtime + TensorRT libs (Jetson) | **FORBIDDEN** | TensorRT is CUDA-based; same basis as Cuda. (A DRIVE-OS CUDA-on-QNX variant is out of scope for this vendor-neutral lane.) |
| **QualcommQnn** (QNN NPU) | Qualcomm QNN SDK / NPU runtime + loader | **PENDING-#36** | QNN-on-QNX compatibility is the **#37** analysis (now filed: `work/decisions.md` ADL-012 — the distribution gate makes QNX QAIRT binaries Ride/automotive-BSP-only, not public); gated by it **and** #36. Denied until both confirm. |
| **TiTidl** (TI TIDL) | TI TIDL runtime + device access | **PENDING-#36** | TIDL runtime availability on QNX unconfirmed. Denied until #36. |
| **IntelOpenVino** (OpenVINO) | Intel OpenVINO runtime (x86) | **PENDING-#36** | OpenVINO targets Linux/Windows; QNX support unconfirmed (likely unavailable) — denied until #36 confirms either way. |
| **AmdVitis** (Vitis AI) | AMD Vitis AI / XRT runtime | **PENDING-#36** | Vitis AI / XRT on QNX unconfirmed. Denied until #36. |

**Net current posture on QNX: deny-all.** No backend is `CONFIRMED` yet — `Cuda`
and `TensorRT` are permanently `FORBIDDEN`; every other backend is `PENDING-#36`
and therefore denied at runtime. This is the intended conservative starting point;
rows earn `CONFIRMED` one at a time as #36 (and #37 for QNN) produce target evidence.

---

## 4. Enforcement (parko-core)

The table above is enforced by a **pure, platform-parameterized** policy — testable
on any host, not buried under `cfg`:

```rust
pub enum TargetPlatform { Linux, Qnx, Other }
pub fn current_platform() -> TargetPlatform;                 // cfg(target_os = "nto") => Qnx
pub fn backend_permitted(platform: TargetPlatform,
                         d: BackendDescriptor) -> Result<(), BackendError>;
```

- **Off-QNX (`Linux` / `Other`) → always permitted.** The held line: existing
  users see **zero** behavior change. The QNX rules engage only on the QNX target.
- **On QNX → the §3 allowlist.** `FORBIDDEN` and `PENDING-#36` rows **both** return
  `Err` — the error names this rule doc, and `PENDING` rows name **#36**.
- **`BackendSelector::new` / `from_env` consult the gate FIRST**, before factory or
  stub resolution — so a **registered factory cannot bypass** the platform rule
  (tested). The deterministic **Mock default** (unset `KIRRA_BACKEND`) is
  platform-agnostic (no hardware, no restricted POSIX) and remains the safe
  fallback on any target.

`current_platform()` uses `cfg(target_os = "nto")` for QNX; the policy itself takes
the platform as a **parameter**, so every rule is exercised by host-run unit tests
(`backend_selector::tests`) without cross-compiling.

---

## 5. What stays blocked on #36 (and #37)

- Moving any `PENDING-#36` row to `CONFIRMED` — requires #36's POSIX-availability
  findings on the QNX target (ORT build, the sanctioned `dlopen` loader path,
  threading model, device access).
- `QualcommQnn` additionally requires the **#37** QNN+QNX compatibility analysis.
- When a row is confirmed, the change is: (1) update its row here to `CONFIRMED`
  with the evidence, and (2) move that descriptor to a permitted arm in
  `backend_permitted`'s QNX match — a one-line, test-covered change. Until then the
  fence holds deny-by-default.

---

## 6. Cross-references

- **#36** (PARK-024) — QNX deployment spike; the availability evidence this gates on.
- **#37** (PARK-025) — QNN + QNX compatibility analysis; gates the `QualcommQnn` row.
- **#34 / PR #290** — `BackendSelector` (factory registry + fail-closed env
  selection); this is the platform gate layered ahead of its resolution.
- **ADR-0001** — Governor Deployment Platform.
- **ADR-0006** — Governor transport; the partition topology (governor QNX-resident,
  planner an isolated guest) this document's framing rests on.
- **EPIC #270** — the QNX governor-transport lane.
