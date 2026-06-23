// src/governor_guard.rs
//
// The shared non-finite governor-input guard (`all_finite`, the #410 convergence
// point) moved VERBATIM to the lean `kirra-core` crate (de-monolith Stage 5) so the
// certified governor surface need not pull the verifier service's heavy tree. Re-exported
// here so every existing `crate::governor_guard::*` (and `kirra_runtime_sdk::
// governor_guard::*`) path — the scalar kernel, the C FFI, parko's diverse governor —
// keeps the SAME predicate. Zero churn, zero logic change.
pub use kirra_core::governor_guard::*;
