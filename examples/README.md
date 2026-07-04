# Kirra SDK examples

Runnable consumers of the Kirra safety governor — the CHECKER that bounds a doer's
proposed commands, fail-closed. All are exercised in CI (`SDK docs + examples`), so
they never rot.

| Example | Language | Run |
|---|---|---|
| `governor_quickstart.rs` | Rust | `cargo run --example governor_quickstart` |
| `c/kirra_ffi_demo.c` | C (over the `include/kirra.h` ABI) | `./examples/c/build_and_run.sh` |
| `langchain_action_filter.py` | Python | see file header |
| `openai_action_filter.py` | Python | see file header |

## Rust — in-process governor

`governor_quickstart.rs` constructs a `KirraKernelGovernor` over a
`KinematicContract` (the hard envelope) and feeds it a sequence of proposed
velocities — safe, over-envelope, and a corrupt `NaN` — printing what the checker
actually emits. It asserts the two invariants the checker guarantees: the emitted
command is **always finite** and **always inside the envelope**.

## C — the same over the C ABI

The root crate builds a `cdylib` (`libkirra_verifier`), so a C program links it and
calls the stable ABI in [`include/kirra.h`](../include/kirra.h). `build_and_run.sh`
builds the library, compiles the demo against it, and runs it:

```sh
./examples/c/build_and_run.sh            # release
PROFILE=debug ./examples/c/build_and_run.sh
```

## API docs

Build the SDK's rustdoc locally:

```sh
cargo doc --no-deps --open
```
