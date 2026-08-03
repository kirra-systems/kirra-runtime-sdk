//! **Kirra World storage adapter — PROTOTYPE: shape only, no storage.**
//!
//! No SQLite, no schema, no event log, no projections. ADR-0041 (WM-2) proposes
//! the persistence design and its ratification is **measurement-gated** —
//! Jetson replay, query benchmarks, a corruption/restart experiment, growth
//! estimates, a migration PoC. None of that has been run, so nothing is
//! implemented here.
//!
//! # What the graph does and does not settle
//!
//! ADR-0040 open question 1 asks whether this crate earns its separation, and
//! pre-authorizes collapsing it into a `kirra-world` feature if it does not.
//! The prototype gives a partial answer and it is worth being precise about
//! which part:
//!
//! **What it shows.** A consumer can depend on `kirra-world` alone and get the
//! domain vocabulary with *no* storage dependency. That is the seam's actual
//! value, and it is now demonstrable rather than asserted — a future doer-side
//! adapter that only needs to name entities does not link a database.
//!
//! **What it cannot show.** Whether the separation is worth its maintenance cost
//! is a question about the storage implementation that does not exist yet. An
//! empty crate splits cleanly from anything. If a second backend never appears
//! and nothing ever depends on the core without the store, the honest answer
//! will be that the seam did not earn its keep — and the collapse is already
//! pre-authorized for exactly that outcome. **This prototype does not close
//! open question 1.**

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Re-exported so the dependency edge is real rather than declared-and-unused:
// the graph this prototype exists to prove has to actually compile.
pub use kirra_world::{EntityId, ObservationId};
