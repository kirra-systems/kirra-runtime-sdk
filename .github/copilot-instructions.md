# GitHub Copilot Repository Instructions

## Repository Overview

Kirra is a production-grade Rust SDK for deterministic, safety-oriented autonomous systems. The repository prioritizes correctness, deterministic execution, maintainability, API stability, and long-term certification readiness over convenience or brevity.

This is not a prototype repository. Treat all code as production quality.

Primary engineering goals:

1. Functional correctness
2. Deterministic behavior
3. Memory safety
4. Concurrency safety
5. API stability
6. Performance
7. Maintainability
8. Documentation and traceability

When these priorities conflict, preserve the higher priority.

---

# Repository Characteristics

- Language: Rust (stable)
- Workspace: Cargo workspace with multiple crates
- Runtime targets:
  - Linux
  - Embedded Linux
  - QNX
  - Hypervisor-based deployments
- Primary focus:
  - Zero-copy communication
  - Deterministic execution
  - Safety-critical middleware
  - FFI support
  - IPC
  - Real-time systems

Assume changes may eventually support ISO 26262 evidence generation.

---

# General Engineering Rules

Always prefer:

- explicit code over clever code
- predictable behavior over abstraction
- deterministic execution over convenience
- maintainability over micro-optimizations
- correctness over performance

Never introduce unnecessary complexity.

Never weaken safety boundaries.

Never bypass architectural abstractions simply because a shortcut exists.

---

# Before Making Changes

Read before modifying code:

- README.md
- CONTRIBUTING.md (if present)
- Cargo.toml (workspace)
- crate Cargo.toml
- relevant crate README
- existing tests
- GitHub workflows under .github/workflows

Do not search the repository unnecessarily if the required information already exists in documentation.

---

# Build

Always use Cargo workspace commands unless intentionally working inside a single crate.

Typical validation order:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
cargo build --workspace
```

If a single crate is modified:

```bash
cargo test -p <crate>
```

before running the entire workspace.

Do not claim any command passes unless it was actually executed.

---

# Validation

Before considering work complete:

- formatting passes
- compilation succeeds
- affected tests pass
- no new warnings introduced
- documentation updated if behavior changed

If validation cannot be executed, explicitly state that it was not verified.

Never guess.

---

# Pull Request Expectations

Pull requests should remain focused.

Avoid unrelated refactoring.

Avoid formatting unrelated files.

Avoid broad API changes unless explicitly requested.

Keep diffs easy to review.

---

# Review Priorities

When reviewing code, evaluate in this order:

1. Correctness
2. Safety
3. Undefined behavior
4. Memory ownership
5. Concurrency
6. Determinism
7. API compatibility
8. Performance
9. Maintainability
10. Documentation
11. Style

Do not report stylistic suggestions as defects.

Label style suggestions as low priority.

---

# Rust Guidelines

Prefer:

- ownership over shared mutable state
- borrowing over cloning
- iterators when they improve clarity
- explicit error handling
- Result over panic
- exhaustive match statements

Avoid:

- unnecessary allocations
- unnecessary Arc
- unnecessary Mutex
- unnecessary clone()
- unwrap()
- expect() outside tests
- hidden heap allocations
- hidden blocking operations

Unsafe code must remain minimal.

Every unsafe block should have documented safety invariants.

Never expand unsafe usage without strong justification.

---

# FFI

Treat every FFI boundary as safety-critical.

Verify:

- ownership
- lifetime
- alignment
- layout
- panic boundaries
- null handling

Panics must never cross FFI boundaries.

---

# Concurrency

Always verify:

- race conditions
- deadlocks
- lock ordering
- starvation
- atomic ordering
- Send/Sync correctness

Avoid introducing blocking behavior into deterministic paths.

---

# Determinism

Deterministic execution is a core project requirement.

Avoid introducing:

- hidden allocations
- unpredictable scheduling
- unnecessary synchronization
- timing variability
- random behavior
- non-deterministic iteration when ordering matters

---

# Performance

Optimize only after correctness.

Prefer:

- zero-copy
- borrowing
- stack allocation where appropriate
- cache-friendly data layouts

Do not sacrifice readability for insignificant gains.

---

# Public APIs

Public APIs are considered stable.

Avoid:

- breaking signatures
- unnecessary renames
- behavioral changes
- hidden side effects

If a breaking change is unavoidable, clearly explain why.

---

# Documentation

Behavioral changes require documentation updates.

Document:

- assumptions
- invariants
- safety requirements
- concurrency expectations
- ownership rules

Public APIs should include Rust documentation.

---

# Architecture

Respect existing module boundaries.

Do not bypass abstraction layers.

Preserve separation between:

- safety-critical logic
- platform-specific code
- IPC
- transport
- public SDK APIs
- implementation details

Avoid introducing cyclic dependencies.

---

# Dependencies

Avoid adding dependencies unless clearly justified.

Prefer existing workspace libraries.

Minimize compile time and dependency graph growth.

---

# Testing

Prefer:

- unit tests
- integration tests
- regression tests

Bug fixes should include regression tests whenever practical.

---

# Code Reviews

Base review comments only on evidence visible in the repository.

Do not speculate.

If evidence is insufficient, explicitly state additional context is required.

Separate:

- confirmed defects
- possible risks
- stylistic suggestions

Clearly explain why a recommendation improves:

- correctness
- safety
- maintainability
- determinism
- performance

---

# Continuous Integration

Assume every change will be validated by CI.

Before proposing a change, ensure it is consistent with:

- formatting
- linting
- compilation
- tests
- repository workflows

Do not intentionally introduce warnings.

---

# Agent Behavior

Prefer modifying existing code over rewriting it.

Preserve project style.

Reuse existing abstractions.

Avoid duplicate implementations.

Minimize repository exploration.

Trust these instructions unless repository documentation explicitly contradicts them.

If documentation and implementation disagree, treat implementation as authoritative and document the discrepancy rather than guessing.
