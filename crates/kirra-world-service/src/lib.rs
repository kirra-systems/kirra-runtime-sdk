//! **Kirra World service adapter — PROTOTYPE: shape only, no service.**
//!
//! No HTTP, no routes, no server, no runtime, no transport dependency. This
//! crate exists so the third node of ADR-0040's proposed graph is present and
//! checkable; it does not run.
//!
//! # Why the emptiness is the point
//!
//! Fence A says Kirra World must be structurally unable to reach an actuator or
//! an authorization. A *service* crate is where that would most plausibly erode
//! — a transport dependency added "just to publish status", a ROS handle
//! threaded through "temporarily". `ci/check_kirra_world_bidirectional_fence.py`
//! now walks this crate's dependency closure for exactly those edges, and it
//! does so from today rather than from whenever the service is first written.
//!
//! A fence that arrives with the code is a fence argued with. This one is
//! already here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Both edges exercised, so the proposed three-node graph is genuinely built and
// not merely described in a manifest.
pub use kirra_world::ResolutionOutcome;
pub use kirra_world_store::EntityId;
