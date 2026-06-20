# ADR-0010: Where parko backend construction + warm-up wiring lives

| Field | Value |
|---|---|
| Status | **Accepted** — warm-up *hook* wired (PR #418) + backend *construction/selection* implemented (`parko_ros2::backend_select`) |
| Date | 2026-06-19 |
| Deciders | Project owner (pending on the construction/selection half) |
| Issues | #415 (PARK-021 #2 — engine warm-up) |
| Code | `parko-core::backend::InferenceBackend::warm_up` (hook), `parko-tensorrt` (`warm_up_report`/`engine_sha`/trait impl), `parko-ros2` `parko_ros2_node::build_loop` (call site) |

## Update (implemented) — the warm-up wiring is a TRAIT HOOK, not a parko-kirra factory

This ADR originally guessed the wiring belonged in a `parko-kirra` integration
factory. Inspection corrected that: `parko-kirra` constructs no backends, and the
real runtime entrypoint is the `parko_ros2_node` binary, which holds the backend
behind an `Arc<B: InferenceBackend>` and calls `load_model` in `build_loop`. So the
warm-up was wired as a **lifecycle hook on the `InferenceBackend` trait** instead:

- `parko-core`: `fn warm_up(&self, &ModelHandle) -> Result<(), BackendError>` with a
  default **no-op** (CPU/OpenVINO/mock need no warm-up). `&self` because backends are
  shared behind `Arc`.
- `parko-tensorrt`: overrides it to force the engine build; `warm_up_report` (now
  `&self`, SHA captured via a `OnceLock` exposed by `engine_sha()`) carries the detail.
- `parko-ros2`: `build_loop` calls `backend.warm_up(&model)` right after `load_model`,
  **fail-closed** (exit 4) — the node refuses to serve against an unbuilt engine.

No `parko-kirra` factory and no new crate were needed for the hook.

## Update 2 (implemented) — backend construction/selection: `parko_ros2::backend_select`

The construction/selection half is now resolved too — as a **compile-time, fail-closed,
feature-gated** selector in the `parko-ros2` *lib* (NOT ros2-gated, so it builds and is
CI-verified without a ROS 2 distro):

Two explicit gates, both fail-closed, both must agree:

- **Compile-time (authoritative):** `(no feature)` → `MockBackend` (dev only) ·
  `onnx-backend` → `parko-onnx OrtBackend` · `tensorrt-backend` → `parko-tensorrt
  TrtBackend` (takes precedence when both on). Exactly one backend compiles in.
- **Runtime (operator declaration):** if `PARKO_BACKEND` is set it MUST name the
  compiled-in backend (`mock` / `onnx` / `tensorrt`, case-insensitive) — else the node
  refuses to start (`verify_backend_env`). It is a cross-check, never a switch (only one
  backend is compiled in); it catches "deployed the wrong binary". Unset → the
  compile-time gate stands. This is the "feature **+** explicit env" selection the project
  owner chose (PARK-021 Q1).
- A real backend whose runtime/EP is unavailable returns `Err` and the node REFUSES to
  start (no silent substitution) — mirrors the installer's explicit `--target` and
  `parko-core::backend_selector`'s "selection is explicit" rule.
- `parko_ros2_node::main` calls `select_backend(&model_path)` (fail-closed, exit 2), then
  `build_loop` calls the `warm_up` hook (fail-closed, exit 4). The installer's
  `--target tensorrt` maps to building `--features ros2,tensorrt-backend`.

**Verified:** all three lanes build (`cargo build -p parko-ros2 --features {onnx,tensorrt}-backend`),
mock-lane tests pass, runtime-isolation guard still passes (the `parko-tensorrt` dep is
optional, so `parko-ros2` stays off the runtime-dependent list). A new CI job
(`parko-ros2-backends`) compiles the onnx/tensorrt lanes.

**Remaining open item (pre-existing CI gap, not introduced here):** no CI job builds the
`parko-ros2` *binary* with `--features ros2`, so the node `main`/`build_loop` call sites
(this ADR's wiring + #418's warm-up) are not yet compile-checked by CI. A `parko-ros2
--features ros2` build job (ROS 2 env, like the kirra-ros2-adapter job) should be added.

## Context

`TrtBackend::warm_up()` is implemented (PARK-021 #2): it forces the per-model/shape
TensorRT engine to build at startup (measured ~2.2 s cold on the Orin for MNIST) and
captures the engine SHA-256 into `TrtPosture.engine_sha`, so that multi-second build
never lands on the first real command. **But nothing calls it** — `parko-tensorrt` is
not yet constructed by any running service; it appears only in tests and doc comments.

The natural place to call `warm_up()` is wherever the concrete backend is constructed
at startup. That place is constrained:

- **It cannot be `parko-core`.** `backend_selector.rs` lives in `parko-core`, and the
  dependency direction is **backends depend on `parko-core`, not the reverse**
  (`parko-onnx`, `parko-tensorrt`, … → `parko-core`). `parko-core` selects a
  `BackendDescriptor`; it must not import a concrete backend crate.
- **So a concrete-backend factory must live in a higher layer** — the crate that owns
  the runtime and may depend on every backend crate (e.g. `parko-kirra`, or a new
  thin `parko-runtime` integration crate, or the service binary).

## Decision (proposed — react to this, it is not yet implemented)

Introduce a **backend factory at the integration layer** (NOT `parko-core`) that owns
the descriptor → concrete-backend mapping and the startup lifecycle:

1. **Select** the `BackendDescriptor` via `parko-core::backend_selector` (explicit;
   auto-detect only suggests — mirrors `scripts/install-parko-backend.sh`).
2. **Construct** the concrete backend (`TrtBackend::with_config` for `TensorRT`,
   `OrtBackend::new` for `Cpu`, …). Fail-closed: an unavailable EP refuses; never
   substitute another backend.
3. **Warm up at startup, before the node is advertised ready.** Call
   `backend.warm_up(&model)` once during init. The ~2.2 s build is paid here, off the
   hot path.
4. **Gate readiness on warm-up success (fail-closed).** A failed/absent warm-up → the
   node stays in a not-ready posture and does **not** serve inference; the Kirra
   governor must not admit commands against an unbuilt/cold engine. Record the
   `WarmUpReport` (`engine_sha`, timing) into the audit/posture.

### Why fail-closed on warm-up

A warm-up that errors means the engine could not be built/loaded — serving inference
anyway would either (a) pay the build cost on the first real command, or (b) run a
backend whose engine never materialized. Both violate the "no surprise on the first
command" intent; treat warm-up as a readiness precondition, consistent with the
installer's `validate_backend_loads` fail-closed gate.

## Consequences

- A clean separation: `parko-core` stays backend-agnostic (selection only); the
  integration layer owns construction + lifecycle. No dependency-direction violation.
- `warm_up()` finally has a caller; `engine_sha` reaches the audit record at startup.
- Startup gains a bounded blocking step (engine build); it runs during init, never on
  the command path.

## Open questions (for the deciding session)

- **Home of the factory:** extend `parko-kirra` startup, vs a new `parko-runtime`
  crate, vs the service binary. (Recommend `parko-kirra` — it already bridges parko to
  the governor — but this is the owner's call.)
- **Readiness signal coupling:** which health/ready endpoint or posture field the
  warm-up gates.
- **Warm-up input:** `warm_up()` currently synthesizes a zero input at the model's
  declared shape; confirm that is acceptable for engine build for all target models
  (it is, since the build keys on shape, not values — but record the assumption).
