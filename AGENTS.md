# AGENTS.md

## Cursor Cloud specific instructions

This is a Rust Cargo workspace ("Kirra" — a fail-closed runtime safety governor).
The root crate is `kirra-verifier`; sibling crates live under `crates/*`; `parko/`
is a **separate** Cargo workspace consumed via `path =` deps. See `README.md` and
`CLAUDE.md` for architecture; standard build/test/run commands are in `README.md`.

### Toolchain (important)
- The repo has **no `rust-toolchain.toml`** and CI uses the latest **stable** Rust.
- The base VM image's default rustup toolchain may be pinned to an **older** Rust
  (e.g. 1.83) that **cannot build the committed `Cargo.lock`** — some transitive
  deps require `edition2024` (Rust ≥ 1.85). The fix is `rustup default stable`
  (the startup update script does this). If a build fails with
  `feature edition2024 is required`, your active toolchain is too old.

### Build / test / lint (matches CI in `.github/workflows/ci.yml`)
- Build: `cargo build --workspace --locked`
- Test: `cargo test --workspace --locked` (root workspace does **not** descend into `parko/`).
- Lint: `cargo clippy --workspace --locked -- -D warnings -W clippy::all -A clippy::module_name_repetitions -A clippy::must_use_candidate`
- Parko (separate workspace, runtime-free lane): from `parko/`, run
  `cargo test --workspace --exclude parko-onnx --exclude parko-openvino --exclude parko-tensorrt`.
  Those three excluded crates `dlopen` native inference runtimes (ONNX Runtime /
  OpenVINO) at backend init and need libs installed — skip them locally.
- ROS 2 code is `--features ros2` gated and is **never** built by a default build
  or `cargo test --workspace`; it needs a sourced ROS 2 (Jazzy) toolchain. Same for
  the `tpm`/`cyclonedds` features (native libs). Leave all of these OFF for normal work.

### Running the verifier service (the one MUST-run product)
- Binary: `kirra_verifier_service` (axum HTTP, listens on `0.0.0.0:8090` by default).
- **`KIRRA_ADMIN_TOKEN` must be set and non-empty or the process aborts at startup**
  (SG-008 fail-closed invariant), e.g. `KIRRA_ADMIN_TOKEN=demo-admin`.
- **Non-obvious gotcha — a fresh/empty fleet is `LockedOut`, so EVERY non-exempt
  route (incl. all `GET /fleet/*` reads and the actuator route) returns `503`.**
  This is intentional (M-9 "no positive trust evidence → fail closed" in
  `recalculate_and_broadcast`). Only `/health`, `/ready`, `/metrics`, and the whole
  `/console` plane are posture-exempt. The posture cache also has a 5 s TTL and
  fail-closes if its periodic refresh stops.
- **To get a non-empty fleet for local/dev work, seed the store first** with the
  dev-only `kirra_console_demo_seed` binary (it refuses a non-empty DB and requires
  `KIRRA_LOG_SIGNING_KEY` = base64 of a 32-byte ed25519 seed, e.g.
  `head -c 32 /dev/urandom | base64`). Then start the service against the same
  `KIRRA_DB_PATH` and the same `KIRRA_LOG_SIGNING_KEY`. The seed populates 6
  `KIRRA-DEMO-*` nodes + a signed audit chain and makes `http://127.0.0.1:8090/console`
  showable end-to-end. See `docs/CONSOLE_RUNBOOK.md`.
- Operator recovery action (works under LockedOut, posture-exempt): record a
  clearance grant at `POST /console/clearance-grants`. The break-glass path uses
  the header `x-kirra-supervisor-key: <KIRRA_SUPERVISOR_RESET_KEY>` (the console UI's
  primary flow is operator-signed instead).
- `kirra_verifier_service` installs a SIGINT/SIGTERM graceful-shutdown handler that
  can take a few seconds; if it lingers, kill it by **PID** (never by name).

### Other binaries
- All other binaries are OPTIONAL/dev (CARLA client needs a simulator; demo/diagnostic
  bins; the two-box UDP `kirra-governor-service` + `kirra-proposal-bench`; ROS2 nodes).
