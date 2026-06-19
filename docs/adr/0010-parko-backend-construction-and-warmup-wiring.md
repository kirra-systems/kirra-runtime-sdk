# ADR-0010: Where parko backend construction + warm-up wiring lives

| Field | Value |
|---|---|
| Status | **Proposed** |
| Date | 2026-06-19 |
| Deciders | Project owner (pending) |
| Issues | #415 (PARK-021 #2 — engine warm-up) |
| Code | `parko/crates/parko-tensorrt/src/lib.rs` (`TrtBackend::warm_up`, `WarmUpReport`), `parko/crates/parko-core/src/backend_selector.rs` |

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
